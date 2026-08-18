//! 「ブロックされていない怪しい通信」の候補を組み立てる。
//!
//! **ブロック済みの一覧(`/api/domains`)と役割が違う。** あちらは Pi-hole が既に止めたものを
//! 「なぜ止まったか」で確かめる画面だが、こちらは**素通りしているものの中から、
//! 目を向ける価値のあるものだけを拾う**。全部を並べたら1日1,300ドメインになって読めない。
//!
//! **判定はコードでやり、AIには渡さない。** 候補を絞ってから「これは何か」を聞くのが
//! 既存の「AIに聞く」で、全ドメインを投げると枠を使い切るだけで精度も上がらない。
//!
//! いまの手は3つ。**どれも「いつもと違う」の言い換え**なので、比較対象(ingest.rs が
//! 貯めた過去)が無いと成立しない。
//!
//! | 手 | 何を捕まえるか | 誤検知の出方 |
//! |---|---|---|
//! | 初出 | 新しいトラッカー、入り込んだ直後の通信 | CDNの新しいシャード |
//! | NXDOMAIN多発 | 生成ドメイン(DGA)、死んだ接続先、設定ミス | 名前解決の設定ミス |
//! | 珍しいクエリ種別 | DNSトンネリング | 新しい機器の正常な動作 |

use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;

use crate::db::Db;

/// 見る窓(秒)。**初出もNXDOMAINもこの窓で数える。**
/// 長くすると候補が増えて読めなくなり、短くすると寝ている間の出来事を見落とす。
const WINDOW_SECS: f64 = 24.0 * 3600.0;

/// NXDOMAIN を「多発」と呼ぶ下限。1〜2回は打ち間違いや一時的な失敗で普通に出る。
const NXDOMAIN_MIN: i64 = 5;

/// 平常の形として扱うクエリ種別。**ここに無い種別が出たら挙げる。**
///
/// 実測(この環境の1日)では A / AAAA / HTTPS / PTR / SVCB でほぼ全部を占め、
/// **TXT は46,939件中1件、ANY と NULL は0件**だった。DNSトンネリングは TXT や NULL を
/// 大量に使うので、平常がここまできれいだと、始まった日に一目で分かる。
const COMMON_QTYPES: &[&str] = &[
    "A", "AAAA", "HTTPS", "PTR", "SVCB", "SOA", "SRV", "NS", "MX", "NAPTR", "DS", "DNSKEY",
];

/// 候補から外す末尾。**通信そのものではないものを落とす。**
///
/// 逆引き(`*.in-addr.arpa` / `*.ip6.arpa`)は、Pi-hole 自身がローカル端末の名前を
/// 引くために出しているもので、ローカルの名前解決を持っていない環境では**必ず NXDOMAIN
/// になる**。落とさないと「存在しない名前として何回返っている」の一覧が、
/// 家の中の端末の逆引きだけで埋まる(実際にそうなった)。
const EXCLUDED_SUFFIXES: &[&str] = &[".in-addr.arpa", ".ip6.arpa", ".arpa"];

/// 候補が挙がった理由。**1つのドメインに複数付く**(初出かつNXDOMAIN多発、など)。
///
/// **理由は必ず画面に出す。** 「なぜこれが並んでいるのか」が分からない一覧は、
/// 誤検知なのか本物なのかを人が判断できず、結局全部無視されることになる。
#[derive(Debug, Clone, Serialize)]
pub struct Reason {
    /// 機械が見分ける印(画面の色分けに使う)
    pub kind: &'static str,
    /// 人が読む説明
    pub detail: String,
}

/// 怪しい通信の候補1件。
///
/// **`domain` / `count` / `reviewed` / `note` はブロック済み一覧と同じ形にしてある** ——
/// 画面の行・詳細モーダル・「AIに聞く」・メモがそのまま使い回せる。
#[derive(Debug, Clone, Serialize)]
pub struct WatchItem {
    pub domain: String,
    pub count: i64,
    pub reviewed: bool,
    pub note: String,
    pub reasons: Vec<Reason>,
    /// この窓で引いた端末(分かる範囲)
    pub clients: Vec<String>,
    /// はじめて見た時刻(unix秒)。遡り取り込みで埋めた行は日単位の粒度
    pub first_seen: i64,
}

/// 画面に渡す一式。**数字だけでなく「どこまで見えているか」も返す** ——
/// 貯まっていない時期に「初出0件」と出ると、平穏なのか単に材料が無いのか区別できない。
#[derive(Debug, Clone, Serialize)]
pub struct WatchResult {
    /// 遡り取り込みが済んでいるか。**偽なら初出の判定は当てにならない**
    /// (過去を知らないので、すべてが初出に見える)
    pub ready: bool,
    pub backfill_days: i64,
    pub window_hours: i64,
    /// 生のクエリが実際に何時間ぶん貯まっているか(NXDOMAIN・種別はこの範囲しか見ていない)
    pub data_hours: f64,
    pub total_domains: i64,
    /// この窓で観測したクエリ種別と件数(平常の形。画面に出して判断材料にする)
    pub qtypes: Vec<(String, i64)>,
    pub items: Vec<WatchItem>,
}

/// 候補を組み立てる。
pub async fn candidates(db: &Db, now: f64) -> Result<WatchResult> {
    let since_ts = now - WINDOW_SECS;
    let since_secs = since_ts as i64;

    let backfill_days = db.backfilled_days().await?;
    let stats = db.ingest_stats().await?;

    // 理由をドメインごとに束ねる。**同じドメインを手ごとに何度も並べない** ——
    // 一覧が重複で埋まると、件数が実際より多く見える
    let mut reasons: HashMap<String, Vec<Reason>> = HashMap::new();
    let mut first_seen: HashMap<String, i64> = HashMap::new();

    // ① 初出
    for (domain, seen, _total) in db.first_seen_since(since_secs).await? {
        first_seen.insert(domain.clone(), seen);
        reasons.entry(domain).or_default().push(Reason {
            kind: "first_seen",
            detail: format!("{}にはじめて見た", ago(now - seen as f64)),
        });
    }

    // ② NXDOMAIN多発
    for (domain, n) in db.nxdomain_since(since_ts, NXDOMAIN_MIN).await? {
        reasons.entry(domain).or_default().push(Reason {
            kind: "nxdomain",
            detail: format!("存在しない名前として{n}回返っている"),
        });
    }

    // ③ 珍しいクエリ種別
    let qtypes = db.qtype_counts_since(since_ts).await?;
    let rare: Vec<String> = qtypes
        .iter()
        .map(|(t, _)| t.clone())
        .filter(|t| !COMMON_QTYPES.contains(&t.as_str()))
        .collect();
    for (domain, qtype, n) in db.domains_by_qtype_since(since_ts, rare).await? {
        reasons.entry(domain).or_default().push(Reason {
            kind: "rare_qtype",
            detail: format!("珍しい種別 {qtype} を{n}回引いている"),
        });
    }

    // 件数と端末はまとめて1回で引く(ドメインごとに問い合わせない)
    let domains: Vec<String> = reasons.keys().cloned().collect();
    let activity = db.domain_activity_since(since_ts, domains).await?;
    let records = db.records().await?;

    let mut items: Vec<WatchItem> = reasons
        .into_iter()
        .filter(|(domain, _)| !is_excluded(domain))
        .map(|(domain, reasons)| {
            let (count, clients) = activity.get(&domain).cloned().unwrap_or((0, Vec::new()));
            let record = records.get(&domain);
            WatchItem {
                first_seen: first_seen.get(&domain).copied().unwrap_or(0),
                count,
                reviewed: record.map(|r| r.reviewed).unwrap_or(false),
                note: record.map(|r| r.note.clone()).unwrap_or_default(),
                reasons,
                clients,
                domain,
            }
        })
        .collect();

    // **理由の多いものを上に。** 1つの手に引っかかっただけのものより、
    // 複数の手に同時に引っかかったもののほうが見る価値がある。
    // 同数なら件数の多い順、それも同じならドメイン名で安定させる
    items.sort_by(|a, b| {
        b.reasons
            .len()
            .cmp(&a.reasons.len())
            .then(b.count.cmp(&a.count))
            .then(a.domain.cmp(&b.domain))
    });

    let data_hours = stats
        .oldest_ts
        .map(|oldest| ((now - oldest) / 3600.0).max(0.0))
        .unwrap_or(0.0);

    Ok(WatchResult {
        ready: backfill_days > 0,
        backfill_days,
        window_hours: (WINDOW_SECS / 3600.0) as i64,
        data_hours,
        total_domains: stats.domains,
        qtypes,
        items,
    })
}

/// 候補から外す相手か(`EXCLUDED_SUFFIXES` 参照)。
fn is_excluded(domain: &str) -> bool {
    let d = domain.to_ascii_lowercase();
    EXCLUDED_SUFFIXES.iter().any(|s| d.ends_with(s))
}

/// 経過秒を「3時間前」のような日本語にする。
fn ago(secs: f64) -> String {
    let secs = secs.max(0.0);
    if secs < 60.0 {
        return "たった今".to_string();
    }
    if secs < 3600.0 {
        return format!("{}分前", (secs / 60.0) as i64);
    }
    if secs < 86_400.0 {
        return format!("{}時間前", (secs / 3600.0) as i64);
    }
    format!("{}日前", (secs / 86_400.0) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ago_reads_naturally() {
        assert_eq!(ago(10.0), "たった今");
        assert_eq!(ago(600.0), "10分前");
        assert_eq!(ago(3.0 * 3600.0), "3時間前");
        assert_eq!(ago(50.0 * 3600.0), "2日前");
        // **負の経過を「-1分前」と出さない**(時刻のずれで未来になることがある)
        assert_eq!(ago(-5.0), "たった今");
    }

    #[test]
    fn reverse_lookups_are_excluded() {
        // Pi-hole 自身がローカル端末の名前を引いているだけで、通信ではない。
        // ローカルの名前解決が無ければ必ず NXDOMAIN になるので、落とさないと
        // 「NXDOMAIN多発」が家の中の逆引きで埋まる
        assert!(is_excluded("235.1.168.192.in-addr.arpa"));
        assert!(is_excluded("1.0.0.0.0.0.0.0.ip6.arpa"));
        // **普通のドメインは落とさない**(部分一致で巻き込まないこと)
        assert!(!is_excluded("example.com"));
        assert!(!is_excluded("arpa-labs.example.com"));
    }

    #[test]
    fn common_qtypes_cover_the_normal_shape() {
        // 実測でこの環境の99.9%を占めていた種別。ここから漏らすと、
        // 普通の通信が毎日「珍しい種別」として挙がってくる
        for t in ["A", "AAAA", "HTTPS", "PTR", "SVCB"] {
            assert!(COMMON_QTYPES.contains(&t), "{t} が平常の種別から漏れている");
        }
        // トンネリングに使われる種別は**平常に入れない**(入れると検出できなくなる)
        for t in ["TXT", "NULL", "ANY"] {
            assert!(!COMMON_QTYPES.contains(&t), "{t} を平常に入れてはいけない");
        }
    }
}

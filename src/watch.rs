//! 「ブロックされていない怪しい通信」の候補を組み立てる。
//!
//! ブロック済みの一覧(`/api/domains`)と役割が違う。 あちらは Pi-hole が既に止めたものを
//! 「なぜ止まったか」で確かめる画面だが、こちらは素通りしているものの中から、
//! 目を向ける価値のあるものだけを拾う。全部を並べたら1日1,300ドメインになって読めない。
//!
//! 判定はコードでやり、AIには渡さない。 候補を絞ってから「これは何か」を聞くのが
//! 既存の「AIに聞く」で、全ドメインを投げると枠を使い切るだけで精度も上がらない。
//!
//! いまの手は5つ。どれも「いつもと違う」の言い換えなので、比較対象(ingest.rs が
//! 貯めた過去)が無いと成立しない。
//!
//! | 手 | 何を捕まえるか | 誤検知の出方 |
//! |---|---|---|
//! | 初出 | 新しいトラッカー、入り込んだ直後の通信 | CDNの新しいシャード |
//! | NXDOMAIN多発 | 生成ドメイン(DGA)、死んだ接続先、設定ミス | 名前解決の設定ミス |
//! | 珍しいクエリ種別 | DNSトンネリング | 新しい機器の正常な動作 |
//! | 周期(ビーコン) | C2、常時テレメトリ | ルーターの死活確認・更新確認 |
//! | ラベルの形 | DNSトンネリング、DGA | CDN・クラウドのホスト名 |
//!
//! しきい値の説明は [`methods`] がコードから組み立てて画面へ返す。
//! 散文で別に書くと、定数をいじったときに説明だけが古くなる。
//!
//! 各理由には [`QueryFilter`](Reason::filter) が付いていて、画面はそれを
//! Pi-hole のクエリログ(`/admin/queries.lp`)の絞り込みリンクにする ——
//! 「本当にそうなっているか」は元の通信を見るのが一番早い。

use std::collections::HashMap;

use anyhow::Result;
use serde::Serialize;

use crate::db::{ClientActivity, Db};

/// 既定で見る窓(秒)。初出もNXDOMAINもこの窓で数える。
/// ブロック済みの一覧(`api::domains`)のアクセス元と期間も同じ窓を使う ——
/// 2つの一覧を並べて読むので、窓が違うと同じ端末の見え方が食い違う。
/// 長くすると候補が増えて読めなくなり、短くすると寝ている間の出来事を見落とす。
pub const WINDOW_SECS: f64 = 24.0 * 3600.0;

/// 基準日時の置き場(`settings` 表のキー)。値は unix 秒。
///
/// ネットワークの設定を変えた日の前後は、同じ画面に別の環境の記録が混ざる。
/// 例えばDHCPが配るDNSを変えると、変える前の通信はぜんぶルーター発として記録されていて、
/// 変えた後の「どの端末が」とは噛み合わない。そこを境に切れるようにしてある。
pub const BASELINE_KEY: &str = "watch:baseline";

/// NXDOMAIN を「多発」と呼ぶ下限。1〜2回は打ち間違いや一時的な失敗で普通に出る。
const NXDOMAIN_MIN: i64 = 5;

/// 珍しい種別を「出ている」と呼ぶ下限。1回では挙げない ——
/// 実測で `push.apple.com` の TXT が1回だけ出て候補になったが、
/// これは判断のしようがないノイズだった。DNSトンネリングは同じ種別を何百回と使うので、
/// この下限で取り逃がすものは無い。
const RARE_QTYPE_MIN: i64 = 5;

/// 平常の形として扱うクエリ種別。ここに無い種別が出たら挙げる。
///
/// 実測(この環境の1日)では A / AAAA / HTTPS / PTR / SVCB でほぼ全部を占め、
/// TXT は46,939件中1件、ANY と NULL は0件だった。DNSトンネリングは TXT や NULL を
/// 大量に使うので、平常がここまできれいだと、始まった日に一目で分かる。
const COMMON_QTYPES: &[&str] = &[
    "A", "AAAA", "HTTPS", "PTR", "SVCB", "SOA", "SRV", "NS", "MX", "NAPTR", "DS", "DNSKEY",
];

// ---- 周期(ビーコン)の検出 ----
//
// C2やテレメトリは機械が鳴らすので間隔が揃う。 人の操作で引かれる名前は間隔がばらつく。
// 揃い方は変動係数(標準偏差÷中央値)で測る —— 0に近いほど機械的。
// 端末ごとに分けて数える(同じドメインを複数台が引くと間隔が混ざって周期が消える)。

/// 周期とみなす変動係数の上限。実測では 0.25 以下に絞ると読める数(数件)に落ち着き、
/// 0.5 を超えるあたりからは人の操作の揺らぎと区別が付かなくなる。
const BEACON_MAX_CV: f64 = 0.25;
/// 周期と言うのに必要な間隔の数(観測はこれ+1回)。少ないと偶然そろっただけのものが混じる。
const BEACON_MIN_INTERVALS: usize = 6;
/// これより短い間隔は周期として扱わない。ブロックされた名前の再試行が秒間隔で並ぶので、
/// それを「規則正しい通信」と呼ばないための下限。
const BEACON_MIN_MEDIAN_SECS: f64 = 20.0;
/// 同時に飛ぶ問い合わせ(A/AAAA/HTTPS)を1回に畳む幅。畳まないと間隔0が大量に混じる。
const BEACON_SAME_SHOT_SECS: f64 = 1.0;

// ---- ラベルの形(DNSトンネリング・DGA) ----
//
// 長くて出鱈目な名前だけでは決め手にならない。 実測すると、CDNとクラウドが
// 同じ形の名前を大量に使っていた(`azr.footprintdns.com` に17個、`elb.amazonaws.com` に16個…)。
// 105個の親が引っかかり、そのどれもが正常な通信だった。
//
// 分かれ目は「同じ名前を繰り返し引くか」。 CDNは同じホスト名を何度も引く(実測した
// スマートスピーカーの常時接続先は 怪しい子2個 ÷ 総クエリ207回 = 0.01)。一方トンネリングは
// 1回の通信ごとに新しい名前を作るので、この比が1に近づく。

/// 「長い」とみなすラベルの文字数。
const LABEL_LONG: usize = 25;
/// 「出鱈目」とみなすエントロピー(1文字あたりのビット数)。英小文字+数字の乱数で約5.2。
const LABEL_ENTROPY: f64 = 3.5;
/// 1つの親の下にこれだけの数が揃ってはじめて疑う。
const LABEL_MIN_DISTINCT: usize = 10;
/// ユニークな名前 ÷ 問い合わせ回数。これが決め手(1に近い = 毎回ちがう名前)。
const LABEL_MIN_UNIQUE_RATIO: f64 = 0.7;
/// 親としてまとめる深さ(後ろから何ラベルか)。`a.b.example.com` を `b.example.com` にまとめる。
const LABEL_PARENT_DEPTH: usize = 3;

/// 候補から外す末尾。通信そのものではないものを落とす。
///
/// 逆引き(`*.in-addr.arpa` / `*.ip6.arpa`)は、Pi-hole 自身がローカル端末の名前を
/// 引くために出しているもので、ローカルの名前解決を持っていない環境では必ず NXDOMAIN
/// になる。落とさないと「存在しない名前として何回返っている」の一覧が、
/// 家の中の端末の逆引きだけで埋まる(実際にそうなった)。
const EXCLUDED_SUFFIXES: &[&str] = &[".in-addr.arpa", ".ip6.arpa", ".arpa"];

/// 候補が挙がった理由。1つのドメインに複数付く(初出かつNXDOMAIN多発、など)。
///
/// 理由は必ず画面に出す。 「なぜこれが並んでいるのか」が分からない一覧は、
/// 誤検知なのか本物なのかを人が判断できず、結局全部無視されることになる。
#[derive(Debug, Clone, Serialize)]
pub struct Reason {
    /// 機械が見分ける印(画面の色分けに使う)
    pub kind: &'static str,
    /// 人が読む説明
    pub detail: String,
    /// この理由の元になった通信を Pi-hole 側で絞り込むための条件。
    /// 画面はこれをそのままクエリログのリンクにする(下記 [`QueryFilter`])。
    pub filter: QueryFilter,
}

/// Pi-hole のクエリログを絞り込む条件。
///
/// フィールド名は Pi-hole の GET パラメータ名そのまま(`domain` / `client_ip` /
/// `type` / `reply`)。画面はキーと値を URL エンコードして並べるだけでよく、
/// 「どう絞り込むか」の知識はこちら側に閉じる —— 手ごとに違う条件
/// (種別なら `type`、NXDOMAIN なら `reply`)を画面に散らかさないため。
///
/// ドメインは `*` を使える(`/api/queries` の仕様。親でまとめた
/// 「ラベルの形」だけは `*.親` で引く —— 親そのものは引かれていないことが多い)。
#[derive(Debug, Clone, Default, Serialize)]
pub struct QueryFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_ip: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub qtype: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
}

impl QueryFilter {
    fn domain(domain: &str) -> Self {
        Self {
            domain: Some(domain.to_string()),
            ..Self::default()
        }
    }

    fn client(mut self, client: &str) -> Self {
        self.client_ip = Some(client.to_string());
        self
    }

    fn qtype(mut self, qtype: &str) -> Self {
        self.qtype = Some(qtype.to_string());
        self
    }

    fn reply(mut self, reply: &str) -> Self {
        self.reply = Some(reply.to_string());
        self
    }
}

/// 候補の選び方1つぶんの説明。画面に出す文をコードから作る ——
/// しきい値を散文で別に書くと、定数をいじったときに説明だけが古くなる
/// (「なぜ挙がったか」が読めない一覧は結局無視される、と同じ理由で、
/// 「どう選んでいるか」が古い一覧も信用されない)。
#[derive(Debug, Clone, Serialize)]
pub struct Method {
    /// [`Reason::kind`] と同じ印(画面が理由の札と同じ色を当てる)
    pub kind: &'static str,
    pub label: &'static str,
    /// 何を捕まえたいか
    pub catches: &'static str,
    /// どう判定しているか(しきい値つき)
    pub how: String,
    /// 見える範囲・誤検知の出方。「静かなのか、材料が無いのか」を読み分けさせる
    pub caveat: String,
}

/// 手の一覧を、いまのしきい値そのままで組み立てる。
fn methods(backfill_days: i64) -> Vec<Method> {
    vec![
        Method {
            kind: "first_seen",
            label: "初出",
            catches: "新しいトラッカー、入り込んだ直後の通信",
            how: "日ごとのドメイン集計(過去ぶんも遡って埋めてある)を見て、\
                  はじめて見た日がいま見ている範囲の中にあるものを挙げる"
                .to_string(),
            caveat: format!(
                "遡って埋めてあるのは{backfill_days}日ぶんなので、それより前に出ていた\
                 ドメインも「初出」に見えることがある。CDNがシャードを増やしただけのこともある"
            ),
        },
        Method {
            kind: "nxdomain",
            label: "NXDOMAIN多発",
            catches: "生成ドメイン(DGA)、死んだ接続先、設定ミス",
            how: format!("存在しない名前として{NXDOMAIN_MIN}回以上返っている"),
            caveat: "Pi-hole 自身がブロックのためにNXDOMAINを返すことがあり\
                     (iCloud Private Relay の mask.icloud.com など)、それも数に入る ——\
                     行の「ブロック済み」の印で見分けられる。\
                     逆引き(*.in-addr.arpa / *.ip6.arpa)は必ずNXDOMAINになるので除いてある。\
                     生のクエリが残っている範囲でしか見えない"
                .to_string(),
        },
        Method {
            kind: "rare_qtype",
            label: "珍しいクエリ種別",
            catches: "DNSトンネリング(TXT・NULL・ANY を大量に使う)",
            how: format!(
                "平常の種別({})に無いものを{RARE_QTYPE_MIN}回以上引いている",
                COMMON_QTYPES.join(" / ")
            ),
            caveat: "新しい機器が正常な動作で珍しい種別を引くこともある。\
                     生のクエリが残っている範囲でしか見えない"
                .to_string(),
        },
        Method {
            kind: "beacon",
            label: "周期(ビーコン)",
            catches: "C2との定時通信、常時動いているテレメトリ",
            how: format!(
                "同じ端末が同じドメインを{}回以上引いていて、間隔のばらつき\
                 (標準偏差÷中央値)が{BEACON_MAX_CV}以下。端末ごとに分けて数える\
                 (混ぜると間隔が乱れて周期が消える)。間隔の中央値が{}秒未満のものと、\
                 {}秒以内に続けて飛ぶA/AAAA/HTTPSは数えない",
                BEACON_MIN_INTERVALS + 1,
                BEACON_MIN_MEDIAN_SECS as i64,
                BEACON_SAME_SHOT_SECS as i64,
            ),
            caveat: "ルーターの死活確認・NASの更新確認・テレメトリなど、\
                     正常でも規則正しく鳴るものは多い(実測: 61秒・80秒・10分・29分)。\
                     生のクエリが残っている範囲でしか見えない"
                .to_string(),
        },
        Method {
            kind: "label_shape",
            label: "ラベルの形",
            catches: "DNSトンネリング、DGA",
            how: format!(
                "1つの親({LABEL_PARENT_DEPTH}ラベル)の下に、{LABEL_LONG}文字以上で\
                 1文字あたりの情報量が{LABEL_ENTROPY}ビット以上の名前が{LABEL_MIN_DISTINCT}種類以上あり、\
                 ちがう名前の数÷問い合わせ回数が{LABEL_MIN_UNIQUE_RATIO}以上(＝毎回ちがう名前を引いている)"
            ),
            caveat: "長くて出鱈目な名前はCDNとクラウドも大量に使う(実測で105個の親が\
                     引っかかり、そのすべてが正常だった)。分かれ目は最後の比 ——\
                     CDNは同じ名前を何度も引くので比が小さくなる。\
                     この比には生のクエリの件数が要るので、窓が埋まるまでは何も挙がらない"
                .to_string(),
        },
    ]
}

/// 怪しい通信の候補1件。
///
/// `domain` / `count` / `reviewed` / `note` はブロック済み一覧と同じ形にしてある ——
/// 画面の行・詳細モーダル・「AIに聞く」・メモがそのまま使い回せる。
#[derive(Debug, Clone, Serialize)]
pub struct WatchItem {
    pub domain: String,
    pub count: i64,
    pub reviewed: bool,
    pub note: String,
    /// 「詳しく調べる」の結果（詳細画面でメモの上に出す）
    pub research: String,
    pub researched_at: String,
    /// 分類のタグ(発生元のアプリ名など)
    pub tags: Vec<String>,
    pub reasons: Vec<Reason>,
    /// この窓で引いた端末(分かる範囲。件数の多い順)。1台ずつ件数と期間を持つ
    pub clients: Vec<ClientActivity>,
    /// はじめて見た時刻(unix秒)。遡り取り込みで埋めた行は日単位の粒度
    pub first_seen: i64,
    /// この窓で実際に通信が起きていた期間(unix秒。1件も無ければ 0)。
    /// 件数だけでは「1時間に集中したのか、2日かけて散ったのか」が分からない
    pub active_from: i64,
    pub active_to: i64,
    /// `count` のうち Pi-hole が止めたぶん。画面が「ブロック済み」の印を出す。
    ///
    /// この一覧はブロックの有無で絞っていない(名前を引いた記録すべてを見る)ので、
    /// 止められているドメインも挙がる —— 実際 `mask.icloud.com` は Pi-hole が
    /// `SPECIAL_DOMAIN` で止めて NXDOMAIN を返しており、その NXDOMAIN が
    /// 「多発」に当たって候補になる。落とさずに印を付けるのは、
    /// 止められているのに端末が鳴らし続けていること自体が見たい情報だから
    /// (ブロックが効いていても、その端末は接続先を諦めていない)。
    pub blocked_count: i64,
}

/// 画面に渡す一式。数字だけでなく「どこまで見えているか」も返す ——
/// 貯まっていない時期に「初出0件」と出ると、平穏なのか単に材料が無いのか区別できない。
#[derive(Debug, Clone, Serialize)]
pub struct WatchResult {
    /// 遡り取り込みが済んでいるか。偽なら初出の判定は当てにならない
    /// (過去を知らないので、すべてが初出に見える)
    pub ready: bool,
    pub backfill_days: i64,
    pub window_hours: i64,
    /// 設定されている基準日時(unix秒)。未設定なら `None`
    pub baseline: Option<i64>,
    /// 実際に見はじめる時刻(unix秒)。基準日時が古すぎるときは丸めた後の値
    pub since: i64,
    /// 見ている範囲の終わり(unix秒 = いま)。画面が持つ時刻を使わない ——
    /// 理由のリンクに載せる範囲は、判定に使った範囲とそろっている必要がある
    pub until: i64,
    /// サーバのいま(unix秒)。画面が「まだ続いている通信か」を判定するのに使う。
    /// `until` と同じ値だが名前で役割を分ける —— あちらは「判定に使った範囲の終わり」で、
    /// 窓の決め方を変えれば別の値になりうる。ブロック済みの一覧も同じ名前で返す
    pub now: i64,
    /// 基準日時が遡り取り込みの範囲より古くて丸めたか。
    /// 丸めたことは画面に出す —— 黙って狭めると「設定した日から見ているつもり」で
    /// 実際は違う、という食い違いが起きる
    pub baseline_clamped: bool,
    /// 生のクエリが実際に何時間ぶん貯まっているか(NXDOMAIN・種別はこの範囲しか見ていない)
    pub data_hours: f64,
    pub total_domains: i64,
    /// この窓で観測したクエリ種別と件数(平常の形。画面に出して判断材料にする)
    pub qtypes: Vec<(String, i64)>,
    /// 候補の選び方(しきい値つき)。画面の「どうやって候補を選んでいるか」に出す
    pub methods: Vec<Method>,
    /// Pi-hole の管理画面の URL。理由の札のリンクを組むのに使う
    /// (空なら画面はリンクにしない)
    pub pihole_url: String,
    pub items: Vec<WatchItem>,
}

/// 候補を組み立てる。
pub async fn candidates(db: &Db, now: f64, pihole_url: &str) -> Result<WatchResult> {
    let backfill_days = db.backfilled_days().await?;
    let stats = db.ingest_stats().await?;

    // 基準日時があればそこから、無ければ既定の窓から見る。
    // 遡り取り込みの範囲より前には戻れない(初出の判定に使う `first_seen` を
    // そこまでしか知らないので、それ以前は「はじめて見た」が嘘になる)
    let baseline = db.setting(BASELINE_KEY).await?.and_then(|v| v.parse::<i64>().ok());
    let oldest_allowed = now - (backfill_days.max(1) as f64) * 86_400.0;
    let (since_ts, baseline_clamped) = match baseline {
        Some(at) if (at as f64) < oldest_allowed => (oldest_allowed, true),
        Some(at) => (at as f64, false),
        None => (now - WINDOW_SECS, false),
    };
    let since_secs = since_ts as i64;

    // 理由をドメインごとに束ねる。同じドメインを手ごとに何度も並べない ——
    // 一覧が重複で埋まると、件数が実際より多く見える
    let mut reasons: HashMap<String, Vec<Reason>> = HashMap::new();
    let mut first_seen: HashMap<String, i64> = HashMap::new();

    // ① 初出
    for (domain, seen, _total) in db.first_seen_since(since_secs).await? {
        first_seen.insert(domain.clone(), seen);
        let filter = QueryFilter::domain(&domain);
        reasons.entry(domain).or_default().push(Reason {
            kind: "first_seen",
            detail: format!("{}にはじめて見た", ago(now - seen as f64)),
            filter,
        });
    }

    // ② NXDOMAIN多発
    for (domain, n) in db.nxdomain_since(since_ts, NXDOMAIN_MIN).await? {
        let filter = QueryFilter::domain(&domain).reply("NXDOMAIN");
        reasons.entry(domain).or_default().push(Reason {
            kind: "nxdomain",
            detail: format!("存在しない名前として{n}回返っている"),
            filter,
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
        if n < RARE_QTYPE_MIN {
            continue;
        }
        let filter = QueryFilter::domain(&domain).qtype(&qtype);
        reasons.entry(domain).or_default().push(Reason {
            kind: "rare_qtype",
            detail: format!("珍しい種別 {qtype} を{n}回引いている"),
            filter,
        });
    }

    // ④ 周期(ビーコン)
    for b in beacons(db.timeline_since(since_ts).await?) {
        let filter = QueryFilter::domain(&b.domain).client(&b.client);
        reasons.entry(b.domain).or_default().push(Reason {
            kind: "beacon",
            detail: format!(
                "{} が {}おきに{}回、規則正しく鳴っている",
                b.client,
                interval_text(b.median_secs),
                b.observations
            ),
            filter,
        });
    }

    // ⑤ ラベルの形(トンネリング・DGA)
    for t in tunneling(db.domain_query_counts_since(since_ts).await?) {
        // 親そのものは引かれていないので、子をまとめて見せる(`*` はPi-holeが解釈する)
        let filter = QueryFilter::domain(&format!("*.{}", t.parent));
        reasons.entry(t.parent).or_default().push(Reason {
            kind: "label_shape",
            detail: format!(
                "毎回ちがう長い名前を{}個引いている(同じ名前を繰り返していない)",
                t.distinct
            ),
            filter,
        });
    }

    // 件数と端末はまとめて1回で引く(ドメインごとに問い合わせない)
    let domains: Vec<String> = reasons.keys().cloned().collect();
    // 素通りしたぶんも数える(この一覧はブロックの有無で絞っていない)
    let activity = db.domain_activity_since(since_ts, domains, false).await?;
    let records = db.records().await?;

    let mut items: Vec<WatchItem> = reasons
        .into_iter()
        .filter(|(domain, _)| !is_excluded(domain))
        .map(|(domain, reasons)| {
            let seen = activity.get(&domain).cloned().unwrap_or_default();
            let record = records.get(&domain);
            WatchItem {
                first_seen: first_seen.get(&domain).copied().unwrap_or(0),
                active_from: seen.first_ts as i64,
                active_to: seen.last_ts as i64,
                blocked_count: seen.blocked,
                count: seen.count,
                reviewed: record.map(|r| r.reviewed).unwrap_or(false),
                note: record.map(|r| r.note.clone()).unwrap_or_default(),
                research: record.map(|r| r.research.clone()).unwrap_or_default(),
                researched_at: record.map(|r| r.researched_at.clone()).unwrap_or_default(),
                tags: record.map(|r| r.tags.clone()).unwrap_or_default(),
                reasons,
                clients: seen.clients,
                domain,
            }
        })
        .collect();

    // 理由の多いものを上に。 1つの手に引っかかっただけのものより、
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
        // 実際に見ている長さ(基準日時があればそこからの時間)
        window_hours: (((now - since_ts) / 3600.0).round() as i64).max(0),
        baseline,
        since: since_secs,
        until: now as i64,
        now: now as i64,
        baseline_clamped,
        data_hours,
        total_domains: stats.domains,
        qtypes,
        methods: methods(backfill_days),
        pihole_url: pihole_url.to_string(),
        items,
    })
}

/// 見つかった周期。
struct Beacon {
    domain: String,
    /// どの端末が鳴らしているか。 一覧に出す「この窓で引いた端末」は複数並ぶことが
    /// あるので、周期を持っているのがどれかは理由の側に書く
    client: String,
    median_secs: f64,
    observations: usize,
}

/// 端末ごとに間隔の揃い方を測り、機械的に鳴っているものを返す。
///
/// 入力は (ドメイン, 端末, 時刻) をその順に並べたもの(`db.timeline_since`)。
/// 隣り合う同じ組を畳んで進むので、全部をメモリに持ち直さずに済む。
fn beacons(timeline: Vec<(String, String, f64)>) -> Vec<Beacon> {
    let mut out: Vec<Beacon> = Vec::new();
    let mut idx = 0usize;
    while idx < timeline.len() {
        let (domain, client, _) = &timeline[idx];
        let mut end = idx;
        while end < timeline.len() && timeline[end].0 == *domain && timeline[end].1 == *client {
            end += 1;
        }
        let times: Vec<f64> = timeline[idx..end].iter().map(|t| t.2).collect();
        if !is_excluded(domain) {
            if let Some((median, n)) = periodicity(&times) {
                // 同じドメインを複数の端末が鳴らしている場合は、一番揃っているものだけ残す
                if !out.iter().any(|b| b.domain == *domain) {
                    out.push(Beacon {
                        domain: domain.clone(),
                        client: client.clone(),
                        median_secs: median,
                        observations: n,
                    });
                }
            }
        }
        idx = end;
    }
    out
}

/// 時刻の並びが「機械的に等間隔」かを見る。等間隔なら (間隔の中央値, 観測回数) を返す。
fn periodicity(times: &[f64]) -> Option<(f64, usize)> {
    let mut gaps: Vec<f64> = Vec::new();
    for w in times.windows(2) {
        let gap = w[1] - w[0];
        // 同時に飛ぶ A/AAAA/HTTPS は1回の通信なので畳む
        if gap > BEACON_SAME_SHOT_SECS {
            gaps.push(gap);
        }
    }
    if gaps.len() < BEACON_MIN_INTERVALS {
        return None;
    }
    let median = median_of(&gaps);
    if median < BEACON_MIN_MEDIAN_SECS {
        return None;
    }
    let mean = gaps.iter().sum::<f64>() / gaps.len() as f64;
    let var = gaps.iter().map(|g| (g - mean).powi(2)).sum::<f64>() / gaps.len() as f64;
    let cv = var.sqrt() / median;
    (cv <= BEACON_MAX_CV).then_some((median, gaps.len() + 1))
}

fn median_of(values: &[f64]) -> f64 {
    let mut v = values.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = v.len() / 2;
    if v.len() % 2 == 0 {
        (v[mid - 1] + v[mid]) / 2.0
    } else {
        v[mid]
    }
}

/// 見つかったトンネリングらしい親ドメイン。
struct Tunneling {
    parent: String,
    distinct: usize,
}

/// 「1つの親の下に、毎回ちがう長くて出鱈目な名前が並んでいる」ものを返す。
fn tunneling(counts: Vec<(String, i64)>) -> Vec<Tunneling> {
    // 親 → (怪しい子の数, その子たちの問い合わせ回数の合計)
    let mut by_parent: HashMap<String, (usize, i64)> = HashMap::new();
    for (domain, n) in counts {
        if is_excluded(&domain) {
            continue;
        }
        let labels: Vec<&str> = domain.split('.').collect();
        if labels.len() <= LABEL_PARENT_DEPTH {
            continue;
        }
        let label = labels[0];
        if label.len() < LABEL_LONG || entropy(label) < LABEL_ENTROPY {
            continue;
        }
        let parent = labels[labels.len() - LABEL_PARENT_DEPTH..].join(".");
        let e = by_parent.entry(parent).or_insert((0, 0));
        e.0 += 1;
        e.1 += n;
    }

    by_parent
        .into_iter()
        .filter(|(_, (distinct, queries))| {
            // ここが CDN との分かれ目。 同じ名前を繰り返し引いていれば比は小さくなる
            *distinct >= LABEL_MIN_DISTINCT
                && *queries > 0
                && (*distinct as f64 / *queries as f64) >= LABEL_MIN_UNIQUE_RATIO
        })
        .map(|(parent, (distinct, _))| Tunneling { parent, distinct })
        .collect()
}

/// 1文字あたりの情報量(シャノンエントロピー)。出鱈目な文字列ほど大きい。
fn entropy(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let mut counts: HashMap<char, usize> = HashMap::new();
    for ch in s.chars() {
        *counts.entry(ch).or_insert(0) += 1;
    }
    let n = s.chars().count() as f64;
    -counts
        .values()
        .map(|&c| {
            let p = c as f64 / n;
            p * p.log2()
        })
        .sum::<f64>()
}

/// 間隔を「10分」「1.5時間」のように読ませる。
///
/// 1時間ちょうどを「60分」と出さない。 周期は 60秒 / 5分 / 30分 / 1時間 のような
/// きりのよい値になることが多く、そこが読み取りやすい単位で出てほしい。
fn interval_text(secs: f64) -> String {
    if secs < 90.0 {
        return format!("{}秒", secs.round() as i64);
    }
    if secs < 3600.0 {
        return format!("{}分", (secs / 60.0).round() as i64);
    }
    let hours = secs / 3600.0;
    // 1.5時間のような半端は小数1桁で出し、ちょうどのときは整数で出す
    if (hours - hours.round()).abs() < 0.05 {
        format!("{}時間", hours.round() as i64)
    } else {
        format!("{hours:.1}時間")
    }
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
        // 負の経過を「-1分前」と出さない(時刻のずれで未来になることがある)
        assert_eq!(ago(-5.0), "たった今");
    }

    #[test]
    fn reverse_lookups_are_excluded() {
        // Pi-hole 自身がローカル端末の名前を引いているだけで、通信ではない。
        // ローカルの名前解決が無ければ必ず NXDOMAIN になるので、落とさないと
        // 「NXDOMAIN多発」が家の中の逆引きで埋まる
        assert!(is_excluded("4.2.0.192.in-addr.arpa"));
        assert!(is_excluded("1.0.0.0.0.0.0.0.ip6.arpa"));
        // 普通のドメインは落とさない(部分一致で巻き込まないこと)
        assert!(!is_excluded("example.com"));
        assert!(!is_excluded("arpa-labs.example.com"));
    }

    fn ticks(start: f64, gap: f64, n: usize, jitter: f64) -> Vec<f64> {
        (0..n)
            .map(|i| start + gap * i as f64 + if i % 2 == 0 { jitter } else { -jitter })
            .collect()
    }

    #[test]
    fn periodicity_finds_machine_like_intervals() {
        // 60秒ちょうど → 周期
        let (median, n) = periodicity(&ticks(0.0, 60.0, 10, 0.0)).expect("等間隔を拾えていない");
        assert!((median - 60.0).abs() < 1.0);
        assert_eq!(n, 10);
        // ばらつきが大きい(人の操作) → 周期ではない
        assert!(periodicity(&ticks(0.0, 60.0, 10, 40.0)).is_none());
        // 回数が足りない → 偶然そろっただけかもしれないので拾わない
        assert!(periodicity(&ticks(0.0, 60.0, 4, 0.0)).is_none());
    }

    #[test]
    fn periodicity_ignores_retry_bursts_and_simultaneous_shots() {
        // ブロックされた名前の再試行(数秒間隔)は「規則正しい通信」と呼ばない
        assert!(periodicity(&ticks(0.0, 3.0, 12, 0.0)).is_none());
        // A/AAAA/HTTPS が同時に飛ぶぶんは畳む —— 畳まないと間隔0が混じって周期が壊れる
        let mut t = Vec::new();
        for i in 0..10 {
            let base = i as f64 * 60.0;
            t.extend([base, base + 0.01, base + 0.02]);
        }
        let (median, _) = periodicity(&t).expect("同時分を畳めていない");
        assert!((median - 60.0).abs() < 1.0, "median={median}");
    }

    #[test]
    fn beacons_split_by_client() {
        // 同じドメインでも端末ごとに数える。 混ぜると間隔が乱れて周期が消える。
        // 2台が30秒ずれて60秒おきに鳴らすと、混ぜた列は 30/30/30… に見えてしまう
        let mut timeline = Vec::new();
        for i in 0..10 {
            timeline.push(("x.example.com".into(), "10.0.0.1".into(), i as f64 * 60.0));
        }
        for i in 0..10 {
            timeline.push(("x.example.com".into(), "10.0.0.2".into(), i as f64 * 60.0 + 30.0));
        }
        timeline.sort_by(|a: &(String, String, f64), b| {
            a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.total_cmp(&b.2))
        });
        let found = beacons(timeline);
        assert_eq!(found.len(), 1, "同じドメインは1件にまとめる");
        assert!((found[0].median_secs - 60.0).abs() < 1.0, "端末ごとの60秒を拾えていない");
    }

    #[test]
    fn tunneling_ignores_cdn_hostnames() {
        // CDNは同じ名前を何度も引く。 実測でもスマートスピーカーは 怪しい子2個 ÷ 総クエリ207回 = 0.01
        // だった。ここを分けられないと、CDNとクラウドで一覧が埋まる(実測105件)
        // ゼロ埋めの数値では駄目（'0' ばかりでエントロピーが低く、
        // そもそも「出鱈目な名前」の条件を満たさない）ので、散らばった文字列を作る
        let label = |i: usize| -> String {
            let mut state = (i as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            (0..32)
                .map(|_| {
                    // 混ぜてから1文字取る（規則的な並びだとエントロピーがしきい値付近に貼り付く）
                    state ^= state >> 33;
                    state = state.wrapping_mul(0xFF51_AFD7_ED55_8CCD);
                    let v = (state >> 17) % 36;
                    if v < 10 {
                        (b'0' + v as u8) as char
                    } else {
                        (b'a' + (v - 10) as u8) as char
                    }
                })
                .collect()
        };
        let cdn: Vec<(String, i64)> = (0..20)
            .map(|i| (format!("{}.xz.fbcdn.net", label(i)), 50))
            .collect();
        assert!(tunneling(cdn).is_empty(), "繰り返し引かれる名前を拾ってはいけない");

        // 毎回ちがう名前を1回ずつ = トンネリングの形
        let tunnel: Vec<(String, i64)> = (0..20)
            .map(|i| (format!("{}.t.evil.example", label(i)), 1))
            .collect();
        let found = tunneling(tunnel);
        assert_eq!(found.len(), 1, "毎回ちがう名前を拾えていない");
        assert_eq!(found[0].distinct, 20);
    }

    #[test]
    fn entropy_separates_random_from_words() {
        assert!(entropy("aaaaaaaaaaaaaaaa") < 1.0, "同じ文字だけなら低い");
        assert!(entropy("abf9a92a132203c964dfab9a7b305fb2") > LABEL_ENTROPY, "16進の乱数は高い");
        assert!(entropy("") == 0.0);
    }

    #[test]
    fn interval_text_reads_naturally() {
        assert_eq!(interval_text(60.0), "60秒");
        assert_eq!(interval_text(600.0), "10分");
        assert_eq!(interval_text(3600.0), "1時間");
        assert_eq!(interval_text(5400.0), "1.5時間");
    }

    #[test]
    fn reason_filter_uses_pihole_parameter_names() {
        // キー名は Pi-hole の GET パラメータそのもの。 画面はこれをそのまま
        // `/admin/queries.lp?...` に並べるので、名前を変えるとリンクが黙って効かなくなる
        // (押しても絞り込まれていないクエリログが開く)
        let full = QueryFilter::domain("example.com")
            .client("10.0.0.5")
            .qtype("TXT")
            .reply("NXDOMAIN");
        let json = serde_json::to_string(&full).expect("filterをJSONにできない");
        for expected in [
            "\"domain\":\"example.com\"",
            "\"client_ip\":\"10.0.0.5\"",
            "\"type\":\"TXT\"",
            "\"reply\":\"NXDOMAIN\"",
        ] {
            assert!(json.contains(expected), "{expected} が入っていない: {json}");
        }
        // 持っていない条件は出さない。 空の値を並べると、絞り込みが効かないどころか
        // 「そのドメインで種別が空のクエリ」を探しに行く
        assert_eq!(
            serde_json::to_string(&QueryFilter::domain("a.example.com")).unwrap(),
            "{\"domain\":\"a.example.com\"}"
        );
    }

    #[test]
    fn methods_explain_every_kind_with_its_threshold() {
        let methods = methods(30);
        let kinds: Vec<&str> = methods.iter().map(|m| m.kind).collect();
        // 手を足したのに説明を足し忘れると、画面の「どうやって選んでいるか」に
        // 出てこない理由が並ぶ
        for kind in ["first_seen", "nxdomain", "rare_qtype", "beacon", "label_shape"] {
            assert!(kinds.contains(&kind), "{kind} の説明が無い");
        }
        // しきい値は定数から組み立てる(散文で書き写すと、変えたときに説明だけ古くなる)
        let how = |kind: &str| {
            methods
                .iter()
                .find(|m| m.kind == kind)
                .map(|m| m.how.clone())
                .unwrap_or_default()
        };
        assert!(how("nxdomain").contains(&NXDOMAIN_MIN.to_string()));
        assert!(how("rare_qtype").contains(&RARE_QTYPE_MIN.to_string()));
        assert!(how("beacon").contains(&BEACON_MAX_CV.to_string()));
        assert!(how("label_shape").contains(&LABEL_MIN_UNIQUE_RATIO.to_string()));
    }

    #[test]
    fn common_qtypes_cover_the_normal_shape() {
        // 実測でこの環境の99.9%を占めていた種別。ここから漏らすと、
        // 普通の通信が毎日「珍しい種別」として挙がってくる
        for t in ["A", "AAAA", "HTTPS", "PTR", "SVCB"] {
            assert!(COMMON_QTYPES.contains(&t), "{t} が平常の種別から漏れている");
        }
        // トンネリングに使われる種別は平常に入れない(入れると検出できなくなる)
        for t in ["TXT", "NULL", "ANY"] {
            assert!(!COMMON_QTYPES.contains(&t), "{t} を平常に入れてはいけない");
        }
    }
}

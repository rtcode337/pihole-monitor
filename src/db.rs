//! SQLite操作(domain_notes・settings テーブル)。Pi-holeには一切書き込まない。
//!
//! rusqliteは同期APIなので、実際のクエリは [`tokio::task::spawn_blocking`] に逃がして
//! 非同期ランタイムのワーカースレッドを塞がないようにしている。接続は1本を
//! `Mutex` で共有する(この規模ではプールを持つ必要がない)。

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

/// ドメイン1件についてこちらが持っている記録。
///
/// メモと確認済みは独立している。 行があること = 確認済み だった頃は、
/// メモを残すために確認済みにするしかなかった —— 「まだ判断していないが調べた内容は
/// 残したい」(まとめてAIに聞いた結果がまさにそれ)が表せなかった。
#[derive(Debug, Clone, Default)]
pub struct DomainRecord {
    pub note: String,
    /// 調べて納得したか。状態はこれ1つ(かつては「問題あり / 問題なし」に分けていたが、
    /// 見返すときに要るのは「もう見たか」だけだった)。
    pub reviewed: bool,
    /// 「詳しく調べる」の結果。メモとは別に持つ ——
    /// メモは人が書く（あるいは「一括AIメモ生成」が書く）ものなので、
    /// 調査結果で黙って上書きすると、書いた判断が消える。
    /// 画面では詳細のメモの上に出す。
    pub research: String,
    /// 調べた日時（ISO8601）。いつの調査かが分からないと鵜呑みにできない。
    pub researched_at: String,
    /// 分類のタグ(発生元のアプリ名など)。並びは付けた順。
    /// メモとは別に持つ —— メモは文章で、タグは絞り込みの鍵。文章の中に
    /// 埋めると同じアプリを同じ表記で書き続けることになり、揃わない
    pub tags: Vec<String>,
}

/// 1つのドメインについてこちらが観測した事実(「詳しく調べる」でAIに渡す材料)。
#[derive(Debug, Clone, Default)]
pub struct DomainProfile {
    pub first_seen: Option<i64>,
    pub last_seen: Option<i64>,
    /// 記録し始めてからの総問い合わせ回数(遡り取り込みぶんを含む)
    pub total: i64,
    /// 以下はいずれも保持期間の窓の中(生のクエリが残っている範囲)
    pub clients: Vec<(String, i64)>,
    pub statuses: Vec<(String, i64)>,
    pub replies: Vec<(String, i64)>,
    pub qtypes: Vec<(String, i64)>,
}

/// 記録が残っているドメイン1件(設定のページの「メモが残っているドメイン」に出す)。
///
/// [`DomainRecord`] と中身はほぼ同じだが、ドメイン名と更新時刻を持つのでそのまま並べられる。
/// 2つの一覧(ブロック済み・監視)はどちらも「いま出ているもの」しか並べないため、
/// 落ち着いたドメインのメモはここからしか辿れない。
#[derive(Debug, Clone, Serialize)]
pub struct NoteRow {
    pub domain: String,
    /// 最後に更新した時刻(RFC3339)
    pub updated_at: String,
    pub note: String,
    pub reviewed: bool,
    pub research: String,
    pub researched_at: String,
    pub tags: Vec<String>,
}

/// アクセス元1件の日ごとの件数(設定のページの「アクセス元の内訳」に出す)。
///
/// `counts` は返す `days` と同じ並び・同じ長さ(その日に出ていなければ 0)。
/// 画面が日付とつき合わせずに済むよう、穴埋めはここでやる。
#[derive(Debug, Clone, Serialize)]
pub struct ClientDaily {
    pub client: String,
    /// 記録している全期間の合計(表に出す日数だけの合計ではない)
    pub total: i64,
    pub counts: Vec<i64>,
}

/// アクセス元1件ぶんの観測(そのドメインを引いた端末・件数・鳴っていた期間)。
///
/// ドメインの一覧が1台ずつ「期間 アクセス元 (件数)」で出す。 かつては期間を
/// ドメイン全体で1つ出し、その後ろに端末の名前を並べていたが、2台以上あると
/// 「どちらがいつ引いたのか」が言えなかった —— 期間はドメインのもの、
/// 名前はただの並びで、両者が結びついていない。
///
/// 時刻は秒に丸めた unix秒。 応答の `active_from` / `active_to` と
/// 同じ名前・同じ意味にしてあるので、画面は同じ組み立てで期間を出せる。
#[derive(Debug, Clone, Serialize)]
pub struct ClientActivity {
    pub client: String,
    pub count: i64,
    pub active_from: i64,
    pub active_to: i64,
}

/// ドメインごとの観測(ある範囲での件数・引いた端末・通信が起きていた期間)。
///
/// ブロック済みの一覧と監視の候補が同じ形を使う。 出どころ(Pi-holeの集計 /
/// 貯めたクエリ)は違っても、画面に出す「誰が・いつからいつまで」は同じものなので、
/// 組み立ても1か所にしてある。
#[derive(Debug, Clone, Default)]
pub struct DomainActivity {
    pub count: i64,
    /// そのうち Pi-hole が止めたぶん([`BLOCKED_STATUS_SQL`])。
    /// 監視の一覧が「ブロック済み」の印を出すのに使う —— あちらはブロックの有無で
    /// 絞っていないので、止められているドメインが混ざる。落とさずに印を付ける
    /// (止められているのに端末が鳴らし続けているのは、それ自体が見たい情報)
    pub blocked: i64,
    /// 引いた端末(件数の多い順)。1台ずつ件数と期間を持つ
    pub clients: Vec<ClientActivity>,
    /// 範囲の中で最初/最後に引かれた時刻(unix秒)。1件も無ければ 0
    pub first_ts: f64,
    pub last_ts: f64,
}

/// Pi-hole が「止めた」ことを表す status の条件(SQL の断片)。
///
/// 素通りした側(`FORWARDED` / `CACHE` / `CACHE_STALE` / `IN_PROGRESS` …)を数えないための
/// もので、ブロック済みの一覧の「アクセス元」と「期間」がこれで決まる。
/// 接尾辞つき(`GRAVITY_CNAME` = CNAME の先がブロックリストに載っていた、
/// `DENYLIST_CNAME` …)も拾いたいので前方一致で書く。
/// `SPECIAL_DOMAIN` は Pi-hole 自身の特別扱い(iCloud Private Relay の
/// `mask.icloud.com` など)で、これもブロックの一種。
const BLOCKED_STATUS_SQL: &str = "(status LIKE 'GRAVITY%' OR status LIKE 'DENYLIST%' \
     OR status LIKE 'REGEX%' OR status LIKE 'EXTERNAL_BLOCKED%' OR status = 'SPECIAL_DOMAIN')";

/// 取り込みの状況(件数と、生のクエリの一番古い時刻)。
///
/// いまは遡り取り込みの完了ログにしか出していない。 画面に「どれだけ貯まっているか」を
/// 出すのは次の段(初出ドメインの一覧)の仕事なので、そこで使う。
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub struct IngestStats {
    pub queries: i64,
    pub domains: i64,
    pub oldest_ts: Option<f64>,
}

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

/// unix秒 → 日本時間の日付(YYYY-MM-DD)。
///
/// 日付の境界は日本時間で数える。 UTC のまま日付に直すと、日本の朝9時までが
/// 前日に入り、「今日の件数」が実際とずれる。JST は夏時間を持たないので固定の +9h でよい。
fn jst_day(ts: f64) -> String {
    const JST_OFFSET_SECS: i64 = 9 * 3600;
    let days = (ts as i64 + JST_OFFSET_SECS).div_euclid(86_400);
    // 1970-01-01 からの日数を暦に直す(chrono の DateTime を経由せずに済ませる)
    chrono::NaiveDate::from_num_days_from_ce_opt(days as i32 + 719_163)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

impl Db {
    /// DBファイルを開き、テーブルが無ければ作る。親ディレクトリも自動生成する
    /// (初回起動時に永続化ボリュームが空でも落ちないようにするため)。
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("データディレクトリを作成できない: {}", dir.display()))?;
        }

        let conn = Connection::open(path)
            .with_context(|| format!("DBを開けない: {}", path.display()))?;

        migrate(&conn)?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS domain_notes (
                 domain        TEXT PRIMARY KEY,
                 updated_at    TEXT NOT NULL,
                 note          TEXT,
                 reviewed      INTEGER NOT NULL DEFAULT 0,
                 research      TEXT,
                 researched_at TEXT,
                 tags          TEXT
             )",
        )
        .context("domain_notesテーブルを作成できない")?;

        // 画面から決める設定(いまは「どの AI に聞くか」だけ)。環境変数ではなく DB に持つ
        // —— 実行のたびに読むので、コンテナを作り直さなくても切り替えが効く
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                 key        TEXT PRIMARY KEY,
                 value      TEXT NOT NULL,
                 updated_at TEXT NOT NULL
             )",
        )
        .context("settingsテーブルを作成できない")?;

        // ---- 「ブロックされていない通信」を見るための蓄積(第2の柱) ----
        //
        // ブロック済みの一覧(domain_notes)と役割が違う。 あちらは Pi-hole を叩いた
        // その場の集計だが、こちらは「いつもと違うか」を言うための時系列で、
        // 比較対象が無いと何も判定できない。だから貯める。
        conn.execute_batch(
            // 生のクエリ。保持期間つきの窓(DNS_RETENTION_DAYS)。周期の検出に
            // 1件ごとの時刻が要るので、集計ではなく生で持つ。
            // 主キーは Pi-hole の id ——取りこぼさないよう窓を重ねて取るので、
            // 重複を弾く手立てが要る
            "CREATE TABLE IF NOT EXISTS dns_queries (
                 id       INTEGER PRIMARY KEY,
                 ts       REAL NOT NULL,
                 domain   TEXT NOT NULL,
                 client   TEXT NOT NULL,
                 qtype    TEXT NOT NULL,
                 status   TEXT NOT NULL,
                 reply    TEXT,
                 upstream TEXT,
                 cname    TEXT
             );
             CREATE INDEX IF NOT EXISTS dns_queries_ts ON dns_queries(ts);
             CREATE INDEX IF NOT EXISTS dns_queries_domain ON dns_queries(domain, ts);

             -- ドメインの一生。保持期間を過ぎても消さない ——
             -- 初出(はじめて見た日)の判定はこの列だけが根拠で、
             -- 生のクエリと一緒に消すと『毎日すべてが初出』になる
             CREATE TABLE IF NOT EXISTS dns_domains (
                 domain     TEXT PRIMARY KEY,
                 first_seen INTEGER NOT NULL,
                 last_seen  INTEGER NOT NULL,
                 total      INTEGER NOT NULL DEFAULT 0,
                 -- 1 なら遡り取り込みで作った行(first_seen が日単位の粒度しかない)
                 backfilled INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS dns_domains_first ON dns_domains(first_seen);

             -- クライアントごとの日次の件数。生のクエリが消えても残す ——
             -- 「この端末が急に見えなくなった(VPN・DoHに切り替わった)」を言うには
             -- 保持期間より長い並びが要る
             CREATE TABLE IF NOT EXISTS dns_client_daily (
                 day    TEXT NOT NULL,
                 client TEXT NOT NULL,
                 count  INTEGER NOT NULL,
                 PRIMARY KEY (day, client)
             )",
        )
        .context("DNS取り込み用のテーブルを作成できない")?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// ドメイン → 記録(メモ・確認済み)の対応を全件返す。
    pub async fn records(&self) -> Result<HashMap<String, DomainRecord>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT domain, note, reviewed, research, researched_at, tags FROM domain_notes",
            )?;
            let rows = stmt.query_map([], |row| {
                let domain: String = row.get(0)?;
                let note: Option<String> = row.get(1)?;
                let reviewed: i64 = row.get(2)?;
                let research: Option<String> = row.get(3)?;
                let researched_at: Option<String> = row.get(4)?;
                let tags: Option<String> = row.get(5)?;
                Ok((
                    domain,
                    DomainRecord {
                        note: note.unwrap_or_default(),
                        reviewed: reviewed != 0,
                        research: research.unwrap_or_default(),
                        researched_at: researched_at.unwrap_or_default(),
                        tags: decode_tags(tags.as_deref()),
                    },
                ))
            })?;
            Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
        })
        .await
    }

    /// 確認済み / 未確認をまとめて切り替える(1件でも同じ経路)。
    ///
    /// アクセス元ごとの日ごとの件数を、新しいほうから `days` 日ぶん返す。
    ///
    /// 材料は `dns_client_daily`(生のクエリが消えても残す表)。 だから
    /// 保持期間より長い並びが出せる —— 見たいのは「この端末が急に見えなくなった」
    /// 「ルーター経由の割合が減った」といった動きで、1日の数字だけでは言えない。
    ///
    /// 合計は全期間ぶんを返す(表に出す日数の合計ではない)。 並べ替えのキーであり、
    /// 「そもそもどれだけ喋っている相手か」は表の外まで含めて見たいため。
    pub async fn client_daily(&self, days: i64) -> Result<(Vec<String>, Vec<ClientDaily>)> {
        self.with_conn(move |conn| {
            // 出す日は「記録のある日」の新しいほうから。 今日から数えると、
            // 取り込みが止まっていた日や記録の無い日が空の列として並ぶ
            let mut stmt =
                conn.prepare("SELECT DISTINCT day FROM dns_client_daily ORDER BY day DESC LIMIT ?1")?;
            let mut shown: Vec<String> = stmt
                .query_map([days], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            shown.reverse();

            let mut totals: HashMap<String, i64> = HashMap::new();
            let mut stmt = conn.prepare("SELECT client, SUM(count) FROM dns_client_daily GROUP BY client")?;
            for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
                let (client, total) = row?;
                totals.insert(client, total);
            }

            // 表に出す日の中身。日付 → 列の位置に置き換えてから穴を埋める
            let index: HashMap<&str, usize> =
                shown.iter().enumerate().map(|(i, d)| (d.as_str(), i)).collect();
            let mut counts: HashMap<String, Vec<i64>> = HashMap::new();
            if let Some(first) = shown.first() {
                let mut stmt = conn
                    .prepare("SELECT day, client, count FROM dns_client_daily WHERE day >= ?1")?;
                let rows = stmt.query_map([first], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
                })?;
                for row in rows {
                    let (day, client, n) = row?;
                    if let Some(&i) = index.get(day.as_str()) {
                        counts
                            .entry(client)
                            .or_insert_with(|| vec![0; shown.len()])[i] = n;
                    }
                }
            }

            // 多い順。同数のときはアクセス元の名前で並びを固定する
            // (HashMap の列挙順は毎回変わるので、読み直すたびに行が入れ替わってしまう)
            let mut items: Vec<ClientDaily> = totals
                .into_iter()
                .map(|(client, total)| ClientDaily {
                    counts: counts
                        .get(&client)
                        .cloned()
                        .unwrap_or_else(|| vec![0; shown.len()]),
                    client,
                    total,
                })
                .collect();
            items.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.client.cmp(&b.client)));
            Ok((shown, items))
        })
        .await
    }

    /// 記録の残っているドメインを、更新の新しい順に1ページぶん返す(件数の総数も一緒に)。
    ///
    /// 出す対象は間引かない(一覧に出ているかどうかは見ない)。 設定のページはここを
    /// 「調べたものの控え」として出すので、絞ると控えの意味が無くなる ——
    /// 代わりにページで切る。調査結果は1件で1KBを超えることがあり、全件を1回で返すと
    /// 設定のページを開くたびに数百KBを運ぶことになる。
    /// 並べ替えはSQLに任せる(`updated_at` は同じ書式のRFC3339なので文字列順で並ぶ)。
    pub async fn notes_page(&self, offset: i64, limit: i64) -> Result<(Vec<NoteRow>, i64)> {
        self.with_conn(move |conn| {
            // 総数は別に数える。 ページの中身だけでは「あと何件あるか」が言えない
            let total: i64 = conn.query_row("SELECT COUNT(*) FROM domain_notes", [], |r| r.get(0))?;
            let mut stmt = conn.prepare(
                "SELECT domain, updated_at, note, reviewed, research, researched_at, tags
                   FROM domain_notes ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2",
            )?;
            let rows = stmt.query_map([limit, offset], |row| {
                Ok(NoteRow {
                    domain: row.get(0)?,
                    updated_at: row.get(1)?,
                    note: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    reviewed: row.get::<_, i64>(3)? != 0,
                    research: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    researched_at: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    tags: decode_tags(row.get::<_, Option<String>>(6)?.as_deref()),
                })
            })?;
            Ok((rows.collect::<rusqlite::Result<Vec<_>>>()?, total))
        })
        .await
    }

    /// `note` は渡したときだけ書く。 チェックした複数件を確認済みにするときに、
    /// 既に付いているメモ(AIに聞いた結果)を空で上書きしないため。
    /// 未確認に戻してもメモは消さない —— メモと確認済みは別の話なので、
    /// 戻したときに調べた内容まで失われては困る(メモが空の行だけ消える)。
    ///
    /// 1つのトランザクションで書く —— 途中で落ちたときに半分だけ残さないため。
    pub async fn set_reviewed(
        &self,
        domains: Vec<String>,
        reviewed: bool,
        note: Option<String>,
        tags: Option<Vec<String>>,
    ) -> Result<()> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            for domain in &domains {
                upsert(&tx, domain, note.as_deref(), Some(reviewed), tags.as_deref())?;
                if !reviewed {
                    cleanup(&tx, domain)?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// メモ(とタグ)だけ保存する(確認済みかどうかは変えない)。
    /// `tags` は渡したときだけ書く —— メモを書き直す口(AIの結果の書き戻し)から
    /// 呼ばれたときに、人が付けた分類を空で上書きしないため。
    pub async fn save_note(
        &self,
        domain: String,
        note: String,
        tags: Option<Vec<String>>,
    ) -> Result<()> {
        self.with_conn(move |conn| {
            upsert(conn, &domain, Some(&note), None, tags.as_deref())?;
            cleanup(conn, &domain)?;
            Ok(())
        })
        .await
    }

    /// タグを書く。`add` なら既にあるものに足し、そうでなければ入れ替える。
    /// まとめて付けられるようにしてある(チェックした行に同じアプリ名を付ける、が
    /// 分類の主な使い方なので)。1つのトランザクションで書く。
    pub async fn set_tags(
        &self,
        domains: Vec<String>,
        tags: Vec<String>,
        add: bool,
    ) -> Result<()> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            for domain in &domains {
                let merged = if add {
                    let current: Option<String> = tx
                        .query_row(
                            "SELECT tags FROM domain_notes WHERE domain = ?1",
                            [domain],
                            |row| row.get(0),
                        )
                        .optional()?
                        .flatten();
                    let mut merged = decode_tags(current.as_deref());
                    merged.extend(tags.iter().cloned());
                    normalize_tags(merged)
                } else {
                    tags.clone()
                };
                upsert(&tx, domain, None, None, Some(&merged))?;
                cleanup(&tx, domain)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// 使われているタグと、付いているドメインの数(付けるときの候補と絞り込みの選択肢)。
    /// 件数の多い順、同数なら名前順。
    pub async fn tag_counts(&self) -> Result<Vec<(String, i64)>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT tags FROM domain_notes WHERE COALESCE(tags, '') NOT IN ('', '[]')",
            )?;
            let mut counts: HashMap<String, i64> = HashMap::new();
            for encoded in stmt.query_map([], |row| row.get::<_, String>(0))? {
                for tag in decode_tags(Some(&encoded?)) {
                    *counts.entry(tag).or_insert(0) += 1;
                }
            }
            let mut list: Vec<(String, i64)> = counts.into_iter().collect();
            list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            Ok(list)
        })
        .await
    }

    /// メモが空のときだけ書く。書いたらその中身を、既にあれば `None` を返す。
    ///
    /// 「詳しく調べる」で使う —— 調べた以上は一覧にも一言残っていてほしいが、
    /// 人が書いた（あるいは「一括AIメモ生成」が書いた）判断は上書きしない。
    /// 読みと書きは1つの接続の中で済ませる（間に他の書き込みを挟ませない）。
    pub async fn save_note_if_empty(&self, domain: String, note: String) -> Result<Option<String>> {
        self.with_conn(move |conn| {
            let current: Option<String> = conn
                .query_row(
                    "SELECT note FROM domain_notes WHERE domain = ?1",
                    [&domain],
                    |row| row.get(0),
                )
                .optional()?;
            if current.is_some_and(|note| !note.trim().is_empty()) {
                return Ok(None);
            }
            upsert(conn, &domain, Some(&note), None, None)?;
            Ok(Some(note))
        })
        .await
    }

    /// メモをまとめて保存する(まとめてAIに聞いた結果の書き戻し)。
    /// 1つのトランザクションで書く —— 途中で落ちたときに半分だけ残さないため。
    pub async fn save_notes(&self, notes: Vec<(String, String)>) -> Result<()> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            for (domain, note) in &notes {
                upsert(&tx, domain, Some(note), None, None)?;
                cleanup(&tx, domain)?;
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// 「詳しく調べる」の結果を保存する。メモには触らない
    /// （人が書いた判断を調査結果で上書きしないため）。
    ///
    /// 入れ替える（追記ではない）。もう一度「詳しく調べる」を押すのは
    /// 調べ直しなので、前の結果と追加の質問はそこで流れる。
    pub async fn save_research(&self, domain: String, research: String) -> Result<()> {
        self.with_conn(move |conn| write_research(conn, &domain, &research))
            .await
    }

    /// いまの調査結果（無ければ空文字）。追加の質問を投げる前の材料になる。
    pub async fn research(&self, domain: String) -> Result<String> {
        self.with_conn(move |conn| {
            let value: Option<String> = conn
                .query_row(
                    "SELECT research FROM domain_notes WHERE domain = ?1",
                    [&domain],
                    |row| row.get(0),
                )
                .optional()?;
            Ok(value.unwrap_or_default())
        })
        .await
    }

    /// 追加の質問と答えを調査結果の末尾に足す。返すのは足した後の全文。
    ///
    /// 読みと書きを1つの接続の中で済ませる（間に他の書き込みを挟ませない）。
    /// 入れ替えないのは、深掘りが前の答えを踏まえた続きだから ——
    /// 上書きすると、次の質問に渡す材料（それまでのやり取り）が消える。
    pub async fn append_research(&self, domain: String, addition: String) -> Result<String> {
        self.with_conn(move |conn| {
            let current: Option<String> = conn
                .query_row(
                    "SELECT research FROM domain_notes WHERE domain = ?1",
                    [&domain],
                    |row| row.get(0),
                )
                .optional()?;
            let current = current.unwrap_or_default();
            let merged = if current.trim().is_empty() {
                addition
            } else {
                format!("{}\n\n{}", current.trim_end(), addition)
            };
            write_research(conn, &domain, &merged)?;
            Ok(merged)
        })
        .await
    }

    /// 設定を1件読む。未設定なら `None`。
    pub async fn setting(&self, key: &'static str) -> Result<Option<String>> {
        self.with_conn(move |conn| {
            let value = conn
                .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                    row.get::<_, String>(0)
                })
                .optional()?;
            Ok(value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()))
        })
        .await
    }

    /// 設定を1件書く。`None` なら行を消す(= 未設定に戻す)。
    pub async fn set_setting(&self, key: &'static str, value: Option<String>) -> Result<()> {
        let updated_at = chrono::Local::now().to_rfc3339();
        self.with_conn(move |conn| {
            match value {
                Some(value) => conn.execute(
                    "INSERT INTO settings (key, value, updated_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT(key) DO UPDATE SET
                         value=excluded.value, updated_at=excluded.updated_at",
                    (key, &value, &updated_at),
                )?,
                None => conn.execute("DELETE FROM settings WHERE key = ?1", (key,))?,
            };
            Ok(())
        })
        .await
    }

    /// ブロッキングなDB操作をブロッキング用スレッドプールで実行する。
    // ---- DNS取り込み(ingest.rs から呼ぶ) ----

    /// 取り込みの進み具合。`settings` に文字列で持つ(専用のテーブルを増やさない)。
    pub async fn ingest_cursor(&self) -> Result<Option<f64>> {
        let raw = self.setting("dns_ingest_cursor").await?;
        Ok(raw.and_then(|v| v.parse::<f64>().ok()))
    }

    pub async fn set_ingest_cursor(&self, ts: f64) -> Result<()> {
        self.set_setting("dns_ingest_cursor", Some(format!("{ts:.3}")))
            .await
    }

    /// 遡り取り込みを終えた日数(設定を伸ばしたらその差分だけやり直す)。
    pub async fn backfilled_days(&self) -> Result<i64> {
        let raw = self.setting("dns_backfilled_days").await?;
        Ok(raw.and_then(|v| v.parse::<i64>().ok()).unwrap_or(0))
    }

    pub async fn set_backfilled_days(&self, days: i64) -> Result<()> {
        self.set_setting("dns_backfilled_days", Some(days.to_string()))
            .await
    }

    /// いま持っている一番大きい id。Pi-hole 側の id がこれより小さくなったら、
    /// 向こうの DB が作り直されている(id が振り直される)ので、こちらも捨てて取り直す。
    pub async fn max_query_id(&self) -> Result<i64> {
        self.with_conn(|conn| {
            Ok(conn.query_row("SELECT COALESCE(MAX(id), 0) FROM dns_queries", [], |r| {
                r.get(0)
            })?)
        })
        .await
    }

    /// 取り込んだクエリを保存し、ドメインとクライアントの集計も同じトランザクションで更新する。
    /// 戻り値は実際に増えた件数(重複は `id` で弾かれる)。
    ///
    /// 1つのトランザクションで書く。 途中で落ちて「生のクエリだけ入って集計が古い」
    /// 状態を残さないため。
    pub async fn insert_queries(&self, records: Vec<crate::pihole::QueryRecord>) -> Result<usize> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            let mut inserted = 0usize;
            {
                let mut stmt = tx.prepare(
                    "INSERT OR IGNORE INTO dns_queries
                       (id, ts, domain, client, qtype, status, reply, upstream, cname)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                )?;
                let mut dom = tx.prepare(
                    // first_seen は小さいほうを残す(遡り取り込みが先に入れた日付を、
                    // 後から来た新しい時刻で上書きしない)
                    "INSERT INTO dns_domains (domain, first_seen, last_seen, total, backfilled)
                     VALUES (?1, ?2, ?2, ?3, 0)
                     ON CONFLICT(domain) DO UPDATE SET
                       first_seen = MIN(first_seen, excluded.first_seen),
                       last_seen  = MAX(last_seen,  excluded.last_seen),
                       total      = total + excluded.total",
                )?;
                let mut cli = tx.prepare(
                    "INSERT INTO dns_client_daily (day, client, count) VALUES (?1, ?2, ?3)
                     ON CONFLICT(day, client) DO UPDATE SET count = count + excluded.count",
                )?;

                for r in &records {
                    let n = stmt.execute(rusqlite::params![
                        r.id, r.ts, r.domain, r.client, r.qtype, r.status,
                        r.reply, r.upstream, r.cname
                    ])?;
                    // 重複した行では集計を進めない。 窓を重ねて取るので、
                    // ここで数えると同じクエリを何度も足してしまう
                    if n == 0 {
                        continue;
                    }
                    inserted += 1;
                    let secs = r.ts as i64;
                    dom.execute(rusqlite::params![r.domain, secs, 1i64])?;
                    cli.execute(rusqlite::params![jst_day(r.ts), r.client, 1i64])?;
                }
            }
            tx.commit()?;
            Ok(inserted)
        })
        .await
    }

    /// 遡り取り込み: その日に出たドメインを `dns_domains` に反映する。
    /// 生のクエリは入れない(集計しか取っていないので時刻が無い)。
    pub async fn merge_domain_counts(
        &self,
        counts: Vec<crate::pihole::DomainCount>,
        day_start: i64,
    ) -> Result<()> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO dns_domains (domain, first_seen, last_seen, total, backfilled)
                     VALUES (?1, ?2, ?2, ?3, 1)
                     ON CONFLICT(domain) DO UPDATE SET
                       first_seen = MIN(first_seen, excluded.first_seen),
                       last_seen  = MAX(last_seen,  excluded.last_seen),
                       total      = total + excluded.total",
                )?;
                for c in &counts {
                    stmt.execute(rusqlite::params![c.domain, day_start, c.count])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// 保持期間を過ぎた生のクエリを消す。`dns_domains` と `dns_client_daily` は消さない
    /// —— 初出とカバレッジの判定はそちらが根拠なので、一緒に消すと積み上げた意味が無くなる。
    pub async fn prune_queries(&self, before_ts: f64) -> Result<usize> {
        self.with_conn(move |conn| {
            Ok(conn.execute("DELETE FROM dns_queries WHERE ts < ?1", [before_ts])? )
        })
        .await
    }

    /// Pi-hole の DB が作り直されたときに、こちらの生のクエリを捨てる
    /// (id が振り直されるので、そのままだと新しい行が重複として弾かれ続ける)。
    pub async fn reset_queries(&self) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute_batch("DELETE FROM dns_queries")?;
            Ok(())
        })
        .await
    }

    // ---- 「怪しい通信」の候補を数える(watch.rs から呼ぶ) ----

    /// `since` 以降にはじめて見たドメイン(初出)。新しい順。
    ///
    /// 判定の根拠は `dns_domains.first_seen` だけで、生のクエリの保持期間には依らない
    /// (遡り取り込みが埋めた日付がそのまま効く)。
    pub async fn first_seen_since(&self, since: i64) -> Result<Vec<(String, i64, i64)>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT domain, first_seen, total FROM dns_domains
                  WHERE first_seen >= ?1 ORDER BY first_seen DESC",
            )?;
            let rows = stmt.query_map([since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// `since` 以降に NXDOMAIN が返ったドメインと件数(多い順)。
    pub async fn nxdomain_since(&self, since: f64, min_count: i64) -> Result<Vec<(String, i64)>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT domain, COUNT(*) AS n FROM dns_queries
                  WHERE ts >= ?1 AND reply = 'NXDOMAIN'
                  GROUP BY domain HAVING n >= ?2 ORDER BY n DESC",
            )?;
            let rows = stmt.query_map(rusqlite::params![since, min_count], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// `since` 以降に出たクエリ種別ごとの件数(全体)。平常の形を知るために使う ——
    /// 何が珍しいかは、その環境で実際に出ている割合からしか決められない。
    pub async fn qtype_counts_since(&self, since: f64) -> Result<Vec<(String, i64)>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT qtype, COUNT(*) AS n FROM dns_queries
                  WHERE ts >= ?1 GROUP BY qtype ORDER BY n DESC",
            )?;
            let rows = stmt.query_map([since], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// `since` 以降に、指定した種別を引いたドメインと件数。
    pub async fn domains_by_qtype_since(
        &self,
        since: f64,
        qtypes: Vec<String>,
    ) -> Result<Vec<(String, String, i64)>> {
        if qtypes.is_empty() {
            return Ok(Vec::new());
        }
        self.with_conn(move |conn| {
            let holes = vec!["?"; qtypes.len()].join(",");
            let sql = format!(
                "SELECT domain, qtype, COUNT(*) AS n FROM dns_queries
                  WHERE ts >= ?1 AND qtype IN ({holes})
                  GROUP BY domain, qtype ORDER BY n DESC"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(since)];
            for t in &qtypes {
                params.push(Box::new(t.clone()));
            }
            let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
            let rows = stmt.query_map(refs.as_slice(), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// `since` 以降の (ドメイン, 端末, 時刻) を、その3つの順に並べて返す。
    ///
    /// 周期の判定は端末ごとに分ける。 同じドメインを複数台が引くと間隔が混ざり、
    /// 機械的に等間隔でも周期が消えてしまう。
    /// 並べ替えておくことで、呼び出し側は同じ組を隣り合わせで畳める(全部をメモリに
    /// 持たずに済む)。
    pub async fn timeline_since(&self, since: f64) -> Result<Vec<(String, String, f64)>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT domain, client, ts FROM dns_queries
                  WHERE ts >= ?1 ORDER BY domain, client, ts",
            )?;
            let rows = stmt.query_map([since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// `since` 以降の、ドメインごとの問い合わせ回数。
    /// ラベルの形の判定で「同じ名前を何回引いたか」を見るために使う
    /// (CDNは同じホスト名を何度も引き、トンネリングは毎回ちがう名前を引く)。
    pub async fn domain_query_counts_since(&self, since: f64) -> Result<Vec<(String, i64)>> {
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT domain, COUNT(*) FROM dns_queries WHERE ts >= ?1 GROUP BY domain",
            )?;
            let rows = stmt.query_map([since], |r| Ok((r.get(0)?, r.get(1)?)))?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    /// ドメインごとの、`since` 以降の件数・引いたクライアント・通信が起きていた期間。
    ///
    /// どの端末が引いたかは判断材料そのもの(PCが引くのと家電が引くのでは意味が違う)。
    /// 期間(`first_ts` / `last_ts`)も同じ問い合わせで取る —— 「いつからいつまで
    /// 鳴っていたのか」は件数だけでは分からず(1,400回が1時間に集中したのか
    /// 2日かけて散ったのかで意味が違う)、ドメインごとに引き直すと数百回になる。
    ///
    /// `blocked_only` を立てると、Pi-hole が止めたクエリだけを数える
    /// ([`BLOCKED_STATUS_SQL`] を参照)。ブロック済みの一覧が使う ——
    /// 同じドメインが通ったり止まったりする(端末ごとの設定・CNAME)ので、
    /// 素通りしたぶんまで混ぜると「止められた通信の期間」ではなくなる。
    pub async fn domain_activity_since(
        &self,
        since: f64,
        domains: Vec<String>,
        blocked_only: bool,
    ) -> Result<HashMap<String, DomainActivity>> {
        if domains.is_empty() {
            return Ok(HashMap::new());
        }
        self.with_conn(move |conn| {
            let holes = vec!["?"; domains.len()].join(",");
            let blocked = if blocked_only {
                format!("AND {BLOCKED_STATUS_SQL}")
            } else {
                String::new()
            };
            let sql = format!(
                "SELECT domain, client, COUNT(*), MIN(ts), MAX(ts),
                        SUM(CASE WHEN {BLOCKED_STATUS_SQL} THEN 1 ELSE 0 END)
                   FROM dns_queries
                  WHERE ts >= ?1 AND domain IN ({holes}) {blocked}
                  GROUP BY domain, client"
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(since)];
            for d in &domains {
                params.push(Box::new(d.clone()));
            }
            let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
            let rows = stmt.query_map(refs.as_slice(), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, f64>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })?;
            let mut out: HashMap<String, DomainActivity> = HashMap::new();
            for row in rows {
                let (domain, client, n, first, last, blocked) = row?;
                let e = out.entry(domain).or_default();
                e.count += n;
                e.blocked += blocked;
                e.first_ts = if e.first_ts == 0.0 { first } else { e.first_ts.min(first) };
                e.last_ts = e.last_ts.max(last);
                // 端末ごとの件数と期間は SQL がすでに出している(`GROUP BY domain, client`)。
                // ここで畳まずにそのまま持つ —— 画面は1台ずつ期間と件数を出す
                e.clients.push(ClientActivity {
                    client,
                    count: n,
                    active_from: first as i64,
                    active_to: last as i64,
                });
            }
            // 端末は件数の多い順に並べる。 SQLの列挙順のままだと、同じ一覧を
            // 読み直すたびに並びが変わって「増えた端末」に気づけない
            for e in out.values_mut() {
                e.clients
                    .sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.client.cmp(&b.client)));
            }
            Ok(out)
        })
        .await
    }

    /// 1つのドメインについて、こちらが観測した事実をまとめて返す。
    ///
    /// 「詳しく調べる」でAIに渡す材料。 そのドメインが何かは web でも分かるが、
    /// このネットワークでどう振る舞っているかはこちらしか知らない。
    /// 両方を突き合わせて初めて「放っておいてよいか」が言える。
    pub async fn domain_profile(&self, domain: String, since: f64) -> Result<DomainProfile> {
        self.with_conn(move |conn| {
            let seen: Option<(i64, i64, i64)> = conn
                .query_row(
                    "SELECT first_seen, last_seen, total FROM dns_domains WHERE domain = ?1",
                    [&domain],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;

            let mut grouped = |sql: &str| -> rusqlite::Result<Vec<(String, i64)>> {
                let mut stmt = conn.prepare(sql)?;
                let rows = stmt.query_map(rusqlite::params![&domain, since], |r| {
                    Ok((r.get::<_, Option<String>>(0)?.unwrap_or_default(), r.get(1)?))
                })?;
                rows.collect()
            };

            Ok(DomainProfile {
                first_seen: seen.map(|s| s.0),
                last_seen: seen.map(|s| s.1),
                total: seen.map(|s| s.2).unwrap_or(0),
                clients: grouped(
                    "SELECT client, COUNT(*) n FROM dns_queries
                      WHERE domain = ?1 AND ts >= ?2 GROUP BY client ORDER BY n DESC",
                )?,
                statuses: grouped(
                    "SELECT status, COUNT(*) n FROM dns_queries
                      WHERE domain = ?1 AND ts >= ?2 GROUP BY status ORDER BY n DESC",
                )?,
                replies: grouped(
                    "SELECT reply, COUNT(*) n FROM dns_queries
                      WHERE domain = ?1 AND ts >= ?2 GROUP BY reply ORDER BY n DESC",
                )?,
                qtypes: grouped(
                    "SELECT qtype, COUNT(*) n FROM dns_queries
                      WHERE domain = ?1 AND ts >= ?2 GROUP BY qtype ORDER BY n DESC",
                )?,
            })
        })
        .await
    }

    /// 取り込みの状況(画面と表示に出す)。
    pub async fn ingest_stats(&self) -> Result<IngestStats> {
        self.with_conn(|conn| {
            let queries: i64 =
                conn.query_row("SELECT COUNT(*) FROM dns_queries", [], |r| r.get(0))?;
            let domains: i64 =
                conn.query_row("SELECT COUNT(*) FROM dns_domains", [], |r| r.get(0))?;
            let oldest: Option<f64> =
                conn.query_row("SELECT MIN(ts) FROM dns_queries", [], |r| r.get(0))?;
            Ok(IngestStats {
                queries,
                domains,
                oldest_ts: oldest,
            })
        })
        .await
    }

    async fn with_conn<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            // 別のリクエストがロック保持中にpanicしても、DB自体は壊れていないので処理を続ける
            let conn = conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            f(&conn)
        })
        .await
        .context("DB操作のタスクが異常終了した")?
    }
}

/// 調査結果を書く（入れ替え）。保存と追記で同じ1文を使う ——
/// 列の並びや時刻の付け方が2か所に分かれると、片方だけ直す事故が起きる。
fn write_research(conn: &Connection, domain: &str, research: &str) -> Result<()> {
    let now = chrono::Local::now().to_rfc3339();
    conn.execute(
        "INSERT INTO domain_notes (domain, updated_at, note, reviewed, research, researched_at)
         VALUES (?1, ?2, '', 0, ?3, ?2)
         ON CONFLICT(domain) DO UPDATE SET
             research      = excluded.research,
             researched_at = excluded.researched_at",
        rusqlite::params![domain, now, research],
    )?;
    Ok(())
}

/// 1行を書く。`note` / `reviewed` / `tags` は `None` の項目を触らない ——
/// 「メモだけ保存」と「確認済みにする」と「タグを付ける」が互いの値を巻き込まないようにするため。
fn upsert(
    conn: &Connection,
    domain: &str,
    note: Option<&str>,
    reviewed: Option<bool>,
    tags: Option<&[String]>,
) -> rusqlite::Result<()> {
    let updated_at = chrono::Local::now().to_rfc3339();
    let tags = tags.map(encode_tags);
    conn.execute(
        "INSERT INTO domain_notes (domain, updated_at, note, reviewed, tags)
         VALUES (?1, ?2, COALESCE(?3, ''), COALESCE(?4, 0), COALESCE(?5, '[]'))
         ON CONFLICT(domain) DO UPDATE SET
             updated_at = excluded.updated_at,
             note       = COALESCE(?3, domain_notes.note),
             reviewed   = COALESCE(?4, domain_notes.reviewed),
             tags       = COALESCE(?5, domain_notes.tags)",
        rusqlite::params![domain, updated_at, note, reviewed, tags],
    )?;
    Ok(())
}

/// タグの列の形は JSON の配列(`["YouTube","Google"]`)。区切り文字で繋ぐと、
/// 名前に区切りが混ざったときに壊れる(設定の `AiChoice` と同じ理由)。
fn encode_tags(tags: &[String]) -> String {
    serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string())
}

/// 読めない値は空にする(画面から付け直せる)。
fn decode_tags(encoded: Option<&str>) -> Vec<String> {
    encoded
        .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
        .map(normalize_tags)
        .unwrap_or_default()
}

/// 1つのタグの長さの上限(文字数)。行に札として並べるので、長いと一覧が読めなくなる
pub const TAG_MAX_CHARS: usize = 40;
/// 1つのドメインに付けられるタグの数の上限
pub const TAGS_MAX_PER_DOMAIN: usize = 20;

/// 空白を落とし、重複を除き、上限で切る。順番は保つ(付けた順に並べるため)。
/// 書き込む側も読む側もここを通す —— どちらか片方だけだと、古い行の形が違ったときに
/// 画面に同じタグが2つ並ぶ。
pub fn normalize_tags(tags: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    tags.into_iter()
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
        .map(|t| t.chars().take(TAG_MAX_CHARS).collect::<String>())
        .filter(|t| !t.is_empty())
        .filter(|t| seen.insert(t.clone()))
        .take(TAGS_MAX_PER_DOMAIN)
        .collect()
}

/// 何も持たなくなった行を消す(未確認 + メモ空 + タグ無し)。
/// 残しても害は無いが、`/api/domains` が件数0の行として一覧に足してしまう。
fn cleanup(conn: &Connection, domain: &str) -> rusqlite::Result<()> {
    // 調査結果が入っている行は消さない。 メモが空でも、30秒かけて調べた結果は
    // 残しておきたい（消すと押し直すまで戻らない）
    conn.execute(
        "DELETE FROM domain_notes
         WHERE domain = ?1 AND reviewed = 0
           AND COALESCE(TRIM(note), '') = '' AND COALESCE(TRIM(research), '') = ''
           AND COALESCE(tags, '') IN ('', '[]')",
        [domain],
    )?;
    Ok(())
}

/// 古いDBを今の形に合わせる。マイグレーション機構は持たない(個人運用なので、
/// 起動時にこの関数が差分を埋める)。どの手順も「もう済んでいるか」を見てから実行する
/// ので、何度起動しても同じ結果になる。
fn migrate(conn: &Connection) -> Result<()> {
    // 旧名は `reviewed_domains` —— 行があること = 確認済み だった頃の名前。
    // メモだけの行も入るようになったので、名前を記録そのものに変えた
    if table_exists(conn, "reviewed_domains")? && !table_exists(conn, "domain_notes")? {
        conn.execute_batch("ALTER TABLE reviewed_domains RENAME TO domain_notes")
            .context("reviewed_domainsをdomain_notesへ改名できない")?;
        tracing::info!("reviewed_domains を domain_notes へ改名した");
    }

    if !table_exists(conn, "domain_notes")? {
        return Ok(()); // 新規環境。この後の CREATE TABLE が今の形で作る
    }

    // `reviewed_at`(確認済みにした時刻)は「最後に更新した時刻」に意味が変わった
    if column_exists(conn, "domain_notes", "reviewed_at")?
        && !column_exists(conn, "domain_notes", "updated_at")?
    {
        conn.execute_batch("ALTER TABLE domain_notes RENAME COLUMN reviewed_at TO updated_at")
            .context("reviewed_at列をupdated_atへ改名できない")?;
        tracing::info!("domain_notes.reviewed_at を updated_at へ改名した");
    }

    // 既存行の既定は1。この列より前に入っていた行は、すべて確認済みの記録だった
    if !column_exists(conn, "domain_notes", "reviewed")? {
        conn.execute_batch(
            "ALTER TABLE domain_notes ADD COLUMN reviewed INTEGER NOT NULL DEFAULT 1",
        )
        .context("reviewed列を追加できない")?;
        tracing::info!("domain_notes に reviewed 列を追加した(既存行は確認済みとして扱う)");
    }

    // 判定(問題あり / 問題なし)をやめて「確認済み」の1状態に戻した。列ごと落とす ——
    // 読まなくなった列を残すと、次にこのテーブルを読む人が「どちらが本当の状態か」を
    // 確かめることになる。確認済みかどうかは `reviewed` に残るので、
    // 消えるのは「どちらだったか」だけ
    if table_exists(conn, "domain_notes")? && column_exists(conn, "domain_notes", "verdict")? {
        conn.execute_batch("ALTER TABLE domain_notes DROP COLUMN verdict")
            .context("verdict列を削除できない")?;
        tracing::info!("domain_notes の verdict 列を削除した(状態は確認済みだけになった)");
    }

    // 「詳しく調べる」の結果はメモと別に持つ（この列より前のDBには無い）。
    // 分類のタグ(`tags`。JSON の配列)も同じく後から足した列
    for column in ["research", "researched_at", "tags"] {
        if !column_exists(conn, "domain_notes", column)? {
            conn.execute_batch(&format!("ALTER TABLE domain_notes ADD COLUMN {column} TEXT"))
                .with_context(|| format!("{column}列を追加できない"))?;
            tracing::info!(column, "domain_notes に列を追加した");
        }
    }

    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

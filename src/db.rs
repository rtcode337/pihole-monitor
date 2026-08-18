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

/// ドメイン1件についてこちらが持っている記録。
///
/// **メモと確認済みは独立している。** 行があること = 確認済み だった頃は、
/// メモを残すために確認済みにするしかなかった —— 「まだ判断していないが調べた内容は
/// 残したい」(まとめてAIに聞いた結果がまさにそれ)が表せなかった。
#[derive(Debug, Clone, Default)]
pub struct DomainRecord {
    pub note: String,
    pub reviewed: bool,
}

/// 取り込みの状況(件数と、生のクエリの一番古い時刻)。
///
/// **いまは遡り取り込みの完了ログにしか出していない。** 画面に「どれだけ貯まっているか」を
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
/// **日付の境界は日本時間で数える。** UTC のまま日付に直すと、日本の朝9時までが
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
                 domain     TEXT PRIMARY KEY,
                 updated_at TEXT NOT NULL,
                 note       TEXT,
                 reviewed   INTEGER NOT NULL DEFAULT 0
             )",
        )
        .context("domain_notesテーブルを作成できない")?;

        // 画面から決める設定(いまは「どの AI に聞くか」だけ)。**環境変数ではなく DB に持つ**
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
        // **ブロック済みの一覧(domain_notes)と役割が違う。** あちらは Pi-hole を叩いた
        // その場の集計だが、こちらは「いつもと違うか」を言うための時系列で、
        // 比較対象が無いと何も判定できない。だから貯める。
        conn.execute_batch(
            // 生のクエリ。**保持期間つきの窓**(DNS_RETENTION_DAYS)。周期の検出に
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

             -- ドメインの一生。**保持期間を過ぎても消さない** ——
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

             -- クライアントごとの日次の件数。**生のクエリが消えても残す** ——
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
            let mut stmt = conn.prepare("SELECT domain, note, reviewed FROM domain_notes")?;
            let rows = stmt.query_map([], |row| {
                let domain: String = row.get(0)?;
                let note: Option<String> = row.get(1)?;
                let reviewed: i64 = row.get(2)?;
                Ok((
                    domain,
                    DomainRecord {
                        note: note.unwrap_or_default(),
                        reviewed: reviewed != 0,
                    },
                ))
            })?;
            Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
        })
        .await
    }

    /// 確認済み / 未確認をまとめて切り替える(1件でも同じ経路)。
    ///
    /// **`note` は渡したときだけ書く。** チェックした複数件を確認済みにするときに、
    /// 既に付いているメモ(AIに聞いた結果)を空で上書きしないため。
    /// **未確認に戻してもメモは消さない** —— メモと確認済みは別の話なので、
    /// 戻したときに調べた内容まで失われては困る(メモが空の行だけ消える)。
    ///
    /// **1つのトランザクションで書く** —— 途中で落ちたときに半分だけ残さないため。
    pub async fn set_reviewed(
        &self,
        domains: Vec<String>,
        reviewed: bool,
        note: Option<String>,
    ) -> Result<()> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            for domain in &domains {
                upsert(&tx, domain, note.as_deref(), Some(reviewed))?;
                if !reviewed {
                    cleanup(&tx, domain)?;
                }
            }
            tx.commit()?;
            Ok(())
        })
        .await
    }

    /// メモだけ保存する(確認済みかどうかは変えない)。
    pub async fn save_note(&self, domain: String, note: String) -> Result<()> {
        self.with_conn(move |conn| {
            upsert(conn, &domain, Some(&note), None)?;
            cleanup(conn, &domain)?;
            Ok(())
        })
        .await
    }

    /// メモをまとめて保存する(まとめてAIに聞いた結果の書き戻し)。
    /// **1つのトランザクションで書く** —— 途中で落ちたときに半分だけ残さないため。
    pub async fn save_notes(&self, notes: Vec<(String, String)>) -> Result<()> {
        self.with_conn(move |conn| {
            let tx = conn.unchecked_transaction()?;
            for (domain, note) in &notes {
                upsert(&tx, domain, Some(note), None)?;
                cleanup(&tx, domain)?;
            }
            tx.commit()?;
            Ok(())
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

    /// いま持っている一番大きい id。**Pi-hole 側の id がこれより小さくなったら、
    /// 向こうの DB が作り直されている**(id が振り直される)ので、こちらも捨てて取り直す。
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
    /// **1つのトランザクションで書く。** 途中で落ちて「生のクエリだけ入って集計が古い」
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
                    // **重複した行では集計を進めない。** 窓を重ねて取るので、
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
    /// **生のクエリは入れない**(集計しか取っていないので時刻が無い)。
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

    /// 保持期間を過ぎた生のクエリを消す。**`dns_domains` と `dns_client_daily` は消さない**
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

    /// `since` 以降にはじめて見たドメイン(初出)。**新しい順**。
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

    /// `since` 以降に出たクエリ種別ごとの件数(全体)。**平常の形を知るために使う** ——
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
    /// **周期の判定は端末ごとに分ける。** 同じドメインを複数台が引くと間隔が混ざり、
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
    /// **ラベルの形の判定で「同じ名前を何回引いたか」を見るために使う**
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

    /// ドメインごとの、`since` 以降の件数と引いたクライアント。
    /// **どの端末が引いたかは判断材料そのもの**(PCが引くのと家電が引くのでは意味が違う)。
    pub async fn domain_activity_since(
        &self,
        since: f64,
        domains: Vec<String>,
    ) -> Result<HashMap<String, (i64, Vec<String>)>> {
        if domains.is_empty() {
            return Ok(HashMap::new());
        }
        self.with_conn(move |conn| {
            let holes = vec!["?"; domains.len()].join(",");
            let sql = format!(
                "SELECT domain, client, COUNT(*) FROM dns_queries
                  WHERE ts >= ?1 AND domain IN ({holes})
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
                ))
            })?;
            let mut out: HashMap<String, (i64, Vec<String>)> = HashMap::new();
            for row in rows {
                let (domain, client, n) = row?;
                let e = out.entry(domain).or_insert((0, Vec::new()));
                e.0 += n;
                if !e.1.contains(&client) {
                    e.1.push(client);
                }
            }
            Ok(out)
        })
        .await
    }

    /// 取り込みの状況(画面と起動ログに出す)。
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

/// 1行を書く。`note` / `reviewed` は `None` の項目を**触らない** ——
/// 「メモだけ保存」と「確認済みにする」が互いの値を巻き込まないようにするため。
fn upsert(
    conn: &Connection,
    domain: &str,
    note: Option<&str>,
    reviewed: Option<bool>,
) -> rusqlite::Result<()> {
    let updated_at = chrono::Local::now().to_rfc3339();
    conn.execute(
        "INSERT INTO domain_notes (domain, updated_at, note, reviewed)
         VALUES (?1, ?2, COALESCE(?3, ''), COALESCE(?4, 0))
         ON CONFLICT(domain) DO UPDATE SET
             updated_at = excluded.updated_at,
             note       = COALESCE(?3, domain_notes.note),
             reviewed   = COALESCE(?4, domain_notes.reviewed)",
        rusqlite::params![domain, updated_at, note, reviewed],
    )?;
    Ok(())
}

/// 何も持たなくなった行を消す(未確認 + メモ空)。
/// 残しても害は無いが、`/api/domains` が件数0の行として一覧に足してしまう。
fn cleanup(conn: &Connection, domain: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM domain_notes
         WHERE domain = ?1 AND reviewed = 0 AND COALESCE(TRIM(note), '') = ''",
        [domain],
    )?;
    Ok(())
}

/// 古いDBを今の形に合わせる。**マイグレーション機構は持たない**(個人運用なので、
/// 起動時にこの関数が差分を埋める)。**どの手順も「もう済んでいるか」を見てから実行する**
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

    // **既存行の既定は1**。この列より前に入っていた行は、すべて確認済みの記録だった
    if !column_exists(conn, "domain_notes", "reviewed")? {
        conn.execute_batch(
            "ALTER TABLE domain_notes ADD COLUMN reviewed INTEGER NOT NULL DEFAULT 1",
        )
        .context("reviewed列を追加できない")?;
        tracing::info!("domain_notes に reviewed 列を追加した(既存行は確認済みとして扱う)");
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

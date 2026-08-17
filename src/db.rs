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

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
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

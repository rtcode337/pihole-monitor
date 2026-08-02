//! SQLite操作(reviewed_domainsテーブル)。Pi-holeには一切書き込まない。
//!
//! rusqliteは同期APIなので、実際のクエリは [`tokio::task::spawn_blocking`] に逃がして
//! 非同期ランタイムのワーカースレッドを塞がないようにしている。接続は1本を
//! `Mutex` で共有する(この規模ではプールを持つ必要がない)。

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use rusqlite::Connection;

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
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS reviewed_domains (
                 domain      TEXT PRIMARY KEY,
                 reviewed_at TEXT NOT NULL,
                 note        TEXT
             )",
        )
        .context("reviewed_domainsテーブルを作成できない")?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// 確認済みドメイン → メモ の対応を全件返す。メモ未設定は空文字。
    pub async fn reviewed_domains(&self) -> Result<HashMap<String, String>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT domain, note FROM reviewed_domains")?;
            let rows = stmt.query_map([], |row| {
                let domain: String = row.get(0)?;
                let note: Option<String> = row.get(1)?;
                Ok((domain, note.unwrap_or_default()))
            })?;
            Ok(rows.collect::<rusqlite::Result<HashMap<_, _>>>()?)
        })
        .await
    }

    /// 確認済みにする(既にあればメモを上書き)。
    pub async fn mark_reviewed(&self, domain: String, note: String) -> Result<()> {
        let reviewed_at = chrono::Local::now().to_rfc3339();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT OR REPLACE INTO reviewed_domains (domain, reviewed_at, note)
                 VALUES (?1, ?2, ?3)",
                (&domain, &reviewed_at, &note),
            )?;
            Ok(())
        })
        .await
    }

    /// 未確認に戻す。
    pub async fn delete_reviewed(&self, domain: String) -> Result<()> {
        self.with_conn(move |conn| {
            conn.execute("DELETE FROM reviewed_domains WHERE domain = ?1", (&domain,))?;
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

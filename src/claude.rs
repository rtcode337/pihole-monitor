//! 「Claudeに聞く」の問い合わせとトークン管理。
//!
//! **CLI はこのイメージに入っていない。** 別コンテナの CLI ブリッジ(chiezo-bridge。
//! Claude Code を OpenAI 互換の `/chat/completions` に見せるサイドカー)へ HTTP で頼む ——
//! CLI 本体と、それを動かすための Node で 150MB 近く積んでいたが、アプリ自体は数 MB しかない。
//!
//! 認証は `claude setup-token` で発行した長期OAuthトークンを使う(ホストの `~/.claude` は
//! マウントしない)。**渡し方は共有ディレクトリの設定 DB** —— ブリッジは別コンテナなので
//! 環境変数では渡せない。こちらが `provider_settings` 表に書き、ブリッジが読み取り専用で
//! マウントして読む。ブリッジは要求のたびに読み直すので、入れ替えても再起動は要らない。

use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::json;

use crate::config::{AUTH_ERROR_KEYWORDS, Config};

/// 「Claudeに聞く」の失敗理由。
pub enum AskError {
    /// トークンが未保存、または認証エラーだった。フロントにトークン入力を促させる
    TokenRequired,
    /// それ以外の失敗。文字列はそのまま画面に出す
    Failed(String),
}

/// ブリッジ側で Claude Code を指す名前(`CHIEZO_BRIDGE_CLI` と同じ値)。
const PROVIDER: &str = "claude";

/// 道具を引く往復の上限。道具は使わせない設定で動かすので 1 回で返るが、
/// CLI が内部で 1 往復使う場合に備えて 2 にしてある(使わなければ増えない)。
const MAX_TURNS: u32 = 2;

/// ブリッジが読む表。**列を減らさないこと** —— ブリッジは credential しか読まないが、
/// 同じ形のファイルを chiezo 本体が開くこともある。
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS provider_settings (
     provider    TEXT PRIMARY KEY,
     enabled     INTEGER NOT NULL DEFAULT 0,
     credential  TEXT,
     model       TEXT,
     verified_at TEXT,
     updated_at  TEXT NOT NULL
 )";

#[derive(Clone)]
pub struct ClaudeClient {
    http: reqwest::Client,
    bridge_url: String,
    settings_path: PathBuf,
    /// CLI を同梱していた頃のトークンの置き場。**初回に設定 DB へ移して消す**
    /// (移さないと、更新した環境で入れ直しを求めることになる)。
    legacy_token_path: PathBuf,
    timeout: Duration,
}

impl ClaudeClient {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            // **こちらの待ちはブリッジより長くする。** 先に切れると「ブリッジが何秒で
            // 諦めたか」が分からなくなる(向こうは 504 と経過秒数を返してくれる)
            http: reqwest::Client::builder()
                .timeout(config.claude_timeout + Duration::from_secs(30))
                .build()
                .context("HTTPクライアントを作成できない")?,
            bridge_url: config.claude_bridge_url.clone(),
            settings_path: config.state_dir.join("settings.db"),
            legacy_token_path: config.claude_token_path.clone(),
            timeout: config.claude_timeout,
        })
    }

    /// 保存済みのトークン。画面が「トークン入力を出すか」を決めるのに使う
    /// (問い合わせのときに送るわけではない —— 読むのはブリッジ)。
    pub fn load_token(&self) -> Option<String> {
        if let Some(token) = self.stored_token() {
            return Some(token);
        }
        self.migrate_legacy_token()
    }

    pub fn save_token(&self, token: &str) -> Result<()> {
        self.write_credential(Some(token.trim()))
    }

    fn clear_token(&self) {
        if let Err(e) = self.write_credential(None) {
            tracing::warn!(error = ?e, "認証エラー後のトークン削除に失敗した");
        }
    }

    /// 指定ドメインについてブリッジ経由で Claude に説明を求める。
    pub async fn ask_about_domain(&self, domain: &str) -> Result<String, AskError> {
        if self.load_token().is_none() {
            return Err(AskError::TokenRequired);
        }

        let prompt = format!(
            "Pi-holeの広告/トラッキングブロックリストによってブロックされたドメイン「{domain}」について、\
             これがどのようなサービス・通信に関連するドメインで、なぜブロックリストに含まれている可能性が高いかを\
             日本語で3〜5行程度で簡潔に説明してください。"
        );

        let url = format!("{}/chat/completions", self.bridge_url.trim_end_matches('/'));
        let response = self
            .http
            .post(&url)
            .json(&json!({
                "messages": [{"role": "user", "content": prompt}],
                "chiezo_max_turns": MAX_TURNS,
                "chiezo_timeout": self.timeout.as_secs_f64(),
            }))
            .send()
            .await;

        let response = match response {
            Ok(response) => response,
            Err(e) if e.is_timeout() => {
                tracing::warn!(timeout_secs = self.timeout.as_secs(), "CLIブリッジへの問い合わせがタイムアウトした");
                return Err(AskError::Failed("timeout".to_string()));
            }
            Err(e) => {
                tracing::error!(error = %e, url = %url, "CLIブリッジに繋がらない");
                return Err(AskError::Failed("bridge unreachable".to_string()));
            }
        };

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if status == reqwest::StatusCode::GATEWAY_TIMEOUT {
            tracing::warn!(timeout_secs = self.timeout.as_secs(), "CLIブリッジが上限秒数で打ち切った");
            return Err(AskError::Failed("timeout".to_string()));
        }

        if !status.is_success() {
            tracing::error!(status = %status, body = %excerpt(&body), "CLIブリッジがエラーを返した");
            // 401 は「ブリッジがトークンを読めていない」。CLI 側の認証エラーは 502 の
            // 本文(CLI の stderr)に出るので、そちらも見る
            if status == reqwest::StatusCode::UNAUTHORIZED || is_auth_error(&body) {
                self.clear_token();
                return Err(AskError::TokenRequired);
            }
            return Err(AskError::Failed(excerpt(&body)));
        }

        match answer_from(&body) {
            Some(answer) => Ok(answer),
            None => {
                tracing::warn!(body = %excerpt(&body), "CLIブリッジの応答から本文を取り出せない");
                Err(AskError::Failed("empty response from claude".to_string()))
            }
        }
    }

    fn stored_token(&self) -> Option<String> {
        let conn = Connection::open(&self.settings_path).ok()?;
        let token: String = conn
            .query_row(
                "SELECT credential FROM provider_settings WHERE provider = ?1",
                [PROVIDER],
                |row| row.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten()?;
        let token = token.trim().to_string();
        (!token.is_empty()).then_some(token)
    }

    /// CLI を同梱していた頃のトークンファイルを設定 DB へ移す(移せたら元は消す)。
    fn migrate_legacy_token(&self) -> Option<String> {
        let token = fs::read_to_string(&self.legacy_token_path).ok()?;
        let token = token.trim().to_string();
        if token.is_empty() {
            return None;
        }
        if let Err(e) = self.write_credential(Some(&token)) {
            tracing::warn!(error = ?e, "保存済みトークンをCLIブリッジ用の設定へ移せない");
            return None;
        }
        if let Err(e) = fs::remove_file(&self.legacy_token_path)
            && e.kind() != ErrorKind::NotFound
        {
            tracing::warn!(error = %e, "移行後の旧トークンファイルを消せない");
        }
        tracing::info!("保存済みトークンをCLIブリッジ用の設定へ移した");
        Some(token)
    }

    /// トークンを書く。`None` なら無効化する(残すと古いトークンでブリッジが動き続ける)。
    fn write_credential(&self, token: Option<&str>) -> Result<()> {
        if let Some(dir) = self.settings_path.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("共有ディレクトリを作成できない: {}", dir.display()))?;
        }

        let conn = Connection::open(&self.settings_path)
            .with_context(|| format!("設定DBを開けない: {}", self.settings_path.display()))?;
        // **WAL にしない。** ブリッジはこのファイルを読み取り専用でマウントして読むが、
        // WAL の読み手は -shm への書き込みを要求するので開けなくなる。journal_mode は
        // ファイルに焼き付く属性なので、書かないだけでは戻らない(毎回指定する)
        conn.pragma_update(None, "journal_mode", "DELETE")
            .context("設定DBのjournal_modeを設定できない")?;
        conn.execute_batch(SCHEMA)
            .context("provider_settingsテーブルを作成できない")?;
        conn.execute(
            "INSERT INTO provider_settings (provider, enabled, credential, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(provider) DO UPDATE SET
                 credential=excluded.credential,
                 enabled=excluded.enabled,
                 updated_at=excluded.updated_at",
            rusqlite::params![
                PROVIDER,
                i32::from(token.is_some()),
                token,
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .context("トークンを保存できない")?;

        // **パーミッションは絞らない。** CLI を同梱していた頃のトークンファイルは 0600 に
        // していたが、この設定 DB は**別コンテナのブリッジ(uid 1000 固定)が読む**。
        // 本体の実行ユーザーはホストに合わせて変えられる(PIHOLE_MONITOR_UID)ので、
        // 所有者だけに絞ると uid がずれた環境でブリッジが読めなくなる
        Ok(())
    }
}

/// 応答(OpenAI 互換)から本文を取り出す。
fn answer_from(body: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(body).ok()?;
    let text = parsed
        .get("choices")?
        .get(0)?
        .get("message")?
        .get("content")?
        .as_str()?
        .trim()
        .to_string();
    (!text.is_empty()).then_some(text)
}

fn excerpt(text: &str) -> String {
    let trimmed = text.trim();
    match trimmed.char_indices().nth(300) {
        Some((idx, _)) => format!("{}…", &trimmed[..idx]),
        None => trimmed.to_string(),
    }
}

fn is_auth_error(text: &str) -> bool {
    let lowered = text.to_lowercase();
    AUTH_ERROR_KEYWORDS.iter().any(|kw| lowered.contains(kw))
}

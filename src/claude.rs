//! Claude Code CLI(`claude`)のヘッドレス実行とトークン管理。
//!
//! Chiezo を使わないときの経路。 どの AI に聞くかを選べるのは Chiezo 越しのときだけで
//! (`chiezo.rs`)、Chiezo の URL が未設定か、相手を選んでいない場合はここが受け持つ。
//!
//! CLI はこのイメージに同梱してある(Dockerfile)。アプリが `claude -p` を
//! プロセスとして起動する。かつては別コンテナのサイドカー(chiezo-bridge。Claude Code を
//! OpenAI 互換の口に見せるもの)へ HTTP で頼み、CLI をイメージから外していた ——
//! 実体が大きくイメージが倍近くなるためだったが、別コンテナを立てないと「AIに聞く」が
//! 動かないのは、置き場所の都合をそのまま利用者に負わせている。同梱なら compose を
//! 1つ起こすだけで動く。
//!
//! 認証は `claude setup-token` で発行した長期OAuthトークンを使う(ホストの `~/.claude` は
//! マウントしない)。渡し方は子プロセスの環境変数(`CLAUDE_CODE_OAUTH_TOKEN`)——
//! このプロセス自身の環境変数は変えない(他の子プロセスへ漏らさないため)。
//! 起動のたびに読むので、画面で入れ替えた値がそのまま次の問い合わせに効く。

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};
use rusqlite::Connection;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

use crate::ai::AskError;
use crate::config::{AUTH_ERROR_KEYWORDS, Config};

/// トークンを置く表の行。サイドカーだった頃と同じ形のままにしてある ——
/// 形を変えると、更新した環境でトークンの入れ直しを求めることになる。
const PROVIDER: &str = "claude";

/// 子プロセスの後始末を待つ上限。SIGKILL の後なのですぐ終わる。
const CHILD_WAIT: Duration = Duration::from_secs(2);

/// トークンを置く表。
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
    executable: String,
    /// 使わせるモデル。空なら CLI の既定(サイドカー経由だった頃と同じ)。
    model: String,
    settings_path: PathBuf,
    /// CLI を同梱していなかった頃のトークンの置き場。初回に設定 DB へ移して消す。
    legacy_token_path: PathBuf,
    /// CLI に使わせるホーム。イメージに固定のホームを持たせない ——
    /// 実行ユーザーはホストに合わせて変えられる(`PIHOLE_MONITOR_UID`)ので、
    /// 書ける場所は永続化した置き場の下に作る。作業ディレクトリも兼ねる:
    /// 開発ホストでそのまま起動すると、CLI がリポジトリの `CLAUDE.md` を読んで
    /// プロンプトに混ぜてしまう。空のディレクトリで走らせれば拾うものが無い。
    home_dir: PathBuf,
    timeout: Duration,
}

impl ClaudeClient {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            executable: config.claude_executable.clone(),
            model: config.claude_model.clone(),
            settings_path: config.state_dir.join("settings.db"),
            legacy_token_path: config.claude_token_path.clone(),
            home_dir: config.state_dir.join("claude-home"),
            timeout: config.claude_timeout,
        })
    }

    /// 保存済みのトークン。画面が「トークン入力を出すか」を決めるのにも使う。
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

    /// 画面から消す。失敗は理由ごと返す ——
    /// 認証エラーで落とすとき(clear_token)と違い、押した人が結果を待っている。
    pub fn delete_token(&self) -> Result<()> {
        self.write_credential(None)
    }

    /// Claude に聞く。プロンプトは受け取る —— 相手が Chiezo 越しでも同じ文言に
    /// なるよう、指示文は `ai.rs` の1か所に置いてある。
    pub async fn ask(&self, system_prompt: &str, user_prompt: &str) -> Result<String, AskError> {
        self.ask_within(system_prompt, user_prompt, self.timeout).await
    }

    /// 上限秒数を指定して聞く。「詳しく調べる」は web 検索を伴って長くかかるので、
    /// 通常の問い合わせと同じ上限だと必ず途中で切れる。
    pub async fn ask_within(
        &self,
        system_prompt: &str,
        user_prompt: &str,
        timeout: Duration,
    ) -> Result<String, AskError> {
        let Some(token) = self.load_token() else {
            return Err(AskError::TokenRequired);
        };
        if let Err(e) = fs::create_dir_all(&self.home_dir) {
            tracing::error!(error = %e, path = %self.home_dir.display(), "CLIのホームを作れない");
            return Err(AskError::Failed(format!("claude home not writable: {e}")));
        }

        let mut command = Command::new(&self.executable);
        command
            .arg("-p")
            .arg("--output-format")
            .arg("text")
            // 指示文はこちらのものに置き換える(既定の system prompt は
            // コーディング用で、こちらの用途とは噛み合わない)
            .arg("--system-prompt")
            .arg(system_prompt)
            // web 検索だけ許す。 「詳しく調べる」はドメインの運営元・評判を
            // 外から調べる前提の指示文になっている。他の道具は許可を求めるが、
            // `-p` では聞けないので実行されない
            .arg("--allowed-tools")
            .arg("WebSearch")
            // 手元の MCP 設定を拾わせない(渡すのは自前のプロンプトだけ)
            .arg("--strict-mcp-config")
            // 会話を保存させない(後から再開しないし、ホームに書くものを減らせる)
            .arg("--no-session-persistence")
            .env("CLAUDE_CODE_OAUTH_TOKEN", token)
            .env("HOME", &self.home_dir)
            .current_dir(&self.home_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if !self.model.is_empty() {
            command.arg("--model").arg(&self.model);
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                tracing::error!(executable = %self.executable, "claudeコマンドが見つからない");
                return Err(AskError::Failed("claude command not found".to_string()));
            }
            Err(e) => {
                tracing::error!(error = %e, "claudeコマンドを実行できない");
                return Err(AskError::Failed(e.to_string()));
            }
        };

        // 本文は標準入力から渡す。 Linux の単一引数の上限(MAX_ARG_STRLEN = 128KiB)に
        // 当たると起動そのものができない —— 監視の一覧は数十件ぶんのドメインと理由を
        // 渡すので、材料が増えると届く長さになる
        let mut stdin = child.stdin.take();
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let (mut out, mut err) = (String::new(), String::new());

        let reading = async {
            if let Some(mut pipe) = stdin.take() {
                if let Err(e) = pipe.write_all(user_prompt.as_bytes()).await {
                    tracing::warn!(error = %e, "claudeへプロンプトを書けない");
                }
                // 閉じて EOF を送る。 開けたままだと CLI は入力の続きを待つ
                drop(pipe);
            }
            if let (Some(o), Some(e)) = (stdout.as_mut(), stderr.as_mut()) {
                let _ = tokio::join!(o.read_to_string(&mut out), e.read_to_string(&mut err));
            }
        };

        let finished = tokio::time::timeout(timeout, reading).await;
        if finished.is_err() {
            tracing::warn!(timeout_secs = timeout.as_secs(), "claudeの実行がタイムアウトした");
            stop(&mut child).await;
            return Err(AskError::Failed("timeout".to_string()));
        }

        let status = tokio::time::timeout(CHILD_WAIT, child.wait()).await;
        let (out, err) = (out.trim().to_string(), err.trim().to_string());
        let ok = matches!(&status, Ok(Ok(status)) if status.success());
        if !ok {
            tracing::error!(status = ?status, stdout = %out, stderr = %err, "claudeコマンドが異常終了した");
            // 認証エラーは標準出力側に出ることもあるので両方を見る
            if is_auth_error(&out) || is_auth_error(&err) {
                self.clear_token();
                return Err(AskError::TokenRequired);
            }
            let message = [err, out]
                .into_iter()
                .find(|text| !text.is_empty())
                .unwrap_or_else(|| "claude command failed".to_string());
            return Err(AskError::Failed(excerpt(&message)));
        }

        if out.is_empty() {
            tracing::warn!("claudeコマンドの標準出力が空だった");
            return Err(AskError::Failed("empty response from claude".to_string()));
        }
        Ok(out)
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

    /// CLI を同梱していなかった頃のトークンファイルを設定 DB へ移す(移せたら元は消す)。
    fn migrate_legacy_token(&self) -> Option<String> {
        let token = fs::read_to_string(&self.legacy_token_path).ok()?;
        let token = token.trim().to_string();
        if token.is_empty() {
            return None;
        }
        if let Err(e) = self.write_credential(Some(&token)) {
            tracing::warn!(error = ?e, "保存済みトークンを設定DBへ移せない");
            return None;
        }
        if let Err(e) = fs::remove_file(&self.legacy_token_path)
            && e.kind() != ErrorKind::NotFound
        {
            tracing::warn!(error = %e, "移行後の旧トークンファイルを消せない");
        }
        tracing::info!("保存済みトークンを設定DBへ移した");
        Some(token)
    }

    /// トークンを書く。`None` なら無効化する(残すと古いトークンで動き続ける)。
    fn write_credential(&self, token: Option<&str>) -> Result<()> {
        if let Some(dir) = self.settings_path.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("設定の置き場を作成できない: {}", dir.display()))?;
        }

        let conn = Connection::open(&self.settings_path)
            .with_context(|| format!("設定DBを開けない: {}", self.settings_path.display()))?;
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

        // 他のユーザーから読めないようにする。 サイドカーへ渡していた頃は別コンテナが
        // 読むので絞れなかったが、いま読むのはこのプロセスだけ
        if let Err(e) = fs::set_permissions(&self.settings_path, fs::Permissions::from_mode(0o600))
        {
            tracing::warn!(error = %e, "設定DBのパーミッションを絞れない");
        }
        Ok(())
    }
}

/// 途中でやめた子プロセスを片付ける。殺すだけでは足りない ——
/// `kill_on_drop` は SIGKILL を送るだけで待たないので、死んだ子はゾンビのまま残る
/// (PID 1 がこのアプリ自身のコンテナでは、拾ってくれる init もいない)。
async fn stop(child: &mut Child) {
    let _ = tokio::time::timeout(CHILD_WAIT, child.kill()).await;
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

//! Claude Code CLI(`claude`)のヘッドレス実行とトークン管理。
//!
//! 認証は `claude setup-token` で発行した長期OAuthトークンを使う。ホストの `~/.claude` は
//! マウントせず、トークンは永続化ボリューム上に600で置いて、実行時に
//! `CLAUDE_CODE_OAUTH_TOKEN` 環境変数として渡す。

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::config::{AUTH_ERROR_KEYWORDS, Config};

/// 「Claudeに聞く」の失敗理由。
pub enum AskError {
    /// トークンが未保存、または認証エラーだった。フロントにトークン入力を促させる
    TokenRequired,
    /// それ以外の失敗。文字列はそのまま画面に出す
    Failed(String),
}

#[derive(Clone)]
pub struct ClaudeClient {
    token_path: PathBuf,
    timeout: Duration,
}

impl ClaudeClient {
    pub fn new(config: &Config) -> Self {
        Self {
            token_path: config.claude_token_path.clone(),
            timeout: config.claude_timeout,
        }
    }

    pub fn load_token(&self) -> Option<String> {
        let token = fs::read_to_string(&self.token_path).ok()?;
        let token = token.trim();
        (!token.is_empty()).then(|| token.to_string())
    }

    pub fn save_token(&self, token: &str) -> Result<()> {
        if let Some(dir) = self.token_path.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("データディレクトリを作成できない: {}", dir.display()))?;
        }
        fs::write(&self.token_path, token.trim())
            .with_context(|| format!("トークンを保存できない: {}", self.token_path.display()))?;
        // 他のユーザーから読めないようにする(既存ファイルを上書きした場合も含めて毎回設定する)
        fs::set_permissions(&self.token_path, fs::Permissions::from_mode(0o600))
            .context("トークンのパーミッションを設定できない")?;
        Ok(())
    }

    fn clear_token(&self) {
        if let Err(e) = fs::remove_file(&self.token_path)
            && e.kind() != ErrorKind::NotFound
        {
            tracing::warn!(error = %e, "認証エラー後のトークン削除に失敗した");
        }
    }

    /// 指定ドメインについて `claude` に説明を求め、標準出力をそのまま回答として返す。
    pub async fn ask_about_domain(&self, domain: &str) -> Result<String, AskError> {
        let Some(token) = self.load_token() else {
            return Err(AskError::TokenRequired);
        };

        let prompt = format!(
            "Pi-holeの広告/トラッキングブロックリストによってブロックされたドメイン「{domain}」について、\
             これがどのようなサービス・通信に関連するドメインで、なぜブロックリストに含まれている可能性が高いかを\
             日本語で3〜5行程度で簡潔に説明してください。"
        );

        let mut command = tokio::process::Command::new("claude");
        command
            .arg("-p")
            .arg(&prompt)
            .arg("--output-format")
            .arg("text")
            .env("CLAUDE_CODE_OAUTH_TOKEN", token)
            .stdin(Stdio::null())
            // タイムアウトでフューチャーを捨てたときに子プロセスを残さない
            .kill_on_drop(true);

        let output = match tokio::time::timeout(self.timeout, command.output()).await {
            Err(_) => {
                tracing::warn!(timeout_secs = self.timeout.as_secs(), "claudeの実行がタイムアウトした");
                return Err(AskError::Failed("timeout".to_string()));
            }
            Ok(Err(e)) if e.kind() == ErrorKind::NotFound => {
                tracing::error!("claudeコマンドが見つからない");
                return Err(AskError::Failed("claude command not found".to_string()));
            }
            Ok(Err(e)) => {
                tracing::error!(error = %e, "claudeコマンドを実行できない");
                return Err(AskError::Failed(e.to_string()));
            }
            Ok(Ok(output)) => output,
        };

        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

        if !output.status.success() {
            tracing::error!(
                status = ?output.status.code(),
                stdout = %stdout,
                stderr = %stderr,
                "claudeコマンドが異常終了した"
            );
            // 認証エラーは標準出力側に出ることもあるので両方を見る
            if is_auth_error(&stdout) || is_auth_error(&stderr) {
                self.clear_token();
                return Err(AskError::TokenRequired);
            }
            let message = if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                "claude command failed".to_string()
            };
            return Err(AskError::Failed(message));
        }

        if stdout.is_empty() {
            tracing::warn!("claudeコマンドの標準出力が空だった");
            return Err(AskError::Failed("empty response from claude".to_string()));
        }
        Ok(stdout)
    }
}

fn is_auth_error(text: &str) -> bool {
    let lowered = text.to_lowercase();
    AUTH_ERROR_KEYWORDS.iter().any(|kw| lowered.contains(kw))
}

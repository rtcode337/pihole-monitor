//! `/api/*` のJSONエンドポイント。

use std::collections::HashSet;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::claude::{AskError, ClaudeClient};
use crate::db::Db;
use crate::pihole::PiholeClient;

/// ハンドラ間で共有する依存。いずれも中身は `Arc` か `Clone` が安いものなので、
/// axumがリクエストごとにcloneしても問題ない。
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub pihole: PiholeClient,
    pub claude: ClaudeClient,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/domains", get(domains))
        .route("/api/review", post(review_post).delete(review_delete))
        .route("/api/ask-claude", post(ask_claude))
        .route("/api/claude-token", post(claude_token))
}

#[derive(Serialize)]
struct DomainEntry {
    domain: String,
    count: u32,
    reviewed: bool,
    note: String,
}

#[derive(Deserialize)]
struct ReviewRequest {
    domain: Option<String>,
    #[serde(default)]
    note: String,
}

#[derive(Deserialize)]
struct AskRequest {
    domain: Option<String>,
}

#[derive(Deserialize)]
struct TokenRequest {
    #[serde(default)]
    token: String,
}

/// ブロック済みドメイン一覧。確認済みフラグとメモを付けて返す。
async fn domains(State(state): State<AppState>) -> Response {
    let blocked = match state.pihole.blocked_domains().await {
        Ok(blocked) => blocked,
        Err(e) => {
            tracing::error!(error = ?e, "Pi-holeからブロック済みドメインを取得できない");
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": "pihole_unavailable" })),
            )
                .into_response();
        }
    };

    let reviewed = match state.db.reviewed_domains().await {
        Ok(reviewed) => reviewed,
        Err(e) => return internal_error(e, "確認済みドメインを読み出せない"),
    };

    let blocked_names: HashSet<&String> = blocked.keys().collect();
    let mut entries: Vec<DomainEntry> = blocked
        .iter()
        .map(|(domain, count)| DomainEntry {
            domain: domain.clone(),
            count: *count,
            reviewed: reviewed.contains_key(domain),
            note: reviewed.get(domain).cloned().unwrap_or_default(),
        })
        .collect();

    // 直近のブロック済みクエリに出てこない確認済みドメインも、件数0として一覧に残す
    for (domain, note) in &reviewed {
        if !blocked_names.contains(domain) {
            entries.push(DomainEntry {
                domain: domain.clone(),
                count: 0,
                reviewed: true,
                note: note.clone(),
            });
        }
    }

    // 未確認を先に、件数の多い順。HashMapの列挙順は毎回変わるので、
    // 件数が同じときはドメイン名で並びを固定する
    entries.sort_by(|a, b| {
        a.reviewed
            .cmp(&b.reviewed)
            .then(b.count.cmp(&a.count))
            .then_with(|| a.domain.cmp(&b.domain))
    });

    Json(entries).into_response()
}

/// 確認済みにする(メモも保存)。
async fn review_post(State(state): State<AppState>, Json(req): Json<ReviewRequest>) -> Response {
    let Some(domain) = non_empty(req.domain) else {
        return bad_request("domain required");
    };
    match state.db.mark_reviewed(domain, req.note).await {
        Ok(()) => success(),
        Err(e) => internal_error(e, "確認済みにできない"),
    }
}

/// 未確認に戻す。
async fn review_delete(State(state): State<AppState>, Json(req): Json<ReviewRequest>) -> Response {
    let Some(domain) = non_empty(req.domain) else {
        return bad_request("domain required");
    };
    match state.db.delete_reviewed(domain).await {
        Ok(()) => success(),
        Err(e) => internal_error(e, "未確認に戻せない"),
    }
}

/// 指定ドメインについてClaude CLIに問い合わせる。
async fn ask_claude(State(state): State<AppState>, Json(req): Json<AskRequest>) -> Response {
    let Some(domain) = non_empty(req.domain) else {
        return bad_request("domain required");
    };
    match state.claude.ask_about_domain(&domain).await {
        Ok(answer) => Json(json!({ "success": true, "answer": answer })).into_response(),
        // フロントは token_required を見てトークン入力モーダルに切り替える
        Err(AskError::TokenRequired) => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "success": false, "error": "token_required" })),
        )
            .into_response(),
        Err(AskError::Failed(message)) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({ "success": false, "error": message })),
        )
            .into_response(),
    }
}

/// `claude setup-token` で発行したトークンを保存する。
async fn claude_token(State(state): State<AppState>, Json(req): Json<TokenRequest>) -> Response {
    let token = req.token.trim();
    if token.is_empty() {
        return bad_request("token required");
    }
    match state.claude.save_token(token) {
        Ok(()) => success(),
        Err(e) => internal_error(e, "トークンを保存できない"),
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn success() -> Response {
    Json(json!({ "success": true })).into_response()
}

fn bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "success": false, "error": message })),
    )
        .into_response()
}

/// 内部エラーは詳細をログにだけ残し、画面には短いメッセージを返す。
fn internal_error(error: anyhow::Error, message: &str) -> Response {
    tracing::error!(error = ?error, "{message}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "success": false, "error": message })),
    )
        .into_response()
}

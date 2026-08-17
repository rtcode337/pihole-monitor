//! `/api/*` のJSONエンドポイント。

use std::collections::HashSet;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ai::{Ai, AiChoice, AskError};
use crate::db::Db;
use crate::pihole::PiholeClient;

/// ハンドラ間で共有する依存。いずれも中身は `Arc` か `Clone` が安いものなので、
/// axumがリクエストごとにcloneしても問題ない。
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub pihole: PiholeClient,
    pub ai: Ai,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/domains", get(domains))
        .route("/api/review", post(review_post).delete(review_delete))
        // **`/api/ask-claude` ではない。** 答える相手は画面から切り替えられるので、
        // 名前に相手を入れると Chiezo 越しの Codex に聞いたときに嘘になる
        .route("/api/ask", post(ask))
        .route("/api/ai", get(ai_get).post(ai_post))
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

/// 指定ドメインについて、選ばれているAIに問い合わせる。
async fn ask(State(state): State<AppState>, Json(req): Json<AskRequest>) -> Response {
    let Some(domain) = non_empty(req.domain) else {
        return bad_request("domain required");
    };
    match state.ai.ask_about_domain(&domain).await {
        // **誰が書いたかを一緒に返す。** 相手を切り替えられるので、
        // 回答だけ返すと画面がどのAIの答えを出しているのか言えない
        Ok(answer) => Json(json!({
            "success": true,
            "answer": answer.text,
            "author": answer.author,
        }))
        .into_response(),
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
    match state.ai.save_token(token) {
        Ok(()) => success(),
        Err(e) => internal_error(e, "トークンを保存できない"),
    }
}

/// いま選べる相手と、選ばれているもの。**繋がらない理由も一緒に返す** ——
/// 一覧が空なのが「未設定」なのか「届かない」のかを画面が言い分けられるように。
async fn ai_get(State(state): State<AppState>) -> Response {
    let (backends, error) = match state.ai.backends().await {
        Ok(backends) => (backends, None),
        Err(message) => (Vec::new(), Some(message)),
    };

    Json(json!({
        "chiezo_url": state.ai.chiezo_url(),
        "bridge_label": crate::ai::BRIDGE_LABEL,
        "backends": backends,
        "selection": state.ai.selection().await,
        "current": state.ai.current_name().await,
        "error": error,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct SelectRequest {
    /// 空・未指定なら CLI ブリッジ経由に戻す。
    backend: Option<String>,
    #[serde(default)]
    model: String,
    #[serde(default)]
    effort: String,
}

/// 聞く相手を保存する。**実在しない相手は受け付けない** ——
/// 黙って保存すると、次に聞いたときまで間違いに気づけない。
async fn ai_post(State(state): State<AppState>, Json(req): Json<SelectRequest>) -> Response {
    let Some(backend_id) = non_empty(req.backend) else {
        // 戻すだけなので Chiezo に問い合わせない(繋がらなくても戻せる必要がある)
        return match state.ai.select(None).await {
            Ok(()) => Json(json!({ "success": true, "current": crate::ai::BRIDGE_LABEL }))
                .into_response(),
            Err(e) => internal_error(e, "AIの選択を保存できない"),
        };
    };

    let backends = match state.ai.backends().await {
        Ok(backends) => backends,
        Err(message) => return bad_gateway(&message),
    };

    let Some(backend) = backends.into_iter().find(|b| b.id == backend_id) else {
        return bad_request("Chiezo にその相手がいません。一覧を読み直してください。");
    };

    let model = req.model.trim().to_string();
    if model.is_empty() {
        if backend.model_required {
            return bad_request("この相手はモデルの指定が必要です。");
        }
    } else if !backend.models.contains(&model) {
        return bad_request("Chiezo が知らないモデルです。一覧を読み直してください。");
    }

    let effort = req.effort.trim().to_string();
    if !effort.is_empty() && !backend.efforts.contains(&effort) {
        return bad_request("Chiezo が知らない考える量です。一覧を読み直してください。");
    }

    let choice = AiChoice {
        backend: backend.id,
        // **表記は選んだ時点のものを保存する**(表示のたびに Chiezo へ聞きに行かない)
        label: backend.label,
        model: (!model.is_empty()).then_some(model),
        effort: (!effort.is_empty()).then_some(effort),
    };

    match state.ai.select(Some(&choice)).await {
        Ok(()) => Json(json!({ "success": true, "current": choice.display_name() })).into_response(),
        Err(e) => internal_error(e, "AIの選択を保存できない"),
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

/// 相手(Pi-hole・Chiezo)側の失敗。理由はそのまま画面に出す。
fn bad_gateway(message: &str) -> Response {
    (
        StatusCode::BAD_GATEWAY,
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

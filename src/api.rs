//! `/api/*` のJSONエンドポイント。

use std::collections::HashSet;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ai::{Ai, AiChoice, AskError, BRIDGE_BACKEND, BRIDGE_LABEL, MAX_DOMAINS_PER_ASK};
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
        // **メモは確認済みと独立している。** 確認済みにしないと残せないと、
        // 「まだ判断していないが調べた内容は残したい」が表せない
        .route("/api/note", post(note_post))
        // **口は1つ。** 行のボタン(1件)も「まとめて聞く」(区切って何度も)も同じここを通り、
        // どちらも結果をメモとして保存する —— 1件用の口を別に持つと、指示文と保存の仕方が
        // 2か所に分かれる
        .route("/api/ask", post(ask))
        // 「ブロックされていない怪しい通信」の候補。**ブロック済みの一覧とは別の口** ——
        // あちらは Pi-hole をその場で叩く集計、こちらは貯めた過去との突き合わせで、
        // 材料も判定も違う(watch.rs)
        .route("/api/watch", get(watch))
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

/// 「ブロックされていない怪しい通信」の候補。判定は watch.rs、材料は取り込んだ蓄積。
async fn watch(State(state): State<AppState>) -> Response {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    match crate::watch::candidates(&state.db, now).await {
        Ok(result) => Json(result).into_response(),
        Err(e) => {
            tracing::error!(error = ?e, "怪しい通信の候補を組み立てられない");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "watch_failed" })),
            )
                .into_response()
        }
    }
}

/// 確認済み/未確認の切り替えとメモの保存。**ドメインは配列** ——
/// 1件でもチェックした複数件でも同じ口を通す(一括用の口を別に持つと、
/// 「メモを巻き込まない」等の決めごとが2か所に分かれる)。
#[derive(Deserialize)]
struct ReviewRequest {
    #[serde(default)]
    domains: Vec<String>,
    /// **省略したらメモは触らない。** 一括で確認済みにするときに、
    /// 既に付いているメモ(AIに聞いた結果)を空で上書きしないため。
    note: Option<String>,
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

    let records = match state.db.records().await {
        Ok(records) => records,
        Err(e) => return internal_error(e, "ドメインの記録を読み出せない"),
    };

    let blocked_names: HashSet<&String> = blocked.keys().collect();
    let mut entries: Vec<DomainEntry> = blocked
        .iter()
        .map(|(domain, count)| {
            let record = records.get(domain);
            DomainEntry {
                domain: domain.clone(),
                count: *count,
                reviewed: record.is_some_and(|r| r.reviewed),
                note: record.map(|r| r.note.clone()).unwrap_or_default(),
            }
        })
        .collect();

    // 直近のブロック済みクエリに出てこない記録も、件数0として一覧に残す
    // (確認済みだけでなく**メモだけの行も残す** —— 調べた内容を画面から消さないため)
    for (domain, record) in &records {
        if !blocked_names.contains(domain) {
            entries.push(DomainEntry {
                domain: domain.clone(),
                count: 0,
                reviewed: record.reviewed,
                note: record.note.clone(),
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

/// 確認済みにする(1件でもまとめてでも)。`note` を渡したときだけメモも保存する。
async fn review_post(State(state): State<AppState>, Json(req): Json<ReviewRequest>) -> Response {
    let domains = clean(req.domains);
    if domains.is_empty() {
        return bad_request("domains required");
    }
    let count = domains.len();
    match state.db.set_reviewed(domains, true, req.note).await {
        Ok(()) => reviewed_count(count),
        Err(e) => internal_error(e, "確認済みにできない"),
    }
}

/// 未確認に戻す。**メモは消さない**(メモが空の行だけ消える)。
async fn review_delete(State(state): State<AppState>, Json(req): Json<ReviewRequest>) -> Response {
    let domains = clean(req.domains);
    if domains.is_empty() {
        return bad_request("domains required");
    }
    let count = domains.len();
    match state.db.set_reviewed(domains, false, None).await {
        Ok(()) => reviewed_count(count),
        Err(e) => internal_error(e, "未確認に戻せない"),
    }
}

/// メモだけ保存する(確認済みかどうかは変えない)。
async fn note_post(State(state): State<AppState>, Json(req): Json<ReviewRequest>) -> Response {
    let Some(domain) = clean(req.domains).into_iter().next() else {
        return bad_request("domains required");
    };
    match state.db.save_note(domain, req.note.unwrap_or_default()).await {
        Ok(()) => success(),
        Err(e) => internal_error(e, "メモを保存できない"),
    }
}

#[derive(Deserialize)]
struct AskRequest {
    #[serde(default)]
    domains: Vec<String>,
}

/// 聞いて、結果をそのままメモに書き戻す(1件でも複数でも同じ)。
///
/// **区切るのは画面側**(`MAX_DOMAINS_PER_ASK` ずつ何度も呼ぶ)。1回のリクエストを
/// 短く保つと進捗が出せて、途中で失敗しても**そこまでのメモは残る** ——
/// 全件を1リクエストにすると、最後まで待たされたうえに落ちたら何も残らない。
async fn ask(State(state): State<AppState>, Json(req): Json<AskRequest>) -> Response {
    let domains = clean(req.domains);
    if domains.is_empty() {
        return bad_request("domains required");
    }
    if domains.len() > MAX_DOMAINS_PER_ASK {
        // 黙って切り詰めない —— 切った分が「聞いたのにメモが付かない」形で表に出る
        return bad_request("1回に聞ける件数を超えています");
    }

    let answer = match state.ai.ask_about_domains(&domains).await {
        Ok(answer) => answer,
        Err(AskError::TokenRequired) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "success": false, "error": "token_required" })),
            )
                .into_response();
        }
        Err(AskError::Failed(message)) => return bad_gateway(&message),
    };

    // **書き戻しは1トランザクション**。ここで失敗したら聞き直しになるので、
    // 半分だけ入って「どこまで済んだか」が分からない状態は作らない
    if let Err(e) = state.db.save_notes(answer.notes.clone()).await {
        return internal_error(e, "メモを保存できない");
    }

    let answered: HashSet<&String> = answer.notes.iter().map(|(domain, _)| domain).collect();
    let missing: Vec<&String> = domains.iter().filter(|d| !answered.contains(d)).collect();
    if !missing.is_empty() {
        tracing::warn!(?missing, "聞いたが答えが返らなかったドメインがある");
    }

    Json(json!({
        "success": true,
        // 実際に書いた相手。**複数いる**ので配列
        "authors": answer.authors,
        // 答えられなかった相手と理由(**1人落ちても残りは使う**)
        "failures": answer.failures,
        "results": answer.notes.iter()
            .map(|(domain, note)| json!({ "domain": domain, "note": note }))
            .collect::<Vec<_>>(),
        // 答えが返らなかった分。画面が「聞けなかった件数」を出せるようにする
        "missing": missing,
    }))
    .into_response()
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
        "bridge_label": BRIDGE_LABEL,
        // CLIブリッジを指す予約id。**画面に埋め込まない** —— 突き合わせる値は
        // サーバが持っているものをそのまま使う
        "bridge_backend": BRIDGE_BACKEND,
        // **有無だけ。** 値は返さない —— 画面が出すのは「登録済みか」だけでよい
        "token_saved": state.ai.has_token(),
        "backends": backends,
        "selections": state.ai.selections().await,
        // 実際に聞く相手の名前(**空にならない** —— 未選択ならCLIブリッジ)
        "current": state.ai.current_names().await,
        "error": error,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct SelectRequest {
    /// 空なら CLI ブリッジ経由に戻す。**複数選べる** —— 選んだ全員に聞いて、
    /// 答えを1つのメモに並べる。
    #[serde(default)]
    selections: Vec<SelectEntry>,
}

#[derive(Deserialize)]
struct SelectEntry {
    backend: String,
    #[serde(default)]
    model: String,
    #[serde(default)]
    effort: String,
}

/// 聞く相手を保存する。**実在しない相手は受け付けない** ——
/// 黙って保存すると、次に聞いたときまで間違いに気づけない。
async fn ai_post(State(state): State<AppState>, Json(req): Json<SelectRequest>) -> Response {
    // 同じ相手を2回選んでも意味が無い(同じ答えを2回もらうだけ)
    let mut seen = HashSet::new();
    let entries: Vec<SelectEntry> = req
        .selections
        .into_iter()
        .filter(|e| !e.backend.trim().is_empty())
        .filter(|e| seen.insert(e.backend.trim().to_string()))
        .collect();

    if entries.is_empty() {
        // 戻すだけなので Chiezo に問い合わせない(繋がらなくても戻せる必要がある)
        return match state.ai.select(&[]).await {
            Ok(()) => Json(json!({ "success": true, "current": [BRIDGE_LABEL] })).into_response(),
            Err(e) => internal_error(e, "AIの選択を保存できない"),
        };
    }

    // **Chiezo に問い合わせるのは、Chiezo の相手が選ばれているときだけ** ——
    // CLI ブリッジだけを選ぶ操作が、Chiezo の生死に左右されないようにする
    let backends = if entries.iter().any(|e| e.backend != BRIDGE_BACKEND) {
        match state.ai.backends().await {
            Ok(backends) => backends,
            Err(message) => return bad_gateway(&message),
        }
    } else {
        Vec::new()
    };

    let mut choices = Vec::new();
    for entry in entries {
        if entry.backend == BRIDGE_BACKEND {
            choices.push(AiChoice::bridge());
            continue;
        }

        let Some(backend) = backends.iter().find(|b| b.id == entry.backend) else {
            return bad_request("Chiezo にその相手がいません。一覧を読み直してください。");
        };

        let model = entry.model.trim().to_string();
        if model.is_empty() {
            if backend.model_required {
                return bad_request("この相手はモデルの指定が必要です。");
            }
        } else if !backend.models.contains(&model) {
            return bad_request("Chiezo が知らないモデルです。一覧を読み直してください。");
        }

        let effort = entry.effort.trim().to_string();
        if !effort.is_empty() && !backend.efforts.contains(&effort) {
            return bad_request("Chiezo が知らない考える量です。一覧を読み直してください。");
        }

        choices.push(AiChoice {
            backend: backend.id.clone(),
            // **表記は選んだ時点のものを保存する**(表示のたびに Chiezo へ聞きに行かない)
            label: backend.label.clone(),
            model: (!model.is_empty()).then_some(model),
            effort: (!effort.is_empty()).then_some(effort),
        });
    }

    match state.ai.select(&choices).await {
        Ok(()) => Json(json!({
            "success": true,
            "current": choices.iter().map(AiChoice::display_name).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(e) => internal_error(e, "AIの選択を保存できない"),
    }
}

/// 空白を落として重複を除く。**順番は保つ** ——
/// 画面が出した並びのまま処理したいため(進捗の見え方が変わらない)。
fn clean(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .filter(|v| seen.insert(v.clone()))
        .collect()
}

fn reviewed_count(count: usize) -> Response {
    Json(json!({ "success": true, "count": count })).into_response()
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

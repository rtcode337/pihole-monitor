//! `/api/*` のJSONエンドポイント。

use std::collections::{HashMap, HashSet};

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ai::{
    Ai, AiChoice, AskError, AskMode, BRIDGE_BACKEND, BRIDGE_LABEL, MAX_DOMAINS_PER_ASK,
};
use crate::db::Db;
use crate::pihole::PiholeClient;

/// ハンドラ間で共有する依存。いずれも中身は `Arc` か `Clone` が安いものなので、
/// axumがリクエストごとにcloneしても問題ない。
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub pihole: PiholeClient,
    pub ai: Ai,
    /// Pi-hole の**管理画面**の URL。理由の札から絞り込んだクエリログへ飛ばすために、
    /// `/api/watch` の応答へそのまま載せる(開くのはブラウザなので、APIのURLとは別に持てる)
    pub pihole_web_url: String,
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
        // 監視の基準日時。**ネットワークの設定を変えた日に押す** ——
        // 変える前の記録は別の環境のもので、同じ画面に混ぜるとノイズにしかならない
        .route("/api/watch/baseline", post(watch_baseline))
        // **1件を詳しく調べる。** `/api/ask` (まとめて短いメモを書かせる) とは
        // 役割も相手も違う —— こちらはメインの1人だけに、web 検索と観測データを
        // 渡して深く調べさせる
        .route("/api/investigate", post(investigate))
        // **調べた結果をもとに、もう一歩聞く。** 相手も材料も `/api/investigate` と同じで、
        // 違うのは「これまでのやり取りと質問を渡し、答えを調査結果の末尾に足す」ところ
        .route("/api/followup", post(followup))
        // 設定画面の疎通確認(ping / 経路)。**一覧の判定には関わらない** ——
        // 名前を引いた記録だけでは「その先に届くのか」が分からないので、手で叩ける口を置く
        .route("/api/diag", post(diag))
        .route("/api/ai", get(ai_get).post(ai_post))
        .route("/api/claude-token", post(claude_token))
}

#[derive(Serialize)]
struct DomainEntry {
    domain: String,
    count: u32,
    reviewed: bool,
    /// `""` = 未確認 / `"issue"` = 問題あり(ブロックされて当然) / `"ok"` = 問題なし(無害だった)
    verdict: String,
    note: String,
    /// 「詳しく調べる」の結果。**メモとは別**（詳細画面でメモの上に出す）
    research: String,
    researched_at: String,
}

#[derive(Deserialize)]
struct BaselineRequest {
    /// unix秒。**null なら解除**（既定の窓に戻る）
    #[serde(default)]
    at: Option<i64>,
}

/// 監視の基準日時を決める。
async fn watch_baseline(
    State(state): State<AppState>,
    Json(req): Json<BaselineRequest>,
) -> Response {
    // **未来は受け付けない。** 受け付けると候補が常に0件になり、
    // 「静かなのか壊れているのか」が画面から区別できなくなる
    let now = unix_now() as i64;
    if req.at.is_some_and(|at| at > now) {
        return bad_request("基準日時に未来は指定できません");
    }
    let value = req.at.map(|at| at.to_string());
    match state.db.set_setting(crate::watch::BASELINE_KEY, value).await {
        Ok(()) => Json(json!({ "success": true, "baseline": req.at })).into_response(),
        Err(e) => internal_error(e, "基準日時を保存できない"),
    }
}

#[derive(Deserialize)]
struct InvestigateRequest {
    domain: String,
    /// どちらの一覧から押されたか(`"blocked"` / `"watch"`)。**指示文がこれで変わる** ——
    /// 監視の候補を「ブロックされたドメイン」として説明させないため
    #[serde(default)]
    mode: String,
    /// 候補に挙げた理由(監視のときだけ。画面が出しているものをそのまま渡す)
    #[serde(default)]
    reason: String,
}

/// 1件のドメインを詳しく調べる。**メインのAI1人**に、web 検索とこちらの観測データを渡す。
async fn investigate(
    State(state): State<AppState>,
    Json(req): Json<InvestigateRequest>,
) -> Response {
    let domain = req.domain.trim().to_string();
    if domain.is_empty() {
        return bad_request("domain required");
    }

    // 観測データは**保持期間の窓**から作る(生のクエリが残っている範囲)
    let now = unix_now();
    let profile = match state
        .db
        .domain_profile(domain.clone(), now - PROFILE_WINDOW_SECS)
        .await
    {
        Ok(profile) => profile,
        Err(e) => return internal_error(e, "観測データを組み立てられない"),
    };

    let (author, note) = match state
        .ai
        .investigate(
            &domain,
            &format_profile(&profile, now),
            AskMode::parse(&req.mode),
            &req.reason,
        )
        .await
    {
        Ok(result) => result,
        Err(AskError::TokenRequired) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "success": false, "error": "token_required" })),
            )
                .into_response();
        }
        Err(AskError::Failed(message)) => return bad_gateway(&message),
    };

    // **メモには書かない。** メモは人が書く（あるいは「まとめてAIに聞く」が書く）もので、
    // 調査結果で黙って上書きすると、書いた判断が消える。画面は詳細でメモの上に出す
    if let Err(e) = state
        .db
        .save_research(domain.clone(), note.clone())
        .await
    {
        return internal_error(e, "調査結果を保存できない");
    }

    Json(json!({
        "success": true,
        "domain": domain,
        "research": note,
        "researched_at": chrono::Local::now().to_rfc3339(),
        "author": author,
    }))
    .into_response()
}

/// 観測データを渡す窓。**生のクエリが残っている範囲**(保持期間)より長くしても中身は増えない。
const PROFILE_WINDOW_SECS: f64 = 7.0 * 24.0 * 3600.0;

/// 追加の質問の長さの上限(文字)。**長文を貼り付けさせない** ——
/// 質問は「調べた結果のどこを掘るか」の一言で足り、
/// 材料(調査結果・観測データ)はこちらが付ける。
const MAX_QUESTION_CHARS: usize = 500;

#[derive(Deserialize)]
struct FollowupRequest {
    domain: String,
    /// 利用者の質問。**空は受け付けない**(何を聞くのか決まっていない問い合わせを投げない)
    question: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    reason: String,
}

/// 調査結果をもとに、もう一歩聞く。**答えは調査結果の末尾に足す** ——
/// 別の列に分けると「1つ目の答え」と「その続き」が離れて読めなくなるし、
/// 次の質問に渡す材料(それまでのやり取り)も組み立てにくい。
async fn followup(State(state): State<AppState>, Json(req): Json<FollowupRequest>) -> Response {
    let domain = req.domain.trim().to_string();
    let question = req.question.trim().to_string();
    if domain.is_empty() {
        return bad_request("domain required");
    }
    if question.is_empty() {
        return bad_request("質問を入れてください");
    }
    if question.chars().count() > MAX_QUESTION_CHARS {
        return bad_request("質問が長すぎます（500文字まで）");
    }

    // **調べた結果が無ければ聞かない。** 深掘りは前の答えを踏まえた続きなので、
    // 材料が無いまま投げると「詳しく調べる」を劣った形でやり直すだけになる
    // (画面は調査結果があるときしか入力欄を出さないので、これは直に POST された場合の守り)
    let research = match state.db.research(domain.clone()).await {
        Ok(research) if !research.trim().is_empty() => research,
        Ok(_) => return bad_request("先に「詳しく調べる」を実行してください"),
        Err(e) => return internal_error(e, "調査結果を読み出せない"),
    };

    let now = unix_now();
    let profile = match state
        .db
        .domain_profile(domain.clone(), now - PROFILE_WINDOW_SECS)
        .await
    {
        Ok(profile) => profile,
        Err(e) => return internal_error(e, "観測データを組み立てられない"),
    };

    let (author, answer) = match state
        .ai
        .follow_up(
            &domain,
            &format_profile(&profile, now),
            AskMode::parse(&req.mode),
            &req.reason,
            &research,
            &question,
        )
        .await
    {
        Ok(result) => result,
        Err(AskError::TokenRequired) => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "success": false, "error": "token_required" })),
            )
                .into_response();
        }
        Err(AskError::Failed(message)) => return bad_gateway(&message),
    };

    // **質問も一緒に残す。** 答えだけ足すと、後から読んだときに何に答えたのか分からない
    let addition = format!("── 質問: {question}\n{answer}");
    let merged = match state.db.append_research(domain.clone(), addition).await {
        Ok(merged) => merged,
        Err(e) => return internal_error(e, "調査結果を保存できない"),
    };

    Json(json!({
        "success": true,
        "domain": domain,
        // **全文を返す**(画面は差分を組み立てずに描き直せばよい)
        "research": merged,
        "researched_at": chrono::Local::now().to_rfc3339(),
        "author": author,
    }))
    .into_response()
}

/// 観測データを AI に渡す文面へ整える。**数字はそのまま渡す** ——
/// こちらで「怪しい」と解釈してから渡すと、その解釈ごと信じた答えが返ってくる。
fn format_profile(p: &crate::db::DomainProfile, now: f64) -> String {
    fn counts(label: &str, rows: &[(String, i64)]) -> String {
        if rows.is_empty() {
            return String::new();
        }
        let body = rows
            .iter()
            .map(|(k, n)| format!("{}={}", if k.is_empty() { "不明" } else { k }, n))
            .collect::<Vec<_>>()
            .join(", ");
        format!("- {label}: {body}
")
    }

    let mut out = String::new();
    if let Some(first) = p.first_seen {
        let days = ((now - first as f64) / 86_400.0).floor() as i64;
        out.push_str(&format!("- はじめて観測した日: 約{days}日前
"));
    }
    out.push_str(&format!("- 記録開始からの総問い合わせ回数: {}
", p.total));
    out.push_str(&counts("問い合わせた端末(直近7日)", &p.clients));
    out.push_str(&counts("Pi-holeの処理結果(直近7日)", &p.statuses));
    out.push_str(&counts("応答の種類(直近7日)", &p.replies));
    out.push_str(&counts("クエリ種別(直近7日)", &p.qtypes));
    out.push_str(
        "
補足: 処理結果の GRAVITY と SPECIAL_DOMAIN はPi-holeがブロックしたことを、         FORWARDED と CACHE は通したことを意味します。",
    );
    out
}

/// 「ブロックされていない怪しい通信」の候補。判定は watch.rs、材料は取り込んだ蓄積。
async fn watch(State(state): State<AppState>) -> Response {
    let now = unix_now();
    match crate::watch::candidates(&state.db, now, &state.pihole_web_url).await {
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
    /// 判定。**そのドメイン自身が問題のある通信か**を記録する。
    /// `"issue"` = 問題あり(広告・トラッカー等。ブロックされて当然) /
    /// `"ok"` = 問題なし(怪しい候補として挙がったが無害だった)。
    /// **省略は `"ok"`** —— 画面は必ず送るので、これは直に POST されたときの保険。
    #[serde(default)]
    verdict: String,
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
                verdict: record.map(|r| r.verdict.clone()).unwrap_or_default(),
                note: record.map(|r| r.note.clone()).unwrap_or_default(),
                research: record.map(|r| r.research.clone()).unwrap_or_default(),
                researched_at: record.map(|r| r.researched_at.clone()).unwrap_or_default(),
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
                verdict: record.verdict.clone(),
                note: record.note.clone(),
                research: record.research.clone(),
                researched_at: record.researched_at.clone(),
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
    // **知らない値は受け付けない。** 直に POST された値で、絞り込みのどこにも
    // 出てこない行を作らせない
    let verdict = match req.verdict.trim() {
        "" | "ok" => "ok",
        "issue" => "issue",
        _ => return bad_request("verdict は ok か issue"),
    };
    match state
        .db
        .set_reviewed(domains, true, Some(verdict.to_string()), req.note)
        .await
    {
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
    match state.db.set_reviewed(domains, false, None, None).await {
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
    /// どちらの一覧について聞いているか(`"blocked"` / `"watch"`)。
    /// **未指定はブロック済み** —— 既定の一覧がそちらなので、古い画面や手で叩いた
    /// リクエストもそこに倒れる
    #[serde(default)]
    mode: String,
    /// ドメイン → 候補に挙げた理由(監視のときだけ)。**配列ではなく対応表で受ける** ——
    /// 並びで対応させると、重複や空白落とし(`clean`)でずれる
    #[serde(default)]
    reasons: HashMap<String, String>,
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

    let answer = match state
        .ai
        .ask_about_domains(&domains, &req.reasons, AskMode::parse(&req.mode))
        .await
    {
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

#[derive(Deserialize)]
struct DiagRequest {
    /// `ping` か `traceroute`。**知らない値は断る**(呼ぶ側にコマンドを組ませない)
    tool: String,
    /// 相手先(ホスト名かIP)。文字の確かめは `diag::run` の中
    target: String,
}

/// 疎通を確かめる(設定画面から手で叩く)。
///
/// **結果は加工せずに返す。** 見たいのは応答時間・欠落・どのホップで止まったかで、
/// こちらで要約すると落ちる。**終了コードが0でなくても失敗ではない**
/// (応答が無いのも結果のうち)ので、成功として本文と一緒に返す。
async fn diag(State(_state): State<AppState>, Json(req): Json<DiagRequest>) -> Response {
    let Some(tool) = crate::diag::Tool::parse(&req.tool) else {
        return bad_request("ping か traceroute を指定してください");
    };

    match crate::diag::run(tool, &req.target).await {
        Ok(outcome) => Json(json!({
            "success": true,
            "command": outcome.command,
            "output": outcome.output,
            "ok": outcome.ok,
            "elapsed_ms": outcome.elapsed_ms,
        }))
        .into_response(),
        // 打てなかった(相手先の形が悪い・コマンドが無い・時間切れ)。理由をそのまま出す
        Err(message) => bad_request(&message),
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
        "bridge_label": BRIDGE_LABEL,
        // CLIブリッジを指す予約id。**画面に埋め込まない** —— 突き合わせる値は
        // サーバが持っているものをそのまま使う
        "bridge_backend": BRIDGE_BACKEND,
        // **有無だけ。** 値は返さない —— 画面が出すのは「登録済みか」だけでよい
        "token_saved": state.ai.has_token(),
        "backends": backends,
        "selections": state.ai.selections().await,
        // **メインの1人**(「詳しく調べる」の宛先)。画面のラジオを立てるのに使う
        "primary": state.ai.primary_backend().await,
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
    /// 「詳しく調べる」を頼む1人。**複数立っていても先頭だけを採る**
    /// (画面はラジオなので普通は1人だが、直に POST されたら分からない)。
    #[serde(default)]
    primary: bool,
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
            primary: entry.primary,
        });
    }

    // **メインは必ず1人にする。** 誰も立っていなければ先頭を立てる ——
    // 立っていないと「詳しく調べる」が誰に行くのか画面から読めない。
    // 複数立っていたら先頭だけ残す(直に POST された値を信じない)
    let mut seen_primary = false;
    for choice in choices.iter_mut() {
        if choice.primary && !seen_primary {
            seen_primary = true;
        } else {
            choice.primary = false;
        }
    }
    if !seen_primary {
        if let Some(first) = choices.first_mut() {
            first.primary = true;
        }
    }

    match state.ai.select(&choices).await {
        // **保存したあとに読み直す。** 実際に聞く並び(メインが先頭)で返さないと、
        // 保存直後のトーストだけ画面のボタンと違う順で出る
        Ok(()) => Json(json!({
            "success": true,
            "current": state.ai.current_names().await,
        }))
        .into_response(),
        Err(e) => internal_error(e, "AIの選択を保存できない"),
    }
}

fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
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

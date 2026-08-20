//! `/api/*` のJSONエンドポイント。

use std::collections::{HashMap, HashSet};

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{StreamExt, iter as stream_iter};

use crate::ai::{
    Ai, AiChoice, AskError, AskMode, CLI_BACKEND, CLI_LABEL, MAX_DOMAINS_PER_ASK,
};
use crate::db::Db;
use crate::diag::Event as DiagEvent;
use crate::pihole::PiholeClient;

/// ハンドラ間で共有する依存。いずれも中身は `Arc` か `Clone` が安いものなので、
/// axumがリクエストごとにcloneしても問題ない。
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub pihole: PiholeClient,
    pub ai: Ai,
    /// Pi-hole の管理画面の URL。理由の札から絞り込んだクエリログへ飛ばすために、
    /// `/api/watch` の応答へそのまま載せる(開くのはブラウザなので、APIのURLとは別に持てる)
    pub pihole_web_url: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/domains", get(domains))
        .route("/api/review", post(review_post).delete(review_delete))
        // メモは確認済みと独立している。 確認済みにしないと残せないと、
        // 「まだ判断していないが調べた内容は残したい」が表せない
        .route("/api/note", post(note_post))
        // 口は1つ。 行のボタン(1件)も「まとめて聞く」(区切って何度も)も同じここを通り、
        // どちらも結果をメモとして保存する —— 1件用の口を別に持つと、指示文と保存の仕方が
        // 2か所に分かれる
        .route("/api/ask", post(ask))
        // 「ブロックされていない怪しい通信」の候補。ブロック済みの一覧とは別の口 ——
        // あちらは Pi-hole をその場で叩く集計、こちらは貯めた過去との突き合わせで、
        // 材料も判定も違う(watch.rs)
        .route("/api/watch", get(watch))
        // 監視の基準日時。ネットワークの設定を変えた日に押す ——
        // 変える前の記録は別の環境のもので、同じ画面に混ぜるとノイズにしかならない
        .route("/api/watch/baseline", post(watch_baseline))
        // 1件を詳しく調べる。 `/api/ask` (まとめて短いメモを書かせる) とは
        // 役割も相手も違う —— こちらはメインの1人だけに、web 検索と観測データを
        // 渡して深く調べさせる
        .route("/api/investigate", post(investigate))
        // 調べた結果をもとに、もう一歩聞く。 相手も材料も `/api/investigate` と同じで、
        // 違うのは「これまでのやり取りと質問を渡し、答えを調査結果の末尾に足す」ところ
        .route("/api/followup", post(followup))
        // 設定画面の疎通確認(ping / 経路)。一覧の判定には関わらない ——
        // 名前を引いた記録だけでは「その先に届くのか」が分からないので、手で叩ける口を置く
        .route("/api/diag", post(diag))
        // メモ・確認済みが残っているドメインの控え。設定のページが読む ——
        // 2つの一覧はどちらも「いま出ているもの」しか並べないので、
        // 落ち着いたドメインの記録はここからしか辿れない
        .route("/api/notes", get(notes_get))
        // 画面から決める設定。環境変数では渡せない —— 入口が2つあると
        // 「どちらの値が効いているのか」を画面が説明し続けることになる
        .route("/api/settings", get(settings_get).post(settings_post))
        .route("/api/ai", get(ai_get).post(ai_post))
        .route(
            "/api/claude-token",
            post(claude_token).delete(claude_token_delete),
        )
}

#[derive(Serialize)]
struct DomainEntry {
    domain: String,
    count: u32,
    reviewed: bool,
    note: String,
    /// 「詳しく調べる」の結果。メモとは別（詳細画面でメモの上に出す）
    research: String,
    researched_at: String,
    /// ブロックされた通信を出した端末（件数の多い順）。出どころは貯めたクエリで、
    /// 件数（Pi-hole の集計）とは範囲が違う —— 画面が前置きでそう断っている
    clients: Vec<String>,
    /// ブロックされた通信が起きていた期間（unix秒。分からなければ 0）。
    /// 監視の候補（`WatchItem`）と同じ名前・同じ意味にしてある
    active_from: i64,
    active_to: i64,
}

#[derive(Deserialize)]
struct BaselineRequest {
    /// unix秒。null なら解除（既定の窓に戻る）
    #[serde(default)]
    at: Option<i64>,
}

/// 監視の基準日時を決める。
async fn watch_baseline(
    State(state): State<AppState>,
    Json(req): Json<BaselineRequest>,
) -> Response {
    // 未来は受け付けない。 受け付けると候補が常に0件になり、
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
    /// どちらの一覧から押されたか(`"blocked"` / `"watch"`)。指示文がこれで変わる ——
    /// 監視の候補を「ブロックされたドメイン」として説明させないため
    #[serde(default)]
    mode: String,
    /// 候補に挙げた理由(監視のときだけ。画面が出しているものをそのまま渡す)
    #[serde(default)]
    reason: String,
}

/// 1件のドメインを詳しく調べる。メインのAI1人に、web 検索とこちらの観測データを渡す。
async fn investigate(
    State(state): State<AppState>,
    Json(req): Json<InvestigateRequest>,
) -> Response {
    let domain = req.domain.trim().to_string();
    if domain.is_empty() {
        return bad_request("domain required");
    }

    // 観測データは保持期間の窓から作る(生のクエリが残っている範囲)
    let now = unix_now();
    let profile = match state
        .db
        .domain_profile(domain.clone(), now - PROFILE_WINDOW_SECS)
        .await
    {
        Ok(profile) => profile,
        Err(e) => return internal_error(e, "観測データを組み立てられない"),
    };

    let found = match state
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

    // 調査結果はメモとは別の列に入れる。画面は詳細でメモの上に出す
    if let Err(e) = state
        .db
        .save_research(domain.clone(), found.research.clone())
        .await
    {
        return internal_error(e, "調査結果を保存できない");
    }

    // メモが空のときだけ「ひとこと」を書く。 調べた以上は一覧にも一言残っていて
    // ほしいが、人が書いた（あるいは「まとめてAIに聞く」が書いた）判断は上書きしない
    // —— 調査結果で黙って上書きすると、書いた判断が消える
    let note = match found.summary.clone() {
        Some(summary) => match state.db.save_note_if_empty(domain.clone(), summary).await {
            Ok(written) => written,
            Err(e) => return internal_error(e, "メモを保存できない"),
        },
        None => None,
    };

    Json(json!({
        "success": true,
        "domain": domain,
        "research": found.research,
        "researched_at": chrono::Local::now().to_rfc3339(),
        "author": found.author,
        // 書いたときだけ入る（既にメモがあれば null）。画面はこれを見て一覧に映す
        "note": note,
    }))
    .into_response()
}

/// 観測データを渡す窓。生のクエリが残っている範囲(保持期間)より長くしても中身は増えない。
const PROFILE_WINDOW_SECS: f64 = 7.0 * 24.0 * 3600.0;

/// 追加の質問の長さの上限(文字)。長文を貼り付けさせない ——
/// 質問は「調べた結果のどこを掘るか」の一言で足り、
/// 材料(調査結果・観測データ)はこちらが付ける。
const MAX_QUESTION_CHARS: usize = 500;

#[derive(Deserialize)]
struct FollowupRequest {
    domain: String,
    /// 利用者の質問。空は受け付けない(何を聞くのか決まっていない問い合わせを投げない)
    question: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    reason: String,
}

/// 調査結果をもとに、もう一歩聞く。答えは調査結果の末尾に足す ——
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

    // 調べた結果が無ければ聞かない。 深掘りは前の答えを踏まえた続きなので、
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

    // 質問も一緒に残す。 答えだけ足すと、後から読んだときに何に答えたのか分からない
    let addition = format!("── 質問: {question}\n{answer}");
    let merged = match state.db.append_research(domain.clone(), addition).await {
        Ok(merged) => merged,
        Err(e) => return internal_error(e, "調査結果を保存できない"),
    };

    Json(json!({
        "success": true,
        "domain": domain,
        // 全文を返す(画面は差分を組み立てずに描き直せばよい)
        "research": merged,
        "researched_at": chrono::Local::now().to_rfc3339(),
        "author": author,
    }))
    .into_response()
}

/// 観測データを AI に渡す文面へ整える。数字はそのまま渡す ——
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

/// 確認済み/未確認の切り替えとメモの保存。ドメインは配列 ——
/// 1件でもチェックした複数件でも同じ口を通す(一括用の口を別に持つと、
/// 「メモを巻き込まない」等の決めごとが2か所に分かれる)。
#[derive(Deserialize)]
struct ReviewRequest {
    #[serde(default)]
    domains: Vec<String>,
    /// 判定。そのドメイン自身が問題のある通信かを記録する。
    /// 省略したらメモは触らない。 一括で確認済みにするときに、
    /// 既に付いているメモ(AIに聞いた結果)を空で上書きしないため。
    note: Option<String>,
}

#[derive(Deserialize)]
struct TokenRequest {
    #[serde(default)]
    token: String,
}

/// Pi-hole から取り込むブロック済みクエリの件数。DB に持つ(画面から変える)。
const QUERY_LIMIT_KEY: &str = "pihole:query_limit";

/// 既定は全件。Pi-hole v6 API の `length` は省くと 100 件で切れるので、
/// こちらから明示しないと「一覧が途中までしか出ない」ことに気づけない。
const QUERY_LIMIT_DEFAULT: i64 = -1;

/// 1回に取り込む件数の上限。青天井にしない —— 大きすぎる値を入れると
/// Pi-hole の応答が数十MBになり、取り込みのたびに読み切れずに詰まる。
const QUERY_LIMIT_MAX: i64 = 1_000_000;

/// いまの取得件数。読めない値は既定に倒す(画面から入れ直せる)。
async fn query_limit(state: &AppState) -> i64 {
    match state.db.setting(QUERY_LIMIT_KEY).await {
        Ok(Some(raw)) => raw.parse().unwrap_or(QUERY_LIMIT_DEFAULT),
        Ok(None) => QUERY_LIMIT_DEFAULT,
        Err(e) => {
            tracing::warn!(error = ?e, "取得件数の設定を読み出せない");
            QUERY_LIMIT_DEFAULT
        }
    }
}

/// 画面から決める設定を返す。
async fn settings_get(State(state): State<AppState>) -> Response {
    Json(json!({
        "query_limit": query_limit(&state).await,
        "query_limit_default": QUERY_LIMIT_DEFAULT,
        "query_limit_max": QUERY_LIMIT_MAX,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct SettingsRequest {
    /// 取得件数。`-1` で全件。知らない値は断る(下の検証)
    query_limit: i64,
}

/// 設定を保存する。次の取得から効く(再起動は要らない)。
async fn settings_post(State(state): State<AppState>, Json(req): Json<SettingsRequest>) -> Response {
    // `0` と負の数(-1 以外)は断る。 0 件は「一覧が空になる」設定で、
    // 押した人が意図することはまず無い —— 全件のつもりなら -1
    if req.query_limit != QUERY_LIMIT_DEFAULT
        && (req.query_limit < 1 || req.query_limit > QUERY_LIMIT_MAX)
    {
        return bad_request(&format!(
            "取得件数は -1(全件)か 1〜{QUERY_LIMIT_MAX} の数で指定してください"
        ));
    }
    match state
        .db
        .set_setting(QUERY_LIMIT_KEY, Some(req.query_limit.to_string()))
        .await
    {
        Ok(()) => Json(json!({ "success": true, "query_limit": req.query_limit })).into_response(),
        Err(e) => internal_error(e, "設定を保存できない"),
    }
}

/// ブロック済みドメイン一覧。確認済みフラグとメモを付けて返す。
async fn domains(State(state): State<AppState>) -> Response {
    // 件数は実行のたびに読む(設定を変えたら次の更新から効く)
    let limit = query_limit(&state).await;
    let blocked = match state.pihole.blocked_domains(limit).await {
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

    // 出すのは Pi-hole の集計に載っているものだけ。 かつては記録(メモ・確認済み)の
    // あるドメインを「件数0」として足していたが、監視の側で調べたメモもここに現れる
    // ようになって破綻した —— 監視で「詳しく調べる」を押した `chatgpt.com` が、
    // 一度も止められていないのに「ブロック済み」の一覧に並ぶ(実測: 手元の記録 1,466 件が
    // 全部素通り)。いま止められていないものは、この一覧に置く理由が無い。
    // 一覧から外れたメモは消えていない —— 設定のページの「メモが残っているドメイン」で
    // 全部読める(`/api/notes`)
    let mut entries: Vec<DomainEntry> = blocked
        .iter()
        .map(|(domain, count)| {
            let record = records.get(domain);
            DomainEntry {
                domain: domain.clone(),
                count: *count,
                reviewed: record.is_some_and(|r| r.reviewed),
                note: record.map(|r| r.note.clone()).unwrap_or_default(),
                research: record.map(|r| r.research.clone()).unwrap_or_default(),
                researched_at: record.map(|r| r.researched_at.clone()).unwrap_or_default(),
                clients: Vec::new(),
                active_from: 0,
                active_to: 0,
            }
        })
        .collect();

    // アクセス元と期間は貯めたクエリから足す。 Pi-hole の集計は
    // 「ドメインと件数」しか返さないので、誰が引いたのかも、いつからいつまで
    // 鳴っていたのかもあちらからは分からない。
    //
    // 数えるのは止められたクエリだけ(`blocked_only`)。 同じドメインが通ったり
    // 止まったりする(端末ごとの設定・CNAME 経由)ので、素通りしたぶんまで混ぜると
    // 「ブロックされた通信の期間」ではなくなる。
    // 範囲は手元に残っている全部(`since` = 0)—— 保持期間より前は分からないので、
    // どこまで見えているかは画面が前置きで断る
    let names: Vec<String> = entries.iter().map(|e| e.domain.clone()).collect();
    let activity = match state.db.domain_activity_since(0.0, names, true).await {
        Ok(activity) => activity,
        Err(e) => return internal_error(e, "ブロックされた通信の記録を読み出せない"),
    };
    for entry in &mut entries {
        if let Some(seen) = activity.get(&entry.domain) {
            entry.clients = seen.clients.clone();
            entry.active_from = seen.first_ts as i64;
            entry.active_to = seen.last_ts as i64;
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

    // 監視(`/api/watch`)と同じく、一覧そのものではなく「一覧 + どこまで見えているか」を返す。
    // アクセス元と期間の出どころ(貯めたクエリ)は件数(Pi-hole の集計)と範囲が違うので、
    // 画面がそれを断れるだけの材料をここで渡す
    let stats = match state.db.ingest_stats().await {
        Ok(stats) => stats,
        Err(e) => return internal_error(e, "取り込みの状況を読み出せない"),
    };
    Json(json!({
        // 理由の札と同じく、ドメインで絞り込んだクエリログへ飛ばすために渡す
        // (空なら画面はリンクにしない)
        "pihole_url": state.pihole_web_url,
        // 貯めたクエリの一番古い時刻(unix秒)。アクセス元と期間が見えている範囲そのもの
        "data_since": stats.oldest_ts.map(|ts| ts as i64),
        // サーバのいま(unix秒)。画面が「まだ続いている通信か」を判定するのに使う ——
        // 画面の時計で測ると、端末の時計がずれているだけで常に続いて見えたり、
        // 逆に一度も続かなくなる(記録の時刻は Pi-hole 側の時計で付いている)
        "now": unix_now() as i64,
        "items": entries,
    }))
    .into_response()
}

/// 控えの1ページの件数(画面もこの数で区切る)。
const NOTES_LIMIT_DEFAULT: i64 = 50;

/// 1回に返す上限。青天井にしない —— 調査結果は1件で1KBを超えることがあり、
/// 大きな値を渡されると1リクエストで数MBを運ぶことになる。
const NOTES_LIMIT_MAX: i64 = 200;

#[derive(Deserialize)]
struct NotesQuery {
    /// 何件目から(既定は先頭)
    #[serde(default)]
    offset: i64,
    /// 1ページの件数(既定 [`NOTES_LIMIT_DEFAULT`])
    #[serde(default)]
    limit: Option<i64>,
}

/// メモ・確認済みの残っているドメインを1ページぶん返す(設定のページの控え)。
///
/// 一覧の口(`/api/domains` / `/api/watch`)と違って Pi-hole を叩かない ——
/// 中身は手元の記録そのもので、いま通信が出ているかどうかとは関係が無い。
///
/// 範囲の外を渡されても断らずに丸める。 押した人が組み立てる値ではなく画面が
/// 送るものなので、400 を返しても打つ手が無い(控えが空で出るだけになる)。
async fn notes_get(State(state): State<AppState>, Query(q): Query<NotesQuery>) -> Response {
    let offset = q.offset.max(0);
    let limit = q.limit.unwrap_or(NOTES_LIMIT_DEFAULT).clamp(1, NOTES_LIMIT_MAX);
    match state.db.notes_page(offset, limit).await {
        Ok((items, total)) => Json(json!({
            "items": items,
            "total": total,
            "offset": offset,
            "limit": limit,
        }))
        .into_response(),
        Err(e) => internal_error(e, "メモの一覧を読み出せない"),
    }
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

/// 未確認に戻す。メモは消さない(メモが空の行だけ消える)。
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
    /// どちらの一覧について聞いているか(`"blocked"` / `"watch"`)。
    /// 未指定はブロック済み —— 既定の一覧がそちらなので、古い画面や手で叩いた
    /// リクエストもそこに倒れる
    #[serde(default)]
    mode: String,
    /// ドメイン → 候補に挙げた理由(監視のときだけ)。配列ではなく対応表で受ける ——
    /// 並びで対応させると、重複や空白落とし(`clean`)でずれる
    #[serde(default)]
    reasons: HashMap<String, String>,
}

/// 聞いて、結果をそのままメモに書き戻す(1件でも複数でも同じ)。
///
/// 区切るのは画面側(`MAX_DOMAINS_PER_ASK` ずつ何度も呼ぶ)。1回のリクエストを
/// 短く保つと進捗が出せて、途中で失敗してもそこまでのメモは残る ——
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

    // 書き戻しは1トランザクション。ここで失敗したら聞き直しになるので、
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
        // 実際に書いた相手。複数いるので配列
        "authors": answer.authors,
        // 答えられなかった相手と理由(1人落ちても残りは使う)
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
    /// `ping` か `traceroute`。知らない値は断る(呼ぶ側にコマンドを組ませない)
    tool: String,
    /// 相手先(ホスト名かIP)。文字の確かめは `diag::run` の中
    target: String,
}

/// 疎通を確かめる(設定画面から手で叩く)。
///
/// 結果は加工せずに返す。 見たいのは応答時間・欠落・どのホップで止まったかで、
/// こちらで要約すると落ちる。終了コードが0でなくても失敗ではない
/// (応答が無いのも結果のうち)ので、エラーにはせず `end` に添えて返す。
///
/// 応答は溜めずに流す(1行1JSON = NDJSON)。ping は4回・経路は応答しないホップが
/// あると数十秒かかるので、終わるまで待たせると画面が止まって見える。
/// 種類は4つ —— `start`(走らせたコマンド)/ `line`(出力の1行)/ `name`(その行のIPの名前。
/// 後から届いて行に足される)/ `end`(終了コードとかかった時間)、それに `error`。
///
/// 打てなかったときだけ400のJSON(相手先の形が悪い・コマンドが無い)——
/// 流し始めてから言うと、画面は 200 を受け取った後でエラーを読むことになる。
async fn diag(State(_state): State<AppState>, Json(req): Json<DiagRequest>) -> Response {
    let Some(tool) = crate::diag::Tool::parse(&req.tool) else {
        return bad_request("ping か traceroute を指定してください");
    };

    let session = match crate::diag::start(tool, &req.target) {
        Ok(session) => session,
        Err(message) => return bad_request(&message),
    };

    // 走らせたコマンドを先に流す。画面はこれを見て「実行中」の見出しを出す
    let head = stream_iter([ndjson(&json!({
        "t": "start",
        "command": session.command,
    }))]);
    let body = head.chain(ReceiverStream::new(session.events).map(|event| {
        ndjson(&match event {
            DiagEvent::Line { index, text } => json!({"t": "line", "index": index, "text": text}),
            DiagEvent::Name { index, ip, name } => {
                json!({"t": "name", "index": index, "ip": ip, "name": name})
            }
            DiagEvent::End { ok, elapsed_ms } => {
                json!({"t": "end", "ok": ok, "elapsed_ms": elapsed_ms})
            }
            DiagEvent::Error { message } => json!({"t": "error", "message": message}),
        })
    }));

    Response::builder()
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        // 途中経過なので溜めさせない(間に入るものが溜めると流す意味が無くなる)
        .header(header::CACHE_CONTROL, "no-store")
        .header("X-Accel-Buffering", "no")
        .body(Body::from_stream(body))
        .expect("ヘッダは固定値なので組み立てに失敗しない")
}

/// 1行1JSON。流す側で改行まで付ける(受け取る側は行で切るだけでよい)。
fn ndjson(value: &serde_json::Value) -> Result<String, std::convert::Infallible> {
    Ok(format!("{value}\n"))
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

/// 保存したトークンを消す。消しても選択は残す —— 入れ直せば元の相手に戻る
/// (Chiezo の相手を選んでいれば、トークンが無くてもそのまま聞ける)。
async fn claude_token_delete(State(state): State<AppState>) -> Response {
    match state.ai.delete_token() {
        Ok(()) => success(),
        Err(e) => internal_error(e, "トークンを削除できない"),
    }
}

/// いま選べる相手と、選ばれているもの。繋がらない理由も一緒に返す ——
/// 一覧が空なのが「未設定」なのか「届かない」のかを画面が言い分けられるように。
async fn ai_get(State(state): State<AppState>) -> Response {
    let (backends, error) = match state.ai.backends().await {
        Ok(backends) => (backends, None),
        Err(message) => (Vec::new(), Some(message)),
    };

    Json(json!({
        "chiezo_url": state.ai.chiezo_url(),
        "cli_label": CLI_LABEL,
        // 同梱の CLI を指す予約id。画面に埋め込まない —— 突き合わせる値は
        // サーバが持っているものをそのまま使う
        "cli_backend": CLI_BACKEND,
        // 有無だけ。 値は返さない —— 画面が出すのは「登録済みか」だけでよい
        "token_saved": state.ai.has_token(),
        "backends": backends,
        "selections": state.ai.selections().await,
        // メインの1人(「詳しく調べる」の宛先)。画面のラジオを立てるのに使う
        "primary": state.ai.primary_backend().await,
        // 実際に聞く相手の名前(空にならない —— 未選択なら同梱の CLI)
        "current": state.ai.current_names().await,
        "error": error,
    }))
    .into_response()
}

#[derive(Deserialize)]
struct SelectRequest {
    /// 空なら同梱の CLI に戻す。複数選べる —— 選んだ全員に聞いて、
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
    /// 「詳しく調べる」を頼む1人。複数立っていても先頭だけを採る
    /// (画面はラジオなので普通は1人だが、直に POST されたら分からない)。
    #[serde(default)]
    primary: bool,
}

/// 聞く相手を保存する。実在しない相手は受け付けない ——
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
            Ok(()) => Json(json!({ "success": true, "current": [CLI_LABEL] })).into_response(),
            Err(e) => internal_error(e, "AIの選択を保存できない"),
        };
    }

    // Chiezo に問い合わせるのは、Chiezo の相手が選ばれているときだけ ——
    // 同梱の CLI だけを選ぶ操作が、Chiezo の生死に左右されないようにする
    let backends = if entries.iter().any(|e| e.backend != CLI_BACKEND) {
        match state.ai.backends().await {
            Ok(backends) => backends,
            Err(message) => return bad_gateway(&message),
        }
    } else {
        Vec::new()
    };

    let mut choices = Vec::new();
    for entry in entries {
        if entry.backend == CLI_BACKEND {
            choices.push(AiChoice::cli());
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
            // 表記は選んだ時点のものを保存する(表示のたびに Chiezo へ聞きに行かない)
            label: backend.label.clone(),
            model: (!model.is_empty()).then_some(model),
            effort: (!effort.is_empty()).then_some(effort),
            primary: entry.primary,
        });
    }

    // メインは必ず1人にする。 誰も立っていなければ先頭を立てる ——
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
        // 保存したあとに読み直す。 実際に聞く並び(メインが先頭)で返さないと、
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

/// 空白を落として重複を除く。順番は保つ ——
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

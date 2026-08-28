//! 画面(HTML/CSS/JS)とアイコン・マニフェストの配信。
//!
//! どのファイルも `include_str!` / `include_bytes!` で実行ファイルに埋め込んでいる。
//! 実行ファイル1個だけを配れば動くので、コンテナイメージに静的ファイルを別途COPYする
//! 必要がない。そのぶん、CSSやJSだけを直した場合も再ビルドが要る。

use axum::Router;
use axum::extract::{RawQuery, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;

use crate::api::AppState;

const INDEX_HTML: &str = include_str!("../static/index.html");
const STYLE_CSS: &str = include_str!("../static/css/style.css");
const APP_JS: &str = include_str!("../static/js/app.js");
const MANIFEST: &str = include_str!("../static/manifest.webmanifest");

// アイコン。SVGが本体で、PNGは `scripts/gen_icons.py` が同じ図形から書き出したもの
// (iOSのホーム画面もAndroidのマニフェストもSVGを受け付けないため)
const ICON_SVG: &str = include_str!("../static/icon.svg");
const ICON_32: &[u8] = include_bytes!("../static/icon-32.png");
const ICON_180: &[u8] = include_bytes!("../static/icon-180.png");
const ICON_192: &[u8] = include_bytes!("../static/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../static/icon-512.png");

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/static/css/style.css", get(style_css))
        .route("/static/js/app.js", get(app_js))
        .route("/static/manifest.webmanifest", get(manifest))
        .route("/static/icon.svg", get(icon_svg))
        .route("/static/icon-32.png", get(|| png(ICON_32)))
        .route("/static/icon-180.png", get(|| png(ICON_180)))
        .route("/static/icon-192.png", get(|| png(ICON_192)))
        .route("/static/icon-512.png", get(|| png(ICON_512)))
        // HTMLで明示していなくても取りに来るブラウザがいるので、32pxを返しておく
        .route("/favicon.ico", get(|| png(ICON_32)))
        // 画面の「Pi-holeで見る」。直接リンクにしない理由は pihole_queries を参照
        .route("/go/queries", get(pihole_queries))
}

/// Pi-hole のクエリログへ、絞り込みを付けて飛ばす(画面の「Pi-holeで見る」)。
///
/// 直接リンクにしないのは、**Pi-hole v6 が行き先を覚えない**ため。未ログインだと FTL は
/// `/admin/login` へ 302 で送り、ログイン後の飛び先はダッシュボード固定なので、絞り込みは
/// 捨てられる —— 目的のページに着くには2回押すことになる。
///
/// `PIHOLE_WEB_AUTO_LOGIN` が有効なら、ここでセッション(sid)を付けてから飛ばす。
/// FTL は URL の sid でも認証を通し、その応答で sid の cookie も配るので、1回で着くうえ
/// 続きの画面遷移もログイン済みのまま進める(既定は無効。理由は `Config` 側に書いてある)。
///
/// 無効なとき・セッションを取れなかったときも飛ばし先は同じで、これまでどおり
/// ログイン画面を挟むだけになる。
async fn pihole_queries(State(state): State<AppState>, RawQuery(query): RawQuery) -> Response {
    if state.pihole_web_url.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            "Pi-hole の管理画面の URL が分からない(PIHOLE_WEB_URL / PIHOLE_BASE_URL)",
        )
            .into_response();
    }
    // 絞り込みは組み立てたものをそのまま渡す(値は画面側で encodeURIComponent 済み)。
    // 制御文字だけは弾く —— 行き先は Location ヘッダに入るので、改行が混じると応答を割られる
    let filter = query.unwrap_or_default();
    if filter.chars().any(char::is_control) {
        return (StatusCode::BAD_REQUEST, "絞り込みに制御文字が混じっている").into_response();
    }
    let mut params = passthrough_params(&filter);
    if state.pihole_auto_login {
        match state.pihole.session_id().await {
            Ok(sid) => params.push(format!("sid={}", percent_encode(&sid))),
            // 取れなくても飛ばす。ログイン画面を挟む今までの手数に戻るだけで、
            // ここでエラー画面を出すと「クエリログを見に行く」が丸ごと止まる
            Err(e) => {
                tracing::warn!(error = %e, "Pi-holeのセッションを取れないので、ログインなしで飛ばす");
            }
        }
    }
    let query = if params.is_empty() {
        String::new()
    } else {
        format!("?{}", params.join("&"))
    };
    // `.lp` は FTL が拡張子なしへ 301 で直す(絞り込みも sid も引き継がれる)。
    // 古い版でも開けるよう、こちらは `.lp` のまま渡す
    let url = format!("{}/admin/queries.lp{}", state.pihole_web_url, query);
    match HeaderValue::from_str(&url) {
        Ok(location) => (
            StatusCode::SEE_OTHER,
            [
                (header::LOCATION, location),
                // sid が乗るので、途中にも履歴にも残させない
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
        )
            .into_response(),
        Err(_) => (StatusCode::BAD_REQUEST, "行き先を URL にできない").into_response(),
    }
}

/// 受け取った絞り込みのうち、そのまま渡すぶん。
///
/// **`sid` は呼ぶ側に決めさせない** —— 付けるかどうかは設定で決まるので、
/// 混ぜられたものは落として付け直す。
fn passthrough_params(filter: &str) -> Vec<String> {
    filter
        .split('&')
        .filter(|p| !p.is_empty() && !p.starts_with("sid="))
        .map(str::to_string)
        .collect()
}

/// URL に載せるための最小限のエスケープ。sid にしか使わない ——
/// Pi-hole の sid には `+` や `/` が入ることがあり、素で載せると `+` が空白として
/// 解釈されてセッションが通らない。
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

async fn index() -> impl IntoResponse {
    Html(INDEX_HTML)
}

async fn style_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLE_CSS,
    )
}

async fn app_js() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        APP_JS,
    )
}

async fn manifest() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/manifest+json; charset=utf-8")],
        MANIFEST,
    )
}

async fn icon_svg() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "image/svg+xml; charset=utf-8")],
        ICON_SVG,
    )
}

async fn png(bytes: &'static [u8]) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_caller_supplied_sid_is_dropped() {
        assert_eq!(
            passthrough_params("domain=example.com&sid=injected&from=1"),
            vec!["domain=example.com".to_string(), "from=1".to_string()],
        );
    }

    #[test]
    fn an_empty_filter_is_fine() {
        assert!(passthrough_params("").is_empty());
    }

    #[test]
    fn the_sid_is_escaped_including_symbols() {
        // Pi-hole の sid には `+` や `/` が入る。素で載せると `+` が空白として
        // 解釈され、セッションが通らない
        assert_eq!(percent_encode("aB9-_.~"), "aB9-_.~");
        assert_eq!(percent_encode("a+b/c="), "a%2Bb%2Fc%3D");
    }
}

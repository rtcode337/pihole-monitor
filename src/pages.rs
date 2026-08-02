//! 画面(HTML/CSS/JS)とアイコン・マニフェストの配信。
//!
//! どのファイルも `include_str!` / `include_bytes!` で実行ファイルに埋め込んでいる。
//! 実行ファイル1個だけを配れば動くので、コンテナイメージに静的ファイルを別途COPYする
//! 必要がない。そのぶん、CSSやJSだけを直した場合も再ビルドが要る。

use axum::Router;
use axum::http::header;
use axum::response::{Html, IntoResponse};
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

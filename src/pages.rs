//! 画面(HTML/CSS/JS)の配信。
//!
//! 3ファイルとも `include_str!` で実行ファイルに埋め込んでいる。実行ファイル1個だけを
//! 配れば動くので、コンテナイメージに静的ファイルを別途COPYする必要がない。
//! そのぶん、CSSやJSだけを直した場合も再ビルドが要る。

use axum::Router;
use axum::http::header;
use axum::response::{Html, IntoResponse};
use axum::routing::get;

use crate::api::AppState;

const INDEX_HTML: &str = include_str!("../static/index.html");
const STYLE_CSS: &str = include_str!("../static/css/style.css");
const APP_JS: &str = include_str!("../static/js/app.js");

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(index))
        .route("/static/css/style.css", get(style_css))
        .route("/static/js/app.js", get(app_js))
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

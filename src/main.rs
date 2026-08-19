//! エントリーポイント。設定を読み、DBを開き、ルーターを組み立てて待ち受ける。

mod ai;
mod api;
mod chiezo;
mod claude;
mod config;
mod db;
mod diag;
mod ingest;
mod pages;
mod pihole;
mod watch;

use std::net::{Ipv4Addr, SocketAddr};

use anyhow::{Context, Result};
use axum::Router;
use tracing_subscriber::EnvFilter;

use crate::ai::Ai;
use crate::api::AppState;
use crate::config::{Config, PORT};
use crate::db::Db;
use crate::pihole::PiholeClient;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    // reqwestのrustlsは暗号プロバイダを自前で選ばせる設定(rustls-no-provider)にしているので、
    // 最初のHTTPS接続より前にプロセス既定として登録しておく
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_| anyhow::anyhow!("rustlsの暗号プロバイダを登録できない"))?;

    let config = Config::from_env();
    tracing::info!(
        pihole_base_url = %config.pihole_base_url,
        query_limit = config.pihole_query_limit,
        db_path = %config.db_path.display(),
        // 空なら「聞く相手は同梱の Claude Code CLI 固定」の意味
        chiezo_base_url = %config.chiezo_base_url,
        "設定を読み込んだ"
    );

    let db = Db::open(&config.db_path)?;
    let state = AppState {
        ai: Ai::new(&config, db.clone())?,
        db,
        pihole: PiholeClient::new(&config)?,
        // 画面から Pi-hole の管理画面へ飛ばすための URL(開くのはブラウザ)
        pihole_web_url: config.pihole_web_url.clone(),
    };

    // DNSの取り込みは**別タスクで回し続ける**(画面の応答を待たせない)。
    // 失敗しても中で握って続けるので、ここでは投げっぱなしでよい
    tokio::spawn(ingest::run(
        state.db.clone(),
        state.pihole.clone(),
        config.clone(),
    ));

    let app = Router::new()
        .merge(pages::router())
        .merge(api::router())
        .with_state(state);

    let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, PORT));
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("{addr} を待ち受けられない"))?;
    tracing::info!("http://{addr} で待ち受け中");

    axum::serve(listener, app).await.context("サーバーが停止した")?;
    Ok(())
}

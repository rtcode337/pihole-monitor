//! エントリーポイント。設定を読み、DBを開き、ルーターを組み立てて待ち受ける。

mod api;
mod claude;
mod config;
mod db;
mod pages;
mod pihole;

use std::net::{Ipv4Addr, SocketAddr};

use anyhow::{Context, Result};
use axum::Router;
use tracing_subscriber::EnvFilter;

use crate::api::AppState;
use crate::claude::ClaudeClient;
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
        "設定を読み込んだ"
    );

    let state = AppState {
        db: Db::open(&config.db_path)?,
        pihole: PiholeClient::new(&config)?,
        claude: ClaudeClient::new(&config),
    };

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

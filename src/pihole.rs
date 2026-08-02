//! Pi-hole v6 REST APIとの連携。**参照のみで、Pi-holeの設定は変更しない。**
//!
//! リクエストごとに `POST /api/auth` でセッショントークン(sid)を取り、
//! それを使って `GET /api/queries` を1回叩く(1リクエストあたり計2コール)。

use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::config::Config;

#[derive(Clone)]
pub struct PiholeClient {
    http: reqwest::Client,
    base_url: String,
    password: String,
    query_limit: i64,
}

/// `POST /api/auth` のレスポンス。
#[derive(Deserialize)]
struct AuthResponse {
    session: Option<Session>,
}

#[derive(Deserialize)]
struct Session {
    sid: Option<String>,
}

/// `GET /api/queries` のレスポンス。使うのは domain だけなので他のフィールドは読み捨てる。
#[derive(Deserialize)]
struct QueriesResponse {
    #[serde(default)]
    queries: Vec<Query>,
}

#[derive(Deserialize)]
struct Query {
    #[serde(default)]
    domain: Option<String>,
}

impl PiholeClient {
    pub fn new(config: &Config) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .context("HTTPクライアントを作成できない")?;

        Ok(Self {
            http,
            base_url: config.pihole_base_url.clone(),
            password: config.pihole_password.clone(),
            query_limit: config.pihole_query_limit,
        })
    }

    async fn session_id(&self) -> Result<String> {
        let resp: AuthResponse = self
            .http
            .post(format!("{}/api/auth", self.base_url))
            .json(&serde_json::json!({ "password": self.password }))
            .send()
            .await
            .context("Pi-holeの認証エンドポイントに到達できない")?
            .json()
            .await
            .context("Pi-holeの認証レスポンスを解釈できない")?;

        resp.session
            .and_then(|s| s.sid)
            .context("Pi-holeがセッションIDを返さなかった(パスワードが違う可能性がある)")
    }

    /// ブロック済みクエリを取得し、ドメインごとの件数にまとめて返す。
    pub async fn blocked_domains(&self) -> Result<HashMap<String, u32>> {
        let sid = self.session_id().await?;

        let resp = self
            .http
            .get(format!("{}/api/queries", self.base_url))
            .query(&[
                ("upstream", "blocklist".to_string()),
                ("length", self.query_limit.to_string()),
            ])
            .header("sid", sid)
            .send()
            .await
            .context("Pi-holeのクエリ一覧を取得できない")?
            .error_for_status()
            .context("Pi-holeがエラーステータスを返した")?
            .json::<QueriesResponse>()
            .await
            .context("Pi-holeのクエリ一覧を解釈できない")?;

        let mut counts: HashMap<String, u32> = HashMap::new();
        for query in resp.queries {
            if let Some(domain) = query.domain.filter(|d| !d.is_empty()) {
                *counts.entry(domain).or_insert(0) += 1;
            }
        }
        Ok(counts)
    }
}

//! Pi-hole v6 REST APIとの連携。参照のみで、Pi-holeの設定は変更しない。
//!
//! リクエストごとに `POST /api/auth` でセッショントークン(sid)を取り、
//! それを使って `GET /api/queries` を1回叩く(1リクエストあたり計2コール)。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::config::Config;

#[derive(Clone)]
pub struct PiholeClient {
    http: reqwest::Client,
    base_url: String,
    password: String,
    /// 取ったセッション(sid)を使い回す。
    ///
    /// リクエストのたびに `POST /api/auth` してはいけない。 Pi-hole は同時セッション数に
    /// 上限があり(既定16。`webserver.api.max_sessions`)、超えると認証そのものが
    /// `api_seats_exceeded` で断られて取り込みが丸ごと止まる。遡り取り込みは1回で
    /// 30日ぶん叩くので、作り直していると即座に上限に当たる(実際に踏んだ)。
    session: Arc<Mutex<Option<String>>>,
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

/// `GET /api/queries` のレスポンス。ブロック集計では domain しか使わないが、
/// 取り込み(ingest)では時刻・クライアント・種別まで読む。
#[derive(Deserialize)]
struct QueriesResponse {
    #[serde(default)]
    queries: Vec<Query>,
}

#[derive(Deserialize)]
struct Query {
    #[serde(default)]
    id: Option<i64>,
    #[serde(default)]
    time: Option<f64>,
    #[serde(default)]
    domain: Option<String>,
    #[serde(default, rename = "type")]
    qtype: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    upstream: Option<String>,
    #[serde(default)]
    cname: Option<String>,
    #[serde(default)]
    reply: Option<Reply>,
    #[serde(default)]
    client: Option<Client>,
}

#[derive(Deserialize)]
struct Reply {
    #[serde(default)]
    r#type: Option<String>,
}

#[derive(Deserialize)]
struct Client {
    #[serde(default)]
    ip: Option<String>,
}

/// 取り込んで DB に入れるクエリ1件。Pi-hole の `id` をそのまま主キーにする ——
/// 取りこぼしを防ぐために窓を重ねて取るので、重複を弾く手立てが要る。
#[derive(Debug, Clone)]
pub struct QueryRecord {
    pub id: i64,
    pub ts: f64,
    pub domain: String,
    pub client: String,
    pub qtype: String,
    pub status: String,
    pub reply: Option<String>,
    pub upstream: Option<String>,
    pub cname: Option<String>,
}

/// ドメイン1件の期間集計(`GET /api/stats/database/top_domains`)。
///
/// 遡り取り込みはこちらを使う。 生のクエリを引くと30日で136万件になるが、
/// この口なら1日ぶんが約60KB・1リクエストで済む(実測: 1,306ドメイン)。
/// 1回しか出ていないドメインも省略されずに入る。
#[derive(Debug, Clone)]
pub struct DomainCount {
    pub domain: String,
    pub count: i64,
}

#[derive(Deserialize)]
struct TopDomainsResponse {
    #[serde(default)]
    domains: Vec<TopDomain>,
}

#[derive(Deserialize)]
struct TopDomain {
    domain: String,
    #[serde(default)]
    count: i64,
}

/// `GET /api/queries` が1回に返す上限。`length=-1` を渡しても超えられない
/// (実測: 46,939件ある日に -1 で 10,000件しか返らなかった)。
/// これを超えるぶんは `start` でページを送る。
const MAX_ROWS_PER_REQUEST: i64 = 10_000;

/// 1回の取り込みで送るページ数の上限。相手を叩き続けないための歯止めで、
/// 打ち切っても次の周回が続きから取る(カーソルは取れたところまでしか進めない)。
const MAX_PAGES_PER_RUN: usize = 40;

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
            session: Arc::new(Mutex::new(None)),
        })
    }

    /// 手元のセッション。無ければ取りに行って覚える。
    async fn sid(&self) -> Result<String> {
        if let Some(sid) = self.cached_sid() {
            return Ok(sid);
        }
        let sid = self.authenticate().await?;
        self.store_sid(Some(sid.clone()));
        Ok(sid)
    }

    fn cached_sid(&self) -> Option<String> {
        self.session
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn store_sid(&self, sid: Option<String>) {
        *self.session.lock().unwrap_or_else(|p| p.into_inner()) = sid;
    }

    /// GET を1本投げる。401 が返ったらセッションを取り直して1回だけやり直す ——
    /// Pi-hole 側の期限切れ(既定30分)や再起動で、覚えている sid は黙って無効になる。
    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        params: &[(&str, String)],
        what: &'static str,
    ) -> Result<T> {
        for attempt in 0..2 {
            let sid = self.sid().await?;
            let resp = self
                .http
                .get(format!("{}{}", self.base_url, path))
                .query(params)
                .header("sid", sid)
                .send()
                .await
                .with_context(|| format!("Pi-holeに到達できない({what})"))?;

            if resp.status() == reqwest::StatusCode::UNAUTHORIZED && attempt == 0 {
                self.store_sid(None);
                continue;
            }
            return resp
                .error_for_status()
                .with_context(|| format!("Pi-holeがエラーステータスを返した({what})"))?
                .json::<T>()
                .await
                .with_context(|| format!("Pi-holeの応答を解釈できない({what})"));
        }
        anyhow::bail!("Pi-holeの認証をやり直しても通らなかった({what})")
    }

    async fn authenticate(&self) -> Result<String> {
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
            .context("Pi-holeがセッションIDを返さなかった(パスワードが違うか、同時セッション数の上限に達している)")
    }

    /// ブロック済みクエリを取得し、ドメインごとの件数にまとめて返す。
    ///
    /// 件数(`limit`)は呼ぶ側から渡す。 画面から変えられる設定なので、
    /// 起動時に固定すると入れ替えても再起動するまで効かない(`-1` で全件)。
    pub async fn blocked_domains(&self, limit: i64) -> Result<HashMap<String, u32>> {
        let resp: QueriesResponse = self
            .get_json(
                "/api/queries",
                &[
                    ("upstream", "blocklist".to_string()),
                    ("length", limit.to_string()),
                ],
                "ブロック済みクエリ",
            )
            .await?;

        let mut counts: HashMap<String, u32> = HashMap::new();
        for query in resp.queries {
            if let Some(domain) = query.domain.filter(|d| !d.is_empty()) {
                *counts.entry(domain).or_insert(0) += 1;
            }
        }
        Ok(counts)
    }

    /// `since` 以降のクエリを新しい順に取り込む(ページを送って集める)。
    ///
    /// 窓は少し重ねて渡すこと(呼び出し側が `since` を巻き戻す)。Pi-hole 側の記録は
    /// 時刻順に確定するとは限らず、境界ぴったりで切ると取りこぼす。重複は `id` で弾く。
    pub async fn queries_since(&self, since: f64) -> Result<Vec<QueryRecord>> {
        let mut out: Vec<QueryRecord> = Vec::new();

        for page in 0..MAX_PAGES_PER_RUN {
            let start = page as i64 * MAX_ROWS_PER_REQUEST;
            let resp: QueriesResponse = self
                .get_json(
                    "/api/queries",
                    &[
                        ("from", format!("{:.0}", since.max(0.0))),
                        ("length", MAX_ROWS_PER_REQUEST.to_string()),
                        ("start", start.to_string()),
                    ],
                    "クエリ一覧",
                )
                .await?;

            let received = resp.queries.len() as i64;
            out.extend(resp.queries.into_iter().filter_map(record_of));

            // 上限に満たなければ最後のページ
            if received < MAX_ROWS_PER_REQUEST {
                return Ok(out);
            }
        }

        tracing::warn!(
            pages = MAX_PAGES_PER_RUN,
            got = out.len(),
            "1回の取り込みのページ上限に達した(残りは次の周回で取る)"
        );
        Ok(out)
    }

    /// 期間内のドメインごとの件数。遡り取り込み(`DomainCount` のコメント参照)で使う。
    ///
    /// 許可されたぶんとブロックされたぶんを両方引いて合わせる。 既定の応答は
    /// 許可されたドメインだけで、`blocked=true` を付けたときだけブロック済みが返る ——
    /// 片方だけだと、ブロックされているドメインが「遡りでは見なかった」ことになり、
    /// 生の取り込みが最初に拾った瞬間に「初出」として挙がってくる(実際に
    /// mask.icloud.com と api3.siftscience.com がそうなった)。
    pub async fn domain_counts(&self, from: i64, until: i64) -> Result<Vec<DomainCount>> {
        let mut out: Vec<DomainCount> = Vec::new();
        for blocked in ["false", "true"] {
            let resp: TopDomainsResponse = self
                .get_json(
                    "/api/stats/database/top_domains",
                    &[
                        ("from", from.to_string()),
                        ("until", until.to_string()),
                        // 件数で切らない。上位N件にすると、1回しか出ていないドメインが落ちる
                        // —— 初出の判定はまさにそこを見るので、切ると意味が無くなる
                        ("count", "1000000".to_string()),
                        ("blocked", blocked.to_string()),
                    ],
                    "ドメイン集計",
                )
                .await?;
            out.extend(
                resp.domains
                    .into_iter()
                    .filter(|d| !d.domain.is_empty())
                    .map(|d| DomainCount {
                        domain: d.domain,
                        count: d.count,
                    }),
            );
        }
        Ok(out)
    }
}

/// 応答の1件を取り込み用の形に直す。id・時刻・ドメインが欠けている行は捨てる ——
/// 主キーにも並べ替えにも使うので、無い行を入れても後段が扱えない。
fn record_of(q: Query) -> Option<QueryRecord> {
    let domain = q.domain.filter(|d| !d.is_empty())?;
    Some(QueryRecord {
        id: q.id?,
        ts: q.time?,
        domain,
        client: q
            .client
            .and_then(|c| c.ip)
            .unwrap_or_else(|| "unknown".to_string()),
        qtype: q.qtype.unwrap_or_else(|| "UNKNOWN".to_string()),
        status: q.status.unwrap_or_else(|| "UNKNOWN".to_string()),
        reply: q.reply.and_then(|r| r.r#type),
        upstream: q.upstream.filter(|u| !u.is_empty()),
        cname: q.cname.filter(|c| !c.is_empty()),
    })
}

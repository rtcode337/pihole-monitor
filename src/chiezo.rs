//! Chiezo(LAN 内の知識サーバー)の「素の問い合わせ」の口を叩く。
//!
//! **鍵を持たずに複数の AI を使えるようにするための経路。** Claude Code・Codex・
//! Antigravity …といった相手の認証情報は Chiezo が握っていて、こちらは
//! 「どの相手に投げるか」を指定するだけでよい —— 同梱の CLI は Claude Code 1 つだけなので、
//! **相手を選べる経路はここだけ**。
//!
//! **`/v1/chat` ではなく `/v1/ai/complete` を使う。** あちらは知識ベースを引いて答える口で、
//! 必ず抽出が混ざる —— こちらはプロンプトを自前で持っているので邪魔になる。

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::Config;

/// Chiezo に登録してある AI(相手)の1つ。画面の選択肢を組むのに必要なぶんだけを持つ。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Backend {
    /// 相手の識別子(`claude`・`codex` など)。**保存と突き合わせはこれで行う**
    /// (表示名は Chiezo 側で変わりうる)。
    pub id: String,
    /// 画面に出す名前。
    pub label: String,
    /// 選べるモデル。
    #[serde(default)]
    pub models: Vec<String>,
    /// 選べる考える量。**空なら画面に出さない**(その相手には無い)。
    #[serde(default)]
    pub efforts: Vec<String>,
    /// モデルの指定が必須か。false なら「相手の既定に任せる」を選べる。
    #[serde(default, rename = "model_required")]
    pub model_required: bool,
    /// web 検索を持っているか。**詳しく調べさせるときに要る** ——
    /// 持っていない相手に web を頼むと、実行してから断られる。
    #[serde(default)]
    pub web: bool,
}

/// Chiezo が1回の問い合わせで返したもの。
pub struct Completion {
    pub content: String,
    /// **実際に使われたモデル**。「相手の既定に任せる」で頼んだときに、何が書いたのかを
    /// 知る唯一の手がかり(こちらは名前を送っていないため)。
    pub model: Option<String>,
    /// 応答が名乗った相手の表示名。
    pub label: Option<String>,
}

#[derive(Clone)]
pub struct ChiezoClient {
    http: reqwest::Client,
    base_url: String,
}

#[derive(Deserialize)]
struct BackendsResponse {
    #[serde(default)]
    backends: Vec<Backend>,
}

#[derive(Deserialize)]
struct CompleteResponse {
    content: Option<String>,
    model: Option<String>,
    label: Option<String>,
}

impl ChiezoClient {
    /// URL が未設定なら `None`(画面は設定の仕方を出す)。
    pub fn new(config: &Config) -> anyhow::Result<Option<Self>> {
        if config.chiezo_base_url.is_empty() {
            return Ok(None);
        }

        Ok(Some(Self {
            // **こちらの待ちは相手より長くする。** 向こうにいるのは AI なので、
            // 生成そのものに時間がかかる(先に切れると理由が分からなくなる)
            http: reqwest::Client::builder()
                .timeout(config.chiezo_timeout + Duration::from_secs(30))
                .build()?,
            base_url: config.chiezo_base_url.clone(),
        }))
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// いま話せる相手の一覧。繋がらなければ理由を返す(画面がそのまま出す)。
    pub async fn backends(&self) -> Result<Vec<Backend>, String> {
        // 一覧を引くだけなので待ちは短くてよい(生成と違って相手は即答する)
        let url = format!("{}/v1/ai/backends", self.base_url);
        let response = self.send(self.http.get(&url).timeout(Duration::from_secs(15))).await?;
        let parsed: BackendsResponse = serde_json::from_str(&response)
            .map_err(|e| format!("Chiezo の応答が JSON として読めない: {e}"))?;

        Ok(parsed
            .backends
            .into_iter()
            .filter(|backend| !backend.id.trim().is_empty())
            .map(|mut backend| {
                if backend.label.trim().is_empty() {
                    backend.label = backend.id.clone();
                }
                backend
            })
            .collect())
    }

    /// 1 往復投げて本文を受け取る。`web` を立てると相手に web 検索を許す
    /// (持っていない相手に立てても Chiezo 側が無視する)。
    pub async fn complete(
        &self,
        backend: &str,
        model: Option<&str>,
        effort: Option<&str>,
        web: bool,
        timeout: Duration,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<Completion, String> {
        let url = format!("{}/v1/ai/complete", self.base_url);
        let body = self
            .send(self.http.post(&url).json(&json!({
                "backend": backend,
                // 空は送らない ——「相手の既定に任せる」の意味になる
                "model": model.filter(|v| !v.is_empty()),
                "effort": effort.filter(|v| !v.is_empty()),
                "web": web,
                "messages": [
                    {"role": "system", "content": system_prompt},
                    {"role": "user", "content": user_prompt},
                ],
            })).timeout(timeout))
            .await?;

        let parsed: CompleteResponse = serde_json::from_str(&body)
            .map_err(|e| format!("Chiezo の応答が JSON として読めない: {e}"))?;

        match parsed.content.map(|text| text.trim().to_string()) {
            Some(content) if !content.is_empty() => Ok(Completion {
                content,
                model: non_empty(parsed.model),
                label: non_empty(parsed.label),
            }),
            _ => Err("Chiezo が空の応答を返した".to_string()),
        }
    }

    /// 送って本文を取る。**届いたかどうかで理由を言い分ける** ——
    /// 「エラーを返した」なら届いていて相手の設定の話(`/v1` を付けた等)、
    /// それ以外は経路の話。
    ///
    /// **応答が無い理由は特定しない。** 相手が居ないだけの接続も、環境によっては
    /// 拒否されずに待たされる(この開発ホストでは閉じている localhost のポートへの接続が
    /// 15 秒待って切れた)ので、「ポート違い」と「パケットが落ちている」を
    /// タイムアウトで見分けることはできない。**両方を文面に書いて、確かめる先を示す**。
    /// なおコンテナからホストの公開ポートへ戻る経路は塞がれていることがある。
    async fn send(&self, request: reqwest::RequestBuilder) -> Result<String, String> {
        let response = request.send().await.map_err(|e| {
            if e.is_timeout() {
                format!(
                    "Chiezo({})が時間内に応答しなかった。URL とポート、                     経路(ファイアウォール)を確認する。",
                    self.base_url
                )
            } else {
                format!(
                    "Chiezo({})に繋がらない。URL と、Chiezo が動いているかを確認する。",
                    self.base_url
                )
            }
        })?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status.is_success() {
            return Ok(body);
        }

        // Chiezo は理由を JSON の本文に入れて返す(未設定の相手なら 404、
        // 「答える」層が無効なら 503)。そのまま画面へ出せるように載せる
        Err(format!(
            "Chiezo がエラーを返した(HTTP {}): {}",
            status.as_u16(),
            excerpt(&body)
        ))
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn excerpt(text: &str) -> String {
    let trimmed = text.trim();
    match trimmed.char_indices().nth(300) {
        Some((idx, _)) => format!("{}…", &trimmed[..idx]),
        None => trimmed.to_string(),
    }
}

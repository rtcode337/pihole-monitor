//! 「AIに聞く」の入口。**どの AI に聞くかを1か所で決め、プロンプトも1か所に置く。**
//!
//! 経路は2つあり、**選択は DB に持つので再起動なしで切り替わる**:
//!
//! | 経路 | 相手 | 選び方 |
//! |---|---|---|
//! | Chiezo(LAN 内の知識サーバー) | Chiezo に登録してある全部(Claude Code / Codex / …) | 画面で選ぶ |
//! | CLI ブリッジ(サイドカー) | Claude Code だけ | Chiezo 未設定か、相手を選んでいないとき |
//!
//! **指示文は経路で変えない。** 相手を切り替えたときに変わるのは書き手だけで、
//! 聞いていることが変わってしまうと読み比べにならない。

use serde::{Deserialize, Serialize};

use crate::chiezo::{Backend, ChiezoClient};
use crate::claude::ClaudeClient;
use crate::config::Config;
use crate::db::Db;

/// 選択の置き場(`settings` 表のキー)。値は [`AiChoice`] の JSON ——
/// 相手・モデル・考える量の3つ組なので、独自の区切り文字で組み立てると
/// モデル名に区切りが混ざったときに壊れる。
const SELECTION_KEY: &str = "ai:selection";

/// CLI ブリッジ経由のときに画面へ出す名前。
pub const BRIDGE_LABEL: &str = "Claude Code(CLIブリッジ)";

/// 何を聞くか。**ドメイン名以外はここに全部書く** —— 相手ごとに文言が散ると、
/// 切り替えたときに回答の違いが相手の差なのか指示の差なのか分からなくなる。
const SYSTEM_PROMPT: &str = "あなたはDNSとネットワークに詳しい技術者です。\
    Pi-holeの広告/トラッキングブロックリストによってブロックされたドメインについて、\
    それがどのようなサービス・通信に関連するドメインで、なぜブロックリストに\
    含まれている可能性が高いかを日本語で3〜5行程度で簡潔に説明してください。\
    前置き・復唱・箇条書きの記号は書かず、説明の文章だけを書いてください。";

/// 選んだ相手1つ。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiChoice {
    /// Chiezo 側の識別子(`claude` など)。**突き合わせはこれで行う**。
    pub backend: String,
    /// 画面に出す名前。**選んだ時点の表記を持つ** —— 表示のたびに Chiezo へ
    /// 問い合わせると、繋がらない日にボタンの名前が消える。
    pub label: String,
    /// モデル(未指定なら相手の既定)。
    #[serde(default)]
    pub model: Option<String>,
    /// 考える量(未指定なら相手の既定)。
    #[serde(default)]
    pub effort: Option<String>,
}

impl AiChoice {
    /// 画面に出す表記。モデルまで出す —— どの枠を使ったのかが分かるように。
    pub fn display_name(&self) -> String {
        name_with_model(&self.label, self.model.as_deref())
    }
}

/// 「相手 / モデル」の表記を組む。**モデル名が相手の名前と同じなら足さない** ——
/// Chiezo はモデルを選べない相手にも名前を1つ返すので(`Antigravity CLI` の
/// models が `["Antigravity CLI"]`)、そのまま繋ぐと同じ語が2回並ぶ。
fn name_with_model(label: &str, model: Option<&str>) -> String {
    match model {
        Some(model) if !model.is_empty() && !model.eq_ignore_ascii_case(label) => {
            format!("{label} / {model}")
        }
        _ => label.to_string(),
    }
}

/// 回答1件。
pub struct Answer {
    pub text: String,
    /// **誰が書いたか。** 相手を切り替えられる以上、これが無いと読み比べにならない。
    /// Chiezo 経由では**応答が名乗ったモデル**を使う(「既定に任せる」で頼んだときに
    /// 何が書いたのかを知る唯一の手がかり)。
    pub author: String,
}

/// 「AIに聞く」の失敗理由。
pub enum AskError {
    /// トークンが未保存、または認証エラーだった。フロントにトークン入力を促させる
    /// (**CLI ブリッジ経由のときだけ起きる** —— Chiezo は鍵を自分で持っている)
    TokenRequired,
    /// それ以外の失敗。文字列はそのまま画面に出す
    Failed(String),
}

#[derive(Clone)]
pub struct Ai {
    db: Db,
    /// URL 未設定なら `None`(画面が設定の仕方を出す)。
    chiezo: Option<ChiezoClient>,
    claude: ClaudeClient,
}

impl Ai {
    pub fn new(config: &Config, db: Db) -> anyhow::Result<Self> {
        Ok(Self {
            db,
            chiezo: ChiezoClient::new(config)?,
            claude: ClaudeClient::new(config)?,
        })
    }

    /// Chiezo の URL(未設定なら空文字)。画面が「選べるかどうか」を出すのに使う。
    pub fn chiezo_url(&self) -> &str {
        self.chiezo.as_ref().map_or("", ChiezoClient::base_url)
    }

    /// いま話せる相手の一覧。Chiezo 未設定なら空、繋がらなければ理由を返す。
    pub async fn backends(&self) -> Result<Vec<Backend>, String> {
        match &self.chiezo {
            Some(chiezo) => chiezo.backends().await,
            None => Ok(Vec::new()),
        }
    }

    /// 保存済みの選択。**読めない値は「未選択」として扱う**(画面から選び直せる)。
    pub async fn selection(&self) -> Option<AiChoice> {
        let raw = match self.db.setting(SELECTION_KEY).await {
            Ok(raw) => raw?,
            Err(e) => {
                tracing::warn!(error = ?e, "AIの選択を読み出せない");
                return None;
            }
        };

        match serde_json::from_str::<AiChoice>(&raw) {
            Ok(choice) if !choice.backend.trim().is_empty() => Some(choice),
            _ => {
                tracing::warn!(value = %raw, "AIの選択が読めないので未選択として扱う");
                None
            }
        }
    }

    /// 選択を保存する。`None` なら CLI ブリッジ経由に戻す。
    pub async fn select(&self, choice: Option<&AiChoice>) -> anyhow::Result<()> {
        let value = match choice {
            Some(choice) => Some(serde_json::to_string(choice)?),
            None => None,
        };
        self.db.set_setting(SELECTION_KEY, value).await
    }

    /// いま聞く相手の名前。**選択が残っていても Chiezo が未設定なら
    /// ブリッジの名前を出す** —— 実際に答える相手と食い違わせない。
    pub async fn current_name(&self) -> String {
        match self.selection().await {
            Some(choice) if self.chiezo.is_some() => choice.display_name(),
            _ => BRIDGE_LABEL.to_string(),
        }
    }

    /// 指定ドメインについて、選ばれている相手に説明を求める。
    pub async fn ask_about_domain(&self, domain: &str) -> Result<Answer, AskError> {
        let user_prompt = format!("ドメイン: {domain}");

        // 選択が残っているのに Chiezo が未設定(URL を外した)なら、黙ってブリッジへ倒す ——
        // 答えが出ないより、従来の経路で答えが出るほうがよい
        let Some((chiezo, choice)) = self.chiezo.as_ref().zip(self.selection().await) else {
            return self
                .claude
                .ask(SYSTEM_PROMPT, &user_prompt)
                .await
                .map(|text| Answer {
                    text,
                    author: BRIDGE_LABEL.to_string(),
                });
        };

        let completion = chiezo
            .complete(
                &choice.backend,
                choice.model.as_deref(),
                choice.effort.as_deref(),
                SYSTEM_PROMPT,
                &user_prompt,
            )
            .await
            .map_err(AskError::Failed)?;

        // 名前は**応答が名乗ったもの**を優先する。こちらがモデルを指定していないとき、
        // 何が書いたのかを知る手がかりはそれだけ
        let label = completion.label.unwrap_or_else(|| choice.label.clone());
        let model = completion.model.or_else(|| choice.model.clone());

        Ok(Answer {
            text: completion.content,
            author: name_with_model(&label, model.as_deref()),
        })
    }

    /// `claude setup-token` のトークンを保存する(CLI ブリッジ経由のときだけ要る)。
    pub fn save_token(&self, token: &str) -> anyhow::Result<()> {
        self.claude.save_token(token)
    }
}

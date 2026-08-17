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
///
/// 出力は**一覧に並ぶメモ**なので1〜2文に抑えさせる(3〜5行あると読めない)。
/// **番号で対応を返させる**(ドメイン名を書き写させると綴りが揺れて突き合わせできない)。
const BULK_SYSTEM_PROMPT: &str = "あなたはDNSとネットワークに詳しい技術者です。\
    Pi-holeの広告/トラッキングブロックリストによってブロックされた複数のドメインについて、\
    それぞれが何のサービスに関連し、なぜブロックされていそうかを日本語で1〜2文に\
    まとめてください。\
    出力は次の形のJSONだけにし、説明・前置き・コードフェンスは書かないでください。\
    {\"results\":[{\"n\":1,\"note\":\"…\"}]}\
    n は入力の番号です。番号は必ず入力どおりに対応させ、ドメイン名は書かないでください。\
    分からないドメインも推測せず、その旨を1文で書いてください。";

/// 1回のまとめて質問に渡すドメインの上限。**多すぎると応答が崩れる** ——
/// tech-antenna では200件を1回に詰めて300秒のタイムアウトを超え、まとめて失敗した。
/// 画面はこの数ずつに区切って何度も呼ぶので、進捗が出るし途中まででも残る。
pub const MAX_BULK_DOMAINS: usize = 10;

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

/// まとめて聞いた結果。
pub struct BulkAnswer {
    /// ドメイン → メモ。**答えが返らなかったドメインは入らない**
    /// (画面が「聞けなかった件数」を出せるようにするため)。
    pub notes: Vec<(String, String)>,
    /// **誰が書いたか。** 相手を切り替えられる以上、これが無いと結果を読み分けられない。
    /// **応答が名乗ったモデル**を優先する(「相手の既定に任せる」で頼んだときに、
    /// 何が書いたのかを知る手がかりはそれだけ)。
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

    /// 選ばれている相手に1往復投げて、本文と書き手の名前を受け取る。
    /// **経路(Chiezo / CLI ブリッジ)の分岐はここだけ。**
    async fn complete(
        &self,
        system_prompt: &str,
        user_prompt: &str,
    ) -> Result<(String, String), AskError> {
        // 選択が残っているのに Chiezo が未設定(URL を外した)なら、黙ってブリッジへ倒す ——
        // 答えが出ないより、従来の経路で答えが出るほうがよい
        let Some((chiezo, choice)) = self.chiezo.as_ref().zip(self.selection().await) else {
            let text = self.claude.ask(system_prompt, user_prompt).await?;
            return Ok((text, BRIDGE_LABEL.to_string()));
        };

        let completion = chiezo
            .complete(
                &choice.backend,
                choice.model.as_deref(),
                choice.effort.as_deref(),
                system_prompt,
                user_prompt,
            )
            .await
            .map_err(AskError::Failed)?;

        // 名前は**応答が名乗ったもの**を優先する。こちらがモデルを指定していないとき、
        // 何が書いたのかを知る手がかりはそれだけ
        let label = completion.label.unwrap_or_else(|| choice.label.clone());
        let model = completion.model.or_else(|| choice.model.clone());

        Ok((
            completion.content,
            name_with_model(&label, model.as_deref()),
        ))
    }

    /// 複数のドメインについて**1回の問い合わせで**まとめて聞く。
    ///
    /// **1件ずつ聞かない。** 相手が CLI だと呼び出し1回の固定費(ハーネスの入力)が大きく、
    /// 47件を1件ずつ聞くと固定費を47回払うことになる。まとめて渡すこと自体が対策。
    pub async fn ask_about_domains(&self, domains: &[String]) -> Result<BulkAnswer, AskError> {
        if domains.is_empty() {
            return Err(AskError::Failed("聞く対象がありません".to_string()));
        }

        // 番号付きで渡し、番号で返させる(ドメイン名を書き写させない)
        let user_prompt = domains
            .iter()
            .enumerate()
            .map(|(i, domain)| format!("{}. {domain}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");

        let (text, author) = self.complete(BULK_SYSTEM_PROMPT, &user_prompt).await?;
        let notes = parse_bulk(&text, domains)
            .map_err(|e| AskError::Failed(format!("{e}(相手: {author})")))?;

        Ok(BulkAnswer { notes, author })
    }

    /// `claude setup-token` のトークンを保存する(CLI ブリッジ経由のときだけ要る)。
    pub fn save_token(&self, token: &str) -> anyhow::Result<()> {
        self.claude.save_token(token)
    }
}

/// まとめて聞いた応答を読む。**番号 → ドメイン**に付け直して返す。
fn parse_bulk(text: &str, domains: &[String]) -> Result<Vec<(String, String)>, String> {
    #[derive(Deserialize)]
    struct BulkResponse {
        #[serde(default)]
        results: Vec<BulkItem>,
    }

    #[derive(Deserialize)]
    struct BulkItem {
        n: usize,
        #[serde(default)]
        note: String,
    }

    let json = extract_json(text).ok_or("応答にJSONが入っていない")?;
    let parsed: BulkResponse =
        serde_json::from_str(json).map_err(|e| format!("応答をJSONとして読めない: {e}"))?;

    let notes = parsed
        .results
        .into_iter()
        .filter_map(|item| {
            // **範囲外の番号は捨てる**(LLM の応答をそのまま信じない)。
            // 空のメモも捨てる —— 一覧に空行が並ぶだけ
            let domain = domains.get(item.n.checked_sub(1)?)?;
            let note = item.note.trim();
            (!note.is_empty()).then(|| (domain.clone(), note.to_string()))
        })
        .collect::<Vec<_>>();

    if notes.is_empty() {
        return Err("応答にメモが1件も入っていない".to_string());
    }
    Ok(notes)
}

/// 応答から JSON の本体を切り出す。**前置きとコードフェンスを許して読む** ——
/// 「JSONだけ」と指示しても説明を1行添えてくる応答はあり、そこで丸ごと捨てると
/// そのぶんのメモが消える。
fn extract_json(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (start < end).then(|| text[start..=end].trim())
}

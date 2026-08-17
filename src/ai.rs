//! 「AIに聞く」の入口。**誰に聞くかを1か所で決め、プロンプトも1か所に置く。**
//!
//! **相手は何人でも選べる**(`ai:selections`)。選んだ全員に同じ材料で聞き、
//! **答えを「誰が書いたか」付きで1つのメモに並べる** —— 同じドメインでも相手によって
//! 見立てが違うので、読み比べられるようにしてある。
//!
//! 相手には2種類ある。**選択は DB に持つので再起動なしで切り替わる**:
//!
//! | 相手 | 誰か | 認証 |
//! |---|---|---|
//! | Chiezo(LAN 内の知識サーバー) | Chiezo に登録してある全部(Claude Code / Codex / …) | 要らない(鍵はあちら) |
//! | CLI ブリッジ(サイドカー) | Claude Code だけ | `claude setup-token` のトークン |
//!
//! **指示文は相手で変えない。** 相手を変えたときに変わるのは書き手だけで、
//! 聞いていることが変わってしまうと読み比べにならない。

use serde::{Deserialize, Serialize};

use crate::chiezo::{Backend, ChiezoClient};
use crate::claude::ClaudeClient;
use crate::config::Config;
use crate::db::Db;

/// 選択の置き場(`settings` 表のキー)。値は [`AiChoice`] の**配列**の JSON ——
/// 相手・モデル・考える量の3つ組が何組も入るので、独自の区切り文字で組み立てると
/// モデル名に区切りが混ざったときに壊れる。
const SELECTIONS_KEY: &str = "ai:selections";

/// 単一選択だった頃の置き場。**読むだけ**(見つけたら配列側へ移して消す) ——
/// 移さないと、更新した環境で選び直しを求めることになる。
const LEGACY_SELECTION_KEY: &str = "ai:selection";

/// CLI ブリッジを指す予約された識別子。**`local:` を冠する** ——
/// Chiezo の相手の id(`claude`・`codex` 等)にコロンは使われないので、混ざらない。
pub const BRIDGE_BACKEND: &str = "local:bridge";

/// CLI ブリッジ経由のときに画面へ出す名前。
pub const BRIDGE_LABEL: &str = "Claude Code(CLIブリッジ)";

/// 何を聞くか。**ドメイン名以外はここに全部書く** —— 相手ごとに文言が散ると、
/// 切り替えたときに回答の違いが相手の差なのか指示の差なのか分からなくなる。
///
/// **1件でもまとめてでも同じ指示・同じ形式**(番号付きの入力とJSONの応答)。行のボタンで
/// 1件だけ聞くときも答えはそのままメモになるので、出力の形を変える理由が無い ——
/// **文言を2つ持つと、1件のときとまとめたときでメモの書き方が変わる**。
///
/// 出力は**一覧に並ぶメモ**なので1〜2文に抑えさせる(3〜5行あると読めない。
/// **相手が複数なら人数ぶん並ぶ**ので、短さの意味はさらに大きい)。
/// **番号で対応を返させる**(ドメイン名を書き写させると綴りが揺れて突き合わせできない)。
const SYSTEM_PROMPT: &str = "あなたはDNSとネットワークに詳しい技術者です。\
    Pi-holeの広告/トラッキングブロックリストによってブロックされたドメインについて、\
    それぞれが何のサービスに関連し、なぜブロックされていそうかを日本語で1〜2文に\
    まとめてください。\
    出力は次の形のJSONだけにし、説明・前置き・コードフェンスは書かないでください。\
    {\"results\":[{\"n\":1,\"note\":\"…\"}]}\
    n は入力の番号です。番号は必ず入力どおりに対応させ、ドメイン名は書かないでください。\
    分からないドメインも推測せず、その旨を1文で書いてください。";

/// 1回の問い合わせに渡せるドメインの上限。**多すぎると応答が崩れる** ——
/// tech-antenna では200件を1回に詰めて300秒のタイムアウトを超え、まとめて失敗した。
/// 「まとめて聞く」はこの数ずつに区切って何度も呼ぶので、進捗が出るし途中まででも残る。
pub const MAX_DOMAINS_PER_ASK: usize = 10;

/// 選んだ相手1人。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiChoice {
    /// Chiezo 側の識別子(`claude` など)、または [`BRIDGE_BACKEND`]。
    /// **突き合わせはこれで行う**。
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
    /// CLI ブリッジ(このアプリが直に話す相手)か。
    pub fn is_bridge(&self) -> bool {
        self.backend == BRIDGE_BACKEND
    }

    /// CLI ブリッジを指す選択。
    pub fn bridge() -> Self {
        Self {
            backend: BRIDGE_BACKEND.to_string(),
            label: BRIDGE_LABEL.to_string(),
            model: None,
            effort: None,
        }
    }

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

/// 聞いた結果(1件でもまとめてでも、相手が1人でも複数でも同じ形)。
pub struct Answer {
    /// ドメイン → メモ。**答えが返らなかったドメインは入らない**
    /// (画面が「聞けなかった件数」を出せるようにするため)。
    /// 相手が複数なら、1つのメモに全員ぶんが「誰が書いたか」付きで並ぶ。
    pub notes: Vec<(String, String)>,
    /// 実際に書いた相手の名前。
    pub authors: Vec<String>,
    /// 答えられなかった相手と理由。**1人落ちても残りは使う**ので、
    /// 失敗そのものは値で返して画面に出させる。
    pub failures: Vec<String>,
}

/// 「AIに聞く」の失敗理由。
pub enum AskError {
    /// トークンが未保存、または認証エラーだった。フロントにトークン入力を促させる
    /// (**CLI ブリッジが相手のときだけ起きる** —— Chiezo は鍵を自分で持っている)
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
    /// **Chiezo が未設定なら Chiezo の相手は落とす** —— 実際に聞ける相手と
    /// 画面に出る相手を食い違わせない。
    pub async fn selections(&self) -> Vec<AiChoice> {
        let raw = match self.db.setting(SELECTIONS_KEY).await {
            Ok(Some(raw)) => Some(raw),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(error = ?e, "AIの選択を読み出せない");
                None
            }
        };

        let choices: Vec<AiChoice> = match raw {
            Some(raw) => serde_json::from_str(&raw).unwrap_or_else(|_| {
                tracing::warn!(value = %raw, "AIの選択が読めないので未選択として扱う");
                Vec::new()
            }),
            // 単一選択だった頃の値を拾う(次の保存で配列側へ移る)
            None => self.legacy_selection().await.into_iter().collect(),
        };

        choices
            .into_iter()
            .filter(|choice| !choice.backend.trim().is_empty())
            .filter(|choice| choice.is_bridge() || self.chiezo.is_some())
            .collect()
    }

    async fn legacy_selection(&self) -> Option<AiChoice> {
        let raw = self.db.setting(LEGACY_SELECTION_KEY).await.ok()??;
        serde_json::from_str::<AiChoice>(&raw).ok()
    }

    /// 実際に聞く相手。**空なら CLI ブリッジ** —— 何も選んでいない状態でも
    /// 従来どおり動く(Chiezo を使わない環境がそれ)。
    async fn targets(&self) -> Vec<AiChoice> {
        let selections = self.selections().await;
        if selections.is_empty() {
            vec![AiChoice::bridge()]
        } else {
            selections
        }
    }

    /// 選択を保存する。空なら CLI ブリッジ経由に戻す。
    pub async fn select(&self, choices: &[AiChoice]) -> anyhow::Result<()> {
        let value = if choices.is_empty() {
            None
        } else {
            Some(serde_json::to_string(choices)?)
        };
        self.db.set_setting(SELECTIONS_KEY, value).await?;
        // 単一選択だった頃の値が残っていると、配列を消したときに復活してしまう
        self.db.set_setting(LEGACY_SELECTION_KEY, None).await
    }

    /// いま聞く相手の名前(空にはならない)。画面のボタンと案内に出す。
    pub async fn current_names(&self) -> Vec<String> {
        self.targets()
            .await
            .iter()
            .map(AiChoice::display_name)
            .collect()
    }

    /// ドメインについて**選んだ相手全員に**聞く(1件でも複数でも同じ経路)。
    ///
    /// **1件ずつ聞かない。** 相手がCLIだと呼び出し1回の固定費(ハーネスの入力)が大きく、
    /// 47件を1件ずつ聞くと固定費を47回払う。**まとめて渡すこと自体が対策**。
    ///
    /// **相手には同時に投げる。** 順に聞くと待ち時間が人数ぶん積み上がる。
    pub async fn ask_about_domains(&self, domains: &[String]) -> Result<Answer, AskError> {
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

        let targets = self.targets().await;
        let tasks: Vec<_> = targets
            .iter()
            .cloned()
            .map(|target| {
                let ai = self.clone();
                let prompt = user_prompt.clone();
                let domains = domains.to_vec();
                tokio::spawn(async move { ai.ask_target(&target, &prompt, &domains).await })
            })
            .collect();

        // **選んだ順に受け取る。** メモに並ぶ順が呼ぶたびに変わると読み比べにくい
        let mut answered: Vec<(String, Vec<(String, String)>)> = Vec::new();
        let mut authors = Vec::new();
        let mut failures = Vec::new();
        let mut token_required = false;

        for (target, task) in targets.iter().zip(tasks) {
            let name = target.display_name();
            match task.await {
                Ok(Ok((author, notes))) => {
                    authors.push(author.clone());
                    answered.push((author, notes));
                }
                Ok(Err(AskError::TokenRequired)) => {
                    token_required = true;
                    failures.push(format!("{name}: トークンが未登録です"));
                }
                Ok(Err(AskError::Failed(message))) => failures.push(format!("{name}: {message}")),
                // タスク自体が落ちた(panic 等)。相手のせいではないので理由をそのまま出す
                Err(e) => failures.push(format!("{name}: 実行に失敗しました({e})")),
            }
        }

        if answered.is_empty() {
            // **誰も答えられず、理由がトークンだけならトークンを求める** ——
            // 画面はそれを見て相手を選ぶモーダル(設定の置き場)を開く
            return Err(if token_required && failures.len() == 1 {
                AskError::TokenRequired
            } else {
                AskError::Failed(failures.join(" / "))
            });
        }

        Ok(Answer {
            notes: compose_notes(domains, &answered),
            authors,
            failures,
        })
    }

    /// 相手1人に聞く。返すのは (書き手の名前, ドメインごとのメモ)。
    async fn ask_target(
        &self,
        target: &AiChoice,
        user_prompt: &str,
        domains: &[String],
    ) -> Result<(String, Vec<(String, String)>), AskError> {
        let (text, author) = if target.is_bridge() {
            let text = self.claude.ask(SYSTEM_PROMPT, user_prompt).await?;
            (text, BRIDGE_LABEL.to_string())
        } else {
            // **Chiezo が未設定なら選択から落としてある**ので、ここに来るのは設定済みのとき
            let Some(chiezo) = self.chiezo.as_ref() else {
                return Err(AskError::Failed("Chiezo の URL が未設定です".to_string()));
            };
            let completion = chiezo
                .complete(
                    &target.backend,
                    target.model.as_deref(),
                    target.effort.as_deref(),
                    SYSTEM_PROMPT,
                    user_prompt,
                )
                .await
                .map_err(AskError::Failed)?;

            // 名前は**応答が名乗ったもの**を優先する。こちらがモデルを指定していないとき、
            // 何が書いたのかを知る手がかりはそれだけ
            let label = completion.label.unwrap_or_else(|| target.label.clone());
            let model = completion.model.or_else(|| target.model.clone());
            (completion.content, name_with_model(&label, model.as_deref()))
        };

        let notes = parse_notes(&text, domains)
            .map_err(|e| AskError::Failed(format!("{e}(相手: {author})")))?;
        Ok((author, notes))
    }

    /// CLI ブリッジ用のトークンが保存されているか。**値は返さない** ——
    /// 画面が出すのは「登録済みか」だけでよい。
    pub fn has_token(&self) -> bool {
        self.claude.load_token().is_some()
    }

    /// `claude setup-token` のトークンを保存する(CLI ブリッジが相手のときだけ要る)。
    pub fn save_token(&self, token: &str) -> anyhow::Result<()> {
        self.claude.save_token(token)
    }
}

/// 相手ごとの答えを、ドメインごとに1つのメモへまとめる。
///
/// **誰が書いたかを頭に付ける**(`[Codex CLI] …`)。相手が複数いると、どれがどの見立てか
/// 分からないメモは読み比べに使えない。**相手が1人でも付ける** —— 後で見返したときに
/// 何が書いたのか分かるし、人数で書式が変わると差分が読みにくい。
fn compose_notes(
    domains: &[String],
    answered: &[(String, Vec<(String, String)>)],
) -> Vec<(String, String)> {
    domains
        .iter()
        .filter_map(|domain| {
            let parts: Vec<String> = answered
                .iter()
                .filter_map(|(author, notes)| {
                    notes
                        .iter()
                        .find(|(d, _)| d == domain)
                        .map(|(_, note)| format!("[{author}] {note}"))
                })
                .collect();
            // 1人も答えなかったドメインは入れない(空のメモで上書きしないため)
            (!parts.is_empty()).then(|| (domain.clone(), parts.join("\n\n")))
        })
        .collect()
}

/// 応答を読む。**番号 → ドメイン**に付け直して返す。
fn parse_notes(text: &str, domains: &[String]) -> Result<Vec<(String, String)>, String> {
    #[derive(Deserialize)]
    struct NotesResponse {
        #[serde(default)]
        results: Vec<NoteItem>,
    }

    #[derive(Deserialize)]
    struct NoteItem {
        n: usize,
        #[serde(default)]
        note: String,
    }

    let json = extract_json(text).ok_or("応答にJSONが入っていない")?;
    let parsed: NotesResponse =
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

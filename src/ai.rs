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

use std::collections::HashMap;

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

/// どちらの一覧について聞いているか。**指示文はこれで変える。**
///
/// **相手(経路)では変えない**が、材料が違えば聞くことも違う ——
/// ブロック済みの一覧は「Pi-hole が止めたもの」なので「なぜ止まったか」を聞けばよいが、
/// 監視の一覧は**ブロックの結果ではなく、通信の振る舞いから選んだ候補**で、
/// 同じ文言で聞くと**「ブロックされたと考えられます」という嘘のメモが並ぶ**
/// (実際にそうなっていた)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskMode {
    /// Pi-hole がブロックリストで止めたドメイン(`/api/domains` の一覧)
    Blocked,
    /// 素通りしている通信から、振る舞いで拾った候補(`/api/watch` の一覧)
    Watch,
}

impl AskMode {
    /// 画面から来た文字列を読む。**知らない値はブロック済み扱い** ——
    /// 既定の一覧がそちらで、指定が無い呼び出し(古い画面・手で叩いたcurl)もそこに倒れる。
    pub fn parse(raw: &str) -> Self {
        match raw.trim() {
            "watch" => Self::Watch,
            _ => Self::Blocked,
        }
    }

    fn system_prompt(self) -> &'static str {
        match self {
            Self::Blocked => SYSTEM_PROMPT_BLOCKED,
            Self::Watch => SYSTEM_PROMPT_WATCH,
        }
    }

    /// 「詳しく調べる」で、材料の出どころを1文で伝える文。
    /// **これが無いと、素通りしている通信まで「ブロックされた」前提で説明される。**
    fn origin(self) -> &'static str {
        match self {
            Self::Blocked => "このドメインは Pi-hole のブロックリスト(広告/トラッキング)に載っていて、実際にブロックされています。",
            Self::Watch => "このドメインはブロックリストで止まったものではありません。\
                Pi-hole が記録したDNSの問い合わせの中から、振る舞い(はじめて見た・NXDOMAINが多い・\
                珍しいクエリ種別・規則正しい間隔・毎回ちがう長い名前)で自動的に拾った候補です。\
                ブロックされたかどうかは判断材料に含まれていないので、そう決めつけないでください。",
        }
    }
}

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
const SYSTEM_PROMPT_BLOCKED: &str = "あなたはDNSとネットワークに詳しい技術者です。\
    Pi-holeの広告/トラッキングブロックリストによってブロックされたドメインについて、\
    それぞれが何のサービスに関連し、なぜブロックされていそうかを日本語で1〜2文に\
    まとめてください。\
    出力は次の形のJSONだけにし、説明・前置き・コードフェンスは書かないでください。\
    {\"results\":[{\"n\":1,\"note\":\"…\"}]}\
    n は入力の番号です。番号は必ず入力どおりに対応させ、ドメイン名は書かないでください。\
    分からないドメインも推測せず、その旨を1文で書いてください。";

/// 監視の一覧([`AskMode::Watch`])用。**ブロックを前提にした書き方を禁じるのが要点。**
///
/// こちらの一覧はブロックの結果ではなく、通信の振る舞いから拾った候補なので、
/// 知りたいのは「なぜ止まったか」ではなく**「何の通信で、それは普通に起きることか」**。
/// **候補に挙げた理由(観測した事実)も一緒に渡す** —— 同じドメインでも、
/// 「はじめて見た」のと「10分おきに鳴っている」のとでは書くべきことが違う。
const SYSTEM_PROMPT_WATCH: &str = "あなたはDNSとネットワークに詳しい技術者です。\
    家庭内のPi-holeが記録したDNSの問い合わせから、「いつもと違う振る舞い」として\
    自動で候補に挙げたドメインを見てもらいます。\
    これらはブロックリストで止まったものではありません。\
    「ブロックされた」「ブロックされて当然」のような、ブロックされたことを前提にした\
    書き方はしないでください。\
    それぞれについて、何のサービス・製品が使うドメインで、その通信は普通に起きるものか\
    (あるいは気に留めた方がよいか)を日本語で1〜2文にまとめてください。\
    各行の括弧の中は、こちらが候補に挙げた理由(観測した事実)です。判断の材料にしてください。\
    出力は次の形のJSONだけにし、説明・前置き・コードフェンスは書かないでください。\
    {\"results\":[{\"n\":1,\"note\":\"…\"}]}\
    n は入力の番号です。番号は必ず入力どおりに対応させ、ドメイン名は書かないでください。\
    分からないドメインも推測せず、その旨を1文で書いてください。";

/// 「詳しく調べる」の指示文。**メモを書かせる `SYSTEM_PROMPT` とは役割が違う。**
///
/// あちらは一覧に並ぶ1〜2文で、何十件もまとめて書かせるためのもの。こちらは
/// **1件を深く調べる**ためのもので、web 検索と、こちらが渡す観測データの両方を使わせる。
/// だから長さの制限も緩く、出力は JSON ではなく素の文章にしてある ——
/// **1件ぶんの答えを1つ返すだけなので、番号で対応させる必要が無い**
/// (JSON にすると、長い文章を JSON 文字列へ押し込む過程で崩れる余地が増える)。
///
/// **観測データを渡すのが要点。** 「そのドメインが何か」は web でも分かるが、
/// 「この家のネットワークでどう振る舞っているか」はこちらしか知らない。
/// 両方を突き合わせて初めて「放っておいてよいか」が言える。
const INVESTIGATE_PROMPT: &str = "あなたはDNSとネットワークセキュリティに詳しい技術者です。    家庭内のPi-holeが観測したドメインについて、利用者が「放置してよいか、止めるべきか」を    判断できるように日本語で調べてください。    web検索が使えるなら、そのドメインの運営元・用途・既知の評判を調べてください。    あわせて、利用者のネットワークでの観測データ(このあと渡します)を必ず踏まえてください。    次の見出しで、それぞれ1〜3文で簡潔に書いてください。
    ■ 何のドメインか(運営元と用途)
    ■ どの製品・アプリが使うか(観測データの端末と矛盾しないか)
    ■ 通信の中身と頻度から言えること
    ■ 放置してよいか(止める場合の影響も)
    分からないことは推測せず「不明」と書いてください。    見出し以外の前置き・結び・コードフェンスは書かないでください。";

/// 「調査結果をもとに、もう一歩聞く」ときの指示文。
///
/// **`INVESTIGATE_PROMPT` とは出力の形が違う。** あちらは何も知らない状態から
/// 決まった見出しで一通り書かせるもの。こちらは**すでに書いた内容を材料に、
/// 利用者の1つの問いへ答えるだけ**なので、見出しを付けさせると同じことがもう一度並ぶ。
///
/// **これまでのやり取りを丸ごと渡す**(`research` はそこまでの調査と質問の積み重ね)。
/// 渡さないと、2つ目の質問が1つ目の答えを知らないまま返ってくる。
const FOLLOWUP_PROMPT: &str = "あなたはDNSとネットワークセキュリティに詳しい技術者です。    家庭内のPi-holeが観測したドメインについて、すでに調べた結果(このあと渡します)を踏まえ、    利用者からの追加の質問に日本語で答えてください。    web検索が使えるなら、必要に応じて調べ直してください。    観測データも渡すので、一般論だけでなくこのネットワークで実際に起きていることを踏まえてください。    答えは3〜5文程度で簡潔に。見出し・前置き・結び・コードフェンスは書かないでください。    すでに書いたことをなぞらず、質問に直接答えてください。    分からないことは推測せず「不明」と書いてください。";

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
    /// **メインの相手か。** 「詳しく調べる」はこの1人だけに頼む ——
    /// web 検索を伴って時間も枠も使うので、選んだ全員に投げる話ではない。
    /// **`default` にしてある**ので、この列より前に保存した選択もそのまま読める
    /// (誰も立っていなければ先頭をメインとして扱う)。
    #[serde(default)]
    pub primary: bool,
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
            primary: true,
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
    /// 通常の問い合わせ(メモを書かせる)の上限。
    chiezo_timeout: std::time::Duration,
    /// 「詳しく調べる」の上限。**web 検索を伴うのでずっと長い**。
    investigate_timeout: std::time::Duration,
}

impl Ai {
    pub fn new(config: &Config, db: Db) -> anyhow::Result<Self> {
        Ok(Self {
            db,
            chiezo: ChiezoClient::new(config)?,
            claude: ClaudeClient::new(config)?,
            chiezo_timeout: config.chiezo_timeout,
            investigate_timeout: config.investigate_timeout,
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
    ///
    /// **メインを先頭にそろえる。** この並びがそのまま2か所に出る ——
    /// ツールバーのボタンに出す名前(先頭を出す)と、まとめて聞いたときにメモへ並ぶ順
    /// (`compose_notes` はこの順で書く)。**普段いちばん信用している相手が
    /// 先頭に来ていないと、ボタンの名前もメモの1行目も当てにならない**。
    /// 残りは保存した順のまま(呼ぶたびに並びが変わると読み比べにくい)。
    async fn targets(&self) -> Vec<AiChoice> {
        let selections = self.selections().await;
        if selections.is_empty() {
            return vec![AiChoice::bridge()];
        }
        let mut targets = selections;
        // 安定な並べ替え。メインが立っていなければ何も動かない(先頭がそのまま先頭)
        targets.sort_by_key(|t| !t.primary);
        targets
    }

    /// **「詳しく調べる」を頼む1人。** 立っていなければ先頭、選択が空なら CLI ブリッジ。
    ///
    /// **必ず1人に決まる。** 決まらないと「押したのに誰も答えない」が起きるし、
    /// 全員に投げると web 検索ぶんの時間と枠が人数倍になる。
    pub async fn primary_target(&self) -> AiChoice {
        // `targets` がメインを先頭にそろえてあるので、先頭がそのままメイン
        // (誰も立っていなければ先頭が代わりを務める)
        self.targets().await.remove(0)
    }

    /// メインの相手の識別子(画面のラジオを立てるのに使う)。
    pub async fn primary_backend(&self) -> String {
        self.primary_target().await.backend
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
    ///
    /// `reasons` は「候補に挙げた理由」(監視の一覧のときだけ入る。ドメイン→理由の文)。
    /// **無ければ足さない** —— 空の括弧を並べても材料にならない。
    pub async fn ask_about_domains(
        &self,
        domains: &[String],
        reasons: &HashMap<String, String>,
        mode: AskMode,
    ) -> Result<Answer, AskError> {
        if domains.is_empty() {
            return Err(AskError::Failed("聞く対象がありません".to_string()));
        }

        // 番号付きで渡し、番号で返させる(ドメイン名を書き写させない)
        let user_prompt = domains
            .iter()
            .enumerate()
            .map(|(i, domain)| match reason_of(reasons, domain) {
                Some(reason) => format!("{}. {domain}({reason})", i + 1),
                None => format!("{}. {domain}", i + 1),
            })
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
                tokio::spawn(async move {
                    ai.ask_target(&target, mode.system_prompt(), &prompt, &domains)
                        .await
                })
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
    ///
    /// **指示文は受け取る**(どちらの一覧かで変わる)。**相手では変えない** ——
    /// 切り替えたときに変わるのが書き手だけであるように。
    async fn ask_target(
        &self,
        target: &AiChoice,
        system_prompt: &'static str,
        user_prompt: &str,
        domains: &[String],
    ) -> Result<(String, Vec<(String, String)>), AskError> {
        let (text, author) = if target.is_bridge() {
            let text = self.claude.ask(system_prompt, user_prompt).await?;
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
                    // メモを1〜2文で書かせるだけなので web は要らない
                    false,
                    self.chiezo_timeout,
                    system_prompt,
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

    /// **1件のドメインを詳しく調べる。** 頼むのは[`Self::primary_target`]の1人だけ。
    ///
    /// `profile` はこちらが観測した事実(件数・初出・端末・状態の内訳など)。
    /// **web 検索と観測データの両方を渡す**ので、通常の問い合わせよりずっと時間がかかる
    /// (上限は `INVESTIGATE_TIMEOUT`)。
    ///
    /// `mode` は**どちらの一覧から押されたか**。ブロック済みなら「止まっている」、
    /// 監視なら「振る舞いで拾った候補で、ブロックされたとは限らない」と伝える ——
    /// これを渡さないと、素通りしている通信まで「ブロックされた」前提で説明される。
    /// `reason` は候補に挙げた理由(監視のときだけ。空なら足さない)。
    ///
    /// 戻り値は (書き手の名前, 調べた結果)。**メモの書式は通常と同じ**
    /// (`[書き手] 本文`)にして、画面が出し分けなくて済むようにしてある。
    pub async fn investigate(
        &self,
        domain: &str,
        profile: &str,
        mode: AskMode,
        reason: &str,
    ) -> Result<(String, String), AskError> {
        let target = self.primary_target().await;
        let reason = reason.trim();
        let reason_line = if reason.is_empty() {
            String::new()
        } else {
            format!("この画面が候補に挙げた理由: {reason}\n")
        };
        let user_prompt = format!(
            "調べる対象のドメイン: {domain}
{}
{reason_line}
             このネットワークでの観測データ:
{profile}",
            mode.origin()
        );

        self.ask_primary(&target, INVESTIGATE_PROMPT, &user_prompt)
            .await
    }

    /// **調査結果をもとに、もう一歩聞く。** 相手も上限も[`Self::investigate`]と同じ
    /// (メインの1人・web 検索あり・`INVESTIGATE_TIMEOUT`)で、違うのは
    /// **これまでのやり取り(`research`)と質問を渡し、決まった見出しを求めないこと**。
    ///
    /// `research` には前の答えだけでなく、**それまでの質問と答えも入っている**
    /// (画面が追記しているため)。渡さないと2つ目の質問が1つ目の答えを知らずに返る。
    pub async fn follow_up(
        &self,
        domain: &str,
        profile: &str,
        mode: AskMode,
        reason: &str,
        research: &str,
        question: &str,
    ) -> Result<(String, String), AskError> {
        let target = self.primary_target().await;
        let reason = reason.trim();
        let reason_line = if reason.is_empty() {
            String::new()
        } else {
            format!("この画面が候補に挙げた理由: {reason}\n")
        };
        let user_prompt = format!(
            "対象のドメイン: {domain}
{}
{reason_line}
             これまでの調査結果(あなたや他のAIが書いたもの。質問と答えが続いていることもあります):
{research}

             このネットワークでの観測データ:
{profile}

             追加の質問: {question}",
            mode.origin()
        );

        self.ask_primary(&target, FOLLOWUP_PROMPT, &user_prompt).await
    }

    /// メインの相手に1往復頼む(web 検索あり・長い上限)。
    /// **「詳しく調べる」と「もう一歩聞く」で経路を1本にする** ——
    /// 相手の選び方・web の可否・上限秒数・書き手の名乗りの扱いを2か所に分けない。
    ///
    /// 戻り値は (書き手の名前, 本文)。**本文の頭に書き手を付ける**
    /// (`[書き手] 本文`)ので、画面は出し分けなくてよい。
    async fn ask_primary(
        &self,
        target: &AiChoice,
        system_prompt: &'static str,
        user_prompt: &str,
    ) -> Result<(String, String), AskError> {
        let (text, author) = if target.is_bridge() {
            let text = self
                .claude
                .ask_within(system_prompt, user_prompt, self.investigate_timeout)
                .await?;
            (text, BRIDGE_LABEL.to_string())
        } else {
            let Some(chiezo) = self.chiezo.as_ref() else {
                return Err(AskError::Failed("Chiezo の URL が未設定です".to_string()));
            };
            let completion = chiezo
                .complete(
                    &target.backend,
                    target.model.as_deref(),
                    target.effort.as_deref(),
                    // **ここは web を立てる。** 運営元や評判は手元のデータからは分からない
                    true,
                    self.investigate_timeout,
                    system_prompt,
                    user_prompt,
                )
                .await
                .map_err(AskError::Failed)?;
            let label = completion.label.unwrap_or_else(|| target.label.clone());
            let model = completion.model.or_else(|| target.model.clone());
            (completion.content, name_with_model(&label, model.as_deref()))
        };

        let text = text.trim();
        if text.is_empty() {
            return Err(AskError::Failed(format!("{author} が空の答えを返しました")));
        }
        Ok((author.clone(), format!("[{author}] {text}")))
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

/// そのドメインの「候補に挙げた理由」。**空白だけなら無い扱い** ——
/// 空の括弧を渡すと、材料が無いのに何かあるように読める。
fn reason_of<'a>(reasons: &'a HashMap<String, String>, domain: &str) -> Option<&'a str> {
    reasons
        .get(domain)
        .map(|r| r.trim())
        .filter(|r| !r.is_empty())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_mode_falls_back_to_blocked() {
        assert_eq!(AskMode::parse("watch"), AskMode::Watch);
        assert_eq!(AskMode::parse(" watch "), AskMode::Watch);
        // 未指定・知らない値は既定の一覧(ブロック済み)に倒す ——
        // 手で叩いたリクエストや古い画面がここに来る
        assert_eq!(AskMode::parse(""), AskMode::Blocked);
        assert_eq!(AskMode::parse("なにか"), AskMode::Blocked);
    }

    #[test]
    fn watch_prompt_does_not_assume_the_domain_was_blocked() {
        // **これが「ブロックされたと考えられます」を防いでいる本体。**
        // 監視の一覧はブロックの結果ではないので、そう書かせない
        let watch = AskMode::Watch.system_prompt();
        assert!(watch.contains("ブロックされたことを前提にした"));
        assert!(AskMode::Watch.origin().contains("ブロックリストで止まったものではありません"));
        // ブロック済みの一覧は逆に「なぜ止まったか」を聞く。**混ぜない**
        assert!(AskMode::Blocked.system_prompt().contains("なぜブロックされていそうか"));
        assert_ne!(watch, AskMode::Blocked.system_prompt());
    }

    #[test]
    fn empty_reasons_are_not_appended() {
        let mut reasons = HashMap::new();
        reasons.insert("a.example.com".to_string(), " 3時間前にはじめて見た ".to_string());
        // 空白だけの理由は「無い」扱い —— 空の括弧を渡しても材料にならない
        reasons.insert("b.example.com".to_string(), "   ".to_string());
        assert_eq!(reason_of(&reasons, "a.example.com"), Some("3時間前にはじめて見た"));
        assert_eq!(reason_of(&reasons, "b.example.com"), None);
        assert_eq!(reason_of(&reasons, "c.example.com"), None);
    }
}

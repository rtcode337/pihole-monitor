//! 疎通を確かめる道具(ping / 経路)。**設定画面から手で叩くためだけのもの**で、
//! 一覧の判定には一切関わらない。
//!
//! 一覧に並ぶのは「名前を引いた記録」だけなので、**その先に本当に届くのかは分からない**。
//! 「はじめて見た」ドメインが手元のどこを通ってどこへ出ていくのか、Pi-hole 自身に
//! 届いているのか —— そこは実際にパケットを出さないと言えない。
//!
//! **外部コマンドを呼ぶ唯一の場所。** 気をつけているのは3つ:
//!
//! - **シェルを通さない**(`Command` に引数を配列で渡す)。文字列を組み立てて `sh -c` に
//!   渡すと、`;` や `$()` を含む相手先でそのまま実行されてしまう
//! - **相手先の文字を絞る**([`validate_target`])。ホスト名とIPに出てくる文字だけを許し、
//!   **`-` で始まるものは断る** —— 断らないと `-f`(flood)のような**オプションとして**
//!   渡せてしまう
//! - **必ず上限をつける**。コマンド側の上限(`-w` 等)に加えてこちらでも待つのをやめ、
//!   `kill_on_drop` で子プロセスごと落とす(応答しない相手で溜まり続けないように)

use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::Command;

/// 相手先の長さの上限。ホスト名の上限(253)に合わせる。
const MAX_TARGET_LEN: usize = 253;

/// 画面に返す出力の上限(文字)。**黙って切らない**(切ったことは末尾に書く)。
const MAX_OUTPUT_CHARS: usize = 8_000;

/// ping の回数と、コマンド側の締め切り(秒)。
const PING_COUNT: &str = "4";
const PING_DEADLINE: &str = "8";

/// 経路をたどる最大ホップ数。**家庭から見る用途なので深追いしない**。
const TRACE_MAX_HOPS: &str = "15";

/// こちらが待つ上限。**コマンド側の締め切りより少し長く**する ——
/// 先に切ると「コマンドが何秒で諦めたか」が出力から読めなくなる。
const PING_TIMEOUT: Duration = Duration::from_secs(15);
const TRACE_TIMEOUT: Duration = Duration::from_secs(45);

/// 打てる道具。**増やすときは必ずここに足す**(呼ぶ側が文字列でコマンドを組めないように)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Ping,
    Trace,
}

impl Tool {
    /// 画面から来た文字列を読む。知らない値は `None`(受け付けない)。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "ping" => Some(Self::Ping),
            "traceroute" | "trace" => Some(Self::Trace),
            _ => None,
        }
    }

    fn timeout(self) -> Duration {
        match self {
            Self::Ping => PING_TIMEOUT,
            Self::Trace => TRACE_TIMEOUT,
        }
    }

    /// 使う実行ファイルの候補と引数。**先に見つかったものを使う**。
    ///
    /// 経路の1番目が `traceroute` ではなく `tracepath` なのは、**コンテナが非rootで
    /// 動くから** —— `traceroute` は raw socket を開くので `CAP_NET_RAW` が要り、
    /// 非rootのままでは「Operation not permitted」で終わる。`tracepath` は
    /// 特権なしで動くように作られている(イメージにはこちらを入れてある)。
    fn candidates(self, target: &str) -> Vec<(&'static str, Vec<String>)> {
        let t = target.to_string();
        match self {
            // `-n` は名前を引き直さないため(引くと遅いうえ、DNSの調子を見たいときに邪魔)
            Self::Ping => vec![(
                "ping",
                vec![
                    "-n".into(),
                    "-c".into(),
                    PING_COUNT.into(),
                    "-w".into(),
                    PING_DEADLINE.into(),
                    t,
                ],
            )],
            Self::Trace => vec![
                (
                    "tracepath",
                    vec!["-n".into(), "-m".into(), TRACE_MAX_HOPS.into(), t.clone()],
                ),
                (
                    "traceroute",
                    vec![
                        "-n".into(),
                        "-m".into(),
                        TRACE_MAX_HOPS.into(),
                        "-q".into(),
                        "1".into(),
                        "-w".into(),
                        "2".into(),
                        t,
                    ],
                ),
            ],
        }
    }
}

/// 打った結果。**出力はそのまま返す** —— こちらで要約すると、
/// 見たかった数字(応答時間・欠落・どのホップで止まったか)が落ちる。
pub struct Outcome {
    /// 実際に走らせたコマンド(画面に出す。何をしたのか分からないと結果を読めない)
    pub command: String,
    pub output: String,
    /// 終了コード0か。**偽でも失敗ではない**(応答が無いのも結果のうち)
    pub ok: bool,
    pub elapsed_ms: u128,
}

/// 打つ。返すのは (結果) か、打てなかった理由。
pub async fn run(tool: Tool, target: &str) -> Result<Outcome, String> {
    let target = validate_target(target)?;
    let started = Instant::now();

    let mut missing = Vec::new();
    for (program, args) in tool.candidates(&target) {
        // **シェルを通さない。** 引数は配列で渡す
        let child = Command::new(program)
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // 待つのをやめたら子プロセスも落とす(応答しない相手で溜めない)
            .kill_on_drop(true)
            .output();

        let output = match tokio::time::timeout(tool.timeout(), child).await {
            Err(_) => {
                return Err(format!(
                    "{program} が {} 秒で終わりませんでした",
                    tool.timeout().as_secs()
                ))
            }
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                // 次の候補を試す(どれも無ければまとめて理由にする)
                missing.push(program);
                continue;
            }
            Ok(Err(e)) => return Err(format!("{program} を実行できません: {e}")),
            Ok(Ok(output)) => output,
        };

        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            // **標準エラーも見せる。** 権限や名前解決の失敗はこちらに出る
            text.push_str(stderr.trim_end());
            text.push('\n');
        }

        return Ok(Outcome {
            command: format!("{program} {}", args.join(" ")),
            output: truncate(text.trim_end()),
            ok: output.status.success(),
            elapsed_ms: started.elapsed().as_millis(),
        });
    }

    Err(format!(
        "この環境に {} が入っていません(コンテナのイメージには入れてあります)",
        missing.join(" / ")
    ))
}

/// 相手先を確かめる。**ホスト名とIPに出てくる文字だけ**を許す。
///
/// 通すのは英数字と `.` `-` `_` `:`(IPv6)。**`-` で始まるものは断る** ——
/// コマンドのオプションとして渡せてしまうため(`-f` で flood ping になる)。
fn validate_target(raw: &str) -> Result<String, String> {
    let target = raw.trim();
    if target.is_empty() {
        return Err("相手先を入れてください".to_string());
    }
    if target.len() > MAX_TARGET_LEN {
        return Err("相手先が長すぎます".to_string());
    }
    if target.starts_with('-') {
        return Err("相手先を - で始めることはできません".to_string());
    }
    if !target
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':'))
    {
        return Err("相手先に使えるのは英数字と . - _ : だけです（URLではなくホスト名かIPを入れてください）".to_string());
    }
    Ok(target.to_string())
}

/// 長すぎる出力を切る。**切ったことは末尾に書く**(黙って切ると、
/// 経路が途中で終わったのか画面の都合なのか分からない)。
fn truncate(text: &str) -> String {
    if text.chars().count() <= MAX_OUTPUT_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_OUTPUT_CHARS).collect();
    format!("{head}\n…（出力が長いのでここで切りました）")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_hostname_and_ip_characters_pass() {
        assert_eq!(validate_target(" example.com ").unwrap(), "example.com");
        assert_eq!(validate_target("192.168.0.4").unwrap(), "192.168.0.4");
        assert!(validate_target("2001:db8::1").is_ok());
        assert!(validate_target("_dmarc.example.com").is_ok());
    }

    #[test]
    fn shell_and_option_injection_are_refused() {
        // **シェルは通していない**が、通す文字も絞る(将来の呼び出し方に依存しない)
        for bad in [
            "example.com; rm -rf /",
            "example.com && id",
            "$(id)",
            "`id`",
            "example.com|cat",
            "http://example.com/path",
            "example.com /etc/passwd",
        ] {
            assert!(validate_target(bad).is_err(), "{bad} を通してはいけない");
        }
        // **オプションとして渡せる形は断る**(`-f` は flood ping)
        assert!(validate_target("-f").is_err());
        assert!(validate_target("--help").is_err());
        assert!(validate_target("").is_err());
        assert!(validate_target(&"a".repeat(MAX_TARGET_LEN + 1)).is_err());
    }

    #[test]
    fn tool_parse_takes_only_known_names() {
        assert_eq!(Tool::parse("ping"), Some(Tool::Ping));
        assert_eq!(Tool::parse("traceroute"), Some(Tool::Trace));
        assert_eq!(Tool::parse("trace"), Some(Tool::Trace));
        // 知らない道具は受け付けない(呼ぶ側が文字列でコマンドを組めないようにするため)
        assert_eq!(Tool::parse("nmap"), None);
        assert_eq!(Tool::parse(""), None);
    }

    #[test]
    fn trace_prefers_tracepath_because_the_container_runs_as_non_root() {
        // `traceroute` は raw socket が要るので非rootでは動かない。
        // 並び順を入れ替えると、コンテナで「Operation not permitted」しか返らなくなる
        let names: Vec<&str> = Tool::Trace
            .candidates("example.com")
            .into_iter()
            .map(|(p, _)| p)
            .collect();
        assert_eq!(names, vec!["tracepath", "traceroute"]);
    }

    #[test]
    fn output_is_cut_with_a_visible_note() {
        let long = "x".repeat(MAX_OUTPUT_CHARS + 100);
        let cut = truncate(&long);
        assert!(cut.contains("ここで切りました"), "切ったことを書いていない");
        assert!(truncate("短い出力") == "短い出力");
    }
}

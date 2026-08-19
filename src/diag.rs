//! 疎通を確かめる道具(ping / 経路)。設定画面から手で叩くためだけのもので、
//! 一覧の判定には一切関わらない。
//!
//! 一覧に並ぶのは「名前を引いた記録」だけなので、その先に本当に届くのかは分からない。
//! 「はじめて見た」ドメインが手元のどこを通ってどこへ出ていくのか、Pi-hole 自身に
//! 届いているのか —— そこは実際にパケットを出さないと言えない。
//!
//! 出力は溜めずに1行ずつ流す([`start`] が返す [`Session`])。ping は4回・経路は
//! 応答しないホップがあると数十秒かかるので、終わるまで白いままだと「打てているのか」が
//! 分からない —— 実際、応答の無い相手では画面が止まって見えていた。
//!
//! 外部コマンドを呼ぶ唯一の場所。 気をつけているのは3つ:
//!
//! - シェルを通さない(`Command` に引数を配列で渡す)。文字列を組み立てて `sh -c` に
//!   渡すと、`;` や `$()` を含む相手先でそのまま実行されてしまう
//! - 相手先の文字を絞る([`validate_target`])。ホスト名とIPに出てくる文字だけを許し、
//!   `-` で始まるものは断る —— 断らないと `-f`(flood)のようなオプションとして
//!   渡せてしまう
//! - 必ず上限をつける。コマンド側の上限(`-w` 等)に加えてこちらでも待つのをやめ、
//!   `kill_on_drop` で子プロセスごと落とす(応答しない相手で溜まり続けないように)

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

/// 相手先の長さの上限。ホスト名の上限(253)に合わせる。
const MAX_TARGET_LEN: usize = 253;

/// 画面に返す出力の上限(文字)。黙って切らない(切ったことは末尾に書く)。
const MAX_OUTPUT_CHARS: usize = 8_000;

/// ping の回数と、コマンド側の締め切り(秒)。
const PING_COUNT: &str = "4";
const PING_DEADLINE: &str = "8";

/// 経路をたどる最大ホップ数。家庭から見る用途なので深追いしない。
const TRACE_MAX_HOPS: &str = "15";

/// こちらが待つ上限。コマンド側の締め切りより少し長くする ——
/// 先に切ると「コマンドが何秒で諦めたか」が出力から読めなくなる。
///
/// 経路が長いのは、応答しないホップが1つにつき約3秒かかるため ——
/// `-m 15` の全部が黙っていると 45 秒に届く(実測で 24 秒かかった相手がある)。
/// 途中経過を流すようになってからは、切れてもそこまでの経路は画面に残るので、
/// 待つ側の損は小さい。
const PING_TIMEOUT: Duration = Duration::from_secs(15);
const TRACE_TIMEOUT: Duration = Duration::from_secs(60);

/// ホップの名前を引くときの上限。短くする —— 名前は添え物なので、
/// DNS が黙っているときに経路の表示ごと待たせない。
const NAME_TIMEOUT: Duration = Duration::from_secs(2);

/// 子プロセスの後始末を待つ上限。SIGKILL の後なのですぐ終わるが、
/// ここで詰まると次の実行まで止まるので上限は掛ける。
const CHILD_WAIT: Duration = Duration::from_secs(2);

/// 画面へ流す途中経過の溜め。読み手(HTTPの応答)が詰まったらコマンド側も待たせる
/// (溜め続けるとメモリに乗るだけで、どのみち読まれない)。
const EVENT_BUFFER: usize = 64;

/// 打てる道具。増やすときは必ずここに足す(呼ぶ側が文字列でコマンドを組めないように)。
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

    /// ホップのIPから名前を引くか。経路だけでよい ——
    /// ping は同じ相手が並ぶだけなので、全行に同じ名前が付いて邪魔になる。
    fn resolves_names(self) -> bool {
        matches!(self, Self::Trace)
    }

    /// 使う実行ファイルの候補と引数。先に見つかったものを使う。
    ///
    /// 経路の1番目が `traceroute` ではなく `tracepath` なのは、コンテナが非rootで
    /// 動くから —— `traceroute` は raw socket を開くので `CAP_NET_RAW` が要り、
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
            // 経路も `-n` のまま引く。名前はこちらで足す（[`lookup_name`]）——
            // `-n` を外すと tracepath はIPを名前で置き換えてしまい、
            // どのアドレスを通ったのかが読めなくなる(実測)。両方見せたいので自分で引く
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

/// 画面へ流す途中経過。行には番号を振る —— 名前は後から届いて
/// 既に出した行に足されるので、追記だけでは書き足す先を指せない。
pub enum Event {
    /// 出力の1行(番号は0から)
    Line { index: usize, text: String },
    /// その行のIPを引いて分かった名前。IPも一緒に渡す ——
    /// 画面はそのIPのすぐ後ろに名前を差し込む(行末に足すと画面の外に出る)
    Name {
        index: usize,
        ip: String,
        name: String,
    },
    /// 打ち終わった。`ok` が偽でも失敗ではない(応答が無いのも結果のうち)
    End { ok: bool, elapsed_ms: u128 },
    /// 途中で打ち切った理由(時間切れなど)
    Error { message: String },
}

/// 走り出したコマンド。`events` を最後まで読むと終わる。
/// 落とすと子プロセスも死ぬ(`kill_on_drop`)ので、画面を閉じた相手のために
/// ping を打ち続けることはない。
pub struct Session {
    /// 実際に走らせたコマンド(画面に出す。何をしたのか分からないと結果を読めない)
    pub command: String,
    pub events: mpsc::Receiver<Event>,
}

/// 打ち始める。返すのは (途中経過の口) か、打てなかった理由。
///
/// 打てなかった理由はここで返す(相手先の形が悪い・コマンドが無い)——
/// 流し始めてから言うと、画面は 200 を受け取った後でエラーを読むことになる。
pub fn start(tool: Tool, target: &str) -> Result<Session, String> {
    let target = validate_target(target)?;

    let mut missing = Vec::new();
    for (program, args) in tool.candidates(&target) {
        // 実行ファイルは自分で探す。 `stdbuf` に包むと「そのコマンドが無い」が
        // stdbuf の終了コード(127)になり、次の候補へ移る判断ができなくなる
        let Some(path) = which(program) else {
            missing.push(program);
            continue;
        };

        let child = match spawn(&path, &args) {
            Ok(child) => child,
            Err(e) => return Err(format!("{program} を実行できません: {e}")),
        };

        let (tx, events) = mpsc::channel(EVENT_BUFFER);
        tokio::spawn(pump(child, tool, program, tx));

        return Ok(Session {
            // `stdbuf` は出さない。 行ごとに流すためのこちらの都合で、
            // 疎通の結果を読むのには関係がない
            command: format!("{program} {}", args.join(" ")),
            events,
        });
    }

    Err(format!(
        "この環境に {} が入っていません(コンテナのイメージには入れてあります)",
        missing.join(" / ")
    ))
}

/// 子プロセスを起こす。シェルは通さない(引数は配列で渡す)。
///
/// `stdbuf -oL` に包むのは、パイプに繋ぐと出力が溜まるから —— glibc の stdio は
/// 相手が端末でないと満杯になるまで書き出さないので、`tracepath` は応答の無いホップを
/// 何秒待っても1行も出さず、終わってからまとめて出てくる(実測)。`ping` は自分で
/// 行バッファにしているので元から流れるが、包み方は道具で変えない。
/// `stdbuf` が無い環境では包まずに起こす —— 途中経過が出ないだけで結果は同じ。
fn spawn(program: &Path, args: &[String]) -> std::io::Result<Child> {
    let mut command = match which("stdbuf") {
        Some(stdbuf) => {
            let mut c = Command::new(stdbuf);
            c.arg("-oL").arg(program);
            c
        }
        None => Command::new(program),
    };
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 待つのをやめたら子プロセスも落とす(応答しない相手で溜めない)
        .kill_on_drop(true)
        .spawn()
}

/// 出力を1行ずつ流し、終わったら名前を足して締める。
async fn pump(mut child: Child, tool: Tool, program: &'static str, tx: mpsc::Sender<Event>) {
    let started = Instant::now();
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return;
    };
    let mut out = BufReader::new(stdout).lines();
    let mut err = BufReader::new(stderr).lines();

    let mut budget = OutputBudget::new();
    let mut index = 0usize;
    // 名前を引く相手。行の番号ごと覚える —— 同じIPが何行にも出る(tracepath は
    // ホップを2回出すことがある)ので、引くのは1回で足すのは全部の行
    let mut hops: Vec<(usize, IpAddr)> = Vec::new();

    let reading = async {
        let (mut out_done, mut err_done) = (false, false);
        while !(out_done && err_done) {
            let next = tokio::select! {
                r = out.next_line(), if !out_done => (false, r),
                r = err.next_line(), if !err_done => (true, r),
            };
            let line = match next {
                // 標準エラーも見せる。 権限や名前解決の失敗はこちらに出る
                (_, Ok(Some(line))) => line,
                (is_err, _) => {
                    if is_err {
                        err_done = true;
                    } else {
                        out_done = true;
                    }
                    continue;
                }
            };
            let Some(line) = budget.take(&line) else {
                let _ = tx.send(Event::Line { index, text: CUT_NOTE.into() }).await;
                return true;
            };
            if tool.resolves_names()
                && let Some(ip) = hop_ip(&line)
            {
                hops.push((index, ip));
            }
            if tx.send(Event::Line { index, text: line }).await.is_err() {
                // 画面が閉じた。打ち切りとして扱い、子プロセスは `stop` が片付ける
                return true;
            }
            index += 1;
        }
        false
    };

    // こちらの上限で打ち切る。 コマンド側の締め切りより長くしてあるので、
    // ここに来るのはコマンドが自分で諦めなかったときだけ
    let cut = match tokio::time::timeout(tool.timeout(), reading).await {
        Ok(cut) => cut,
        Err(_) => {
            let _ = tx
                .send(Event::Error {
                    message: format!(
                        "{program} が {} 秒で終わりませんでした",
                        tool.timeout().as_secs()
                    ),
                })
                .await;
            stop(&mut child).await;
            return;
        }
    };

    let ok = if cut {
        stop(&mut child).await;
        false
    } else {
        matches!(
            tokio::time::timeout(CHILD_WAIT, child.wait()).await,
            Ok(Ok(status)) if status.success()
        )
    };
    let elapsed_ms = started.elapsed().as_millis();

    // 名前は経路が出そろってから引く。 行ごとに引くと、DNS の応答を待つあいだ
    // 次のホップまで止まって「途中経過を見せる」意味が無くなる。
    // 読み手が居なくなっていたら引かない(誰も読まない名前のために DNS を叩かない)
    if !hops.is_empty() && !tx.is_closed() {
        send_names(&hops, &tx).await;
    }
    let _ = tx.send(Event::End { ok, elapsed_ms }).await;
}

/// 途中でやめた子プロセスを片付ける。
///
/// 殺すだけでは足りない。 `kill_on_drop` は SIGKILL を送るだけで待たないので、
/// 死んだ子はゾンビのまま残る —— 次にコマンドを起こすまで消えない
/// (画面を閉じて経路をやめた後、`tracepath` が残っているのを実測した)。
/// PID 1 がこのアプリ自身のコンテナでは、拾ってくれる init もいない。
async fn stop(child: &mut Child) {
    let _ = tokio::time::timeout(CHILD_WAIT, child.kill()).await;
}

/// ホップのIPを名前に直して流す。まとめて同時に引く(1件ずつだと最大2秒×ホップ数)。
async fn send_names(hops: &[(usize, IpAddr)], tx: &mpsc::Sender<Event>) {
    let Some(getent) = which("getent") else {
        return;
    };

    let mut unique: Vec<IpAddr> = Vec::new();
    for (_, ip) in hops {
        if !unique.contains(ip) {
            unique.push(*ip);
        }
    }

    let lookups: Vec<_> = unique
        .into_iter()
        .map(|ip| {
            let getent = getent.clone();
            tokio::spawn(async move { (ip, lookup_name(&getent, ip).await) })
        })
        .collect();

    for lookup in lookups {
        let Ok((ip, Some(name))) = lookup.await else {
            continue;
        };
        for (index, _) in hops.iter().filter(|(_, hop)| *hop == ip) {
            let _ = tx
                .send(Event::Name {
                    index: *index,
                    ip: ip.to_string(),
                    name: name.clone(),
                })
                .await;
        }
    }
}

/// IPから名前を引く。`getent` に任せる —— このアプリが使うのと同じ経路
/// (`/etc/resolv.conf` → Pi-hole)で引けるので、手元の機器には Pi-hole が知っている
/// 名前が付く。自前で PTR を投げると、その経路をもう一度実装することになる。
///
/// 引けなければ `None`(名前が無いのは普通のことなので、理由は画面に出さない)。
async fn lookup_name(getent: &Path, ip: IpAddr) -> Option<String> {
    let output = Command::new(getent)
        .arg("hosts")
        .arg(ip.to_string())
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output();

    let output = tokio::time::timeout(NAME_TIMEOUT, output).await.ok()?.ok()?;
    if !output.status.success() {
        return None;
    }
    parse_getent(&String::from_utf8_lossy(&output.stdout), ip)
}

/// `getent hosts` の出力(`1.1.1.1  one.one.one.one`)から名前だけ取る。
fn parse_getent(stdout: &str, ip: IpAddr) -> Option<String> {
    let name = stdout.lines().next()?.split_whitespace().nth(1)?;
    // 名前が付いていないと、IPがそのまま2列目に出ることがある(足しても読めない)
    if name.parse::<IpAddr>().is_ok_and(|parsed| parsed == ip) {
        return None;
    }
    Some(name.to_string())
}

/// 出力の1行からホップのIPを取る。IPとして読める最初の語だけを見る ——
/// `tracepath` も `traceroute` も応答時間や `pmtu` を同じ行に並べるので、
/// 位置(何番目の語か)で決め打つと道具を変えた瞬間に外れる。
fn hop_ip(line: &str) -> Option<IpAddr> {
    line.split_whitespace()
        .find_map(|word| word.trim_end_matches(&[',', ':'][..]).parse::<IpAddr>().ok())
}

/// 切ったことを書く1行。黙って切ると、経路が途中で終わったのか
/// 画面の都合なのか分からない。
const CUT_NOTE: &str = "…（出力が長いのでここで切りました）";

/// 流してよい残りの文字数。行を出すたびに減らす ——
/// 溜めずに流す以上、最後にまとめて切ることはできない。
struct OutputBudget {
    left: usize,
}

impl OutputBudget {
    fn new() -> Self {
        Self {
            left: MAX_OUTPUT_CHARS,
        }
    }

    /// 収まるなら行を返す。`None` は「ここで打ち切る」。
    fn take(&mut self, line: &str) -> Option<String> {
        let count = line.chars().count();
        if count > self.left {
            return None;
        }
        self.left -= count;
        Some(line.to_string())
    }
}

/// PATH から実行ファイルを探す。`which(1)` を呼ばない(外部コマンドを増やさない)。
fn which(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|path| is_executable(path))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// 相手先を確かめる。ホスト名とIPに出てくる文字だけを許す。
///
/// 通すのは英数字と `.` `-` `_` `:`(IPv6)。`-` で始まるものは断る ——
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
        // シェルは通していないが、通す文字も絞る(将来の呼び出し方に依存しない)
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
        // オプションとして渡せる形は断る(`-f` は flood ping)
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
    fn both_tools_keep_numeric_output_so_addresses_stay_visible() {
        // `-n` を外すと tracepath はIPを名前で置き換える。名前はこちらで足す
        for (_, args) in Tool::Trace.candidates("example.com") {
            assert!(args.contains(&"-n".to_string()), "-n を外してはいけない");
        }
    }

    #[test]
    fn names_are_looked_up_for_the_route_only() {
        // ping は同じ相手が並ぶだけなので、全行に同じ名前が付いても読めない
        assert!(Tool::Trace.resolves_names());
        assert!(!Tool::Ping.resolves_names());
    }

    #[test]
    fn hop_address_is_found_wherever_it_sits_on_the_line() {
        // tracepath
        assert_eq!(
            hop_ip(" 2:  192.168.1.1                       0.882ms "),
            Some("192.168.1.1".parse().unwrap())
        );
        // tracepath（pmtu が後ろに付く行）
        assert_eq!(
            hop_ip(" 4:  198.51.100.7    1.275ms pmtu 1454"),
            Some("198.51.100.7".parse().unwrap())
        );
        // traceroute
        assert_eq!(
            hop_ip(" 1  203.0.113.1  0.317 ms"),
            Some("203.0.113.1".parse().unwrap())
        );
        // 応答の無いホップと、番号だけの行にはIPが無い
        assert_eq!(hop_ip(" 6:  no reply"), None);
        assert_eq!(hop_ip(" 1?: [LOCALHOST]                pmtu 1500"), None);
    }

    #[test]
    fn getent_output_gives_the_name_but_never_the_address_again() {
        let ip: IpAddr = "192.0.2.10".parse().unwrap();
        assert_eq!(
            parse_getent("192.0.2.10     router.example      alias\n", ip),
            Some("router.example".to_string())
        );
        // 名前が付いていない相手（2列目までIP）は足さない
        assert_eq!(parse_getent("192.0.2.10     192.0.2.10\n", ip), None);
        assert_eq!(parse_getent("", ip), None);
    }

    #[test]
    fn output_is_cut_with_a_visible_note() {
        let mut budget = OutputBudget::new();
        let line = "x".repeat(MAX_OUTPUT_CHARS / 2);
        assert!(budget.take(&line).is_some());
        assert!(budget.take(&line).is_some());
        // 3行目は入らない。呼ぶ側はここで打ち切って CUT_NOTE を出す
        assert!(budget.take(&line).is_none());
        assert!(CUT_NOTE.contains("ここで切りました"), "切ったことを書いていない");
    }
}

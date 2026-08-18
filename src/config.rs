//! 環境変数・定数の一元管理。プロセス起動時に一度だけ読む。

use std::env;
use std::path::PathBuf;
use std::time::Duration;

/// リッスンするポート。
///
/// **7000番台に10刻みで割り当てる運用**(末尾0 = ブラウザで開くもの)に合わせた
/// pihole-monitor の枠。ホスト側とコンテナ内で同じ番号にしてある ——
/// composeの `"7060:7060"` を読むだけで対応が分かるようにするため。
///
/// **6000番台は使わない。** 6000はX11用に予約されており、主要ブラウザ
/// (Chrome/Firefox/Safari)が「安全でないポート」として接続を拒否する
/// (ERR_UNSAFE_PORT)。かつて6001を使っていたのはこれを避けるためだった。
pub const PORT: u16 = 7060;

/// CLI ブリッジの応答がこれらを含んでいたら認証エラーとみなし、
/// 保存済みトークンを破棄して再入力を促す(CLI 自身の認証エラーは 502 の本文に出る)。
pub const AUTH_ERROR_KEYWORDS: &[&str] = &[
    "invalid api key",
    "invalid bearer token",
    "authentication_error",
    "unauthorized",
    "please run",
    "/login",
    "oauth token has expired",
    "token has expired",
    "token expired",
    "401",
];

#[derive(Debug, Clone)]
pub struct Config {
    pub pihole_base_url: String,
    pub pihole_password: String,
    /// 取得するブロッククエリの件数。-1で全件(Pi-hole v6 APIの `length` パラメータ)
    pub pihole_query_limit: i64,
    pub claude_timeout: Duration,
    pub db_path: PathBuf,
    /// CLI ブリッジ(chiezo-bridge)の URL。OpenAI 互換の口の根元まで。
    pub claude_bridge_url: String,
    /// Chiezo(LAN 内の知識サーバー)の**ルート URL**。空なら使わない。
    /// **`/v1` は付けない** —— 呼ぶ側が `/v1/ai/...` を足す。
    pub chiezo_base_url: String,
    /// 「詳しく調べる」1回の上限。**通常の問い合わせよりずっと長い** ——
    /// web 検索を伴うので、1〜2文のメモを書かせるのとは桁が違う。
    pub investigate_timeout: Duration,
    /// Chiezo 越しの1回の生成の上限。相手は CLI や大きいモデルなので、
    /// **ブリッジ経由(`claude_timeout`)より長めの既定にしてある**。
    pub chiezo_timeout: Duration,
    /// ブリッジと共有するディレクトリ。ここに設定 DB を書き、ブリッジが読み取り専用で読む。
    pub state_dir: PathBuf,
    /// CLI を同梱していた頃のトークンの置き場(移行のためだけに残している)。
    pub claude_token_path: PathBuf,

    // ---- DNSの取り込み(ingest.rs) ----
    /// 取り込みを回すか。**既定は有効** —— これが無いと「怪しい通信」の判定材料が貯まらない。
    pub dns_ingest_enabled: bool,
    /// 取り込みの周期。短くしても Pi-hole を叩く回数が増えるだけで、得られる情報は変わらない。
    pub dns_ingest_interval: Duration,
    /// 生のクエリを何日ぶん残すか。**長くすると効くのは周期の検出だけ**で、
    /// 初出の判定は `dns_domains` が受け持つので保持期間に関係なく効く。
    pub dns_retention_days: i64,
    /// 起動時に何日ぶん遡ってドメインの初出を埋めるか。
    /// **0にすると「初日はすべてが初出」になる**ので、既定で30日ぶん遡る
    /// (集計の口を使うので1日1リクエスト・約60KBで済む)。
    pub dns_backfill_days: i64,
}

impl Config {
    pub fn from_env() -> Self {
        // コンテナでは /data を永続化ボリュームにマウントする。
        // ホストで直接動かして開発するときは DATA_DIR=./data のように上書きする
        let data_dir = PathBuf::from(env_string("DATA_DIR", "/data"));
        // ブリッジと共有する場所。既定はデータの置き場の下 ——
        // バックアップ(data/ を丸ごとコピー)に一緒に乗るようにするため
        let state_dir = match env_string("STATE_DIR", "") {
            raw if !raw.trim().is_empty() => PathBuf::from(raw),
            _ => data_dir.join("state"),
        };

        Self {
            pihole_base_url: env_string("PIHOLE_BASE_URL", "http://pihole:80")
                .trim_end_matches('/')
                .to_string(),
            pihole_password: env_string("PIHOLE_PASSWORD", ""),
            pihole_query_limit: env_parse("PIHOLE_QUERY_LIMIT", -1),
            claude_timeout: Duration::from_secs(env_parse("CLAUDE_TIMEOUT", 60)),
            db_path: data_dir.join("monitor.db"),
            claude_bridge_url: env_string("CLAUDE_BRIDGE_URL", "http://bridge:7013/v1")
                .trim_end_matches('/')
                .to_string(),
            chiezo_base_url: env_string("CHIEZO_BASE_URL", "")
                .trim()
                .trim_end_matches('/')
                .to_string(),
            investigate_timeout: Duration::from_secs(env_parse("INVESTIGATE_TIMEOUT", 300)),
            chiezo_timeout: Duration::from_secs(env_parse("CHIEZO_TIMEOUT", 180)),
            state_dir,
            claude_token_path: data_dir.join("claude_token"),
            dns_ingest_enabled: env_bool("DNS_INGEST_ENABLED", true),
            dns_ingest_interval: Duration::from_secs(env_parse("DNS_INGEST_INTERVAL", 300)),
            dns_retention_days: env_parse("DNS_RETENTION_DAYS", 7),
            dns_backfill_days: env_parse("DNS_BACKFILL_DAYS", 30),
        }
    }
}

fn env_string(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

/// 数値の環境変数を読む。空文字や解釈できない値はデフォルトに倒して起動を止めない
/// (`.env` に `PIHOLE_QUERY_LIMIT=` のように書かれていても動かしたいため)。
fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    match env::var(key) {
        Ok(raw) => match raw.trim().parse() {
            Ok(value) => value,
            Err(_) => {
                tracing::warn!(key, value = %raw, "環境変数を数値として解釈できないためデフォルトを使う");
                default
            }
        },
        Err(_) => default,
    }
}

/// 真偽値の環境変数。`false`/`0`/`no` を偽、それ以外(空を含む)を既定に倒す。
fn env_bool(key: &str, default: bool) -> bool {
    match env::var(key) {
        Ok(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "false" | "0" | "no" | "off" => false,
            "true" | "1" | "yes" | "on" => true,
            "" => default,
            other => {
                tracing::warn!(key, value = other, "真偽値として解釈できないためデフォルトを使う");
                default
            }
        },
        Err(_) => default,
    }
}

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
    /// Chiezo 越しの1回の生成の上限。相手は CLI や大きいモデルなので、
    /// **ブリッジ経由(`claude_timeout`)より長めの既定にしてある**。
    pub chiezo_timeout: Duration,
    /// ブリッジと共有するディレクトリ。ここに設定 DB を書き、ブリッジが読み取り専用で読む。
    pub state_dir: PathBuf,
    /// CLI を同梱していた頃のトークンの置き場(移行のためだけに残している)。
    pub claude_token_path: PathBuf,
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
            chiezo_timeout: Duration::from_secs(env_parse("CHIEZO_TIMEOUT", 180)),
            state_dir,
            claude_token_path: data_dir.join("claude_token"),
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

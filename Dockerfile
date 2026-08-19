# 本番用イメージ。実行に必要なのはRustでビルドした実行ファイル1個だけ
# (静的ファイルは include_str! で埋め込み済み)なので、Rustのツールチェーンは
# 最終イメージに持ち込まない。
#
# **「Claudeに聞く」のCLIは同梱しない。** 別コンテナのCLIブリッジ(chiezo-bridge)へ
# HTTPで頼む —— 以前はnpmでclaudeを入れており、CLIで97MB・Nodeで52MB積んでいた
# (アプリ本体は3MB)。CLIの更新もブリッジのコンテナを入れ替えるだけで済む。
#
# ベースイメージのDebianコードネームはビルド側・実行側で揃えること(trixie)。
# 揃っていないとglibcのバージョン差で実行ファイルが動かない。

# ---- ビルド ----
FROM rust:1.97.1-slim-trixie AS builder

WORKDIR /build

# 先に依存だけをビルドして、レイヤーキャッシュに載せる。
# ソースを変えただけのときに依存の再ビルドを避けるため、
# 空のmain.rsで一度ビルドしてから本物のソースをCOPYする
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs && \
    cargo build --release && \
    rm -f target/release/pihole-monitor target/release/deps/pihole_monitor*

COPY src/ ./src/
COPY static/ ./static/
RUN cargo build --release

# ---- 実行 ----
FROM debian:trixie-slim AS runtime

# ca-certificates は PIHOLE_BASE_URL を https:// にした場合に必要
# (rustlsがOSの証明書ストアを読む)。CLIブリッジへの通信も同じクライアントを使う。
#
# iputils-ping / iputils-tracepath は設定画面の「ネットワークの確認」で使う。
# **traceroute ではなく tracepath を入れている** —— このコンテナは非rootで動き、
# traceroute は raw socket (CAP_NET_RAW) が要るので「Operation not permitted」で
# 終わってしまう。tracepath は特権なしで動くように作られている。
# ping が非rootで動くのは、Dockerが既定で net.ipv4.ping_group_range を開けていて、
# iputils の ping が raw socket ではなく ICMP datagram socket を使えるため
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates iputils-ping iputils-tracepath && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/pihole-monitor /usr/local/bin/pihole-monitor

# SQLiteとCLIブリッジ用の設定の置き場。docker-compose.ymlでホスト側のディレクトリを
# マウントする。非rootのuid 1000で動かすため、マウント元も uid 1000 が書ける必要がある
# (所有者合わせは compose の pihole-monitor-init が起動前に行う)
RUN mkdir -p /data/state && chown -R 1000:1000 /data

# 名前つきユーザーは作らない。composeの user: で 1000 以外を指定できる設計なので、
# /etc/passwd に載っていないuidでも動く必要がある(HOMEを使うものはもう無い)
USER 1000:1000

EXPOSE 7060

CMD ["pihole-monitor"]

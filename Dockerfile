# 本番用イメージ。実行に必要なのは
#   1. Rustでビルドした実行ファイル1個(静的ファイルは include_str! で埋め込み済み)
#   2. 「Claudeに聞く」で叩く claude コマンド(Node.js製)
# の2つだけなので、Rustのツールチェーンは最終イメージに持ち込まない。
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
FROM node:24-trixie-slim AS runtime

# ca-certificates: nodeのslimイメージには入っていないが、
#   PIHOLE_BASE_URL を https:// にした場合にrustlsがOSの証明書ストアを読むため必要
# claude: 「Claudeに聞く」でヘッドレス実行するCLI。
#   認証はホストの ~/.claude ではなく /data/claude_token のOAuthトークンで行う
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/* && \
    npm install -g @anthropic-ai/claude-code && \
    npm cache clean --force

COPY --from=builder /build/target/release/pihole-monitor /usr/local/bin/pihole-monitor

# SQLiteとトークンの置き場。docker-compose.ymlでホスト側のディレクトリをマウントする。
# nodeイメージに元からいる非rootユーザー(node, uid/gid 1000)で動かすため、
# マウント元のディレクトリも uid 1000 が書ける必要がある
# (所有者合わせは compose の pihole-monitor-init が起動前に行う)
RUN mkdir -p /data && chown node:node /data

# claudeコマンドが設定を書き込む先。compose の user: で 1000 以外のuidを指定されると
# /etc/passwd に無いユーザーになり HOME が / に落ちて書けなくなるため、
# uidに依存せず書けるディレクトリを用意してHOMEにする(/tmpと同じsticky+全書き込み可)
RUN mkdir -p /home/app && chmod 1777 /home/app
ENV HOME=/home/app

USER node

EXPOSE 6001

CMD ["pihole-monitor"]

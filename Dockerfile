# 本番用イメージ。実行に必要なのはRustでビルドした実行ファイル1個だけ
# (静的ファイルは include_str! で埋め込み済み)なので、Rustのツールチェーンは
# 最終イメージに持ち込まない。
#
# ベースイメージのDebianコードネームはビルド側・実行側で揃えること(trixie)。
# 揃っていないとglibcのバージョン差で実行ファイルが動かない。

# ---- ビルド ----
# ビルドホストのアーキで走らせ、対象アーキへはクロスコンパイルする
# (`--platform=$BUILDPLATFORM` + `--target`)。QEMU での `cargo build` は極端に遅く、
# rusqlite(SQLite本体)と ring(暗号)は C とアセンブラまで含むので、エミュレーションだと
# 数十分かかる。クロスなら arm64 でも amd64 と同じ速さで出せる。
FROM --platform=$BUILDPLATFORM rust:1.97.1-slim-trixie AS builder
ARG BUILDARCH
ARG TARGETARCH

WORKDIR /build

# 対象アーキの Rust ターゲットを足す。アーキが違うときだけ C のクロスコンパイラも入れる。
# C のコンパイラが要るのは cc クレート経由の依存のため —— rusqlite は bundled で SQLite を、
# ring は暗号プリミティブをソースからビルドする。gcc-<arch>-linux-gnu だけでは足りず、
# 対象の libc のヘッダ(libc6-dev-<arch>-cross)も要る —— 無いとホスト側の
# /usr/include を読んで "bits/libc-header-start.h: No such file" で落ちる。
# リンカも指定する(cargo は既定でホストの cc を呼ぶ)。
RUN set -eu; \
    case "${TARGETARCH}" in \
      amd64) target=x86_64-unknown-linux-gnu; pkg="gcc-x86-64-linux-gnu libc6-dev-amd64-cross"; cc=x86_64-linux-gnu-gcc ;; \
      arm64) target=aarch64-unknown-linux-gnu; pkg="gcc-aarch64-linux-gnu libc6-dev-arm64-cross"; cc=aarch64-linux-gnu-gcc ;; \
      *) echo "未対応のアーキ: ${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    echo "$target" > /target; \
    : > /cross-env; \
    if [ "${TARGETARCH}" != "${BUILDARCH}" ]; then \
      apt-get update; \
      apt-get install -y --no-install-recommends $pkg; \
      rm -rf /var/lib/apt/lists/*; \
      printf '[target.%s]\nlinker = "%s"\n' "$target" "$cc" >> /usr/local/cargo/config.toml; \
      printf 'CC_%s=%s\n' "$(echo "$target" | tr - _)" "$cc" > /cross-env; \
    fi; \
    rustup target add "$target"

# 先に依存だけをビルドして、レイヤーキャッシュに載せる。
# ソースを変えただけのときに依存の再ビルドを避けるため、
# 空のmain.rsで一度ビルドしてから本物のソースをCOPYする
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs && \
    env $(cat /cross-env) cargo build --release --target "$(cat /target)" && \
    rm -f "target/$(cat /target)/release/pihole-monitor" \
          "target/$(cat /target)"/release/deps/pihole_monitor*

COPY src/ ./src/
COPY static/ ./static/
# 実行ステージが場所を1つだけ見ればよいように、成果物は target 名を含まない場所へ移す
RUN env $(cat /cross-env) cargo build --release --target "$(cat /target)" && \
    cp "target/$(cat /target)/release/pihole-monitor" /pihole-monitor

# ---- Claude Code の CLI を取り出す ----
# npmの配布物にはプラットフォームごとのネイティブな単一実行ファイルが入っており、
# それ単体で動く(nodeは要らない)ので1ファイルだけ抜き出す —— 配布物ぜんぶ
# (glibc/musl両方 + ラッパー)を積むと最終イメージが倍近くなる。
#
# ビルドホストのアーキで走らせる(`--platform=$BUILDPLATFORM`)。対象アーキの版は
# パッケージ名で選べる(`claude-code-linux-x64` / `-arm64`)ので、arm64向けを作るときも
# エミュレーションは要らない —— やっているのは取得と展開だけ。
#
# バージョンは固定する —— latestだと同じイメージタグでも中身が変わり、
# 「昨日まで動いていた要約が落ちる」を再現できなくなる。上げるときはここを変える。
FROM --platform=$BUILDPLATFORM node:24-slim AS claude-cli
ARG TARGETARCH
ARG CLAUDE_CODE_VERSION=2.1.235
RUN arch="$(echo "${TARGETARCH:-amd64}" | sed 's/amd64/x64/')" && \
    cd /tmp && \
    npm pack "@anthropic-ai/claude-code-linux-${arch}@${CLAUDE_CODE_VERSION}" && \
    tar -xzf "anthropic-ai-claude-code-linux-${arch}-${CLAUDE_CODE_VERSION}.tgz" && \
    mkdir -p /out && cp package/claude /out/claude && chmod +x /out/claude

# ---- 実行 ----
FROM debian:trixie-slim AS runtime

# ca-certificates は PIHOLE_BASE_URL を https:// にした場合に必要
# (rustlsがOSの証明書ストアを読む)。ChiezoやClaude Codeの通信にも要る。
#
# iputils-ping / iputils-tracepath は設定画面の「ネットワークの確認」で使う。
# traceroute ではなく tracepath を入れている —— このコンテナは非rootで動き、
# traceroute は raw socket (CAP_NET_RAW) が要るので「Operation not permitted」で
# 終わってしまう。tracepath は特権なしで動くように作られている。
# ping が非rootで動くのは、Dockerが既定で net.ipv4.ping_group_range を開けていて、
# iputils の ping が raw socket ではなく ICMP datagram socket を使えるため
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates iputils-ping iputils-tracepath && \
    rm -rf /var/lib/apt/lists/*

COPY --from=builder /pihole-monitor /usr/local/bin/pihole-monitor

# Claude Code の CLI を同梱する。 「AIに聞く」「詳しく調べる」をサブスクリプションの
# 枠で回す経路で、アプリがプロセスとして起動する(`src/claude.rs`。トークンは画面から
# 設定し、子プロセスの環境変数で渡す)
COPY --from=claude-cli /out/claude /usr/local/bin/claude

# SQLiteと設定(トークン・CLIのホーム)の置き場。docker-compose.ymlでホスト側の
# ディレクトリをマウントする。非rootのuid 1000で動かすため、マウント元も
# uid 1000 が書ける必要がある(所有者合わせは compose の pihole-monitor-init が起動前に行う)
RUN mkdir -p /data/state && chown -R 1000:1000 /data

# 名前つきユーザーは作らない。composeの user: で 1000 以外を指定できる設計なので、
# /etc/passwd に載っていないuidでも動く必要がある。CLIのホームはイメージに持たせない
# —— uidが変えられる以上、固定のホームを作っても書けるとは限らない。アプリが
# `$STATE_DIR/claude-home` を実行時に作り、子プロセスへ HOME として渡す
USER 1000:1000

EXPOSE 7060

CMD ["pihole-monitor"]

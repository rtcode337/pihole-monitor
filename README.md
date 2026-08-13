# pihole-monitor

Pi-holeでブロックされたドメインを一覧表示し、「未確認」「確認済み」で仕分けて管理するWebアプリ。
Pi-holeの設定（ホワイトリスト等）は一切変更せず、確認状態はローカルDBのみで管理する。

## 主な機能

- ブロック済みドメインの一覧表示（未確認 / 確認済み / すべて でフィルタ）
- ドメインを確認済みにする（任意でメモを残せる）
- 各ドメインについて「Claudeに聞く」ボタンから、Claude Code（別コンテナのCLIブリッジ経由）に問い合わせて、そのドメインが何のためにブロックされていそうかの説明を取得できる
- Claudeの回答を見ながら、そのままダイアログ内でメモを書いて確認済みにできる
- Pi-holeへの接続・APIアクセスに失敗した場合は、その旨を画面に表示する
- スマホのホーム画面に追加すると、アドレスバーなしの単独ウィンドウで開く（アイコン付き）

## セットアップ

```bash
cp .env.example .env
# .env を編集してPi-holeの接続先・パスワードを設定
```

`.env` の主な項目:

| 変数名 | 説明 | デフォルト |
|--------|------|-----------|
| `PIHOLE_BASE_URL` | Pi-holeのURL | `http://pihole:80` |
| `PIHOLE_PASSWORD` | Pi-holeの管理パスワード | 空文字 |
| `PIHOLE_QUERY_LIMIT` | 取得するブロッククエリの件数（`-1`で全件） | `-1` |
| `CLAUDE_TIMEOUT` | Claudeへの問い合わせタイムアウト秒数 | `60` |
| `CLAUDE_BRIDGE_URL` | CLIブリッジ（別コンテナ）のURL | `http://bridge:7013/v1` |
| `STATE_DIR` | ブリッジと共有する設定の置き場 | `<DATA_DIR>/state` |

## 起動

イメージは GitHub Actions が main への push でビルドし、GHCR
（`ghcr.io/rtcode337/pihole-monitor`）へ公開している（`linux/amd64` のみ）。
デプロイ先ではソースからビルドせず、これを pull して動かす。

リポジトリが非公開なのでパッケージも非公開。初回だけデプロイ先で `docker login` が要る
（GitHubで `read:packages` スコープのPersonal Access Token（classic）を発行して使う）。

```bash
# デプロイ先で1回だけ
echo "<Personal Access Token>" | docker login ghcr.io -u <GitHubユーザー名> --password-stdin

# 初回・更新とも共通
docker compose pull && docker compose up -d
```

アクセス: `http://ホストのIP:6001`

> ポート6000ではなく6001。6000はX11用に予約されており、主要ブラウザが
> 「安全でないポート」として接続を拒否する（`ERR_UNSAFE_PORT`）。

`latest` のほかにコミットごとの `sha-<短縮ハッシュ>` タグが付く。特定のコミットに
戻したいときは `.env` に `PIHOLE_MONITOR_IMAGE=ghcr.io/rtcode337/pihole-monitor:sha-1234567`
を書いて `docker compose up -d` する。

データ（`data/`）に書き込むユーザーは既定で `1000:1000`。`id -u` が 1000 以外のホストでは
`.env` に `PIHOLE_MONITOR_UID` / `PIHOLE_MONITOR_GID` を設定する。所有者合わせは起動前に
`pihole-monitor-init` が自動でやるので、`chown` を手で打つ必要はない。

### 手元のソースからビルドする場合

コミット前の変更を試すときは、従来どおり手元でビルドできる（`docker compose build` は
GHCRと同じタグ名で手元のイメージを作り直す。次に `docker compose pull` するとGHCR側の
イメージで上書きされる）。

```bash
docker compose up -d --build
```

`src/` と `static/` はビルド時にイメージへ焼き込まれる（ボリュームマウントではない）ため、コードを変更したあとは `docker compose restart` ではなく **`docker compose up -d --build`** で再ビルドする必要がある。CSSやJSだけを直した場合も実行ファイルに埋め込まれているので再ビルドが要る。

### 開発（ホストで直接動かす）

Rust（1.97以降）が入っていれば、コンテナを経由せずに動かせる。

```bash
DATA_DIR=./data PIHOLE_BASE_URL=http://192.168.1.x:80 PIHOLE_PASSWORD=... cargo run
```

`DATA_DIR` はDBと共有設定の置き場（既定は `/data`。コンテナ用の絶対パスなのでホストでは上書きする）。
「Claudeに聞く」を試す場合はCLIブリッジが要る。composeの `bridge` だけ上げて
`CLAUDE_BRIDGE_URL` をそちらへ向ける。

### リポジトリを置けない環境（NASのコンテナマネージャー等）

`.env` もクローンも置けず、管理画面にYAMLを貼り付けて起動するタイプの環境向けに
[docker-compose.standalone.yml](docker-compose.standalone.yml) を用意している。
`${...}`・`env_file` を使わず値を直書きし、`build:` を持たず、データの置き場を絶対パスで
書いたもの。冒頭の「ここだけ編集」——データディレクトリの絶対パスとPi-holeの接続先——を
書き換えて貼り付ければ起動する。

## Claude連携について

「Claudeに聞く」機能は、**別コンテナのCLIブリッジ**（`bridge`。Claude Code CLIをOpenAI互換の
`/chat/completions` に見せるサイドカー）へHTTPで頼む。**本体のイメージにCLIは入っていない** ——
以前はnpmで同梱していて、CLIとNodeだけで150MB近くあった（アプリ本体は3MB）。
ブリッジはホストへポートを公開しない（認証が無く、トークンも読めるため）。

課金される従量課金APIキーではなく、**`claude setup-token` で発行した長期OAuthトークン**を使う方式を取っている。ホストの`~/.claude`はマウントしない。

- トークンが未設定、または期限切れの場合、「Claude」ボタンを押すとトークン入力ダイアログが表示される
- 表示されるメッセージに従い、ブラウザやターミナルが使える別の端末で `claude setup-token` を実行し、表示されたトークンをダイアログに貼り付けて保存する
- 保存されたトークンは `data/state/settings.db` に入り、ブリッジが読み取り専用で読む（要求のたびに読み直すので、入れ替えてもブリッジの再起動は要らない）
- 以前の `data/claude_token` が残っている環境では、初回の問い合わせ時に自動で移し替える（移した後にファイルは消える）
- トークンが期限切れ等で認証エラーになった場合は自動的に破棄され、次回の「Claude」ボタン押下時に再度入力ダイアログが表示される

## ファイル構成

サーバーはRust（axum + rusqlite）、画面はフレームワークを使わないHTML/CSS/JS。
画面の3ファイルは実行ファイルに埋め込まれるので、配布物はバイナリ1個になる。

```
pihole-monitor/
  Cargo.toml
  src/
    main.rs           # エントリーポイント（設定読み込み・ルーター組み立て）
    config.rs         # 環境変数・定数
    db.rs             # SQLite（reviewed_domains）
    pihole.rs         # Pi-hole v6 API連携
    claude.rs         # CLIブリッジへの問い合わせ・トークン管理
    api.rs            # /api/* のJSONエンドポイント
    pages.rs          # 画面の配信（HTML/CSS/JSを埋め込み）
  static/             # 画面（index.html / css / js）とアイコン・マニフェスト
  scripts/
    gen_icons.py      # アイコンのPNGを static/icon.svg から生成（Python標準ライブラリのみ）
  Dockerfile
  docker-compose.yml            # 通常用（GHCRのイメージをpull。手元ビルドも可）
  docker-compose.standalone.yml # .env・クローンを置けない環境向け（値の直書き）
  .github/workflows/build-and-push-image.yml  # イメージをビルドしてGHCRへpush
  data/               # SQLiteのDBとCLIブリッジ用の設定が入る（コンテナ外に永続化・起動時に自動生成、gitignore対象）
    monitor.db
    state/settings.db # Claudeのトークン（ブリッジが読み取り専用で読む）
```

## 既知の制約・注意点

- Pi-hole v6 API前提。v5以前はAPIが異なるため動作しない
- リクエストごとにPi-holeの認証トークンを取得しているため、Pi-holeへのAPIコールが多い
- 確認済み状態はローカルDBのみで管理。Pi-holeを再インストールしても確認済み情報は維持される
- `claude setup-token` のトークンは長期間有効（発行時点の仕様では約1年）だが、失効した場合は次回の問い合わせ時に再入力が必要になる
- コンテナは非rootユーザーで動く（既定 uid/gid 1000）。所有者合わせは起動前に `pihole-monitor-init` が行う

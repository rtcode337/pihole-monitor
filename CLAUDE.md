# pihole-monitor

Pi-holeでブロックされたドメインを管理するWebアプリ。
ブロック済みドメインを一覧表示し、「未確認」「確認済み」で仕分けできる。
Pi-holeの設定（ホワイトリスト等）は一切変更しない。確認状態はローカルDBのみで管理する。

サーバーはRust（axum + rusqlite）。画面はフレームワーク・ビルドステップなしのHTML/CSS/JS。

## ファイル構成

```
pihole-monitor/
  Cargo.toml                   # 依存とリリースプロファイル
  Cargo.lock                   # 実行ファイルを作るリポジトリなのでコミットする
  src/
    main.rs                      # エントリーポイント。設定読み込み・ルーター組み立て・listen
    config.rs                    # 環境変数・定数の一元管理
    db.rs                        # SQLite操作（reviewed_domainsテーブル）
    pihole.rs                    # Pi-hole v6 API連携
    claude.rs                    # Claude CLI連携・トークン管理
    api.rs                       # /api/* のJSONエンドポイント + AppState
    pages.rs                     # 画面の配信（HTML/CSS/JSを実行ファイルに埋め込み）
  static/                      # 画面。ビルド時に include_str! で埋め込まれる
    index.html                   # HTML骨格（テンプレートエンジンは使っていない）
    css/style.css                # 全スタイル
    js/app.js                    # フロントエンドの全ロジック（vanilla JS + fetch）
  Dockerfile                   # マルチステージ（rust:slim でビルド → node:slim で実行）
  .dockerignore                # .env・data・target・.git等をビルドコンテキストから除外
  docker-compose.yml           # 通常用（GHCRのイメージをpull。手元ビルドも可）
  docker-compose.standalone.yml # .env・クローンを置けない環境向け（値の直書き）
  .github/
    workflows/
      build-and-push-image.yml # イメージをビルドしてGHCRへpush（linux/amd64のみ）
  data/               # SQLiteのDBとClaudeトークンが保存される（コンテナ外に永続化・起動時に自動生成）
    monitor.db
    claude_token
```

### 変更したいことから読むべきファイルを引く表

コンテキスト消費を減らすため、目的のファイルだけを読んで変更すること。他ファイルを横断的に読む必要は基本的にない。

| やりたいこと | 読む/変更するファイル |
|---|---|
| UIの見た目（色・余白など）を変える | `static/css/style.css` |
| フロントエンドの挙動（フィルター・モーダル制御など）を変える | `static/js/app.js` |
| 画面のHTML構造・モーダルの追加を変える | `static/index.html` |
| Pi-hole APIとのやり取り（認証・クエリ取得）を変える | `src/pihole.rs` |
| Claude CLI連携・トークン管理を変える | `src/claude.rs` |
| 確認済みドメインのDB操作・スキーマを変える | `src/db.rs` |
| 既存/新規APIエンドポイントを変える | `src/api.rs` |
| 画面の配信ルート（`/`・静的ファイル）を変える | `src/pages.rs` |
| 環境変数・定数を追加/変更する | `src/config.rs` |
| ルーター登録・起動処理を変える | `src/main.rs` |
| 依存クレートを追加する | `Cargo.toml` |

### 各モジュールの詳細

#### 環境変数（`src/config.rs`）

| 変数名 | 説明 | デフォルト |
|--------|------|-----------|
| `PIHOLE_BASE_URL` | Pi-holeのURL（末尾の`/`は落とす） | `http://pihole:80` |
| `PIHOLE_PASSWORD` | Pi-holeの管理パスワード | 空文字 |
| `PIHOLE_QUERY_LIMIT` | 取得するブロッククエリの件数（`-1`で全件） | `-1` |
| `CLAUDE_TIMEOUT` | Claude CLI呼び出しのタイムアウト秒数 | `60` |
| `DATA_DIR` | DBとトークンの置き場 | `/data` |
| `RUST_LOG` | ログレベル（`tracing_subscriber`のEnvFilter） | `info` |

`DATA_DIR`はホストで直接動かして開発するとき用（`DATA_DIR=./data cargo run`）。
**`.env`には書かないこと** —— `.env`は`env_file`でコンテナにも渡るため、コンテナ内の`/data`（永続化ボリューム）がずれる。

数値の環境変数は、空文字や解釈できない値ならwarnログを出してデフォルトに倒す（`.env`に`PIHOLE_QUERY_LIMIT=`とだけ書かれていても起動する）。

#### Pi-hole API連携（`src/pihole.rs`）

Pi-hole v6のREST APIを使用。リクエストごとにセッショントークン（sid）を取得して使う。
**参照のみ。Pi-holeの設定は変更しない。**

```
POST /api/auth                          # sid取得
GET  /api/queries?upstream=blocklist&length=<PIHOLE_QUERY_LIMIT>
```

HTTPクライアントは`reqwest`（タイムアウト5秒）。TLSは`rustls`を使い、暗号プロバイダは`ring`を明示的に選んでいる（reqwest既定の`aws-lc-rs`はビルドに`cmake`が要るため）。プロバイダの登録は`main.rs`の`install_default()`で行っており、**外すと初回のHTTPS接続で失敗する**。

#### SQLite（`src/db.rs`、`$DATA_DIR/monitor.db`）

確認済みドメインと確認メモを保存するローカルDB。Pi-holeには一切書き込まない。

```sql
reviewed_domains (
    domain TEXT PRIMARY KEY,
    reviewed_at TEXT NOT NULL,  -- RFC3339形式（ローカルタイムゾーンのオフセット付き）
    note TEXT                   -- 確認時のフリーテキストメモ（任意）
)
```

`rusqlite`は同期APIなので、接続1本を`Mutex`で持ち、実際のクエリは`tokio::task::spawn_blocking`に逃がしている（非同期ランタイムのワーカースレッドを塞がないため）。この規模では接続プールは持たない。

#### エンドポイント（`src/pages.rs` / `src/api.rs`）

| メソッド | パス | 説明 |
|---------|------|------|
| GET | `/` | Web UI（`static/index.html`を埋め込みから返す） |
| GET | `/static/css/style.css` / `/static/js/app.js` | 埋め込んだCSS・JS |
| GET | `/api/domains` | ブロック済みドメイン一覧（reviewed・noteフラグ付き）。Pi-hole取得失敗時は502 + `{"error": "pihole_unavailable"}` |
| POST/DELETE | `/api/review` | ドメインを確認済みにする（メモも保存）／未確認に戻す |
| POST | `/api/ask-claude` | 指定ドメインについてClaude CLIに問い合わせ、ブロック理由の説明を取得 |
| POST | `/api/claude-token` | `claude setup-token` で発行したトークンを保存 |

`/api/domains`の並び順は「未確認が先 → 件数の多い順 → ドメイン名の昇順」。3番目のキーは`HashMap`の列挙順が毎回変わるため、表示順を固定する目的で入れている。

#### Claude連携（`src/claude.rs`）

サーバー側で `claude -p "<プロンプト>" --output-format text` を`tokio::process`でヘッドレス実行し、標準出力を回答として返す。

- 認証は`claude setup-token`で発行した長期OAuthトークンを使う方式。ホストの`~/.claude`はマウントしない
- トークンは`$DATA_DIR/claude_token`にプレーンテキストで保存し（保存のたびにパーミッションを600に設定し直す）、subprocess実行時に`CLAUDE_CODE_OAUTH_TOKEN`環境変数として渡す
- トークンが未保存、または`claude`コマンドの出力が認証エラーらしき内容（`AUTH_ERROR_KEYWORDS`でキーワード判定）の場合、`/api/ask-claude`は`{"success": false, "error": "token_required"}`（HTTP 401）を返す。判定に該当した場合は保存済みトークンも削除する。**標準出力・標準エラーの両方を見る**（実際の401は標準出力側に出る）
- フロントエンドは`error === "token_required"`を受け取ると、Claudeモーダルの代わりにトークン入力モーダル（`token-modal`）を開く。ユーザーが手元の端末で`claude setup-token`を実行して得たトークンを貼り付けると`POST /api/claude-token`で保存し、保存成功後に同じドメインで`askClaude()`を自動的に再実行する
- タイムアウトは`CLAUDE_TIMEOUT`環境変数で制御（デフォルト60秒）。`kill_on_drop(true)`を付けているので、タイムアウトでフューチャーを捨てると子プロセスも落ちる
- コンテナには`Dockerfile`の実行ステージでNode.js + `@anthropic-ai/claude-code`をインストールしている

#### フロントエンド（`static/`）

HTML骨格・CSS・JSをファイルごとに分離。vanilla JS + fetch APIで動作。フレームワーク不使用、ビルドステップなし。
3ファイルとも`src/pages.rs`の`include_str!`で実行ファイルに埋め込まれる。**CSSやJSだけを直した場合も`cargo build`が必要**。

主な関数：
- `loadDomains()` - `/api/domains`を叩いてドメイン一覧を取得・表示。失敗時は「Pi-holeからの情報取得に失敗しました」と表示
- `renderDomains()` - フィルター状態に応じてリストを描画
- `openModal(domain)` / `submitReview()` - 確認済みにするモーダルの表示とPOST
- `askClaude(domain)` / `openClaudeModal(domain)` - 「Claudeに聞く」ボタン押下時に`/api/ask-claude`へPOSTし、結果をモーダルに表示
- `submitReviewFromClaudeModal()` - Claudeモーダル内のメモ欄から直接確認済みに登録

## 起動方法

```bash
# デプロイ先：GHCRのイメージをpullして起動（初回のみ docker login ghcr.io が必要）
docker compose pull && docker compose up -d

# 手元のソースから作り直す場合
docker compose up -d --build

# ホストで直接動かす場合（Rust 1.97以降が必要）
DATA_DIR=./data PIHOLE_BASE_URL=http://192.168.1.x:80 PIHOLE_PASSWORD=... cargo run
```

アクセス: `http://ホストのIP:6001`

**ポートは6001**（6000ではない）。6000はX11用に予約されており、主要ブラウザが「安全でないポート」として接続を拒否する（`ERR_UNSAFE_PORT`）。ポートを変えるときは`src/config.rs`の`PORT`と`Dockerfile`の`EXPOSE`、`docker-compose.yml`・`docker-compose.standalone.yml`の`ports`、README・本ファイルのアクセスURLを揃えること。

**注意**: `src/`と`static/`はビルド時にイメージへ焼き込まれる（ボリュームマウントではない）ため、コード変更後は`docker compose restart`ではなく`docker compose up -d --build`で再ビルドしないと反映されない。

### 実行ユーザーとデータディレクトリ

コンテナは非rootユーザー（`node`、uid/gid 1000）で動く。
マウント元のデータディレクトリが uid 1000 で書けないとDBを開けずに起動に失敗するので、
その場合は `chown -R 1000:1000 <データディレクトリ>` する。

## イメージの配布（GHCR / GitHub Actions）

本番の実行形態は「GHCRに置いたイメージをデプロイ先がpullして動かす」。デプロイ先ではソースからビルドしない。

- **ワークフロー**: `.github/workflows/build-and-push-image.yml`
  - トリガー: `main`へのpush（`**.md`・`.gitignore`のみの変更は除外）/ `v*` gitタグ / 手動実行
  - `concurrency`で同一refの古い実行を打ち切る（非公開リポジトリのためActions実行時間・GHCRストレージが無料枠を消費する）
- **公開先**: `ghcr.io/rtcode337/pihole-monitor`
  - タグ: `latest`（mainへのpush時）/ `sha-<短縮SHA>`（毎回）/ `v*` gitタグ名
  - **`linux/amd64`のみ**。arm64のネイティブランナーは公開リポジトリでないと無料枠で使えず、QEMUエミュレーションでは`cargo build`・`npm install -g @anthropic-ai/claude-code`が極端に遅くなるため作らない。**arm64が必要になったらQEMUではなくRustのクロスコンパイル**（`--target aarch64-unknown-linux-gnu`）でamd64ランナーからバイナリを作るほうが速い
  - リポジトリが非公開＝パッケージも非公開。デプロイ先では`read:packages`スコープのPATで`docker login ghcr.io`が必要
- **`docker-compose.yml`**: `image`は`${PIHOLE_MONITOR_IMAGE:-ghcr.io/rtcode337/pihole-monitor:latest}`。`.env`の`PIHOLE_MONITOR_IMAGE`で特定タグへ固定・ロールバックできる。`build: .`は手元ビルド用に残してある
- **`docker-compose.standalone.yml`**: `.env`もクローンも置けない環境（NASのコンテナマネージャー等、管理画面にYAMLを貼り付けるタイプ）向けの単体定義。違いは「`${...}`・`env_file`を使わず値を直書き」「`build:`を持たない」「bindマウントを絶対パスで書く」の3点。編集する値はすべて冒頭の「ここだけ編集」にまとめてある。**`docker-compose.yml`側の設定を変えたらstandalone側にも同じ変更を反映すること**（値の直書きぶん古くなりやすい）

### Dockerfileで気をつける点

- **ビルド側と実行側のDebianコードネームを揃える**（現在は`trixie`）。ずれるとglibcのバージョン差で実行ファイルが動かない
- **実行イメージに`ca-certificates`を入れている**。`node:*-slim`には入っておらず、無いと`PIHOLE_BASE_URL`を`https://`にしたときにrustlsの初期化で落ちる（HTTPクライアント生成時にエラー）
- **依存クレートは空の`main.rs`で一度ビルドして別レイヤーに固める**。`Cargo.toml`/`Cargo.lock`を変えなければ、以降のビルドは自前のクレートだけになる（CIの実行時間を抑えるため）
- イメージの大半（283MB）は`@anthropic-ai/claude-code`とNode.js。Rustの実行ファイル自体は約6.5MB

## 既知の制約・注意点

- Pi-hole v6 API前提。v5以前はAPIが異なるため動作しない
- リクエストごとにトークンを取得しているため、Pi-holeへのAPIコールが多い（認証1回＋データ取得で計2回/リクエスト）
- ブロック済みクエリの取得件数は`PIHOLE_QUERY_LIMIT`環境変数で制御（デフォルト`-1`で全件）。Pi-hole v6 APIのパラメータ名は`length`でデフォルト100件
- 確認済み状態はローカルDBのみで管理。Pi-holeを再インストールしても確認済み情報は維持される
- `claude setup-token`で発行されるトークンは長期間有効（発行時点の仕様では約1年）。期限切れ時は認証エラーを検知してトークンを破棄し、次回のClaudeボタン押下時に再入力を促す
- `data/.gitkeep`は空ディレクトリをgit管理下に置くためのプレースホルダー。古いDocker Engine（Raspberry Pi等）は`volumes: - ./data:/data`のホスト側パスが存在しないとbind mountに失敗して起動できないことがあるため、`git clone`した時点で`data/`が必ず存在するようにしている。`data/`配下の実ファイル（`monitor.db`・`claude_token`）は`.gitignore`で引き続き除外
- テストコードは無い。動作確認はイメージをビルドして起動し、`/api/*`をcurlで叩いて行っている

# pihole-monitor

Pi-holeでブロックされたドメインを管理するWebアプリ。
ブロック済みドメインを一覧表示し、「未確認」「確認済み」で仕分けできる。
Pi-holeの設定（ホワイトリスト等）は一切変更しない。確認状態はローカルDBのみで管理する。

## ファイル構成

```
pihole-monitor/
  app.py                       # エントリーポイント。create_app()を呼ぶだけ
  requirements.txt             # flask, requests
  pihole_monitor/
    __init__.py                  # create_app()。init_db()呼び出し + Blueprint登録
    config.py                    # 環境変数・定数の一元管理
    db.py                        # SQLite操作（reviewed_domainsテーブル）
    pihole_client.py             # Pi-hole v6 API連携
    claude_client.py             # Claude CLI連携・トークン管理
    pages.py                     # Blueprint: GET /
    api.py                       # Blueprint: /api/* のJSONエンドポイント
    templates/
      index.html                  # HTML骨格（Jinja2、動的変数は使用していない）
    static/
      css/style.css               # 全スタイル
      js/app.js                   # フロントエンドの全ロジック（vanilla JS + fetch）
  Dockerfile
  docker-compose.yml
  data/               # SQLiteのDBとClaudeトークンが保存される（コンテナ外に永続化・起動時に自動生成）
    monitor.db
    claude_token
```

### 変更したいことから読むべきファイルを引く表

コンテキスト消費を減らすため、目的のファイルだけを読んで変更すること。他ファイルを横断的に読む必要は基本的にない。

| やりたいこと | 読む/変更するファイル |
|---|---|
| UIの見た目（色・余白など）を変える | `pihole_monitor/static/css/style.css` |
| フロントエンドの挙動（フィルター・モーダル制御など）を変える | `pihole_monitor/static/js/app.js` |
| 画面のHTML構造・モーダルの追加を変える | `pihole_monitor/templates/index.html` |
| Pi-hole APIとのやり取り（認証・クエリ取得）を変える | `pihole_monitor/pihole_client.py` |
| Claude CLI連携・トークン管理を変える | `pihole_monitor/claude_client.py` |
| 確認済みドメインのDB操作・スキーマを変える | `pihole_monitor/db.py` |
| 既存/新規APIエンドポイントを変える | `pihole_monitor/api.py` |
| トップページのルーティングを変える | `pihole_monitor/pages.py` |
| 環境変数・定数を追加/変更する | `pihole_monitor/config.py` |
| Blueprint登録・アプリ初期化を変える | `pihole_monitor/__init__.py` |

### 各モジュールの詳細

#### 環境変数（`pihole_monitor/config.py`）

| 変数名 | 説明 | デフォルト |
|--------|------|-----------|
| `PIHOLE_BASE_URL` | Pi-holeのURL | `http://pihole:80` |
| `PIHOLE_PASSWORD` | Pi-holeの管理パスワード | 空文字 |
| `PIHOLE_QUERY_LIMIT` | 取得するブロッククエリの件数（`-1`で全件） | `-1` |
| `CLAUDE_TIMEOUT` | Claude CLI呼び出しのタイムアウト秒数 | `60` |

#### Pi-hole API連携（`pihole_monitor/pihole_client.py`）

Pi-hole v6のREST APIを使用。リクエストごとにセッショントークン（sid）を取得して使う。
**参照のみ。Pi-holeの設定は変更しない。**

```
POST /api/auth                    # sid取得
GET  /api/queries?upstream=blocklist&length=1000  # ブロック済みクエリ直近1000件
```

#### SQLite（`pihole_monitor/db.py`、`/data/monitor.db`）

確認済みドメインと確認メモを保存するローカルDB。Pi-holeには一切書き込まない。

```sql
reviewed_domains (
    domain TEXT PRIMARY KEY,
    reviewed_at TEXT NOT NULL,  -- ISO8601形式
    note TEXT                   -- 確認時のフリーテキストメモ（任意）
)
```

#### Flaskエンドポイント（`pihole_monitor/pages.py` / `pihole_monitor/api.py`）

| メソッド | パス | 説明 |
|---------|------|------|
| GET | `/` | Web UI（`templates/index.html`をrender_templateで返す） |
| GET | `/api/domains` | ブロック済みドメイン一覧（reviewed・noteフラグ付き）。Pi-hole取得失敗時は502 + `{"error": "pihole_unavailable"}` |
| POST/DELETE | `/api/review` | ドメインを確認済みにする（メモも保存）／未確認に戻す |
| POST | `/api/ask-claude` | 指定ドメインについてClaude CLIに問い合わせ、ブロック理由の説明を取得 |

#### Claude連携（`pihole_monitor/claude_client.py`、`ask_claude_about_domain`）

サーバー側で `claude -p "<プロンプト>" --output-format text` をsubprocessでヘッドレス実行し、標準出力を回答として返す。

- 認証は`claude setup-token`で発行した長期OAuthトークンを使う方式。ホストの`~/.claude`はマウントしない
- トークンは`/data/claude_token`（`./data`は永続化ボリューム、パーミッション600）にプレーンテキストで保存し、subprocess実行時に`CLAUDE_CODE_OAUTH_TOKEN`環境変数として渡す（`get_claude_token` / `save_claude_token` / `clear_claude_token`）
- トークンが未保存、または`claude`コマンドの標準エラーが認証エラーらしき内容（`AUTH_ERROR_KEYWORDS`でキーワード判定）の場合、`/api/ask-claude`は`{"success": false, "error": "token_required"}`（HTTP 401）を返す。判定に該当した場合は保存済みトークンも削除する
- フロントエンドは`error === "token_required"`を受け取ると、Claudeモーダルの代わりにトークン入力モーダル（`token-modal`）を開く。ユーザーが手元の端末で`claude setup-token`を実行して得たトークンを貼り付けると`POST /api/claude-token`で保存し、保存成功後に同じドメインで`askClaude()`を自動的に再実行する
- タイムアウトは`CLAUDE_TIMEOUT`環境変数で制御（デフォルト60秒）
- コンテナには`Dockerfile`でNode.js + `@anthropic-ai/claude-code`をインストールしている

#### フロントエンド（`pihole_monitor/templates/index.html`、`static/css/style.css`、`static/js/app.js`）

HTML骨格・CSS・JSをファイルごとに分離。vanilla JS + fetch APIで動作。フレームワーク不使用、ビルドステップなし。

主な関数：
- `loadDomains()` - `/api/domains`を叩いてドメイン一覧を取得・表示。失敗時は「Pi-holeからの情報取得に失敗しました」と表示
- `renderDomains()` - フィルター状態に応じてリストを描画
- `openModal(domain)` / `submitReview()` - 確認済みにするモーダルの表示とPOST
- `askClaude(domain)` / `openClaudeModal(domain)` - 「Claudeに聞く」ボタン押下時に`/api/ask-claude`へPOSTし、結果をモーダルに表示
- `submitReviewFromClaudeModal()` - Claudeモーダル内のメモ欄から直接確認済みに登録

## 起動方法

```bash
# .envのPIHOLE_BASE_URLとPIHOLE_PASSWORDを環境に合わせて編集してから：
docker compose up -d --build
```

アクセス: `http://ホストのIP:8888`

**注意**: `app.py`と`pihole_monitor/`はビルド時にイメージへCOPYされる（ボリュームマウントではない）ため、コード変更後は`docker compose restart`ではなく`docker compose up -d --build`で再ビルドしないと反映されない。

## 既知の制約・注意点

- Pi-hole v6 API前提。v5以前はAPIが異なるため動作しない
- リクエストごとにトークンを取得しているため、Pi-holeへのAPIコールが多い（認証1回＋データ取得で計2回/リクエスト）
- ブロック済みクエリの取得件数は`PIHOLE_QUERY_LIMIT`環境変数で制御（デフォルト`-1`で全件）。Pi-hole v6 APIのパラメータ名は`length`でデフォルト100件
- 確認済み状態はローカルDBのみで管理。Pi-holeを再インストールしても確認済み情報は維持される
- `claude setup-token`で発行されるトークンは長期間有効（発行時点の仕様では約1年）。期限切れ時は認証エラーを検知してトークンを破棄し、次回のClaudeボタン押下時に再入力を促す

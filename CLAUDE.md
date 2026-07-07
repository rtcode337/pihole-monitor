# pihole-monitor

Pi-holeでブロックされたドメインを管理するWebアプリ。
ブロック済みドメインを一覧表示し、「未確認」「確認済み」で仕分けできる。
Pi-holeの設定（ホワイトリスト等）は一切変更しない。確認状態はローカルDBのみで管理する。

## ファイル構成

```
pihole-monitor/
  app.py              # FlaskアプリとHTML/JSをすべて含む単一ファイル
  Dockerfile
  docker-compose.yml
  data/               # SQLiteのDBが保存される（コンテナ外に永続化・起動時に自動生成）
    monitor.db
```

## app.py の構成

### 環境変数

| 変数名 | 説明 | デフォルト |
|--------|------|-----------|
| `PIHOLE_BASE_URL` | Pi-holeのURL | `http://pihole:80` |
| `PIHOLE_PASSWORD` | Pi-holeの管理パスワード | 空文字 |
| `PIHOLE_QUERY_LIMIT` | 取得するブロッククエリの件数（`-1`で全件） | `-1` |
| `CLAUDE_TIMEOUT` | Claude CLI呼び出しのタイムアウト秒数 | `60` |

### Pi-hole API（v6）

Pi-hole v6のREST APIを使用。リクエストごとにセッショントークン（sid）を取得して使う。
**参照のみ。Pi-holeの設定は変更しない。**

```
POST /api/auth                    # sid取得
GET  /api/queries?upstream=blocklist&length=1000  # ブロック済みクエリ直近1000件
```

### SQLite（/data/monitor.db）

確認済みドメインと確認メモを保存するローカルDB。Pi-holeには一切書き込まない。

```sql
reviewed_domains (
    domain TEXT PRIMARY KEY,
    reviewed_at TEXT NOT NULL,  -- ISO8601形式
    note TEXT                   -- 確認時のフリーテキストメモ（任意）
)
```

### Flaskエンドポイント

| メソッド | パス | 説明 |
|---------|------|------|
| GET | `/` | Web UI（HTMLをrender_template_stringで返す） |
| GET | `/api/domains` | ブロック済みドメイン一覧（reviewed・noteフラグ付き）。Pi-hole取得失敗時は502 + `{"error": "pihole_unavailable"}` |
| POST/DELETE | `/api/review` | ドメインを確認済みにする（メモも保存）／未確認に戻す |
| POST | `/api/ask-claude` | 指定ドメインについてClaude CLIに問い合わせ、ブロック理由の説明を取得 |

### Claude連携（`ask_claude_about_domain`）

サーバー側で `claude -p "<プロンプト>" --output-format text` をsubprocessでヘッドレス実行し、標準出力を回答として返す。

- 認証はAPIキーではなく、`docker-compose.yml`でホストの`~/.claude`・`~/.claude.json`をコンテナにマウントして共有する方式（サブスクリプション利用枠を消費、従量課金なし）
- ホスト側の認証が切れている場合はこの機能も失敗する。ホスト側で`claude`に再ログインすればマウント経由で反映される
- タイムアウトは`CLAUDE_TIMEOUT`環境変数で制御（デフォルト60秒）
- コンテナには`Dockerfile`でNode.js + `@anthropic-ai/claude-code`をインストールしている

### フロントエンド

`app.py`内のHTML文字列にすべて含まれている（テンプレートファイルなし）。
vanilla JS + fetch APIで動作。フレームワーク不使用。

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

**注意**: `app.py`はビルド時にイメージへCOPYされる（ボリュームマウントではない）ため、コード変更後は`docker compose restart`ではなく`docker compose up -d --build`で再ビルドしないと反映されない。

## 既知の制約・注意点

- Pi-hole v6 API前提。v5以前はAPIが異なるため動作しない
- リクエストごとにトークンを取得しているため、Pi-holeへのAPIコールが多い（認証1回＋データ取得で計2回/リクエスト）
- ブロック済みクエリの取得件数は`PIHOLE_QUERY_LIMIT`環境変数で制御（デフォルト`-1`で全件）。Pi-hole v6 APIのパラメータ名は`length`でデフォルト100件
- 確認済み状態はローカルDBのみで管理。Pi-holeを再インストールしても確認済み情報は維持される
- Claude連携はホストの認証情報をコンテナと共有する設計のため、ホストとコンテナで同じClaudeアカウントの利用枠を消費する

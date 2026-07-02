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
| GET | `/api/domains` | ブロック済みドメイン一覧（reviewed・noteフラグ付き） |
| POST | `/api/review` | ドメインを確認済みにする（メモも保存） |

### フロントエンド

`app.py`内のHTML文字列にすべて含まれている（テンプレートファイルなし）。
vanilla JS + fetch APIで動作。フレームワーク不使用。

主な関数：
- `loadDomains()` - `/api/domains`を叩いてドメイン一覧を取得・表示
- `renderDomains()` - フィルター状態に応じてリストを描画
- `openModal(domain)` - 確認済みにするモーダルを表示
- `submitReview()` - `/api/review`にPOSTして確認済みに登録

## 起動方法

```bash
# docker-compose.ymlのPIHOLE_BASE_URLとPIHOLE_PASSWORDを環境に合わせて編集してから：
docker compose up -d --build
```

アクセス: `http://ホストのIP:8888`

## 既知の制約・注意点

- Pi-hole v6 API前提。v5以前はAPIが異なるため動作しない
- リクエストごとにトークンを取得しているため、Pi-holeへのAPIコールが多い（認証1回＋データ取得で計2回/リクエスト）
- ブロック済みクエリの取得件数は`PIHOLE_QUERY_LIMIT`環境変数で制御（デフォルト`-1`で全件）。Pi-hole v6 APIのパラメータ名は`length`でデフォルト100件
- 確認済み状態はローカルDBのみで管理。Pi-holeを再インストールしても確認済み情報は維持される

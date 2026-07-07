# pihole-monitor

Pi-holeでブロックされたドメインを一覧表示し、「未確認」「確認済み」で仕分けて管理するWebアプリ。
Pi-holeの設定（ホワイトリスト等）は一切変更せず、確認状態はローカルDBのみで管理する。

## 主な機能

- ブロック済みドメインの一覧表示（未確認 / 確認済み / すべて でフィルタ）
- ドメインを確認済みにする（任意でメモを残せる）
- 各ドメインについて「Claudeに聞く」ボタンから、サーバー側のClaude Code CLI（ヘッドレス）に問い合わせて、そのドメインが何のためにブロックされていそうかの説明を取得できる
- Claudeの回答を見ながら、そのままダイアログ内でメモを書いて確認済みにできる
- Pi-holeへの接続・APIアクセスに失敗した場合は、その旨を画面に表示する

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

## 起動

```bash
docker compose up -d --build
```

アクセス: `http://ホストのIP:8888`

`app.py` と `pihole_monitor/` はビルド時にDockerイメージへコピーされる（ボリュームマウントではない）ため、コードを変更したあとは `docker compose restart` ではなく **`docker compose up -d --build`** で再ビルドする必要がある。

## Claude連携について

「Claudeに聞く」機能は、コンテナ内にインストールしたClaude Code CLI (`claude`) をヘッドレス（`claude -p ... --output-format text`）で呼び出す。

課金される従量課金APIキーではなく、**`claude setup-token` で発行した長期OAuthトークン**を使う方式を取っている。ホストの`~/.claude`はマウントしない。

- トークンが未設定、または期限切れの場合、「Claude」ボタンを押すとトークン入力ダイアログが表示される
- 表示されるメッセージに従い、ブラウザやターミナルが使える別の端末で `claude setup-token` を実行し、表示されたトークンをダイアログに貼り付けて保存する
- 保存されたトークンは `data/claude_token` に保存され、以後はそのトークンで自動的にClaude連携が動作する
- トークンが期限切れ等で認証エラーになった場合は自動的に破棄され、次回の「Claude」ボタン押下時に再度入力ダイアログが表示される

## ファイル構成

```
pihole-monitor/
  app.py                        # エントリーポイント
  requirements.txt
  pihole_monitor/               # Flaskアプリ本体（機能ごとにモジュール分割）
    __init__.py
    config.py
    db.py
    pihole_client.py
    claude_client.py
    pages.py
    api.py
    templates/index.html
    static/css/style.css
    static/js/app.js
  Dockerfile
  docker-compose.yml
  data/               # SQLiteのDBとClaudeトークンが保存される（コンテナ外に永続化・起動時に自動生成、gitignore対象）
    monitor.db
    claude_token
```

技術的な詳細（APIエンドポイント、DBスキーマ、フロントエンド構成など）は [CLAUDE.md](CLAUDE.md) を参照。

## 既知の制約・注意点

- Pi-hole v6 API前提。v5以前はAPIが異なるため動作しない
- リクエストごとにPi-holeの認証トークンを取得しているため、Pi-holeへのAPIコールが多い
- 確認済み状態はローカルDBのみで管理。Pi-holeを再インストールしても確認済み情報は維持される
- `claude setup-token` のトークンは長期間有効（発行時点の仕様では約1年）だが、失効した場合は次回の問い合わせ時に再入力が必要になる

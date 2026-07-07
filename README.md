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

`app.py` はビルド時にDockerイメージへコピーされる（ボリュームマウントではない）ため、`app.py` を変更したあとは `docker compose restart` ではなく **`docker compose up -d --build`** で再ビルドする必要がある。

## Claude連携について

「Claudeに聞く」機能は、コンテナ内にインストールしたClaude Code CLI (`claude`) をヘッドレス（`claude -p ... --output-format text`）で呼び出す。

課金される従量課金APIキーではなく、**ホストマシンで認証済みのClaude Codeのサブスクリプション（`~/.claude`・`~/.claude.json`）をコンテナにそのままマウントして共有**する方式を取っている（`docker-compose.yml`）。そのため、ホスト側でClaude Codeにログイン済みであれば追加の認証作業なしに動作する。

ホスト側の認証が切れている場合、「Claudeに聞く」は失敗する。その場合はホスト側で `claude` にログインし直せば、マウント経由でコンテナ側にも反映される。

## ファイル構成

```
pihole-monitor/
  app.py              # FlaskアプリとHTML/JSをすべて含む単一ファイル
  Dockerfile
  docker-compose.yml
  data/               # SQLiteのDBが保存される（コンテナ外に永続化・起動時に自動生成、gitignore対象）
    monitor.db
```

技術的な詳細（APIエンドポイント、DBスキーマ、フロントエンド構成など）は [CLAUDE.md](CLAUDE.md) を参照。

## 既知の制約・注意点

- Pi-hole v6 API前提。v5以前はAPIが異なるため動作しない
- リクエストごとにPi-holeの認証トークンを取得しているため、Pi-holeへのAPIコールが多い
- 確認済み状態はローカルDBのみで管理。Pi-holeを再インストールしても確認済み情報は維持される
- Claude連携はホストの認証情報をコンテナと共有する設計のため、ホストとコンテナで同じClaudeアカウントの利用枠を消費する

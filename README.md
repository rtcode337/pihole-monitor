# pihole-monitor

Pi-holeでブロックされたドメインを一覧表示し、「未確認」「確認済み」で仕分けて管理するWebアプリ。
Pi-holeの設定（ホワイトリスト等）は一切変更せず、確認状態はローカルDBのみで管理する。

## 主な機能

- ブロック済みドメインの一覧表示（未確認 / 確認済み / すべて でフィルタ）
- ドメインを確認済みにする（任意でメモを残せる）
- **メモは確認済みにしなくても残せる**（調べただけの段階と、人が確認した段階は別）
- **一覧をまとめてAIに聞いて**、そのドメインが何のためにブロックされていそうかの説明を各ドメインのメモとして残せる（10件ずつ順に聞くので進捗が出る）
- **聞く相手を画面から切り替えられる**（Chiezo に登録してある Claude Code / Codex / … から選ぶ。再起動なしで反映）
- 付いたメモはその場で書き直せる（AIの文章をそのまま残しても、自分の言葉に直しても）
- **ライト / ダークのテーマを右上のボタンで切り替えられる**（未選択ならOSの設定に従う）
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
| `CHIEZO_BASE_URL` | Chiezo（LAN内の知識サーバー）のルートURL。入れると聞く相手を画面から選べる | 空文字（使わない） |
| `CHIEZO_TIMEOUT` | Chiezo越しの生成1回のタイムアウト秒数 | `180` |
| `CLAUDE_TIMEOUT` | CLIブリッジ経由の問い合わせのタイムアウト秒数 | `60` |
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

アクセス: `http://ホストのIP:7060`

> ポートは**7000番台に10刻みで割り当てる運用**の pihole-monitor 枠。
> 6000番台は使わない——6000はX11用に予約されており、主要ブラウザが
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
「AIに聞く」を試す場合は相手が要る。Chiezo が動いていれば `CHIEZO_BASE_URL=http://<ChiezoのIP>:7010`
を渡すだけでよい。CLIブリッジ経由を試すなら composeの `bridge` だけ上げて
`CLAUDE_BRIDGE_URL` をそちらへ向ける。

### リポジトリを置けない環境（NASのコンテナマネージャー等）

`.env` もクローンも置けず、管理画面にYAMLを貼り付けて起動するタイプの環境向けに
[docker-compose.standalone.example.yml](docker-compose.standalone.example.yml) を用意している。
`${...}`・`env_file` を使わず値を直書きし、`build:` を持たず、データの置き場を絶対パスで
書いたもの。**これは雛形**なので、コピーしてから編集する（この形式は値を直書きするので、
実値を入れたファイルはコミット対象から外してある）。

```bash
cp docker-compose.standalone.example.yml docker-compose.standalone.yml
```

コピーした側の冒頭の「ここだけ編集」——データディレクトリの絶対パスとPi-holeの接続先——を
書き換えて貼り付ければ起動する。

## AIに聞く機能について

経路は2つあり、**どちらで聞くかは画面から切り替えられる**（右上の「AI: …」ボタン）。
選択はDB（`data/monitor.db` の `settings` 表）に入るので、**再起動なしで反映される**。

| 経路 | 相手 | 認証 |
|---|---|---|
| **Chiezo**（LAN内の知識サーバー） | Chiezoに登録してある全部（Claude Code / Codex / …） | **不要**（鍵はChiezoが持っている） |
| **CLIブリッジ**（サイドカー） | Claude Code だけ | `claude setup-token` のトークンを画面から登録 |

どちらの経路でも聞く内容（プロンプト）は同じ。結果には**誰が書いたか**が付く
（モデル名まで。「相手の既定に任せる」で頼んだときは、応答が名乗ったモデルを出す）。

### まとめて聞く

聞く入口はこれだけ（ドメイン1件ごとのボタンは置いていない）。
ツールバーの「まとめてAIに聞く」で、**いま一覧に出ているドメインのうちメモの無いもの**を
まとめて聞き、結果をそれぞれのメモとして残す（**確認済みにはしない**）。

- **10件ずつ1回の問い合わせにまとめる。** 1件ずつ聞くと、相手がCLIのときは呼び出しの
  固定費（ハーネスの入力）を件数ぶん払うことになる
- 区切って順に聞くので進捗が出て、**途中で失敗してもそこまでのメモは残る**
  （もう一度実行すると、メモの付いていない残りだけを聞く）
- **既にメモがある行は飛ばす。** 人が書いたメモをAIの文章で上書きしないため
  （聞き直したいときは、その行のメモを空にして保存すると次の実行で対象に戻る）
- メモは1〜2文で書かせている（一覧に並ぶので、長いと読めない）

### Chiezo経由（相手を画面で選ぶ）

`CHIEZO_BASE_URL` に Chiezo の**ルートURL**（`http://192.168.1.x:7010`。**`/v1` は付けない**）を
入れて起動すると、「AI: …」ボタンから相手・モデル・考える量を選べる。
未設定のあいだは選択肢がCLIブリッジ1つだけになる。

- 相手の一覧は Chiezo の `/v1/ai/backends`、生成は `/v1/ai/complete` から取る
  （知識ベースを引く `/v1/chat` は使わない——プロンプトは自前で持っているため）
- 繋がらないときは理由をそのままダイアログに出す。`/v1` を付けると `HTTP 404` が返る
- **同じホストでChiezoが動いているのに届かない**ことがある（コンテナからホストの公開ポートへ
  戻る経路が塞がれている）。そのときはChiezoのネットワークに相乗りして
  `http://chiezo-api:7010` を指す（手順は `docker-compose.yml` のコメント）

### CLIブリッジ経由（Claude Code）

**別コンテナのCLIブリッジ**（`bridge`。Claude Code CLIをOpenAI互換の
`/chat/completions` に見せるサイドカー）へHTTPで頼む。**本体のイメージにCLIは入っていない** ——
以前はnpmで同梱していて、CLIとNodeだけで150MB近くあった（アプリ本体は3MB）。
ブリッジはホストへポートを公開しない（認証が無く、トークンも読めるため）。

課金される従量課金APIキーではなく、**`claude setup-token` で発行した長期OAuthトークン**を使う方式を取っている。ホストの`~/.claude`はマウントしない。

- トークンが未設定、または期限切れの場合、「まとめてAIに聞く」を実行するとトークン入力ダイアログが表示される（Chiezo の相手を選んでいるときは不要）
- 表示されるメッセージに従い、ブラウザやターミナルが使える別の端末で `claude setup-token` を実行し、表示されたトークンをダイアログに貼り付けて保存する
- 保存されたトークンは `data/state/settings.db` に入り、ブリッジが読み取り専用で読む（要求のたびに読み直すので、入れ替えてもブリッジの再起動は要らない）
- 以前の `data/claude_token` が残っている環境では、初回の問い合わせ時に自動で移し替える（移した後にファイルは消える）
- トークンが期限切れ等で認証エラーになった場合は自動的に破棄され、次回の押下時に再度入力ダイアログが表示される

## ファイル構成

サーバーはRust（axum + rusqlite）、画面はフレームワークを使わないHTML/CSS/JS。
画面の3ファイルは実行ファイルに埋め込まれるので、配布物はバイナリ1個になる。

```
pihole-monitor/
  Cargo.toml
  src/
    main.rs           # エントリーポイント（設定読み込み・ルーター組み立て）
    config.rs         # 環境変数・定数
    db.rs             # SQLite（domain_notes / settings）と起動時の移行
    pihole.rs         # Pi-hole v6 API連携
    ai.rs             # 「AIに聞く」の入口（相手の選択・プロンプト・経路の振り分け）
    chiezo.rs         # Chiezoの AI エンドポイント（相手の一覧・生成）
    claude.rs         # CLIブリッジへの問い合わせ・トークン管理
    api.rs            # /api/* のJSONエンドポイント
    pages.rs          # 画面の配信（HTML/CSS/JSを埋め込み）
  static/             # 画面（index.html / css / js）とアイコン・マニフェスト
  scripts/
    gen_icons.py      # アイコンのPNGを static/icon.svg から生成（Python標準ライブラリのみ）
  Dockerfile
  docker-compose.yml            # 通常用（GHCRのイメージをpull。手元ビルドも可）
  docker-compose.standalone.example.yml # .env・クローンを置けない環境向けの雛形（値の直書き）
  .github/workflows/build-and-push-image.yml  # イメージをビルドしてGHCRへpush
  data/               # SQLiteのDBとCLIブリッジ用の設定が入る（コンテナ外に永続化・起動時に自動生成、gitignore対象）
    monitor.db        # ドメインごとのメモ・確認済み、と聞く相手の選択
    state/settings.db # Claudeのトークン（ブリッジが読み取り専用で読む）
```

## 既知の制約・注意点

- Pi-hole v6 API前提。v5以前はAPIが異なるため動作しない
- リクエストごとにPi-holeの認証トークンを取得しているため、Pi-holeへのAPIコールが多い
- 確認済み状態とメモはローカルDBのみで管理。Pi-holeを再インストールしても維持される
- 以前のバージョンの `reviewed_domains` 表は、起動時に `domain_notes` へ自動で移行される（既存の行は確認済みとして扱う）
- `claude setup-token` のトークンは長期間有効（発行時点の仕様では約1年）だが、失効した場合は次回の問い合わせ時に再入力が必要になる。**Chiezo経由を選んでいるあいだは不要**（鍵はChiezoが持っている）
- コンテナは非rootユーザーで動く（既定 uid/gid 1000）。所有者合わせは起動前に `pihole-monitor-init` が行う

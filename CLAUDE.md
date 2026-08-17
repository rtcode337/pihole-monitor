# pihole-monitor

Pi-holeでブロックされたドメインを管理するWebアプリ。
ブロック済みドメインを一覧表示し、「未確認」「確認済み」で仕分けできる。
Pi-holeの設定（ホワイトリスト等）は一切変更しない。確認状態はローカルDBのみで管理する。
一覧を**まとめてAIに聞いて**、ブロック理由をメモとして残せる（聞く相手は画面から切り替えられる）。
**メモは確認済みと独立している** —— 調べただけの段階と、人が確認した段階は別。

サーバーはRust（axum + rusqlite）。画面はフレームワーク・ビルドステップなしのHTML/CSS/JS。

## ファイル構成

```
pihole-monitor/
  Cargo.toml                   # 依存とリリースプロファイル
  Cargo.lock                   # 実行ファイルを作るリポジトリなのでコミットする
  src/
    main.rs                      # エントリーポイント。設定読み込み・ルーター組み立て・listen
    config.rs                    # 環境変数・定数の一元管理
    db.rs                        # SQLite操作（domain_notes / settings テーブル）と起動時の移行
    pihole.rs                    # Pi-hole v6 API連携
    ai.rs                        # 「AIに聞く」の入口。相手の選択・プロンプト・経路の振り分け
    chiezo.rs                    # ChiezoのAIエンドポイント（相手の一覧・素の問い合わせ）
    claude.rs                    # CLIブリッジへの問い合わせ・トークン管理
    api.rs                       # /api/* のJSONエンドポイント + AppState
    pages.rs                     # 画面・アイコンの配信（実行ファイルに埋め込み）
  static/                      # 画面とアイコン。ビルド時に実行ファイルへ埋め込まれる
    index.html                   # HTML骨格（テンプレートエンジンは使っていない）
    css/style.css                # 全スタイル
    js/app.js                    # フロントエンドの全ロジック（vanilla JS + fetch）
    icon.svg                     # アイコン（目のモチーフ）の原本
    icon-32/180/192/512.png      # icon.svgから生成。gen_icons.pyの出力なので手で編集しない
    manifest.webmanifest         # ホーム画面に追加したときの名前・アイコン・表示モード
  scripts/
    gen_icons.py                 # icon.svgと同じ図形を描いてPNGを書き出す（依存なし）
  Dockerfile                   # マルチステージ（rust:slim でビルド → debian:slim で実行）
  .dockerignore                # .env・data・target・.git等をビルドコンテキストから除外
  docker-compose.yml           # 通常用（GHCRのイメージをpull。手元ビルドも可）
  docker-compose.standalone.example.yml # .env・クローンを置けない環境向けの雛形（値の直書き）
  .github/
    workflows/
      build-and-push-image.yml # イメージをビルドしてGHCRへpush（linux/amd64のみ）
  data/               # SQLiteのDBとCLIブリッジ用の設定が入る（コンテナ外に永続化・起動時に自動生成）
    monitor.db        # 確認済みドメイン（reviewed_domains）と画面からの設定（settings）
    state/settings.db # Claudeのトークン（ブリッジが読み取り専用で読む）
```

### 変更したいことから読むべきファイルを引く表

コンテキスト消費を減らすため、目的のファイルだけを読んで変更すること。他ファイルを横断的に読む必要は基本的にない。

| やりたいこと | 読む/変更するファイル |
|---|---|
| UIの見た目（色・余白など）を変える | `static/css/style.css`（色は`:root`のカスタムプロパティ。**ライトは2か所**） |
| テーマ（ライト/ダーク）の切り替えを変える | `static/css/style.css` + `static/js/app.js`（`toggleTheme`）+ `static/index.html`（`<head>`の当て込み） |
| フロントエンドの挙動（フィルター・モーダル制御など）を変える | `static/js/app.js` |
| 画面のHTML構造・モーダルの追加を変える | `static/index.html` |
| アイコン（ファビコン・ホーム画面）を変える | `static/icon.svg` + `scripts/gen_icons.py` |
| Pi-hole APIとのやり取り（認証・クエリ取得）を変える | `src/pihole.rs` |
| AIへの指示文（プロンプト）・聞く相手の振り分けを変える | `src/ai.rs` |
| Chiezoの叩き方（相手の一覧・生成）を変える | `src/chiezo.rs` |
| CLIブリッジへの問い合わせ・トークン管理を変える | `src/claude.rs` |
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
| `CHIEZO_BASE_URL` | Chiezo（LAN内の知識サーバー）の**ルートURL**。`/v1`は付けない。空なら使わない | 空文字 |
| `CHIEZO_TIMEOUT` | Chiezo越しの生成1回のタイムアウト秒数 | `180` |
| `CLAUDE_TIMEOUT` | CLIブリッジ経由の問い合わせのタイムアウト秒数 | `60` |
| `CLAUDE_BRIDGE_URL` | CLIブリッジ（別コンテナ）のURL | `http://bridge:7013/v1` |
| `STATE_DIR` | ブリッジと共有する設定の置き場 | `<DATA_DIR>/state` |
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

ドメインごとの記録と、画面から決める設定を持つローカルDB。Pi-holeには一切書き込まない。

```sql
domain_notes (
    domain     TEXT PRIMARY KEY,
    updated_at TEXT NOT NULL,  -- 最後に更新した時刻（RFC3339・ローカルのオフセット付き）
    note       TEXT,           -- フリーテキストのメモ（任意）
    reviewed   INTEGER NOT NULL DEFAULT 0  -- 1 = 確認済み
)
```

**メモと確認済みは独立している。** かつては表の名前が`reviewed_domains`で、
**行があること = 確認済み**だった —— メモを残すには確認済みにするしかなく、
「まだ判断していないが調べた内容は残したい」（**まとめてAIに聞いた結果がまさにそれ**）を
表せなかった。いまは`reviewed`列で分けてある。

- **書き込みは`upsert`1本**で、`note` / `reviewed` のうち渡されなかった側は**触らない**
  （`COALESCE`）。「メモだけ保存」と「確認済みにする」が互いの値を巻き込まないため
- **未確認に戻してもメモは消さない**（`unmark_reviewed`）。メモと確認済みは別の話なので、
  戻したときに調べた内容まで失われては困る
- **何も持たない行は消す**（`cleanup`。未確認かつメモが空）。残すと`/api/domains`が
  件数0の行として一覧に足してしまう
- **まとめて書くときは1トランザクション**（`save_notes`）。途中で落ちて半分だけ残ると、
  どこまで済んだのか分からなくなる

```sql
settings (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL
)
```

画面から決める設定は`settings`表（key/value）に置く。**環境変数ではなくDBに持つ**のは、
実行のたびに読むので**コンテナを作り直さずに切り替えが効く**ため（いまの中身は
「どのAIに聞くか」＝`ai:selection`の1件だけ。値は`AiChoice`のJSON——相手・モデル・
考える量の3つ組なので、独自の区切り文字で組むとモデル名に区切りが混ざったときに壊れる）。

`rusqlite`は同期APIなので、接続1本を`Mutex`で持ち、実際のクエリは`tokio::task::spawn_blocking`に逃がしている（非同期ランタイムのワーカースレッドを塞がないため）。この規模では接続プールは持たない。

##### 古いDBの移行（`migrate`）

**マイグレーション機構は持たない**（個人運用）。起動時に`migrate`が差分を埋める。
**どの手順も「もう済んでいるか」を見てから実行する**ので、何度起動しても同じ結果になる。

| 手順 | 何をするか |
|---|---|
| 1 | `reviewed_domains` があって `domain_notes` が無ければ**表を改名** |
| 2 | `reviewed_at` があって `updated_at` が無ければ**列を改名**（意味が「確認済みにした時刻」から「最後に更新した時刻」に変わった） |
| 3 | `reviewed` 列が無ければ**既定1で追加**（この列より前の行はすべて確認済みの記録だった） |

**手順3の既定を0にしないこと** —— 既存の記録が全部「メモだけの未確認」に化ける。
実データ（11行）のコピーで、改名2回＋列追加が1度だけ走り、行数と内容が保たれることを確認してある。

#### エンドポイント（`src/pages.rs` / `src/api.rs`）

| メソッド | パス | 説明 |
|---------|------|------|
| GET | `/` | Web UI（`static/index.html`を埋め込みから返す） |
| GET | `/static/css/style.css` / `/static/js/app.js` | 埋め込んだCSS・JS |
| GET | `/static/icon.svg` / `/static/icon-{32,180,192,512}.png` / `/favicon.ico` | アイコン（`/favicon.ico`は32pxのPNGを返す） |
| GET | `/static/manifest.webmanifest` | Webアプリマニフェスト |
| GET | `/api/domains` | ブロック済みドメイン一覧（reviewed・noteフラグ付き）。Pi-hole取得失敗時は502 + `{"error": "pihole_unavailable"}` |
| POST/DELETE | `/api/review` | ドメインを確認済みにする（メモも保存）／未確認に戻す（**メモは残る**） |
| POST | `/api/note` | **メモだけ保存**（確認済みかどうかは変えない） |
| POST | `/api/ask-bulk` | **複数ドメインを1回の問い合わせで**聞き、結果をメモとして保存（`MAX_BULK_DOMAINS`件まで）。書き手の名前（`author`）と答えの返らなかったドメイン（`missing`）も返す |
| GET | `/api/ai` | 選べる相手の一覧・選択・Chiezoの URL・繋がらない理由 |
| POST | `/api/ai` | 聞く相手を保存（`backend`が空・未指定ならCLIブリッジ経由に戻す） |
| POST | `/api/claude-token` | `claude setup-token` で発行したトークンを保存 |

**パスに相手の名前を入れない**（かつては`/api/ask-claude`だった）。答える相手は画面から
切り替えられるので、Chiezo越しのCodexに聞いたときに嘘になる。

**1件ずつ聞く口は持たない。** 行ごとの「AIに聞く」ボタンをやめたので、聞くのは
まとめて聞くときだけ —— **呼ばれない口を残すと、画面から辿れない機能が文書にだけ残る**。
特定の1件を聞き直したいときは、**その行のメモを空にして保存すれば**次の実行で対象に戻る。

`/api/domains`の並び順は「未確認が先 → 件数の多い順 → ドメイン名の昇順」。3番目のキーは`HashMap`の列挙順が毎回変わるため、表示順を固定する目的で入れている。

#### 「AIに聞く」の振り分け（`src/ai.rs`）

**どの AI に聞くかを1か所で決め、プロンプトも1か所に置く。** 経路は2つあり、
**選択はDBに持つので再起動なしで切り替わる**（`settings`表の`ai:selection`）。

| 経路 | 相手 | 選ばれる条件 |
|---|---|---|
| Chiezo（`src/chiezo.rs`） | Chiezoに登録してある全部（Claude Code / Codex / …） | 画面で相手を選んだとき |
| CLIブリッジ（`src/claude.rs`） | Claude Codeだけ | Chiezo未設定、または相手を選んでいないとき |

- **指示文（`SYSTEM_PROMPT`）は経路で変えない。** 相手を切り替えたときに変わるのは
  書き手だけで、聞いていることまで変わると読み比べにならない
- **回答には書き手の名前（`author`）を付けて返す。** 相手を切り替えられる以上、
  これが無いと画面はどのAIの答えを出しているのか言えない。Chiezo経由では
  **応答が名乗ったモデル**を優先する（「相手の既定に任せる」で頼んだときに、
  何が書いたのかを知る手がかりはそれだけ）
- **選択が残っているのにChiezoが未設定（URLを外した）なら、黙ってブリッジへ倒す。**
  答えが出ないより、従来の経路で答えが出るほうがよい
- **表示名は選んだ時点の表記を保存する**（`AiChoice.label`）。表示のたびにChiezoへ
  問い合わせると、繋がらない日にボタンの名前が消える
- トークンが要るのは**CLIブリッジ経由のときだけ**（`token_required`はそこからしか返らない）。
  Chiezoは鍵を自分で持っている
- **経路（Chiezo / CLIブリッジ）の分岐は`complete()`1か所**

##### まとめて聞く（`ask_about_domains`）

**聞くのはこの経路だけ**（行ごとのボタンは無い）。**1件ずつ聞かない。** 相手がCLIだと呼び出し1回の固定費（ハーネスの入力）が大きく、
47件を1件ずつ聞くと固定費を47回払う。**まとめて渡すこと自体が対策**
（tech-antennaで測ったときは1件だけ渡すと32,300トークン、5件まとめると1件あたり6,584だった）。

- 出力は**一覧に並ぶメモ**なので1〜2文に抑えさせる（3〜5行あると読めない）
- **番号で対応を返させる**（`{"results":[{"n":1,"note":"…"}]}`）。ドメイン名を書き写させると
  綴りが揺れて突き合わせできない。**範囲外の番号と空のメモは捨てる**（LLMの応答をそのまま信じない）
- **前置きとコードフェンスを許して読む**（`extract_json`。最初の`{`から最後の`}`まで）。
  「JSONだけ」と指示しても説明を1行添えてくる応答はあり、そこで丸ごと捨てるとそのぶんのメモが消える
- **1回に渡すのは`MAX_BULK_DOMAINS`（10）件まで**。多いと応答が崩れ、時間もかかる
  （tech-antennaでは200件を1回に詰めて300秒のタイムアウトを超え、まとめて失敗した）。
  **区切って何度も呼ぶのは画面側**（`BULK_CHUNK`。同じ値にすること）—— 1リクエストを短く保つと
  進捗が出せて、**途中で失敗してもそこまでのメモは残る**
- **書き戻しでは確認済みにしない。** 調べただけの段階と、人が確認した段階は別

#### Chiezo連携（`src/chiezo.rs`）

**鍵を持たずに複数のAIを使えるようにするための経路。** 相手の認証情報はChiezoが握っていて、
こちらは「どの相手に投げるか」を指定するだけでよい——サイドカーのCLIブリッジは
Claude Code 1つしか包めないので、**相手を選べる経路はここだけ**。

```
GET  /v1/ai/backends   # 話せる相手・モデル・考える量（待ちは15秒。相手は即答する）
POST /v1/ai/complete   # 素の問い合わせ（backend / model / effort / messages）
```

- **`/v1/chat` は使わない。** あちらは知識ベースを引いて答える口で、必ず抽出が混ざる ——
  こちらはプロンプトを自前で持っているので邪魔になる（トークンも余分に使う）
- URLは**ルートまで**を受け取り、`/v1/ai/...` はこちらで足す。**`/v1` を付けて渡すと404**
  （画面にそのまま `HTTP 404` が出るので、そこで気づける）
- **こちらの待ちは相手より30秒長くする**（`CHIEZO_TIMEOUT` + 30）。先に切れると理由が分からない
- **応答が無い理由は特定しない。** 相手が居ないだけの接続も、環境によっては拒否されずに
  待たされる（この開発ホストでは閉じているlocalhostのポートへの接続が15秒待って切れた）ので、
  「ポート違い」と「パケットが落ちている」をタイムアウトで見分けることはできない ——
  **両方を文面に書いて確かめる先を示す**。届いた場合（HTTPエラー）だけは相手の設定の話だと言える
- **実在しない相手・モデル・考える量は保存を断る**（`/api/ai` のPOSTが一覧と突き合わせる）。
  黙って保存すると、次に聞いたときまで間違いに気づけない。
  **CLIブリッジへ戻すときはChiezoに問い合わせない** —— 繋がらなくても戻せる必要がある

#### Claude連携（`src/claude.rs`）

**CLIは同梱しない。** 別コンテナのCLIブリッジ（chiezoリポジトリの`chiezo-bridge`。Claude CodeをOpenAI互換の`/chat/completions`に見せるサイドカー）へHTTPで頼み、応答の本文をそのまま回答として返す。以前はnpmで`@anthropic-ai/claude-code`を同梱しており、**CLIで97MB・Nodeで52MB**積んでいた（アプリ本体は3MB）。CLIの更新はブリッジのコンテナを入れ替えるだけで済む。

- 認証は`claude setup-token`で発行した長期OAuthトークンを使う方式。ホストの`~/.claude`はマウントしない
- **トークンは共有ディレクトリの設定DB（`$STATE_DIR/settings.db`の`provider_settings`表）に書く**。CLIは別コンテナで動くので環境変数では渡せない。ブリッジはこれを**読み取り専用でマウントして、要求のたびに読み直す**（入れ替えても再起動が要らない）。**WALにしない** —— 読み取り専用マウントでは`-shm`を作れず開けなくなる。**パーミッションは絞らない**（ブリッジはuid 1000固定で、本体の実行ユーザーはホストに合わせて変えられるため）
- **旧`$DATA_DIR/claude_token`は初回に設定DBへ移して消す**。更新した環境でトークンの入れ直しを求めないため
- トークンが未保存、またはブリッジが401（認証情報を読めていない）／応答が認証エラーらしき内容（`AUTH_ERROR_KEYWORDS`でキーワード判定。CLI自身の認証エラーは502の本文に出る）の場合、`/api/ask-bulk`は`{"success": false, "error": "token_required"}`（HTTP 401）を返す。判定に該当した場合は保存済みトークンも無効化する
- **待ちはブリッジより30秒長くする**。先に切れると「ブリッジが何秒で諦めたか」（504と経過秒数）が分からなくなる
- ブリッジは**ホストへポートを公開しない**。認証が無いうえにトークンを読めるので、外から触れるとそのままAIを使われる
- フロントエンドは`error === "token_required"`を受け取ると、まとめて聞くのを中断してトークン入力モーダル（`token-modal`）を開く。ユーザーが手元の端末で`claude setup-token`を実行して得たトークンを貼り付けると`POST /api/claude-token`で保存する。**保存の副作用で聞き直しは走らせない**（LLMの枠を使う操作なので、対象と件数を出すモーダルに戻して押し直させる）
- **プロンプトは受け取る**（`ask(system, user)`）。相手がChiezo越しでも同じ文言になるよう、指示文は`ai.rs`が持つ
- タイムアウトは`CLAUDE_TIMEOUT`環境変数で制御（デフォルト60秒）。`kill_on_drop(true)`を付けているので、タイムアウトでフューチャーを捨てると子プロセスも落ちる

#### 見た目とテーマ（`static/css/style.css`）

**色は全部`:root`のカスタムプロパティにまとめてある。個々の規則に生の色を書かないこと**
——1か所で調整できなくなるし、ライト/ダークの片方だけ直してしまう。
トークンはダークの既存の値から1:1で起こしたので、**ダークの見た目は変わっていない**。

テーマの決まり方は3通り:

| 状態 | どう決まるか |
|---|---|
| 明示的に選んだ | `html[data-theme="light\|dark"]`（`localStorage`の`theme`に残る） |
| 選んでいない | OSの設定（`prefers-color-scheme`）に従う |
| どちらも無い | `:root`の値（= ダーク。このアプリの既定の見た目） |

- **ライトの値は2か所に書いてある**（OS追従用のメディアクエリと、明示指定用の属性セレクタ）。
  CSSでは「メディアクエリ or 属性」を1つの規則にまとめられないため。**片方だけ直さないこと**
  （`light-dark()`を使えば1か所で済むが、対応ブラウザがまだ新しく、効かない環境では色が壊れる）
- **`color-scheme`もテーマごとに指定する**。指定しないと`textarea`・`select`・チェックボックスなど
  ブラウザが描く部品だけ元のテーマのまま残る
- **当て込みは`<head>`のインラインスクリプトでやる**（`static/index.html`）。
  app.jsは`</body>`の直前で読み込まれるので、そこで当てると**一瞬もう一方の色が出る**
- **ボタンに出すのは「切り替わる先」の印**（ダークなら太陽）。いまの状態を出すと
  「押したらどうなるか」が読めない
- `theme-color`のメタも切り替える（合わせないとアドレスバーの帯だけ暗いまま残る）

#### フロントエンド（`static/`）

HTML骨格・CSS・JSをファイルごとに分離。vanilla JS + fetch APIで動作。フレームワーク不使用、ビルドステップなし。
3ファイルとも`src/pages.rs`の`include_str!`で実行ファイルに埋め込まれる。**CSSやJSだけを直した場合も`cargo build`が必要**。

主な関数：
- `loadDomains()` - `/api/domains`を叩いてドメイン一覧を取得・表示。失敗時は「Pi-holeからの情報取得に失敗しました」と表示
- `renderDomains()` - フィルター状態に応じてリストを描画
- `filteredDomains()` - いまのフィルターで画面に出ているドメイン。**描画と「まとめて聞く」の対象が
  同じ1か所から出る**ようにしてある（食い違うと、見えていない行にメモが付く）
- `openModal(domain, note)` / `submitNote()` / `submitReview()` - メモのモーダル。**鉛筆ボタンは全行に出す**。
  **メモの保存と確認済みは別のボタン**。確認済みの行には「確認済みにする」を出さない
- `toggleTheme()` / `renderTheme()` - テーマの切り替えとボタンの印。上の「見た目とテーマ」を参照
- `openBulkModal()` / `bulkTargets()` / `runBulkAsk()` - まとめてAIに聞く。
  対象は**いま出ている行のうちメモの無いもの**（**既にメモがある行は飛ばす** ——
  人が書いたメモをAIの文章で上書きしないため。聞き直したいときは、その行のメモを空にして保存する）。
  `BULK_CHUNK`件ずつ順に投げ、進捗と結果を1行ずつ積む。**1チャンクの失敗で止めない**
  （相手が一度崩れた応答を返しても残りは聞ける）。実行中はモーダルを閉じさせない
  （閉じても処理は止まらないが、進捗が見えなくなる）
- **行に出すバッジは「確認済」だけ。** 未確認の行に「NEW」を出していたのをやめた ——
  **既読管理はしていないので嘘になる**（出していたのは「まだ確認済みにしていない」だけで、
  それは左の赤い点とバッジの有無で足りる）
- `loadAi()` / `renderAiButton()` - `/api/ai`を読み、ツールバーのボタンに**いま聞く相手の名前**を出す。
  歯車アイコンだけにしないのは、押す前にどのAIが答えるのかを知りたいため
- `openAiModal()` / `renderAiList()` / `saveAiSelection()` - 相手を選ぶモーダル。
  **先頭は常にCLIブリッジの行**（Chiezoが落ちている日にも聞けるよう、選択肢から消さない）。
  `select`は`label`の**外**に置く（中に入れると、ドロップダウンを触るだけでラジオが動く）。
  **保存の失敗理由はモーダルに残す**（閉じてしまうと読めない）。
  モデル・考える量をいじったらその行のラジオを立てる——**リスナーは入れ物（`#ai-list`）に
  1回だけ付ける**（一覧は`innerHTML`で差し替えるため）

#### アイコン（`static/icon.svg`・`scripts/gen_icons.py`）

「監視」を表す目のモチーフ。背景`#0d1117`・まぶた`#c9d1d9`・瞳`#f85149`（ヘッダーの赤いドットと同色）。

- **原本は`static/icon.svg`**。まぶたは半径24.375の円弧2本が作るレンズ形で、瞳はグロー付きの円
- **PNGは`python3 scripts/gen_icons.py`で生成する**（32/180/192/512px、生成物もコミットする）。iOSのapple-touch-iconもAndroidのマニフェストもSVGを受け付けないため必要。PillowやcairosvgではなくPython標準ライブラリだけで図形を直接ラスタライズしている（依存を増やさないため）
- **SVGとスクリプトの図形は自動同期しない**。形を変えるときは`icon.svg`と`gen_icons.py`冒頭の定数を両方直し、スクリプトを再実行すること
- マニフェストの512pxは`purpose: "any maskable"`。Androidのマスク（中央80%の円）に収まるよう、絵柄は中心から半径22.75/32以内に収めてある
- `<head>`のリンクは`static/index.html`にある（`icon` / `apple-touch-icon` / `manifest` / `theme-color`）

## 起動方法

```bash
# デプロイ先：GHCRのイメージをpullして起動（初回のみ docker login ghcr.io が必要）
docker compose pull && docker compose up -d

# 手元のソースから作り直す場合
docker compose up -d --build

# ホストで直接動かす場合（Rust 1.97以降が必要）
DATA_DIR=./data PIHOLE_BASE_URL=http://192.168.1.x:80 PIHOLE_PASSWORD=... cargo run

# 「AIに聞く」でChiezoの相手を選べるようにする（CLIブリッジを上げなくても試せる）
DATA_DIR=./data CHIEZO_BASE_URL=http://192.168.1.x:7010 cargo run
```

アクセス: `http://ホストのIP:7060`

**ポートは7060。** **7000番台に10刻みで割り当てる運用**（末尾0 = ブラウザで開くもの）の
pihole-monitor 枠で、**ホスト側とコンテナ内で同じ番号**にしてある（composeの`"7060:7060"`を
読むだけで対応が分かるようにするため）。
**6000番台は使わない** —— 6000はX11用に予約されており、主要ブラウザが「安全でないポート」として
接続を拒否する（`ERR_UNSAFE_PORT`）。かつて6001を使っていたのはこれを避けるためだった。
ポートを変えるときは`src/config.rs`の`PORT`と`Dockerfile`の`EXPOSE`、
`docker-compose.yml`・`docker-compose.standalone.example.yml`の`ports`、README・本ファイルの
アクセスURLを揃えること。

**注意**: `src/`と`static/`はビルド時にイメージへ焼き込まれる（ボリュームマウントではない）ため、コード変更後は`docker compose restart`ではなく`docker compose up -d --build`で再ビルドしないと反映されない。

### 実行ユーザーとデータディレクトリ

コンテナは非rootユーザーで動く（イメージ内の既定は `node`、uid/gid 1000）。
マウント元のデータディレクトリが同じuidで書けないとDBを開けずに起動に失敗する。

**所有者合わせは手作業ではなく、`pihole-monitor-init` サービスが起動前に行う。**
本体と同じイメージをrootで1回だけ起動し、`chown -R` してすぐ終了する
（`depends_on` の `service_completed_successfully` で本体を待たせる）。
bindマウント先がホストに無くてDockerがroot所有で作った場合、rootで動いていた
旧イメージのデータを引き継ぐ場合、バックアップから別の所有者で復元した場合の
いずれもここで吸収される。

`id -u` が1000以外のホストでは、`.env` の `PIHOLE_MONITOR_UID` / `PIHOLE_MONITOR_GID`
（standaloneは冒頭の `x-run-as`）で合わせる。**chown先と `user:` は同じ値を参照させること**
（片方だけ直すと起動しなくなる）。

uidを1000以外にできる設計なので、**`/etc/passwd`に載っていないuidでも動く必要がある**。
`HOME`を使うものはもう無い（CLIを外したため）が、共有設定のパーミッションを
所有者だけに絞れないのはこれが理由（ブリッジはuid 1000固定で読みに来る）。

## イメージの配布（GHCR / GitHub Actions）

本番の実行形態は「GHCRに置いたイメージをデプロイ先がpullして動かす」。デプロイ先ではソースからビルドしない。

- **ワークフロー**: `.github/workflows/build-and-push-image.yml`
  - トリガー: `main`へのpush（`**.md`・`.gitignore`のみの変更は除外）/ `v*` gitタグ / 手動実行
  - `concurrency`で同一refの古い実行を打ち切る（非公開リポジトリのためActions実行時間・GHCRストレージが無料枠を消費する）
- **公開先**: `ghcr.io/rtcode337/pihole-monitor`
  - タグ: `latest`（mainへのpush時）/ `sha-<短縮SHA>`（毎回）/ `v*` gitタグ名
  - **GHCRに残るのは最新の1版だけ**。push後に古い版を削除している（GHCRのストレージ枠はアカウント全体で共有で、超えると課金ではなくpushがブロックされる）。そのぶん過去のイメージには戻せない。世代を残すならworkflowの`min-versions-to-keep`を上げる
  - **`linux/amd64`のみ**。arm64のネイティブランナーは公開リポジトリでないと無料枠で使えず、QEMUエミュレーションでは`cargo build`が極端に遅くなるため作らない。**arm64が必要になったらQEMUではなくRustのクロスコンパイル**（`--target aarch64-unknown-linux-gnu`）でamd64ランナーからバイナリを作るほうが速い
  - リポジトリが非公開＝パッケージも非公開。デプロイ先では`read:packages`スコープのPATで`docker login ghcr.io`が必要
- **`docker-compose.yml`**: `image`は`${PIHOLE_MONITOR_IMAGE:-ghcr.io/rtcode337/pihole-monitor:latest}`。`.env`の`PIHOLE_MONITOR_IMAGE`で特定タグへ固定できる（**ただしGHCRには最新の1版しか残らないので、過去の版へは戻せない**）。`build: .`は手元ビルド用に残してある。サービスは3つで、`pihole-monitor-init`（所有者合わせ）・本体・`bridge`（Claude CodeのCLIを動かすサイドカー。**公開パッケージ**なのでdocker login不要）。**initも本体と同じイメージ・同じタグを参照しているので、pullもビルドも増えない**（2つ目はキャッシュに当たる）
- **`docker-compose.standalone.example.yml`**: `.env`もクローンも置けない環境（NASのコンテナマネージャー等、管理画面にYAMLを貼り付けるタイプ）向けの単体定義。違いは「`${...}`・`env_file`を使わず値を直書き」「`build:`を持たない」「bindマウントを絶対パスで書く」の3点。編集する値はすべて冒頭の「ここだけ編集」（データの置き場・Pi-holeの接続設定・実行ユーザー）にまとめてある。`chown`先と`user:`はどちらも`x-run-as`アンカーを参照させて、片方だけ直す事故を防いでいる。**`docker-compose.yml`側の設定を変えたらstandalone側にも同じ変更を反映すること**（値の直書きぶん古くなりやすい）
  - **リポジトリに置くのは`.example`の付いた雛形だけ。** 実値を入れてコピーした`docker-compose.standalone.yml`は`.gitignore`してある（`.env.example`と`.env`の関係と同じ。**この形式は値を直書きするので、追記した瞬間にPi-holeのパスワードがコミット対象に入る**）
  - **Chiezoのネットワークに相乗りする設定は、現物の位置にコメントアウトで置いてある**（サービスの`#networks: [default, chiezo]`とファイル末尾の`#networks:`）。**使うときはコメントを外すだけ** —— 手順を散文で書くと、貼り付ける側がインデントを組み直すことになる。既定で外してあるのは、`external: true`が「そのネットワークが既に在ること」を前提にするため（Chiezoを同じホストで動かしていない環境で有効にすると起動できない）

### Dockerfileで気をつける点

- **ビルド側と実行側のDebianコードネームを揃える**（現在は`trixie`）。ずれるとglibcのバージョン差で実行ファイルが動かない
- **実行イメージに`ca-certificates`を入れている**。`debian:*-slim`には入っておらず、無いと`PIHOLE_BASE_URL`を`https://`にしたときにrustlsの初期化で落ちる（HTTPクライアント生成時にエラー）
- **依存クレートは空の`main.rs`で一度ビルドして別レイヤーに固める**。`Cargo.toml`/`Cargo.lock`を変えなければ、以降のビルドは自前のクレートだけになる（CIの実行時間を抑えるため）
- **実行ステージはNodeではなく`debian:trixie-slim`**。CLIを外したので、載せるのはRustの実行ファイル（約6.5MB）と`ca-certificates`だけになった（圧縮後で約182MB → 約34MB）

## 既知の制約・注意点

- Pi-hole v6 API前提。v5以前はAPIが異なるため動作しない
- リクエストごとにトークンを取得しているため、Pi-holeへのAPIコールが多い（認証1回＋データ取得で計2回/リクエスト）
- ブロック済みクエリの取得件数は`PIHOLE_QUERY_LIMIT`環境変数で制御（デフォルト`-1`で全件）。Pi-hole v6 APIのパラメータ名は`length`でデフォルト100件
- 確認済み状態はローカルDBのみで管理。Pi-holeを再インストールしても確認済み情報は維持される
- `claude setup-token`で発行されるトークンは長期間有効（発行時点の仕様では約1年）。期限切れ時は認証エラーを検知してトークンを破棄し、次回の「AIに聞く」押下時に再入力を促す。**Chiezoの相手を選んでいるあいだはトークンを使わない**（鍵はChiezoが持っている）
- `data/.gitkeep`は空ディレクトリをgit管理下に置くためのプレースホルダー。古いDocker Engine（Raspberry Pi等）は`volumes: - ./data:/data`のホスト側パスが存在しないとbind mountに失敗して起動できないことがあるため、`git clone`した時点で`data/`が必ず存在するようにしている。`data/`配下の実ファイル（`monitor.db`・`state/settings.db`）は`.gitignore`で引き続き除外
- テストコードは無い。動作確認はイメージをビルドして起動し、`/api/*`をcurlで叩いて行っている

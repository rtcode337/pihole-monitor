# 構成図

モジュールの関係と、処理の流れを図にした文書。**コードを読む前に「どこに何があるか」を
掴むため**に置いている。各モジュールの中の決めごとは [`CLAUDE.md`](../CLAUDE.md) にある。

## モジュールの関係

```mermaid
classDiagram
    class main {
        +main() 設定を読む・DBを開く・ルーターを組む・取り込みを起こす
    }
    class config {
        +Config
        +from_env() 環境変数を1か所で読む
    }
    class AppState {
        +Db db
        +PiholeClient pihole
        +Ai ai
    }
    class pages {
        +router() 画面とアイコンを配信（実行ファイルに埋め込み）
    }
    class api {
        +/api/domains ブロック済みの一覧（＋アクセス元・期間）
        +/api/watch 未ブロックの怪しい候補
        +/api/watch/baseline 監視の基準日時
        +/api/review 確認済みにする
        +/api/note メモ
        +/api/notes メモの残る全ドメイン（控え）
        +/api/clients アクセス元ごとの日ごとの件数
        +/api/ask まとめて聞く
        +/api/investigate 1件を詳しく調べる
        +/api/followup 調査結果をもとに追加で聞く
        +/api/diag 疎通の確認（ping / 経路）
        +/api/ai 相手の一覧と選択
    }
    class diag {
        +run() シェルを通さず外部コマンドを1つ
        +validate_target() ホスト名とIPの文字だけ
    }
    class ingest {
        +run() 定期取り込みを回し続ける
        +backfill() 遡ってドメインの初出を埋める
    }
    class watch {
        +candidates() 怪しい候補を5つの手で拾う
    }
    class Db {
        +records() 判断（メモ・判定・調査結果）
        +insert_queries() 取り込んだクエリ
        +domain_activity_since() 件数・端末ごとの件数と期間
        +domain_profile() 1件の観測データ
    }
    class PiholeClient {
        +blocked_domains()
        +queries_since()
        +domain_counts()
        -sid 使い回すセッション
    }
    class Ai {
        +ask_about_domains() 選んだ全員に短く
        +investigate() メイン1人に深く
        +follow_up() 調査結果を材料にもう一歩
        +primary_target()
    }
    class ChiezoClient {
        +backends() 話せる相手
        +complete() 1往復（webの可否つき）
    }
    class ClaudeClient {
        +ask_within() 同梱のCLIを起動
        +load_token()
    }

    main --> config
    main --> AppState
    main --> ingest
    AppState --> Db
    AppState --> PiholeClient
    AppState --> Ai
    api --> AppState
    api --> watch
    api --> diag
    pages ..> main : ルーターに合流
    ingest --> Db
    ingest --> PiholeClient
    watch --> Db
    Ai --> ChiezoClient
    Ai --> ClaudeClient
    Ai --> Db : 相手の選択
```

**外部に出ていくのは `PiholeClient` / `ChiezoClient` / `ClaudeClient` の3つだけ。**
ブラウザは Pi-hole も Chiezo も直接は見ない（`/api/*` だけを叩く）——
Pi-hole のパスワードをブラウザに配らないため。

## 起動してから

```mermaid
sequenceDiagram
    participant M as main
    participant D as Db
    participant I as ingest（別タスク）
    participant P as Pi-hole
    participant B as ブラウザ

    M->>D: open（テーブル作成 + 古いDBの差分を埋める）
    M->>I: spawn（画面の応答を待たせない）
    M->>M: 7060 で待ち受け

    loop 起動直後 → 以後 DNS_INGEST_INTERVAL ごと
        I->>D: 遡り済みの日数を読む
        alt 足りない
            I->>P: 日ごとのドメイン集計（許可 + ブロック）
            I->>D: dns_domains に初出を反映
        end
        I->>P: カーソル以降のクエリ（窓を重ねて取る）
        I->>D: dns_queries / dns_domains / dns_client_daily
        I->>D: 保持期間を過ぎた生クエリを消す
    end

    B->>M: GET /api/watch
    M->>D: 初出・NXDOMAIN・種別・周期・ラベルの形
    M-->>B: 候補 + 理由（Pi-holeの絞り込み条件つき）<br/>+ 手の説明（しきい値つき）+ 「どこまで見えているか」
```

理由に付いてくる絞り込み条件は、そのまま Pi-hole の管理画面
（`/admin/queries.lp?domain=…&client_ip=…&from=…&until=…`）へのリンクになる ——
**「本当にそうなっているか」は元の通信を見るのが一番早い**。

飛ぶのは監視アプリ自身の `/go/queries` を経由してで、そこが Pi-hole の URL を組み立てる。
**Pi-hole v6 は未ログインのとき行き先を覚えない**（ログイン後はダッシュボードに着く）ので、
直接リンクにすると目的のページに着くまで2回押すことになるため。
`PIHOLE_WEB_AUTO_LOGIN` が有効なら、そこでセッション（sid）を付けて1回で着かせる。

## 3つの一覧

**材料も判定も違うので、口を分けてある。**

```mermaid
flowchart LR
    subgraph 画面
      T1[ブロック済み]
      T2[未ブロックの監視]
      T3[いま来ているもの]
    end
    subgraph サーバ
      A["/api/domains"]
      W["/api/watch"]
      L["/api/live"]
    end
    P[(Pi-hole)]
    Q[(dns_queries<br/>dns_domains)]
    N[(domain_notes)]

    T1 --> A --> P
    T2 --> W --> Q
    T3 --> L --> P
    A -.->|アクセス元と期間| Q
    A --> N
    W --> N
    L --> N
```

- **ブロック済み**に出るのは**Pi-hole の集計に載っているものだけ**（記録があるだけの
  ドメインは足さない —— 止められていないものを止められたものの一覧に置かない）。
  静かになったドメインのメモは、設定のページの「メモが残っているドメイン」から読む
- **ブロック済み**の件数は Pi-hole をその場で叩いた集計
- **アクセス元（端末）と通信が起きていた期間だけは貯めたクエリから足す**（点線）——
  Pi-hole の集計はドメインと件数しか返さないため。数えるのは**止められたクエリだけ**で、
  範囲は**直近24時間**（未ブロックの監視と同じ窓。件数は全期間なので範囲が違い、
  画面が前置きで断る）。
  **期間と件数はアクセス元ごと**に持ち、画面も1台1行で出す（`ClientActivity`）
- **未ブロックの監視**は貯めた過去との突き合わせ（Pi-hole は叩かない）
- **いま来ているもの**だけは集計ではなく「流れ」。**受信を始めた時点から先**に止められた
  通信を、**1件1行**で積む（同じドメイン・同じアクセス元でも、来るたびに新しい行）。
  数えてまとめると「いま何度も鳴っている」が見えなくなるため。
  アクセス元はその通信を出した1台だけ。確認済みのものも流し、
  出すかどうかはツールバーのフィルター（未確認／確認済み／すべて）が決める。
  材料は貯めたクエリではなく**Pi-hole をその場で叩いたもの** —— 取り込みは数分おきなので、
  数分前の写しから「いま」は作れない
- どれも `domain_notes` を重ねるので、**メモ・判定・調査結果は3つの一覧で共有される**

## 2つの聞き方

```mermaid
flowchart TB
    B1["まとめてAIに聞く<br/>（ツールバー）"] --> ASK["/api/ask"]
    B2["詳しく調べる<br/>（行のボタン）"] --> INV["/api/investigate"]

    ASK --> ALL["チェックした全員<br/>10件ずつ・1〜2文・webなし"]
    INV --> ONE["メインの1人<br/>1件・見出し付きの文章・web検索あり"]

    MODE["どちらの一覧か（mode）<br/>+ 候補に挙げた理由"] -.-> ASK
    MODE -.-> INV

    INV -.-> PROF["観測データ<br/>（端末・回数・状態の内訳）"]
    PROF -.-> ONE

    ALL --> NOTE["domain_notes.note"]
    ONE --> RES["domain_notes.research<br/>（メモとは別）"]

    B3["追加で聞く<br/>（詳細画面）"] --> FU["/api/followup"]
    RES -.->|これまでのやり取りを材料に| FU
    FU --> ONE2["メインの1人<br/>質問1つ・web検索あり"]
    ONE2 -->|末尾に足す| RES
```

**「詳しく調べる」に観測データを渡すのが要点。** そのドメインが何かは web でも分かるが、
**このネットワークでどう振る舞っているか**はこちらしか知らない。両方を突き合わせて初めて
「放っておいてよいか」が言える。

**「追加で聞く」は「詳しく調べる」の続き。** 相手（メインの1人）も材料（観測データ・web検索）も
上限秒数も同じで、違うのは**これまでのやり取りと質問を渡し、答えを `research` の末尾に足す**
ところ。次の質問にはその全文を渡すので会話が続く —— 別々に持つと、2つ目の質問が
1つ目の答えを知らないまま返ってくる。

**どちらの一覧から聞いたか（`mode`）も渡す。** ブロック済みの一覧は「Pi-hole が止めたもの」
なので「なぜ止まったか」を聞けばよいが、監視の一覧は**ブロックの結果ではなく振る舞いで
拾った候補**で、同じ文言で聞くと「ブロックされたと考えられます」という嘘のメモが並ぶ
（実際にそうなっていた）。監視のときは**候補に挙げた理由（観測した事実）も一緒に渡す**
—— 「はじめて見た」のと「10分おきに鳴っている」のとでは、書くべきことが違う。

## 確認の流れ

```mermaid
stateDiagram-v2
    [*] --> 未確認
    未確認 --> 確認済み : 調べて納得した
    確認済み --> 未確認 : 未確認に戻す

    note right of 未確認
        メモと調査結果は
        どちらの状態でも書ける
        （確認済みとは独立）
    end note
```

**状態は「確認済み」の1つだけ。** かつては「問題あり（ブロックされて当然）」と
「問題なし（怪しく見えただけ）」に分けていたが、**見返すときに要るのは
「もう見たか」だけ**だったのでやめた。

## 疎通の確認だけ、外へパケットを出す

一覧の判定はすべて「Pi-hole が記録したもの」の突き合わせで、**このアプリが自分で
パケットを出すのは設定画面の ping / 経路だけ**（`diag.rs`）。外部コマンドを呼ぶのも
ここ1か所なので、守りもここに閉じている。**出力は溜めずに1行ずつ流す** ——
数十秒かかることがあるので、終わるまで待たせると打てているのか分からない。

```mermaid
flowchart LR
    S["設定画面<br/>（相手先を入力）"] --> D["/api/diag"]
    D --> V{"文字を確かめる<br/>英数字と . - _ :<br/>- で始まらない"}
    V -->|違う| NG["理由を返す（400）"]
    V -->|通る| C["Command（シェルを通さない）<br/>stdbuf -oL ping / tracepath"]
    C --> L["1行ずつ流す（NDJSON）"]
    L --> N["経路はIPの名前を引いて足す<br/>（getent。出そろってからまとめて）"]
    C --> T{"上限内に終わったか"}
    T -->|いいえ| K["kill_on_drop で子ごと落とす"]
```

## 更新の手順

図はコードに追従させる。**モジュールを足した・口を足した・状態を増やしたときは、
同じコミットでこの文書も直す**（DBの形を変えたときは [`database.md`](database.md) も）。

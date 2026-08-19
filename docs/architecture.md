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
        +/api/domains ブロック済みの一覧
        +/api/watch 未ブロックの怪しい候補
        +/api/watch/baseline 監視の基準日時
        +/api/review 判定（問題なし/問題あり）
        +/api/note メモ
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
        +ask_within() CLIブリッジへ
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

## 2つの一覧

**材料も判定も違うので、口を分けてある。**

```mermaid
flowchart LR
    subgraph 画面
      T1[ブロック済み]
      T2[未ブロックの監視]
    end
    subgraph サーバ
      A["/api/domains"]
      W["/api/watch"]
    end
    P[(Pi-hole)]
    Q[(dns_queries<br/>dns_domains)]
    N[(domain_notes)]

    T1 --> A --> P
    T2 --> W --> Q
    A --> N
    W --> N
```

- **ブロック済み**は Pi-hole をその場で叩いた集計（貯めたものは使わない）
- **未ブロックの監視**は貯めた過去との突き合わせ（Pi-hole は叩かない）
- どちらも `domain_notes` を重ねるので、**メモ・判定・調査結果は両方の一覧で共有される**

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

## 判定の流れ

```mermaid
stateDiagram-v2
    [*] --> 未確認
    未確認 --> 問題あり : 問題のある通信だった<br/>（ブロックされて当然）
    未確認 --> 問題なし : 怪しく見えただけで<br/>無害だった（誤検知）
    問題あり --> 未確認 : 未確認に戻す
    問題なし --> 未確認 : 未確認に戻す

    note right of 未確認
        メモと調査結果は
        どの状態でも書ける
        （判定とは独立）
    end note
```

**測っているのは「そのドメイン自身が問題のある通信か」**であって、「人が対処すべきか」ではない。
一覧に並ぶものは「ブロックが妥当だったもの」と「怪しく見えただけのもの」の混ざりもので、
**「確認済み」の一語に畳むと、何が誤検知だったのかが分からなくなる**。

## 疎通の確認だけ、外へパケットを出す

一覧の判定はすべて「Pi-hole が記録したもの」の突き合わせで、**このアプリが自分で
パケットを出すのは設定画面の ping / 経路だけ**（`diag.rs`）。外部コマンドを呼ぶのも
ここ1か所なので、守りもここに閉じている。

```mermaid
flowchart LR
    S["設定画面<br/>（相手先を入力）"] --> D["/api/diag"]
    D --> V{"文字を確かめる<br/>英数字と . - _ :<br/>- で始まらない"}
    V -->|違う| NG["理由を返す（400）"]
    V -->|通る| C["Command（シェルを通さない）<br/>ping / tracepath"]
    C --> T{"上限内に終わったか"}
    T -->|はい| OUT["出力をそのまま返す<br/>（終了コード0でなくても）"]
    T -->|いいえ| K["kill_on_drop で子ごと落とす"]
```

## 更新の手順

図はコードに追従させる。**モジュールを足した・口を足した・状態を増やしたときは、
同じコミットでこの文書も直す**（DBの形を変えたときは [`database.md`](database.md) も）。

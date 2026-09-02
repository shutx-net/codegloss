# マスク方針の A/B 判定シート

`docs/model-runtime-notes.md` §12 の続き。**自動指標では決着がつかなかった断片**
だけを並べてある。読んで選ぶのは人間で、この文書はその紙である。

## この文書の読み方

**「良いほう」の欄と集計欄は未記入である。**埋めるのは実際に読む人間であり、
推測で埋めないこと（`zed-display-notes.md` の実機チェックリストと同じ扱い）。
埋めるときは、読んだ日と読んだ人が何を前提にしているか（Rust を読むか、日本語が
母語か）も併記する。判断の再現性はそこに依る。

シートそのものは実行から出た。作り方は最後の節。

## なぜ 14 件しか読まないのか

コーパス 62 ブロック・156 断片のうち、**マスクの有無で出力が違ったのは 38 件**
（空白差を除くと 35 件）で、**保全対象を 1 つも含まない 113 件は 1 件も違わなかった**
（§12）。つまり読む価値があるのは差が出た分だけである。

そこからさらに、**どちらかの腕が保全を落とした断片**を除いてある。落としたほうが
悪いことは数えれば決まるので、人間に問うと「識別子を 1 つ失うかわりに読みやすい
日本語」を選ばせてしまう。残ったのが 14 件。

**14 件では足りない。**符号検定（同点を除いた n に対する片側二項検定）で
p < 0.05 に届くのは:

| n | 必要な勝ち数 | そのときの p | 真の選好が 70:30 のときの検出力 |
|---|---|---|---|
| 14 | 11 | 0.029 | 0.36 |
| 30 | 20 | 0.049 | 0.73 |
| 45 | 29 | 0.036 | 0.84 |

つまりこのシート単独では、はっきりした差があっても 3 回に 2 回は見逃す。
**識別子が濃い第 2 コーパスのシートと合わせて 45 件前後にしてから判定すること。**
第 2 コーパスは第三者のソースから作るのでリポジトリに置けない。作り方は §12。

## 手順

1. 下の 14 件を上から読む。各件の 1 と 2 は、どちらがマスクありでどちらが
   マスク無しか伏せてある（並び順は断片ごとに決まっていて、規則性は無い）。
2. **最後の「答え合わせ」は先に読まない。**
3. 「良いほう」に `1` / `2` / `同じ` のいずれかを書く。基準は 1 つだけ:
   **原文を見ずにこの日本語だけを読んだとき、コードの意味が正しく伝わるか。**
   日本語としての流暢さではない。節が消えている訳は、読みやすくても誤りである。
4. 全部埋めてから答え合わせの表で A / B に読み替え、集計欄を埋める。

## シート

### 1

原文: what [`Translator::translate`] receives is always a masked segment.

1. `Translator::translate`]が受信するものは、常にマスクされたセグメントです。
2. What[`Translator::translate`] は、常にマスキングされたセグメントを受け取ります。

- 良いほう: 

### 2

原文: A typing burst produces one `didChange` per keystroke, and each of them re-extracts every comment in the file.

1. タイピングバーストはキーストロークごとに1つの`didChange`を生成し、それぞれがファイル内のすべてのコメントを再抽出します。
2. タイピングバーストはキーストロークごとに`didChange`を生成し、それぞれがファイル内のすべてのコメントを再抽出します。

- 良いほう: 

### 3

原文: Translates whatever of `pending` is not cached yet, then asks the client to refetch.

1. 未キャッシュの `pending` を翻訳し、クライアントに再フェッチを要求します。
2. `pending` がキャッシュされていないものを翻訳し、クライアントに再フェッチを要求します。

- 良いほう: 

### 4

原文: Hover has no counterpart to this - the protocol has no `workspace/hover/refresh` - so a hover that missed the cache stays as it was until the user hovers again.

1. Hoverにはこれに相当するものがありません - プロトコルには`workspace/hover/refresh`がありません - キャッシュを逃したホバーは、ユーザが再びホバーするまでそのままです。
2. Hover にはこれに相当するものがありません - プロトコルには `workspace/hover/refresh` がありません。

- 良いほう: 

### 5

原文: Returns `false` once the queue is closed, which happens when the server is shutting down.

1. キューが閉じると `false` を返します。これはサーバーがシャットダウンしているときに発生します。
2. キューが閉じられたら `false` を返します。

- 良いほう: 

### 6

原文: IMPORTANT: this crate must keep building without an async runtime, so the shared state is a `std::sync::Mutex` and never a `tokio` one.

1. 重要: このクレートは非同期ランタイムなしでビルドし続ける必要があるため、共有状態は `std::sync::Mutex` であり、決して `tokio` ではありません。
2. 重要: このクレートは非同期ランタイムなしでビルドし続ける必要があるため、共有状態は`std::sync::Mutex`であり、`tokio`ではない。

- 良いほう: 

### 7

原文: The lock is only ever held for a hash lookup - never across the store's file IO - so no `.await` can happen underneath it.

1. ロックは、ハッシュルックアップのためだけに保持される - ストアのファイルIOを越えない - 従って、その下に`.await`は起こらない。
2. ロックはハッシュルックアップのためだけに保持される - ストアのファイルIOに決して渡らない - なので、その下には `.await` は発生しない。

- 良いほう: 

### 8

原文: Values are `Arc<str>` so that handing one to a request handler costs a refcount bump rather than a copy.

1. 値は`Arc<str>`であるため、リクエストハンドラに渡すにはコピーではなく参照バンプが必要です。
2. 値は `Arc<str>` なので、リクエストハンドラに渡すにはコピーではなく refcount バンプが必要です。

- 良いほう: 

### 9

原文: With a [`GlossStore`] behind it the map becomes the hot half of a two-level cache:

1. `GlossStore`]の背後にあるマップは、2レベルキャッシュのホットな半分になります。
2. その背後に[`GlossStore`']があれば、マップは2レベルキャッシュのホットな半分になります。

- 良いほう: 

### 10

原文: Comparing ticks is what makes "least recently used" meaningful without pulling in `Instant`, which is not available on every target this crate has to build for.

1. ティックを比較することは、このクレートが構築しなければならないすべてのターゲットで利用できない `Instant` を引っ張ることなく、「最も最近使用されていない」意味を持つものです。
2. ティックを比較することは、`Instant`を引き込むことなく「最も最近使われていない」意味をなす。

- 良いほう: 

### 11

原文: The gloss the map holds for `key`, marking it as recently used.

1. マップが `key` に対して保持する光沢は、最近使用したようにマークします。
2. マップの光沢は`key`に保持され、最近使用されたようにマークされます。

- 良いほう: 

### 12

原文: The store, if there is one, has its own [`len`](GlossStore::len).

1. もしあるなら、ストアには独自の[`len`](GlossStore::len)があります。
2. 店は、ある場合、独自の[`len`](GlossStore::len)を持っています。

- 良いほう: 

### 13

原文: If `contains` bumped recency, `a` would survive and `b` would go.

1. `contains`が摂氏数を上回れば、`a`は生き残り、`b`は生き残ります。
2. `contains` がバンピングされた頻度であれば、`a` は生き残り、`b` は行きます。

- 良いほう: 

### 14

原文: Reads a source file with Tree-sitter and returns the comments in it as [`codegloss_core::CommentBlock`] values, positions expressed as byte offsets and zero-based line numbers.

1. Tree-sitterでソースファイルを読み、その中のコメントを [`codegloss_core::CommentBlock`] 値、バイトオフセットとして表現された位置、ゼロベースの行番号として返します。
2. Tree-sitterでソースファイルを読み、[`codegloss_core::CommentBlock`]値、バイトオフセットとして表現された位置、ゼロベースの行番号としてコメントを返します。

- 良いほう: 

## 集計（未記入）

読んだ人:
読んだ日:
前提（Rust を読むか、日本語が母語か）:

| | 件数 |
|---|---|
| A（マスクあり）が良い | |
| B（マスク無し）が良い | |
| 同じ | |

同点を除いた n:
勝ち数:
片側 p:
判定:

## 答え合わせ（上を埋めるまで読まない）

各件の「1」がどちらの腕か。A = マスクあり（出荷しているもの）、
B = マスク無し。

| 件 | 1 は |
|---|---|
| 1 | A |
| 2 | B |
| 3 | B |
| 4 | A |
| 5 | B |
| 6 | B |
| 7 | A |
| 8 | A |
| 9 | A |
| 10 | B |
| 11 | B |
| 12 | B |
| 13 | A |
| 14 | B |

## このシートの作り方

`crates/codegloss-translator/tests/pipelines.rs` が書き出す。同梱コーパスに
対して:

```sh
CODEGLOSS_MODEL_PACK=~/codegloss-model CODEGLOSS_SHEET=/tmp/sheet.txt \
  cargo test -p codegloss-translator --features candle --release \
  --test pipelines -- --ignored --nocapture
```

`CODEGLOSS_CORPUS=<file>` で別のコーパスに対して同じものが出る。
選抜の規則はハーネス側の `sheet()` に書いてあり、上の「なぜ 14 件しか読まないのか」
はその規則を日本語にしたものである。

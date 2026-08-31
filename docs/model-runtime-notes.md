# 翻訳エンジン（candle + FuguMT）の実測メモ

P7 で `CandleTranslator` を入れたときに実機で測った値。**すべて実測値で、推測は
書かない。**未検証のものは最後の節にまとめてある。

測定環境:

| | |
|---|---|
| CPU | Intel Xeon @ 2.10GHz、4 コア |
| メモリ | 16 GB |
| OS | Linux 6.18（コンテナ内） |
| Rust | 1.98.0（`rust-toolchain.toml`） |
| ビルド | `--release`（`--features candle`） |
| デバイス | CPU のみ（`Device::Cpu`）。GPU では測っていない |
| モデル | staka/fugumt-en-ja、`model_version = fugumt-en-ja-8b2d3d3b7da2` |

再現するには:

```sh
python3 tools/convert-fugumt/convert.py /path/to/pack
export CODEGLOSS_MODEL_PACK=/path/to/pack

# 訳文と保全（品質）
cargo test -p codegloss-translator --features candle --release -- --ignored --nocapture

# レイテンシとメモリ
cargo run -p codegloss-translator --features candle --release --example measure
```

**メモリを測るときはテストではなく `--example measure` を使うこと。**理由は 6.1。

## 1. 重みの変換は要らなかった

**`VarBuilder::from_pth` が `pytorch_model.bin` をそのまま読む。**candle は
pickle アーカイブを直接開けるので（`candle_core::pickle`）、safetensors への
変換工程が丸ごと不要になった。

| | |
|---|---|
| `pytorch_model.bin` | 121,192,965 バイト（上流のまま） |
| モデルパックの構築から `marian::MTModel` ができるまで | 0.29〜0.44 秒（9 回の実行での範囲） |

計画（P7-T1）は `transformers.MarianMTModel.from_pretrained` →
`safetensors.torch.save_file` を想定していたが、これは torch のフルインストール
（1 GB 超）を要求する。実測の結果それが不要になったので、変換スクリプトの依存は
`protobuf` / `sentencepiece` / `tokenizers` の 3 つだけになった。

`model.safetensors` を置いた場合はそちらが優先される。読み込みは mmap ではなく
`from_buffered_safetensors`（`codegloss-translator` は
`#![forbid(unsafe_code)]` で、`from_mmaped_safetensors` は `unsafe` のため）。

## 2. トークナイザの変換は必要だった

上流には `tokenizer.json` が無い。あるのは `source.spm` / `target.spm`
（SentencePiece）と `vocab.json` だけで、candle の marian-mt 経路は
`Tokenizer::from_file` を前提にしている。

- **transformers 5 系には Marian 用の変換（`MarianConverter`）が無い。**
  4.46.3 にも無い（`transformers.convert_slow_tokenizer` の中を確認）。
  そのため `SpmConverter`（4.46.3）と同じ手順を `tools/convert-fugumt/convert.py`
  に写し、transformers に依存せず組み立てている。
- 生成した高速トークナイザと `transformers.MarianTokenizer`（低速）の ID 列を
  7 例で突き合わせて **7/7 一致**（`convert.py --verify`）。突き合わせた例には
  プレースホルダ・URL・全角混じり・連続空白を含む。
- `source.spm` と `target.spm` は FuguMT では**バイト単位で同一**
  （md5 `32df5391e60817f5d29645777b489afe`）。
- `vocab.json` は 32001 語で、`<pad>` が 32000。`*.spm` のピース 32000 個は
  `vocab.json` の ID 0〜31999 とそのまま一致していた（一致は仮定していない）。

## 3. プレースホルダの形式は `X0Q` にした

P6 が暫定で選んだ `⟦0⟧`（U+27E6 / U+27E7）は、**FuguMT では 1 つも生き残らない。**
どちらの括弧も語彙に無く、トークナイザの段階で `<unk>` になる。

```
"Returns ⟦0⟧ when authentication succeeds."
  -> "認証が成功したら <unk>0<unk> を返します。"
```

`crates/codegloss-translator/tests/placeholders.rs` で、32 文・39 スロットを
候補ごとに実際に翻訳させ、プレースホルダが「ちょうど 1 回、綴りそのままで」
訳文に現れるかを数えた結果:

| 候補 | 形 | 成功率 | 内訳 | トークナイザ通過 | 32 文のトークン数 |
|---|---|---|---|---|---|
| brackets | `⟦0⟧` | **0.0%** | 36 unknown / 3 lost | 0/39 | 470（うち unknown 78） |
| square | `[0]` | 64.1% | 14 lost | 39/39 | 431 |
| bare-tag | `CG0` | 84.6% | 6 lost | 39/39 | 431 |
| underscore | `__CG0__` | 87.2% | 5 lost | 39/39 | 548 |
| q | `Q0` | 92.3% | 3 lost | 39/39 | 392 |
| q-x | `Q0X` | 94.9% | 2 lost | 39/39 | 431 |
| q-under | `Q0_` | 94.9% | 2 lost | 39/39 | 431 |
| **x-q** | **`X0Q`** | **97.4%** | 1 lost | 39/39 | 431 |

**採用したのは `X0Q`**（`crates/codegloss-core/src/preserve.rs` の
`PLACEHOLDER_OPEN` = `'X'` / `PLACEHOLDER_CLOSE` = `'Q'`）。

`X0Q` が落とした 1 件は、モデルが節ごと落とす文で、**どの候補も同じ 1 件を落とす**:

```
"Splits the input on whitespace and drops empty fields; see X0Q."
  -> "空白のフィールドを空白に分割し、空白のフィールドをドロップします。"
```

つまりこの文集合での上限が 38/39 であり、`X0Q` はそこに達している。

### 壊れ方の傾向

数字は残り、**区切り記号が削られる**のが典型だった。文頭のプレースホルダで特に
起きやすい（`@param` や `TODO:` を退避すると文頭に来る）。

```
"Calls [CG0] before [CG1]."      -> "CG0] を [CG1] の前に呼び出します。"   （開き括弧が消える）
"Panics when #0# is called ..."  -> "0#が#1#の前に呼び出される..."          （同上）
"__CG0__ return the cached ..."  -> "__CG0____ がヒットすると..."           （下線が増え、2 個目が消える）
"Returns %%0%% when ..."         -> "認証が成功したら%%0%を返します。"       （記号が減る）
```

記号を使わず**ラテン文字で挟む**形が強い、というのが実測から言えること。
ただし文字の選び方で大きく変わる（下の探索の表を参照）。

### 探索の段階で落とした候補

上の表より前に、14 文・19 スロットの小さい集合で広く振るった結果。**文集合が
違うので上の表と直接は比べられない**が、落とした理由の記録として残す。

| 候補 | 成功率 | | 候補 | 成功率 |
|---|---|---|---|---|
| `X0Q` | 100.0% | | `_CG0_` | 84.2% |
| `Q0` / `Q0X` / `Q0_` | 94.7% | | `ID0` | 84.2% |
| `X0` / `V0` / `QQ0QQ` | 89.5% | | `__0__` / `{{0}}` | 73.7% |
| `Q0Q` / `CG0` | 84.2% | | `W0` | 73.7% |
| `K0` / `__CG0__` | 78.9% | | `#0#` / `[CG0]` / `Q0.` | 68.4% |
| `<CG0>` / `#CG0#` | 63.2% | | `Y0` | 52.6% |
| `Z0` / `<0>` / `@0@` | 42.1% | | `$0$` / `Qz0` | 21.1% |
| `[[0]]` / `%%0%%` | 10.5% | | `~0~` / `※0` | 15.8% |
| `★0★` | 0.0%（unknown） | | `⟦0⟧` | 0.0%（unknown） |

`Z0` が 42.1% なのは、モデルが `Z0` を `20` に書き換えるため。`★` は `⟦` と同じく
語彙に無い。

### 形式を差し替える手順

`crates/codegloss-core/src/preserve.rs` の `PLACEHOLDER_OPEN` /
`PLACEHOLDER_CLOSE` の 2 定数だけ。`placeholder()` と `placeholder_at()` が
その 2 つだけを見ており、両者の整合はテストが固定している。差し替えたら
`placeholders.rs` の候補表に新しい形を足すこと（採用した形が候補表に無いと
テストが落ちる）。

## 4. レイテンシとメモリ（P7 時点）

**この節は P7 の記録。**P8 で測り直した値、削減の内訳、削減後の値は
[§6 軽量化（P8）](#6-軽量化p8)にある。

コーパス（`crates/codegloss-translator/tests/fixtures/comments.jsonl`、50 コメント）
を前処理にかけた **60 セグメント**での実測。1 セグメント = 段落 1 つ
（Javadoc なら `@param` 行 1 本など）。

| | 値 |
|---|---|
| 1 セグメントあたり 平均 | 235 ms |
| 1 セグメントあたり p50 | 231 ms |
| 1 セグメントあたり p90 | 320 ms |
| 1 セグメントあたり 最大 | 517 ms |
| 1 セグメントあたり 最小 | 95 ms |
| 60 セグメントを一括で | 13,510 ms（1 セグメントあたり 225 ms） |

**バッチにしても速くならない。**`CandleTranslator` はバッチを 1 件ずつ回す
（エンコーダ側のパディングとマスクを candle の `marian` が持たないため）。
速度が要るならここが最初の伸びしろ。

常駐メモリ（RSS）:

| 時点 | RSS |
|---|---|
| モデル読み込み前 | 4.2 MiB |
| 読み込み後 | 311.0 MiB |
| 60 セグメント推論後 | 314.2 MiB |

F16 の重み（121 MB）を F32 に展開して読むので、その 2 倍強がそのまま乗る。

### 表示方法への含み

- **Code Lens は成立する。**`codegloss-lsp` を実際に起動して stdio 越しに測った
  値で、**プロセス起動から日本語の Code Lens が返るまで 2.7 秒**
  （コメント 4 ブロック・6 セグメントのファイル。モデル読み込み 0.4 秒と
  デバウンス 150 ms を含む）。`textDocument/codeLens` の応答自体は 1 ms
  （キャッシュを引くだけなので翻訳の時間とは無関係）。同じファイルを
  モデルパック無しで開くと 0.7 秒で英語のまま返る。
- **1 コメント 235 ms は「入力に追随する表示」には遅い。**Inlay Hint を
  タイプ中に更新する用途は、この数字のままでは成立しない。

## 5. 実際の訳文

`--features candle --release` で、P6 の前処理・後処理を通した結果。

```
/// Returns `UserDetails` when authentication succeeds.
  -> 認証が成功したら `UserDetails` を返します。

/// Returns the currently authenticated user.
  -> 現在認証中のユーザーを返します。

/// Panics when `find_user` is called before UserRepository::open().
  -> `find_user` が UserRepository::open() の前に呼び出されるときのパニック。

/// The protocol is described at https://example.com/docs/auth.
  -> プロトコルはhttps://example.com/docs/authで記述されている。

// TODO: return the cached user when find_user hits.
  -> TODO: は、find_user がヒットするとキャッシュされたユーザを返します。

// SAFETY: the pointer comes from Box::into_raw and is never null.
  -> SAFETY: ポインタは Box::into_raw から来ており、決して null ではありません。

/// This is a blocking call and must not run on the async executor.
  -> これはブロッキングコールであり、非同期実行子で実行してはならない。

/// An iterator over the keys of the map, in insertion order.
  -> マップのキーを挿入順に繰り返すイテレータ。

# XXX: the upstream API returns a string here, not a number.
  -> # XXX: 上流の API は、数字ではなく、ここで文字列を返します。

// New returns a client that talks to the given endpoint.
  -> New は、指定されたエンドポイントと通信するクライアントを返します。
```

Javadoc は行構造ごと戻る:

```
/**
 * Returns the currently authenticated user.
 *
 * @param id the id to look up
 * @return authenticated user
 * @throws AuthenticationException if authentication failed
 */
  ->
現在認証中のユーザーを返します。

@param id 調べるIDは
@return 認証済みユーザ
@throws AuthenticationException 認証に失敗すると
```

**気になった点（良し悪しの判断は人間がする）:**

- 退避した語が文頭に来ると、そこだけ日本語として不自然になる。
  `TODO: は、find_user が…`、`@param id 調べるIDは`、
  `@throws AuthenticationException 認証に失敗すると` など。
  前処理が `TODO:` や `@param` を丸ごと隠すため、モデルには文の主語が欠けた
  断片が渡っている。
- `# Panics` の見出しが `# パニックス` になる。見出しを訳すかどうかは未決。
- `{@code findUserById}` は保護されず、`@code findUserById}` と `{` が落ちる。
  前処理（P6）が Javadoc のインラインタグを見ていない。
- 短い文ほど崩れやすい。`/** Closes the underlying stream. Idempotent. */`
  → `流れを閉ざし べき等です`。
- ビームサーチが無い（貪欲法）。FuguMT の `generation_config.json` は
  `num_beams: 12` を指定しているが、candle にビームサーチが無いのでそこは
  再現していない。**ビームサーチありとの比較は測っていない。**

### `<unk>` の抑止

上流の `bad_words_ids` はパディングトークンだけを禁じているが、`CandleTranslator`
は **`<unk>` も禁じている**。実測で訳文に `<unk>` がそのまま出る例があったため:

```
（抑止なし）/** Closes the underlying stream. Idempotent. */ -> 流れを閉ざし<unk>等に
（抑止あり）                                                 -> 流れを閉ざし べき等です
```

## 6. 軽量化（P8）

P7 の数字（1 セグメント 235 ms / 常駐 311 MiB）が実用上重いので削りにいった回。
**この節の数字はすべて同じ日・同じ機械・1 プロセス 1 モデルで測ってある。**同じ
バイナリを続けて走らせても平均が 337〜370 ms の幅で動く程度には機械が揺れるので、
比較はかならず同一セッション内で交互に測った。

再現するには:

```sh
CODEGLOSS_MODEL_PACK=/path/to/pack cargo run -p codegloss-translator \
  --features candle --release --example measure -- --dtype f32
```

`examples/measure.rs` は `VmRSS` と `VmHWM`（ピーク）の両方を、`/proc/self/clear_refs`
でピークを段階ごとに戻しながら出す。**測定をテストから追い出したのは、そのテスト
harness 自体が数字を壊していたため**（次項）。

### 6.1 「ロード時ピーク 1224.8 MiB」の正体はテストの並列実行だった

`cargo test -- --ignored` は `#[ignore]` の付いたテストを **1 プロセス内で並列に**
走らせる。`quality.rs` の 5 本はどれも `support::translator()` を呼ぶので、
**4 コアなら 4 個のモデルが同時に常駐する。**

| 走らせ方 | 読み込み後 RSS |
|---|---|
| `--ignored`（5 本、既定の並列度） | **1215.8 MiB**（モデル読み込みが 4 本同時で 11.7 秒） |
| `--ignored --exact latency_and_memory_are_measured`（1 本） | **311.0 MiB**（読み込み 0.5〜1.3 秒） |

報告されていた 1224.8 MiB はこれで再現した（1215.8 MiB）。**1 モデルのロードには
ピークが立っていない。**下の表のとおり、P7 の `VarBuilder::from_pth` は 1 テンソル
ずつ読んで即座に変換するので、`VmHWM` が定常値と一致する。

そのためレイテンシ・メモリの計測は `examples/measure.rs`（1 プロセス 1 モデル）へ
移し、`quality.rs` からは落とした。

### 6.2 重みの読み方（ピーク RSS の比較）

モデルのみ・tokenizer 抜き。各行は別プロセスで、`VmHWM` を読んでいる。

| 読み方 | dtype | 読み込み後 | **ピーク** | 時間 |
|---|---|---|---|---|
| **`from_pth`（遅延・P7 のまま）** | f32 | 251.3 | **251.3** | 372 ms |
| `from_pth`（遅延） | f16 | 122.2 | **122.2** | 211 ms |
| `pickle::read_all`（一括） | f32 | 239.3 | 441.1 | 678 ms |
| safetensors `from_buffered` | f32 | 250.1 | 365.6 | 327 ms |
| safetensors `from_mmaped`（**unsafe**） | f32 | 250.3 | 365.7 | 243 ms |
| safetensors `from_buffered` | f16 | 121.2 | 236.6 | 171 ms |
| safetensors `from_mmaped`（**unsafe**） | f16 | 121.4 | 236.8 | 84 ms |

**`#![forbid(unsafe_code)]` を外す理由は無い。**mmap と `from_buffered` のピークは
365.6 と 365.7 で**同じ**。mmap したページも読んだ時点で RSS に載るので、ピークは
減らない。減るのは読み込み時間だけ（327 → 243 ms、84 ms）。しかも **safetensors は
どちらも現行の `from_pth` より 114 MiB 悪い。**

**safetensors 化そのものに利が無い**ので `convert.py` は変更しない。ついでに分かった
こと: candle の `safetensors::save` に素通しすると **219 MB** になる。FuguMT の
pickle は 32001x512 の埋め込みを `model.shared.weight` / `lm_head.weight` /
`model.{encoder,decoder}.embed_tokens.weight` の **4 つの名前で共有**していて、
zip には 1 本しか入っていない（だから 121 MB）。safetensors は共有を表現できないので
4 本に展開される。重複を落とせば 121,138,802 バイトになり、上の表はその版で測った。

### 6.3 dtype

`bf16` は**動かない。**candle の CPU バックエンドに BF16 の matmul が無い
（`unsupported dtype BF16 for op matmul`、`SinusoidalPositionalEmbedding::new` の
時点で落ちる）。残る選択肢は f32 と f16。

60 セグメント、交互に 2 回ずつ:

| | f32（既定） | f16 |
|---|---|---|
| 読み込み | 436〜454 ms | 271〜276 ms |
| 読み込み後 RSS | **280.9 MiB** | **158.0 MiB** |
| ピーク RSS | 280.9 MiB | 162.3 MiB |
| 推論後 RSS | 284.1 MiB | 162.6 MiB |
| 1 セグメント 平均 | 385〜398 ms | 417〜420 ms |
| 1 セグメント p50 | 368〜393 ms | 408〜409 ms |
| 1 セグメント p90 | 569〜584 ms | 581〜596 ms |
| 60 セグメント一括 | 22.9 秒 | 24.5〜24.9 秒 |

**メモリ −44%、レイテンシ +6〜8%。**既定は f32 のままにして、`--precision f16` で
選べるようにした（`codegloss-lsp` の `--precision` / `CODEGLOSS_MODEL_PRECISION`）。
レイテンシの方が体感に効くという P7 の判断を覆すだけの材料が無いため。

品質は実質的に同じ。`CODEGLOSS_MODEL_PRECISION=f16` で `quality.rs` の 4 本が通り、
コーパス 50 件のうち**訳文が変わったのは 2 件**。良し悪しは両方向:

```
（f32）空白に入力を分割し、空白のフィールドをドロップします。
（f16）空白の入力を分割し、空のフィールドをドロップします。          ← f16 の方が良い

（f32）ここで割り当てられるものはありません:これはオーディオスレッドで実行されます。
（f16）オーディオスレッド上で実行されます。                          ← 節が 1 つ落ちている
```

f16 が速くならない理由は 6.5 に書いた。

### 6.4 tokenizer は 1 つで足りる

`Tokenizer` 1 つが **約 30 MiB** を占める。FuguMT の `source.spm` と `target.spm`
はバイト単位で同一（§2）なので、`tokenizer-source.json` と `tokenizer-target.json`
も同一になる。**2 つのファイルのバイト列を比べて、同じなら 1 つだけ構築して
`Arc` で共有する**ようにした。違えば従来どおり 2 つ作るので、他のパックでも壊れない。

| | 共有前 | 共有後 |
|---|---|---|
| f32 常駐 | 310.5 MiB | **280.9 MiB** |
| f16 常駐 | 194.4 MiB | **158.0 MiB** |

比較のためのコストは 4.8 MB の読み込み 1 回だけ（ピークが 158.0 に対し 162.3 に
なっているのがそれ）。

### 6.5 バッチ推論は入れられない（そして f16 が遅い理由でもある）

**candle の `marian::Encoder` にアテンションマスクが無い。**`Encoder::forward` は
`(xs, past_kv_len)` しか取らず、`EncoderLayer` は `self_attn.forward(xs, None, None)`
を呼ぶ。クロスアテンションも同様にマスクを受け取らない。パディングして束ねると
モデルがパッド位置を見に行き、**訳文が静かに悪くなる**。正しくやるには marian の
フォークが要るので、v0.1 では入れない。

どれだけ損をしているかは測った。1 デコードステップの matmul（`Linear::forward` と
同じ `w.t()` レイアウト、4 コア）:

| 形 | dtype | batch 1 | batch 4 | batch 8 |
|---|---|---|---|---|
| 512x512（attention 投影） | f32 | 77.6 us | 203.0 us | 206.6 us |
| 512x512 | f16 | 67.3 us | **624.1 us** | **639.3 us** |
| 512->2048（FFN） | f32 | 227.0 us | 332.4 us | 429.3 us |
| 512->2048 | f16 | 185.6 us | **1174.9 us** | **1157.8 us** |
| 512->32001（lm_head） | f32 | 2942.4 us | 3126.9 us | 4143.1 us |
| 512->32001 | f16 | 1762.2 us | 12316.4 us | 13539.9 us |

読み取れること。

- **どれもメモリ帯域律速。**512x512 の matvec は 1 MB の重みを 77.6 us で読んでいて
  13.5 GB/s。演算能力はほとんど使っていない。`RAYON_NUM_THREADS` を 1〜4 で振っても
  平均は 331〜358 ms で差が出ない（＝並列化の余地ではない）。
- **1 トークンあたりの内訳は、6 層ぶんの投影と FFN が約 5.5 ms、lm_head が約 2.9 ms。**
  合計 8.4 ms で、40 トークン出せば 336 ms。実測の 1 セグメント 385 ms とほぼ合う。
- **バッチ化できれば 3 倍前後は速くなる。**512x512 は batch 8 でも 206.6 us
  （1 行あたり 25.8 us、batch 1 の 1/3）。重みを 1 回読めば何行でも処理できるため。
- **f16 は batch 1 では速く、batch 2 以上で 3 倍遅くなる**（`w.t()` レイアウトのとき。
  `contig` なら 229.4 us で済む）。デコーダは batch 1 なので f16 が効くが、
  **エンコーダは batch = 入力トークン数**で回るためここで losses が出る。
  6.3 で f16 が全体として +6〜8% 遅くなるのはこれが理由。

### 6.6 訳をディスクに残す

推論そのものは 350 ms/セグメントから下がらない以上、**同じ訳を二度払わない**のが
一番効く。`GlossStore`（`codegloss-core`）が `GlossKey` の 16 進をファイル名にして
1 訳 1 ファイルで置き、`GlossCache` がメモリで外し、ディスクで当たったらメモリへ
昇格する。`GlossKey` には model_version が入っているので、エンジンを替えた訳が
出てくることはない。

`scripts/measure-code-lens.py --lines 200`（コメントブロック 67 件）を同じ
`CODEGLOSS_CACHE_DIR` で 2 回:

| | 1 回目（ディスク空） | 2 回目（ディスクに 69 件） |
|---|---|---|
| didOpen 直後の codeLens | 5.36 ms / **プレースホルダ 67 件** | 5.22 ms / **プレースホルダ 0 件** |
| didOpen から codeLens/refresh まで | **40,055 ms** | refresh 自体が飛ばない（訳す物が無い） |

**2 回目は最初の codeLens がそのまま日本語で返る。**既定は有効で、置き場所は
`--cache-dir`、無効化は `--no-cache`。既定のディレクトリは
`$XDG_CACHE_HOME/codegloss/glosses`（macOS は `~/Library/Caches`、Windows は
`%LOCALAPPDATA%`）。ディレクトリが作れない・書けない場合はログを出してメモリのみで
動く（**キャッシュが理由でサーバが落ちてはいけない**、モデルパックと同じ方針）。

上限は 50,000 件で、超過ぶんは**起動時に 1 回だけ**古い順に消す。読み出しでは
ファイルの時刻を触らない（ホバーのたびに mtime を書き戻すのは割に合わない）ので、
「古い順」は書いた順。

### 6.7 入れなかったもの

| | 理由 |
|---|---|
| safetensors への変換 | ピークが `from_pth` より 114 MiB 悪い（6.2）。`convert.py` は変更していない |
| `from_mmaped_safetensors`（unsafe） | `from_buffered` に対してピークの差が 0.1 MiB（6.2）。`#![forbid(unsafe_code)]` を外す価値が無い |
| `pickle::read_all` での一括読み | ピークが 251.3 → 441.1 MiB に悪化（6.2）。埋め込みが 4 重に実体化されるため |
| バッチ推論 | candle の marian にエンコーダのマスクが無く、正しく実装できない（6.5） |
| bf16 | candle の CPU に BF16 matmul が無い（6.3） |
| f16 を既定にする | メモリは 44% 減るがレイテンシが 6〜8% 増える。`--precision f16` で選べる形にした（6.3） |

### 6.8 量子化は要るか

**要る。ただし帯域を減らす目的で。**6.5 のとおり推論はメモリ帯域律速で、1 トークンに
つき f32 なら 90 MB 前後の重みを読み直している。int8 にすれば読む量が 1/4 になるので、
**メモリとレイテンシの両方**が下がる見込みがある（f16 で下がらなかったのは、candle の
f16 カーネルが `w.t()` レイアウト・batch>1 で崩れるためであって、帯域の理屈が
外れたからではない）。

ただし `candle-transformers` に `quantized_marian` は無いので、自前実装になる。
コストは小さくない（量子化した `Linear` と、marian のフォークまたは同等の再実装）。
**バッチ化と同じ場所（エンコーダのマスクを持った marian）に手を入れる話なので、
やるならまとめて 1 回**というのが今回の見立て。

## 7. 未検証

- **量子化。**自前実装が要るので試していない（見立ては 6.8）。
- **ビームサーチとの品質差。**貪欲法のみ。
- **パディングしたバッチ推論の実測。**candle の marian では正しく実装できないので
  （6.5）、matmul 単体でしか測っていない。エンコーダのマスクを足したフォークで
  実際にどれだけ速くなるかは未測定。
- **Linux 以外。**macOS / Windows では測っていない。GPU（CUDA / Metal）も同様。
  ディスクキャッシュの既定ディレクトリも Linux でしか確かめていない。
- **Zed 実機での見え方。**LSP サーバとしての応答は stdio 越しに確認したが、
  Zed 上での表示は確認していない（`docs/zed-display-notes.md` の担当）。
- **長いコメント。**入力は `max_position_embeddings - 1 = 511` トークンで
  切っている。それを超えるコメントで何が起きるかは測っていない。
- **モデルの遅延読み込み。**ディスクキャッシュが全件当たる場合でもモデルは起動時に
  読まれ、280.9 MiB を占めたまま 1 度も使われない。cache key に model_version が
  要るので、パックの `manifest.json` だけ先に読む形にすれば避けられるはずだが、
  実装も計測もしていない。
- **ディスクキャッシュの実運用での大きさ。**上限 50,000 件が妥当かは、長く使った
  ディレクトリで測っていない。

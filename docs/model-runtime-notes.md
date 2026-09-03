# 翻訳エンジン（candle + FuguMT）の実測メモ

`CandleTranslator` を入れたときに実機で測った値と、そのあと軽量化・
訳文の質・バッチ推論で足した分。**すべて実測値で、推測は書かない。**未検証のものは
最後の節にまとめてある。

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

# レイテンシとメモリ（--beams で探索幅を振れる）
cargo run -p codegloss-translator --features candle --release --example measure

# 訳文そのものを見る（%%% 区切りでコメントを流す）
cargo run -p codegloss-translator --features candle --release --example probe < comments.txt

# マスク方針の比較（12 の採点表。CODEGLOSS_CORPUS でコーパスを差し替えられる）
cargo test -p codegloss-translator --features candle --release --test pipelines -- --ignored --nocapture
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

計画（）は `transformers.MarianMTModel.from_pretrained` →
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

暫定で選んでいた `⟦0⟧`（U+27E6 / U+27E7）は、**FuguMT では 1 つも生き残らない。**
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

## 4. レイテンシとメモリ（初版）

**この節は初版の記録。**§6 で測り直した値、削減の内訳、削減後の値は
[§6 軽量化](#6-軽量化)にある。

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

`--features candle --release` で、前処理・後処理を通した結果。

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
  前処理が Javadoc のインラインタグを見ていない。**7.5 で直した。**
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

## 6. 軽量化

§4 の数字（1 セグメント 235 ms / 常駐 311 MiB）が実用上重いので削りにいった回。
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
ピークが立っていない。**下の表のとおり、いまの `VarBuilder::from_pth` は 1 テンソル
ずつ読んで即座に変換するので、`VmHWM` が定常値と一致する。

そのためレイテンシ・メモリの計測は `examples/measure.rs`（1 プロセス 1 モデル）へ
移し、`quality.rs` からは落とした。

### 6.2 重みの読み方（ピーク RSS の比較）

モデルのみ・tokenizer 抜き。各行は別プロセスで、`VmHWM` を読んでいる。

| 読み方 | dtype | 読み込み後 | **ピーク** | 時間 |
|---|---|---|---|---|
| **`from_pth`（遅延・変更なし）** | f32 | 251.3 | **251.3** | 372 ms |
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
レイテンシの方が体感に効くという §4 の判断を覆すだけの材料が無いため。

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

## 7. 訳文の質

読んでいて「日本語として不自然」「まったく関係のない語の羅列になる」という
指摘が出たので、自リポジトリの実コード（`translation.rs` / `cache.rs` /
`codegloss-parser`）からコメント **62 ブロック**を抜き、まとめて訳して調べた。
道具は `crates/codegloss-translator/examples/probe.rs`（マスクありとマスクなしを
並べて出す）。

### 7.1 壊れ方は 4 つあった

**1. 節がまるごと消える。**これがいちばん危険で、**訳文は日本語として完全に
自然なので読者は気づけない**。

```
Returns None once the queue is closed, which happens when the server is shutting down.
  -> キューが閉じられたら None を返します。          ← which 以下が消滅
```

→ **カンマのあとの `which` に限っては 7.6 で切るようにした**（`, which …` を 1 つの
文として分ける）。なお**この例は測り直すと今は落ちない**。上の実測は貪欲法のもので、
ビーム幅 4 の今日は同じ文が `キューが閉じられたら None を返します。これはサーバーが
シャットダウンしているときに発生します。` と節ごと返る。**落ちるのはマスクした形のほう**
（`None` → `X0Q`）で、そちらは幅 4 でも 12 でも消える。詳しくは 7.6。

**2. 意味が反転する。**

```
Opens the file for reading, failing if it does not already exist.
  -> ファイルがまだ存在しない場合は、ファイルを開きます。   ← failing が消えて条件が別の動詞に付いた
```

**3. マスクが訳を悪くしている。**プレースホルダで語を隠すと、語義を決める
手がかりまで消える。95 ユニット中 **39 件**でマスクの有無が出力を変えていた。

```
Wraps find_user, UserRepository::open and CacheHandle::warm in one call.
  マスクあり -> …1回の通話でラップします。      ← call = 電話
  マスクなし -> …1 つの呼び出しで実行します。
```

**4. 用語が一般語になる。**`gloss` → 光沢、`store` → 店、`clock` → 時計用。
FuguMT は一般ドメインのモデルで、ソフトウェア英語は守備範囲外。

**貪欲デコードの反復ループは 0/95 だった。**「同じ語を繰り返す」のはデコーダの
暴走ではなく、`往復1往復`・`光沢…異なる光沢` のような誤訳の副産物。

### 7.2 原因は 2 つ、効いたのは組み合わせ

1 は**デコーダ**。上流の `generation_config.json` は `num_beams: 12` を指定して
おり、FuguMT はビームサーチ前提で評価されたモデルなのに、当初の実装は `argmax`
だけだった。負の対数尤度を足すだけの貪欲法は、短く終わる経路を必ず好む。

もう 1 つは**入力が長すぎること**。「連続する散文行は 1 単位」という決めにより、
ユニット長は中央値 21 語・p90 45 語・最大 100 語だった。FuguMT は文単位のモデル。

同じ 62 ブロックを 4 通りで訳した。日本語の文字数は多いほうが、英語の残量
（＝プレースホルダを落として原文へ戻った量）は少ないほうがよい。

| | 日本語 | 英語の残り | セグメント数 | 語長 p90 / 最大 | 保全の脱落 |
|---|---|---|---|---|---|
| A 段落＋貪欲（従来） | 4850 | 151 | 95 | 45 / 100 | 1/39 |
| B 文分割＋貪欲 | 4684 | 175 | 123 | 32 / 64 | 2/41 |
| C 文分割＋ビーム 4 | 4655 | 201 | 123 | 32 / 64 | 3/41 |
| **D 文＋節分割＋ビーム 4** | **5172** | **141** | 156 | 25 / 54 | **0/43** |

**文分割だけ（B）では良くならない。**短くなった入力でも貪欲デコードは同じように
打ち切るし、打ち切りが起きたときの損失が文単位で見えるようになるぶん、英語の
残りはむしろ増える。ビームサーチだけを足しても（C）まだ足りない。

**足りなかったのは `;` と `:` での分割だった。**`.` が無い 30 語級の文が残って
いて、そこが同じように切られていた。

```
Translation is serialised so that ... runs one inference at a time; X0Q's pool would otherwise happily start hundreds.
  分割なし -> 変換はシリアライズされ、…一度に1つの推論を実行する。   ← セミコロン以降が消滅
  分割あり -> …／ X0Qのプールは、そうでなければ数百から始まります。
```

D はブロック単位で **37 件が改善、18 件が悪化、7 件が同じ**。保全の脱落は
**0 件**になった（A は 1、C は 3）。

### 7.3 ビームサーチの費用

40 セグメント、f32、4 コア:

| 幅 | 1 セグメント平均 | p90 | 常駐 |
|---|---|---|---|
| 1（貪欲） | 304 ms | 465 ms | 283.9 MiB |
| 2 | 474 ms | 652 ms | 283.8 MiB |
| **4（既定）** | **663 ms** | 1013 ms | 287.6 MiB |

**幅 4 で 2.2 倍**、メモリは +3.7 MiB。上流の 12 は採らなかった。費用は幅にほぼ
比例するが、質はそうではない。

2.2 倍で済んでいるのは 3 つの理由による。(1) `lm_head` を最後の位置だけに掛ける
ようにした（貪欲法にも効く）。(2) 文分割で 1 セグメントが短くなった。(3) candle の
marian はもともとクロスアテンションの K/V を毎ステップ計算し直しており、そこが
支配的だった。

**ビームの KV キャッシュは持てない。**candle の `marian` は `kv_cache` が private
で、公開されているのは `reset_kv_cache()` だけ。ビームを並べ替えるときにキャッシュ
を置換できず、置換しないまま使うと**あるビームの前文が別のビームのものとして
扱われる**。そのため毎ステップ前文を読み直しており、訳文長に対して二乗になる。
取り戻すには marian のフォークが要る——**6.5 のバッチ化と同じ場所**なので、
やるならまとめて 1 回。

### 7.4 直していないもの

- **用語（7.1 の 4）。**`gloss` → 光沢 は残っている。**マスクを外しても直らない**
  ことを 12.7 で実測した。後処理で日本語を文字列置換する辞書は入れられない
  （同じく 12.7 に反例）。
- **マスクによる語義の劣化（7.1 の 3）。**保全とのトレードオフ。**測り方と実測は
  §12。**自動指標で決着がつくのは保全だけで、読みやすさの判定は
  `docs/masking-ab.md` に人間が埋める欄として置いてある。
- ~~**`{@code ...}` などの Javadoc インラインタグ。**前処理が見ていない。~~
  → **7.5 で直した**（構文まるごと 1 スパンとして退避する）。
- **節がまるごと消える（7.1 の 1）。**カンマのあとに関係節が来る形（`, which …`）
  だけは **7.6 で直した**。それ以外の位置で節が落ちるのは残っている——`, and …` や
  `, so …` で切るのは測っていないし、`§12` の腕 D のように「落ちたのを検知して
  訳し直す」検出器も無い（プレースホルダは戻ってきてしまうので既存の番人は鳴らない）。

### 7.5 Javadoc のインラインタグを 1 つのスパンにした

7.4 に「前処理が見ていない」と書いた `{@code ...}` を直した回。**Issue #31。**
実測はモデルパック（FuguMT en-ja、f32、幅 4 ＝既定、CPU、release）に対し、手書きの
`%%%` 区切りファイルを `examples/probe.rs` に流して取った。

**壊れていたのは中括弧と中身で、タグ語ではない。**`protected_span` が語を断るのは
直前が語の文字のときだけで、`{` は語の文字ではない。だから `@code` には前から
`doc_tag` が当たっていて、エンジンに素通しされていたのは**中括弧と、その間の本文**
だった。エンジンは句読点を自由に書き換えるので、こうなる:

| コメント | エンジンが受け取る形 | エンジンの答え | 読者に出る訳 |
|---|---|---|---|
| `Calls {@code findUserById} to load the user.` | `Calls {X0Q X1Q} to load the user.` | `X0Q X1Q} を呼び出してユーザーをロードします。` | `@code findUserById} を呼び出して…` |
| `Use {@literal a < b} in prose.` | `Use {X0Q a < b} in prose.` | `X0Q a < b} を散文で使用します。` | `@literal a < b} を散文で使用します。` |

`{` が落ち、閉じ括弧だけが残る。**Javadoc として読めない文字列**が読者に出る。
`{@literal a < b}` では中身の `a < b` がそのままエンジンへ渡っているのも見える。

**毎回壊れるわけではない。**同じ実測で `{@link UserRepository#load}` /
`{@value #DEFAULT_TIMEOUT}` / `{@inheritDoc}` は括弧ごと戻ってきた。壊れるかどうかは
文しだいで、そこが厄介でもある。

**入れた規則**は `preserve.rs` の `inline_doc_tag`。`{@name …}` を**中括弧を含めて
1 スパン**として退避し、`SpanKind::Tag` に数える。

- **タグ名の表ではなく形で当てる。**ファミリは増え続け（`{@snippet}`、
  `{@systemProperty}`…）、JSDoc には JSDoc の一覧がある。表から漏れた名前は上の壊れ方へ
  黙って戻るので、`{@` ＋英字という**形**だけを見る。インラインコードがバッククォート、
  タグが `@` ＋英字で当たっているのと同じ書き方。
- **中括弧の深さを数える。**`{@code new int[]{1, 2}}` は 2 つめの `}` で閉じる。
- **閉じていなければ当てない。**`}` が無い、あるいは `}` が次の行にある `{@` は
  素通しし、従来どおり `@code` だけがタグとして当たる。バッククォートが 1 つだけの
  ときに `inline_code` が当てないのと同じ理由——コメントの残り全部を飲み込むほうが
  害が大きい。
- **`{@link Foo#bar the display label}` の表示ラベルは訳されなくなる。**構文ごと
  隠すので中の散文も一緒に隠れる。英語のまま出るのは読者に見えるが、壊れた構文は
  見えない。見えるほうを採った、という判断。

**直った形**（同じパック・同じ設定）:

```
Calls {@code findUserById} to load the user.
  -> ユーザーをロードするために {@code findUserById} を呼び出す。
Use {@literal a < b} in prose.
  -> 散文で{@literal a < b}を使用します。
```

**この規則は今日のリポジトリでは 1 回も当たらない。**`grep -rn '{@' --include=*.rs crates/`
は 0 件、凍結コーパス（`crates/codegloss-translator/tests/fixtures/comment-corpus.txt`）も
0 件。パーサが読むのは Rust だけで、`{@code}` は Java / JSDoc の構文だから。したがって
**既存の訳への影響は無い**：§12 のハーネス（62 ブロック / 156 断片）を規則の前後で走らせ、
採点表・スパンの損失・用語プローブ・A/B シートが 1 バイトも変わらないことを確認した。
直りかたを測れるのは、いまのところ手書きのコメントに対してだけである。

**残った失敗は括弧のせいではない。**`Returns the {@code User} record for the id, see
{@link UserRepository#load}.` は規則を入れても英語のまま出る。マスク後は
`Returns the X0Q record for the id, see X1Q.` で、エンジンが `, see X1Q` の節を丸ごと
落とし、`X1Q` が戻らないので断片が原文へフォールバックする。これは 7.1 の 1（節がまるごと
消える）で、インラインタグとは別の問題。

`preserve` の出力が変わるので `PIPELINE_VERSION` は `3` に上げた。

### 7.6 カンマのあとの関係節を 1 つの文として切った

7.1 の 1（節がまるごと消える）のうち、**カンマのあとに関係節が来る形**を直した回。
**Issue #31。**実測はすべてモデルパック（FuguMT en-ja、f32、幅 4 ＝既定、CPU、release）
に対して取った。

#### コーパス

正直な母集団は**サーバが実際に見る形のコメント**なので、凍結コーパス（12.1 の 62 ブロック）
ではなく抽出器で作り直した。この枝（`97b66bd`）で:

```sh
cargo run -p codegloss-parser --example extract -- $(find crates -name '*.rs') > /tmp/corpus.txt
```

**1093 ブロック / 1817 断片**（規則を入れる前の断片数）。12.1 のとおり凍結コーパスは
長いブロックに偏っているので、長い文についての規則はこちらで測るほうが実態に近い。

#### どの規則がどれだけ当たるか

| 規則 | 切る断片 |
|---|---|
| **カンマ ＋ 関係代名詞、両側 4 語以上（入れたもの）** | **79（4.3%）** |
| カンマなら何でも、両側 4 語以上 | 737 |
| カンマなら何でも、両側 6 語以上 | 513 |
| カンマなら何でも、両側 8 語以上 | 319 |
| カンマなら何でも、両側 10 語以上 | 170 |

**カンマなら何でも切る規則は広すぎる。**6 語で 1817 中 513＝6 断片に 1 つ。7.2 は
「分割だけでは悪くなる」を実測している（ビームサーチが無かった頃の話とはいえ）ので、
6 つに 1 つを切るのはその実験のやり直しになる。**23 に 1 つ**を、しかも関係節が開く
ところだけで切るのは別の話。

当たった 79 件の内訳は `which` 74・`where` 4・`whose` 1。`who` / `whom` は 0 件で、
表に入れてあるのは同じ構文だからであって測ったからではない。

**誤爆は語数ではなく形が防いでいる。** `1,000` はカンマの後ろに空白が無い
（`split_sentences` はどの境界にも空白を要求する）。`however,` はカンマが語の**後ろ**
なので次の語が関係代名詞にならない。列挙（`a, b, and c`）・同格（`the cache, a bounded
map, is shared`）も同じ試験で落ちる。同梱のフィクスチャ
`crates/codegloss-core/tests/fixtures/line_comments.txt` のカンマは `and` が続くので
切れない——**6 語規則なら切れていた**。

凍結コーパス（12 のハーネスが使うもの）では **156 → 163 断片**、7 件（4.5%）が動く。

#### 左側の断片の終わり方（これが決め手だった）

同じ 79 件を 4 通りに訳し、**日本語の文字数**で採点した（分割側は左＋右の合計、
±5 文字は「同じ」）:

| 左側の終わり方 | 長い / 短い / 同じ | 文字数 |
|---|---|---|
| 分割しない（1 文のまま） | — | 3140 |
| `,` のまま | 26 / 19 / 34 | 3385 |
| カンマを落とす | 28 / 16 / 35 | 3393 |
| **`.` にする（採用）** | **31 / 10 / 38** | **3538** |

**カンマを残すと 26 対 19 で、出す価値が無い。**末尾のカンマはモデルに「文はまだ
終わっていない」と伝えるので、終わっていない日本語が返る。カンマを落とすだけでも
ほぼ変わらない——**効いているのは句点であって、カンマの除去ではない**。

```
Losing the second race means another process got there first,
  -> 2回目のレースで負けるということは別のプロセスが最初に        ← 動詞が無い
Losing the second race means another process got there first.
  -> 第2レースで負けたということは、別のプロセスが最初に着いたことを意味します。

Opening this pack has to succeed,
  -> この箱を開ければ成功です                                    ← pack → 箱
Opening this pack has to succeed.
  -> このパックを開けることは成功しなければならない。
```

**短くなった 10 件は全部読んだ。**読者が失うものがあるのは 4 件:

```
The directory X1Q downloads into, which is the same one the glosses go to:
  分割なし ディレクトリ X1Q は、gloss が行くのと同じディレクトリにダウンロードします。
  分割あり ディレクトリ X1Q は . / 光沢が行くのと同じです            ← 左が壊れた
… which is what makes that promise checkable - a handler that …     ← checkable の節が消えた
Translation is serialised as a result, which costs nothing: …       ← 「費用はかからない」が消えた
… leaves the peak alone, which over-reports rather than under-reports.
  分割あり … / 過度な報告ではなく                                  ← 後半が消えた
```

残り 6 件は短いだけで内容は両側に載っている。むしろ分割したほうが読める例もある:

```
Everything slow happens on the worker task, which asks the client to refetch its hints once results are in.
  分割なし 全てが遅くなると、ワーカタスクが実行され、結果が出たら、クライアントにそのヒントを再取得するように要求される。
  分割あり 作業者のタスクでは、すべてが遅くなります。／ 結果が出たら、クライアントにヒントの再取得を要求します。
```

**長くなった 31 件のほうは、ほとんどが「節が丸ごと戻ってきた」である。**

```
Such a translation is discarded and the English source is returned instead, which is wrong in an obvious way rather than in a subtle one.
  分割なし このような翻訳は破棄され、代わりに英語のソースが返されます。          ← which 以下が消滅
  分割あり 同上 ／ 微妙なことではなく、明らかな方法で間違っているのです。
```

つまり **31 件の救出に対して、読者が失うのは 4 件**。どちらの方向も読者には見えない
（訳文は日本語として自然なまま）ので、数を数えるしかなく、そして接戦ではない。

#### `;` と `:` には効かない（一般化しないこと）

同じ書き換えを、いま出荷している `;` / `:` の分割が作る断片**全 341 件**に当てた:

| | 日本語の文字数 |
|---|---|
| そのまま（`;` / `:` で終わる） | 7978 |
| 終端を `.` に置き換える | 7929 |

6 件が長く、12 件が短い。**利得は無く、わずかに悪い。**「断片の終端をぜんぶ揃える」
という一般化は、既存のセミコロンの訳をすべて作り直させてこの結果になる。だから
`engine_form` はカンマだけを見る。ユニットテストで固定してある。

#### ビーム幅では届かない

「デコーダの問題ならデコーダで直せ」への答え。節を落とす文を幅 4 / 8 / 12 で訳した
（`MAX_NEW_TOKENS` は 512、`LENGTH_PENALTY` は 1.0 なので、打ち切りの上限には
当たっていない）:

| 文 | 幅 4 | 幅 8 | 幅 12 |
|---|---|---|---|
| `Returns X0Q once the queue is closed, which happens …` | 消える | **戻る** | 消える |
| `Dropping it closes the socket …, which is why the shutdown is not graceful.` | 消える | 消える | 消える |

**単調ですらない。**幅を上げれば直るものではないし、8.2 は幅 12 を費用で却下済み。
前処理から手が届く梃子は分割だけで、それは効く。

#### マスクは節の脱落を「増やす」

7.1 の 1 の例は**マスクしていない**形で書かれている。同じ文を並べると:

```
Returns None once the queue is closed, which happens when the server is shutting down.
  -> キューが閉じられたら None を返します。これはサーバーがシャットダウンしているときに発生します。
Returns X0Q  once the queue is closed, which happens when the server is shutting down.
  -> キューが閉じられたら X0Q を返します。                       ← 節が消える
```

**パイプラインが送るのはマスクした側**なので、7.1 の例と
`tests/decoding.rs::beam_search_keeps_a_clause_that_greedy_drops`（マスクなしの文を
使っている）は、どちらも実際の入力を再現していない。これは 1 例であって測定ではないが、
§12 / Issue #32 のマスク論争にそのまま効く材料でもある（測るならあちら側で）。

#### 入れたもの

`sentence.rs` に `RELATIVE_OPENERS`（`which` / `who` / `whom` / `whose` / `where`）と、
`is_a_boundary` のカンマ用の枝（両側 `MIN_CLAUSE_WORDS` ＝ 4 語以上）。
`TERMINATORS` / `CLAUSE_TERMINATORS` / `OPENERS` / `opens_a_sentence` /
`is_abbreviation` は触っていない。

**句点はエンジンへ渡す側にだけ付く。**`split_sentences` は原文のスライス
（`Vec<&str>`、カンマ付き）を返したままで、`GlossPlan::segments()` が
`engine_form` を通す。理由は 2 つ:

1. **断片が原文へフォールバックしたときの英語が、切り出し元の散文と 1 バイトも
   変わらない。**`Masked::unmask_fragment(sentence, translated)` の第 1 引数は
   「その断片がどのプレースホルダを持っていたか」と英語の作成にしか使われず、
   合成した句点はプレースホルダを持たない。`join_sentences` はカンマの後ろに空白を
   入れるので、`plan.restore(plan.sources())` は `plan.source()` と一致する。
2. 公開シグネチャ（`Vec<&str>`）を変えずに済み、§12 のハーネスにも波及しない。

**ただし「入力をそのまま返すエンジンが原文を再現する」という言い方は、これで
カンマ分割のユニットには当てはまらなくなる。**エンジンが受け取る断片には原文に無い
句点が入っているので、それを echo すればその句点が返る。厳密な往復として言えるのは
`restore(sources()) == source()` のほうで、テストもそちらで書いた
（`docblock.rs::a_comma_split_reaches_the_engine_terminated_and_falls_back_untouched`）。

~~なお**これは今回入った性質ではない**。`join_sentences` は `.` の後ろに空白を入れない
ので、以前から `/// Returns the user. Nothing is cached.` を passthrough で通すと
`Returns the user.Nothing is cached.` になる（実測）。英語が出るのはモデルパックが
無いときとフォールバックのときだけなので目立っていないが、**別の欠陥として issue に
する価値がある**。今回の変更はその形をもう 1 つ増やしただけで、新種ではない。~~
→ **7.7 で直した**（Issue #49。空白を落とす終端文字を日本語の句読点だけにした）。

#### §12 の採点表（前後）

凍結コーパスに対してハーネスを規則の前後で走らせた
（`cargo test -p codegloss-translator --features candle --release --test pipelines -- --ignored`）:

```
前（62 ブロック, 156 断片）              後（62 ブロック, 163 断片）
arm             japanese  english  spans lost    japanese  english  spans lost
A hide everything   5103        0     0 / 61  ->     5162        0     0 / 61
B hide nothing      5166        0    22 / 61  ->     5158        0    22 / 61
C keep identifiers  5098        0     3 / 61  ->     5154        0     3 / 61
D verify, else A    5151        0     0 / 61  ->     5138        0     0 / 61
```

**出荷している腕 A は日本語が 5103 → 5162 に増え、英語の残りとスパンの脱落は 0 の
まま。**動いたのは 7 断片（156 の 4.5%）だけである。腕 B（何も隠さない）が
5166 → 5158 とわずかに減るのは上の「マスクが脱落を増やす」と整合する——隠さなければ
節はもともと残りやすいので、分割で買えるものが少なく、たまに損をする。
`arm_a_reproduces_the_shipped_pipeline` は通っている（ハーネスの組み立ても
`engine_form` を通したため。通し忘れるとこのテストが落ちる＝設計どおり）。

#### Issue #31 の例そのもの

```
/// Dropping it closes the socket and wakes every task blocked on accept, which is why the shutdown is not graceful.
  分割なし それをドロップすると、ソケットが閉じて、acceptでブロックされたすべてのタスクが起動します。
  分割あり ドロップするとソケットが閉じて、acceptでブロックされたすべてのタスクが起動します。シャットダウンが優雅でない理由です
```

`sentence` の出力が変わるので `PIPELINE_VERSION` は `4` に上げた。

### 7.7 英語の文の終わりが後ろの空白を飲むのをやめた

`join_sentences` が**訳文の断片をつなぐときに落としていた空白**を直した回。
**Issue #49。**実測は 2 通り: 症状と規則そのものの前後はモデル無し（入力を
そのまま返すエンジン）で、実エンジンでの影響範囲はモデルパック（FuguMT en-ja、
f32、幅 4 ＝既定、CPU、release）に対して取った。

#### 症状

翻訳単位が英語の文を 2 つ以上持つと、組み直した結果から**文の間の空白が消える**。
入力をそのまま返すエンジンと同じもの（`restore(segments())`）を通した実測:

```text
/// Returns the user. Nothing is cached.
  source()            Returns the user. Nothing is cached.
  restore(segments()) Returns the user.Nothing is cached.

/// Returns the user. Fails when the id is unknown. Nothing is cached.
  restore(segments()) Returns the user.Fails when the id is unknown.Nothing is cached.

// Really?! It does. Wait... The rest arrives later.
  restore(segments()) Really?!It does.Wait...The rest arrives later.
```

`;` と `:` の境界は影響を受けない（どちらも終端文字の集合に入っていない）。同じ実測で
`… at a time; the pool would …` と `… not graceful: requests …` は
`restore(segments()) == source()` が成り立っている。

#### 原因

空白を落とす条件が `['。', '！', '？', '．', '.', '!', '?']` で、**日本語の句読点と
ASCII の終端文字が同じ集合に入っていた**。`。` は切れ目を字が持っているので後ろに
空白を入れないのが正しく、ASCII のピリオドは持っていないので入れないと 2 つの文が
1 語になる。

#### 英語がこの経路を通る 2 つの場合

1. **モデルパックが無いとき**（`PassthroughTranslator`）。`config.rs` のフォールバック
   経路そのもので、README が「普通の初回」として書いている状態。`candle` feature 無し
   でビルドした場合と `--no-download` も同じ。
2. **断片がプレースホルダを落としてフォールバックしたとき**
   （`Masked::unmask_fragment`）。単位全体ではなく**その 1 文だけ**が英語に戻る。

どちらも hover と code lens にそのまま出る。code lens の 1 行化
（`code_lens.rs` の `single_line`）は空白で語に切ってから 1 つの空白でつなぐので、
**書かれなかった空白を戻せない**——`user.Nothing` は 1 語として通る。

#### どれだけ踏むか（入力をそのまま返すエンジン）

| コーパス | 継ぎ目 | うち ASCII 終端の後 | 変わる単位 | 変わるブロック |
|---|---|---|---|---|
| 凍結コーパス（12.1 の 62 ブロック / 95 単位 / 163 断片） | 68 | 35 | 29 / 95 | 25 / 62 |
| 抽出コーパス（1133 ブロック / 1166 単位 / 1982 断片） | 816 | 471 | 349 / 1166 | 341 / 1133 |

抽出コーパスは `cargo run -p codegloss-parser --example extract -- $(find crates -name '*.rs' | sort)`
をこの枝の分岐点（`c84bcf4`）で走らせたもの。**自リポジトリのコメントブロックの
3 つに 1 つ**が、モデルパックが届くまでのあいだ、どこかの文の切れ目を潰した形で
読者に出ていたことになる。しかも訳はディスクキャッシュに書かれるので、
その状態はダウンロードが終わっても消えない（`PIPELINE_VERSION` を上げる理由の 1 つ）。

#### どれだけ踏むか（実エンジン）

同じ 2 つのコーパスを FuguMT で訳し、断片の訳文をそのまま両方の規則でつないで
突き合わせた:

| コーパス | 継ぎ目 | うち ASCII 終端の後 | 変わる単位 | 断片の訳文の終わり（ASCII / 日本語 / どちらでもない） |
|---|---|---|---|---|
| 凍結コーパス | 68 | 0 | 0 / 95 | 0 / 149 / 14 |
| 抽出コーパス | 816 | 6（`.` 4、`?` 2） | 6 / 1166 | 26 / 1714 / 242 |

**凍結コーパスでは 1 件も動かない。**12 のハーネスが使うのはこのコーパスだが、
採点表が前後で 1 文字も動かない理由はこれとは別で、構造のほうにある（下記）。

**抽出コーパスでは 6 件動き、そのうち 3 件は英語 → 日本語の継ぎ目**で、
規則が直したかった形そのもの（1 文だけ英語に戻った断片の後ろに日本語の文が来る）。

#### 残る 3 件は日本語で、採否は判断であって実測ではない

残る 3 件は**日本語の文が半角の終端文字で終わっていた**もの。前後を全文引く:

```text
エンジン費は?記憶とコメントあたりの時間
  → エンジン費は? 記憶とコメントあたりの時間

`model_version`の答えは?これはすべてのキャッシュキーの一部であるため、…
  → `model_version`の答えは? これはすべてのキャッシュキーの一部であるため、…

… ディレクトリ `--fetch-model` は .光沢が行くのと同じです バックアップを…
  → … ディレクトリ `--fetch-model` は . 光沢が行くのと同じです バックアップを…
```

日本語の組版では半角の `?` の後ろにはアキを入れるのが普通なので、上の 2 件は
読みやすくなる側だと**読んだ**。3 件目はどちらにしても壊れた日本語で、
空白の有無で良くも悪くもならない。

**これは実測ではなく編集上の判断である。**測ったのは「1166 単位のうち 3 単位
（0.26%）が動く」ところまでで、その 3 件が良くなったか悪くなったかを決める指標は
無い。判断を避ける案（終端文字の**手前**の文字が日本語かどうかで決める）は、
出荷経路に「日本語かどうか」の判定を持ち込むうえ、3 件目には空白とバッククォートが
挟まっていて当たらないので採らなかった。

#### 判定は単位ごとではなく継ぎ目ごと

**「この単位の言語」という入力は作れない。**プレースホルダを落とした断片は
1 つだけ英語に戻るので、1 つの単位の中で日本語と英語が隣り合う。実測:

```text
join_sentences(["ユーザを返します。", "Fails when `id` is unknown.", "何もキャッシュされません。"])
  → ユーザを返します。Fails when `id` is unknown. 何もキャッシュされません。
```

**1 つ目の継ぎ目は空白なし、2 つ目は空白あり**で、これを 1 回の呼び出しで出す必要が
ある。対象言語を引数で渡す案は、`ja` なら 2 つ目が詰まり、`en` なら 1 つ目が開いて
しまうので、どちらの値でも間違える。直前の断片の最後の 1 文字だけが両方を出せる。

日本語の断片が識別子やプレースホルダで終わっている場合は、前から空白が入っていて
今回も変わらない（`["これを呼ぶのは find_user", "次の文。"]` →
`これを呼ぶのは find_user 次の文。`）。対象言語を `ja` で渡す実装はここを詰めてしまう。

#### 12 の採点表は動かない（構造上の理由と、その確認）

ハーネスの採点（`score` / `term_probe` / `sheet`）は**断片ごとの訳文**を見ていて、
つないだ単位を見ていない。`join_sentences` はハーネスの中では
`Corpus::glosses` の 1 箇所からしか呼ばれず、それを使うのは
`arm_a_reproduces_the_shipped_pipeline`（腕 A と `GlossPlan` の突き合わせ）だけで、
**両側とも同じ `join_sentences` を通る**。

確認した結果:

```text
                    前          後
62 ブロック / 163 断片
A hide everything   5162  0  0 (0)   0/61   →  5162  0  0 (0)   0/61
B hide nothing      5158  0 38 (35) 22/61   →  5158  0 38 (35) 22/61
C keep identifiers  5154  0  5 (5)   3/61   →  5154  0  5 (5)   3/61
D verify, else A    5138  0 17 (14)  0/61   →  5138  0 17 (14)  0/61
```

規則ごとの脱落表・用語プローブ・「D が 163 中 142 で unmask 済みの訳を採った」まで
1 行ずつ同じで、`CODEGLOSS_SHEET` が書く A/B シートも**前後でバイト単位で同じ**
（6025 バイト、md5 `71e17a91ac28777dfd9536a56cdcae76`）。出力の差はロード時間と
訳出時間の秒数だけだった。つまり**このハーネスはこの変更を検出する道具ではない**。
上の実測を専用のツールで取ったのはそのため。

#### 入れたもの

`sentence.rs` に `JAPANESE_TERMINATORS`（`。！？．`）を足し、`join_sentences` の条件を
`!joined.ends_with(JAPANESE_TERMINATORS)` にした。`TERMINATORS`（分割側が使う
「文を終える文字」）はそのままで、**2 つの表を 1 つにしないことがこの修正の中身**。

往復のテストは 2 つとも「単位あたり 1 文」の入力しか持っていなかったので
（それが通っていた理由でもある）、複数文の raw を足した:
`docblock.rs::a_passthrough_translation_restores_the_source_exactly` と、
新しい `restore_of_sources_is_the_source_for_every_shape`（`sources()` 側。
カンマ分割も入る）、フィクスチャ `crates/codegloss-core/tests/fixtures/prose.txt`、
サーバ越しの `crates/codegloss-lsp/tests/preservation.rs::a_comment_of_two_sentences_keeps_the_space_between_them`。
最後のものが読者の見る文字列を直接見ている唯一のテスト。

なお `docblock.rs` のコメントにあった「この経路を見るのはモデルだけで、通るのは
英語だけ」は**半分が誤り**だった。`PassthroughTranslator` もエンジンで、しかも
すべてのインストールが最初に動かすエンジンである。書き直した。

**Issue #49 の「範囲外」の但し書きも半分だけ正しい。**カンマ分割（#31 / #51）が
この欠陥を踏むのは**エンジンが入力を返す経路だけ**で、フォールバック経路では
踏まない——`sources()` はカンマを保ったままで、カンマは空白を落とす集合に
入っていないため、`restore(sources()) == source()` は修正前から成り立っていた（実測）。
逆に言うと、モデルパックが無いあいだ `…blocked on accept, which is…` が
`…blocked on accept. which is…` と出るのは 7.6 の `engine_form` によるもので、
今回の修正の対象ではない（原文に無い句点が読者に出る形なので、別途 issue にする値打ちはある）。

`sentence` の出力が変わるので `PIPELINE_VERSION` は `5` に上げた。日本語の訳文も
1166 単位のうち 3 単位が動くので、「英語（＝劣化モード）しか変わらない」という
逃げ道は無い。

## 8. marian のフォーク

7.3 で「ビームの KV キャッシュは持てない」と書いた壁を、フォークで越えた。
`crates/codegloss-translator/src/marian.rs` は candle 0.11.0 の
`models::marian`（MIT OR Apache-2.0）のコピーで、**上流のままでは外から手が
出せない 3 点だけ**を変えてある。

1. **`Decoder::reorder_kv_cache`。**上流は `kv_cache` が private で、公開されて
   いるのは `reset_kv_cache()` だけ。ビームを並べ替えるときにキャッシュを置換
   できず、置換しないまま使うと**あるビームの前文が別のビームのものとして
   扱われる**——訳文としては流暢なまま間違う、いちばん困る壊れ方をする。
2. **エンコーダとクロスアテンションのアテンションマスク。**上流は両方に `None`
   を渡す。6.5 でバッチ推論を諦めた理由がこれ。
3. **エンコーダ層を `is_decoder: false` で作る。**上流は `true` を渡していて、
   エンコーダ層が使いようのない KV キャッシュを持ち、ヘッド数を
   `decoder_attention_heads` から取る。FuguMT は 8 と 8 で一致するので数値は
   変わらないが、コードの意味とは違う。

`MTModel::decode` は落とした（causal mask を必ず F32 で作るので、F16 では
そもそも使えなかった）。`lm_head` と `final_logits_bias` も一緒に落として、
`CandleTranslator` が同じ `VarBuilder` から自前で読む（storage は共有される）。

**それ以外は上流のまま。**使っていない部分も残してある。上流の新版と diff を
取れることが、コピーを抱えていい条件だから。

### 8.1 速くなった

40 セグメント、f32、4 コア。フォーク前は毎ステップ前文を読み直していた。

| 幅 | フォーク前 | フォーク後 | 常駐 |
|---|---|---|---|
| 1（貪欲） | 304 ms | 329 ms | 284.2 MiB |
| **4（既定）** | 663 ms | **352 ms** | 287.7 MiB |
| 8 | — | 425 ms | 295.2 MiB |
| 12 | — | 498 ms | 303.8 MiB |

**幅 4 で 1.9 倍速くなり、貪欲法に対する上乗せは 2.2 倍から 1.07 倍になった。**
速さのために質を落とす理由がほぼ無くなったということ。

**訳文はフォークの前後で完全に一致する。**7.2 のコーパス 62 ブロックを両方で
流して `diff` が空だったことを確認した。速さだけが変わっている。

### 8.2 幅 12 は採らない（測った上で）

上流の `generation_config.json` は `num_beams: 12`。498 ms なら払えるので、
7.2 と同じコーパスで比べた。

| | 日本語 | 英語の残り | 保全の脱落 |
|---|---|---|---|
| 幅 4 | 5172 | 141 | 0/43 |
| 幅 12 | 5161 | 145 | 0/43 |

訳文は 38/62 で変わるが、良くなったのが 15、悪くなったのが 18。**差し引きゼロ、
わずかに悪い。**既定は 4 のまま。上流の値を継がなかったのは好みではなく実測。

### 8.3 candle-transformers を落とせた

`marian` が唯一の利用箇所だったので、依存から外した。`candle-core` と
`candle-nn` だけになる。

### 8.4 テストはモデルパック無しで走る

3 つの変更にはそれぞれユニットテストが付いていて（`src/marian.rs` の
`mod tests`）、8 次元・1 層のランダム重みモデルを組んで確かめる。重みが
でたらめでも困らない——見ているのは**アテンションがどの位置に届くか**で、
それは重みの中身によらない。

- マスクありでパディングしても、前の位置の出力が動かないこと
- **マスク無しだと動くこと**（＝上流がパディングしたバッチで静かにやっていること）
- `reorder_kv_cache` が行を複製して幅を広げ（1 → 4、探索の 1 歩目）、
  また狭められること（4 → 2、ビームが終端に達したとき）

実モデルが要るぶんは `tests/decoding.rs`（`#[ignore]`）にある。貪欲法が落とす
節をビーム探索が残すこと、バッチの順序が訳文を変えないこと、同じ入力が何度でも
同じ訳文になること。

## 9. バッチ推論

8 のフォークでエンコーダとクロスアテンションにマスクが入り、6.5 で諦めた理由が
消えた。長さ順に並べてグループに切り、パディングしてマスクする。

**注意：この節と 8.1 の絶対値は別のセッションで測っている。**同じコミットが
8.1 で 352 ms、この節の対照で 508 ms になる程度にはマシンの負荷で動く。
**比べていいのは同じ表の中だけ。**

### 9.1 速くなった

40 セグメント、幅 4、f32、4 コア。すべて同一セッション。

| 行数 | 1 セグメント | 常駐 |
|---|---|---|
| 対照（バッチ以前のコミット） | 508 ms | 285.6 MiB |
| 4（＝1 グループ 1 セグメント） | 526 ms | 285.9 MiB |
| 8 | 342 ms | 289.6 MiB |
| 16 | 213 ms | 297.4 MiB |
| **32（既定）** | **149 ms** | 312.2 MiB |
| 64 | 128 ms | 340.5 MiB |

**32 行で 3.4 倍**、常駐は +26 MiB。64 行にしても 1.16 倍しか伸びず、常駐は
さらに +28 MiB。**既定は 32**（`MAX_BATCH_ROWS`）。1 セグメントが `beams` 行を
占めるので、幅 4 なら 8 セグメントぶん。

行数 4 が対照とほぼ同じ（526 対 508）なのは、グループが 1 セグメントになると
バッチ以前と同じ計算になるから。**バッチ化の実装そのものに費用は無い。**

### 9.2 パディングは訳文を変えない

7.2 のコーパス 62 ブロック（156 セグメント）を、1 グループ 1 セグメント（＝
パディングなし）と 32 行のバッチの両方で流し、**出力がバイト単位で一致**した。
マスクが正しいということ。

モデルパック無しのユニットテスト（`src/marian.rs`）と実モデルのテスト
（`tests/decoding.rs`）の両方で、短い文をずっと長い文と同じバッチに入れても
訳文が変わらないことを見ている。

### 9.3 訳文が変わった原因はバッチではなく、探索の選択規則だった

バッチ化を入れたら 156 セグメント中 28 件（18%）で訳文が変わった。原因を
二分したところ、**バッチを切っても同じ差が出た**——犯人は同時に直した選択規則
のほうだった。

以前は、`beams` 個が終了した時点で**走行中の仮説も最終候補に含めていた**。
走行中の仮説は終端していない。終わることはタダではない（終端トークンがそこで
尤もらしい必要がある）ので、同じ長さなら走行中のほうがつねに良いスコアを出せて
しまう。**それを返すのは、モデルが終わらせなかった文を返すということ**で、
ビームサーチが消しに来た打ち切りそのものだった。

いまは終了した仮説を優先し、1 つも終了しなかった場合（トークン予算切れ）だけ
走行中のものへ落ちる。上流の実装と同じ。

品質は拮抗している。日本語 5172 → 5103 文字、英語の残りは 141 で変わらず、
終端記号で終わらない訳文は 12 → 11。**どちらが良いかは測っても決まらない。**
採ったのは規則が正しいからで、数字が良いからではない。

`ENGINE_VERSION` は `candle-marian-3` へ。

## 10. 未検証

- **量子化。**自前実装が要るので試していない（見立ては 6.8）。
- **`;` `:` 分割の下限語数。**`MIN_CLAUSE_WORDS = 4` は 1 つのコーパスで
  決めた値で、振ってはいない。
- **バッチのグループ分けの良し悪し。**長さ順に並べて切っているだけで、
  実際のファイルでどれだけパディングが無駄になるかは測っていない（9.1 の
  コーパスは短い文が多く、条件としては良いほう）。
- **`MAX_BATCH_ROWS` の環境依存。**4 コアでしか測っていない。コア数が違えば
  最適値も動くはずだが、設定にはしていない。
- **Linux 以外。**macOS / Windows では測っていない。GPU（CUDA / Metal）も同様。
  ディスクキャッシュの既定ディレクトリも Linux でしか確かめていない。
- **長いコメント。**入力は `max_position_embeddings - 1 = 511` トークンで
  切っている。それを超えるコメントで何が起きるかは測っていない。
- **ディスクキャッシュの実運用での大きさ。**上限 50,000 件が妥当かは、長く使った
  ディレクトリで測っていない。

## 11. モデルの遅延読み込み

起動時にモデルを読むのをやめ、**最初に何かを翻訳するときまで**遅らせた。
`CandleTranslator::load*` は `manifest.json` と `config.json` を読み、重みと
2 つのトークナイザがそこにあることを確かめて返る。重みを読むのは
`Translator::translate` の初回。

成立するのは `model_version` が `manifest.json` から作れるから。キャッシュキーは
LSP のリクエスト経路で引かれるので、これを答えるのにモデルが要るなら結局
最初の hover で読むことになり、何も得られない。**`model_version` を
`String` のフィールドに置いておくことが、この節の前提そのもの。**

### 11.1 測った値

f32、幅 4、release、`examples/measure.rs`。同一セッション、ページキャッシュが
温まった状態。

| | 常駐 | 時間 |
|---|---|---|
| プロセス開始時 | 3.8 MiB | — |
| パックを開いた直後 | 4.2 MiB | 1 ms 未満 |
| 重みを読んだ直後 | 280.8 MiB | 約 0.4 秒 |

**ディスクキャッシュが全件当たるセッションは 4.2 MiB で終わる。**280.8 MiB も、
起動時の 0.4 秒も払わない。

読む費用そのものは変わっていない（同じ手順を verbatim に移しただけ）。6 回ずつ
測って中央値 443 ms 対 396 ms（対照＝遅延なしの従来コミット）、どちらも
0.9〜2.3 秒の外れ値を出す。**この差はこの環境の揺れで、遅延の費用ではない。**
費用は環境で大きく動くほうが本質で、同じパックを debug ビルドで読むと 3.9 秒
かかる。

推論は変わらない。コーパス 65 セグメントで 1 セグメント平均 477 ms（対照
479 ms）、バッチ 157 ms/セグメント（対照 157 ms）。実モデルのテスト
（`tests/decoding.rs` / `tests/placeholders.rs` / `tests/quality.rs`）もそのまま
通る＝遅延して組み立てたエンジンは以前と同じもの。

サーバとして測っても同じ。`codegloss-lsp` に同じパックとキャッシュディレクトリを
渡して起動し、コメント 1 ブロックの Rust ファイルを開いて `textDocument/codeLens`
を投げ、終了直前に `/proc/<pid>/status` を読む。

| ディスクキャッシュ | 最初の code lens | 常駐（VmRSS） |
|---|---|---|
| 空 | `⟳ 翻訳中…`（訳ができ次第 refresh で差し替わる） | 289.9 MiB |
| 全件当たる | **最初から日本語** | **8.7 MiB** |

温かいほうのログに `loaded the translation model` は出ない。訳はディスクから
出ていて、モデルは 1 度も読まれていない。

Issue #33 が心配していたのは「遅延させると最初の翻訳に読み込み時間が乗って
体感が悪くなるのでは」だった。**測ったところ悪くならない。**同じサーバに
コメント 4 ブロックのファイルを didOpen し、**プロセス起動から日本語のレンズが
返るまで**を stdio 越しに測る（3 回）。

| ディスクキャッシュ | 対照（起動時に読む） | 遅延 |
|---|---|---|
| 空 | 1.44〜1.50 秒 | 1.37〜1.42 秒 |
| 全件当たる | 0.45 秒 | **0.01 秒** |

空のときの差は無い（揺れの範囲）。読む仕事は同じで、払う場所が変わっただけ
だから。全件当たるときの 0.45 → 0.01 秒が、起動時の読み込みがそのまま最初の
応答を待たせていたということ。

### 11.2 失敗したときどうなるか

パックを**開く**段階の失敗（`manifest.json` が無い・壊れている、`config.json`
が読めない、重みやトークナイザのファイルが無い）は従来どおり。`config::load`
が `None` を返し、サーバは英語のまま動く。

変わったのは、**開けたのに読めなかった場合**。そこではもう
`PassthroughTranslator` へ落とせない——キャッシュキーは candle の
`model_version` で、英語をそこに書き込むと（`GlossCache::insert` はディスクへ
書き抜ける）パックを直したあとも英語が出続ける。なのでバッチを失敗させる。
hover はキャッシュミスのときと同じく**原文の英語を返し**、code lens が
`⟳ 翻訳中…` のまま止まる。**ログにはパックの場所と理由を 1 行**出す（止まった
レンズはそれで追える）。

失敗は覚えて読み直さない。壊れたパックを `didChange` のたびに 120 MB 読み直す
のを避けるため。一時的な失敗（そのときメモリが足りなかった等）も同じく失敗し
続けることになるが、これは割り切りで、直し方はサーバの再起動。

## 12. マスク方針の測り方と実測

§7.4 に「**マスクによる語義の劣化**。保全とのトレードオフで、どちらが良いかを
決める判断材料が無い」と書いた。Issue #32 が求めているのはその判断材料そのもの
である。この節はその**測り方**と、いま測った値を記録する。

**この回で挙動は 1 バイトも変えていない。**`PIPELINE_VERSION` も据え置き
（`preserve` / `sentence` / `docblock` の出力は不変で、`codegloss-core` の
`tests/preserve.rs` にマスク結果を書き下したピン留めを足してある）。既定を
変えるのは、下の数字と `docs/masking-ab.md` の人間の判定が揃ってから。

道具は `crates/codegloss-translator/tests/pipelines.rs`。

```sh
CODEGLOSS_MODEL_PACK=~/codegloss-model \
  cargo test -p codegloss-translator --features candle --release \
  --test pipelines -- --ignored --nocapture
```

測定環境は文書冒頭と同じ（4 コア、f32、幅 4、release、CPU）。重みも §7 / §9 と
同一で、`pytorch_model.bin` の SHA-256 は `8b2d3d3b7da2…`、121,192,965 バイト。
手元のパックの `manifest.json` は `model_version` を `fugumt-en-ja-OTHER` と
名乗っているのでキャッシュキー上の名前だけは違うが、重みは同じで、**下の
「日本語 5103 文字」が §9.3 と一致すること**でも裏が取れている。

### 12.1 コーパスをリポジトリに固定した

§7.2 と §9.3 の 62 ブロックは、これまでリポジトリのどこにも無かった。**再現でき
ないコーパスの上の数字は誰も検算できない**ので、
`crates/codegloss-translator/tests/fixtures/comment-corpus.txt` として凍結した。

内訳（現在のソースと照合して確認した）:

| 出どころ | ブロック |
|---|---|
| `crates/codegloss-lsp/src/translation.rs` | 36 |
| `crates/codegloss-core/src/cache.rs` | 23 |
| `crates/codegloss-parser/src/lib.rs` | 1 |
| 抜いたあとにコメントが編集され、現在のソースには一致しない | 2 |

**全部このリポジトリ自身のコメント（MIT）である。**抜かれた時点はおよそ
`26b4359`（「(P9)」と「The task that owns the engine.」が両方あった main 上の
最後のコミット）。凍結標本なので、抽出コマンドの出力と一致することを検証には
しない——無関係なコメント修正でビルドが落ちるだけだから。

**そしてこの標本は `codegloss-parser` の抽出器が作るものではない。**同じ 3
ファイルを `examples/extract.rs` に通すと、いまの main で 109 ブロック、
`26b4359` で 96 ブロックになり、62 ブロックのうちそのまま一致するのは 4 件だけ
だった。差の理由は全部同じで、**空の `///` / `//!` 行が連結を切る**（AGENTS.md）
ため抽出器は doc コメントを段落ごとに分ける。凍結標本のほうは段落をまたいだ
長いブロックを持っている。

→ 含み: **§7 の数字は、いまサーバがエンジンに渡している単位より長いブロックの
上で測られている。**4 腕とも同じ入力なので比較としては成立しているが、母集団と
しては実際より長いほうに偏っている。

コーパスは `%%%` 区切りのテキストなら何でもよく、`CODEGLOSS_CORPUS` で差し替わる。

```sh
cargo run -p codegloss-parser --example extract -- path/to/src/*.rs > /tmp/corpus.txt
CODEGLOSS_MODEL_PACK=~/codegloss-model CODEGLOSS_CORPUS=/tmp/corpus.txt \
  cargo test -p codegloss-translator --features candle --release \
  --test pipelines -- --ignored --nocapture
```

**第三者のソースから作ったコーパスはリポジトリに置かない**（コメント文もその
著作物の一部）。裸の識別子は Rust では珍しい——バッククォートで囲む文化がある
ため——ので、識別子の是非を問うには識別子が濃いコーパスが要る。それは手元の
チェックアウトから上のコマンドで作る。

### 12.2 4 つの腕

| | エンジンに渡すもの | 戻し方 |
|---|---|---|
| A | 保全対象を全部隠す（**出荷しているもの**） | 断片ごとに unmask |
| B | 何も隠さない | そのまま（戻すものが無い） |
| C | コード・URL・タグ・注意書きは隠し、**裸の識別子だけ本文に残す** | 断片ごとに unmask |
| D | — | B の訳のうち保全が全部原文どおり返った断片を採り、残りは A |

腕はすべて `codegloss-core` の公開 API だけで組んであり、**出荷経路には触れて
いない**。順序は `mask → split_sentences → その腕が明かす種別だけ本文へ戻す`。
この順序は譲れない: `split_sentences` は URL や `a.b()` の中のピリオドを境界と
誤認しないために**マスク後**を前提にしており、先に明かすと文分割が壊れて
§7.2 で節分割が稼いだぶんを丸ごと失う。

腕 A が `GlossPlan` と 1 バイト違わないことは、モデル無しのテスト
（`arm_a_reproduces_the_shipped_pipeline`）でコーパス全件について固定してある。
ここが落ちたら下の表は無効。

### 12.3 指標と、それぞれが答えられないこと

| 指標 | 何を数えるか | 答えられないこと |
|---|---|---|
| 保全の脱落 | 断片が預かったスパンのうち、最終的な訳文に**同じ綴りで**現れないもの。規則（`SpanKind`）ごと | 識別子が**正しい位置**に置かれたか。存在しか見ていない |
| 英語へ落ちた断片 | プレースホルダが返らず訳を捨てた断片の数 | 読者にとっての損失の大きさ |
| A からの乖離 | 腕 A と出力が違う断片の数（空白正規化ありとなし） | どちらが良いか |
| 日本語の文字数 | 訳文中の仮名・漢字の数 | **どちらが良いか。継続用の指標であって判定には使わない**（12.6） |
| 用語プローブ | 一般語に流れた訳語の出現（下記） | 語彙表の外。しかも `stray` 列が示すとおり部分文字列判定そのものが危うい |

### 12.4 実測 — コーパス 1（自リポジトリ 62 ブロック / 156 断片）

```
arm                  japanese   english   differ (not space)   spans lost
A hide everything         5103         0           0 (  0)            0 / 61
B hide nothing            5166         0          38 ( 35)           22 / 61
C keep identifiers        5098         0           5 (  5)            3 / 61
D verify, else A          5151         0          17 ( 14)            0 / 61
```

規則ごとの脱落（脱落 / 預かり）:

| | Code | Identifier |
|---|---|---|
| A 全部隠す | 0 / 55 | 0 / 6 |
| B 何も隠さない | **19 / 55** | **3 / 6** |
| C 識別子だけ残す | 0 / 55 | 3 / 6 |
| D 検証して落とす | 0 / 55 | 0 / 6 |

（Placeholder / Url / Tag / Marker はこのコーパスでは預かりが 0 件。）

- **保全対象を 1 つも含まない 113 断片は、3 腕とも出力がバイト一致した。**エンジンは
  決定的で、観測した差はすべて入力の差から来ている（比較実験として成立している
  ことの対照）。
- **D は 156 断片中 135（87%）で B の訳を採れた。**残り 21 だけが A へ落ちる。
- 所要時間: A 57.4 秒 / B 66.7 秒 / C 60.5 秒（コーパス全体を 1 回の
  `translate` に渡している）。2 テストで 186 秒。**1 回の実行の値**で、
  実行ごとに数秒ぶれる（同じ表を出す再実行は 178 秒だった）。

用語プローブ（`hit / at risk` と、英語に語が無いのに訳語が出た `+stray`）:

| | gloss/光沢 | store/店 | clock/時計 | queue/キュー | value/値 |
|---|---|---|---|---|---|
| A 全部隠す | 8/22 +0 | 2/16 +0 | 1/1 +0 | 8/9 +1 | 2/3 +1 |
| B 何も隠さない | **8/22 +0** | 1/16 +0 | 1/1 +0 | 8/9 +1 | 3/3 +1 |
| C 識別子だけ残す | 8/22 +0 | 1/16 +0 | 1/1 +0 | 8/9 +1 | 2/3 +1 |
| D 検証して落とす | 8/22 +0 | 1/16 +0 | 1/1 +0 | 8/9 +1 | 2/3 +1 |

### 12.5 実測 — コーパス 2（識別子が濃い 55 ブロック / 85 断片）

第三者の Rust（LSP まわりのコード）から、裸の識別子を含むブロックだけを集めた
もの。**リポジトリには入れていない**（12.1）。預かるスパンの構成が違う:
Identifier 77、Code 11、Url 7、Marker 3。

```
arm                  japanese   english   differ (not space)   spans lost
A hide everything         2199         2           0 (  0)            0 / 98
B hide nothing            2260         0          50 ( 47)           19 / 98
C keep identifiers        2273         1          44 ( 40)            7 / 98
D verify, else A          2240         2          34 ( 32)            0 / 98
```

| | Code | Url | Marker | Identifier |
|---|---|---|---|---|
| A 全部隠す | 0 / 11 | 0 / 7 | 0 / 3 | 0 / 77 |
| B 何も隠さない | 5 / 11 | 4 / 7 | 1 / 3 | **9 / 77（11.7%）** |
| C 識別子だけ残す | 0 / 11 | 0 / 7 | 0 / 3 | **7 / 77（9.1%）** |
| D 検証して落とす | 0 / 11 | 0 / 7 | 0 / 3 | 0 / 77 |

- **D は 85 断片中 69（81%）で B の訳を採れた。**
- 保全対象を含まない 24 断片は、ここでも 3 腕とも一致した。
- 所要時間: A 39.4 秒 / B 61.9 秒 / C 40.4 秒、2 テストで 143.6 秒。

### 12.6 決まったことと、決まらないこと

**決まったこと 1: 日本語の文字数ではこの問いに答えられない。**マスクあり 5103 に
対しマスク無し 5166（+1.2%）、識別子が濃いコーパスでも 2199 対 2260（+2.8%）。
**腕の差より指標の粒度のほうが粗い**うえ、この指標は冗長な訳と繰り返しを加点する。
表には残すが、判定には使わない。

**決まったこと 2: 失敗の重さが非対称である。**マスクありで失敗すると
プレースホルダが返らず**原文の英語がそのまま出る**——読者に見える失敗で、
コーパス 1 で 0 件、コーパス 2 で 2 件。マスクを外して失敗すると**誤訳された
識別子が日本語の中に紛れて出る**——読者に見えない失敗で、コーパス 2 の識別子で
9/77、コーパス 1 のインラインコードで 19/55。**同じ「1 件」ではない。**
だから決着の軸は流暢さではなく保全になる。

**決まったこと 3: 「保全を検証してから採る」（D）が強い。**保全を 1 件も犠牲に
せずに、マスク無しの訳を**コーパス 1 で 135/156、コーパス 2 で 69/85** 採れる。
費用は落ちた断片ぶんの 2 回目の推論だけ。**この回では入れない**——「B の訳の
ほうが良い」が人間の判定でまだ確かめられていないので、入れると 87% の断片を
未検証の理由で置き換えることになる。

**決まらないこと: どちらが読みやすいか。**自動指標で分けられない断片は
コーパス 1 で 14 件、コーパス 2 で 31 件。伏せ字にして人間に渡す
（`docs/masking-ab.md`）。**14 件では符号検定が効かない**（片側 p<0.05 に届くのは
11 勝以上で、真の選好が 70:30 でも検出力 0.36）。コーパス 2 の 31 件と合わせて
45 件前後にすると検出力 0.84。

### 12.7 用語（§7.1 の 4）はマスクとは別の問題である

**マスクを外しても直らない。**`gloss` を含む 22 断片のうち 光沢 が出るのは
マスクありで 8 件、マスク無しでも 8 件、識別子だけ残しても 8 件。Issue #32 が
「2 つの別々の問題」と書いているとおりで、実測でも別々だった。

**そして後処理で日本語を文字列置換する用語辞書は入れられない。**上の表の
`+stray` 列が反例そのもので、`キュー` は原文に `queue` の無い断片に 1 件、
`値` も `value` の無い断片に 1 件出ている。前者は `エグゼキュータ`（executor の
訳）の中に、後者は `価値` の中にある。加えて、**どの日本語がどの英単語から来た
のかを教える対応情報はどこにも無い**ので、正しい 店 と誤った 店 を区別できない。
入れるなら、保全と同じ「マスクして、戻すときに訳語を入れる」機構になる。別の
Issue。

### 12.8 未検証

- **識別子の位置。**腕 D の「原文どおり返ったか」は `contains` による存在判定で、
  位置は見ていない。識別子が誤った節に置かれた訳を通してしまう。
- **`looks_like_code` が拾いすぎていないか。**「そもそも保全しない」という第 3 の
  梃子は誰も測っていない（狭めると `PIPELINE_VERSION` が動く）。
- **Rust 以外。**裸の識別子の密度はバッククォート文化の有無で変わるはずだが、
  parser が Rust しか読めないので測れない。言語が増えたら測り直しになる。
- **用語プローブの語彙表の置き場所。**同じ表から作った辞書を同じ表で採点すると
  循環する。半分を保留するのか、辞書の採点は人間の A/B だけにするのか。
- **コーパス 1 の偏り。**自リポジトリのコメントなので CodeGloss 自身の語
  （`gloss`）が 22 断片にも出る。用語プローブの母集団としては偏っている。
- **エンジンを変えたときの再測定。**幅・精度・モデルを変えるたびに腕の比較は
  やり直しになる。1 回 2〜3 分なので手で回す道具であり、CI に載せる話ではない。

## 13. コードフェンスの中で連結を切らない（Issue #53）

`///` の doctest が**ブロックに割れ、開きも閉じもフェンスが消える**ので、
`docblock.rs` にコードだと分からず、エンジンに散文として渡っていた。この節は
その実測と、直したあとの実測を記録する。

**この回で `PIPELINE_VERSION` は上げていない。**理由は 13.7。

### 13.1 不具合の再現

Issue #53 の断片をそのまま `examples/extract.rs` に通す。

```sh
cargo run -q -p codegloss-parser --example extract -- /tmp/issue53.rs
```

直す前は **4 ブロック**で、開きの ```` /// ``` ```` も閉じの
```` /// ``` ```` も `/// }` も、どのブロックにも入っていない。直したあとは
**3 ブロック**で、3 つめが開きから閉じまでの 8 行そのものになる。

同じ 4 ブロックを実モデル（FuguMT en-ja、f32、幅 4、CPU）に通した訳:

| 原文（セグメント） | 訳 |
|---|---|
| `let mut pos = 0;` | `mut pos = 0 とする。`（`let` が消える） |
| `while pos < X0Q { let n = writer.write(&data[pos..]).await?; pos += n;` | `pos < X0Q { let n = writer.write(&data[pos.]).await?; pos += n;`（`while` が消え、`[pos..]` が `[pos.]` になる） |
| `Ok(())` | `OK()` |

**Issue の表が均している点が 1 つある。**コードの 3 行は 1 つの翻訳単位で、それを
文の分割器が 2 セグメントに切っている。`while pos < data.len() {` が単独で
エンジンに渡っているわけではない。

直したあとは、この doctest の**セグメントが 0 個**になる。エンジンは呼ばれず、
gloss はコードそのものになる（同じ入力で 6.19 秒 → 1.58 秒。ディスクキャッシュ
無し、モデルが 1 度も読まれないため）。

### 13.2 規則

> 隣接した行コメントの連なりの中で Markdown のフェンス状態を持ち回る。英数字を
> 1 つも含まないコメントは、**連なりがフェンスの中に無いときだけ**落とし、そこで
> 連結を切る。`is_translatable` そのものは触らない。

判定は `codegloss-core::opens_or_closes_a_fence`。`FENCES` の写しをパーサ側に
作らないためで、これは `preserve.rs` の `SpanKind` と同じ理由（AGENTS.md）。

### 13.3 コーパス

| | コーパス 1 | コーパス 2 |
|---|---|---|
| 出どころ | 自リポジトリ（1e6f77f、`git ls-files '*.rs'`） | この機械の registry にある 11 クレート |
| ファイル | 36 | 624 |

コーパス 2 の内訳: tokio 1.53.1 / serde 1.0.229 / tracing 0.1.44 /
tracing-core 0.1.36 / tracing-subscriber 0.3.23 / regex 1.13.1 /
regex-automata 0.4.18 / regex-syntax 0.8.11 / dashmap 6.2.1 /
thiserror 2.0.20 / tower-lsp-server 0.23.0 の `src/**/*.rs`。**Issue #53 が使った
465 ファイルとは違う**（`lru` がこの機械に無い）ので、絶対値は Issue の表と
一致しない。

### 13.4 規模

| | コーパス 1 前 | 後 | コーパス 2 前 | 後 |
|---|---|---|---|---|
| ブロック | 1183 | 1182 | 34575 | 28450 |
| 翻訳単位 | 1219 | 1218 | 37273 | 28052 |
| セグメント | 2070 | 2069 | 53476 | 41377 |
| 閉じないフェンスで終わるブロック | 9 | 0 | 724 | 4 |
| 翻訳単位を 1 つも持たないブロック | 9 | 9 | 676 | 1927 |

**エンジンに渡すセグメントが 12099 件（53476 の 22.6%）減る。**残る 4 件の開いた
ままのフェンスは、閉じフェンスが上流のソースに無い doctest である（tokio
`sync/mutex.rs` の `try_lock_owned`、tokio `io/interest.rs`、tokio
`task/coop/mod.rs`、tracing `span.rs`。ファイルを開いて確認した）。

**Issue #53 の「規模」表（20337 → 8959 ブロック、28789 → 27382 セグメント、
1407 件削除）はこの変更の数字ではない。**空行だけを繋ぐ腕（#47 のもの）である。
同じコーパス 2 で両方を測ると、空行の腕は 51956 セグメント（1520 件削除）で、
この変更は 41377 セグメント（12099 件削除）。**比は 8.0 倍**で、Issue が挙げて
いるのは Issue 自身が「これでは直らない」と書いているほうの数字だった。

### 13.5 散文は 1 件も変わらない（厳密な部分列）

`GlossPlan::segments()` を全ブロック分に平らに並べ、前後を突き合わせる。**独立な
2 通りで測った**: 貪欲な部分列判定（通れば部分列であることの証明になる）と、
`diff -d`（最小の編集脚本＝最長共通部分列）。どちらも同じ答えを出す。

| | 前 | 後 | 追加 | 削除 |
|---|---|---|---|---|
| コーパス 1 セグメント | 2070 | 2069 | **0** | 1 |
| コーパス 2 セグメント | 53476 | 41377 | **0** | 12099 |
| コーパス 1 翻訳単位 | 1219 | 1218 | **0** | 1 |
| コーパス 2 翻訳単位 | 37273 | 28052 | **0** | 9221 |

**新しい列は古い列の厳密な部分列である。**減るだけで、1 件も変わらない。これが
この変更の安全性の中身である。

**`diff` は既定では最小でないことに注意。**大きな入力では速度のための発見的処理
が働き、同じ 53476 行に対して「追加 1」と答える。`-d`（`--minimal`）を付けると
0 になり、貪欲判定と一致する。

消えた 1 件がどこかというと、**このリポジトリの `docblock.rs` の module doc に
ある ASCII 図**である。Issue #53 の「自リポジトリでは当たりません」は誤りで、
CodeGloss はフェンスの処理を実装しているファイル自身の図を機械翻訳していた
（```` //! ```text ```` は言語タグに英数字があるので生き残り、図の側だけが別の
フェンス無しブロックになっていた）。

**他の腕は散文を変える。**同じ測り方で、`retain` の述語だけを差し替えた 3 つの腕
（いずれも 1e6f77f のコードに 1 行の変更）:

| 腕 | コーパス 2 セグメント | 追加 | 削除 | コーパス 1 の追加 |
|---|---|---|---|---|
| **この変更**（フェンスの中だけ残す） | 41377 | **0** | 12099 | **0** |
| 空の行コメントも残す（#47 の腕） | 51956 | 0 | 1520 | 0 |
| `///` / `//!` の行コメントを全部残す | 41381 | **15** | 12110 | **5** |
| 行コメントを全部残す | 41526 | **193** | 12143 | **5** |

**doc マーカーだけを見る規則では足りない。**新しく変わる 15 件は Markdown の
引用（`> > From [The ultimate … page…`）、波括弧だけの文
（`if let Some(pids) = map.remove(&id1) { map.insert(id2, pids); }`）、
そして表の行である。このリポジトリでも 5 件変わり、うち 1 件は `pipelines.rs` の
doc コメントにある表が
`| | what the engine is given | what comes back | |---|---|---| | A | …` という
1 つの散文単位になったものだった。`docblock.rs` はフェンス・見出し・箇条書き・
doc タグの規則を持っているが、`>` にも `|` にも setext の下線にも持っていない。
**「`docblock.rs` が構造として扱えるもの」はフェンス行とその間だけ**で、
「doc マーカーの行」より狭い。

**全部残すのはさらに悪い。**193 件変わり、その中には regex の DFA の ASCII 図
（`DQMMMMAAAAASSSSSSNNNNNNN | | |---------| accelerated states …`）や
`-----`、ベンチマークの表が入っている。

### 13.6 消えたのはコードか

判定は tree-sitter-rust で、次の 3 通りのどれかで ERROR / MISSING ノードの無い
木になれば「Rust の構文」とする: (1) そのままソースファイルとして、(2)
`fn __p() { … }` の本体として、(3) (2) に開きっぱなしの括弧を閉じ足して。

| | Rust の構文 | 母数 |
|---|---|---|
| 消えた翻訳単位 | 8074（**87.6%**） | 9221 |
| 残った翻訳単位（対照） | 2610（9.3%） | 28052 |
| 消えたセグメント | 9960（82.3%） | 12099 |
| 残ったセグメント（対照） | 2665（6.4%） | 41377 |

**残り 12.4% も目で見るとコードである。**内訳は、フェンスの中の空行をまたいで
切られた断片（`let pairs:` と `Vec<(&'static str, …)> = …` が別々の単位になる）
と、生文字列に正規表現が入っているもの。この判定基準が保守的なだけで、散文が
消えているわけではない。

**消えた翻訳単位のうち 1124 件（12.2%）は `//` で始まる。**doctest の中に書かれた
英語のコメントで、読者はその訳を失う。これは `docblock.rs` がもともと持っている
立場（「フェンスの中はコードなので丸ごと写す」）を初めて一貫して適用した結果で
あり、この変更で読者が失う唯一のものでもある。

### 13.7 `PIPELINE_VERSION` は上げない

キャッシュキーの本文は `CommentBlock.raw`（`documents.rs::comment_sources` が
`raw` を返し、`backend.rs` が `&block.raw` で引く）。パーサの変更は**境界が動いた
ブロックのキー本文そのものを変える**ので、古い項目に当たりようがない。

| 相異なる `raw` | 前 | 後 | 前後で同一 | 新規 | 使われなくなる |
|---|---|---|---|---|---|
| コーパス 1 | 1182 | 1181 | 1172 | 9 | 10 |
| コーパス 2 | 24569 | 21290 | 19268 | 2022 | 5301 |

`PIPELINE_VERSION` が守っているのは「**キー本文が同じまま** `preserve` /
`sentence` / `docblock` の出力が変わる」ことで、これはそれに当たらない。実際、
`codegloss-core` 側の変更（`FENCES` を使う 2 つの式を 1 つの関数にまとめただけ）
が出力を変えていないことは、変更前のコアと変更後のコアに**同じ**コーパスを通して
確かめてある: 翻訳単位・セグメント・セグメントの原文とも、コーパス 1 の 1183
ブロックとコーパス 2 の 34575 ブロックでバイト単位に一致した。

上げると、まだ正しい 1172 / 1182 のキャッシュを捨てることになる。ピンの
`model.rs::the_key_encoding_is_stable` は
`0190a04bef87bcc9e895441c0ebab1791f2df38a9c91a7498fc30f828c7b149f` のまま通る。

### 13.8 code lens の可視文字数は変わらない

#47 と同じ測り方——実モデル（/tmp/wf/pack、f32、幅 4、CPU）でコーパス 1 の
相異なる 2054 セグメントを 1 バッチで訳し（588.7 秒）、ブロックごとに
`GlossPlan::restore` してから `code_lens::single_line` と同じ平坦化をかけ、
`MAX_TITLE_CHARS = 120` で切る。

| | レンズ | 見える文字 | 総文字 | 率 |
|---|---|---|---|---|
| 前 | 1183 | 71410 | 82603 | 86.4% |
| 後 | 1182 | 71403 | 82682 | 86.4% |
| 空行を繋いだ世界（#47 の腕） | 831 | 56699 | 82955 | 68.3% |

3 行目は同じ訳文表を使って**この場で測り直した**もので、#47 が公表した
56696 / 82952（68.3%）と 82952 文字中 3 文字しか違わない。3 つの世界の日本語の
文字数はいずれも 58427 で同じ——同じ訳が違う数のレンズに配られているだけである。

**#47 は 18 ポイント払う変更だったが、これは払わない。**繋がるのがフェンスの中の
コード（そのまま写される）であって散文ではないため、レンズ 1 件あたりの散文の
量が増えない。

### 13.9 区切り線は落ち続ける

AGENTS.md が意図的なものとして書いている挙動（「区切り線と空コメントは落とす」）
は変わらない。フェンスの中に無い区切り線は今までどおり `is_translatable` が
決めるので、規則そのものが触れていない。

```
// One.        // Rule one.      /* --- */      /// Doc one.
// ====        //////////                       ///
// Two.        // Rule two.                     /// Doc two.
```

この 4 つを 1 ファイルにしたものは、前後とも **6 ブロック**でバイト単位に同じ
出力になる。コーパスでも、ブロックの本文に入ったスラッシュの区切り線は前後とも
0 行、`=` の見出し行は前後とも 211 行（`// = note:` のように英数字を持つ行で、
もともと落ちない）。

### 13.10 凍結コーパス（§12）は動かない

`tests/pipelines.rs` が読む `tests/fixtures/comment-corpus.txt` はファイルなので
パーサの変更は届かない。モデル不要の漂流防止
`arm_a_reproduces_the_shipped_pipeline` も通ったままである。§12.1 が書いている
「抽出器の出力と凍結標本のずれ」も変わらない: 同じ 3 ファイルを抽出すると前後
とも **109 ブロック**で、62 ブロックのうちそのまま一致するのは前後とも 3 件、
インデントを無視して一致するのは前後とも 38 件。

3 ファイルの出力で変わるのは 1 行だけで、`translation.rs` の module doc の
```` //! ```text ```` 図に**閉じフェンスが付く**（言語タグ付きの開きは英数字を
持つので前から生き残っており、閉じだけが落ちていた）。この変更が消しているのは
「```` ```text ```` と書けばフェンスを尊重し ```` ``` ```` と書けば捨てる」という
非対称そのものである。

### 13.11 この変更に含めなかったもの

- **フェンスの中のインデントが落ちる。**`CommentShape::parse` が
  `strip_markers`（trim する）の結果から `Piece::Verbatim` を作るので、
  `/// if x {` / `///     y();` / `/// }` は `y()` が左端に寄って組み直される。
  言語タグ付きのフェンスでは**今も起きている**（このリポジトリの翻訳単位ゼロの
  9 ブロックは全部これを通る）。直すとキー本文が同じまま `docblock` の出力が
  変わるので **`PIPELINE_VERSION` の更新が要る**。別 Issue。
- **チルダの飾り罫がコアではフェンスを開く。**
  `CommentShape::parse("/// ~~~~~ Section ~~~~~\n/// Prose after it.")` は今の
  main で**翻訳単位を 1 つも返さない**。この変更は裸の `/// ~~~~~~~~~~` も残す
  ようになるので露出が広がる。ただしチルダのフェンス行は両コーパスの 35758
  ブロック（1183 + 34575）に 0 件で、観測された損失ではなく危険である。別 Issue。
- **翻訳単位を持たないブロックにレンズを出すかどうか。**変更後はコーパス 2 の
  1927 / 28450 と、このリポジトリの 9 / 1182 が該当し、自分のコードを 1 行に
  畳んで表示する。これは新しい挙動ではなく（このリポジトリの 9 件は今の main の
  出荷挙動）、機械翻訳されたコードを表示していたレンズと入れ替わるだけなので
  ここでは触らない。`code_lens` の話であってパーサの話ではない。
- **末尾の切り詰めが効くブロックは 3 件。**閉じないフェンスの末尾に来た
  `/// }` / `/// # }` / `///` を落として、ブロックの範囲がコメント本体で終わる
  ようにしてある（コーパス 2 で 3 / 28450、コーパス 1 で 0）。切り詰めないと
  `ranges_point_back_at_the_original_source` が「範囲はコメントを指す」と言って
  いるのと食い違う。

### 13.12 未検証

- **Rust だけ。**`codegloss-parser` は Rust しか読まないので、Javadoc の
  `<pre>{@code …}</pre>` や Python docstring の `>>>` ブロック——同じ不具合の
  別構文——がどうなるかはここでは何も言えない。§12.8 と同じ留保。
- **doctest 1 つが 1 キャッシュ項目になったことのレイテンシ。**測っていない。
  1 ブロックが「全部訳せたか全部落ちたか」の単位になり、コーパス 2 で最大の
  ブロックは数行から 1 例ぶんに伸びる。
- **抽出そのものの時間は測ってある。**release ビルドで 36 ファイル、5 回の中央値
  で **0.201 秒 → 0.215 秒**。プロセス起動と文法の読み込みが大半を占める規模で、
  この差の内訳は分けていない。

## 14. コードフェンスの中の字下げを保つ（Issue #55）

`CommentShape::parse` がフェンスの中の行も trim してから写すので、doctest の
**字下げが落ちて左詰めになる**。#54 でフェンスが `docblock.rs` に届くように
なったため、言語タグの有無にかかわらず全部のフェンスで起きる。

**この回で `PIPELINE_VERSION` を 5 -> 6 に上げる。**理由は 14.7（#56 と 1 回で
共有する）。

### 14.1 不具合の再現

Issue #55 の断片をそのまま抽出器に通し、`GlossPlan::new(raw).source()` を見る。

```
/// Loads a user.          前: ```                     後: ```
///                             if let Some(user) = …       if let Some(user) = …
/// ```                         println!("{user}");             println!("{user}");
/// if let Some(user) = … {     log(user);                      log(user);
///     println!("{user}");     }                           }
///     log(user);              ```                         ```
/// }
/// ```
```

翻訳単位は前後とも 0 で、エンジンは呼ばれていない。**壊れているのは復元だけ**で
ある。

### 14.2 コーパス

| | コーパス 1 | コーパス 2 | コーパス 3 |
|---|---|---|---|
| 出どころ | 自リポジトリ（3a1ad60、`git ls-files '*.rs'`） | §13.3 と同じ 11 クレート | syn 2.0.119 の `src/**/*.rs` |
| ファイル | 36 | 624 | 55 |
| ブロック | 1224 | 28450 | 1661 |
| 翻訳単位 | 1260 | 28052 | 1629 |
| セグメント | 2149 | 41377 | 1864 |

コーパス 2 は §13.4 の「後」の列（28450 / 28052 / 41377）と完全に一致する。
コーパス 1 が §13.4 の 1182 より多いのは #54 がテストを足したぶんである。
**コーパス 1 は測定の前に 3a1ad60 の内容へ凍結してある**——このリポジトリ自身が
コーパスなので、変更中のツリーをそのまま測ると入力ごと動いてしまう。

コーパス 3 を足したのは #56 のためで、この機械の registry 全体（266 クレート
7829 ファイル）でチルダの罫線を含むコメントが syn にしか無いからである（15.1）。

### 14.3 字下げの基準は 2 つある

- **案 A**: マーカーの直後の 1 スペースだけを剥がし、残りは全部内容とする。
- **案 B**: 開きフェンスの字下げを 0 列とみなして各行から引く（CommonMark /
  rustdoc の畳み方）。

開きフェンスの字下げ（コメント記号を剥がしたあとの列）のヒストグラム:

| | 0 列 | 2 列 |
|---|---|---|
| コーパス 1 | 9 | 0 |
| コーパス 2 | 2129 | 15 |
| コーパス 3 | 120 | 4 |

**2 案が違う答えを出すのは、開きフェンスが字下げされている場合だけ**で、その件数は
コーパス 1 で **0**、コーパス 2 で **15**、コーパス 3 で **4**。15 件の内訳は
thiserror `lib.rs` が 9、tracing `span.rs` が 4、tracing `lib.rs` が 1、
tracing-core `lib.rs` が 1（いずれも ```` //!   ```rust ```` と 2 列内側に書かれた
doctest）。**そのどれでも案 B は今の出荷と同じにはならない**（15 件中 0 件）:
中の `#[error(…)]` の 4 列は案 A でも案 B でも残り、消えるのは共通の 2 列だけで
ある。

**採ったのは案 A。**測って決めた部分と、決めていない部分を分けて書く:

- 測って分かるのは「2 案の答えが違うのは 28450 中 15 ブロック」ということだけで、
  **どちらが読みやすいかは測っていない**。
- 案 A を採る理由は設計上のもの: (a) 行ごとに閉じていて状態を持ち回らない、(b)
  書き手が打った文字を 1 つも消さない——gloss は Markdown のレンダリングではなく、
  隣に並ぶコメント本文の書き換えである、(c) `docblock.rs` は入れ子の箇条書きも
  引用も表も構造として扱っていない（§13.5）。字下げされたフェンスを生むのは
  たいてい箇条書きの入れ子で、その入れ子を再現しないまま畳み方だけを入れるのは
  半分の規則になる。

**Issue #55 が案 B の根拠に挙げている「ブロックコメントの ` * ` で列がずれる」は
案 A には起きない。**`after_markers` は `*` を探す前に行を trim するので、星の列が
ずれても効かない（`*` の直後の 1 スペースだけを剥がす）。ただし**両コーパスに
「フェンスを含む星付きブロックコメント」は 0 件**で、この形は実測では押さえられず
`a_starred_block_comment_keeps_the_indentation_inside_its_fence` が唯一の言明に
なる。

### 14.4 規模

| | コーパス 1 | コーパス 2 | コーパス 3 |
|---|---|---|---|
| フェンスを含むブロック | 9 | 2090 | 123 |
| `source()` が変わるブロック | **8** | **1269**（60.7%） | **110** |
| フェンスの中の行 | 38 | 32655 | 2464 |
| そのうち字下げが戻る行 | 24（63.2%） | **9131（28.0%）** | 1171（47.5%） |

コーパス 1 の 8 件は `docblock.rs:12` / `sentence.rs:9` / `sentence.rs:56` /
`translation.rs:18` / `extract.rs:11` / `measure.rs:9` / `probe.rs:10` /
`pipelines.rs:34`。フェンスを持つ 9 件のうち変わらないのは `sentence.rs:260`
だけで、これは中の行に字下げが 1 つも無い。**Issue #55 の「9 件のうち 7 件」は
8 件が正しい。**

字下げにタブを使っている行はコーパス 2 に 8 行あり、全部
regex-syntax `hir/mod.rs` の doctest（2255 行と 2285 行から 4 行ずつ）である。
**タブは空白の数に読み替えられない**ので、剥がすのは 1 スペースだけという規則が
ここで効く（`a_tab_inside_a_fence_is_content`）。

### 14.5 翻訳の入出力は 1 バイトも変わらない

`CommentBlock.raw`（＝キャッシュのキー本文）・翻訳単位・エンジンへ渡すセグメントを
全ブロック分並べて、変更の前後で突き合わせた。

| | コーパス 1 | コーパス 2 | コーパス 3 |
|---|---|---|---|
| BLOCK（raw） | 一致 | 一致 | 一致 |
| UNIT（翻訳単位） | 一致 | 一致 | 一致 |
| SEG（セグメント） | 一致 | 一致 | 一致 |
| SRC（`source()`） | 8 件変化 | 1269 件変化 | 110 件変化 |

**変わるのは復元だけである。**フェンスの中の行は前からエンジンに渡っていない
（#54）ので、この変更で新しく訳される文も、訳されなくなる文も無い。

### 14.6 code lens には出ない。hover にだけ出る

`code_lens::single_line` が `split_whitespace()` で空白の連なりを 1 つに畳むので、
字下げはレンズのタイトルに届かない。

| | レンズ | 可視文字（合計） |
|---|---|---|
| コーパス 1 | 1224 | 111462 → 111462 |
| コーパス 2 | 28450 | 2249954 → 2249954 |
| コーパス 3 | 1661 | 107741 → 107741 |

**31335 件のタイトルがバイト単位に同一。**hover 側は LSP 越しに実測してあり
（`the_hover_markup_keeps_the_indentation`）、`textDocument/hover` の
`contents.value` がフェンス付きコードブロックのまま 4 スペースを保つ。フェンスの
中には hard break（行末の 2 スペース）が入らない——`with_hard_breaks` が
フェンスの中を避けるためで、避けなければコードに 2 文字足すことになる。

### 14.7 `PIPELINE_VERSION` は上げる（§13.7 との違い）

キャッシュの値は `plan.restore(…)` の完成した gloss で、キーは
`CommentBlock.raw` そのもの。**この変更は raw を 1 バイトも変えないまま gloss を
変える**（14.5 の BLOCK 列が一致することがその実測）ので、`PIPELINE_VERSION` が
守っている「キー本文が同じまま `preserve` / `sentence` / `docblock` の出力が
変わる」場合にまさに当たる。#54（§13.7）が上げなかったのは、あちらが**境界ごと**
動かしてキー本文を変える変更だったからで、逆である。

ピンは `model.rs::the_key_encoding_is_stable`:

```
"5" -> 0190a04bef87bcc9e895441c0ebab1791f2df38a9c91a7498fc30f828c7b149f
"6" -> 11eb0cb87f7522a1da6d74f43736ebf88bcd885c0b3b599c74158f12a1805167
```

差し替えた値は**計算したものではなく、テストを走らせて印字された left を読んだ
もの**である。古い項目は `GlossStore::open` の上限超過削除で自然に落ちるので、
手で消す手順は要らない。

### 14.8 この変更に含めなかったもの

- **星の無いブロックコメントは今までどおり左詰め。**`/*!` を列 0 に書き、
  継続行に `*` を置かない形（regex 系のモジュールドキュメント）。コーパス 2 に
  8 ブロックあり、regex `bytes.rs:1` / regex `lib.rs:1` / regex-automata
  `dfa/mod.rs:1` / `dfa/sparse.rs:1` / `hybrid/mod.rs:1` / `lib.rs:1` /
  regex-syntax `lib.rs:1` / `utf8.rs:1`。行頭の空白がファイルの字下げと内容の
  字下げの両方で、1 行だけ見て分けられない。**8 件とも変更の前後で出力が同一**で、
  悪化はしていない（`a_block_comment_without_stars_cannot_keep_its_indentation`）。
  直すならブロック全体の最小字下げを引く前処理が要る。
- **`restore(sources()) == source()` が成り立たないブロックが 64 件ある。**
  コーパス 1 で 2、コーパス 2 で 59、コーパス 3 で 3。**件数も対象ブロックも
  変更の前後で同じ**で、#55 とは無関係の既存の性質である。内訳は 2 種類:
  文の切れ目にある**空白の連なりが 1 つに畳まれる**もの（`Windows.  Note that`
  が `Windows. Note that` になる。コーパス 2 で 57、コーパス 3 で 3、
  tracing-subscriber `time_crate.rs` の `:` のあとの空白 2 件を含む）と、
  **コメント本文にプレースホルダと同じ形の文字列（`X0Q`）が書いてある**もの
  （コーパス 1 の 2 件＝`docblock.rs:797` と `preserve.rs:744`。どちらも
  プレースホルダの説明を書いたテストのコメント）。別 Issue に値する。

### 14.9 未検証

- **レイテンシは測っていない。**フェンス行 1 本あたり `strip_prefix` が 1 回
  増えるだけだが、数字は持っていない。
- **Rust だけ。**Javadoc の ` * ` 付きフェンスは単体テストでしか押さえていない
  （両コーパスに 0 件）。Python docstring の `>>>` ブロックは何も測っていない。

## 15. チルダの罫線をコードフェンスと見なさない（Issue #56）

`~~~` で始まる行を Markdown のフェンスとして扱うと、**飾りの罫線もフェンスの
開きに見える**。後ろの散文は「閉じないフェンスの中身」になり、翻訳単位が 0 に
なって英語のまま出る。CommonMark としては正しい読み方だが、コメントの中では
罫線のほうが多い。

コーパスは §14.2 と同じ 3 つ。**`PIPELINE_VERSION` は §14.7 で 5 -> 6 に
上げたぶんが両方をまかなう**（この変更でも上げ直さない）。

### 15.1 実損は syn にある。「危険だけ」は誤り

Issue #56 は「両コーパスに 0 件なので今日の実損は無い」と書いているが、**母数
35758 は #54 前の数字**である。#54 後は 1224 + 28450 = **29674 ブロック**で、
やはりチルダのフェンス行は 0 件——ここまでは Issue のとおり。

しかし**この機械の registry 全体を見ると違う。**266 クレート 7829 ファイルで
`~~~` を含む `.rs` の行は **12**、うちコメント行は **10**、うち**コメント記号を
剥がしたあと `~~~` で始まる行＝フェンスと読まれる行は 6**:

| 場所 | 行 | 何か |
|---|---|---|
| syn 2.0.119 / 3.0.4 `attr.rs:71` | `///   ~~~~~~Path` | ASCII 図 |
| syn 2.0.119 / 3.0.4 `attr.rs:75` | `///   ~~~~Path` | ASCII 図 |
| syn 2.0.119 / 3.0.4 `punctuated.rs:20` | `//!   ~~~~^ ~~~~^ ~~~~` | ASCII 図 |

（registry に syn が 2 版あるので 3 行 x 2 版）。残りの 2 行はコメントではない
（anyhow のマクロの中の冗談の定数と、regex-syntax のテストの生文字列）。
**真陽性——本物のチルダのコードフェンス——は 1 件も無い。**

syn `attr.rs` の図は ```` ```text ```` の中にあり、`~~~~~~Path` がフェンスを
**閉じて**しまうので、今日この瞬間 2 ブロックに割れ、**コードが 2 セグメント
エンジンに渡っている**:

```
TO ENGINE: "^^^^^^^^^^^^^^^^^^^X0Q"
TO ENGINE: "#[path = \"sys/windows.rs\"]"
```

実モデル（/tmp/wf/pack、f32、幅 4、CPU）に通すと、前者の答えは
`^^^^^^^^^^^^^^^^^^X0Q` ——**キャレットが 19 本から 18 本に減る**。図の桁が
ずれ、`Meta::List` が指しているはずの位置を指さなくなる。

### 15.2 4 案の実測

Issue が挙げている 4 案を全部実装して、同じ 3 コーパスに通した。

| | コーパス 1 | コーパス 2（セグメント） | コーパス 3 | 裸の `~~~~~~~~~~` | syn の図 |
|---|---|---|---|---|---|
| 案 1 対応を数える | 不変 | 41377 → **41395（+18）** | +1 | 直る | **直らない** |
| 案 2 `~~~` を外す | **不変** | **不変** | −2 | 直る | **直る** |
| 案 3 情報文字列に `~` を含まない | 不変 | 不変 | 不変 | **直らない** | **直らない** |
| 案 4 何もしない | — | — | — | 直らない | 直らない |

- **案 1**（フェンスの対応が付かない連なりではフェンスと見なさない）は
  コーパス 2 でブロック 28450 → 28459、翻訳単位 28052 → 28069、セグメント
  41377 → **41395**。増えるのは `use tokio::io::Interest;` /
  `let mutex = X0Q(X1Q(1));` / `Ok(())` のような Rust のコードで、守られなく
  なるのは**上流に閉じフェンスが無い doctest 4 件**（tokio `io/interest.rs:159`
  ——ファイルを開くと 165 行でコメントが終わり閉じフェンスが無い、tokio
  `sync/mutex.rs:739`、tokio `task/coop/mod.rs:476`、tracing `span.rs:1088`）。
  syn の図はフェンス行が偶数で対応が付いてしまうので**直らず**、逆に syn
  `punctuated.rs:18` が守られなくなって `` X0Q`text X1Q(arg1, arg2, arg3); ``
  が新しくエンジンへ渡る（コーパス 3 で +1）。加えて
  `opens_or_closes_a_fence(&str) -> bool` では表せない: パーサもコアも先読みが
  要り、#54 が意図して作った結合（判定はコアに 1 つだけ）を壊す。
- **案 2**（`FENCES` から `~~~` を外す）はコーパス 1 と 2 で**ダンプがバイト単位に
  同一**、コーパス 3 で 1 ブロック統合・翻訳単位 −2・セグメント −2。3 コーパス
  とも**新しいセグメントは 0 件で、新しい列は古い列の厳密な部分列**である。
- **案 3**（チルダのフェンスは情報文字列に `~` を含まないことを要求する）は
  3 コーパスとも出力が 1 バイトも変わらない。直るのは飾り罫
  `~~~~~ Section ~~~~~`（実測 0 件）だけで、裸の `~~~~~~~~~~` も syn の
  `~~~~~~Path` も直らない。
- **案 4**（何もしない）は #54 が壊した形を壊れたまま残す。

**採ったのは案 2。**確立済みの 2 コーパス 29674 ブロックで出力が 1 バイトも
変わらず、registry 全体で唯一の実発生を直す唯一の案だからである。

### 15.3 案 2 の代金

- **本物の `~~~` の doctest を守らなくなる。**両コーパス＋registry 7829
  ファイルで 0 件だが、0 ではない。`/// ~~~\n/// let x = 1;\n/// ~~~` は
  2 ブロックになり `let x = 1;` がセグメントになる。
- **飾り罫は散文と 1 単位になり、チルダごとエンジンへ渡る。**
  `~~~~~ Section ~~~~~` は英数字を含むのでパーサが落とさない（`// ==== Section
  ====` が昔からそうであるのと同じ）。単体テスト
  `a_tilde_rule_does_not_swallow_the_prose_after_it` がこの姿を書き留めてある。
- **裸の `~~~~~~~~~~` は #54 以前の挙動に戻る**——`is_translatable` に落ちて
  連なりが切れ、前後が別ブロックになる（`a_tilde_rule_breaks_a_run_like_any_other_decoration`）。

### 15.4 直していないもの

- **`backend.rs::with_hard_breaks` は 3 つ目のフェンス判定を持ったまま。**
  `line.trim_start().starts_with("```")` で、案 2 のあとは
  `opens_or_closes_a_fence` と**答えが完全に一致する**（`FENCES` の要素が
  ```` ``` ```` 1 つだけになるため）。それでも寄せなかったのは、**訊いている
  問いが違う**から: `opens_or_closes_a_fence` は「CodeGloss が写す行か」を
  答え、`with_hard_breaks` は「editor の Markdown がフェンスと見なす行か」を
  答える。後者の権威は CommonMark で、`~~~` を扱わないのは CodeGloss の都合
  である。AGENTS.md の「判定はここにしか無い」にこの例外を書き足した。
- **syn `punctuated.rs:18` の図は前後とも翻訳単位 0。**`~~~~^ ~~~~^ ~~~~` が
  フェンスを閉じても、続く ```` ``` ```` が開き直して行が最後まで verbatim に
  なるため、案 2 の前後で出力が変わらない（案 1 だけがこれを壊す）。

### 15.5 未検証

- **Rust 以外。**Javadoc の `<pre>{@code …}</pre>` や Python docstring の
  `>>>` ブロックで `~~~` がどう使われるかは何も測っていない（§13.12 と同じ
  留保）。
- **registry の 266 クレートは「この機械にあるもの」であって、crates.io の
  標本ではない。**真陽性 0 件はこの母集団に対する数字である。

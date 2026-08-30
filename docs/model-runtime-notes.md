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
CODEGLOSS_MODEL_PACK=/path/to/pack \
  cargo test -p codegloss-translator --features candle --release -- --ignored --nocapture
```

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

## 4. レイテンシとメモリ

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

## 6. 未検証

- **f16 での推論。**F32 のみ測った。candle の CPU バックエンドで f16 が速いのか
  遅いのかも測っていない。
- **量子化。**candle の量子化 marian は試していない。
- **ビームサーチとの品質差。**貪欲法のみ。
- **バッチ推論。**1 件ずつしか回していない。パディングした一括推論がどれだけ
  速くなるかは未測定。
- **Linux 以外。**macOS / Windows では測っていない。GPU（CUDA / Metal）も同様。
- **Zed 実機での見え方。**LSP サーバとしての応答は stdio 越しに確認したが、
  Zed 上での表示は確認していない（`docs/zed-display-notes.md` の担当）。
- **長いコメント。**入力は `max_position_embeddings - 1 = 511` トークンで
  切っている。それを超えるコメントで何が起きるかは測っていない。
- **翻訳キャッシュの永続化。**`GlossCache` はインメモリのままなので、サーバを
  再起動すると 235 ms/セグメントを払い直す。

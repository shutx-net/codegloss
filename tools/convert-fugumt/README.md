# convert-fugumt

FuguMT（[staka/fugumt-en-ja](https://huggingface.co/staka/fugumt-en-ja)）を
CodeGloss の**モデルパック**に変換する。

```sh
pip install -r requirements.txt
python3 convert.py /path/to/pack
codegloss-lsp --model-pack /path/to/pack
```

Python を使うのは変換のときだけで、動くもの（`codegloss-lsp`）には入らない
（Issue #1 の "Python may be used during model evaluation/conversion, but should
not be part of the shipped runtime"）。

## 出力

| ファイル | 中身 |
|---|---|
| `manifest.json` | `model_id` / `model_version` / `license` / `attribution` と全ファイルの sha256 |
| `config.json` | 上流のコピー。candle の `marian::Config` がそのまま読む |
| `generation_config.json` | 上流のコピー（参照用。実行時には読まない） |
| `pytorch_model.bin` | 上流の重み、**変換なし** |
| `tokenizer-source.json` | `source.spm` + `vocab.json` から生成 |
| `tokenizer-target.json` | `target.spm` + `vocab.json` から生成 |
| `LICENSE` | CC-BY-SA-4.0 の全文 |
| `NOTICE` | 帰属表示 |

`model_version` は重みの sha256 の先頭 12 桁から作る。翻訳キャッシュのキーに
入るので、重みが変われば必ず変わる。

## 重みを変換しない理由

**candle は `pytorch_model.bin` をそのまま読める。**`VarBuilder::from_pth` が
pickle アーカイブを直接開く（`candle_core::pickle`）。実測では 121 MB の
`pytorch_model.bin` から `marian::MTModel` を組み立てるのに 0.3〜0.45 秒で、
safetensors 化の手間に見合う差は無かった。

つまり **torch も transformers も要らない。**必要なのは `protobuf` /
`sentencepiece` / `tokenizers` の 3 つだけで、いずれも数十 MB に収まる。

`model.safetensors` を置いた場合はそちらが優先される（`CandleTranslator` が
先に探す）。mmap ではなく読み込みで開く（`codegloss-translator` は
`#![forbid(unsafe_code)]` で、`from_mmaped_safetensors` は `unsafe`）。

## トークナイザを変換する理由

上流には `tokenizer.json` が無く、あるのは SentencePiece の `source.spm` /
`target.spm` と `vocab.json` だけ。candle 側は `Tokenizer::from_file` を前提に
しているので、ここで組み立てる。

Marian の低速トークナイザは SentencePiece を「文字列をピースに割る」ためだけに
使い、ピース → ID の対応は `vocab.json` で持つ。そのため `*.spm` の内部 ID を
そのまま使うのではなく、`vocab.json` の ID の位置にピースを並べ直している。
FuguMT ではこの 2 つは一致していたが、一致を仮定はしていない。

手順は transformers の `SpmConverter`（4.46.3）と同じ。**transformers 5 系には
Marian 用の変換（`MarianConverter`）がもう無い**ので、必要な部分だけを
`convert.py` に写してある。`--verify` を付けると、生成した高速トークナイザと
`transformers.MarianTokenizer`（低速）の ID 列を突き合わせる（transformers<5 が
入っているときだけ動く。実測で 7/7 一致）。

なお `source.spm` と `target.spm` は FuguMT では**バイト単位で同一**
（md5 `32df5391e60817f5d29645777b489afe`）。生成される 2 本の tokenizer.json も
同じ内容になるが、上流が 2 ファイルで配っているのでパックも 2 ファイルで持つ。

`</s>` を末尾に足す post-processor は付けていない。`SpmConverter` も付けず、
candle の marian-mt の例は自分で足すため。`CandleTranslator` は末尾に `</s>` が
無ければ足す（あれば足さない）ので、どちらのトークナイザでも壊れない。

## ライセンス

**変換したものをこのリポジトリにコミットしない。**

FuguMT の重みは CC-BY-SA-4.0。CC-BY-SA-4.0 は継承条項を持つので、
**`tokenizer-source.json` / `tokenizer-target.json` のような変換物も
二次的著作物**であり、配布するなら CC-BY-SA-4.0 のまま、帰属表示を付けて配る
必要がある。パックに `LICENSE`（全文）と `NOTICE`（帰属表示）を同梱するのは
そのため。

CodeGloss のコードは MIT なので、両者は別の成果物として配る（AGENTS.md）。

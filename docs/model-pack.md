# モデルパックの配布

`codegloss-lsp --fetch-model` が取りに行く先を用意する手順。**重みはこのリポジトリには置かない**（AGENTS.md）。FuguMT は CC-BY-SA-4.0 で、コードは MIT なので、別リポジトリのリリースアセットとして配る。

## 置き場所

| | |
|---|---|
| リポジトリ | `shutx-net/codegloss-models`（新規） |
| リリースタグ | `fugumt-en-ja-1` |
| 取得 URL | `https://github.com/shutx-net/codegloss-models/releases/download/fugumt-en-ja-1/<ファイル名>` |

URL は `crates/codegloss-lsp/src/model_pack.rs` の `DEFAULT_BASE_URL` に焼き込んである。**アーカイブにまとめず、ファイルを 1 つずつアセットとして上げること**（サーバは名前で 1 つずつ取りに行く。展開のための依存を持たないため）。

## 上げるもの

`python3 tools/convert-fugumt/convert.py <dir>` が作ったディレクトリの中身をそのまま。

```
manifest.json
config.json
generation_config.json
pytorch_model.bin
tokenizer-source.json
tokenizer-target.json
LICENSE
NOTICE
```

**`manifest.json` を必ず含めること。**これが信頼の起点で、サーバは最初にこれを取り、**残り全部をこれと照合する**（バイト数と SHA-256）。`convert.py` が書いたものをそのまま上げる——手で書き換えたら照合が落ちる。

## 版が一致していること

`manifest.json` の `model_version` が、`model_pack.rs` の `EXPECTED_MODEL_VERSION` と一致している必要がある。

```sh
python3 -c "import json;print(json.load(open('<dir>/manifest.json'))['model_version'])"
# fugumt-en-ja-8b2d3d3b7da2
```

これは好みの問題ではない。`model_version` は**訳文のキャッシュキーに入っている**ので、違う版のパックは、このビルドが引けない鍵で訳文を書くことになる。違っていたら定数のほうを直す（＝コード変更）。

## 上げる前に確かめる

ローカルで配って、本番と同じ経路を通す。

```sh
cd <dir> && python3 -m http.server 8731 &
CODEGLOSS_MODEL_URL=http://127.0.0.1:8731 CODEGLOSS_CACHE_DIR=/tmp/packtest \
  ./target/release/codegloss-lsp --fetch-model
```

`the model pack is installed` が出れば通っている。落ちるなら、そのエラーは本番でも出る。

## ライセンス

FuguMT は **CC-BY-SA-4.0**。派生物（`tokenizer-source.json` / `tokenizer-target.json` も含む）も同じライセンスで、帰属表示を維持したまま配る必要がある。

- パックには `LICENSE`（CC-BY-SA-4.0 全文）と `NOTICE`（帰属表示）が入っている。**両方ともアセットとして上げること。**
- `codegloss-models` の README にも、ライセンスと出典（<https://huggingface.co/staka/fugumt-en-ja>）を書く。
- サーバはインストール時にログへ帰属表示を出す（`manifest.json` の `attribution`）。

## 何が検証されて、何がされないか

- `manifest.json` は**検証されない**。信じているのは出どころ（HTTPS と、バイナリに焼き込んだ URL）だけ。
- **それ以外のファイルはすべて検証される**（バイト数と SHA-256）。途中で切れたダウンロードや、殺されたプロセスが半分書いたパックを捕まえるため。**壊れた重みはエラーにならず、流暢な出鱈目になる**ので、使う前に捕まえる必要がある。
- 検証を通ったときだけ、`.partial` から本番のディレクトリへ移す。落ちたら前のパックがそのまま残る。

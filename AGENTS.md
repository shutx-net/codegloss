# AGENTS.md

This file provides guidance to coding agents (Claude Code, etc.) when working with code in this repository.

## プロジェクト概要

CodeGloss は、ソースファイルを書き換えずに英語コメントの日本語訳を仮想 UI として表示する読解支援ツール。最初のターゲットは Zed 拡張。

- 設計の全体像: Issue #1
- 技術選定の根拠と Zed ソースの実測調査: `docs/tech-stack-evaluation.md`（必要になったときに読む。常時参照しない）
- Zed 上での表示の確認事項と性能の実測値: `docs/zed-display-notes.md`（**目視確認のチェックリストは未記入のまま。実機を持つ人間が埋める。推測で埋めない**）
- 翻訳エンジン（candle + FuguMT）の実測値: `docs/model-runtime-notes.md`（プレースホルダ形式の選定、レイテンシ、メモリ、実際の訳文）

## 現状

Cargo ワークスペースがあり、`cargo build --workspace` / `cargo test --workspace` が通る。

- `crates/codegloss-core` — ドメイン型（`CommentBlock` / `Segment` / `Gloss` / `GlossKey`）、翻訳キャッシュ（`GlossCache`。LRU。`GlossStore` を渡すとディスクが後ろに付く）、前処理・後処理。`CommentBlock` は `text`（コメント記号を剥がした本文）と `raw`（元のソーステキストそのまま＝前処理・後処理の入力）の両方を持つ。
  - `preserve.rs` — `mask` / `Masked::unmask`。インラインコード・URL・doc タグ・`TODO:` 系接頭辞・識別子をプレースホルダへ退避し、訳文へ戻す。プレースホルダの形式は `X0Q`（`X` ＋ 十進の添字 ＋ `Q`）。P7 で 8 候補を FuguMT に通して実測して決めた（`X0Q` が 97.4%、暫定だった `⟦0⟧` は語彙に無く 0%。表は `docs/model-runtime-notes.md`）。生成と読み取りは同ファイルの 2 関数に閉じてあり、差し替えるのは `PLACEHOLDER_OPEN` / `PLACEHOLDER_CLOSE` の 2 定数だけ。訳文からプレースホルダが消えていたらその単位は原文へフォールバックする。
  - `model.rs` — `PIPELINE_VERSION`。**前処理・後処理の版**で、`GlossKey` にモデル版と並べてハッシュされる。キャッシュに入るのは完成した訳文なので、エンジンが同じでも `preserve` / `sentence` / `docblock` の出力が変われば古い訳が出続ける。**それらを変えたら上げること。**`model.rs` の `the_key_encoding_is_stable` が固定値で守っているので、上げるのは意図的な操作になる。
  - `store.rs` — `GlossStore`。訳 1 件 1 ファイル（ファイル名は `GlossKey::to_hex()`）でディレクトリに置く。`GlossCache::with_store` を通すと、メモリで外れたときにディスクを引き、当たったらメモリへ昇格する。**読み書きの失敗はすべてキャッシュミス扱い**（キャッシュが理由でサーバが止まってはいけない）。上限超過ぶんは `GlossStore::open` が起動時に 1 回だけ古い順に消す。
  - `sentence.rs` — `split_sentences` / `join_sentences`。**マスク済みの**本文を文に切り、訳文を 1 行へ戻す。FuguMT は文単位のモデルで、段落を丸ごと渡すと節を黙って落とす。原文ではなくマスク後に切るのは、URL・`a.b()`・インラインコードの中のピリオドが原文には残っているため（マスク後はすべてプレースホルダ）。略語表と「文の始まりに見えるか」の 2 つで境界を決める。
  - `docblock.rs` — `CommentShape` / `GlossPlan`。`raw` から Javadoc / Rustdoc の行構造を読み、段落・タグ行・箇条書き・見出し・コードフェンスを別々の翻訳単位に分けて、訳文を同じ行構造へ組み直す。連続する散文行は 1 単位にまとめ、その単位を**文ごとに**エンジンへ渡す（マスクとキャッシュは単位のまま）。プレースホルダを落とした文はその文だけ原文へ戻る（`Masked::unmask_fragment`）。組み直した訳文にコメント記号（`/**`・` * `・`///`）は含めない。
  - `cargo build -p codegloss-core --target wasm32-unknown-unknown` が通ることを確認済み（将来のブラウザ拡張との共有のため）。ただしこのターゲットは `rust-toolchain.toml` にも CI にも入れていない。
- `crates/codegloss-parser` — Tree-sitter によるコメント抽出。対応言語は Rust のみ。連続する行コメントを 1 ブロックに連結し、区切り線と空コメントは落とす。**空の `///` 行はここで落ちて連結も切れる**ため、`///` の doc コメントは空行のたびに別ブロックになる。`docblock.rs` の行構造の復元（空行・コードフェンス）が効くのは `/* */` 系のブロックコメントと、空行を挟まない `///` の連なりだけ。
- `crates/codegloss-translator/src/marian.rs` — **candle の `models::marian` のフォーク**（candle 0.11.0、MIT OR Apache-2.0）。上流のままでは外から手が出せない 3 点だけを変えてある: (1) `Decoder::reorder_kv_cache`（上流は `kv_cache` が private で `reset_kv_cache` しか無く、ビーム探索がキャッシュを動かせない）、(2) エンコーダとクロスアテンションのアテンションマスク（上流は両方に `None` を渡すので、パディングすると訳文が静かに悪くなる）、(3) エンコーダ層を `is_decoder: false` で作る（上流は `true`）。`MTModel::decode` は落とした（causal mask を必ず F32 で作るため）。**それ以外は上流のまま**にしてある。上流の新版と diff を取れることが、コピーを抱える条件だから。3 つの変更にはモデルパック不要のユニットテストが付いている（`src/marian.rs` の `mod tests`）。
- `crates/codegloss-translator` — `trait Translator`（`translate(&[Segment]) -> Vec<String>` と `model_version()`）と 2 つの実装。
  - `PassthroughTranslator` — 入力をそのまま返す。常に入っている。
  - `CandleTranslator` — candle + FuguMT（Marian）。**`feature = "candle"` の裏**にあり、既定の `cargo build` / `cargo test` には入らない（CI を重くしないため）。モデルパックのディレクトリを渡して `CandleTranslator::load`（既定 F32）または `load_with(pack, Precision)` で読む。既定はビームサーチ（幅 4、長さ正規化あり）。`--beams 1` で貪欲法。CPU。KV キャッシュはステップをまたいで持ち越し、ビームの並べ替えに合わせて動かす（`marian::Decoder::reorder_kv_cache`）。貪欲法に対する上乗せは実測 1.07 倍。`MTModel` は `&mut self` を要求するので内部を `Mutex` で包んでいる。
    - 重みは `VarBuilder` で**遅延**に読む。一括読み（`pickle::read_all`）はピークが 251 → 441 MiB に悪化するので使わない。FuguMT の pickle は 32001x512 の埋め込みを 4 つの名前で共有しているため（`docs/model-runtime-notes.md` §6.2）。同じ名前を 2 度求めても同じテンソルが返るよう `Weights` でキャッシュしてある。
    - `tokenizer-source.json` と `tokenizer-target.json` がバイト単位で同じなら 1 つだけ構築して共有する（`Tokenizer` 1 つで約 30 MiB）。
    - デコードは `MTModel::decode` ではなく `Engine::decode`。上流は causal mask を必ず F32 で作るため、F16 だと dtype が合わずに落ちる。`Precision` を選べるのはこのため。
    - `Precision::BFloat16` は**動かない**（candle の CPU に BF16 matmul が無い）。列挙子は残してあるが選ぶとロードが失敗する。
  - モデルパックは `tools/convert-fugumt/convert.py` が作る（`manifest.json` / `config.json` / `pytorch_model.bin` / `tokenizer-source.json` / `tokenizer-target.json` / `LICENSE` / `NOTICE`）。**重みは変換しない**（candle の `VarBuilder::from_pth` が pickle を直接読む）ので torch は要らない。
  - 実モデルが要るテスト（`tests/quality.rs` / `tests/placeholders.rs`）は `#[ignore]` 付き。`CODEGLOSS_MODEL_PACK=<dir> cargo test -p codegloss-translator --features candle --release -- --ignored` で走る。`CODEGLOSS_MODEL_PRECISION=f16` を足すと f16 で同じ基準を当てられる。
  - **メモリとレイテンシはテストで測らないこと。**テストハーネスは `#[ignore]` のテストを 1 プロセス内で並列に走らせるので、モデルが同時に何本も常駐して RSS がその合計になる（P7 の「ロード時 1224.8 MiB」はこれだった）。計測は `examples/measure.rs`（1 プロセス 1 モデル、`VmHWM` も出す）で行う。
- `crates/codegloss-lsp` — LSP サーバ。initialize / didOpen / didChange / didClose / hover / codeLens / executeCommand に応答する。hover はコメント上でだけ答え、コード上では `null` を返す。codeLens はコメントブロック 1 件につきレンズ 1 件を、そのコメント行に返す（Zed は行の**上**に描くのでコメントの 1 つ上に出る）。翻訳は `translation.rs` のバックグラウンドワーカーが行い、ハンドラはキャッシュを引くだけ。ワーカーは翻訳の直前に `GlossPlan::new`（mask）、直後に `GlossPlan::restore`（unmask ＋ 行構造の復元）を通す。エンジンが受け取るのは常にマスク済みのセグメント。キャッシュとキューのキーは `CommentBlock.raw`（マスク前の原文そのまま）で、値は unmask 済みの訳文。訳が無いあいだ hover は原文を、codeLens は `⟳ 翻訳中…` を返す（差し替えられるのは後者だけなので、わざと挙動を変えている。理由は `crates/codegloss-lsp/src/code_lens.rs` の冒頭と README）。訳ができると `workspace/inlayHint/refresh` と `workspace/codeLens/refresh` を送る。エンジンの選択とキャッシュの用意は `config.rs`：`--model-pack <dir>`（または環境変数 `CODEGLOSS_MODEL_PACK`）が指すモデルパックを読めれば `CandleTranslator`、無い・壊れている・`candle` feature 無しでビルドされている、のいずれでも `PassthroughTranslator` にフォールバックしてログに残す（**モデルパックが理由でサーバが落ちてはいけない**）。`--precision f32|f16`（`CODEGLOSS_MODEL_PRECISION`）で重みの精度、`--beams <n>`（`CODEGLOSS_MODEL_BEAMS`）で探索幅、`--cache-dir <dir>`（`CODEGLOSS_CACHE_DIR`）で訳の置き場所、`--no-cache` でディスクキャッシュの無効化。キャッシュのディレクトリが使えないときも同じくログを出してメモリのみで動く。
- `editors/zed` — Zed 拡張。`codegloss-lsp` を見つけて起動するだけ。ルートワークスペースからは exclude してあるため `cargo build --workspace` には含まれない（`cd editors/zed && cargo build --target wasm32-wasip2`）。

## 開発環境

セットアップ手順は `DEVELOPERS.md` を参照（Nix flake と Dev Container の 2 通り。どちらも同じツールチェーンになる）。

**IMPORTANT: Rust のバージョンを変えるときは `rust-toolchain.toml` だけを編集する。** flake.nix も devcontainer の rustup も同じこのファイルを読むため、他の場所に版を書くと二重管理になる。

セットアップの検証は `scripts/verify-toolchain.sh`。

## 確定済みの設計判断

調査済みなので再検討不要。覆す場合は `docs/tech-stack-evaluation.md` の根拠に当たること。

- Zed 拡張は Rust → `wasm32-wasip2` 以外に選択肢がない。拡張の責務はサーバの取得・起動・設定受け渡しのみに留める。
- 翻訳エンジンはネイティブバイナリ `codegloss-lsp` 側に置く。WASM 拡張には入れない。
- 翻訳ランタイムは v0.1 では candle（純 Rust）。ct2rs / ort / bergamot へ差し替えられるよう `trait Translator` の裏に置く。
- 重みは pickle（`pytorch_model.bin`）のまま読む。safetensors 化も mmap も**ピーク RSS を減らさない**ことを実測済み（`docs/model-runtime-notes.md` §6.2）。`#![forbid(unsafe_code)]` を外す理由は無い。
- バッチ推論はまだ入っていない。**理由は変わった。**「candle の `marian::Encoder` にアテンションマスクが無い」（§6.5）という障害は `src/marian.rs` のフォークで解消済みで、マスクは実装もテストもしてある。残っているのは実装そのもの（長さでのグループ分け、パディング、ビームとの掛け合わせ）で、**速くなるかは測っていない**。入れるなら §6.5 の matmul 表（batch 4 と batch 8 がほぼ同時間）が出発点。
- モデル第一候補は FuguMT（Marian, CC-BY-SA-4.0）。
- コメント抽出は正規表現ではなく Tree-sitter を使う（文字列中の URL を行コメントと誤認しないため）。

## 実装上の制約

**IMPORTANT: LSP リクエストの中で同期的に翻訳しないこと。** キャッシュ済みの結果のみ即座に返し、翻訳はバックグラウンドで実行、完了後に `workspace/inlayHint/refresh` / `workspace/codeLens/refresh` を送って Zed に再取得させる。同期実装はどのエンジンを選んでも破綻する。

- 前処理・後処理（識別子 / バッククォート / URL / `@return` / `TODO:` の保全、Javadoc 構造の復元）とキャッシュは `codegloss-core` に置く。`Translator` 実装の中に書くとエンジン差し替え時に失われる。
- Zed の表示方式には以下の差がある:
  - `textDocument/codeLens` → コメント行の**上**に独立した行として描画される（Issue のモックアップ相当）。表示テキストは `command.title` のみで、command なしのレンズは描画されない。
  - `textDocument/inlayHint` → **行内**にしか出せない。行の下に別行は作れない。
  - `textDocument/hover` → ユーザ設定なしで動く唯一のモード。疎通確認の初手に向く。
- Zed 側の既定値は `"code_lens": "off"` と `"inlay_hints": { "enabled": false }`。**インストールしただけでは何も表示されない**ため、README への設定スニペット掲載は必須。
- CodeGloss のヒントは LSP の `kind` を持たないので、Zed では `show_other_hints` の管轄になる。
- 拡張が登録した LSP は Zed の既定設定 `"language_servers": ["..."]` により rust-analyzer 等と自動的に並走する。Inlay Hint / Hover / Code Lens はサーバ横断でマージされるので、共存のための特別な実装は不要。

## ライセンス

- コードは MIT。**モデルの重みはこのリポジトリにコミットしない**（別リポジトリまたはリリースアセットで配布する）。`.gitignore` で重みと変換物（`*.safetensors` / `*.spm` / `pytorch_model.bin` / `tokenizer-*.json` / `/models` / `/model-pack*`）を弾いてある。
- FuguMT は CC-BY-SA-4.0。ONNX / safetensors 等への変換物も二次的著作物なので、CC-BY-SA-4.0 と帰属表示を維持したまま配布する必要がある。**`tools/convert-fugumt` が作る `tokenizer-source.json` / `tokenizer-target.json` もこれに当たる。**そのためモデルパックには `LICENSE`（CC-BY-SA-4.0 全文）と `NOTICE`（帰属表示）を同梱し、`manifest.json` にも `license` / `attribution` を書いている。
- 変換スクリプト（`tools/convert-fugumt/`）自体は重みを含まないのでコミットしてよい。
- NLLB-200 は CC-BY-NC のため採用しない。

## 規約

- `docs/` 配下のドキュメントとコミットメッセージは日本語。コード内の識別子とコメントは英語。
- 作業は `claude/<topic>` ブランチで行い、`main` へ直接プッシュしない。

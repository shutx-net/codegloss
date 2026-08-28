# AGENTS.md

This file provides guidance to coding agents (Claude Code, etc.) when working with code in this repository.

## プロジェクト概要

CodeGloss は、ソースファイルを書き換えずに英語コメントの日本語訳を仮想 UI として表示する読解支援ツール。最初のターゲットは Zed 拡張。

- 設計の全体像: Issue #1
- 技術選定の根拠と Zed ソースの実測調査: `docs/tech-stack-evaluation.md`（必要になったときに読む。常時参照しない）
- Zed 上での表示の確認事項と性能の実測値: `docs/zed-display-notes.md`（**目視確認のチェックリストは未記入のまま。実機を持つ人間が埋める。推測で埋めない**）

## 現状

Cargo ワークスペースがあり、`cargo build --workspace` / `cargo test --workspace` が通る。

- `crates/codegloss-core` — ドメイン型（`CommentBlock` / `Segment` / `Gloss` / `GlossKey`）と翻訳キャッシュ（`GlossCache`。LRU・インメモリ）。前処理・後処理は未実装。`CommentBlock` は `text`（コメント記号を剥がした本文＝翻訳の入力）と `raw`（元のソーステキストそのまま＝後処理の入力）の両方を持つ。
- `crates/codegloss-parser` — Tree-sitter によるコメント抽出。対応言語は Rust のみ。連続する行コメントを 1 ブロックに連結し、区切り線と空コメントは落とす。
- `crates/codegloss-translator` — `trait Translator`（`translate(&[Segment]) -> Vec<String>` と `model_version()`）と `PassthroughTranslator`（入力をそのまま返す）。candle はまだ入っていない。
- `crates/codegloss-lsp` — LSP サーバ。initialize / didOpen / didChange / didClose / hover / codeLens / executeCommand に応答する。hover はコメント上でだけ答え、コード上では `null` を返す。codeLens はコメントブロック 1 件につきレンズ 1 件を、そのコメント行に返す（Zed は行の**上**に描くのでコメントの 1 つ上に出る）。翻訳は `translation.rs` のバックグラウンドワーカーが行い、ハンドラはキャッシュを引くだけ。訳が無いあいだ hover は原文を、codeLens は `⟳ 翻訳中…` を返す（差し替えられるのは後者だけなので、わざと挙動を変えている。理由は `crates/codegloss-lsp/src/code_lens.rs` の冒頭と README）。訳ができると `workspace/inlayHint/refresh` と `workspace/codeLens/refresh` を送る。
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

- コードは MIT。**モデルの重みはこのリポジトリにコミットしない**（別リポジトリまたはリリースアセットで配布する）。
- FuguMT は CC-BY-SA-4.0。ONNX / safetensors 等への変換物も二次的著作物なので、CC-BY-SA-4.0 と帰属表示を維持したまま配布する必要がある。
- NLLB-200 は CC-BY-NC のため採用しない。

## 規約

- `docs/` 配下のドキュメントとコミットメッセージは日本語。コード内の識別子とコメントは英語。
- 作業は `claude/<topic>` ブランチで行い、`main` へ直接プッシュしない。

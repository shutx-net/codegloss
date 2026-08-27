# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## プロジェクト概要

CodeGloss は、ソースファイルを書き換えずに英語コメントの日本語訳を仮想 UI として表示する読解支援ツール。最初のターゲットは Zed 拡張。

- 設計の全体像: Issue #1
- 技術選定の根拠と Zed ソースの実測調査: `docs/tech-stack-evaluation.md`（必要になったときに読む。常時参照しない）

## 現状

実装コードはまだ存在しない（`LICENSE` / `README.md` / `docs/` のみ）。Cargo ワークスペース未作成のため `cargo` コマンドは通らない。

## 確定済みの設計判断

調査済みなので再検討不要。覆す場合は `docs/tech-stack-evaluation.md` の根拠に当たること。

- Zed 拡張は Rust → `wasm32-wasip1` 以外に選択肢がない。拡張の責務はサーバの取得・起動・設定受け渡しのみに留める。
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
- Zed 拡張の動作確認は Zed のコマンドパレットから `zed: install dev extension` で `editors/zed/` を指定する（ローカルビルドの拡張はレジストリ経由ではインストールしない）。

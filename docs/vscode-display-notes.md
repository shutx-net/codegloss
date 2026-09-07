# VS Code 上での表示に関する所見

`docs/zed-display-notes.md` の VS Code 版。Issue #73 で拡張を足すにあたって、
表示まわりで確かめたことと、まだ確かめていないことを分けて書く。

## この文書の読み方

1 節は VS Code のソースで確認済みの前提で、**再調査は要らない**。実装がすでに
従っている。

2 節のチェックリストは **すべて未確認** である。埋めるのは実機を持っている人間で、
**推測を所見として書かないこと。**書くときは、確認した VS Code のバージョンと OS も
併記する。

## 1. 前提（一次情報で確認済み・再調査不要）

`microsoft/vscode` の `main`（2026-09-07 に取得）で確認した挙動。

### 1.1 Code Lens

- レンズは対象行の**上**に描かれる（`src/vs/editor/contrib/codelens/browser/
  codelensWidget.ts` の `ContentWidgetPositionPreference.ABOVE`）。Zed の
  `BlockPlacement::Above` と同じで、Issue #1 のモックアップの形になる。
- **`command` を持たないレンズは描かれない**（同 97 行の `if (lens.command)`）。
  そのうえ、行に載っているレンズがどれも command を持たないと、VS Code は
  **`no commands` という文字列を描く**（112〜114 行）。`codegloss.noop` は
  Zed と同じ理由でここでも要る。
- `command.id` が空文字なら `<a>` ではなく `<span>` で描かれる（99〜105 行）ので、
  VS Code だけならクリックできない素のテキストにもできる。**いまのままで両方の
  エディタで正しく描かれるので、変えていない。**
- 同じ行の複数のレンズは `" | "` で連結される（107 行）。訳文中の `|` を
  全角へ置換する既存の処理はここでも要る。
- レンズは 1 行で、折り返さない。`overflow: hidden` と `text-overflow: ellipsis`、
  `white-space: nowrap` が当たっている（`codelensWidget.css`）。サーバ側の
  120 文字での切り詰めの上に、VS Code 自身の省略が重なる。
- フォントは `editor.codeLensFontSize` / `editor.codeLensFontFamily`。既定は
  どちらも自動（`0` と空文字）で、コードより小さい。

### 1.2 既定値（**Zed と決定的に違う**）

`src/vs/editor/common/config/editorOptions.ts`。

| 設定 | VS Code の既定 | Zed の既定 |
|---|---|---|
| `editor.codeLens` | `true`（6185 行） | `"code_lens": "off"` |
| `editor.inlayHints.enabled` | `'on'`（3274 行） | `enabled: false` |
| `editor.hover.enabled` | `'on'`（2384 行） | 有効 |

**インストールしただけで Code Lens が出る。**README に設定スニペットを載せるのが
必須なのは Zed 側の事情であって、VS Code には無い。

`editor.inlayHints.maximumLength` の既定は **43**（同 3274 行）。Issue #29 に
効くが、これが何を 43 数えるのか（コードポイントか表示幅か）は下の 2 節。

### 1.3 プロトコル

- `vscode-languageclient` は `workspace/codeLens/refresh` に対応している
  （`microsoft/vscode-languageserver-node` の `client/src/common/codeLens.ts`
  46 行で `refreshSupport = true` を宣言し、51 行でハンドラを登録している）。
  訳が届いたらレンズが差し替わる、という設計はそのまま成り立つ。
- `workspace/hover/refresh` は LSP に無い。ホバーが 1 回目に原文を出すのは
  Zed と同じ理由で同じ挙動になる。

### 1.4 VSIX の中の実行ビット

`vsce` は zip のエントリにモードを書き、VS Code は展開時にそれを渡す
（`src/vs/base/node/zip.ts` の `modeFromEntry` → `createWriteStream(.., { mode })`）。
**起動時に `chmod +x` する必要は無い。**リリースワークフローが VSIX を作る前に
実行ビットを立てているので（zip から出したものは実行ビットを持たない）、
そこから先は保たれる。

## 2. 実機で確かめること（**すべて未確認**）

以下は実機を持つ人間が埋める。**推測を所見として書かないこと。**

- [ ] `.rs` / `.go` / `.js` / `.jsx` / `.ts` / `.tsx` のそれぞれで拡張が起動し、
      サーバがコメントを返す（`activationEvents` の 6 つがそれぞれ当たる）
  - 所見:
- [ ] 同梱のサーバが使われる（`codegloss.server.path` を書かない状態で、
      `PATH` に別の `codegloss-lsp` があっても VSIX の中のものが動く）
  - 所見:
- [ ] **日本語のグロスが Code Lens の小さいフォントで読めるか。**`editor.codeLens`
      が既定で on なので、これが VS Code での既定の見え方になる
  - 所見:
- [ ] 長い訳文が VS Code 側の `text-overflow: ellipsis` で切られたとき、
      サーバ側の 120 文字での `…` と二重に見えないか
  - 所見:
- [ ] rust-analyzer / vtsls / gopls のレンズと同じ行に並んだときの見え方
      （` | ` で連結される）
  - 所見:
- [ ] `editor.inlayHints.maximumLength` の **43** が日本語で何文字になるか
      （コードポイント数か、表示幅か）。#29 の設計はこの答えで変わる
  - 所見:
- [ ] 設定を変えたときのサーバの再起動が、編集中に目に見える中断にならないか
      （`onDidChangeConfiguration` で止めて起動し直している）
  - 所見:
- [ ] サーバが見つからない構成（target 無しの VSIX）でのメッセージが、
      実際に何をすればいいか伝わるか
  - 所見:

## 3. 別のこととして残っていること

- **Open VSX**（VSCodium / Cursor / Windsurf）への publish は別手順（`ovsx`）で、
  何もしていない。要るかどうかは配布の方針しだい。
- **Cursor / Windsurf** の VS Code 互換バージョンが `engines.vscode: ^1.91.0`
  （`vscode-languageclient` 10.x の要求）を満たすかは確かめていない。
- **web 版**（vscode.dev）は対象外。サーバがネイティブバイナリなので動かない。
  ブラウザで読むための道は Issue #64 の側にある。

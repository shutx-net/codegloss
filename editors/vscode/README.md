# CodeGloss

Reads the English comments in your code and shows a Japanese translation over
them, without touching the file.

Translation runs locally: `codegloss-lsp`, a native binary shipped with this
extension, holds the model and the cache. Nothing is sent anywhere.

## Languages

Rust, Go, JavaScript, TypeScript, JSX and TSX.

## What you see

- **Code lens** — the translation on its own line, directly above the comment.
  This is on by default in VS Code (`editor.codeLens`).
- **Hover** — the translation and the English source together, in the popup.

A comment that has not been translated yet shows `⟳ 翻訳中…` on its lens; the
lens is replaced as soon as the translation lands.

## First run

The translation model is not part of this extension. The server downloads it
(about 120 MB) the first time it starts, in the background: comments stay in
English until it arrives, and the editor is usable throughout. Set
`codegloss.model.download` to `false` to stop that and point
`codegloss.model.pack` at a pack you installed yourself.

## Settings

| Setting | What it does |
| --- | --- |
| `codegloss.server.path` | Run this `codegloss-lsp` instead of the bundled one. |
| `codegloss.server.arguments` | Extra arguments for the server. |
| `codegloss.model.pack` | Directory holding the model pack. |
| `codegloss.model.precision` | `f32` or `f16`. `f16` halves the memory. |
| `codegloss.model.beams` | Beam search width. `1` is greedy: faster, worse. |
| `codegloss.model.download` | Let the server fetch a model pack by itself. |
| `codegloss.cache.directory` | Where finished translations are kept. |
| `codegloss.cache.enabled` | Keep them across restarts at all. |
| `codegloss.trace.server` | Log LSP traffic to the CodeGloss output channel. |

Run **CodeGloss: Restart Language Server** from the command palette to restart
the server by hand; changing any setting above restarts it anyway.

## Licence

The extension and the server are MIT. The translation model, downloaded
separately, is [FuguMT](https://huggingface.co/staka/fugumt-en-ja) under
CC-BY-SA-4.0; its licence and attribution ship inside the model pack.

Source, issues and the Zed extension:
[github.com/shutx-net/codegloss](https://github.com/shutx-net/codegloss)

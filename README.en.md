# CodeGloss

*[日本語](README.md) · English*

A reading aid that shows English comments in Japanese without touching the source
file.

```java
// Return cached user if available.
   ↳ キャッシュされたユーザーがあれば返します。
public User findUser(String id) {
```

The Japanese exists only in the editor's display. Not one byte of the file
changes, and copying the code gives you the original.

## What it is for

- **Leaves the source alone** — the translation is display only. It shows up in
  no diff and in nothing you copy.
- **Translates locally** — your code is never sent to a service.
- **Needs no setup** — the translation model is fetched on first run. No Python,
  no API key.

## Status

Early. The Zed extension and the language server are in place, Rust comments are
extracted with Tree-sitter, and they come back as hovers and as code lenses (a
line of their own above the comment). **Translation works for real** (candle +
FuguMT, about 0.15 s per sentence on a CPU, when a batch of them is translated
at once as it is when a file is opened).

### What you need to use it

Install the extension. There is no Rust toolchain to set up, nothing to build by
hand, and no Python.

- **The server** (`codegloss-lsp`) is fetched by the extension, from **the
  release matching the extension's own version**, so an old extension is never
  paired with a newer server. A `codegloss-lsp` on `PATH` wins over the download,
  so building your own stays possible.
- **The translation model** (120 MB) is fetched by the server in the background
  after it starts. It answers normally while that happens, showing the comments
  in English, and switches to Japanese once the model arrives. Fetched once,
  reused after that.

**Not downloading 120 MB behind `initialize` is deliberate.** A language server
that does not answer looks broken rather than busy.

`--no-download` turns the automatic fetch off. Even then, `codegloss-lsp
--fetch-model` fetches it explicitly (it downloads and exits without serving).
To use a model pack you converted yourself, pass `--model-pack <dir>`, which
wins over everything else; see
[tools/convert-fugumt/README.md](tools/convert-fugumt/README.md) (Japanese) for
how to make one.

Arguments go in `lsp.codegloss.binary.arguments` in settings.json. See
[DEVELOPERS.en.md, "Running it with the translation model"](DEVELOPERS.en.md#running-it-with-the-translation-model).

Correctness is preferred over speed, so the default is beam search (width 4).
Greedy decoding sometimes stops a sentence part way through, and **the result
still reads as fluent Japanese, so the reader cannot tell**. Beam search costs
only 1.07x over greedy (`--beams 1`), which leaves almost no reason to trade
quality for speed. The measurements are in the documents below.

The engine stays replaceable, behind `trait Translator`.

- Design: [Issue #1](https://github.com/shutx-net/codegloss/issues/1)
- Why this stack: [docs/tech-stack-evaluation.md](docs/tech-stack-evaluation.md) (Japanese)
- Measured speed, memory and sample output: [docs/model-runtime-notes.md](docs/model-runtime-notes.md) (Japanese)

Zed is the first target. Other editors, and a browser extension for reading
source on GitHub, are in view later.

## Display modes and settings

CodeGloss has three display modes in mind.

| Mode | How it looks | Zed setting needed | Status |
|---|---|---|---|
| Hover | the translation appears when the cursor is over a comment (with the original quoted below) | none | done |
| Code lens | the translation appears on a line of its own above the comment | `"code_lens": "on"` | done |
| Inlay hint | the translation appears inline on the comment's own line | `"inlay_hints": { "enabled": true }` | not yet |

Zed defaults to `"code_lens": "off"` and `"inlay_hints": { "enabled": false }`.
**Installing the extension alone shows neither the code lenses nor the inlay
hints.**

How to write the settings is in
[DEVELOPERS.en.md, "Configuring the display modes"](DEVELOPERS.en.md#configuring-the-display-modes).
**Which file you write them in decides whether they take effect**, so that is
the one place they are documented.

### Before the translation lands (hover and code lens differ on purpose)

Translation never runs inside an LSP request. It takes long enough that waiting
for it inside a request would freeze the editor. Comments join a background
queue the moment a file is opened, and land in the cache as they are finished.

What is shown while a translation is missing is **deliberately different**
between the two:

| | While missing | Once it lands |
|---|---|---|
| Hover | the English original, as it is | the translation, on the next hover |
| Code lens | `⟳ 翻訳中…` ("translating…") | replaced by the translation, automatically |

It looks inconsistent, but the two sit in different places.

- **A code lens sits on the line directly above the original.** Putting the
  original there would stack the same English sentence on two adjacent lines,
  which is noise and nothing else. A hover popup, by contrast, covers the code,
  so the original inside it is the only original the reader can see.
- **A code lens can be replaced afterwards.** When a translation is ready the
  server sends `workspace/inlayHint/refresh` and `workspace/codeLens/refresh`,
  and the editor comes back for it. The placeholder is on screen for a moment.
- **A hover has no such mechanism.** There is no `workspace/hover/refresh` in
  the protocol, and nothing else replaces a popup that is already up. Showing
  the English beats showing a "translating…" that can never be corrected.
- Showing **nothing** until the code lens lands was not taken either: a line
  would appear the instant the translation arrived, and the code being read
  would jump. Holding the line is part of what the placeholder is for.

## Development

See [DEVELOPERS.en.md](DEVELOPERS.en.md).

## License

[MIT](LICENSE)

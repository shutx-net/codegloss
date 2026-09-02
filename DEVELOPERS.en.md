# Setting up a development environment

*[日本語](DEVELOPERS.md) · English*

Two ways are provided, a Nix flake and a Dev Container. Either one gives you the
same Rust toolchain.

## One place for the toolchain version

**To change the Rust version, edit `rust-toolchain.toml` and nothing else.** That
file is the single source of truth, and both environments read it.

```
rust-toolchain.toml  (channel = "1.98.0")
   ├─ flake.nix    → rust-bin.fromRustupToolchainFile ./rust-toolchain.toml
   └─ devcontainer → rustup reads the same file by itself
```

It is not only the version number that matches; the artifacts themselves are
identical. rust-overlay fetches from `https://static.rust-lang.org/dist`
(`lib/dist-root.nix`), which is where rustup gets its official artifacts too.

`.devcontainer/devcontainer.json` deliberately does not name a Rust version.
Naming one there would duplicate what `rust-toolchain.toml` says, and the
duplication is itself how the two drift apart.

**There is one exception: `rust-version` in `editors/zed/Cargo.toml` is not kept
in step with this file.** The extension is built by the Zed extension registry,
which this file has no say over (it was on rustc 1.90.0 as of 2026-09). Matching
the two makes the registry's CI fail with `requires rustc X`. The value is the
floor the extension's own code and dependencies actually need, and CI's `msrv`
job checks that it builds at that version.

## Option A: Nix flake

Needs [Nix with flakes enabled](https://nixos.org/download/).

```sh
nix develop
```

If you use [direnv](https://direnv.net/), an `.envrc` (`use flake`) is provided,
so running this once makes the environment activate on entering the directory.

```sh
direnv allow
```

The first `nix develop` generates `flake.lock`. **Commit the generated
`flake.lock`.** It pins the revisions of the inputs, and reproducibility is lost
without it.

Supported systems: `x86_64-linux`, `aarch64-linux`, `x86_64-darwin`,
`aarch64-darwin`.

## Option B: Dev Container

Open `.devcontainer/devcontainer.json` with the VS Code Dev Containers extension
or the [devcontainer CLI](https://github.com/devcontainers/cli).

The base image is `mcr.microsoft.com/devcontainers/rust:1-bookworm`. After the
container is created, `.devcontainer/post-create.sh` runs and does the following.

1. Installs `pkg-config` (the base image's buildpack-deps carries `libssl-dev`
   but not `pkg-config`, so this matches the dependencies of the Nix devShell)
2. Installs the toolchain `rust-toolchain.toml` names
3. Verifies with `scripts/verify-toolchain.sh`

## Verifying the setup

The same script works in either environment.

```sh
scripts/verify-toolchain.sh
```

It checks that the `channel` in `rust-toolchain.toml` matches the actual
`rustc --version`, and that the `wasm32-wasip2` standard library is available.
There is no rustup in the Nix environment, so the check is
`rustc --print target-libdir` rather than `rustup target list`.

## What you get

| Item | Why |
|---|---|
| Rust toolchain (rust-analyzer / rust-src / rustfmt / clippy) | the usual set |
| `wasm32-wasip2` target | the Zed extension compiles to WebAssembly |
| A C compiler (from stdenv, under Nix) | the tree-sitter grammar crates compile C |
| `pkg-config` / `openssl` | needed when a networking crate pulls in native-tls |
| Python (`protobuf` / `sentencepiece` / `tokenizers`) | used by `tools/convert-fugumt` to build a model pack. Not needed to run translation |

Under Nix that Python is declared with `python3.withPackages`. **Do not use
`pip install`** (the Nix Python is not writable). On the Dev Container side,
PEP 668 refuses `pip install` into the system Python, so use `python3 -m venv`.
The steps are in `tools/convert-fugumt/README.md`.

No extra linker is needed for wasm. rustc ships lld and uses it automatically for
wasm targets.

## How the workspace is laid out

```
Cargo.toml          the root workspace (members = ["crates/*"])
 └─ crates/
     ├─ codegloss-core        domain types, pre- and post-processing, cache
     ├─ codegloss-parser      comment extraction with Tree-sitter
     ├─ codegloss-translator  trait Translator and its implementations (Passthrough / candle)
     └─ codegloss-lsp         the language server (the native binary that is shipped)
editors/zed         the Zed extension. A workspace of its own (excluded from the root)
tools/convert-fugumt  the Python script that builds a model pack (not shipped)
```

`editors/zed` is kept out of the root workspace because Zed's builder runs
`cargo build --target wasm32-wasip2` with the extension directory as the working
directory. It must not be mixed with ordinary host-target builds.

```sh
cargo build --workspace   # the native side (crates/*)
cargo test --workspace
```

**The default build has no translation model in it.** candle and tokenizers are
pulled in only with `--features candle` (putting them in the default adds minutes
to a build and makes CI heavy). The server runs without a model and shows the
comments in English.

## Running it with the translation model

1. Build a model pack. See
   [tools/convert-fugumt/README.md](tools/convert-fugumt/README.md) (Japanese)
   for details.

   ```sh
   pip install -r tools/convert-fugumt/requirements.txt
   python3 tools/convert-fugumt/convert.py ~/codegloss-model
   ```

   **Do not put the resulting pack in the repository.** The FuguMT weights are
   CC-BY-SA-4.0, and `.gitignore` is set up to keep them out (AGENTS.md, under
   "ライセンス").

2. Build with candle and start the server on the pack.

   ```sh
   cargo build --release -p codegloss-lsp --features candle
   ./target/release/codegloss-lsp --model-pack ~/codegloss-model
   ```

   `CODEGLOSS_MODEL_PACK=~/codegloss-model` does the same (the argument wins).

   When the pack is missing, broken, or the binary was built without `candle`,
   the reason is logged and the server falls back to Passthrough. **The model is
   never a reason for the server to go down.**

   If you are not converting one yourself, the server fetches the pack in the
   background after it starts (`--no-download` stops that). To fetch it without
   waiting on a server start, use `--fetch-model`: it downloads and exits without
   serving.

   ```sh
   ./target/release/codegloss-lsp --fetch-model
   ```

   The server answers normally while the fetch runs, showing English. When the
   pack lands only the engine is swapped, and `workspace/*/refresh` brings the
   client back for the translations. **The minutes before the translations appear
   are by design**, and better than downloading 120 MB behind `initialize` and
   looking broken to the editor.

3. To use it from Zed, pass the arguments through `binary.arguments` in
   `.zed/settings.json` (the extension hands them to the server as they are).

   ```json
   {
     "lsp": {
       "codegloss": {
         "binary": {
           "path": "/absolute/path/to/codegloss/target/release/codegloss-lsp",
           "arguments": ["--model-pack", "/absolute/path/to/codegloss-model"]
         }
       }
     }
   }
   ```

   This setting has been confirmed to reach the server in a real Zed
   ([Issue #18](https://github.com/shutx-net/codegloss/issues/18)).
   `--beams`, `--precision` and `--cache-dir` are passed the same way.

4. The precision of the weights and the cache of the translations are arguments.

   | Argument | Environment variable | Default | What it changes |
   |---|---|---|---|
   | `--precision f32\|f16` | `CODEGLOSS_MODEL_PRECISION` | `f32` | `f16` brings the resident set from 281 down to 158 MiB, and makes a segment 6-8% slower |
   | `--cache-dir <dir>` | `CODEGLOSS_CACHE_DIR` | `$XDG_CACHE_HOME/codegloss/glosses` | where translations are kept. **A restart does not re-translate** |
   | `--no-cache` | — | off | never write translations to disk (memory only) |
   | `--no-download` | — | off | never fetch the model pack |

   The numbers come from §6 of
   [docs/model-runtime-notes.md](docs/model-runtime-notes.md) (Japanese). When the
   cache directory cannot be created or written to, the reason is logged and the
   server keeps the translations in memory only - as with the model pack, **a
   cache is never a reason for the server to go down**.

5. The tests that need a real model are marked `#[ignore]`. Use `--release`, or
   they are far too slow.

   ```sh
   CODEGLOSS_MODEL_PACK=~/codegloss-model \
     cargo test -p codegloss-translator --features candle --release -- --ignored --nocapture
   ```

   Adding `CODEGLOSS_MODEL_PRECISION=f16` applies the same bar to f16.

6. Measure latency and memory **with the example, not with the tests.** The test
   harness runs `#[ignore]` tests in parallel within one process, so several
   models are resident at once and the RSS is their sum
   ([docs/model-runtime-notes.md](docs/model-runtime-notes.md) §6.1, Japanese).

   ```sh
   CODEGLOSS_MODEL_PACK=~/codegloss-model \
     cargo run -p codegloss-translator --features candle --release --example measure
   ```

   The measurements are in
   [docs/model-runtime-notes.md](docs/model-runtime-notes.md) (Japanese).

7. Comparing **masking policies** - whether identifiers and inline code are
   hidden from the engine or not - is what the measurement harness is for. It
   needs a real model, so it is `#[ignore]`d; use `--release`.

   ```sh
   CODEGLOSS_MODEL_PACK=~/codegloss-model \
     cargo test -p codegloss-translator --features candle --release \
     --test pipelines -- --ignored --nocapture
   ```

   It runs four arms - hide everything (what ships), hide nothing, keep only
   bare identifiers in the text, and translate unhidden but keep the answer
   only where every protected span came back - through one model in one
   process, and prints a scoreboard.

   | Environment variable | Default | What it changes |
   |---|---|---|
   | `CODEGLOSS_CORPUS` | the 62 blocks that ship with the test | the corpus to measure (a file with `%%%` between blocks) |
   | `CODEGLOSS_SHEET` | standard error | where to write the blinded A/B sheet |

   A corpus is built with `codegloss-parser`'s example. **Do not commit one
   built from third-party sources** - the comments are part of that work too.

   ```sh
   cargo run -p codegloss-parser --example extract -- src/*.rs > /tmp/corpus.txt
   ```

   The measurements are §12 of
   [docs/model-runtime-notes.md](docs/model-runtime-notes.md) (Japanese), and
   the A/B sheet a human fills in is
   [docs/masking-ab.md](docs/masking-ab.md) (Japanese).

## Trying the Zed extension

1. Build the language server first. The extension only starts this binary; it
   holds no translation logic at all.

   ```sh
   cargo build -p codegloss-lsp
   ```

2. Write the server's absolute path in the **project settings**,
   `<repo>/.zed/settings.json`. Without it the extension looks at PATH, and
   **downloads from the releases** if it finds nothing there - so your local
   build is ignored, and the code you just fixed looks like it does not work.
   During development this is the only reliable way to name a binary.

   ```json
   {
     "lsp": {
       "codegloss": {
         "binary": {
           "path": "/absolute/path/to/codegloss/target/debug/codegloss-lsp"
         }
       }
     }
   }
   ```

   The `"codegloss"` key is the `[language_servers.codegloss]` table key in
   `editors/zed/extension.toml`. It is neither the display name
   (`name = "CodeGloss"`) nor the extension's id in the registry
   (`id = "codegloss-lsp"`). There are three confusable names in that one file,
   and the wrong one leaves the settings with no effect.

   `.zed/settings.json` is in `.gitignore`, since the absolute path differs per
   machine.

3. To see the code lenses, write `"code_lens": "on"` in the **user settings**, as
   "Configuring the display modes" below says. **Writing it in the project
   settings has no effect.**

   This step is unnecessary if you only want to check that hover works. Hover is
   the one display mode that needs no settings.

4. Run `zed: install dev extension` from Zed's command palette and point it at
   `editors/zed/`. A local build of the extension is not installed through the
   registry. Zed does the wasm build itself.

   **That calls the Rust on your machine, so it fails where there is no Rust.**
   It is not how the extension is distributed (through the registry, a prebuilt
   wasm comes down), so trying it on a machine without Rust means registering it
   first ([Issue #38](https://github.com/shutx-net/codegloss/issues/38)).

5. Open a Rust file and hover over a comment. The body of that comment appearing
   means everything is connected. It shows up alongside rust-analyzer's hover.
   Consecutive `//` lines come out as one. CodeGloss shows no hover over code.

   With the engine still on Passthrough (a dummy that returns its input), what
   appears is the English. Hovering the same comment a second time turns it into
   two parts, the translation with the original quoted below it. That is the sign
   the translation reached the cache; the first hover showing only the original
   is by design (README, "Before the translation lands").

To check the extension's wasm build by hand first, run the following. It uses the
same target and directory layout Zed does.

```sh
cd editors/zed && cargo build --target wasm32-wasip2 --release
# → editors/zed/target/wasm32-wasip2/release/codegloss_zed.wasm
```

When something does not work, look at `zed: open log`. The server's log level is
set with `CODEGLOSS_LOG` (for example `CODEGLOSS_LOG=debug`). Logs go to stderr
only; stdout carries the LSP's JSON-RPC.

## Configuring the display modes

**This section is the single source of truth for the display settings.** The
README points here, and does not repeat how to write them.

| Mode | Setting needed | Where it goes | Per-language override | Status |
|---|---|---|---|---|
| Hover | none | — | — | done |
| Code lens | `"code_lens": "on"` | **user settings only** | **not possible** | done |
| Inlay hint | `"inlay_hints": { "enabled": true }` | either | possible | not yet |

- user settings = `~/.config/zed/settings.json` (the same path on macOS)
- project settings = `<repo>/.zed/settings.json`

Zed defaults to `"code_lens": "off"` and `"inlay_hints": { "enabled": false }`, so
**installing the extension alone shows neither the code lenses nor the inlay
hints.**

### Code lens

```jsonc
{
  "code_lens": "on"
}
```

`"off"` (the default) shows nothing. `"menu"` moves them from above the line into
the code actions menu (`Ctrl` + `.` on Linux), which **is of no use for
CodeGloss**: the block above the line is gone, so the translation is not visible
while reading, and the lenses have an empty range, so **they only appear in the
menu when the cursor is at column 0 of the comment's first line** (confirmed on a
real Zed; `docs/zed-display-notes.md` 2.4, Japanese). Use `"on"` for the line
above.

**`languages.<name>.code_lens` is not a key that exists.** `code_lens` is on
`EditorSettingsContent` only, not on `LanguageSettingsContent`. There is no
`deny_unknown_fields` either, so writing it raises no error and is silently
ignored. Code lenses cannot be switched per language.

### Inlay hint (not implemented yet)

```jsonc
{
  "inlay_hints": {
    "enabled": true,
    "show_type_hints": false,
    "show_parameter_hints": false,
    "show_other_hints": true
  }
}
```

`inlay_hints` is a language setting (`LanguageSettingsContent`), so it can be
narrowed per language under `languages.Rust`. It works in the project settings
too.

Turning it on also brings out rust-analyzer's type and parameter hints, so set
`show_type_hints` / `show_parameter_hints` to `false` if you do not want them.
CodeGloss's hints carry no LSP `kind`, which puts them under `show_other_hints`;
setting that to `false` removes the translations as well.

## Things that trip people up

- **Why `code_lens` has no effect in the project settings.** The rule itself is
  under "Configuring the display modes"; what is here is only the reason. There is
  no error and no warning - simply nothing appears - which makes it hard to trace.

  In Zed 1.17.2 (`c8e44cf`) only three places read this setting, and all three go
  through `EditorSettings::get_global` (`editor.rs:2622`, `editor.rs:9995`,
  `code_actions.rs:543`). `SettingsStore::value_for_path(None)` returns the global
  value, and the only local setting that merges into a global value is
  `disable_ai` (`recompute_values` in `settings_store.rs`).

  `lsp.codegloss.binary.path` in that same `.zed/settings.json` does work, because
  it is read through `LspSettings::for_worktree()` - **an API that takes the file's
  location into account**. **Two settings in one file, and only one of them takes
  effect**, which leaves the server running with only the code lenses missing.
  `inlay_hints` works in the project settings for the same reason: it resolves
  with a location, as `snapshot.language_settings_at(location, cx).inlay_hints`.

- **Install a Japanese font first on WSL and on bare Linux containers.** Without
  one every translated character comes out as □. On Ubuntu:

  ```sh
  sudo apt install -y fonts-noto-cjk && fc-cache -f
  ```

  An empty `fc-list ":charset=3042"` means no font on the system has kana.

- **Since rustup 1.28, the toolchain in `rust-toolchain.toml` is no longer
  installed implicitly.** To install it by hand, use the form rustup's CHANGELOG
  points at.

  ```sh
  rustup show active-toolchain || rustup toolchain install
  ```

- There is no `rustup` in the Nix environment. Commands that assume one do not
  work as written.

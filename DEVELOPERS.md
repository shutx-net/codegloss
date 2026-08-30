# 開発環境のセットアップ

Nix flake と Dev Container の 2 通りを用意している。どちらを選んでも同じ Rust ツールチェーンになる。

## ツールチェーンの一元管理

**Rust のバージョンを変えるときは `rust-toolchain.toml` だけを編集する。** このファイルが唯一の真実で、両方の環境がここから読む。

```
rust-toolchain.toml  (channel = "1.98.0")
   ├─ flake.nix    → rust-bin.fromRustupToolchainFile ./rust-toolchain.toml
   └─ devcontainer → rustup が同じファイルを自動で読む
```

バージョン番号が揃うだけでなく、配布物そのものが同一になる。rust-overlay の取得元は `https://static.rust-lang.org/dist`（`lib/dist-root.nix`）で、rustup が使う公式アーティファクトと同じものだからである。

`.devcontainer/devcontainer.json` にはあえて Rust のバージョンを書いていない。そこに書くと `rust-toolchain.toml` との二重管理になり、それ自体がずれの原因になる。

## 選択肢 A: Nix flake

[Flakes を有効にした Nix](https://nixos.org/download/) が必要。

```sh
nix develop
```

[direnv](https://direnv.net/) を使う場合は `.envrc`（`use flake`）が用意してあるので、初回のみ次を実行すればディレクトリに入るたび自動で有効になる。

```sh
direnv allow
```

初回の `nix develop` で `flake.lock` が生成される。**生成された `flake.lock` はコミットすること。** 入力のリビジョンを固定するもので、これが無いと再現性が失われる。

対応システムは `x86_64-linux` / `aarch64-linux` / `x86_64-darwin` / `aarch64-darwin`。

## 選択肢 B: Dev Container

VS Code の Dev Containers 拡張、または [devcontainer CLI](https://github.com/devcontainers/cli) で `.devcontainer/devcontainer.json` を開く。

ベースイメージは `mcr.microsoft.com/devcontainers/rust:1-bookworm`。コンテナ作成後に `.devcontainer/post-create.sh` が走り、次を行う。

1. `pkg-config` の導入（ベースイメージの buildpack-deps は `libssl-dev` を含むが `pkg-config` を含まないため、Nix 側の devShell と依存を揃える）
2. `rust-toolchain.toml` が指すツールチェーンの導入
3. `scripts/verify-toolchain.sh` による検証

## セットアップの検証

どちらの環境でも同じスクリプトが使える。

```sh
scripts/verify-toolchain.sh
```

`rust-toolchain.toml` の `channel` と実際の `rustc --version` が一致するか、`wasm32-wasip2` の標準ライブラリが利用できるかを確認する。Nix 環境には rustup が存在しないため、`rustup target list` ではなく `rustc --print target-libdir` で判定している。

## 用意されているもの

| 対象 | 理由 |
|---|---|
| Rust ツールチェーン（rust-analyzer / rust-src / rustfmt / clippy） | 開発一式 |
| `wasm32-wasip2` ターゲット | Zed 拡張は WebAssembly にコンパイルされる |
| C コンパイラ（Nix では stdenv 由来） | tree-sitter のグラマークレートが C をコンパイルする |
| `pkg-config` / `openssl` | ネットワーク系クレートが native-tls を引いたときに必要 |

wasm 向けのリンカは追加不要。rustc が lld を同梱しており、wasm ターゲットでは自動的に使われる。

## ワークスペースの構成

```
Cargo.toml          ルートワークスペース（members = ["crates/*"]）
 └─ crates/
     ├─ codegloss-core        ドメイン型・前処理・後処理・キャッシュ
     ├─ codegloss-parser      Tree-sitter によるコメント抽出
     ├─ codegloss-translator  trait Translator と実装（Passthrough / candle）
     └─ codegloss-lsp         LSP サーバ（配布するネイティブバイナリ）
editors/zed         Zed 拡張。独立したワークスペース（ルートからは exclude）
tools/convert-fugumt  モデルパックを作る Python スクリプト（配布物には入らない）
```

`editors/zed` をルートワークスペースから外しているのは、Zed のビルダが拡張
ディレクトリを作業ディレクトリにして `cargo build --target wasm32-wasip2` を
実行するためである。ホストターゲット向けの通常のビルドと混ぜない。

```sh
cargo build --workspace   # ネイティブ側（crates/*）
cargo test --workspace
```

**既定のビルドには翻訳モデルが入らない。**candle と tokenizers は
`--features candle` を付けたときだけ引かれる（既定に入れるとビルドが数分伸び、
CI が重くなるため）。モデル無しでもサーバは動き、コメントは英語のまま出る。

## 翻訳モデルを入れて動かす

1. モデルパックを作る。詳しくは
   [tools/convert-fugumt/README.md](tools/convert-fugumt/README.md)。

   ```sh
   pip install -r tools/convert-fugumt/requirements.txt
   python3 tools/convert-fugumt/convert.py ~/codegloss-model
   ```

   **できたパックはリポジトリに入れないこと。**FuguMT の重みは CC-BY-SA-4.0 で、
   `.gitignore` にも弾く設定を入れてある（AGENTS.md「ライセンス」）。

2. candle 付きでビルドして、パックを渡して起動する。

   ```sh
   cargo build --release -p codegloss-lsp --features candle
   ./target/release/codegloss-lsp --model-pack ~/codegloss-model
   ```

   `CODEGLOSS_MODEL_PACK=~/codegloss-model` でも同じ（引数が優先）。

   パックが見つからない・壊れている・`candle` 無しでビルドされている場合は、
   ログに理由を出して Passthrough にフォールバックする。**モデルが理由で
   サーバが落ちることはない。**

3. Zed から使うときは `.zed/settings.json` の `binary.arguments` に渡す
   （拡張はここをそのままサーバへ渡す）。

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

   **この 3 の設定は実機の Zed では未確認**（サーバ側は stdio 越しに確認済み）。

4. 実モデルが要るテストは `#[ignore]` 付き。速度が出ないので `--release` で。

   ```sh
   CODEGLOSS_MODEL_PACK=~/codegloss-model \
     cargo test -p codegloss-translator --features candle --release -- --ignored --nocapture
   ```

   実測値は [docs/model-runtime-notes.md](docs/model-runtime-notes.md)。

## Zed 拡張の動作確認

1. LSP サーバを先にビルドする。拡張はこのバイナリを起動するだけで、翻訳処理は
   一切持たない。

   ```sh
   cargo build -p codegloss-lsp
   ```

2. **プロジェクト設定** `<repo>/.zed/settings.json` にサーバの絶対パスを書く。
   開発中はサーバを PATH に置かないため、これが無いと拡張はサーバを見つけられない。

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

   キーの `"codegloss"` は `editors/zed/extension.toml` の
   `[language_servers.codegloss]` のテーブルキーであって、表示名の
   `name = "CodeGloss"` ではない。取り違えると設定が効かない。

   `.zed/settings.json` は環境ごとに絶対パスが変わるので `.gitignore` してある。

3. Code Lens で見たいなら「表示方法の設定」のとおり**ユーザ設定**に
   `"code_lens": "on"` を書く。**プロジェクト設定に書いても効かない。**

   ホバーだけで疎通を見るならこの手順は要らない。ホバーは設定なしで動く唯一の
   表示方法である。

4. Zed のコマンドパレットから `zed: install dev extension` を実行し、
   `editors/zed/` を指定する。ローカルビルドの拡張はレジストリ経由では
   インストールしない。wasm へのビルドは Zed 側が実行する。

5. Rust ファイルを開いてコメントの上にホバーする。そのコメントの本文が出れば
   疎通できている。rust-analyzer のホバーと同時に表示される。連続する `//` は
   1 つにまとめて出る。コードの上では CodeGloss のホバーは出ない。

   翻訳エンジンがまだ Passthrough（入力をそのまま返すダミー）なので、出るのは
   英語のままである。同じコメントに 2 回目のホバーをすると、訳文の下に原文が
   引用された 2 段の表示に変わる。これは訳がキャッシュに入ったサインで、
   1 回目に原文だけが出るのは仕様（README「ホバーは 1 回目が原文、2 回目から
   訳文」）。

拡張の wasm を手元で先に確かめたいときは次を実行する。Zed が使うのと同じ
ターゲットとディレクトリ構成になる。

```sh
cd editors/zed && cargo build --target wasm32-wasip2 --release
# → editors/zed/target/wasm32-wasip2/release/codegloss_zed.wasm
```

うまく動かないときは `zed: open log` を見る。サーバ側のログの粒度は環境変数
`CODEGLOSS_LOG`（例 `CODEGLOSS_LOG=debug`）で変えられる。ログは stderr にしか
出さない（stdout は LSP の JSON-RPC が占有している）。

## 表示方法の設定

**表示方法の設定はここが唯一の正である。** README からはこの節を参照しており、
向こうに設定の書き方は載せない。

| 表示方法 | 必要な設定 | 書く場所 | 言語ごとの上書き | 状況 |
|---|---|---|---|---|
| ホバー | 不要 | — | — | 実装済み |
| Code Lens | `"code_lens": "on"` | **ユーザ設定のみ** | **できない** | 実装済み |
| Inlay Hint | `"inlay_hints": { "enabled": true }` | どちらでも可 | できる | 未実装（P9） |

- ユーザ設定 = `~/.config/zed/settings.json`（macOS も同じパス）
- プロジェクト設定 = `<repo>/.zed/settings.json`

Zed の既定は `"code_lens": "off"` と `"inlay_hints": { "enabled": false }` なので、
**拡張を入れただけでは Code Lens も Inlay Hint も表示されない。**

### Code Lens

```jsonc
{
  "code_lens": "on"
}
```

`"off"`（既定）では何も出ない。`"menu"` にすると行の上ではなくコードアクション
メニュー（Linux は `Ctrl` + `.`）の中に入るが、**CodeGloss では使いものにならない。**
行の上のブロックが消えて読んでいる最中に訳文が見えなくなるうえ、レンズの range が
空なので**コメント先頭行の 0 桁目にカーソルがあるときしかメニューに現れない**
（実機確認済み。詳細は `docs/zed-display-notes.md` 2.4）。行の上に出したいなら
`"on"` を使うこと。

**`languages.<name>.code_lens` は存在しないキーである。** `code_lens` は
`EditorSettingsContent` にしかなく、`LanguageSettingsContent` には無い。
`deny_unknown_fields` も付いていないので、書いてもエラーにならず黙って無視される。
言語ごとに Code Lens を切り替えることはできない。

### Inlay Hint（P9 で実装予定）

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

`inlay_hints` は言語設定（`LanguageSettingsContent`）なので、`languages.Rust`
の下に書いて言語ごとに絞れる。プロジェクト設定でも効く。

有効にすると rust-analyzer の型ヒントや引数ヒントも一緒に出るため、不要なら
`show_type_hints` / `show_parameter_hints` を `false` にする。CodeGloss のヒントは
LSP の `kind` を持たないので `show_other_hints` の管轄になり、これを `false` に
すると訳文も消える。

## つまずきやすい点

- **`code_lens` がプロジェクト設定で効かない理由。** 規則そのものは「表示方法の
  設定」にある。ここに書くのは、なぜそうなるのかだけ。何のエラーも警告も出ず、
  ただ何も表示されないので原因にたどり着きにくい。

  Zed 1.17.2（`c8e44cf`）でこの設定を読んでいるのは 3 箇所しかなく、すべて
  `EditorSettings::get_global` である（`editor.rs:2622`、`editor.rs:9995`、
  `code_actions.rs:543`）。`SettingsStore::value_for_path(None)` はグローバル値を
  返すだけで、ローカル設定がグローバル値に合流するのは `disable_ai` だけ
  （`settings_store.rs` の `recompute_values`）。

  同じ `.zed/settings.json` に書いた `lsp.codegloss.binary.path` の方が効くのは、
  あちらが `LspSettings::for_worktree()` という**ファイル位置を見る API** で
  読まれるからである。**同じファイルに書いたのに片方だけ効く**ので、サーバは
  起動しているのに Code Lens だけ出ない、という状態になる。`inlay_hints` が
  プロジェクト設定でも効くのも同じ理由で、あちらは
  `snapshot.language_settings_at(location, cx).inlay_hints` と位置つきで解決される。

- **WSL や素の Linux コンテナでは日本語フォントを先に入れる。** 入っていないと
  訳文がすべて □ になる。Ubuntu なら次のとおり。

  ```sh
  sudo apt install -y fonts-noto-cjk && fc-cache -f
  ```

  `fc-list ":charset=3042"` が空なら、システム上のどのフォントも仮名を持って
  いない。

- **rustup 1.28 以降、`rust-toolchain.toml` のツールチェーンは暗黙にはインストールされない。** 手動で入れる場合は rustup の CHANGELOG が案内している次の形を使う。

  ```sh
  rustup show active-toolchain || rustup toolchain install
  ```

- Nix 環境に `rustup` は入っていない。`rustup` 前提のコマンドはそのままでは動かない。

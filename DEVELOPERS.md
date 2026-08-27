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
     ├─ codegloss-core   ドメイン型・前処理・後処理・キャッシュ
     └─ codegloss-lsp    LSP サーバ（配布するネイティブバイナリ）
editors/zed         Zed 拡張。独立したワークスペース（ルートからは exclude）
```

`editors/zed` をルートワークスペースから外しているのは、Zed のビルダが拡張
ディレクトリを作業ディレクトリにして `cargo build --target wasm32-wasip2` を
実行するためである。ホストターゲット向けの通常のビルドと混ぜない。

```sh
cargo build --workspace   # ネイティブ側（crates/*）
cargo test --workspace
```

## Zed 拡張の動作確認

Zed のコマンドパレットから `zed: install dev extension` を実行し、`editors/zed/` を指定する。ローカルビルドの拡張はレジストリ経由ではインストールしない。

## つまずきやすい点

- **rustup 1.28 以降、`rust-toolchain.toml` のツールチェーンは暗黙にはインストールされない。** 手動で入れる場合は rustup の CHANGELOG が案内している次の形を使う。

  ```sh
  rustup show active-toolchain || rustup toolchain install
  ```

- Nix 環境に `rustup` は入っていない。`rustup` 前提のコマンドはそのままでは動かない。

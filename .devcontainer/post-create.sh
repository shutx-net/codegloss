#!/usr/bin/env bash
# devcontainer の初回作成時に一度だけ実行される。
set -euo pipefail

# flake.nix 側の devShell と依存を揃える。ベースイメージ（buildpack-deps）は
# libssl-dev を含むが pkg-config は含まないため、ここで補う。
sudo apt-get update -qq
# python3-venv は tools/convert-fugumt 用。Debian 12 は PEP 668 により
# システムの Python への pip install を拒むので、venv を切れる状態にしておく。
sudo apt-get install -y --no-install-recommends pkg-config python3-venv
sudo rm -rf /var/lib/apt/lists/*

# rustup 1.28 以降、rust-toolchain.toml のツールチェーンは暗黙には
# インストールされない。rustup の CHANGELOG が案内している手順に従う。
# https://github.com/rust-lang/rustup/blob/master/CHANGELOG.md
rustup show active-toolchain || rustup toolchain install

scripts/verify-toolchain.sh

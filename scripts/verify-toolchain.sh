#!/usr/bin/env bash
# Nix devShell と devcontainer で Rust のツールチェーンがずれていないか検証する。
# rustup の有無に依存しないので、どちらの環境でもそのまま実行できる。
set -euo pipefail
cd "$(dirname "$0")/.."

fail() { printf 'NG: %s\n' "$1" >&2; exit 1; }

# channel には正確なバージョン（例 1.98.0）を書く前提。"stable" 等に
# 変えるとこの比較は成立しなくなる。
expected="$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\(.*\)"$/\1/p' rust-toolchain.toml)"
[ -n "$expected" ] || fail "rust-toolchain.toml から channel を読み取れませんでした"

actual="$(rustc --version | cut -d' ' -f2)"
[ "$actual" = "$expected" ] || fail "rust-toolchain.toml は $expected を指定していますが、rustc は $actual です"

# Zed 拡張のビルドに必要な wasm ターゲットが使えるか。
# rustup target list は Nix 環境では使えないため rustc に直接問い合わせる。
target="wasm32-wasip2"
libdir="$(rustc --print target-libdir --target "$target" 2>/dev/null || true)"
[ -n "$libdir" ] && [ -d "$libdir" ] || fail "ターゲット $target の標準ライブラリが見つかりません"

printf 'OK: rustc %s / %s\n' "$actual" "$target"

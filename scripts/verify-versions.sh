#!/usr/bin/env bash
# 版が書いてある 3 か所がずれていないか検証する。
#
# リリースのタグは手で打つ。打つ値が正しいことは、打つ人ではなくこれが保証する。
#
# ずれると何が起きるか:
#   - extension.toml と Cargo.toml がずれる  -> 拡張が、存在しない版のバイナリを
#     リリースから探しに行く。利用者から見るとインストールが失敗する
#   - タグと Cargo.toml がずれる            -> 配ったあと、何が入っているのか
#     追えなくなる（こちらは release.yml が公開の直前に見ている）
#
# 引数にタグを渡すと、それも同じ値かを見る。省略すると 3 か所だけを見る。
#
#     scripts/verify-versions.sh
#     scripts/verify-versions.sh v0.1.0
set -euo pipefail
cd "$(dirname "$0")/.."

fail() { printf 'NG: %s\n' "$1" >&2; exit 1; }

# `[workspace.package]` の次の `[` までを切り出してから読む。ファイル全体を
# 見ると `[workspace.dependencies]` 側の version= を拾ってしまう。
section_version() {
  sed -n "/^\\[$2\\]/,/^\\[/p" "$1" | sed -n 's/^version[[:space:]]*=[[:space:]]*"\(.*\)"$/\1/p' | head -1
}

workspace="$(section_version Cargo.toml workspace.package)"
[ -n "$workspace" ] || fail "Cargo.toml の [workspace.package] から version を読み取れませんでした"

extension_crate="$(section_version editors/zed/Cargo.toml package)"
[ -n "$extension_crate" ] || fail "editors/zed/Cargo.toml から version を読み取れませんでした"

# extension.toml はセクションの外に書く。テーブルより前にあるので、
# 最初の [ までを切り出す。
extension="$(sed -n '1,/^\[/p' editors/zed/extension.toml | sed -n 's/^version[[:space:]]*=[[:space:]]*"\(.*\)"$/\1/p' | head -1)"
[ -n "$extension" ] || fail "editors/zed/extension.toml から version を読み取れませんでした"

[ "$extension_crate" = "$workspace" ] ||
  fail "editors/zed/Cargo.toml は $extension_crate ですが、ワークスペースは $workspace です"
[ "$extension" = "$workspace" ] ||
  fail "editors/zed/extension.toml は $extension ですが、ワークスペースは $workspace です"

if [ $# -gt 0 ]; then
  # 形式もここで見る。ワークフローのタグ絞り込みは「当てはまらないと何も
  # 起きない」ので、通ってしまった形を落とすのはこちら側の仕事になる。
  #
  # case のパターンに繰り返しは書けない（* は何でも拾ってしまい、
  # v1.0.0-rc1 のような後ろに付いた形を通してしまう）ので、3 つに分けて見る。
  bad_tag() { fail "タグ $1 の形式が v<major>.<minor>.<patch> ではありません"; }
  case "$1" in v*.*.*) ;; *) bad_tag "$1" ;; esac
  tag="${1#v}"
  # 数字と区切り以外が混ざっていないか。
  case "$tag" in *[!0-9.]*) bad_tag "$1" ;; esac
  # 区切りがちょうど 2 つで、どの成分も空でないか。
  case "$tag" in *.*.*.* | .* | *. | *..*) bad_tag "$1" ;; esac
  [ "$tag" = "$workspace" ] ||
    fail "タグ $1 に対して、ワークスペースの版は $workspace です"
  printf 'OK: 版は %s（タグ・ワークスペース・拡張のすべて）\n' "$workspace"
else
  printf 'OK: 版は %s（ワークスペースと拡張）\n' "$workspace"
fi

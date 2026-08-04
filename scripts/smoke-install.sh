#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  printf 'usage: %s <extracted-release-directory>\n' "$0" >&2
  exit 2
fi

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="$(awk -F '"' '/^version = / { print $2; exit }' "$ROOT/Cargo.toml")"
SOURCE="$(cd -- "$1" && pwd)"
PREFIX="$(mktemp -d)"
trap 'rm -rf -- "$PREFIX"' EXIT

PREFIX="$PREFIX" "$SOURCE/install.sh"
FIRST="$("$PREFIX/bin/csgraph" --version)"
PREFIX="$PREFIX" "$SOURCE/install.sh"
SECOND="$("$PREFIX/bin/csgraph" --version)"

[[ "$FIRST" == "csgraph $VERSION" ]]
[[ "$SECOND" == "$FIRST" ]]
[[ -x "$PREFIX/bin/code-system-graph-hooks" ]]
[[ -f "$PREFIX/share/code-system-graph/install-manifest-v1.txt" ]]

PREFIX="$PREFIX" "$SOURCE/uninstall.sh"
[[ ! -e "$PREFIX/bin/csgraph" ]]
[[ ! -e "$PREFIX/bin/code-system-graph-hooks" ]]

printf 'install, upgrade, and uninstall smoke passed\n'

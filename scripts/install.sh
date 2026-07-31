#!/usr/bin/env bash
set -euo pipefail

SOURCE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="${PREFIX:-${HOME}/.local}"
BIN_DIR="$PREFIX/bin"
SHARE_DIR="$PREFIX/share/code-system-graph"
DOC_DIR="$PREFIX/share/doc/code-system-graph"
MANIFEST="$SHARE_DIR/install-manifest-v1.txt"
BACKUP_DIR="$SHARE_DIR/backups"
TIMESTAMP="$(date -u +%Y%m%dT%H%M%SZ)"

if [[ ! -x "$SOURCE/bin/csgraph" || ! -x "$SOURCE/bin/code-system-graph-hooks" ]]; then
  printf 'install source must contain bin/csgraph and bin/code-system-graph-hooks\n' >&2
  exit 2
fi

mkdir -p "$BIN_DIR" "$SHARE_DIR" "$DOC_DIR" "$BACKUP_DIR"
chmod 0700 "$SHARE_DIR" "$BACKUP_DIR"

for binary in csgraph code-system-graph-hooks; do
  destination="$BIN_DIR/$binary"
  if [[ -e "$destination" ]]; then
    cp -p "$destination" "$BACKUP_DIR/$binary.$TIMESTAMP"
  fi
  temporary="$destination.tmp.$$"
  install -m 0755 "$SOURCE/bin/$binary" "$temporary"
  mv -f "$temporary" "$destination"
done

if [[ -d "$SOURCE/share/doc/code-system-graph" ]]; then
  cp -R "$SOURCE/share/doc/code-system-graph/." "$DOC_DIR/"
fi

{
  printf '%s\n' "$BIN_DIR/csgraph"
  printf '%s\n' "$BIN_DIR/code-system-graph-hooks"
  printf '%s\n' "$DOC_DIR"
} > "$MANIFEST"
chmod 0600 "$MANIFEST"

"$BIN_DIR/csgraph" --version
printf 'Code System Graph installed under %s\n' "$PREFIX"

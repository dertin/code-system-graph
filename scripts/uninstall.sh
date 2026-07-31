#!/usr/bin/env bash
set -euo pipefail

PREFIX="${PREFIX:-${HOME}/.local}"
SHARE_DIR="$PREFIX/share/code-system-graph"
MANIFEST="$SHARE_DIR/install-manifest-v1.txt"
EXPECTED_BIN="$PREFIX/bin"
EXPECTED_DOC="$PREFIX/share/doc/code-system-graph"

if [[ ! -f "$MANIFEST" ]]; then
  printf 'Code System Graph install manifest was not found under %s\n' "$PREFIX" >&2
  exit 3
fi

while IFS= read -r path; do
  case "$path" in
    "$EXPECTED_BIN/csgraph"|"$EXPECTED_BIN/code-system-graph-hooks")
      rm -f -- "$path"
      ;;
    "$EXPECTED_DOC")
      rm -rf -- "$path"
      ;;
    *)
      printf 'refusing to remove unexpected manifest path: %s\n' "$path" >&2
      exit 5
      ;;
  esac
done < "$MANIFEST"

rm -f -- "$MANIFEST"
if [[ -d "$SHARE_DIR/backups" ]] && [[ -z "$(ls -A "$SHARE_DIR/backups")" ]]; then
  rmdir "$SHARE_DIR/backups"
fi
if [[ -d "$SHARE_DIR" ]] && [[ -z "$(ls -A "$SHARE_DIR")" ]]; then
  rmdir "$SHARE_DIR"
fi
printf 'Code System Graph removed from %s\n' "$PREFIX"

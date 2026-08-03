#!/usr/bin/env bash
set -euo pipefail

root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
hooks_dir="$(git -C "$root" rev-parse --git-path hooks)"
source_dir="$root/scripts/git-hooks"

if [[ ! -d "$source_dir" ]]; then
  printf 'missing hook templates: %s\n' "$source_dir" >&2
  exit 1
fi

mkdir -p "$hooks_dir"

for hook in "$source_dir"/*; do
  [[ -f "$hook" ]] || continue
  name="$(basename -- "$hook")"
  install -m 0755 "$hook" "$hooks_dir/$name"
  printf 'installed %s\n' "$name"
done

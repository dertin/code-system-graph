#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  printf 'usage: %s <manifest-path>\n' "$0" >&2
  exit 2
fi

manifest="$1"
repo_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

cd "$repo_root"

export COOLDOWN_LOCKFILE_BASELINE="${COOLDOWN_LOCKFILE_BASELINE:-ignore}"

cargo cooldown check --manifest-path "$manifest" --locked

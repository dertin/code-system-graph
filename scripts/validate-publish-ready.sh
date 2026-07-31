#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="$(rustc -vV | awk '/^host:/ { print $2 }')"
PACKAGE_DIR="$ROOT/dist/code-system-graph-$TARGET-v1.0.0"

cd "$ROOT"

cargo +nightly fmt --all -- --check
cargo +nightly clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo +stable check --workspace --all-targets --all-features --locked
cargo +stable test --workspace --all-targets --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo +stable doc --workspace --all-features --no-deps --locked
cargo deny check
cargo audit --deny warnings
if command -v gitleaks >/dev/null 2>&1; then
  gitleaks dir --no-banner --redact "$ROOT"
elif command -v docker >/dev/null 2>&1; then
  container="$(docker create zricethezav/gitleaks:latest dir /tmp --no-banner --redact)"
  trap 'docker rm -f "$container" >/dev/null 2>&1 || true' EXIT
  tar \
    --exclude='./.git' \
    --exclude='./.cursor' \
    --exclude='./.codegraph/daemon.sock' \
    --exclude='./dist' \
    --exclude='./target' \
    -cf - . | docker cp - "$container:/tmp"
  docker start --attach "$container"
  docker rm "$container" >/dev/null
  trap - EXIT
else
  printf 'gitleaks or Docker is required for secret scanning\n' >&2
  exit 1
fi

"$ROOT/scripts/package-release.sh" "$TARGET"
"$ROOT/scripts/smoke-install.sh" "$PACKAGE_DIR"

if rg -n -i \
  'phase[ _-]?[0-9]+|fase[ _-]?[0-9]+|PLAN\.md|STATUS\.md|PROMPT-(GPT|RESUME)|schema v[2-9][0-9]*|3700fe3|93c0078|/opt/procesador' \
  "$ROOT" \
  --glob '!target/**' \
  --glob '!dist/**' \
  --glob '!.git/**' \
  --glob '!.cursor/**' \
  --glob '!scripts/validate-publish-ready.sh'
then
  printf 'internal development references remain in the publishable tree\n' >&2
  exit 1
fi

printf 'Code System Graph 1.0.0 publish-readiness validation passed\n'

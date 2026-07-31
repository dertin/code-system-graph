#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="$(awk -F '"' '/^version = / { print $2; exit }' "$ROOT/Cargo.toml")"
TARGET="${1:-$(rustc -vV | awk '/^host:/ { print $2 }')}"
DIST="${DIST_DIR:-$ROOT/dist}"
NAME="code-system-graph-${TARGET}-v${VERSION}"
STAGE="$DIST/$NAME"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
EXE_SUFFIX=""
if [[ "$TARGET" == *windows* ]]; then
  EXE_SUFFIX=".exe"
fi

mkdir -p "$DIST"
rm -rf "$STAGE"
mkdir -p "$STAGE/bin" "$STAGE/share/doc/code-system-graph"

CARGO_TARGET_DIR="$TARGET_DIR" cargo build \
  --manifest-path "$ROOT/Cargo.toml" \
  --release \
  --locked \
  --target "$TARGET" \
  --bin csgraph \
  --bin code-system-graph-hooks

install -m 0755 "$TARGET_DIR/$TARGET/release/csgraph$EXE_SUFFIX" "$STAGE/bin/"
install -m 0755 "$TARGET_DIR/$TARGET/release/code-system-graph-hooks$EXE_SUFFIX" "$STAGE/bin/"
install -m 0644 "$ROOT/LICENSE" "$ROOT/NOTICE" "$ROOT/THIRD_PARTY_NOTICES.md" \
  "$ROOT/README.md" "$ROOT/SECURITY.md" "$STAGE/share/doc/code-system-graph/"
cp -R "$ROOT/docs" "$STAGE/share/doc/code-system-graph/"
install -m 0755 "$ROOT/scripts/install.sh" "$ROOT/scripts/uninstall.sh" "$STAGE/"

SBOM="$DIST/$NAME.cdx.json"
syft \
  "dir:$ROOT" \
  --exclude './target/**' \
  --exclude './dist/**' \
  --source-name "Code System Graph" \
  --source-version "$VERSION" \
  -o cyclonedx-json > "$SBOM"

ARCHIVE="$DIST/$NAME.tgz"
tar \
  --sort=name \
  --mtime="@${SOURCE_DATE_EPOCH:-0}" \
  --owner=0 \
  --group=0 \
  --numeric-owner \
  -C "$DIST" \
  -czf "$ARCHIVE" \
  "$NAME"

(
  cd "$DIST"
  sha256sum "$(basename "$ARCHIVE")" "$(basename "$SBOM")" > "$NAME.sha256"
)

printf '%s\n' "$ARCHIVE" "$SBOM" "$DIST/$NAME.sha256"

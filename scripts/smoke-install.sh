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
TEMPLATE_ROOT="$SOURCE/share/code-system-graph/agent-integration-template"
[[ -f "$TEMPLATE_ROOT/README.md" ]]
[[ -f "$TEMPLATE_ROOT/agent-plugin/plugin.json" ]]
[[ -f "$TEMPLATE_ROOT/agent-plugin/mcp.json" ]]
[[ -f "$TEMPLATE_ROOT/agent-plugin/generated.gitignore" ]]
[[ -f "$TEMPLATE_ROOT/agent-plugin/metadata/skill-description.txt" ]]
[[ -f "$TEMPLATE_ROOT/agent-plugin/metadata/openai-display-name.txt" ]]
[[ -f "$TEMPLATE_ROOT/agent-plugin/metadata/openai-default-prompt.txt" ]]
[[ -f "$TEMPLATE_ROOT/agent-plugin/skills/code-system-graph/SKILL.md" ]]
[[ -f "$TEMPLATE_ROOT/agent-plugin/skills/code-system-graph/references/operating-guide.md" ]]
[[ -f "$TEMPLATE_ROOT/native-hooks/strict-gate.sh" ]]

SMOKE_WORKSPACE="$PREFIX/plugin-smoke"
mkdir -p "$SMOKE_WORKSPACE/repo/src"
printf '%s\n' 'pub fn smoke() {}' > "$SMOKE_WORKSPACE/repo/src/lib.rs"
printf '%s\n' 'version: 1' 'name: release-plugin-smoke' 'repos:' '  app:' '    path: repo' \
  > "$SMOKE_WORKSPACE/code-system-graph.yaml"
"$PREFIX/bin/csgraph" scan \
  --config "$SMOKE_WORKSPACE/code-system-graph.yaml" \
  --database "$SMOKE_WORKSPACE/graph.db" > /dev/null
"$PREFIX/bin/csgraph" plugin create \
  --output "$SMOKE_WORKSPACE/plugin" \
  --config "$SMOKE_WORKSPACE/code-system-graph.yaml" \
  --database "$SMOKE_WORKSPACE/graph.db" > "$SMOKE_WORKSPACE/plugin-report.json"
[[ -f "$SMOKE_WORKSPACE/plugin/plugin.json" ]]
[[ -f "$SMOKE_WORKSPACE/plugin/mcp.json" ]]
[[ -f "$SMOKE_WORKSPACE/plugin/.gitignore" ]]
[[ -f "$SMOKE_WORKSPACE/plugin/.local/code-system-graph/mcp-binding.json" ]]
grep -q '^/.local/$' "$SMOKE_WORKSPACE/plugin/.gitignore"
! grep -E -R -q '\{\{[A-Z0-9_]+\}\}' "$SMOKE_WORKSPACE/plugin"
grep -q '"changed":true' "$SMOKE_WORKSPACE/plugin-report.json"

BASE_PLUGIN="$SMOKE_WORKSPACE/base-plugin"
mkdir -p "$BASE_PLUGIN/.codex-plugin" "$BASE_PLUGIN/skills/release-system-graph"
printf '%s\n' '{"name":"release-base","version":"1.0.0","description":"Release smoke base"}' \
  > "$BASE_PLUGIN/plugin.json"
printf '%s\n' '{"mcpServers":{"release-code-system-graph":{"type":"stdio","command":"csgraph","args":["mcp","--binding","${PLUGIN_ROOT}/.local/code-system-graph/mcp-binding.json"],"env":{"CODE_SYSTEM_GRAPH_MCP_ADMIN":"0"}}}}' \
  > "$BASE_PLUGIN/mcp.json"
printf '%s\n' '{"name":"release-base","version":"1.0.0","mcpServers":{"release-code-system-graph":{"type":"stdio","command":"csgraph","args":["mcp","--binding","${PLUGIN_ROOT}/.local/code-system-graph/mcp-binding.json"],"env":{"CODE_SYSTEM_GRAPH_MCP_ADMIN":"0"}}}}' \
  > "$BASE_PLUGIN/.codex-plugin/plugin.json"
printf '%s\n' '/.local/' > "$BASE_PLUGIN/.gitignore"
printf '%s\n' '---' 'name: release-system-graph' 'description: Release smoke routing skill.' '---' \
  > "$BASE_PLUGIN/skills/release-system-graph/SKILL.md"
"$PREFIX/bin/csgraph" plugin create \
  --output "$BASE_PLUGIN" \
  --mcp-server-name release-code-system-graph \
  --routing-skill release-system-graph \
  --config "$SMOKE_WORKSPACE/code-system-graph.yaml" \
  --database "$SMOKE_WORKSPACE/graph.db" > "$SMOKE_WORKSPACE/existing-plugin-report.json"
[[ -f "$BASE_PLUGIN/.local/code-system-graph/mcp-binding.json" ]]
grep -q '"workspace": "release-plugin-smoke"' "$BASE_PLUGIN/.local/code-system-graph/mcp-binding.json"
grep -q '"mode":"existing_plugin"' "$SMOKE_WORKSPACE/existing-plugin-report.json"
grep -q '"changed":true' "$SMOKE_WORKSPACE/existing-plugin-report.json"

PREFIX="$PREFIX" "$SOURCE/uninstall.sh"
[[ ! -e "$PREFIX/bin/csgraph" ]]
[[ ! -e "$PREFIX/bin/code-system-graph-hooks" ]]

printf 'install, upgrade, and uninstall smoke passed\n'

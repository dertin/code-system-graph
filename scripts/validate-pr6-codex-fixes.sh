#!/usr/bin/env bash
# Reproduces and validates Codex Review findings addressed in PR #6.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

pass() { printf '  [PASS] %s\n' "$1"; }
fail() { printf '  [FAIL] %s\n' "$1"; exit 1; }
section() { printf '\n== %s ==\n' "$1"; }

run_test() {
    local description="$1"
    shift
    if "$@"; then
        pass "$description"
    else
        fail "$description"
    fi
}

section 'P1: Unsafe artifact path must not echo rejected display in diagnostics'
run_test 'ApplicationError::UnsafeArtifactPath regression test' \
    cargo test -p code-system-graph unsafe_artifact_path_error_must_not_echo_rejected_display --locked -- --nocapture

section 'P2: Non-regular local config entry must not be silently ignored'
run_test 'resolve_repository_config rejects directory-shaped .code-system-graph.yaml' \
    cargo test -p code-system-graph-core repository_local_config_should_reject_directory_entry --locked -- --nocapture

TMP_CONFIG="$(mktemp -d)"
trap 'rm -rf "$TMP_CONFIG"' EXIT
mkdir "$TMP_CONFIG/.code-system-graph.yaml"
if cargo test -p code-system-graph-core repository_local_config_should_reject_directory_entry --locked >/dev/null 2>&1; then
    pass 'filesystem fixture: directory entry at .code-system-graph.yaml is rejected by resolver'
else
    fail 'filesystem fixture: directory entry at .code-system-graph.yaml is rejected by resolver'
fi

section 'P2: Missing managed parent must be treated as absent during uninstall'
run_test 'ManagedRoot::remove_file_if_exists on missing parent' \
    cargo test -p code-system-graph-hooks managed_root_should_treat_missing_parent --locked -- --nocapture
run_test 'CapabilityDir::remove_file_if_exists on missing parent' \
    cargo test -p code-system-graph-core capability_dir_should_treat_missing_parent --locked -- --nocapture
run_test 'uninstall on clean repository without hooks directory is a no-op' \
    cargo test -p code-system-graph-hooks uninstall_on_clean_repository_without_hooks_directory --locked -- --nocapture

section 'P2: Bounded reads must continue until EOF (short-read simulation)'
run_test 'capability_dir read_file_to_end_bounded survives chunked reads' \
    cargo test -p code-system-graph-core read_file_to_end_bounded_should_survive_short_reads --locked -- --nocapture
run_test 'capability_dir read_file_to_end_bounded enforces max bytes after chunked reads' \
    cargo test -p code-system-graph-core read_file_to_end_bounded_should_reject_overflow_after_short_reads --locked -- --nocapture
run_test 'managed_root read_file_to_end_bounded survives chunked reads' \
    cargo test -p code-system-graph-hooks read_file_to_end_bounded_should_survive_short_reads --locked -- --nocapture
run_test 'managed_root read_file_to_end_bounded enforces max bytes after chunked reads' \
    cargo test -p code-system-graph-hooks read_file_to_end_bounded_should_reject_overflow_after_short_reads --locked -- --nocapture

section 'Regression guard: full workspace test command used by CI'
run_test 'cargo +stable test --workspace --exclude code-system-graph-fuzz --all-targets --all-features --locked' \
    cargo +stable test --workspace --exclude code-system-graph-fuzz --all-targets --all-features --locked

printf '\nAll Codex PR #6 validations passed.\n'

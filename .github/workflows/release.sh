#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 <version> [prepare|publish]" >&2
  echo "  prepare  Run publish-readiness gates and dry-run all workspace crates (default)." >&2
  echo "  publish  Push a signed tag, publish crates, and dispatch the GitHub release workflow." >&2
  exit 2
}

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  usage
fi

version="$1"
mode="${2:-prepare}"
tag="v${version}"

if [[ ! "$version" =~ ^[0-9]+[.][0-9]+[.][0-9]+$ ]]; then
  echo "Version must look like X.Y.Z; got ${version}" >&2
  exit 2
fi

if [ "$mode" != "prepare" ] && [ "$mode" != "publish" ]; then
  usage
fi

repo_root="$(git rev-parse --show-toplevel)"
cd "$repo_root"

github_repo_from_remote() {
  local url
  url="$(git remote get-url origin)"
  case "$url" in
    git@github.com:*)
      printf '%s\n' "${url#git@github.com:}" | sed 's/\.git$//'
      ;;
    https://github.com/*)
      printf '%s\n' "${url#https://github.com/}" | sed 's/\.git$//'
      ;;
    *)
      echo "Unsupported origin remote: ${url}" >&2
      exit 1
      ;;
  esac
}

github_repo="$(github_repo_from_remote)"
workspace_metadata="$(cargo metadata --locked --no-deps --format-version 1)"
publishable_crates_json='["code-system-graph-model","code-system-graph-hooks","code-system-graph-store-sqlite","code-system-graph-core","code-system-graph"]'
manifest_version="$(
  jq -er --argjson publishable "$publishable_crates_json" \
    '[.packages[] | select(.name as $n | $publishable | index($n)) | .version] | unique | if length == 1 then .[0] else error("publishable crate versions differ") end' \
    <<<"$workspace_metadata"
)"
manifest_repo="$(
  jq -er --argjson publishable "$publishable_crates_json" \
    '[.packages[] | select(.name as $n | $publishable | index($n)) | .repository] | unique | if length == 1 then .[0] else error("publishable crate repositories differ") end' \
    <<<"$workspace_metadata"
)"

if [ "$manifest_version" != "$version" ]; then
  echo "Workspace version ${manifest_version} does not match ${version}" >&2
  exit 1
fi

if [ "$manifest_repo" != "https://github.com/${github_repo}" ]; then
  echo "Workspace repository ${manifest_repo} does not match origin ${github_repo}" >&2
  exit 1
fi

crate_manifests=(
  crates/code-system-graph-model/Cargo.toml
  crates/code-system-graph-hooks/Cargo.toml
  crates/code-system-graph-store-sqlite/Cargo.toml
  crates/code-system-graph-core/Cargo.toml
  crates/code-system-graph-cli/Cargo.toml
)

crate_names=(
  code-system-graph-model
  code-system-graph-hooks
  code-system-graph-store-sqlite
  code-system-graph-core
  code-system-graph
)

crate_version_exists() {
  cargo info --registry crates-io "${1}@${version}" >/dev/null 2>&1
}

wait_for_crate() {
  local crate_name="$1"
  for _ in {1..60}; do
    if crate_version_exists "$crate_name"; then
      return 0
    fi
    sleep 5
  done
  echo "Timed out waiting for ${crate_name} ${version} on crates.io" >&2
  return 1
}

dry_run_crate() {
  local crate_name="$1"
  local manifest="$2"
  local patch_args=()

  # First publication cannot resolve unpublished workspace crates from crates.io. Patch only the
  # dry-run verification graph to the local packages; real publication remains registry-backed.
  case "$crate_name" in
    code-system-graph-store-sqlite | code-system-graph-core)
      patch_args=(
        --config 'patch.crates-io.code-system-graph-model.path="crates/code-system-graph-model"'
      )
      ;;
    code-system-graph)
      patch_args=(
        --config 'patch.crates-io.code-system-graph-model.path="crates/code-system-graph-model"'
        --config 'patch.crates-io.code-system-graph-hooks.path="crates/code-system-graph-hooks"'
        --config 'patch.crates-io.code-system-graph-store-sqlite.path="crates/code-system-graph-store-sqlite"'
        --config 'patch.crates-io.code-system-graph-core.path="crates/code-system-graph-core"'
      )
      ;;
  esac

  cargo publish --locked --dry-run --manifest-path "$manifest" "${patch_args[@]}"
}

preflight_publish_readiness() {
  local index
  git diff --check
  for index in "${!crate_manifests[@]}"; do
    dry_run_crate "${crate_names[$index]}" "${crate_manifests[$index]}"
  done
}

publish_workspace() {
  local index crate_name manifest
  for index in "${!crate_manifests[@]}"; do
    manifest="${crate_manifests[$index]}"
    crate_name="${crate_names[$index]}"
    if crate_version_exists "$crate_name"; then
      echo "${crate_name} ${version} is already published"
      continue
    fi
    cargo publish --locked --manifest-path "$manifest"
    wait_for_crate "$crate_name"
  done
}

trigger_github_release() {
  local remote_head="$1"
  gh workflow run release.yml -R "$github_repo" --ref main -f "tag=${tag}"

  local run_id=""
  for _ in {1..30}; do
    run_id="$(
      gh run list \
        -R "$github_repo" \
        --workflow release.yml \
        --event workflow_dispatch \
        --branch main \
        --commit "$remote_head" \
        --json databaseId,displayTitle \
        --jq ".[] | select(.displayTitle == \"Release ${tag}\") | .databaseId" \
        --limit 20 |
        head -n 1
    )"
    if [ -n "$run_id" ]; then
      break
    fi
    sleep 2
  done

  if [ -z "$run_id" ]; then
    echo "Could not find the release workflow run for ${tag}" >&2
    exit 1
  fi

  gh run watch "$run_id" -R "$github_repo" --exit-status
}

if [ "$mode" = "prepare" ]; then
  if [ -n "$(git status --porcelain)" ]; then
    echo "Working tree must be clean before prepare" >&2
    exit 1
  fi
  "$repo_root/scripts/validate-publish-ready.sh"
  preflight_publish_readiness
  echo "Prepare mode complete for ${tag}. No tag, push, publish, or release dispatch was performed."
  exit 0
fi

current_branch="$(git branch --show-current)"
if [ "$current_branch" != "main" ]; then
  echo "Release must run from main; current branch is ${current_branch}" >&2
  exit 1
fi

git fetch origin main --tags

local_head="$(git rev-parse HEAD)"
remote_head="$(git rev-parse origin/main)"
if [ "$local_head" != "$remote_head" ]; then
  echo "Local main is not aligned with origin/main" >&2
  exit 1
fi

if [ -n "$(git status --porcelain)" ]; then
  echo "Working tree must be clean before release" >&2
  exit 1
fi

preflight_publish_readiness

if git ls-remote --exit-code --tags origin "refs/tags/${tag}" >/dev/null 2>&1; then
  tag_commit="$(git rev-list -n 1 "$tag")"
  if [ "$tag_commit" != "$remote_head" ]; then
    echo "Tag ${tag} does not point to origin/main" >&2
    exit 1
  fi
  publish_workspace
  if gh release view "$tag" -R "$github_repo" >/dev/null 2>&1; then
    echo "Release ${tag} is already complete on GitHub."
    exit 0
  fi
  trigger_github_release "$remote_head"
  exit 0
fi

if git rev-parse -q --verify "refs/tags/${tag}" >/dev/null; then
  echo "Tag ${tag} exists locally but not on origin; push it or delete it first." >&2
  exit 1
fi

git tag -s "$tag" -m "Code System Graph ${tag}"
if ! git push origin "refs/tags/${tag}:refs/tags/${tag}"; then
  git tag -d "$tag" >/dev/null 2>&1 || true
  echo "Failed to push ${tag}; removed the local tag and did not publish crates." >&2
  exit 1
fi

publish_workspace
trigger_github_release "$remote_head"

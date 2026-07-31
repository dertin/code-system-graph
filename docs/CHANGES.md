# Change and Pull-Request Analysis

Code System Graph treats a change as an immutable, bounded input to federated analysis. Local Git is the
default provider. GitHub and Bitbucket Cloud are optional remote providers and require two explicit
gates: provider enablement and consent on every request.

## Local Git scopes

The local provider supports:

- unstaged tracked worktree changes;
- staged index changes;
- all staged, tracked worktree, and untracked paths as distinct layers;
- merge-base comparison against one validated ref;
- one full commit object ID; and
- an explicit base-to-head range.

Refs reject option injection and revision expressions outside the selected scope. Git output,
execution time, and stderr are bounded. Cancellation terminates the child process. Code System Graph reads
status, raw path records, numstat, and zero-context diff positions but does not retain changed
source text. Native path bytes remain lossless where the operating system permits.

Every change set fingerprints the repository and checkout identities, canonical worktree and
common Git directory, checked-out commit, exact staged and worktree state, workspace manifest,
contract registry, and analyzer versions. A result becomes stale if any of these inputs changes.
Stale analysis must be recomputed and cannot authorize a commit.

Semantic analysis matches old and new native paths to persisted evidence and intersects
zero-context hunk positions with evidence line ranges. Evidence-backed graph edges identify
artifacts, symbols, contracts, services, repositories, and communities before bounded impact
propagation. Renames and deletions inspect both old and new neighborhoods. Binary, untracked,
non-representable, file-only, truncated, or unmatched changes remain partial or unknown; they
never produce a false-safe no-impact conclusion. Compatibility deltas require explicit
before/after fingerprints, otherwise their status is unknown.

Commit gates model default, all-tracked, only, and include selection without running `git add`,
`git commit`, hooks, or tests. Ambiguous path/layer selections fail closed.

## GitHub and Bitbucket Cloud

Remote pull-request access is disabled by default. CLI use requires both `--enabled` and
`--consent`. MCP requires the server to start with `--enable-github-pull-requests` or
`--enable-bitbucket-pull-requests`, and each `analyze_pull_request` call must still set
`consent_to_remote_access`.

The CLI accepts an optional token through the environment variable named by `--token-env`.
Bitbucket user API tokens additionally use the Atlassian account email from `--user-env` and HTTP
Basic authentication. OAuth and repository/project/workspace access tokens omit `--user-env` and
use Bearer authentication. MCP reads `BITBUCKET_USER` when present and otherwise treats
`BITBUCKET_TOKEN` as Bearer; GitHub uses `GITHUB_TOKEN`. Credentials are ephemeral, redacted from
diagnostics, excluded from schemas, and never persisted.

The public API endpoints are fixed and allowlisted. HTTPS is mandatory, automatic redirects are
disabled, pagination cannot cross origins, and response bytes, pages, items, and time are bounded.
Rate limits are returned to the caller; Code System Graph does not sleep or retry indefinitely.

Provider responses normalize metadata, refs, changed-file counts, CI/check state, reviews,
approvals, warnings, and rate-limit metadata. Patch bodies and raw responses are discarded.
Structured ETag/TTL caching retains only this source-free normalized result. Bitbucket Data Center
is intentionally not implemented in 1.0.0.

`csgraph pr list` returns one provider page of source-free summaries for GitHub or Bitbucket
Cloud. It uses the same enablement, per-call consent, credential, HTTPS, rate-limit, cancellation,
and pagination-origin policies as inspection. Cursors are opaque positive page identifiers and
page limits cannot exceed 100. `csgraph pr show` inspects one PR, while `csgraph pr overlap`
compares two local source-free semantic input documents without network access.

## Semantic overlap and ordering

PR overlap is deterministic and evidence-backed. Shared files, services, communities, contracts,
migrations, and dependency directions can classify two changes as disjoint, related, overlapping,
or conflicting. Suggested order uses dependency direction and readiness; stale inputs,
dependency cycles, unknown compatibility, incomplete mapping, or missing CI/review state produce
an indefinite recommendation instead of a false-safe order.

## Deliberate limits

Code System Graph does not fetch source files, retain patches, execute tests, mutate Git state, submit
reviews, merge pull requests, or bypass branch protection. Dynamic generated changes and provider
metadata beyond the bounded public contracts remain unknown. File-only remote summaries have less
semantic precision than local zero-context line evidence and are reported as incomplete.

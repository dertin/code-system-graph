# Federated Data Model

Code System Graph stores normalized repository-boundary facts and their evidence. The initial 1.0 database
schema is installed as one migration and retains migration support for future releases.

## Identity

IDs are versioned, namespaced BLAKE3 fingerprints of canonical inputs:

- workspace: workspace name plus canonical manifest-directory path;
- repository: credential-free normalized remote, falling back to the Git common directory or
  canonical checkout path;
- checkout: repository ID plus canonical worktree path;
- graph node, edge, and evidence: typed canonical keys.

Linked worktrees share a repository ID and retain distinct checkout IDs. Aliases are
workspace-local labels, not identity. Line numbers are mutable locators and never primary
identity.

Native paths are stored as a platform encoding plus lossless bytes. Diagnostic display paths may
be lossy and are not used for equality. Unix paths preserve raw `OsStr` bytes; Windows paths
preserve little-endian UTF-16 code units.

## Workspace registry

`workspaces` owns the manifest path and fingerprint. `repositories` stores identity-level metadata
such as a normalized remote and Git common directory. `repository_checkouts` stores canonical
native paths, HEAD, linked-worktree state, and observed working-tree state.
`workspace_repositories` maps workspace aliases to repository and checkout IDs.

Repository paths are canonicalized before registration. The manifest directory is an implicit
allowed root; paths outside it require an explicit `allowedRoots` entry. Canonical checkout and
Git common-directory paths must remain beneath an allowed root.

Workspace removal cascades snapshots and alias mappings, then garbage-collects repositories and
checkouts no longer referenced by another workspace.

Manifest updates are constrained to the top-level `repos` mapping. Preview validates the complete
resulting manifest and registry without writing. Commit verifies the original content fingerprint,
creates a non-overwriting backup, and atomically replaces the canonical manifest while preserving
comments, ordering, line endings, and unrelated bytes.

## Snapshots, graph, and evidence

`repo_snapshots` is published atomically per workspace. Nodes, edges, evidence, edge-evidence links,
community data, manual-link records, and per-checkout freshness are written in the publication
transaction. If any constraint or write fails, the previous current snapshot remains visible.

Nodes and edges are snapshot-scoped. Foreign keys reject dangling graph edges and missing evidence.
FTS5 indexes bounded labels and stable keys without source bodies. Search is scoped to the current
workspace snapshot and accepts quoted, bounded input rather than raw FTS syntax.

Evidence records retain source ownership, hashes, line locators, extraction metadata, and bounded
notes. Source bodies, diff bodies, credentials, configuration values, connection strings, and
external-provider responses are not graph evidence.

Integrity checks combine SQLite `quick_check`, exact schema-version validation, and
`foreign_key_check`.

## Extraction artifacts

`artifact_fingerprints` stores bounded BLAKE3 content hashes keyed by snapshot, repository,
checkout, lossless relative path, and extractor. Scan planning compares current and previous keys:

- current only: added;
- previous only: deleted;
- same identity with a different content hash: modified;
- same identity and hash: unchanged.

A rename is represented as delete plus add unless an extractor can establish semantic continuity.
`extractor_runs` records versioned counts and status, while `extractor_run_inputs` binds each run to
exact content hashes. If merged configuration and all fingerprints are unchanged, scanning reuses
the published snapshot without running extractors or writing SQLite.

Repository-local `.code-system-graph.yaml` configuration participates in the workspace fingerprint, so a
configuration change invalidates freshness even when contract artifacts are byte-identical.

Versioned output batches are keyed by snapshot, repository, checkout, lossless source path, and
extractor. Payloads contain structured observations only, and output counts are checked while
decoding. Compatible unchanged batches are reused; add, replace, and delete actions identify the
link neighborhoods to recompute.

## Federated entities

The graph represents package, HTTP, event, GraphQL, RPC, data, infrastructure, documentation,
ownership, configuration, test, and implementation-anchor entities.

- Event identities include broker, namespace, and channel.
- GraphQL identities include repository, role, root type, field, and operation.
- RPC identities include repository, role, package-qualified service, and method.
- Data identities use database, schema, table, and column coordinates.
- Deployment identities include technology, namespace, and name.
- Service identities include repository and name.
- Document and configuration identities include repository and source or scope.
- Owner identities use normalized global names.

Source locations remain evidence locators rather than identity components. Compatibility
fingerprints are derived from structured contracts and are not stored as source text.

Secret-safe extraction records configuration key names, scopes, line locators, and sensitivity
classifications only. Infrastructure environment and secret observations follow the same rule.

## Cross-language relationships

Languages do not create graph edges by themselves. Tests and implementations connect through
shared contracts:

```text
test_case --validates--> http_operation --implemented_by--> symbol_ref
```

`test_case` identities include repository, language, framework, source path, and test name.
`symbol_ref` identities include repository, language, source path, and declared symbol.

A `validates` edge requires test evidence and provider-contract evidence. An `implemented_by` edge
requires provider-contract evidence plus implementation declaration or source evidence. Missing
providers remain unlinked, and duplicate providers remain ambiguous. Optional CodeGraph
corroboration can strengthen an existing implementation edge after exact path, name, and line
agreement; it does not create unsupported federated relationships.

## Communities and derived views

Each graph snapshot may own one immutable `CommunitySnapshot` plus normalized communities and
memberships. It records engine version, algorithm, seed, scope, resolution, confidence threshold,
relationship weights, and iteration bound. Community rows retain deterministic labels, central
nodes, metrics, contract boundaries, label evidence, and limitations.

Community identities derive from sorted memberships and remain stable only while membership is
stable. Snapshot deltas relate old and new identities through deterministic Jaccard overlap without
mutating graph nodes or edges. Search rank, centrality, and community membership are derived views,
not provenance.

Impact reports, risk scores, compatibility comparisons, local-provider summaries, test
recommendations, exports, diagnostics, and host-routing responses are also derived views. Reports
identify their model version and immutable graph inputs. Stale or incomplete freshness removes
numeric risk rather than implying safety.

Change sets retain repository and checkout identity, native worktree and Git common-directory
paths, HEAD/base/head identities, staged/worktree/exact-diff hashes, manifest and contract-registry
hashes, analyzer versions, file layers, statuses, and line positions. They exclude source and diff
bodies. Any identity, Git state, manifest, registry, or analyzer mismatch invalidates the report.

Remote pull-request inspections are source-free derived inputs. Their bounded cache stores
normalized metadata, changed-file counts, CI and review state, ETag, expiration, rate-limit
metadata, and one deterministic fingerprint. Tokens, patches, and raw responses are excluded.

## Manual links and bounded runtime data

`manual_links` stores a declaration ID, exact resolved source and target node IDs, edge kind,
addition or suppression disposition, required reason, versioned link decision, manifest
configuration version, and owning immutable snapshot.

Each manifest `manualLinks` endpoint must exactly match a node ID or stable key in the candidate
snapshot and resolve to one node. No match is unresolved; multiple matches are ambiguous. An
optional `contract` value supplies audit context and does not change endpoint matching.

An addition replaces an automatic edge with the same source, target, and edge kind and creates one
confirmed edge backed only by manual provenance. A suppression removes automatic edges matching
that exact triple and fails when none exists. Extractor observations and unrelated edges remain
intact. Updating or removing a declaration changes current state while preserving prior immutable
snapshot history.

`provider_capabilities` stores normalized capability names for one workspace, repository, provider
name, and provider version, plus the observation timestamp. It does not store provider responses.

`query_cache` stores bounded JSON result summaries keyed by workspace, immutable snapshot, and
normalized input fingerprint. Each summary is capped at 1 MiB, and each workspace retains at most
1,024 entries.

Administrative MCP mutations append a separate source-free JSONL audit record containing version,
timestamp, workspace, operation, and resulting snapshot ID. Audit records are not graph evidence.

## Freshness

Each published snapshot records checkout ID, repository ID, HEAD, manifest hash, state, and a
bounded reason. Current status compares those inputs with the live registry:

- dirty checkout: `working_tree_changed`;
- manifest fingerprint drift: `config_changed`;
- HEAD drift: `commits_behind`;
- missing checkout: `unavailable`;
- no prior observation: `unknown`.

Unknown, unavailable, corrupt, or partial inputs never produce a fresh or safe conclusion.

## Migrations, locking, and recovery

Migrations are ordered, embedded, and recorded in `schema_metadata`. Databases newer than the
binary are rejected. Before a forward migration, an existing database receives an automatic,
non-overwriting versioned backup through SQLite's backup API.

Restore validates integrity and schema compatibility, preserves the destination as a separate
safety backup, restores through the backup API, and then applies supported forward migrations.

A restrictive sidecar lock enforces one writer while independent read-only WAL connections remain
available during publication. Lock metadata records a format version, PID, process start time, and
random owner token. PID and process start time prevent PID reuse from preserving an orphaned lock;
abrupt process termination is recoverable after the configured threshold.

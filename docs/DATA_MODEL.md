# Federated Data Model

Code System Graph stores normalized repository-boundary facts and their evidence. Each binary
embeds one exact database schema; a database is accepted only when it matches that schema.

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

Workspace removal cascades the current graph and alias mappings, then garbage-collects repositories
and checkouts no longer referenced by another workspace.

Manifest updates are constrained to the top-level `repos` mapping. Preview validates the complete
resulting manifest and registry without writing. Commit verifies the original content fingerprint,
creates a non-overwriting backup, and atomically replaces the canonical manifest while preserving
comments, ordering, line endings, and unrelated bytes.

## Snapshots, graph, and evidence

Each workspace owns exactly one current graph. `repo_snapshots` holds one row per workspace whose
snapshot ID identifies the published inputs. Publication compares the candidate with the stored
graph by identifier and content digest, then inserts, updates, and deletes only the differing nodes,
edges, evidence, edge-evidence links, fingerprints, and extractor batches in one transaction.
Manual-link records, extractor runs, and per-checkout freshness are rewritten in the same
transaction. If any constraint or write fails, the previous graph remains visible. An unchanged
republication writes no graph rows, so database size stays proportional to the current graph.

Graph tables are keyed by workspace. Foreign keys reject dangling graph edges and missing evidence.
FTS5 indexes bounded labels and stable keys without source bodies and is maintained by triggers on
node rows. Search is scoped to the workspace and accepts quoted, bounded input rather than raw FTS
syntax. Reads addressed by snapshot ID resolve that ID to its workspace and fail when the snapshot is
no longer current.

Evidence records retain source ownership, hashes, line locators, extraction metadata, and bounded
notes. Source bodies, diff bodies, credentials, configuration values, connection strings, and
external-provider responses are not graph evidence.

Integrity checks combine SQLite `quick_check`, exact schema-identity validation, and
`foreign_key_check`.

## Extraction artifacts

`artifact_fingerprints` stores bounded BLAKE3 content hashes keyed by workspace, repository,
checkout, lossless relative path, and extractor. Scan planning compares current and previous keys:

- current only: added;
- previous only: deleted;
- same identity with a different content hash: modified;
- same identity and hash: unchanged.

A rename is represented as delete plus add unless an extractor can establish semantic continuity.
`extractor_runs` records versioned counts and status per repository, checkout, and extractor; the
fingerprints of the same scope are the exact run inputs. If merged configuration and all
fingerprints are unchanged, scanning reuses the published snapshot without running extractors or
writing SQLite.

Repository-local `.code-system-graph.yaml` configuration participates in the workspace fingerprint, so a
configuration change invalidates freshness even when contract artifacts are byte-identical.

Versioned output batches are keyed by workspace, repository, checkout, lossless source path, and
extractor. Payloads contain structured observations only, and output counts are checked while
decoding. Each batch stores a BLAKE3 payload hash, so publication never decodes unchanged payloads. Compatible unchanged batches are reused, and relationships are relinked from all current
batches on every scan.

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
`symbol_ref` identities include repository, language, source path, and declared symbol. A test
callback without a function name, such as a Jest `it` block, declares the symbol formed by its
`describe` chain and title joined with ` > `.

A `validates` edge requires test evidence and provider-contract evidence. An `implemented_by` edge
requires provider-contract evidence plus implementation declaration or source evidence.

HTTP operations are identified by method and canonical route shape. Parameter syntax and names are
not part of the identity: `{id}`, `:id`, `<int:id>`, `{id:int}`, `{id:[0-9]+}`, and `[id]` are one
parameter segment, and `{*rest}`, `{path...}`, `<path:rest>`, `[...slug]`, and `*rest` are one
catch-all segment. `calls_remote`, `validates`, and `implemented_by` resolve through one per-method
route index:

- provider routes carry their full path: extractors record the router each route is registered on
  and every router mount with its prefix, and mounts are composed per repository across files
  before linking;
- consumer calls are composed per repository before linking: a call whose URL depends on a
  parameter of its function is instantiated at each call site that binds it, and a test owns the
  exact calls of the functions and fixtures it reaches, with evidence at the call site in the test;
  a call made through a parameter is kept only when the parameter resolves to an in-process test
  client through a fixture or every caller;
- a concrete path such as `/orders/42` matches the `/orders/{id}` template;
- the most specific template wins, comparing segments left to right (static, then parameter, then
  catch-all);
- equally specific providers are narrowed by scope: an explicit repository restriction (an
  authority declared in the manifest or inferred from Compose and Kubernetes services, or an
  in-process test client), then the caller's own repository, then a unique provider in the
  workspace;
- loopback hosts resolve across the workspace, and calls to other unknown hosts are external and
  never linked;
- remaining ties are reported as ambiguities with their candidates, and implementation anchors
  resolve only inside their own repository.

Missing providers remain unlinked. Every scan publishes an HTTP link report with the workspace
graph: the number of consumer and test calls (counted per calling node) that were linked, that have
no provider, that are ambiguous, and that target an external host, plus one gap row per unlinked
call with its method, path, reason, and, for ambiguities, the candidate providers. The report is
replaced with the graph and read by `status`, `query`, and the MCP coverage resource.

Optional CodeGraph
corroboration can strengthen an existing implementation edge after exact path, name, and line
agreement; it does not create unsupported federated relationships.

## Communities and derived views

Each graph snapshot may own one `CommunitySnapshot` plus normalized communities and memberships.
The analyses of the current and the previous snapshot are retained so deltas can be compared; older
analyses are deleted on publication. Communities are recomputed only when graph topology changes;
otherwise the previous analysis is carried forward under the new snapshot ID. It records engine version, algorithm, seed, scope, resolution, confidence threshold,
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
addition or suppression disposition, required reason, versioned link decision, and manifest
configuration version.

Each manifest `manualLinks` endpoint must exactly match a node ID or stable key in the candidate
snapshot and resolve to one node. No match is unresolved; multiple matches are ambiguous. An
optional `contract` value supplies audit context and does not change endpoint matching.

An addition replaces an automatic edge with the same source, target, and edge kind and creates one
confirmed edge backed only by manual provenance. A suppression removes automatic edges matching
that exact triple and fails when none exists. Extractor observations and unrelated edges remain
intact. Updating or removing a declaration changes the current graph on the next publication.

`provider_capabilities` stores normalized capability names for one workspace, repository, provider
name, and provider version, plus the observation timestamp. It does not store provider responses.

`query_cache` stores bounded JSON result summaries keyed by workspace, current snapshot, and
normalized input fingerprint. Publication clears the workspace cache. Each summary is capped at 1 MiB, and each workspace retains at most
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

## Schema, locking, and recovery

`schema_metadata` records the schema identity, a BLAKE3 digest of the embedded schema definition,
and an opaque database instance identity. The complete SQLite schema and the recorded identity must
match the binary exactly; any other database is rejected with an instruction to remove the
disposable database and run a full scan. The owner-only `<database>.work.db` sidecar holds
operational state only: checkpointed extractor batches (metadata plus the raw payload BLOB, evicted
least-recently-used under `maxCheckpointCacheBytes`), the per-file stat cache used to skip reading
unchanged files, and watcher leases. It is bound to the database instance identity, so a sidecar
from another or rebuilt database is discarded rather than reused.

Restore validates integrity and exact schema compatibility, preserves the destination as a
separate safety backup, and restores through SQLite's backup API without migration.

A restrictive sidecar lock enforces one writer while independent read-only WAL connections remain
available during publication. Lock metadata records a format version, PID, process start time, and
random owner token. PID and process start time prevent PID reuse from preserving an orphaned lock;
abrupt process termination is recoverable after the configured threshold.

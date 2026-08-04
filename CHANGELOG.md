# Changelog

All notable public changes to Code System Graph are documented in this file. Code System Graph follows Semantic
Versioning.

## [1.0.1] - 2026-08-04

### Fixed

- Fixed the portable capability-directory reader so Windows builds preserve the diagnostic path
  without moving it before the bounded read.
- Canonicalized work-sidecar parent directories before SQLite opens them, preserving final-file
  `NOFOLLOW` protection while supporting the standard symlinked `/var` path on macOS.
- Pinned the CLI and its tests to the bundled SQLite implementation so macOS and Windows use the
  same validated database engine as the persistence crate.
- Raised the `csgraph` executable stack on Windows to match the extraction workload without
  changing process memory or execution-policy limits.
- Serialized native Windows and macOS test execution to stay within platform file-descriptor and
  filesystem concurrency limits while retaining the complete test suite.

### Release engineering

- Added full native Windows x86_64 and macOS x86_64/ARM64 build and test gates to pull-request CI.
- Added archive smoke tests for Unix and Windows release assets before publication.
- Made release validation and installation smoke tests derive the workspace version instead of
  embedding `1.0.0`.

## [1.0.0] - 2026-08-04

First public release of Code System Graph.

### CodeGraph delivery

- Added the read-only `explore` MCP tool and HTTP endpoint for bounded, ephemeral
  repository-local source and flow context. The tool is advertised only when CodeGraph is
  explicitly enabled through `--codegraph` or `CODE_SYSTEM_GRAPH_CODEGRAPH=1`.
- Added server-configured automatic CodeGraph enrichment to MCP and HTTP impact requests.
- Added server-configured CodeGraph corroboration to administrative scans without tool-call
  controls.
- Added action-discriminated MCP contracts, communities, workspace updates, and manual-link
  mutations; impact options support partial overrides.
- Added host routing guidance that prefers Code System Graph for both federated and local
  exploration. Hook installation accepts `--codegraph` so static guidance matches the MCP profile.

### Federated graph

- Added strict workspace manifests, canonical repository and checkout identities, root allowlists,
  linked worktree support, and lossless native path persistence.
- Added per-repository `excludes` and `includeDefaults` glob policies shared by native scanning,
  OpenAPI autodetection, and watched synchronization. Protected metadata directories remain
  excluded, default dependency and build directories can be selectively reopened, explicit
  artifacts are retained, configured patterns are canonicalized under a strict portable glob
  grammar, and policy changes invalidate incremental scan fingerprints.
- Added atomic SQLite snapshots, online backup and restore, exact-schema validation with
  fail-closed rejection of incompatible databases, integrity checks, writer locks, historical
  snapshots, and per-checkout freshness.
- Added deterministic evidence-backed linking with explicit ambiguity, coverage, provenance, and
  freshness reporting.
- Added versioned manual relationship additions and exact suppressions with required reasons,
  validation, provenance, and snapshot history.

### Contracts and extraction

- Added OpenAPI 3.x, Swagger 2.0, generated-client metadata, and focused HTTP client, route,
  implementation, and test extraction for JavaScript/TypeScript, Python, Go, Java, and Rust.
- Added AsyncAPI 2.x/3.x and focused publisher/subscriber extraction for Kafka, RabbitMQ, SNS/SQS,
  NATS, Google Pub/Sub, and generic event APIs.
- Added GraphQL schema, operation, fragment, persisted-operation, resolver, and federation
  extraction with exact consumer, provider, and resolver linking.
- Added protobuf package, message, enum, service, RPC, import, streaming, and generated gRPC
  client/server extraction.
- Added package relationship extraction for npm, pnpm, Yarn, Python, Poetry, Cargo, Go, Maven,
  Gradle, NuGet, and MSBuild.
- Added exact Cargo workspace-manifest containment so multi-crate repositories connect their root
  workspace declaration to every literal member manifest and package graph.
- Added SQL and migration, Prisma, Alembic, SQLAlchemy, Diesel, and literal-query extraction with
  exact reader and writer links to unambiguous tables.
- Added complete migration artifact retention plus explicit reversible-pair and numeric-order
  lineage for migration-only repositories.
- Added native SQLx extraction for runtime queries, checked and unchecked macros, `raw_sql`,
  `QueryBuilder`, external query files, and embedded/runtime migrations, including Cargo-root path
  resolution, `sqlx.toml` migration-directory overrides, and incremental relinking.
- Added native `mysql_async` extraction for `Queryable`, prepared `exec*`, streaming, and fluent
  query APIs, with exact cross-language and cross-repository table relationships.
- Added Python aiohttp and static HTTP-registry discovery, PyMySQL/wrapper SQL relationships,
  bounded MariaDB dump recovery, and Factory Boy factory-to-model links.
- Added advisory Utoipa operation extraction that coexists with higher-authority Actix/Axum
  executable routes, plus FastAPI router-prefix composition, Next.js App Router routes,
  psycopg/psycopg2 value-formatted SQL, SQLAlchemy ORM read/write links, and recursive discovery of
  multiple distinct OpenAPI contracts per repository.
- Added Docker Compose, Kubernetes, Helm, Terraform, and OpenTofu deployment, service, dependency,
  resource, and configuration extraction.
- Added Markdown, ADR, CODEOWNERS, and service-catalog extraction with documentation and ownership
  links.
- Added secret-safe dotenv, YAML, JSON, and TOML configuration extraction that stores key names
  and sensitivity classifications but never values.
- Added evidence-backed cross-language test paths through shared contracts to implementation
  anchors.

### Query, traversal, and communities

- Added deterministic ranked search across exact and normalized identities, full text, scope,
  centrality, community, evidence quality, and freshness with visible score components.
- Added bounded directed BFS, confidence-weighted Dijkstra, and loopless k-shortest traversal with
  frontier, unconfirmed-relationship, coverage, timeout, and truncation reporting.
- Added deterministic connected components, weighted clustering, and seeded Louvain communities
  with metrics, centrality, structural labels, historical comparison, and material deltas.
- Added bounded cross-repository HTTP, event, GraphQL, and gRPC traces.

### Impact and change analysis

- Added conservative upstream, downstream, and bidirectional impact analysis with direct,
  transitive, possible, and coverage-unknown classifications.
- Added evidence-backed risk reports and repository, service, contract, owner, community,
  environment, and test impact summaries.
- Added compatibility findings for HTTP/OpenAPI, events, GraphQL, protobuf/gRPC, packages, and
  databases with fingerprints and recommended validations.
- Added bounded local Git analysis for staged, unstaged, worktree, comparison, revision, and range
  scopes with source-free positions and exact input fingerprints.
- Added semantic change mapping from evidence to graph entities, compatibility deltas, and
  federated impact.
- Added opt-in GitHub and Bitbucket Cloud pull-request inspection with explicit consent, ephemeral
  credentials, bounded HTTPS transport, normalized CI and review state, and source-free caching.
- Added deterministic pull-request overlap, conflict classification, cycle detection, and
  dependency-aware review ordering.

### Interfaces and integrations

- Added a non-interactive CLI for workspace and repository lifecycle, scan, status, backup,
  restore, migration, query, traversal, communities, impact, changes, pull requests, contracts,
  export, diagnostics, cleanup, HTTP serving, hooks, and shell completions.
- Added deterministic JSON results, JSON/GraphML/Markdown export, structured stderr diagnostics,
  correlation IDs, and stable failure classes.
- Added `csgraph config show` with optional repository filtering and a versioned, deterministic
  JSON report of protected, default, configured, and effective exclusion rules without requiring
  a database or modifying the workspace.
- Added a read-only-by-default MCP stdio server for status, contracts, source context, trace,
  query, communities, impact, changes, and consented pull-request inspection.
- Added bounded, source-free MCP resources and a generated public schema catalog.
- Added an explicitly enabled MCP administrative profile for scans, community recomputation,
  workspace updates, manual relationship writes, and query-cache cleanup with validation, writer
  locking, and source-free audit records.
- Added an optional bounded HTTP server with loopback defaults, authenticated non-loopback binds,
  constant-time bearer checks, security headers, and request, concurrency, rate, and timeout
  limits.
- Added optional host integration for Claude Code, Codex, Gemini, Antigravity, and Cursor with
  install, status, uninstall, advisory routing, and strict staged-change gating.
- Added idempotent `.gitignore` setup for workspace initialization and host integration when their
  roots belong to Git worktrees, without creating unnecessary files in unversioned containers.
- Added optional CodeGraph integration through public MCP and CLI capabilities for symbol
  resolution, local context, neighbors, impact, and affected tests with bounded requests,
  cancellation, and conservative degradation.
- Added `csgraph sync` for explicit one-shot or watched incremental publication. It refreshes each
  initialized repository-local CodeGraph index through the public CLI, uses native filesystem
  notifications across supported platforms with bounded debouncing, and provides polling for WSL,
  network filesystems, and native-watcher setup failures.

### Safety and distribution

- Added validation for unsafe control characters, bidirectional metadata, duplicate graph
  identities, invalid references, malformed numeric evidence, and oversized metadata.
- Added GraphQL extraction payload strictness: legacy `default_value` fields are rejected and
  structural default categories use `default_value_kind` only.
- Added backup-restore validation for missing or altered schema objects, foreign-key integrity,
  and pinned read-only backup sources.
- Added host hook filesystem boundaries that reject FIFO reads without blocking, resist parent
  symlink replacement after root open, and tolerate control-character and bidirectional path
  components at the filesystem edge.
- Added source-free diagnostic bundles, private Unix file permissions, non-overwriting output, and
  conservative health reporting.
- Added cancellation, deadlines, output limits, process concurrency limits, and deterministic
  child-process cleanup for external operations.
- Added self-contained release archives with `csgraph` and `code-system-graph-hooks`, documentation,
  notices, install/uninstall scripts, CycloneDX SBOMs, and SHA-256 checksums.
- Added crates.io publication for the workspace crates and cargo-binstall metadata aligned with
  GitHub Release archives for Linux x86_64.

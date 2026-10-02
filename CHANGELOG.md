# Changelog

All notable public changes to Code System Graph are documented in this file. Code System Graph follows Semantic
Versioning.

## [1.2.0] - Unreleased

### Changed

- The SQLite store keeps one current graph per workspace and publishes each scan as a delta:
  only inserted, changed, and removed nodes, edges, evidence, fingerprints, and extractor batches
  are written. An unchanged republication writes no graph rows, and the database no longer grows
  with the number of scans.
- The database records a schema identity derived from the embedded schema definition. `status`,
  `doctor`, restore, and MCP status report `schema_id`.
- Communities are recomputed only when graph topology changes; the analyses of the current and the
  previous snapshot are retained for comparison.
- FTS5 search rows are maintained by triggers keyed by node row.
- Every store read runs in one SQLite read transaction, so readers never observe a snapshot that a
  concurrent publication replaced between statements.
- Scans fingerprint each physical file once and reuse its content hash from a per-file stat cache
  (size, modification time, and, on Unix, device, inode, and change time) when unchanged; files
  modified within two seconds of the cached observation are always read.
- Changed files are read once and shared by all of their extractors. Extraction runs on up to
  `executionPolicy.maxExtractionWorkers` threads (default 8, bounded by available parallelism) and
  merges results in artifact-key order, so the published graph is identical for every worker count.
- Event and literal-SQL source extractors skip files that contain none of their recognizer
  markers, and Rust files are parsed for database calls only when they reference `sqlx` or
  `mysql_async`.
- Checkpointed batches are written in one sidecar transaction and stored as raw payload BLOBs;
  only artifacts that differ from the published graph are looked up in the checkpoint cache.
- Targeted `--repository` scans discover and fingerprint only the selected repository.
- The scan no longer stages a complete copy of the candidate graph in the operational sidecar
  before publication.
- `ExecutionSummary` reports `statCacheHits`, `extractionWorkers`, `contentBytesRead`,
  `publishedRows`, and per-phase wall time and resident memory in `phases`.
- Source files whose GraphQL, event, generated-protobuf, or literal-SQL scan finds no facts no
  longer add a per-file artifact node and `contains` edge; their batches are persisted without
  outputs, and reusing a batch without outputs decodes nothing. Every registered repository has a
  repository node.
- A scan whose snapshot identity (a hash of the manifest, the extraction contract, the budgets, and
  every artifact fingerprint) equals the current snapshot reuses it without loading the previous
  fingerprints or any extractor batch. The snapshot identifier now covers artifact paths and sizes
  in addition to content hashes.
- Incremental scans publish fingerprints and extractor batches as a delta planned against the
  stored fingerprints; `--force` scans compare every stored artifact row. The incremental plan
  lists only added, modified, and deleted artifacts, and `ArtifactChangeKind::Unchanged`, the
  unused extractor-batch planner (`plan_extractor_batches`, `ExtractorBatchPlan`, `PlannedBatch`,
  `BatchAction`), and `ArtifactKey` are removed from the public API.
- Community detection evaluates each Louvain move from incremental modularity gains and community
  degree totals instead of recomputing modularity for every candidate.
- The scan worker reports progress at most every 50 ms. The supervisor's memory sampler reads only
  process memory and parent links instead of listing every thread and reading CPU, disk usage, and
  executable paths of every process, which cuts each sample by about four times on a desktop host
  and keeps the no-progress watchdog on schedule under CPU contention.
- CodeGraph corroboration, extraction planning, and extractor-run accounting index artifacts and
  graph elements by borrowed keys in `foldhash` maps instead of scanning or cloning them per
  artifact, and reused extractor batches are moved rather than copied.
- Workspace registration inspects repositories in parallel and caches each `origin` remote by the
  modification time and size of its Git config. `sync` resolves the workspace once per pass and runs
  CodeGraph status and sync for up to four repositories concurrently.
- HTTP operations are identified by method and canonical route shape, so `{id}`, `:id`,
  `<int:id>`, `{id:int}`, `[id]`, and catch-all forms such as `{*rest}` or `[...slug]` declare the
  same operation. One per-method route index resolves `calls_remote`, `validates`, and
  `implemented_by`: concrete paths such as `/orders/42` match templates, the most specific template
  wins, and equally specific providers are narrowed to an explicit repository restriction or the
  caller's repository before being reported as ambiguous with their candidates.
- Relationships are relinked from all current batches on every scan, so incremental and full scans
  publish identical graphs.
- `sync --watch` passes after the initial one discover and synchronize only the repositories touched
  by the coalesced filesystem events; manifest edits, ignore-rule changes, event overflow, and
  watcher errors widen the pass to the whole workspace.
- Absolute consumer URLs are linked by path and scoped by their authority. Loopback hosts resolve
  across the workspace, the new per-repository `authorities` manifest field restricts a host to one
  repository, Compose and Kubernetes service names are inferred as authorities of the declaring
  repository, and any other host is classified external instead of being reported as a call
  without a provider.
- TypeScript, JavaScript, Go, and Java source extraction no longer treats `//` inside string
  literals as a comment, so absolute URLs are recognized.
- Server routes are published with their full path. Router prefixes are composed per repository,
  across files, for FastAPI `include_router`, Flask blueprints, Express `use`, NestJS controllers
  and global prefixes, Spring and Feign class-level mappings, Gin and Chi groups and mounts, Axum
  `nest`/`merge`, and Actix Web `scope`/`service`/`configure`. Go 1.22 `net/http` method patterns
  such as `"GET /orders/{id}"` are recognized.
- Client URLs are evaluated instead of requiring one string literal: constants and variables bound
  earlier in the same function or file, concatenation, Python f-strings, `%` and `.format`, Rust
  `format!` and `concat!`, JavaScript template literals, Go `fmt.Sprintf`, Java `String.format`,
  and conversions such as `encodeURIComponent` or `strconv.Itoa`. Runtime values in the scheme,
  the authority, or a whole path segment keep the path exact, with the segment as a parameter.
- Client calls are composed per repository through wrapper functions: a call whose URL depends on a
  parameter of its function is instantiated at every call site that binds it, also through
  wrappers of wrappers and across modules. Tests are linked to the endpoints reached through the
  helpers they call and the pytest fixtures they request, including fixtures in `conftest.py`.
- TypeScript and JavaScript clients are recognized per call instead of per statement, so every
  `fetch` in a callback is reported; Axios instances created with `axios.create({ baseURL })`,
  Axios config-object requests, Go `http.Client` receivers, `http.Head`, `http.PostForm`, and
  `http.NewRequestWithContext` are recognized, and client calls carry their enclosing function.
- Tests are recognized in every supported language: Jest, Vitest, Mocha, and Playwright
  `describe`/`it`/`test` blocks, identified by file, `describe` chain, and title and linked to the
  `beforeEach`/`beforeAll` blocks that run before them; Go `TestX(t *testing.T)` functions; and
  JUnit `@Test`, `@ParameterizedTest`, and `@RepeatedTest` methods.
- In-process test clients are recognized and resolve only against providers in their own
  repository: Python `TestClient`, Flask `test_client()`, and HTTPX with `app=` or an ASGI/WSGI
  transport, including pytest fixtures that return them; supertest and Playwright `request`; Go
  `httptest.NewRequest` and `httptest.NewServer` URLs; Spring MockMvc, RestAssured,
  `WebTestClient`, `RestTemplate`, and `TestRestTemplate`; Axum `Request` builders sent with
  `oneshot`; and Actix `test::TestRequest`.
- Helm templates are recognized only as YAML or `.tpl` files under the `templates` directory of a
  chart with `Chart.yaml`, so Jinja `*.j2` files and other `templates` directories are no longer
  parsed as Helm. Path classification uses repository-relative paths only.
- Vendored, theme, bundled, and minified JavaScript is no longer scanned for literal SQL.
- Route handlers are resolved through middleware arguments, member references such as
  `orders.list`, single-argument wrappers, FastAPI `add_api_route`, Flask `add_url_rule`, and Spring
  mapping annotations followed by other annotations; inline handlers are identified by method and
  path.
- The CodeGraph adapter is validated against CodeGraph 1.6.1. The structured CLI contract accepts
  1.6.1 and later 1.6.x patch releases; CodeGraph 1.5 and earlier are reported as incompatible.

### Added

- Every scan publishes an HTTP link report: counts of linked, provider-less, ambiguous, and
  external consumer and test calls, and each unlinked call with its reason and candidate
  providers. `status` reports it in `http_links`, query results list the unlinked calls among
  their entities in `link_gaps`, and the MCP coverage resource and query rendering include both.

### Fixed

- `--database` accepts a bare file name such as `graph.db`; the database, its lock, and its work
  sidecar are created in the current directory.

### Release engineering

- Added a release-mode 200-repository, 50,000-file synchronization acceptance test that gates
  content reads of an unchanged scan, rows published by a one-file sync, and database growth
  across 20 syncs. Results and a comparison with 1.1.0 are recorded in `docs/PERFORMANCE.md`.
- Added the `fixtures/cross-language-matrix` workspace: tests in Python, TypeScript, Go, Java, and
  Rust call FastAPI, Flask, Express, NestJS, Next.js, Spring, Gin, Chi, `net/http`, Axum, and
  Actix Web providers through base URLs and cross-file router prefixes, and every pair must
  produce `validates` and `implemented_by` edges.
- Added differential tests: an incrementally maintained database, including its fingerprints and
  extractor batches, equals a fresh scan after each step of a mutation sequence, and the published
  graph, evidence, link report, and artifact rows are identical across repository order, file
  creation order, and extraction worker count.
- Replaced the CodeGraph 1.5.0 fixtures with fixtures captured from CodeGraph 1.6.1; contract tests
  parse every structured CLI output, and the live smoke test runs each structured operation.
- Lowered the workspace MSRV from 1.97.1 to 1.96.0, the lowest toolchain that builds every locked
  dependency at its latest release.
- Updated all dependencies to their latest releases, including `jsonschema` 0.58, `sqlparser`
  0.63, `tree-sitter` 0.27, `rmcp` 3.5, and `serde-saphyr` 1.3.

## [1.1.0] - 2026-10-02

### Breaking changes

- MCP tools now return one bounded Markdown text block plus typed agent-delivery
  `structuredContent`; result `outputSchema` remains omitted. All MCP resources use
  `text/markdown`, with fenced JSON only in the schema catalog.
- HTTP and CLI tool envelopes use delivery schema v2. Explore returns `ExploreReport` and places
  ephemeral source in `source_markdown`.
- Direct MCP mode requires `--config`; binding mode loads its recorded global manifest. Generated
  plugin bindings and ownership receipts use version 2.
- SQLite schema version 2 deliberately rejects 1.0.x databases. A fresh database and complete scan
  are required; no legacy serializer, cache, plugin, or database compatibility path is provided.

### Added

- Added global Explore, Query, MCP-tool, MCP-resource, and schema-catalog limits to
  `executionPolicy`, including checked capacity relationships and immutable policy sharing across
  long-lived servers.
- Added independent scan and agent-delivery fingerprints so presentation-only limits do not
  invalidate snapshots, batches, or checkpoints.
- Explore now reports repository context, source Markdown, resolved symbols, callers/callees,
  evidence-correlated federated handoffs, coverage gaps, truncations, grounded next actions, and
  exact provider execution accounting.
- Query now returns grounded next actions and directs zero-hit source questions toward Explore when
  a real registered repository alias is recognized.
- Added bounded Markdown rendering with UTF-8-safe, block-stable truncation and centralized escaping
  for headings, inline values, paths, controls, and untrusted source blocks.

### Fixed

- Explore now enforces one deadline across snapshot loading, provider traversal, and correlation;
  timed-out or abandoned work is cooperatively cancelled while partial context remains available.
- Snapshot and candidate identity now includes the complete scan fingerprint while excluding
  agent-delivery-only settings, so scan-facing changes cannot reuse stale persisted results.
- Explore reports observed provider concurrency and counts an anchor as traversed only after both
  caller and callee directions complete successfully.
- MCP rendering now preserves compact status, freshness, warnings, coverage, and paths at the
  256-byte minimum, and reports collection retention after both item and byte limits are applied.

### Release engineering

- Added exhaustive typed Markdown fixtures and golden coverage for all 15 MCP tools and every MCP
  resource, including low-budget, UTF-8, fenced-source, error, and nested-collection cases.
- Added direct deadline and cancellation tests for Explore snapshot loading and correlation, plus
  synchronized Markdown-only MCP instructions and CLI configuration examples.
- Updated `rustls` to 0.23.45 (RUSTSEC-2026-0285) with `rustls-webpki` 0.103.15, and replaced the
  yanked `chacha20` 0.10.1 with 0.10.2.

## [1.0.3] - 2026-08-09

### Added

- Added `executionPolicy.maxCodeGraphCorroborationAnchorsPerRepo`, retaining the 50-anchor default
  while supporting smaller positive limits and explicit `-1` unlimited mode.
- Added opt-in per-repository `useGitignore` with workspace-over-local precedence, nested Git rule
  semantics, observable configuration origins, and shared native, OpenAPI, and watch discovery.
  Watched workspaces now reload ignore matchers and directory watches after enabled `.gitignore`
  files change.
- Added `csgraph plugin create` and a versioned Agent Plugins 1.0.0 template for generating portable
  read-only MCP packages with stable clone-independent identities, YAML-safe workspace metadata,
  workspace-verifying routing guidance, and an ignored developer-local runtime binding.
- Added existing-plugin composition mode, `csgraph plugin uninstall`, and `csgraph mcp --binding`.
  Managed MCP entries, routing skills, receipts, and local bindings can be installed and removed
  without changing unrelated plugin components; modified or unowned content is never deleted. New
  local bindings and receipts record the generating `csgraph` version, exact executable
  fingerprint, and build-time source commit/dirty state when available.
- Complete plugins generate one client-neutral Agent Skill. Existing Codex plugins additionally
  receive optional UI metadata in `agents/openai.yaml`; portable-only plugins do not. Every managed
  skill file is validated and owned by the integration receipt.
- Native Claude Code, Codex, and Gemini prompt hooks now act only as intent-based skill selectors;
  the packaged Agent Skill remains the single detailed MCP procedure. Cursor and Antigravity
  project rules are documented as fallbacks when their clients cannot load that skill.
- Consolidated the portable Agent Plugin, canonical skill, native classifier signals, dynamic hook
  guidance, static fallback rules, strict gate, and host-facing text under one visible
  `agent-integration-template/` tree; Rust no longer carries editable routing prose or shell bodies.
- Reduced Agent Skill and hook prompt noise with task-oriented tool selection, progressive loading
  of maintenance guidance, and narrower English and Spanish routing signals that ignore generic
  coding prompts.
- Plugin creation and binding failures now preserve the standard CLI exit classifications for
  invalid input, missing paths, conflicts, and internal failures.

### Fixed

- Existing-plugin uninstall accepts earlier version-1 integration receipts that omitted the newer
  managed-document and local-binding ownership lists, while still verifying exact MCP entries and
  skills plus the recognized local-binding identity before removal.
- Staged change analysis now uses Git's empty-tree identity for an unborn checkout, allowing the
  optional strict pre-commit gate to validate a repository's initial commit.
- Release publication now pushes an explicit tag refspec, avoiding ambiguity when a release branch
  and its version tag share the same name.

### Release engineering

- Added schema validation, idempotency and conflict coverage, Unicode and space-path coverage,
  YAML-frontmatter validation, stable exit-code coverage, local-binding ownership checks, MCP
  handshakes for complete and existing-plugin profiles, and watched `.gitignore` reload coverage.
- Included the complete versioned Agent integration template tree in Unix and Windows release
  archives and smoke validation.

## [1.0.2] - 2026-08-05

### Performance and reliability

- Made `csgraph sync` inspect structured CodeGraph status before invoking the provider, report
  changed and unchanged indexes separately, and reuse the current graph snapshot when neither
  native inputs nor CodeGraph indexes changed.
- Grouped CodeGraph corroboration inputs in one pass, bounded and deduplicated queries
  deterministically, and limited focused scans to the selected repository.
- Released the previous graph before snapshot staging and verified incremental behavior with a
  100-repository synthetic release workload and a representative large-repository workload.
- Turned ambiguous HTTP providers into deterministic scan degradations so unrelated links still
  resolve and exact manual relationships can disambiguate the intended provider.
- Replaced raw worker parser failures with bounded, secret-safe diagnostics that retain actionable
  workspace, repository, configuration, and contract context.

### Documentation

- Documented workspace-scoped Cursor MCP configuration, including restart and enablement checks,
  without requiring a separate CodeGraph installation.
- Added troubleshooting guidance for Cursor setup and per-value extraction budget failures,
  including a concrete `maxStringBytesPerValue` example.

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

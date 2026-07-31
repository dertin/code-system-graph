# Federated Impact and Risk

Code System Graph analyzes impact from one immutable graph snapshot. A missing path means "not observed
within current coverage," never "safe."

## Propagation

The engine resolves one exact node identity or stable key and traverses incoming, outgoing, or both
directions. Confirmed edges at or above the configured confidence threshold produce
`directly_dependent` or `transitively_affected` results. Candidate, inferred, ambiguous,
low-confidence, stale, or incomplete paths remain `possibly_affected` or
`unknown_due_to_coverage`.

Traversal is deterministic and bounded by depth, visited nodes, examined edges, pagination, and
result count. Cycles are visited once. Reports retain stable paths, edge direction, confidence,
epistemic status, and evidence identities.

## Risk model 1.0.0

The numeric score is capped at 100 and uses these additive factors:

- direct consumers: 4 points each, capped at 24;
- transitive fan-out: 1.5 points each, capped at 15;
- additional repositories: 4 points each, capped at 16;
- affected services: 2 points each, capped at 10;
- explicitly public contract: 12 points;
- breaking compatibility: 30 points;
- potentially breaking compatibility: 18 points;
- target centrality at least 0.75: centrality multiplied by 12;
- coupled affected communities: 3 points each, capped at 9;
- missing linked tests: 10 points;
- missing owners: 8 points;
- multiple explicit environments: 10 points;
- explicit criticality tags: authentication 35, data boundary 45, security/payment 50, critical
  system 55.

Scores below 25 are `LOW`, 25-49.999 are `MEDIUM`, and 50 or above are `HIGH`. `CRITICAL` requires
a score of at least 85 plus explicit evidenced critical, security, payment, or data-boundary
assignment. Labels never imply criticality.

`UNKNOWN` has no numeric score. Stale, partial, unavailable, missing, or corrupt relevant
repositories; low-confidence/candidate paths; unknown compatibility; incomplete local enrichment;
and any truncation prevent a numeric conclusion. These conditions can never lower risk.

## Compatibility inputs

Compatibility engines cover the currently modeled portions of HTTP/OpenAPI, events, GraphQL,
protobuf/gRPC, packages, and databases. Every report includes exact before/after fingerprints,
deterministically ordered findings, factors, evidence locators, and recommended validations.
Unmodeled schema semantics return `unknown` or `incomparable`, not `compatible`.

## Tests and owners

Graph `TestCase` nodes connected through `validates` are ranked before optional CodeGraph affected
tests. `owned_by` relations attach owner nodes. Repository-level Rust and Python test commands are
displayed when supported evidence is present. Commands are recommendations only and are never
executed by impact analysis.

## Optional CodeGraph enrichment

CLI impact enrichment is opt-in with `--codegraph`. MCP and HTTP delivery automatically enriches
impact when CodeGraph is enabled in trusted server startup configuration; tool callers do not make
that policy decision. Code System Graph probes each bounded repository-local symbol anchor and uses public
CodeGraph CLI/MCP contracts for local impact and affected tests.
Requests are repository-scoped, limited to three anchors, 25 items, 256 KiB, depth 8, and two
seconds per operation. Source bodies are neither requested for impact nor persisted. Missing,
stale, incompatible, timed-out, degraded, or truncated results are visible coverage gaps.

## Interfaces

- `csgraph impact` returns versioned JSON and supports target identity/stable key, direction,
  depth, summary mode, pagination, and optional CodeGraph enrichment.
- `impact` exposes the conservative report as a read-only MCP and HTTP tool. Server-configured
  local enrichment remains bounded and can never lower federated risk.

Compatibility input from concrete local changes, branches, and pull requests is supplied by the
change-analysis layer. The graph-only interface does not invent a before/after change.

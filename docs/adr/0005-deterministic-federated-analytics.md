# ADR 0005: Deterministic federated analytics

## Status

Accepted

## Context

Code System Graph must rank architecture entities, traverse confirmed cross-repository relationships, and
detect communities without converting incomplete evidence into false certainty. Results must be
reproducible across machines and comparable between immutable snapshots.

## Decision

Query and analytics operate on the published federated snapshot, never on repository source bodies.
All operations are bounded by explicit result, node, edge, depth, path, iteration, and time limits.
Candidate or incomplete edges are reported separately and are not traversed as confirmed paths.

Search combines deterministic structural signals with SQLite FTS5 candidates. Every score exposes
its contributing signals and freshness penalty. Pagination is applied only after a stable
score-and-identity ordering.

Traversal supports deterministic BFS, weighted shortest path, and bounded loopless k-shortest
paths. Relationship confidence and configured kind costs determine positive finite path weights.
Failure to observe a path returns the reachable frontier, candidate links, coverage gaps, and
truncation state.

Community analysis supports connected components, deterministic weighted clustering, and a seeded
Louvain implementation. Inputs include algorithm version, scope, seed, resolution, confidence
threshold, edge weights, and iteration limit. Community identities derive from sorted memberships;
labels derive only from structural graph terms with explicit evidence. Snapshot comparison uses
member overlap to report created, removed, split, merged, and materially changed communities.

Community snapshots and memberships are persisted in the same transaction as their graph snapshot.
An unchanged scan reuses the current graph and community result.

## Consequences

- Identical graph, configuration, engine version, and seed produce byte-identical results.
- Approximate scores and clusters remain explainable measurements, not evidence-backed graph facts.
- Dynamic source semantics and unresolved links cannot silently improve ranking or connectivity.
- Algorithm changes require an engine version change and new deterministic fixtures.
- Large graph performance is controlled by bounds; release-scale benchmarks remain a separate
  release gate.

# Federated Query and Communities

Code System Graph analyzes only immutable federated snapshots. Search results, paths, centrality, and
communities are derived views; they do not create graph evidence or claim that an unobserved
relationship is absent.

## Ranked search

Search candidates combine:

- exact and normalized label or stable-key matches;
- normalized prefix and suffix matches;
- SQLite FTS5 rank;
- requested entity, repository, service, and community scopes;
- graph centrality and community membership;
- direct evidence confidence and epistemic quality;
- a conservative freshness penalty.

The response includes every contributing signal, applied filters, coverage, and freshness.
Ordering is score descending and then stable node identity. Offset pagination is applied after that
ordering and all result limits are bounded. Search returns graph entities and locators, never source
bodies.

## Bounded traversal

Traversal algorithms are deterministic BFS, weighted shortest path, and bounded loopless
k-shortest paths. Requests constrain direction, depth, cross-repository hops, relationship kinds,
minimum confidence, environments, tests, generated artifacts, node and edge visits, timeout, and
path count.

Only confirmed eligible edges participate in a confirmed path. Candidate and incomplete edges are
returned separately. When no path is observed, the report includes the last reachable frontier,
coverage gaps, and whether any bound truncated the search.

Weighted traversal uses positive finite costs derived from relationship confidence and explicit
kind costs. Local and cross-repository segments are identified from endpoint repository ownership.

## Community algorithms

The engine supports:

1. Connected components for isolated graph regions.
2. Deterministic weighted clustering as a conservative fallback.
3. Seeded Louvain modularity optimization for architecture communities.

Configuration records the algorithm, engine version, scope, seed, resolution, confidence
threshold, relationship weights, and iteration limit. Eligible directed relationships are
symmetrized deterministically for clustering. Supported scopes are repository, exact service,
workspace, and the complete federated graph.

The same snapshot, configuration, engine version, and seed must serialize identically. A changed
algorithm or tie-breaking rule requires an engine version change.

## Metrics and labels

Each community reports:

- members, repositories, services, and central nodes;
- inbound and outbound contracts;
- size, density, cohesion, coupling, and cross-community edges;
- centrality and god-node limitations;
- deterministic label terms and their source node identities;
- incomplete inputs and other limitations.

Labels use only bounded structural terms from repository, service, contract, and central-node
labels. LLM-generated names are outside the factual 1.0.0 engine.

## Snapshot deltas

Community snapshots are immutable and versioned with their graph snapshot. Member Jaccard overlap
drives deterministic comparison:

- `created`: no material predecessor;
- `removed`: no material successor;
- `split`: one predecessor maps materially to multiple successors;
- `merged`: multiple predecessors map materially to one successor;
- `materially_changed`: one best match remains but membership or metrics changed materially.

Every delta includes related community identities, overlap, and an explanation. Deltas do not
infer organizational intent.

## Persistence and recomputation

Community snapshots, communities, and memberships are stored transactionally with the graph
snapshot. Membership rows reference existing snapshot nodes. Unchanged scans reuse the current
snapshot; changed scans recompute communities before publishing the replacement snapshot.

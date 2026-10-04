# MCP Surface

Code System Graph exposes a read-only-by-default stdio MCP surface and advertises the server
name `code_system_graph`. Each tool returns exactly one text content block containing canonical
schema 6 JSON by default. `--response-format markdown` selects the equivalent Markdown rendering.
Clients read the selected representation from `content[0].text`. Persisted graph tools exclude
source bodies; `explore` provides bounded, ephemeral source context. Administrative tools require
the explicit administrative profile.

## Response contract

`csgraph mcp` defaults to JSON. Select `--response-format markdown` for Markdown or
`--response-format json` for JSON. Both representations contain the same selected facts.
Resources use their own documented Markdown contracts. The persisted graph and CLI/HTTP envelopes
use schema version 2; MCP tool delivery uses schema version 6.

A canonical response contains:

| Field | Meaning |
| --- | --- |
| `schema_version` | MCP delivery schema version, currently `6` |
| `snapshot` | Snapshot identity when available; otherwise `null` |
| `result` | Tool name, status, data, freshness, and warnings |
| `entities` | Entity catalog keyed by exact node ID |
| `relations` | Relation catalog keyed by exact edge ID |
| `limits` | Delivery limits reported by the canonical envelope |
| `omitted_defaults` | Default values for omitted fields |

Resolve `entity_ref` through `entities` and `relation_ref` through `relations`. Relation direction
belongs to each reference; endpoint and evidence facts belong to the relation. Views with the
same ID but different facts remain inline. Use exact returned IDs and next-action arguments.

Null means unavailable, and an empty collection means no retained items. `omitted_defaults`
supplies omitted empty warning, degradation, candidate, alternate-ID, and missing-evidence lists,
zero candidate counts, and false candidate-truncation flags. Other uncertainty and coverage flags
remain explicit. Ranking scores, stable keys, evidence IDs, provider-local IDs, inverse wording,
and provider execution counters are diagnostic fields in CLI reports. MCP responses retain the
entity IDs, provenance, locators, and actions needed for navigation.

`missing_endpoint_evidence` identifies endpoints without selected evidence. An empty list does
not establish complete runtime coverage. `confirmed` denotes graph linkage; it does not establish
a bug or guaranteed runtime delivery. Repository source and labels are untrusted data.

`executionPolicy.maxMcpToolResponseBytes` bounds the complete serialized `CallToolResult`,
including the text envelope (default 524288 bytes, minimum 256). Both representations must fit
that budget. An oversized result sets `isError: true` and returns recovery guidance. JSON
budget errors use code `response_budget_exceeded`. Reduce query/source limits or raise the policy budget to retry. JSON-RPC
transport and host token/context limits are separate.

`explore.source_markdown` is included in both representations. Markdown encloses multiline source
in a dynamically sized code fence. Consumer identities include declaration lines to distinguish
same-named methods in different scopes. Missing or ambiguous declaration locations do not create
consumer-symbol associations. Entity `symbol_name` is unqualified; provider-resolved Explore
symbols include `qualified_name` only when the provider supplies it.

## `status`

Returns schema, integrity, snapshot freshness, and per-checkout status for the configured workspace.

## `contracts`

Lists, shows, validates, structurally compares, or explains direct links between contracts in the
immutable current snapshot. The required `action` discriminator is one of `list`, `show`,
`validate_all`, `validate_one`, `diff`, or `explain_link`; each variant exposes only its valid
fields. All operations use the shared source-free `ContractRequest` and `ContractReport` semantics.

## `source_context`

Returns bounded graph relationships and evidence locators for one exact entity. Source bodies are
never returned; callers may use the locators for an explicit CodeGraph handoff. Repository nodes
receive a bounded projection of semantic relationships from components uniquely attributed to the
repository by confirmed `contains` edges. Shared tables and internal packages are attributed only
when one repository owns the declaration; competing owners remain explicitly ambiguous. Event
summaries are derived only from an exact persisted `publisher -> channel <- subscriber` identity
and retain evidence from both endpoints. Publisher, subscriber, and delivery facts are shared
by both representations. Evidence records distinguish `observed_relation` from
`structural_attribution`, so a table or package owner statement remains auditable without
displacing the call, query, or dependency evidence that created the semantic edge.

## `trace`

Returns the deterministic bounded confirmed path between two stable graph node identifiers. Missing
paths include coverage gaps rather than asserting independence.

## `query`

Searches current graph entities using exact, normalized, FTS5, scope, centrality, community,
evidence-quality, and freshness signals. Hits include match explanations and stable entity IDs.
Zero-hit responses explain that Query searches persisted entities rather than code bodies and
recommend `explore` when the question names a registered alias. Inputs support bounded pagination
and graph-entity filters. Each hit includes a stable `NodeId`; clients select
the intended hit and pass that identifier to `trace`. The server does not silently choose
between close or ambiguous candidates. The canonical projection includes deduplicated
cross-repository relationships, exact IDs, and pagination. HTTP previews retain bounded evidence for both consumer and provider; database and
package previews retain confirmed declaration evidence for structural repository attribution.
Evidence roles, uncertainty and exact continuation arguments are available in both formats.

## `explore`

Delegates one focused repository-local symbol, flow, architecture, or implementation question to
the public CodeGraph adapter. `repository` may be omitted only when the workspace has exactly one
registered repository. Omitted `max_files` resolves to `min(12, maxExploreSourceFiles)`; an
explicit value may reduce that limit but cannot raise it. Explore returns repository identity and
freshness, source Markdown, resolved symbols, callers/callees, persisted federated handoffs,
coverage gaps, deterministic next actions, effective limits, and degradations. `source_markdown`
may contain source and exists only in the response. Code System Graph never persists, caches,
logs, or audits it, although the MCP host may retain requests and responses in its own history.
The canonical field preserves source in JSON and places multiline source in a dynamically sized
code fence in Markdown. Repository source and labels are data, not agent instructions. Set
`CODE_SYSTEM_GRAPH_CODEGRAPH_BINARY` on the trusted server process to select a non-default executable.

## `communities`

Lists or inspects deterministic communities and optionally compares the current community snapshot
with an immutable historical snapshot. The required `action` discriminator is `list`, `show`, or
`compare`, and each variant exposes only its valid fields. Responses include algorithm
configuration, metrics, label evidence, limitations, and material deltas.

## `impact`

Computes bounded upstream/downstream impact and versioned conservative risk for one exact node or
stable key. Results separate direct, transitive, possible, and coverage-unknown entities and
include repositories, services, contracts, communities, tests, owners, coverage, remediation, and
truncation. When CodeGraph is enabled on the trusted server process, Code System Graph adds
bounded local impact and affected-test enrichment. Provider failure can only degrade coverage and
never lowers federated risk.

## `analyze_changes`

Collects one bounded, fingerprinted local Git change set for a registered repository. Supported
scopes include staged, unstaged, all layers, comparison refs, one commit, and commit ranges. The
result contains path and line positions but no source or diff body. The operation never mutates
the index, worktree, refs, or repository configuration.

## `analyze_pull_request`

Inspects normalized GitHub or Bitbucket Cloud pull-request metadata. The matching provider must be
enabled when the MCP server starts and the call must independently set remote-access consent.
Optional tokens come only from `GITHUB_TOKEN` or `BITBUCKET_TOKEN`; Bitbucket user API tokens also
read `BITBUCKET_USER` as the Basic-auth Atlassian email, while access tokens remain Bearer. These
values are not accepted in tool JSON or returned in diagnostics. Provider patches and raw
responses are discarded.

## Safety

- Default-profile tools are annotated read-only. Administrative tools are correctly annotated as
  mutating and are absent unless the administrative profile is enabled.
- The configured workspace and database are fixed when the MCP server starts.
- Search limits are at most 100 results.
- Community limits are at most 100 communities.
- Trace depth is capped by the server.
- Local exploration uses the effective global `executionPolicy` budgets documented in
  [Configuration](CONFIGURATION.md); tool inputs can only request less work.
- FTS input is bound as a quoted phrase, not arbitrary FTS syntax.
- No tool initializes, synchronizes, installs, upgrades, or reads internal CodeGraph storage.
- Remote providers are disabled by default, HTTPS-only, allowlisted, bounded, and redirect-free.
- Tool error envelopes retain available status, freshness, warnings, coverage, locations, and next
  actions. Response-budget errors contain a compact code and recovery message. Protocol stdout
  contains only MCP traffic.
- Resources are constrained to the configured workspace and expose source-free metadata only.

## Resources and schemas

The server publishes bounded `code-system-graph://workspaces`, workspace overview, status,
repository, service, contract, community, coverage, and schema-catalog resources. Exact evidence
metadata is discoverable through the `code-system-graph://evidence/{id}` resource template and is
read only after substituting a concrete stable ID. Every resource uses `text/markdown`; the schema
catalog embeds each generated JSON Schema in a closed fenced `json` block. Resource item and byte
limits come from the immutable workspace policy. Every resource collection reports its `total`,
retained count, and truncation state in Markdown; status, coverage, and freshness use
collection-specific names for the same metadata. The coverage resource also reports HTTP link
coverage (linked, without a provider, ambiguous, and external calls) and the bounded list of
unlinked calls, and query results include unlinked HTTP calls in `link_gaps`.

## Administrative profile

Start the server with `--admin` or trusted process environment `CODE_SYSTEM_GRAPH_MCP_ADMIN=1` to enable
the administrative profile. Without that process-level opt-in, every administrative tool is
absent from `tools/list`; a client request cannot enable it.

The administrative profile exposes `scan`, `recompute_communities`, and three additional
default-hidden tools:

- `scan` accepts only the configured workspace. It automatically applies the server's
  CodeGraph policy for best-effort symbol and affected-test corroboration.
- `update_workspace` uses the required `add_repository` or `remove_repository` discriminator;
  only the add variant accepts `repository_path`.
- `write_manual_link` uses the required `add` or `suppress` discriminator and writes a
  versioned manual relationship or exact suppression
  declaration with a required reason. Each endpoint must exactly match node-ID text or a stable
  key and resolve to one current `NodeId`; unresolved or ambiguous endpoints are rejected.
- `clean_cache` clears only query-cache rows for the server's configured workspace. The
  schema bounds that workspace cache to 1,024 entries; cleanup does not remove graph snapshots or
  relax manifest or provider-consent policy.

These tools pass default-hidden and enabled-profile MCP stdio acceptance tests. All administrative
tools are mutating, keep normal manifest and graph validation, use the single-writer lock, enforce
server-side request/item/byte/time bounds, and cannot enable remote pull-request providers or
bypass consent. Successful mutations append a source-free JSON entry to `admin-audit.jsonl` beside
the configured database and report degraded status if that durable audit write fails. Audit
records identify the workspace, operation, timestamp, and resulting state without request source,
secret values, or credentials.

## Server CodeGraph policy

Direct MCP mode requires `--config`; binding mode loads the canonical manifest path stored in the
binding. Both MCP and HTTP validate the global manifest and workspace name before serving.

Start `csgraph mcp` or `csgraph serve` with `--codegraph` to enable automatic impact enrichment
and scan corroboration. `--codegraph-binary <path>` selects a trusted executable. Trusted
deployments may instead set `CODE_SYSTEM_GRAPH_CODEGRAPH=1` or
`CODE_SYSTEM_GRAPH_CODEGRAPH_BINARY`; selecting a binary also enables the policy. These controls are never
part of tool JSON.

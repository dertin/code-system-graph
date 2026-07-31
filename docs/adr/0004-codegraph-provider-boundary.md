# ADR 0004: CodeGraph provider boundary

- Status: Accepted
- Date: 2026-07-29

## Context

[CodeGraph](https://github.com/colbymchenry/codegraph) is an independent MIT-licensed project by
Colby Mchenry and an optional authority for intra-repository symbols and flow. It is not part of
Code System Graph. Code System Graph owns cross-repository boundaries and must remain functional
when CodeGraph is absent, stale, or incompatible.

## Decision

The application depends on a `LocalCodeIntelligenceProvider` port. Production adapters use
only CodeGraph's public MCP or CLI interfaces. MCP is preferred and performs `initialize` plus
`tools/list`; adapters map capabilities from tool names and schemas instead of assuming a
fixed release.

Every operation has a timeout, cancellation token, output limit, repository scope, and
capability check. Child stdout is protocol-only and stderr is diagnostic-only. Source returned
by CodeGraph is ephemeral and is never written to the Code System Graph database, durable cache, logs,
or historical results.

Unavailable, stale, invalid, or timed-out providers produce a per-repository degraded result.
Missing optional capabilities do not fail federated boundary traversal. Code System Graph never reads
CodeGraph internals and never runs its initialization, synchronization, installation, or
upgrade commands.

## Consequences

- Contract fixtures and a documented compatibility matrix define supported behavior.
- Tests use an explicit fake provider; production code does not simulate provider success.
- Federated answers carry handoffs so callers can request exact local context separately.

## Implementation evidence

The provider contract is validated against CodeGraph 1.5.0. The official Rust MCP client
negotiates initialization and discovers tools dynamically; 1.5.0 exposes `codegraph_explore`
with explicit `projectPath`.
Structured symbol, neighbor, impact, and affected-test operations use versioned CLI JSON
contracts. MCP starts with `--no-watch`, timeout opens a per-repository MCP circuit, and CLI
fallback remains bounded by the same end-to-end request deadline.

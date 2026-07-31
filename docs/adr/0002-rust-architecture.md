# ADR 0002: Rust workspace architecture

- Status: Accepted
- Date: 2026-07-29

## Context

Code System Graph needs stable domain contracts, multiple adapters, bounded concurrency, and a
self-contained cross-platform distribution. The domain must not depend on delivery or storage
details.

## Decision

Code System Graph uses Rust 2024 in a Cargo workspace. Dependencies point inward:

```text
CLI / MCP
    -> application
        -> query / linker / extractors / registry ports
            -> model

SQLite and CodeGraph are outbound adapters implementing application-owned ports.
```

The initial slice starts with only crates that enforce a real boundary:

- `code-system-graph-model`: identities, graph entities, evidence, freshness, and public envelopes.
- `code-system-graph-store-sqlite`: embedded migrations and transactional persistence.
- `code-system-graph-core`: strict manifest loading, HTTP boundary extraction, deterministic linking,
  trace application service, and provider ports.
- `code-system-graph-cli`: process entry point and delivery adapters, including MCP stdio.

Crates split further only when extractor, query, provider, or interface boundaries have enough
behavior to justify independent compilation and ownership. Library errors are typed with
`thiserror`; `anyhow` is restricted to binary entry points. Tokio owns asynchronous lifecycle,
while SQLite and CPU-bound work run outside async executor threads.

## Consequences

- The core remains testable without SQLite, MCP, CLI, HTTP, network, or CodeGraph.
- Fewer initial crates reduce empty scaffolding while preserving directed dependencies.
- Public APIs require English rustdoc and JSON Schema where they cross process boundaries.

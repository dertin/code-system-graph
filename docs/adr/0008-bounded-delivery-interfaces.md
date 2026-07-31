# ADR 0008: Bounded Delivery Interfaces and Optional Host Integration

## Status

Accepted.

## Context

Code System Graph exposes the same local intelligence through CLI, MCP stdio, optional HTTP, and host
hooks. Divergent business logic, implicit network listeners, hidden mutation, or source-bearing
protocol responses would weaken the local-first and evidence-first guarantees.

## Decision

CLI, MCP, and HTTP handlers delegate to shared application services and versioned Rust contracts.
Delivery adapters may enforce stricter limits but cannot widen application bounds. Public JSON
schemas derive from the Rust request and report types.

MCP stdio is read-only by default. Administrative tools are absent from capability discovery
unless the process starts with an explicit administrative profile. Administrative calls retain
normal writer locks, previews, audit records, and server-side bounds.

HTTP is optional and binds to loopback by default. A non-loopback bind requires a configured
bearer token, constant-time token verification, request/concurrency/rate limits, timeouts, and
graceful cancellation. Authentication material is ephemeral and excluded from schemas,
diagnostics, and persistence.

Resources use stable `code-system-graph://` URIs, enforce the configured workspace policy, and return small
versioned source-free documents. Evidence resources expose metadata and locators, never source
bodies.

Host hooks are optional, advisory, idempotent, marker-owned, and fail open by default. They
classify routing intent but do not automatically scan, synchronize CodeGraph, fetch source, or
mutate repositories. Strict pre-commit behavior is a separate explicit mode tied to an exact
staged-change fingerprint. Install and uninstall preserve unrelated host configuration.

## Consequences

- Protocol adapters remain testable against the same deterministic services.
- Starting Code System Graph does not implicitly open a network listener or expose mutation.
- HTTP and strict hook operation require visible operator consent and configuration.
- Unsupported host protocols degrade to a documented guidance file instead of invented behavior.
- Public URI and schema changes require explicit versioning.

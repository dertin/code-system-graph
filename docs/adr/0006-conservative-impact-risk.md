# ADR 0006: Conservative Federated Impact and Risk

## Status

Accepted.

## Context

Cross-repository impact is incomplete whenever a repository, extractor, contract, local CodeGraph
index, or traversal bound is incomplete. Treating low confidence or an unobserved path as low risk
would create a false-safe result.

## Decision

Code System Graph computes impact from one immutable graph snapshot with a versioned deterministic risk
model. It separates directly dependent, transitively affected, possibly affected, and
unknown-due-to-coverage entities. Confirmed relationships produce confirmed propagation; candidate,
incomplete, stale, low-confidence, or truncated observations produce explicit uncertainty.

Risk uses documented factors and bounded scores. `UNKNOWN` has no numeric score. Stale or partial
inputs, missing required repositories, unresolved direct candidates, and truncation can only
increase uncertainty. `LOW` requires fresh sufficient coverage, while `CRITICAL` requires explicit
breaking, criticality, or sensitive-boundary factors with evidence.

Compatibility reports are inputs to impact rather than graph truth. HTTP/OpenAPI, events, GraphQL,
protobuf/gRPC, packages, and database engines retain before/after fingerprints, rule factors,
evidence locators, and recommended validations.

CodeGraph enrichment is optional, bounded, ephemeral, and disabled by default. Code System Graph sends only
repository-scoped anchors through public MCP/CLI contracts. Provider source is never persisted.
Missing, stale, incompatible, timed-out, or truncated enrichment is reported as coverage
degradation and cannot lower risk.

Test recommendations combine graph-linked tests, optional CodeGraph affected tests, owners, and
explicit validation commands. Code System Graph does not execute tests in another repository unless the
user issues a separate explicit execution command.

## Consequences

- Identical graph, configuration, compatibility, enrichment, and freshness inputs produce
  byte-reproducible reports.
- Empty impact means "not observed within coverage," never "definitively safe."
- Pagination and summary modes preserve aggregate risk and coverage factors.
- Risk-model factor or threshold changes require a model-version change and new fixtures.
- Change-set and pull-request analysis remain separate producers of impact inputs.

# Technical Documentation

This index is for maintainers, integrators, and contributors. If you want to install and use Code
System Graph, start with the [README](../README.md), [first workspace guide](GETTING_STARTED.md),
or user-facing [supported technologies guide](SUPPORTED_TECHNOLOGIES.md).

## Product contracts

- [Architecture](ARCHITECTURE.md): component boundaries and runtime flow.
- [Data model](DATA_MODEL.md): identities, nodes, edges, evidence, snapshots, and persistence.
- [Extractor coverage](EXTRACTOR_COVERAGE.md): exact validated formats, frameworks, evidence, and
  deliberate limits.
- [Delivery interfaces](INTERFACES.md): shared CLI, MCP, HTTP, and hook behavior.
- [MCP surface](MCP.md): tools, resources, bounds, profiles, and safety policy.
- [CodeGraph integration](CODEGRAPH_INTEGRATION.md): public provider contract, compatibility,
  fallback, and degradation.

## Analysis internals

- [Impact and risk](IMPACT.md)
- [Change and pull-request analysis](CHANGES.md)
- [Community analysis](COMMUNITIES.md)

## Operations and release

- [Performance](PERFORMANCE.md): workload definitions and measured local evidence.
- [Release engineering](RELEASE.md): validation, packaging, platform evidence, and publication
  requirements.
- [Host hooks](HOOKS.md): routing lifecycle, marker ownership, and strict mode.
- [Security policy](../SECURITY.md)
- [Threat model](threat-model/initial.md)

## Architecture decisions

- [ADR 0001: Original implementation and licensing](adr/0001-original-implementation-and-licensing.md)
- [ADR 0002: Rust architecture](adr/0002-rust-architecture.md)
- [ADR 0003: SQLite storage](adr/0003-sqlite-storage.md)
- [ADR 0004: CodeGraph provider boundary](adr/0004-codegraph-provider-boundary.md)
- [ADR 0005: Deterministic federated analytics](adr/0005-deterministic-federated-analytics.md)
- [ADR 0006: Conservative impact risk](adr/0006-conservative-impact-risk.md)
- [ADR 0007: Opt-in change providers](adr/0007-opt-in-change-providers.md)
- [ADR 0008: Bounded delivery interfaces](adr/0008-bounded-delivery-interfaces.md)
- [ADR 0009: Release engineering](adr/0009-release-engineering.md)

## Development

- [Contributing](../CONTRIBUTING.md): toolchain, checks, tests, and contribution expectations.
- [Changelog](../CHANGELOG.md)

Public behavior belongs in user guides or a versioned interface document. Internal rationale and
implementation decisions belong here or in an ADR; they should not make the README onboarding path
longer.

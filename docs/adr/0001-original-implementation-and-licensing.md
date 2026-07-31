# ADR 0001: Original Implementation and Licensing

- Status: Accepted
- Date: 2026-07-29

## Context

Code System Graph provides federated repository intelligence and integrates with optional external tools.
Its implementation and distribution need clear provenance, compatible licensing, and stable
boundaries between Code System Graph data and provider-specific data.

Code System Graph is distributed under the Apache License 2.0. Dependencies, bundled assets, examples, and
contributions must preserve that distribution model and provide the notices required by their
licenses.

## Decision

Code System Graph implementation, tests, schemas, fixtures, and documentation are maintained as
project-original work based on documented product behavior and public technical standards.
Contributions must identify third-party material and confirm that its license is compatible with
Apache-2.0 distribution.

Dependency policy checks evaluate licenses and advisories. Release artifacts include the project
license, notices, third-party notices, and a software bill of materials generated from the locked
dependency graph.

The optional [CodeGraph](https://github.com/colbymchenry/codegraph) integration interoperates with
that independent MIT-licensed project through its public MCP capabilities or versioned CLI JSON
contracts. CodeGraph is not bundled or redistributed with Code System Graph. Code System Graph
treats provider identifiers as opaque external locators and does not make provider storage layouts
part of its data model. Provider source and context are transient; only bounded, source-free metadata
and exact corroborating evidence can enter Code System Graph results.

Compatibility with an external provider is established through capability discovery, versioned
contracts, and reproducible tests. Missing capabilities, unknown versions, stale indexes, and
transport failures remain explicit degradation data.

## Consequences

- Public behavior and compatibility claims require reproducible tests or public documentation.
- New dependencies and copied assets require license review and appropriate attribution.
- Release artifacts carry notices and an SBOM alongside integrity metadata.
- Provider adapters can evolve without coupling Code System Graph identity or persistence to private
  provider formats.
- Unclear contribution provenance must be resolved before the contribution is distributed.

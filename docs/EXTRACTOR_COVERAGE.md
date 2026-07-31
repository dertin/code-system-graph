# Extractor Coverage

This is the technical validation matrix for maintainers and contributors. If you want to know
whether your application stack is recognized and what `scan` can connect, start with
[Supported technologies](SUPPORTED_TECHNOLOGIES.md).

This matrix records validated behavior for package/HTTP, event/GraphQL/protobuf, and
data/infrastructure/documentation extraction. An item is covered only when its parser, evidence
behavior, ambiguity handling, incremental persistence, and tests are present.

## Package ecosystems

- npm, pnpm, and Yarn: package coordinates, dependency scopes, workspaces, exports, and lockfile
  format/presence from `package.json`, `package-lock.json`, `pnpm-lock.yaml`, and `yarn.lock`.
- Python: PEP 621, Poetry groups, requirements markers, optional dependencies, and lockfile
  metadata from `pyproject.toml`, requirements files, and `poetry.lock`.
- Cargo: packages, exact workspace-member manifest containment, dependency scopes, target
  conditions, features, renames, and lockfile metadata.
- Go: module/workspace identities, requirements, replacements, indirect conditions, and members.
- Maven and Gradle: literal coordinates, scopes/configurations, optional flags, and declared
  profile/condition metadata.
- NuGet/MSBuild: `packages.config` and SDK-style `PackageReference` declarations.

External dependencies remain external unless an exact package identity matches a package owned by
another registered repository. Lockfiles prove format and presence; Code System Graph does not infer a
complete transitive graph from lockfile syntax that the focused parser does not understand.
Dynamic Gradle expressions, URL requirements, includes, and unsafe paths are not promoted to exact
dependencies.

## HTTP contracts and generated clients

- OpenAPI 3.x and Swagger 2.0 provider operations, including conservative Swagger `basePath`
  normalization.
- OpenAPI Generator `openapitools.json`, `.openapi-generator/VERSION`, and
  `.openapi-generator/FILES` metadata. An explicit repository-relative `inputSpec` creates an
  evidence-backed `consumes` edge from the generated-client artifact to matching contract
  operations.
- TypeScript/JavaScript: Fetch, Axios, Express, Fastify, NestJS, and Next.js App Router file routes.
- Python: requests, httpx, aiohttp, FastAPI/`APIRouter` prefixes, and Flask.
- Go: `net/http`, Gin, and Chi.
- Java: Spring MVC, WebClient, and Feign.
- Rust: Axum and Actix Web executable routes, advisory Utoipa operation annotations, and Reqwest.
- Rust tests: built-in test attributes, Tokio tests, and rstest.
- Python tests: top-level pytest functions, direct `unittest.TestCase` methods, and canonical
  class-level `test_` methods whose indirect runner inheritance is outside the current file. The
  latter use the generic `python-test` framework label rather than an unproven runner label.

Each mandatory source language is parsed with its Tree-sitter grammar before narrow
framework-specific recognition. A recognized fact must have a structural call, decorator,
annotation, attribute, or canonical test-function candidate. Syntax recovery makes observations
incomplete. Only literal or conservatively resolvable methods and paths become confirmed graph
boundaries; dynamic values remain incomplete or ambiguous and are not auto-linked.

Actix/Axum routes and Utoipa annotations coexist when attached to the same Rust symbol. Executable
route declarations retain confirmed confidence; Utoipa implementation edges remain inferred at
advisory confidence unless stronger executable evidence confirms the same operation.

## Event contracts and source boundaries

- AsyncAPI 2.x and 3.x YAML/JSON channels, send/receive or publish/subscribe operations, local
  component references, message names, bounded top-level payload schemas, broker bindings,
  delivery semantics, routing/partition keys, and dead-letter channels.
- Literal publisher/subscriber calls for Kafka, RabbitMQ, AWS SNS/SQS, NATS/JetStream, Google
  Pub/Sub, and generic publish/subscribe APIs across supported source languages.
- Broker/namespace/channel graph identities with `publishes`, `subscribes`, and directional
  `delivers_to` edges. Namespace-free and generic source facts link only to one unambiguous
  declaration identity.
- Compatibility detects removed channels/roles, removed or type-changed fields, newly required
  fields, routing/partition changes, and explicit schema-version changes. Incomplete documents
  produce `unknown` rather than compatible.

Dynamic or interpolated channels never become exact graph nodes. Remote references, wrappers,
aliases, broker defaults, and runtime delivery guarantees are not inferred.

## GraphQL

- SDL types, fields, arguments, nested type references, operations, fragments, expanded consumed
  field paths, persisted-operation manifests, resolvers, and federation metadata.
- Focused resolver and embedded-document recognition for JavaScript/TypeScript, Python, Go, Java,
  and Rust framework patterns.
- Exact root-field consumer/provider links and same-repository resolver implementation links;
  zero or multiple candidates remain unlinked.
- Compatibility detects removed types/fields/root operations, output narrowing, newly required
  arguments, and changed or removed persisted operations.

Dynamic documents and resolver symbols remain incomplete. Code System Graph does not execute a GraphQL
schema, fetch remote schemas, or infer resolver ownership without an exact coordinate.

## Protobuf and gRPC

- Proto2, proto3, and editions declarations; packages; default/public/weak imports; messages;
  nested messages/enums; field names/numbers; cardinality; oneofs; maps; wire types; reservations;
  options; services; request/response types; and client/server streaming modes.
- Exact canonical gRPC paths from source containing a recognized generated-code header. Marker
  roles are client, server, or unknown based on explicit nearby generated constructs.
- Package-qualified generated-client/provider and generated-server/implementation links only when
  one exact RPC provider exists. Unknown roles and duplicate providers remain unlinked.
- Compatibility detects package, message, enum, service, and method removal; field-number reuse or
  movement; wire/type/cardinality changes; newly required proto2 fields; enum numeric reassignment;
  and RPC request/response/streaming changes.

Generated filenames alone are not evidence. Code System Graph does not run `protoc`, resolve imported
schemas outside the extracted file set, or infer application handlers from generated stubs.

## Data and SQL

- SQL DDL and migration tables, columns, indexes, foreign keys, and bounded migration metadata.
  Every recognized migration remains an artifact even when no table declaration is recoverable.
  Exact reversible filename pairs emit `reverts` edges, and unique numeric revisions in one
  directory emit consecutive `precedes` edges.
- Prisma models and mappings, Alembic operations, SQLAlchemy model/table bindings, and Diesel
  table declarations.
- SQLx runtime query functions, checked and unchecked inline macros, `raw_sql`, direct imports,
  import aliases, and conservative `QueryBuilder` recognition.
- SQLx checked and unchecked `query_file*!`, `query_file_as*!`, and `query_file_scalar*!`
  references resolved from the nearest Cargo crate root, with exact source-symbol -> query-file ->
  table links and incremental relinking when the external query changes.
- SQLx default/custom `migrate!` and runtime `Migrator::new` directory references, including
  `sqlx.toml` `[migrate].migrations-dir` overrides and simple or reversible
  `.up.sql`/`.down.sql` migration filenames.
- `mysql_async` `Queryable::query*`, `exec*`, streaming and preparation methods plus fluent
  `Query`, `WithParams`, and `BatchQuery` calls, including trait aliases and byte-string queries.
- psycopg/psycopg2, PyMySQL, and Python wrapper calls such as `Database.sql(...)`, preserving
  static table access in value-only f-strings and `.format(...)` calls while leaving interpolated
  table names incomplete.
- SQLAlchemy ORM reads and writes linked only through exact model-to-table declarations.
- MariaDB/MySQL dump recovery that retains table declarations across supported dialect statements
  and truncates oversized column/index detail without aborting the workspace scan.
- Factory Boy `Meta.model` links from a factory symbol to its statically imported Python model.
- Literal SELECT/WITH/INSERT/UPDATE/DELETE observations across supported source languages with
  reader/writer roles and enclosing-symbol anchors.
- Exact linking to one unambiguous normalized table identity shared across languages and
  repositories.

Dynamic, interpolated, concatenated, or unparseable SQL remains incomplete and unlinked.
`QueryBuilder` retains any complete literal base statement but is incomplete when later fragments
can change table identity. SQLx row/type derives do not prove a table binding. Query bodies and
literal values are discarded before persistence.

## Infrastructure and deployment

- Docker Compose services, images, ports, dependencies, and environment key names.
- Kubernetes workloads, Services, Ingresses, static resources, selectors, and Secret key names.
- Helm templates and values with static names retained and templated values marked incomplete.
- Terraform/OpenTofu literal resource declarations and dependency metadata.
- Repository `deploys`, deployment `provides`, resource dependency, and config-key links from
  direct declarations.

Code System Graph does not render Helm, execute Terraform, resolve remote modules, or infer Kubernetes
workload ownership from selectors alone.

## Documentation, ownership, and configuration

- Markdown README/runbook/RFC/ADR classification, headings, explicit links, and canonical
  references.
- CODEOWNERS ordered rules and explicit owner identities.
- YAML/JSON service catalogs with declared services, owners, contracts, and dependencies.
- Dotenv, YAML, JSON, and TOML key names, nested scopes, and conservative sensitive-key
  classification without values.
- Canonical, unique, or repository-qualified document links; ambiguous labels remain unlinked.

Documentation prose is inert data and does not imply graph relations. Configuration values,
credentials, connection strings, and secret material are never stored.

## Incremental and linking behavior

Extractor outputs are stored per repository, checkout, lossless source path, extractor, content
hash, and extractor version. Identical batches are reused. Adds, replacements, and deletions
recompute only affected method/path neighborhoods; a missing previous deletion batch fails closed.
`--force` never reuses a planned replacement, and internal extractor revisions invalidate cached
outputs even when source content is unchanged.
Duplicate providers remain ambiguous. Consumers, tests, provider contracts, and implementation
symbols require direct evidence before edges are created.

Optional scan-time CodeGraph corroboration uses only public MCP/CLI operations. Exact symbol
path/name/line matches add provenance to an existing source-derived implementation edge; they
never invent a federated edge or persist returned source.

## Validated fixtures

The platform fixture proves:

- TypeScript Fetch consumer -> OpenAPI provider -> Rust Axum implementation;
- Python pytest test -> OpenAPI provider -> Rust implementation;
- explicit generated-client metadata -> OpenAPI operations;
- Rust publisher -> AsyncAPI channel -> Python worker subscriber;
- GraphQL operation -> exact SDL root provider;
- generated Python gRPC client -> package-qualified RPC provider;
- Python literal SQL reader -> exact SQL migration table;
- repository -> Docker Compose deployment -> provided service;
- Markdown document -> repository-qualified Kubernetes service;
- persisted extractor batches exclude the fixture secret value;
- breaking event, GraphQL, and protobuf schema fixtures;
- unchanged batch/snapshot reuse and add/replace/delete affected-neighborhood relinking;
- exact CodeGraph corroboration without source payload persistence.

Unit fixtures cover every framework and package ecosystem listed above, malformed structured
inputs, dynamic values, ambiguity, deterministic ordering, bounded fingerprints, and unsafe path
rejection.

## Deliberate limits

Code System Graph does not build or persist complete syntax trees, call graphs, generated source, or
transitive package solver state. Source-derived HTTP facts expose method, normalized path, role,
implementation/test anchor, line range, confidence, and warnings. Compatibility reports classify
direct contract changes; impact analysis propagates supported graph risk and recommends relevant
tests.

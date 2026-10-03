# Supported Technologies

This guide explains what Code System Graph can discover in your repositories and which
cross-repository relationships `scan` can create from that evidence.

You do not select languages or frameworks in `code-system-graph.yaml`. Declare the repository
paths, run `scan`, and supported files and source patterns are detected automatically.

## What "supported" means

Code System Graph is a focused system-boundary scanner. It does not build a complete syntax or call
graph for every language. Instead, it finds evidence that helps connect repositories:

- contracts and the code that consumes, provides, implements, or tests them;
- event publishers, channels, and subscribers;
- database tables and code that reads or writes them;
- package dependencies shared across repositories;
- deployments, infrastructure resources, owners, documentation, and configuration keys.

A relationship is created automatically only when the target has one exact, unambiguous identity.
Dynamic values and duplicate candidates are reported as incomplete or ambiguous instead of being
guessed.

## Source languages and HTTP frameworks

| Language | HTTP clients | HTTP servers | Database access | Recognized tests |
| --- | --- | --- | --- | --- |
| TypeScript / JavaScript | Fetch, Axios | Express, Fastify, NestJS, Next.js App Router | Literal SQL | Jest, Vitest, Mocha, and Playwright `describe`/`it`/`test`; supertest, Playwright `request` |
| Python | requests, HTTPX, aiohttp, static method registries | FastAPI, Flask | psycopg/psycopg2, PyMySQL, SQLAlchemy ORM, Alembic, literal SQL | pytest, unittest, Factory Boy model links; `TestClient`, Flask `test_client()`, HTTPX with `app=` or an ASGI/WSGI transport |
| Go | `net/http` | `net/http`, Gin, Chi | Literal SQL | `TestX(t *testing.T)`; `httptest.NewRequest`, `httptest.NewServer` with `srv.URL` |
| Java | WebClient, Feign, RestTemplate | Spring MVC | Literal SQL | JUnit `@Test`, `@ParameterizedTest`, `@RepeatedTest`; MockMvc, RestAssured, `WebTestClient`, `TestRestTemplate` |
| Rust | Reqwest | Axum, Actix Web; advisory Utoipa/OpenAPI operations | SQLx, `mysql_async`, Diesel, literal SQL | Built-in, Tokio, Actix, and rstest tests; Axum `Request` builders sent with `oneshot`, Actix `test::TestRequest` |

Server routes carry the full path a request reaches, including router prefixes declared in other
files of the same repository:

| Framework | Composed prefixes |
| --- | --- |
| FastAPI | `APIRouter(prefix=)` and `include_router(router, prefix=)`, including imported routers |
| Flask | `Blueprint(url_prefix=)` and `register_blueprint(bp, url_prefix=)` |
| Express | `router.use(prefix, router)`, imported, default-exported, `module.exports`, and factory routers |
| NestJS | `@Controller(path)` and `app.setGlobalPrefix(prefix)` |
| Spring MVC and Feign | Class-level `@RequestMapping` and `@FeignClient(path=)` |
| Gin and Chi | `Group`, `Route`, `Mount`, `http.StripPrefix`, and router functions called with a group |
| Axum | `nest` and `merge` of local, returned, and module-path routers |
| Actix Web | `web::scope`, `service`, and `configure` of local and module-path services |

Route handlers are the function a route registration names, whatever it is called: the last
argument of `app.get("/x", auth, orders.list)` or `r.GET("/x", handlers.Get)`, unwrapped from a
single-argument wrapper such as `asyncHandler(orders.create)`, FastAPI `add_api_route` and Flask
`add_url_rule` endpoints, and the Spring method declared after a mapping annotation. An inline
handler function is identified by its method and path.

Prefix chains deeper than eight mounts, mount cycles, and routers that do not resolve to exactly
one declaration in the repository keep the route path declared next to the handler.

JavaScript and TypeScript are separate parser inputs but share the same focused framework
recognition. Each supported language is parsed structurally before framework-specific patterns are
considered; a matching word in a comment or string is not enough to prove a boundary.

## Cross-language boundary recognition

The HTTP table is not the complete support matrix. These boundaries are recognized independently
of the HTTP framework:

| Boundary | What source scanning recognizes | Contract or declaration |
| --- | --- | --- |
| gRPC | Exact RPC paths in generated client/server source with a recognized generator header | Protobuf services and methods |
| Events | Kafka, RabbitMQ, SNS/SQS, NATS/JetStream, Google Pub/Sub, and generic publish/subscribe calls | AsyncAPI 2.x and 3.x |
| GraphQL | Operations, embedded documents, persisted operations, and resolver patterns | GraphQL SDL and federation metadata |
| Database | Literal `SELECT`, `WITH`, `INSERT`, `UPDATE`, and `DELETE` statements, including SQLx, `mysql_async`, and PyMySQL/wrapper APIs | SQL DDL/migrations (including MariaDB dumps), standalone query files, Prisma, Alembic, SQLAlchemy, and Diesel |

The generated header and exact canonical RPC path are required for gRPC source links. A filename
that merely looks generated is not sufficient evidence.

## Contracts and relationships

### HTTP and OpenAPI

Supported contract inputs:

- OpenAPI 3.x;
- Swagger 2.0;
- OpenAPI Generator metadata with an explicit repository-relative `inputSpec`.

Distinct OpenAPI files whose names contain `openapi` are discovered recursively in ordinary source
and documentation directories. This permits public/private or backend/mock contracts in one
repository; competing formats for the same path stem remain ambiguous.

When methods and normalized paths match exactly, the graph can connect:

```text
TypeScript Fetch call
  -> consumes POST /orders
  -> OpenAPI operation in the API repository
  -> implemented by a Rust Axum handler
```

Tests with a supported HTTP target can also be connected to the contract they validate. A concrete
request path such as `/orders/42` reaches the `/orders/{id}` template whatever parameter syntax the
provider framework uses; the most specific template wins, and equally specific providers are
narrowed to the caller's repository before being reported as ambiguous.

Parameters embedded in a segment retain their literal constraints: `/files/{name}.json` matches
`/files/readme.json` and rejects `/files/readme.xml`. Static segments rank ahead of mixed segments,
then whole-segment parameters and catch-alls. Overlapping mixed templates remain ambiguous.

Client URLs are evaluated from the expression that builds them:

- constants and variables bound earlier in the same function or file, including `this.` and `self.`
  fields with one value;
- concatenation, Python f-strings, `%` and `.format`, Rust `format!` and `concat!`, JavaScript
  template literals, Go `fmt.Sprintf`, and Java `String.format`;
- conversions such as `str()`, `String()`, `encodeURIComponent`, `strconv.Itoa`, and
  `String.valueOf`.

A runtime value in the scheme or authority leaves the path exact and the authority unknown. A
runtime value that fills a whole path segment becomes a path parameter. Any other runtime value
makes the path dynamic, and the call is reported without a path.

A client call whose URL depends on a parameter of its function is a wrapper. Each call site that
binds the parameter, in the same repository and through relative imports, package imports, or
module paths, gains the instantiated call. Wrappers that forward the parameter to another wrapper
are followed for up to three hops. A test is linked to every exact client call made by the
functions it calls and the pytest fixtures it requests, including fixtures in an enclosing
`conftest.py`, for up to three calls deep. Calls that do not resolve to exactly one function in the
repository are not followed.

A test without a named function, such as a Jest or Playwright `it`/`test` callback, is identified by
its file, its `describe` chain, and its title, and it also reaches the `beforeEach`, `beforeAll`, and
Mocha `before` blocks of its enclosing suites. A request made through a parameter, such as
`client.get("/orders/42")` in a helper or a test that receives `client`, is attributed to an
in-process test client only when that parameter is a pytest fixture returning one, or receives one
from every caller that resolves; otherwise it is not reported.

In-process test clients (`TestClient`, Flask `test_client()`, supertest, `httptest`, MockMvc,
RestAssured, `WebTestClient`, `TestRestTemplate`, Axum `oneshot`, and Actix `test::TestRequest`)
exercise the application of their own repository, so their requests resolve only against providers
in that repository, even when another repository serves the same route.

Rust executable-route evidence and Utoipa/OpenAPI annotations are additive, not mutually
exclusive. An exact Actix or Axum declaration is confirmed executable evidence. A Utoipa operation
remains an independently searchable implementation candidate with advisory confidence, so a stale
documentation path cannot displace the code path while still contributing useful full-path and
contract context.

### Events and AsyncAPI

Supported contract and source inputs:

- AsyncAPI 2.x and 3.x in YAML or JSON;
- Kafka;
- RabbitMQ;
- AWS SNS and SQS;
- NATS and JetStream;
- Google Pub/Sub;
- generic publish and subscribe calls.

The graph can connect producers and consumers through a channel:

```text
Rust publisher -> publishes orders.created
AsyncAPI channel -> describes orders.created
Python worker -> subscribes to orders.created
```

Broker, namespace, and channel identity are used when available. Interpolated channel names and
ambiguous namespace-free matches remain unlinked.

### GraphQL

Supported inputs include:

- SDL types, fields, arguments, operations, and fragments;
- persisted-operation manifests;
- resolvers and embedded documents in the supported source languages;
- federation metadata.

An exact root field can connect a client operation to its schema provider and, within the provider
repository, to a resolver implementation. Code System Graph does not execute schemas or fetch
remote schemas.

### Protobuf and gRPC

Supported inputs include proto2, proto3, and editions declarations, including packages, messages,
enums, services, RPC methods, imports, streaming modes, and field compatibility information.

Recognized generated-code markers can connect a generated client or server to one exact
package-qualified RPC provider. A generated-looking filename by itself is not treated as evidence,
and Code System Graph does not run `protoc`.

## Databases and data models

Supported declarations:

- SQL DDL and migration tables, columns, indexes, and foreign keys;
- migration artifacts even when a dialect-specific or destructive statement yields no recoverable
  table declaration, with exact `down -> reverts -> up` pairs and numeric
  `previous -> precedes -> next` lineage inside each migration directory;
- standalone SQL query files;
- Prisma models and mappings;
- Alembic operations;
- SQLAlchemy model and table bindings;
- Diesel table declarations.
- MariaDB/MySQL dumps with bounded recovery around dialect-specific administrative statements.

Literal `SELECT`, `WITH`, `INSERT`, `UPDATE`, and `DELETE` statements are recognized across the
supported source languages. If one normalized table identity matches, the enclosing code is linked
as a reader or writer:

```text
orders-service repository: INSERT INTO orders -> writes to table orders
worker repository: SELECT ... FROM orders -> reads from table orders
```

Query bodies and literal values are discarded before persistence. Dynamic, concatenated, or
interpolated table names remain incomplete.

### Python database and factory relationships

psycopg/psycopg2 and PyMySQL imports and direct `execute` calls are recognized together with
project wrappers such as `Database.sql(...)`. Python f-strings and `.format(...)` calls may retain
a table relationship only when interpolation is confined to values; an interpolated table
identifier remains incomplete and unlinked. SQLAlchemy `query`, `add`, `update`, and `delete`
accesses are linked through an exact model-to-`__tablename__` binding; table names are not guessed
from class names. All exact accesses converge on the same table nodes used by Rust SQLx and
`mysql_async`.

Factory Boy classes with a literal `Meta.model` assignment create a direct factory-to-model
relationship. The imported model module is resolved to its repository-relative Python path when
the `from ... import ...` declaration is static. Arbitrary factory execution and dynamically
selected models are not inferred.

### SQLx

[SQLx](https://github.com/transact-rs/sqlx) is recognized structurally in Rust source:

- runtime `query`, `query_as`, `query_scalar`, and `raw_sql` functions, including their argument
  variants;
- checked and unchecked `query*!`, `query_as*!`, and `query_scalar*!` macros;
- all checked and unchecked `query_file*!`, `query_file_as*!`, and `query_file_scalar*!` macros;
- `QueryBuilder::new` and `QueryBuilder::with_arguments`, with dynamic fragments reported as
  incomplete;
- embedded `migrate!`, `sqlx.toml` migration-directory overrides, and runtime `Migrator::new`
  references;
- direct imports and explicit import aliases;
- Cargo dependency features and backend selection through the normal Cargo manifest extractor.

SQLx file and migration paths are resolved relative to the nearest Cargo crate root, matching
SQLx's `CARGO_MANIFEST_DIR` behavior. An exact query-file reference links the calling Rust symbol
to both the SQL artifact and every uniquely declared table read or written by that query. Query
file or `sqlx.toml` changes are incrementally relinked even when the Rust call site is unchanged.

Computed SQL and dynamic `QueryBuilder` fragments remain incomplete. Derives such as `FromRow` and
`Type` do not imply a table by themselves, so they are not promoted to table relationships.

### mysql_async

[`mysql_async`](https://github.com/blackbeam/mysql_async) is recognized structurally in Rust
source across both public query styles:

- `Queryable::query*`, `Queryable::exec*`, `query_stream`, `exec_stream`, and `prep`;
- fluent `Query::{run, first, fetch, reduce, map, stream, ignore}`;
- prepared parameters through `WithParams::with` and batch execution through
  `BatchQuery::batch`;
- prelude glob imports, direct trait imports, explicit trait aliases, qualified calls, ordinary
  strings, raw strings, byte strings, and raw byte strings.

Literal reads and writes become the same normalized database-table relationships as SQLx and
literal SQL in Python, Java, Go, TypeScript, or JavaScript. Those table nodes are workspace-global,
so exact identities can connect consumers across repositories. Agent-facing MCP summaries
attribute a table definition to a repository only when confirmed containment yields one owner and
include both usage and definition evidence; competing definitions remain explicitly ambiguous and
are not reported as a proven cross-repository boundary. Dynamic statement variables remain
incomplete; preparing or constructing a literal statement is retained as direct evidence even when
execution happens through a later statement handle.

## Packages

| Ecosystem | Recognized inputs |
| --- | --- |
| npm, pnpm, Yarn | `package.json`, npm/pnpm/Yarn lockfiles, workspaces, exports, dependency scopes |
| Python and Poetry | `pyproject.toml`, requirements files, `poetry.lock`, groups, markers, optional dependencies |
| Cargo | Packages, workspaces, scopes, target conditions, features, renames, lockfile metadata |
| Go | Modules, workspaces, requirements, replacements, indirect conditions |
| Maven and Gradle | Literal coordinates, scopes/configurations, optional and declared condition metadata |
| NuGet and MSBuild | `packages.config` and SDK-style `PackageReference` declarations |

An external dependency becomes a cross-repository dependency only when its exact package identity
matches a package owned by another declared repository.

Agent-facing MCP summaries resolve consuming and owning repositories through confirmed manifest
containment. If the same package coordinate is owned by more than one repository, the summary
reports ambiguous ownership instead of selecting a repository.

Literal Cargo workspace members also connect the workspace manifest to each member
`Cargo.toml`. Member manifests then contain their package coordinates, while local path
dependencies connect those package nodes. Globbed member declarations remain unexpanded unless an
exact member manifest is independently discovered.

## Infrastructure, ownership, and documentation

| Area | Recognized inputs and relationships |
| --- | --- |
| Docker Compose | Services, images, ports, dependencies, environment key names |
| Kubernetes | Workloads, Services, Ingresses, static resources, selectors, Secret key names |
| Helm | `Chart.yaml`, values, and YAML or `.tpl` templates of a chart; templated identities remain incomplete |
| Terraform / OpenTofu | Literal resources and dependency metadata |
| Documentation | README, runbook, RFC, ADR, headings, and explicit links |
| Ownership | Ordered CODEOWNERS rules and explicit owner identities |
| Service catalogs | Declared services, owners, contracts, and dependencies in YAML or JSON |
| Configuration | Dotenv, YAML, JSON, and TOML key names and scopes, without values |

These inputs can create repository-to-deployment, deployment-to-service, resource-dependency,
document, ownership, and configuration-key relationships. Documentation prose alone never creates
a system relationship.

## What is automatic and what may need configuration?

Automatic during `scan`:

- language, framework, manifest, contract, and infrastructure detection;
- exact linking across every repository declared in the workspace;
- reuse of unchanged extraction results on later scans;
- conservative warnings for incomplete or ambiguous evidence.

Optional configuration:

- `openapi` when the provider contract is outside the automatically detected root locations;
- `httpConsumers`, `integrationTests`, or `implementations` for real boundaries that focused
  source recognition cannot observe;
- `manualLinks` as a documented last resort for a real relationship that cannot be represented by
  automatic evidence;
- CodeGraph enrichment for repository-local symbol context.

See [Configuration](CONFIGURATION.md) for precedence, examples, and safety rules.

## Deliberate limits

Code System Graph does not:

- infer dynamic routes, topics, SQL, package coordinates, or generated-code roles;
- choose between equally specific providers that no scope rule separates;
- execute application code, GraphQL schemas, Terraform, Helm, or `protoc`;
- persist source bodies, SQL literal values, credentials, or configuration values;
- replace a repository-local symbol and call graph.

These limits are intentional: the graph preserves evidence and uncertainty rather than inventing a
clean but misleading system map.

Maintainers and contributors can find parser-level behavior, incremental semantics, fixtures, and
validation details in [Extractor coverage](EXTRACTOR_COVERAGE.md).

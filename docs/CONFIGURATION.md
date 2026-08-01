# Configuration

Code System Graph uses one strict YAML manifest named `code-system-graph.yaml`. Unknown fields are
rejected instead of being silently ignored.

## Minimal configuration

```yaml
version: 1
name: commerce
repos:
  web:
    path: ./web
  orders-service:
    path: ./orders-service
```

Required fields:

| Field | Meaning |
| --- | --- |
| `version` | Manifest schema version; currently `1` |
| `name` | Stable workspace name used by CLI and MCP |
| `repos` | At least one unique repository alias |
| `repos.<alias>.path` | Repository path, resolved from the manifest directory |

Repository aliases should describe stable system roles (`web`, `orders-api`, `worker`), not
temporary branch names or local folder names.

## What scan discovers automatically

Within every declared repository, `scan` looks for supported:

- package manifests and workspace metadata;
- OpenAPI/Swagger, AsyncAPI, GraphQL, and protobuf/gRPC contracts;
- focused HTTP, event, GraphQL, SQL, test, and implementation patterns;
- SQL migrations and supported data-model declarations;
- Docker Compose, Kubernetes, Helm, Terraform, and OpenTofu metadata;
- Markdown, ADRs, CODEOWNERS, service catalogs, and configuration key names.

You do not configure languages or frameworks. Dynamic or ambiguous values remain incomplete rather
than being guessed. Start with [Supported technologies](SUPPORTED_TECHNOLOGIES.md) for the
user-facing language, framework, and relationship matrix. Parser-level validation details are in
[Extractor coverage](EXTRACTOR_COVERAGE.md).

## Optional repository fields

```yaml
version: 1
name: commerce
repos:
  orders-service:
    path: ./orders-service
    openapi: ./contracts/openapi.yaml
    excludes:
      - coverage/**
      - "**/generated/**"
    includeDefaults:
      - vendor/internal-sdk/**
    httpConsumers:
      - method: POST
        path: /orders
        source: src/legacy-client.ts
```

| Field | Use it when |
| --- | --- |
| `openapi` | The provider contract is not at a location discovered automatically |
| `httpConsumers` | A real HTTP consumer cannot yet be observed from supported source patterns |
| `integrationTests` | A cross-language test-to-contract relation needs an explicit declaration |
| `implementations` | A contract implementation anchor cannot be linked exactly from source |
| `excludes` | Additional repository-relative paths must be omitted from automatic discovery |
| `includeDefaults` | A specific path inside a default dependency or build exclusion must be discovered |

These fields add explicit evidence. They are not required for supported, unambiguous source
patterns.

## Discovery exclusions

`excludes` and `includeDefaults` accept repository-relative globs with `*`, `?`, and `**`. The
recursive `**` wildcard must occupy a complete path component. Character classes, alternations,
and other glob syntax are rejected. Use `/` as the separator on every operating system.

Patterns are canonicalized before matching, reporting, deduplication, and fingerprinting. Leading
or internal `.` components and repeated `/` separators are removed, so `./coverage//**` is reported
and evaluated as `coverage/**`. A terminal `/` remains directory-specific. Absolute paths, parent
traversal with `..`, patterns without a path component, malformed globs, and unsafe terminal
characters are rejected.

Ordinary patterns match both files and directories. For example, `generated/*` prunes a
`generated/output` directory and everything below it. Add a terminal `/` only when the pattern
must match a directory and not a file with the same repository-relative path.

Built-in protected exclusions cover `.git`, `.hg`, `.svn`, `.codegraph`, and
`.code-system-graph`. They prevent version-control metadata, local indexes, and generated graph
state from entering discovery and cannot be re-enabled. Reactivable defaults cover common
dependency, environment, cache, and build trees including `node_modules`, `vendor`, `target`,
`dist`, `build`, Python virtual environments and tool caches, `.next`, and `__pycache__`.

`includeDefaults` reopens only matching paths inside reactivable defaults. An explicit `excludes`
match still wins. Explicit `openapi`, `httpConsumers`, `integrationTests`, and `implementations`
artifacts remain authoritative and observable even when their containing tree is excluded from
automatic discovery.

Inspect the complete effective policy, including rules not present in YAML, without creating or
opening a database:

```bash
csgraph config show --config code-system-graph.yaml
csgraph config show --config code-system-graph.yaml --repo orders-service
```

The JSON result reports protected defaults, reactivable defaults, configured values and their
source, and effective rules in precedence order. This policy applies to native scanning, OpenAPI
auto-detection, and `sync --watch`; the independent CodeGraph executable manages its own files.

## Repository-local configuration and precedence

A repository may keep its optional boundary declarations in `.code-system-graph.yaml` at the
checkout root:

```yaml
version: 1
openapi: ./contracts/openapi.yaml
excludes:
  - coverage/**
```

Effective values use this precedence, from highest to lowest:

1. an explicit CLI override such as `--repo-openapi orders-service=contracts/openapi.yaml`;
2. the repository entry in the workspace manifest;
3. the repository's local `.code-system-graph.yaml`;
4. deterministic auto-detection;
5. safe empty defaults.

OpenAPI auto-detection recognizes exactly one of `openapi.yaml`, `openapi.yml`, or `openapi.json` at
the repository root. If more than one exists and no higher-precedence value selects one, the scan
reports ambiguity instead of guessing.

## Paths and allowed roots

Relative repository paths are resolved from the manifest directory. Canonical repository paths
must stay under the manifest root unless explicitly allowlisted:

```yaml
version: 1
name: commerce
allowedRoots:
  - ../shared-services
repos:
  orders-service:
    path: ./orders-service
  identity:
    path: ../shared-services/identity
```

Use the narrowest root that contains the intended checkout. `allowedRoots` is a filesystem safety
boundary, not a discovery mechanism.

## Add or remove a repository safely

The CLI preserves unrelated manifest content and creates a backup when it changes an existing
file:

```bash
csgraph repo add worker ./worker --config code-system-graph.yaml
csgraph repo remove worker --config code-system-graph.yaml --dry-run
csgraph repo remove worker --config code-system-graph.yaml --yes
```

After a manifest change, run one-shot `scan` or `sync`. An active `sync --watch` process detects the
manifest change and publishes automatically; without watch mode, no background refresh occurs.

## Manual links

Use `manualLinks` only when a real relationship cannot be represented by automatic evidence:

```yaml
version: 1
name: commerce
repos:
  web:
    path: ./web
  orders-service:
    path: ./orders-service
manualLinks:
  - from: service:web
    to: service:orders-service
    relation: consumes
    contract: POST /orders
    reason: Legacy gateway rewrites this route before it reaches the orders service
```

Each endpoint must resolve to one exact node ID or stable key. `reason` is required so the override
can be reviewed later. Set `suppress: true` only to remove one exact automatic relationship with a
documented reason.

Manual links are versioned configuration, not a substitute for checking coverage warnings.

## Database location

The database path is a CLI or server argument; it is not stored in the manifest:

```bash
mkdir -p .code-system-graph
csgraph scan --config code-system-graph.yaml --database .code-system-graph/code-system-graph.db
```

Use the same path for `scan`, `status`, queries, and the MCP server.

| Workspace shape | Manifest and database location | Ignore rule |
| --- | --- | --- |
| One repository | Inside that repository root | If it is a Git worktree, `init` adds `.code-system-graph/` to its `.gitignore` |
| Multiple sibling repositories | In their common parent, outside each repository | No rule is needed when the parent is unversioned; if the parent is tracked, `init` adds one there |

The local database should never be committed. When the workspace belongs to a Git worktree,
`csgraph init` preserves existing ignore content and adds this rule once:

```gitignore
.code-system-graph/
```

## Optional features

### CodeGraph

[CodeGraph](https://github.com/colbymchenry/codegraph) is an independent MIT-licensed project by
Colby Mchenry. It is not included with Code System Graph and must be installed separately.

Enable repository-local enrichment explicitly:

```bash
csgraph scan \
  --codegraph \
  --config code-system-graph.yaml \
  --database .code-system-graph/code-system-graph.db
```

For MCP:

```bash
csgraph mcp \
  --codegraph \
  --workspace commerce \
  --database .code-system-graph/code-system-graph.db
```

CodeGraph must already be installed and each repository must already have its own CodeGraph index.
There is no CodeGraph field in the workspace manifest. Initialize each declared repository once
with `codegraph init <repository-path>`. Normal scans and server processes do not create or update
those indexes. The explicit sync workflow updates initialized indexes through CodeGraph's public
CLI before publishing the federated graph:

```bash
csgraph sync \
  --config code-system-graph.yaml \
  --database .code-system-graph/code-system-graph.db
```

Repositories without an initialized index are reported as skipped and native extraction continues.
See [CodeGraph integration](CODEGRAPH_INTEGRATION.md).

### Remote pull requests

GitHub and Bitbucket Cloud are disabled by default. They require provider enablement and consent on
each request. Tokens are optional for public repositories and are read only from explicitly named
environment variables. See [Change and pull-request analysis](CHANGES.md).

### Administrative MCP tools

The default MCP profile is read-only. Start with `--admin` only when a trusted client needs bounded
workspace mutations:

```bash
csgraph mcp \
  --admin \
  --workspace commerce \
  --database .code-system-graph/code-system-graph.db
```

### HTTP

HTTP never starts automatically. `csgraph serve` binds to `127.0.0.1:4767` by default.
Non-loopback access requires bearer authentication. See [Delivery interfaces](INTERFACES.md).

## Environment variables

Most users do not need environment variables.

| Variable | Purpose |
| --- | --- |
| `CODE_SYSTEM_GRAPH_DEBUG=1` | Add bounded diagnostics on stderr |
| `CODE_SYSTEM_GRAPH_CODEGRAPH=1` | Enable CodeGraph for MCP/HTTP server startup |
| `CODE_SYSTEM_GRAPH_CODEGRAPH_BINARY` | Select and enable a trusted CodeGraph executable |
| `GITHUB_TOKEN` | Optional MCP GitHub pull-request credential |
| `BITBUCKET_TOKEN` | Optional MCP Bitbucket Cloud credential |
| `BITBUCKET_USER` | Atlassian email for Bitbucket user API-token authentication |

Do not put secret values in `code-system-graph.yaml`.

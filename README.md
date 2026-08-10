<div align="center">

# Code System Graph

**Open-source cross-repository code intelligence, dependency mapping, and impact analysis for AI
coding agents**

Map APIs, events, schemas, packages, databases, and ownership across repositories before a change
breaks another service.

[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
![Source version](https://img.shields.io/badge/source-v1.1.0-orange.svg)
[![crates.io](https://img.shields.io/crates/v/code-system-graph.svg)](https://crates.io/crates/code-system-graph)
![Platforms](https://img.shields.io/badge/validated-Linux%20%7C%20macOS%20%7C%20Windows-1793d1.svg)
![Privacy](https://img.shields.io/badge/privacy-local%20%7C%20no%20telemetry-2ea44f.svg)
![Agents](https://img.shields.io/badge/agents-5%20supported-7c3aed.svg)

Claude Code | Codex | Gemini CLI | Antigravity | Cursor

[**Get started**](#get-started) |
[**Supported technologies**](docs/SUPPORTED_TECHNOLOGIES.md) |
[**Connect an agent**](docs/AGENT_SETUP.md) |
[**Documentation**](#documentation)

</div>

Code System Graph is a local-first static analysis and system architecture tool that builds an
evidence-backed dependency graph across repositories. It connects APIs, events, schemas, packages,
databases, deployments, tests, owners, and documentation, then exposes that context to AI coding
agents through the Model Context Protocol (MCP). Agents can answer system-level questions that are
difficult to solve by searching one repository at a time.

## What problem does it solve?

Your repositories already describe how the system works, but the evidence is scattered:

- a frontend calls an endpoint declared in another repository;
- an API publishes an event consumed by a worker;
- several services read the same table or depend on the same package;
- a deployment, test, owner, or ADR explains a boundary somewhere else.

Code System Graph connects those facts into one federated graph and keeps the evidence, confidence,
freshness, and coverage behind every answer. It helps answer:

- **Impact:** "If I change this API, event, schema, or package, what else may be affected?"
- **Architecture:** "How does checkout travel from the web app to the API and worker?"
- **Ownership:** "Which repositories and owners are involved in this capability?"
- **Change review:** "Which tests matter, and do these pull requests overlap?"
- **Unknowns:** "Where is the evidence stale, ambiguous, or incomplete?"

It is deliberately conservative: missing evidence is reported as unknown, not safe.

### Code System Graph and CodeGraph

[CodeGraph](https://github.com/colbymchenry/codegraph) is an independent open-source project by
Colby Mchenry, distributed under the MIT License. It is not part of, maintained by, or distributed
with Code System Graph. The two projects solve different levels of the same problem:

| Question | Best source |
| --- | --- |
| Symbols, callers, callees, and implementation flow inside one repository | [CodeGraph](https://github.com/colbymchenry/codegraph), optional |
| Contracts, dependencies, ownership, and impact across repositories | Code System Graph |
| One end-to-end answer with both levels | Code System Graph with optional CodeGraph enrichment |

CodeGraph does not add its private nodes or relationships to the persisted Code System Graph.
When enabled, it can add evidence to existing implementation links and provides bounded,
request-time `explore` context and local impact. Without it, the federated graph remains available,
but repository-local implementation detail does not.

Code System Graph is useful on its own. It does not install, initialize, or read CodeGraph's private
database. See the [CodeGraph integration notes](docs/CODEGRAPH_INTEGRATION.md) for the supported
public interface and attribution details.

## What does `scan` connect?

`scan` examines every repository declared in the workspace and joins matching evidence into one
graph:

| Evidence found in one or more repositories | Relationships added when the match is exact |
| --- | --- |
| HTTP client call, OpenAPI operation, and server route | Client **calls** contract; server **implements** contract |
| Event publisher, AsyncAPI channel, and subscriber | Producer **publishes**; consumer **subscribes**; channel **delivers to** consumer |
| GraphQL operation, schema field, and resolver | Client **consumes** field; schema **provides** field; resolver **implements** it |
| Generated gRPC marker, protobuf service, and server marker | Client **calls** RPC; service **provides** it; server **implements** it |
| Literal SQL statement and one declared table | Code **reads from** or **writes to** the table |
| Package, deployment, documentation, ownership, and config declarations | Dependency, deploys/provides, documents, owns, and config-key links |

It does not guess through dynamic routes, interpolated table names, duplicate providers, or prose.
Those cases remain incomplete or ambiguous so an agent cannot mistake missing evidence for safety.

## Supported languages and frameworks

Source scanning currently recognizes these focused framework and language patterns:

| Language | HTTP clients | HTTP servers | Database access | Recognized tests |
| --- | --- | --- | --- | --- |
| TypeScript / JavaScript | Fetch, Axios | Express, Fastify, NestJS, Next.js App Router | Literal SQL | Not recognized |
| Python | requests, HTTPX, aiohttp, static method registries | FastAPI, Flask | psycopg/psycopg2, PyMySQL, SQLAlchemy, Alembic, literal SQL | pytest, unittest, Factory Boy model links |
| Go | `net/http` | `net/http`, Gin, Chi | Literal SQL | Not recognized |
| Java | WebClient, Feign | Spring MVC | Literal SQL | Not recognized |
| Rust | Reqwest | Axum, Actix Web; advisory Utoipa/OpenAPI operations | SQLx, `mysql_async`, Diesel, literal SQL | Built-in tests, Tokio tests, rstest |

Other boundary support is shared across these languages rather than tied to one web framework:

- **gRPC:** protobuf services and exact generated client/server markers;
- **events:** AsyncAPI plus Kafka, RabbitMQ, SNS/SQS, NATS/JetStream, Google Pub/Sub, and generic
  publish/subscribe calls;
- **GraphQL:** SDL, operations, persisted operations, federation, and resolver patterns;
- **data:** language-specific access above plus Prisma schemas, standalone query files, and SQL
  migrations. Exact table identities are shared across languages and repositories, so readers and
  writers implemented with different clients converge on the same table node.

Executable Rust routes and Utoipa/OpenAPI annotations coexist as separate evidence. Exact
Actix/Axum declarations have greater authority; documentation-oriented annotations remain
searchable and linkable at lower confidence because they can lag behind runtime registration.

Supporting files also add Docker Compose, Kubernetes, Helm, Terraform/OpenTofu, common package
ecosystems, Markdown, CODEOWNERS, service catalogs, and configuration key names.

See [Supported technologies](docs/SUPPORTED_TECHNOLOGIES.md) for what is extracted, concrete
cross-repository examples, automatic linking rules, and known limits. The implementation-level
validation matrix remains in [Extractor coverage](docs/EXTRACTOR_COVERAGE.md).

## Get started

### 1. Install

Prebuilt binaries are available for Linux x86_64/ARM64, macOS x86_64/ARM64, and Windows x86_64.
See [Installation](docs/INSTALLATION.md) for platform status, verification, upgrades, and
uninstall.

**Recommended: prebuilt binaries with cargo-binstall** (no Rust compiler required):

```bash
cargo binstall code-system-graph code-system-graph-hooks
```

Requires Cargo and [cargo-binstall](https://github.com/cargo-bins/cargo-binstall). Both binaries are
installed to Cargo's binary directory, normally `$HOME/.cargo/bin`.

The crates.io package is `code-system-graph`; the user-facing CLI command is `csgraph` (not
`code-system-graph`). The hooks runtime installs as `code-system-graph-hooks`.

**From crates.io** (builds locally; requires Rust 1.97.1 or newer):

```bash
cargo install code-system-graph code-system-graph-hooks
```

**From a trusted checkout** (for development or unreleased changes):

```bash
cargo +stable install --locked --path crates/code-system-graph-cli && cargo +stable install --locked --path crates/code-system-graph-hooks
```

Verify either path:

```bash
csgraph --version
command -v code-system-graph-hooks
```

`code-system-graph-hooks` is an internal runtime for optional agent routing hooks, not a
user-facing CLI. Installing both avoids a later partial setup.

### 2. Describe the repositories in your system

For multiple sibling repositories, use their parent directory as the Code System Graph workspace:

```text
my-project/
|-- code-system-graph.yaml
|-- .code-system-graph/
|   `-- code-system-graph.db
|-- repo_1/
`-- repo_2/
```

Run `csgraph` commands from `my-project/`. Keep both the manifest and database in that parent, not
inside `repo_1` or `repo_2`. Declare each repository explicitly:

```yaml
version: 1
name: my-project
repos:
  repo_1:
    path: ./repo_1
  repo_2:
    path: ./repo_2
```

Only `version`, `name`, `repos`, and each repository's `path` are required. Code System Graph
discovers supported contracts and boundaries under those paths; you do not list every file,
framework, or dependency.

`csgraph init` does not discover sibling repositories. If you use it in `my-project/`, it creates
one `root` entry with `path: .`; replace that entry with `repo_1` and `repo_2` before the first
scan. You can also create the short YAML above directly.

For a workspace containing only one repository, run `init` inside that repository:

```bash
cd /path/to/my-repository
csgraph init . --name my-repository
```

`init` creates the manifest without overwriting an existing one. If the workspace directory belongs
to a Git worktree, it also preserves the existing `.gitignore` and adds `.code-system-graph/` if
needed. It does not create `.gitignore` when an unversioned parent only contains child repositories.
Commit `code-system-graph.yaml` and any generated `.gitignore` update when the workspace definition
is ready to share.

`init` does **not** scan, create the database, detect sibling repositories, or configure an agent.

### 3. Build the system graph

Run this from the directory containing `code-system-graph.yaml`:

```bash
mkdir -p .code-system-graph && csgraph scan --config code-system-graph.yaml --database .code-system-graph/code-system-graph.db
```

The first scan discovers supported boundaries and publishes an atomic SQLite snapshot. Later scans
reuse unchanged extraction results.

### 4. Add repository-level CodeGraph context (optional)

Install the independent CodeGraph CLI from its official repository:

```bash
curl -fsSL https://raw.githubusercontent.com/colbymchenry/codegraph/main/install.sh | sh
codegraph --version
```

Initialize it once in every repository declared in `code-system-graph.yaml`:

```bash
codegraph init ./repo_1
codegraph init ./repo_2
```

Each command creates a local `.codegraph/` index inside that repository. Then one Code System Graph
command keeps the initialized CodeGraph indexes and the federated graph current:

```bash
csgraph sync --config code-system-graph.yaml --database .code-system-graph/code-system-graph.db
```

Run `sync` after relevant changes or use `sync --watch` during a development session. Repositories
without `.codegraph/` are skipped; `--no-codegraph` updates only Code System Graph.

Verify both layers:

```bash
csgraph status --config code-system-graph.yaml --database .code-system-graph/code-system-graph.db
codegraph status ./repo_1
codegraph status ./repo_2
```

Code System Graph never reads CodeGraph's private database. It uses CodeGraph's public CLI and MCP
interfaces. See [Use CodeGraph with a workspace](docs/CODEGRAPH_INTEGRATION.md) for Windows, npm,
custom binary, watch mode, and troubleshooting instructions.

### 5. Connect your coding agent

Code System Graph exposes a local MCP server. For Codex:

```bash
codex mcp add code-system-graph -- \
  csgraph mcp \
  --config /absolute/path/to/my-project/code-system-graph.yaml \
  --codegraph \
  --workspace my-project \
  --database /absolute/path/to/my-project/.code-system-graph/code-system-graph.db
```

MCP tools return one bounded Markdown text block and resources use `text/markdown`. HTTP and CLI
retain typed schema-v2 JSON. Explore is the only source-bearing response and combines ephemeral
source, symbols, callers/callees, federated evidence handoffs, coverage, and grounded next actions.

For clients that support [Agent Plugins](https://agent-plugins.org/), generate one portable,
workspace-bound package plus its ignored local binding instead of configuring MCP and routing
guidance separately:

```bash
csgraph plugin create \
  --output ./code-system-graph-agent-plugin \
  --config code-system-graph.yaml \
  --database .code-system-graph/code-system-graph.db \
  --codegraph
```

The visible `crates/code-system-graph-hooks/agent-integration-template/` directory is the sole
source for editable agent installation, skill, discovery, and activation content. `agent-plugin/`
owns the portable manifest, MCP declaration, canonical skill, operating guide, optional client
metadata, license, validation schemas, and
`/.local/` rule. `native-hooks/` owns classifier signals, dynamic selector guidance, static
fallback rules, strict-gate shell, limitations, and host UI text. Complete-plugin mode emits only
the portable core; client metadata is used only while composing into a plugin that already targets
that client. Rust includes and renders these files and contains no second policy or prompt body.

The manifest, MCP declaration, skill, and guide contain no machine paths and can be versioned. Only
`.local/code-system-graph/mcp-binding.json` records the current developer's manifest, database,
plugin root, optional CodeGraph executable, and auditable generator build identity: version, exact
executable fingerprint, and build-time commit/dirty state when available. Do not commit `.local/`.
Other developers recreate their binding after cloning by running `plugin create --output
<plugin>` with that plugin's `--mcp-server-name` and `--routing-skill`.

If a team already distributes a portable plugin, keep its MCP declarations versioned and write only
the developer-local workspace binding under the ignored `.local/` tree:

```bash
csgraph plugin create \
  --output ./team-agent-plugin \
  --mcp-server-name team-code-system-graph \
  --routing-skill team-system-graph \
  --config code-system-graph.yaml \
  --database .code-system-graph/code-system-graph.db \
  --codegraph
```

The portable and Codex MCP entries must invoke `csgraph mcp --binding
${PLUGIN_ROOT}/.local/code-system-graph/mcp-binding.json`; `create` validates those entries, the
existing routing skill, and the `/.local/` ignore rule without rewriting any of them. Install the
plugin root. Re-run with `--replace-generated` after local paths change; only a binding carrying
Code System Graph's recognized ownership identity can be replaced. Until local binding creation
runs, that MCP entry fails visibly while independent plugin skills and servers remain usable.

The generated MCP is read-only and requires `csgraph 1.1.0` in `PATH`; it does not bundle binaries.
Its plugin, server, and skill share a stable name derived from the declared workspace name, so
clones produce the same versioned files. Install it project-locally for workspace-only activation;
the generated skill also requires the nearest manifest and MCP `status` to report that workspace.
For Claude Code, Codex, and Gemini, an optional native prompt hook can improve activation by asking
the client to use the installed skill. The hook stays brief; the skill remains the only detailed
procedure. Do not combine the skill with an always-on project rule.

Use the same generated Agent Skill for every supported client. Do not copy its policy into a global
`AGENTS.md`, Cursor rule, or another always-on instruction. Agent Plugins clients load the packaged
skill directly; other clients need only a small native manifest/MCP adapter or, as a compatibility
fallback, one manual MCP registration plus the same skill. Agent Plugins does not define a single
cross-vendor marketplace.

`--codegraph` exposes bounded repository-local `explore` context through Code System Graph. Omit it
when CodeGraph is not installed; `explore` then remains unavailable while the federated tools keep
working.

Claude Code, Codex, Gemini CLI, Antigravity, and Cursor are supported. Codex and Cursor can consume
the packaged Agent Plugin; the other clients currently use their native MCP/plugin format with the
same Agent Skill. Native prompt hooks can complement skill activation in Claude Code, Codex, and
Gemini. Static Cursor and Antigravity rules remain fallback mechanisms when a skill cannot load.
Follow
[Connect an agent](docs/AGENT_SETUP.md) for exact commands, configuration files, verification, and
limitations.

For Cursor, keep the entry in `<workspace>/.cursor/mcp.json`; an absolute database path does not
make a global MCP entry workspace-scoped. Reload Cursor and enable the server after changing the
file. `csgraph mcp --codegraph` starts CodeGraph itself, so a separate `codegraph install` entry is
unnecessary.

You can now ask the agent:

```text
Which repositories depend on the orders API, and what evidence connects them?
Trace checkout from the web client to the event consumer.
What could be affected if POST /orders changes?
Which tests and owners should be involved in this change?
Where is coverage incomplete or stale?
```

## What is automatic?

| Behavior | Default |
| --- | --- |
| Discover supported contracts, package manifests, source boundaries, tests, infrastructure, ownership, and docs inside declared repositories | Automatic during `scan` |
| Link exact, evidence-backed relationships across declared repositories | Automatic during `scan` |
| Reuse unchanged extractor results on later scans | Automatic |
| Ignore generated database and hook state when the workspace belongs to a Git worktree | Automatic during `init` |
| Keep source bodies and secret values out of the persisted graph | Automatic |
| Start network services or access GitHub/Bitbucket | Never automatic |
| Detect sibling repositories or choose workspace aliases | Manual manifest configuration |
| Publish updates after files or the manifest change | Manual with one-shot `scan` or `sync`; automatic while `sync --watch` is running |
| Register the MCP server with an agent | One explicit agent-specific command or config |
| Install native routing | Optional skill selector for Claude/Codex/Gemini; static fallback for Cursor/Antigravity |
| Enable CodeGraph enrichment | Optional |

## Configuration at a glance

### Required

- one or more local repository directories;
- a `code-system-graph.yaml` manifest with a workspace name and repository paths;
- a writable path for the embedded SQLite database;
- Linux x86_64/ARM64, macOS x86_64/ARM64, or Windows x86_64 for a prebuilt installation.

### Optional

- explicit OpenAPI paths or manual links when automatic evidence is insufficient;
- repository-specific `excludes`, `includeDefaults`, and opt-in `useGitignore` discovery policy;
- Git for local change, revision, and strict pre-commit analysis;
- [CodeGraph](docs/CODEGRAPH_INTEGRATION.md) for repository-local source and symbol context;
- GitHub or Bitbucket Cloud access for pull-request analysis;
- agent routing hooks and a strict pre-commit gate;
- the HTTP server or administrative MCP tools.

No external database, API key, cloud account, or network connection is required for local scan,
query, trace, impact, status, CLI, or MCP workflows.

See [Configuration](docs/CONFIGURATION.md) for all manifest fields and safe defaults.
Use `csgraph config show --config code-system-graph.yaml` to inspect configured and implicit
discovery rules without opening a database.

## Common use cases

### Before changing a contract

Search for the contract, select its stable node ID, and inspect upstream impact:

```bash
csgraph query "POST /orders" --config code-system-graph.yaml --workspace my-project --database .code-system-graph/code-system-graph.db
csgraph impact --target <node-id> --workspace my-project --database .code-system-graph/code-system-graph.db
```

### Trace a flow across repositories

```bash
csgraph trace --from <source-node-id> --to <target-node-id> --workspace my-project --database .code-system-graph/code-system-graph.db
```

### Review local work before committing

```bash
csgraph changes --repo repo_1 --scope staged --workspace my-project --database .code-system-graph/code-system-graph.db
```

Code System Graph reports evidence and recommended tests; it never stages files, runs tests,
commits, pushes, or merges.

For a complete first workspace, including multi-repository layout and expected outputs, follow the
[Getting started guide](docs/GETTING_STARTED.md).

## Privacy and safety

- Local workflows run without telemetry or required network access.
- Source bodies and secret values are not persisted.
- An MCP host may retain requests and responses, including ephemeral Explore source, in its own
  conversation history; configure host retention separately.
- SQLite snapshots are local and atomically replaced.
- Remote pull-request access is disabled by default and requires explicit enablement and consent.
- MCP is read-only by default; administrative tools require an explicit server flag.
- HTTP never starts implicitly and binds to loopback by default.

Read [Security](SECURITY.md) before enabling remote providers, admin tools, strict hooks, or
non-loopback HTTP access.

## Documentation

### Start here

- [Installation, upgrade, and uninstall](docs/INSTALLATION.md)
- [First workspace](docs/GETTING_STARTED.md)
- [Supported languages, frameworks, contracts, and relationships](docs/SUPPORTED_TECHNOLOGIES.md)
- [Configuration](docs/CONFIGURATION.md)
- [Use CodeGraph with every repository in a workspace](docs/CODEGRAPH_INTEGRATION.md)
- [Connect Claude Code, Codex, Gemini, Antigravity, or Cursor](docs/AGENT_SETUP.md)
- [CLI reference](docs/CLI.md)
- [Troubleshooting](docs/TROUBLESHOOTING.md)

### Analysis guides

- [Impact and risk](docs/IMPACT.md)
- [Local changes and pull requests](docs/CHANGES.md)
- [Communities](docs/COMMUNITIES.md)

Implementation details, data contracts, extractor coverage, delivery internals, ADRs, performance,
and release engineering live in the separate [Technical documentation index](docs/TECHNICAL.md).
Contributors should start with [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Licensed under Apache-2.0. See [LICENSE](LICENSE), [NOTICE](NOTICE), and
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

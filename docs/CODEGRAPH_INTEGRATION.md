# CodeGraph Integration

[CodeGraph](https://github.com/colbymchenry/codegraph) is an independent open-source project by
Colby Mchenry, distributed under the [MIT License](https://github.com/colbymchenry/codegraph/blob/main/LICENSE).
It is not part of, maintained by, or distributed with Code System Graph. CodeGraph can act as an
optional provider of repository-local symbols, callers, callees, impact, source, and editing
context. Code System Graph owns only the federated graph across repository boundaries and
interoperates with CodeGraph through its public MCP and CLI interfaces.

## What enabling CodeGraph changes

- CodeGraph nodes and relationships remain in each repository's private `.codegraph/` index; they
  are not copied into the persisted Code System Graph.
- During a scan, an exact symbol match can add CodeGraph evidence to an existing implementation
  relationship. It does not create a federated node or relationship.
- At request time, `explore` returns bounded local source and call-flow context, while `impact` can
  add a local impact summary. These results are ephemeral.

Without CodeGraph, the federated graph and its cross-repository tools continue to work. Only the
repository-local implementation detail and CodeGraph corroboration are unavailable.

## Set up CodeGraph for a workspace

### 1. Install the CodeGraph CLI

On macOS or Linux, use the official installer:

```bash
curl -fsSL https://raw.githubusercontent.com/colbymchenry/codegraph/main/install.sh | sh
codegraph --version
```

Windows and npm installation commands are available in the
[CodeGraph installation guide](https://github.com/colbymchenry/codegraph#1-install-the-cli).

You do not need to run `codegraph install` for this integration. That command registers a separate
CodeGraph MCP server with an agent; Code System Graph starts the provider itself.

### 2. Initialize every declared repository

Given this workspace manifest:

```yaml
version: 1
name: commerce
repos:
  web:
    path: ./web
  orders-service:
    path: ./orders-service
```

Run from the directory containing the manifest:

```bash
codegraph init ./web
codegraph init ./orders-service
```

Each repository must contain its own `.codegraph/` directory. Code System Graph does not create
these indexes and does not require CodeGraph configuration in `code-system-graph.yaml`.

### 3. Build and keep both graph layers current

Run the first Code System Graph scan:

```bash
mkdir -p .code-system-graph
csgraph scan \
  --codegraph \
  --config code-system-graph.yaml \
  --database .code-system-graph/code-system-graph.db
```

After code changes, use one command to refresh every initialized CodeGraph index and publish the
federated graph:

```bash
csgraph sync \
  --config code-system-graph.yaml \
  --database .code-system-graph/code-system-graph.db
```

Use `csgraph sync --watch ...` during a development session if you want continuous foreground
refresh. Repositories without `.codegraph/` are skipped without blocking the native scan.

### 4. Enable CodeGraph in the Code System Graph MCP server

Use `--codegraph` in the MCP command registered with your agent. Use an absolute database path
because an agent may start outside the workspace directory:

```bash
codex mcp add code-system-graph -- \
  csgraph mcp \
  --codegraph \
  --workspace commerce \
  --database /absolute/path/to/commerce/.code-system-graph/code-system-graph.db
```

Restart the agent and verify that its MCP tool list includes `explore`. Then ask a repository-local
question such as:

```text
In repository orders-service, trace the request handler to the service and database call.
```

CodeGraph is disabled for MCP and HTTP unless the trusted process enables it with `--codegraph`,
`CODE_SYSTEM_GRAPH_CODEGRAPH=1`, or `CODE_SYSTEM_GRAPH_CODEGRAPH_BINARY`. The binary variable also
selects a custom executable. When disabled, MCP does not advertise `explore` and HTTP returns
`403 codegraph_disabled`.

### Cursor workspace scope

Cursor should keep this server in `<workspace>/.cursor/mcp.json`, not `~/.cursor/mcp.json`. Use
absolute paths for both the `csgraph` executable and database, but remember that the MCP config
location controls Cursor scope; an absolute database path does not make a server global. After
editing the file, reload the Cursor window or restart Cursor and enable the server in MCP settings.

Do not run `codegraph install`: `csgraph mcp --codegraph` starts the provider itself and advertises
`explore`. Avoid a separate global CodeGraph MCP entry unless you intentionally want
repository-local CodeGraph tools exposed in every Cursor project. The complete JSON example is in
[Agent setup](AGENT_SETUP.md#cursor).

## Verify the setup

```bash
csgraph status \
  --config code-system-graph.yaml \
  --database .code-system-graph/code-system-graph.db
codegraph status ./web
codegraph status ./orders-service
```

The Code System Graph check and one CodeGraph check per repository must succeed. If one repository
fails, rerun `codegraph init` for that path and then `csgraph sync`.

## Adapter contract

Production integration prefers the public MCP server and falls back to direct CLI execution
without shell interpolation. The official Rust MCP SDK performs `initialize`; Code System Graph then calls
`tools/list` and maps operations from public names and input schemas. Every request is
repository-scoped, bounded, cancellable, and subject to output, item, concurrency, and time limits.

CodeGraph MCP starts as `codegraph serve --mcp --no-watch --path <repo>`. Child stdout is reserved
for newline-delimited JSON-RPC, stderr is captured separately under a strict bound, and both are
discarded after the request. A per-repository circuit temporarily bypasses MCP after a timeout.
Compatible CLI fallback uses direct process arguments and machine-readable JSON where available.

Code System Graph never reads `.codegraph/codegraph.db`, never treats a CodeGraph node identifier as
a global identity, and never persists returned source. Normal scans, MCP, and HTTP never initialize,
synchronize, install, or upgrade CodeGraph automatically. The explicit `csgraph sync` command is
the only exception for synchronization: it reads structured status for initialized indexes and
invokes `codegraph sync --quiet <repo>` only when pending files, a worktree mismatch, incomplete
state, or a reindex recommendation makes an index stale. Current indexes are reported as successful
and unchanged before the normal watcher-free scan path.

## Code System Graph delivery surfaces

- MCP `explore` and HTTP `POST /v1/tools/explore` expose bounded, ephemeral local context.
  A repository alias is optional for a one-repository workspace and required otherwise.
- MCP and HTTP `impact` automatically use bounded local enrichment when CodeGraph is enabled
  for the server process; tool callers do not select this policy.
- Admin MCP `scan` automatically uses the same process-level policy and accepts no CodeGraph
  controls in tool JSON.
- CLI `sync` refreshes initialized repository-local indexes before the federated incremental scan;
  `sync --watch` repeats that one-shot operation after debounced source changes.
- `--codegraph`, `CODE_SYSTEM_GRAPH_CODEGRAPH=1`, or `CODE_SYSTEM_GRAPH_CODEGRAPH_BINARY` enable the policy for MCP
  and HTTP delivery. `--codegraph-binary` or `CODE_SYSTEM_GRAPH_CODEGRAPH_BINARY` selects a trusted
  executable.

`explore` is the only source-bearing Code System Graph tool. Its `content` field is returned directly
to the caller and is never written to SQLite, query caches, diagnostics, or admin audit records.
All persisted resources and other tools remain source-free.

## Compatibility matrix

The minimum and currently tested production contract is CodeGraph 1.5.0. The structured CLI
adapter accepts the 1.5.x contract family; later versions are incompatible until fixtures and
contract tests are added. MCP capability discovery remains name/schema-driven, but unknown
structured output is never parsed speculatively.

| Adapter | Version tested | Capabilities | Result |
| --- | --- | --- | --- |
| Public MCP | 1.5.0 | `codegraph_explore`; `query`, `maxFiles`, `projectPath`; protocol `2024-11-05` | Passing |
| Public CLI JSON | 1.5.0 | status, query, callers, callees, impact, affected | Passing |
| Public CLI text | 1.5.0 | explore context fallback | Passing, opaque and bounded |
| Process fake | 1.5.0 contract | success, invalid MCP, timeout, cancellation, output cap | Passing |
| Live host smoke | 1.5.0 | initialize, tools/list, status, capability mapping | Passing |

The versioned fixtures are under `fixtures/codegraph/1.5.0/`; degradation fixtures cover missing
indexes, stale indexes, missing optional tools, invalid MCP responses, and oversized output.

Operation selection for 1.5.0 is:

- local context: MCP `codegraph_explore`, then CLI `explore` fallback;
- symbol resolution: CLI `query --json`;
- local callers/callees: CLI `callers/callees --json`;
- local impact: CLI `impact --json`;
- affected tests: CLI `affected --json`.

## Degradation

Missing provider, missing index, stale index, timeout, invalid response, and unsupported optional
tool are separate states. Federated boundaries remain queryable, but local detail is degraded and
coverage warnings prevent false-safe conclusions.

`sync` does not start a CodeGraph daemon and does not reuse CodeGraph's private lock or database
format. Its own watcher observes source trees and invokes one-shot public syncs serially. The
short-lived MCP child still starts with `--no-watch`, avoiding duplicate hidden watchers per scan.
CodeGraph database/WAL changes under `.codegraph/` are excluded from csgraph watch events.

CodeGraph process output is never sent to the SQLite store. Typed symbol metadata may be used by
opt-in scan-time corroboration: an exact symbol path, name, and line can add CodeGraph provenance
to an existing source-derived implementation edge. Affected-test paths and all degradation
reasons are returned in the scan summary. Corroboration never invents a federated edge, and
source/context text exists only in the in-memory response.

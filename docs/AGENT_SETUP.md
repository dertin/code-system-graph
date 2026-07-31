# Connect a Coding Agent

Code System Graph works with any local MCP client that can start a stdio server. It also has
optional routing integrations for Claude Code, Codex, Gemini CLI, Antigravity, and Cursor.

There are two separate pieces:

1. **MCP connection, required for agent use:** gives the agent Code System Graph tools.
2. **Routing hook or rule, optional:** reminds the agent when to use the federated graph and when
   repository-local CodeGraph context is more appropriate.

Installing a routing hook without connecting MCP does not give the agent Code System Graph tools.

## Before connecting

Complete one scan and use absolute paths when the agent may start outside the workspace directory:

```bash
csgraph status \
  --config /absolute/path/to/code-system-graph.yaml \
  --database /absolute/path/to/.code-system-graph/code-system-graph.db
```

The MCP command needs the workspace name and database path. It does not scan automatically.

## Supported agents

| Agent | MCP configuration | Optional routing integration |
| --- | --- | --- |
| Claude Code | `claude mcp add` | Native JSON prompt hook |
| Codex CLI, IDE extension, and app | `codex mcp add` or shared Codex settings | Native JSON prompt hook |
| Gemini CLI | `gemini mcp add` | Native JSON prompt hook |
| Antigravity | `.agents/mcp_config.json` or MCP settings | Project routing rule |
| Cursor | `.cursor/mcp.json` or MCP settings | Project routing rule |

Antigravity and Cursor receive an always-on project rule because the current Code System Graph hook
adapter does not inject prompt context through their native lifecycle APIs.

Agent configuration syntax can change independently of Code System Graph. The examples below were
checked against the official MCP documentation for
[Claude Code](https://code.claude.com/docs/en/mcp),
[Codex](https://developers.openai.com/codex/mcp),
[Gemini CLI](https://github.com/google-gemini/gemini-cli/blob/main/docs/tools/mcp-server.md),
[Antigravity](https://antigravity.google/docs/mcp), and
[Cursor](https://docs.cursor.com/context/model-context-protocol).

In the examples below, replace `/absolute/path/to/workspace` and `commerce`.

## Claude Code

Register a project-local stdio server:

```bash
claude mcp add code-system-graph --scope local -- \
  csgraph mcp \
  --workspace commerce \
  --database /absolute/path/to/workspace/.code-system-graph/code-system-graph.db
```

Verify with:

```bash
claude mcp get code-system-graph
```

Inside Claude Code, `/mcp` shows connection status and discovered tools.

## Codex

Register the server:

```bash
codex mcp add code-system-graph -- \
  csgraph mcp \
  --workspace commerce \
  --database /absolute/path/to/workspace/.code-system-graph/code-system-graph.db
```

Verify with:

```bash
codex mcp list
```

The Codex CLI, IDE extension, and app share MCP configuration on the same host. Use `/mcp` in the
Codex terminal UI to inspect active tools.

For a project-scoped setup, Codex also accepts `.codex/config.toml` in a trusted project:

```toml
[mcp_servers.code-system-graph]
command = "csgraph"
args = [
  "mcp",
  "--workspace", "commerce",
  "--database", "/absolute/path/to/workspace/.code-system-graph/code-system-graph.db",
]
```

## Gemini CLI

Register a project-scoped server:

```bash
gemini mcp add --scope project code-system-graph \
  csgraph mcp \
  --workspace commerce \
  --database /absolute/path/to/workspace/.code-system-graph/code-system-graph.db
```

Verify with:

```bash
gemini mcp list
```

The current folder must be trusted before Gemini CLI starts a project stdio server.

## Antigravity

Create or merge `.agents/mcp_config.json` in the workspace:

```json
{
  "mcpServers": {
    "code-system-graph": {
      "command": "csgraph",
      "args": [
        "mcp",
        "--workspace",
        "commerce",
        "--database",
        "/absolute/path/to/workspace/.code-system-graph/code-system-graph.db"
      ]
    }
  }
}
```

Open the MCP manager in Antigravity and confirm that `code-system-graph` is connected. Antigravity
also supports a global configuration, but project scope keeps the workspace identity and database
path together.

## Cursor

Create or merge `.cursor/mcp.json` in the workspace:

```json
{
  "mcpServers": {
    "code-system-graph": {
      "command": "csgraph",
      "args": [
        "mcp",
        "--workspace",
        "commerce",
        "--database",
        "/absolute/path/to/workspace/.code-system-graph/code-system-graph.db"
      ]
    }
  }
}
```

Open Cursor's MCP settings and confirm that the server and its tools are enabled.

## Install optional routing

Run one explicit command for each agent and repository where routing guidance should be available:

```bash
cd /absolute/path/to/workspace/api
csgraph hooks install \
  --host codex \
  --workspace commerce \
  --repository api \
  --database ../.code-system-graph/code-system-graph.db
```

The hook writes project-local agent configuration and private state under
`.code-system-graph/hooks/`. When that root belongs to a Git worktree, the installer preserves its
`.gitignore` and adds `.code-system-graph/` automatically. Review the agent configuration
separately: commit it when the team should share the routing behavior, or keep it local according
to the agent's conventions.

Supported `--host` values:

```text
claude-code
codex
gemini
antigravity
cursor
```

The default advisory mode:

- guides local symbol and implementation questions through Code System Graph's bounded `explore`
  handoff, with direct CodeGraph use only if that provider is degraded;
- guides contracts, architecture, impact, changes, and cross-repository questions toward Code
  System Graph;
- fails open and never blocks normal agent work;
- does not run a scan or query for every prompt.

The installer merges marker-owned content into existing agent configuration, preserves unrelated
settings, creates backups before changing existing files, and is safe to rerun.

Inspect or remove it with the same target arguments:

```bash
csgraph hooks status --host codex --workspace commerce --repository api \
  --database ../.code-system-graph/code-system-graph.db
csgraph hooks uninstall --host codex --workspace commerce --repository api \
  --database ../.code-system-graph/code-system-graph.db
```

Use [strict mode](HOOKS.md#strict-mode) only if you intentionally want a fail-closed Git pre-commit
gate.

## Optional CodeGraph enrichment

[CodeGraph](https://github.com/colbymchenry/codegraph) is an independent MIT-licensed project and
is not included with Code System Graph. If it is installed separately and its index already exists
in each repository, add `--codegraph` to the MCP server arguments. This enables bounded local source
context and affected-test enrichment:

```text
csgraph mcp --codegraph --workspace commerce --database /absolute/path/to/code-system-graph.db
```

Code System Graph continues to answer federated questions if CodeGraph is unavailable, but reports
the local coverage degradation.

## What the agent can and cannot do by default

The default MCP profile can read status, contracts, graph context, query, trace, communities,
impact, local changes, and explicitly enabled pull-request metadata. It cannot mutate the
workspace, scan, clean caches, or write manual links.

Start the server with `--admin` only for a trusted client that needs those bounded mutations.
Remote pull requests and CodeGraph are also separate process-level opt-ins.

See [MCP reference](MCP.md) for every tool, resource, limit, and administrative control.

## Troubleshooting

If the server is absent or disconnected:

1. run `csgraph mcp --workspace ... --database ...` directly and check stderr;
2. verify that the agent can resolve `csgraph` on its `PATH`;
3. replace relative database paths with absolute paths;
4. confirm the workspace name matches the scanned snapshot;
5. rerun the agent's MCP list/status command after changing configuration;
6. rerun `csgraph status` to check database freshness.

Agent configuration is not removed when the `csgraph` binary is uninstalled. Remove MCP registration
and Code System Graph-owned hooks first.

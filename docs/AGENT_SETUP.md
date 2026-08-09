# Connect a Coding Agent

Code System Graph works with any local MCP client that can start a stdio server. The preferred
installation is one plugin containing both the MCP declaration and one Agent Skill that explains
when and how to use the graph.

Keep one source of detailed routing instructions:

1. **Packaged Agent Skill, required when the client supports skills:** owns the concrete MCP
   procedure and verifies the workspace before use.
2. **Native prompt hook, optional activation accelerator:** Claude Code, Codex, and Gemini can
   classify submitted prompts and ask the agent to activate the installed skill. The hook must not
   duplicate the skill procedure.
3. **Static project rule, last-resort fallback:** use only when neither packaged skills nor a
   conditional prompt hook are available.

Do not copy the skill into a global `AGENTS.md` or combine it with an always-on repository rule.
Those mechanisms repeat the same policy with broader activation semantics. An MCP connection
without a skill remains usable, but the brief hook guidance alone does not teach the full safe
workflow.

## Portable Agent Plugin

Clients implementing [Agent Plugins 1.0.0](https://agent-plugins.org/specification) can consume a
single generated package containing the MCP declaration, one portable Agent Skill, its operating
guide, the license, and an ignored local runtime binding:

`crates/code-system-graph-hooks/agent-integration-template/` is the single versioned source for
editable agent installation, skill, discovery, and activation content. Its `agent-plugin/`
subtree owns the manifest, MCP declaration, canonical skill, operating guide, optional client
metadata, license, schemas, and ignore rule. Its
`native-hooks/` subtree owns classifier signals, dynamic guidance, fallback rules, strict-gate
shell, limitations, and host UI text. Rust includes and renders those files; it contains no second
skill, routing policy, or hook-script body.

```bash
csgraph plugin create \
  --output /absolute/path/to/code-system-graph-plugin \
  --config /absolute/path/to/code-system-graph.yaml \
  --database /absolute/path/to/.code-system-graph/code-system-graph.db \
  --codegraph
```

Omit `--codegraph` to keep `explore` unavailable, or add
`--codegraph-binary /absolute/path/to/codegraph` with the opt-in flag. Generation requires a valid
manifest and an existing matching snapshot. Stale snapshots are allowed and reported. `csgraph
1.0.3` must be in the client's `PATH`, and no platform binary is bundled. Repeating the command is a
no-op only for an exactly identical directory; conflicts, additional files, and symlinks fail
without partial writes.

Only `.local/code-system-graph/mcp-binding.json` contains absolute paths for the current host, and
the generated `/.local/` rule excludes it. Commit the remaining plugin files when the team should
share them. After cloning, each developer uses
[existing-plugin mode](#bind-an-existing-portable-plugin) to generate that local binding.
The local binding also records the generating `csgraph` version, exact executable fingerprint,
and build-time source commit and dirty state when those Git values were available. This metadata
is diagnostic only and does not weaken runtime validation.

The generated server forcibly disables the admin environment profile and never supplies
`--admin`. Its skill routes through `status`, then identity resolution and bounded graph tools,
and never initiates scans or mutations. Complete-plugin mode emits only portable skill files; it
does not add Codex UI metadata or another client's rules. A native prompt hook may complement this
skill by selecting it for relevant prompts, but the hook does not replace the procedure.

Every generated package derives its stable name and suffix only from the manifest's declared
workspace name. The plugin, MCP server entry, and skill share that identity, so clones of one
logical workspace render the same versioned files.

### Bind an existing portable plugin

Use `plugin create` with both integration options when a repository already contains a versioned
base plugin and each developer needs a local workspace binding:

```bash
csgraph plugin create \
  --output /absolute/path/to/base-plugin \
  --mcp-server-name existing-code-system-graph \
  --routing-skill existing-system-graph-skill \
  --config /absolute/path/to/code-system-graph.yaml \
  --database /absolute/path/to/.code-system-graph/code-system-graph.db \
  --codegraph
```

The versioned portable `mcp.json` entry, and the matching Codex entry when present, must use this
runtime shape:

```json
{
  "type": "stdio",
  "command": "csgraph",
  "args": [
    "mcp",
    "--binding",
    "${PLUGIN_ROOT}/.local/code-system-graph/mcp-binding.json"
  ],
  "env": {
    "CODE_SYSTEM_GRAPH_MCP_ADMIN": "0"
  }
}
```

The command adds those entries and the generated routing skill when absent, validates matching
existing components, and requires an exact `/.local/` Git ignore rule. When the existing plugin
already has `.codex-plugin/plugin.json`, it also emits optional Codex UI metadata at
`agents/openai.yaml`; a portable-only plugin does not receive that client-specific file. It
preserves every unrelated manifest field, MCP server, and skill. The ignored
`.local/code-system-graph/` directory contains the runtime binding and an ownership receipt.
Both identify the exact generator build so an installation can be audited independently of the
versioned plugin files.
Recreating an identical integration is a no-op. Pass `--replace-generated` to update older owned
local state; unmanaged state and invalid ownership identities are rejected. When a newer generator
changes the versioned skill itself, run `plugin uninstall` first and then `plugin create`; its
receipt verifies the existing skill before removal instead of silently overwriting it.

Remove only that managed integration before testing a clean reinstall:

```bash
csgraph plugin uninstall \
  --output /absolute/path/to/base-plugin \
  --mcp-server-name existing-code-system-graph \
  --routing-skill existing-system-graph-skill
```

Uninstall verifies the receipt and skill hashes, removes the matching portable and Codex MCP
entries, routing skill, and local directory, and preserves all other plugin content. Modified or
unowned components cause a conflict instead of being deleted. Install the base plugin root in the
client. Before binding, the federated MCP fails to start but independent components remain
available.

Agent Plugins 1.0.0 does not define conditional activation by the agent's current directory; plugin
installation and enablement are client-owned. For actual workspace-only activation, install the
generated package in the client's project-local configuration rooted at the directory containing
the workspace manifest, not globally. The generated skill also fails closed: it requires the task's
nearest manifest and MCP `status` to report the declared workspace. This routing guard prevents
incorrect use, but it cannot stop a client that globally launches every installed MCP process.

### Packaging and marketplaces

The reusable unit is the same `skills/<name>/` directory for every Agent Skills client. Do not
generate Claude-, Gemini-, Antigravity-, Cursor-, or Codex-specific copies of `SKILL.md`. A native
adapter may translate only discovery metadata and the MCP declaration:

| Client | Preferred distribution | Adapter outside the portable core |
| --- | --- | --- |
| Codex | Codex marketplace or Agent Plugin installation | Existing `.codex-plugin/plugin.json`; optional `agents/openai.yaml` UI metadata |
| Cursor | Agent Plugin marketplace or local Agent Plugin | None for skills and MCP |
| Claude Code | Claude plugin or manual MCP plus the same skill | `.claude-plugin/plugin.json` and `.mcp.json` |
| Gemini CLI | Gemini extension or manual MCP plus the same skill | `gemini-extension.json` with `mcpServers` |
| Antigravity | Antigravity plugin or manual MCP plus the same skill | `mcp_config.json` |

Agent Plugins 1.0 does not define one cross-vendor marketplace. Keep one canonical skill and MCP
model in source, then let each marketplace or installer stage the small native adapter it requires.
Do not place those native adapter files into a generated complete Agent Plugin and still describe
that full directory as the portable core.

## Manual compatibility setup

Use the following client-specific MCP registration only when the client cannot install the Agent
Plugin package. Pair it with the same generated Agent Skill when the client supports standalone
skills. A native prompt hook can improve activation for Claude Code, Codex, or Gemini without
duplicating the skill. Use a static routing rule only when the skill itself cannot be installed.

### Before connecting

Complete one scan and use absolute paths when the agent may start outside the workspace directory:

```bash
csgraph status \
  --config /absolute/path/to/code-system-graph.yaml \
  --database /absolute/path/to/.code-system-graph/code-system-graph.db
```

The MCP command needs the workspace name and database path. It does not scan automatically.

The examples below enable optional CodeGraph enrichment with `--codegraph`. Before using them,
initialize CodeGraph in every declared repository as described in
[Use CodeGraph with a workspace](CODEGRAPH_INTEGRATION.md#set-up-codegraph-for-a-workspace). Remove
`--codegraph` if you want only the federated Code System Graph tools.

### Supported agents

| Agent | Manual MCP configuration | Optional activation mechanism |
| --- | --- | --- |
| Claude Code | `claude mcp add` | Packaged skill, optionally selected by native JSON prompt hook |
| Codex CLI, IDE extension, and app | `codex mcp add` or shared Codex settings | Packaged skill, optionally selected by native JSON prompt hook |
| Gemini CLI | `gemini mcp add` | Packaged skill, optionally selected by native JSON prompt hook |
| Antigravity | `.agents/mcp_config.json` or MCP settings | Packaged skill; project rule only as fallback |
| Cursor | `.cursor/mcp.json` or MCP settings | Packaged skill; project rule only as fallback |

When a static fallback is required, Antigravity and Cursor use a project rule because the current
Code System Graph adapter does not inject prompt context through their native lifecycle APIs. Do not
install that rule when either client already loaded the packaged Agent Skill.

Agent configuration syntax can change independently of Code System Graph. The examples below were
checked against the official MCP documentation for
[Claude Code](https://code.claude.com/docs/en/mcp),
[Codex](https://developers.openai.com/codex/mcp),
[Gemini CLI](https://github.com/google-gemini/gemini-cli/blob/main/docs/tools/mcp-server.md),
[Antigravity](https://antigravity.google/docs/mcp), and
[Cursor](https://docs.cursor.com/context/model-context-protocol).

In the examples below, replace `/absolute/path/to/workspace` and `commerce`.

### Claude Code

Register a project-local stdio server:

```bash
claude mcp add code-system-graph --scope local -- \
  csgraph mcp \
  --codegraph \
  --workspace commerce \
  --database /absolute/path/to/workspace/.code-system-graph/code-system-graph.db
```

Verify with:

```bash
claude mcp get code-system-graph
```

Inside Claude Code, `/mcp` shows connection status and discovered tools.

### Codex

Register the server:

```bash
codex mcp add code-system-graph -- \
  csgraph mcp \
  --codegraph \
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
  "--codegraph",
  "--workspace", "commerce",
  "--database", "/absolute/path/to/workspace/.code-system-graph/code-system-graph.db",
]
```

### Gemini CLI

Register a project-scoped server:

```bash
gemini mcp add --scope project code-system-graph \
  csgraph mcp \
  --codegraph \
  --workspace commerce \
  --database /absolute/path/to/workspace/.code-system-graph/code-system-graph.db
```

Verify with:

```bash
gemini mcp list
```

The current folder must be trusted before Gemini CLI starts a project stdio server.

### Antigravity

Create or merge `.agents/mcp_config.json` in the workspace:

```json
{
  "mcpServers": {
    "code-system-graph": {
      "command": "csgraph",
      "args": [
        "mcp",
        "--codegraph",
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

### Cursor

Create or merge `<workspace>/.cursor/mcp.json`. Do not put this Code System Graph entry in
`~/.cursor/mcp.json`; the configuration-file location is what keeps the server scoped to this
workspace. An absolute database path identifies the snapshot but does not make the server global.

```json
{
  "mcpServers": {
    "code-system-graph": {
      "command": "/absolute/path/to/bin/csgraph",
      "args": [
        "mcp",
        "--codegraph",
        "--workspace",
        "commerce",
        "--database",
        "/absolute/path/to/workspace/.code-system-graph/code-system-graph.db"
      ]
    }
  }
}
```

`--codegraph` makes this server advertise `explore`; `csgraph mcp` starts the CodeGraph provider,
so do not run `codegraph install` for this setup. Reload the Cursor window or restart Cursor after
changing the file, then open MCP settings and confirm that `code-system-graph` and its tools are
enabled. A separate global CodeGraph MCP entry would expose repository-local tools in every Cursor
project and is not needed for this integration.

## Optional native activation

For Claude Code, Codex, and Gemini, the installed native hook classifies prompt intent and emits a
short instruction to activate the packaged Code System Graph skill. The skill remains the only
detailed MCP procedure. For Antigravity and Cursor, this command writes a static project rule
instead; skip it when those clients can load the skill. Never add the guidance to a global
`AGENTS.md`.

Run one explicit command for each agent and repository where routing guidance should be available:

```bash
cd /absolute/path/to/workspace/orders-service
csgraph hooks install \
  --host codex \
  --codegraph \
  --workspace commerce \
  --repository orders-service \
  --database ../.code-system-graph/code-system-graph.db
```

The integration writes project-local agent configuration and private state under
`.code-system-graph/hooks/`. It cannot inspect another process's MCP tool list, so its
policy is explicit: pass `--codegraph` only when the registered MCP command also uses
`--codegraph` and advertises `explore`. Omit both flags for the native-only profile. Reinstall the
hook after changing that MCP policy.

When that root belongs to a Git worktree, the installer preserves its
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

The default advisory mode follows the install-time policy:

- with `--codegraph`, guides local symbol and implementation questions through Code System Graph's
  bounded `explore` handoff;
- without `--codegraph`, never recommends `explore` and limits guidance to persisted,
  source-free graph context;
- in both profiles, guides contracts, architecture, impact, changes, and cross-repository questions
  toward Code System Graph;
- fails open and never blocks normal agent work;
- does not run a scan or query for every prompt.

The installer merges marker-owned content into existing agent configuration, preserves unrelated
settings, creates backups before changing existing files, and is safe to rerun.

Inspect or remove it with the same target arguments:

```bash
csgraph hooks status --host codex --codegraph --workspace commerce --repository orders-service \
  --database ../.code-system-graph/code-system-graph.db
csgraph hooks uninstall --host codex --codegraph --workspace commerce --repository orders-service \
  --database ../.code-system-graph/code-system-graph.db
```

Use [strict mode](HOOKS.md#strict-mode) only if you intentionally want a fail-closed Git pre-commit
gate.

## CodeGraph behavior

With `--codegraph`, the agent receives bounded local source context through the `explore` tool and
CodeGraph-backed impact enrichment. Without it, `explore` is not advertised and all native
federated tools remain available. Use the same choice for `csgraph hooks install`; reinstall the
hook if the MCP policy changes. CodeGraph's private nodes and relationships are never merged into
the persisted Code System Graph.

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

For Cursor, also confirm that the entry is in `<workspace>/.cursor/mcp.json`, reload the window,
and enable the server in Cursor's MCP settings. See [Troubleshooting](TROUBLESHOOTING.md) for
extraction-budget and workspace-scope examples.

Agent configuration is not removed when the `csgraph` binary is uninstalled. Remove MCP registration
and Code System Graph-owned hooks first.

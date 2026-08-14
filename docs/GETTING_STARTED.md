# Create Your First Workspace

This guide builds a useful graph for a small multi-repository system, verifies it, and connects it
to an agent. It assumes `csgraph --version` succeeds; otherwise start with
[Installation](INSTALLATION.md).

## The workspace directory

For sibling repositories, their parent directory is the Code System Graph workspace. It does not
need to be a Git repository itself.

```text
my-project/
|-- code-system-graph.yaml
|-- .code-system-graph/
|   `-- code-system-graph.db
|-- repo_1/                    # local repository directory
`-- repo_2/                    # local repository directory
```

A workspace is the system boundary you want to ask questions about. The YAML declares that
boundary; the SQLite file stores its latest and historical graph snapshots. Keep both in
`my-project/`, outside the participating repositories.

Run setup, scan, status, and query commands from `my-project/` so the relative paths and database
path used throughout this guide resolve consistently:

```bash
cd /absolute/path/to/my-project
```

## 1. Create the manifest

### Multiple repositories

`csgraph init` creates a valid single-repository manifest; it does not discover child directories.
You may use it as a scaffold from the parent:

```bash
cd /absolute/path/to/my-project
csgraph init . --name my-project
```

It initially writes:

```yaml
version: 1
name: my-project
repos:
  root:
    path: .
```

If `my-project/` belongs to a Git worktree, `init` preserves or creates its `.gitignore` and adds
`.code-system-graph/` once. When `my-project/` is an unversioned container with Git repositories
only inside `repo_1/` and `repo_2/`, no parent `.gitignore` is created or needed.

Before scanning, replace the `root` entry. Otherwise the whole parent directory is modeled as one
repository and the boundaries between `repo_1` and `repo_2` are lost:

```yaml
version: 1
name: my-project
repos:
  repo_1:
    path: ./repo_1
  repo_2:
    path: ./repo_2
```

That is enough for the first scan. Code System Graph automatically discovers supported package
manifests, API and event schemas, source-level boundaries, database declarations, infrastructure,
tests, ownership, and documentation inside those repositories.

You can skip `init` and create this short manifest directly.

### One repository

For a workspace containing one repository, run `init` inside that repository:

```bash
cd /absolute/path/to/my-repository
csgraph init . --name my-repository
```

`init` refuses to overwrite an existing manifest.

The resulting layout is:

```text
my-repository/
|-- code-system-graph.yaml
|-- .gitignore
|-- .code-system-graph/
|   `-- code-system-graph.db
`-- src/
```

`code-system-graph.yaml` is shareable workspace configuration. `.code-system-graph/` is local
generated state. In a Git repository, `init` adds its ignore rule automatically without replacing
existing rules. Commit the manifest and `.gitignore` update when the setup is ready for the team.

In the multi-repository layout, the database remains in `my-project/`, outside `repo_1` and
`repo_2`. A parent-level ignore rule is created only if `my-project/` itself is tracked.

## 2. Run the first scan

From `my-project/` for multiple repositories, or the repository root for one:

```bash
mkdir -p .code-system-graph
csgraph scan \
  --config code-system-graph.yaml \
  --database .code-system-graph/code-system-graph.db
```

The result is JSON. A successful scan identifies the workspace and snapshot and reports discovery,
coverage, warnings, and any degraded optional capability. Warnings matter: they describe facts
that could not safely become exact graph relationships.

The database is an embedded SQLite file. No database server is required.

## 3. Verify freshness

```bash
csgraph status \
  --config code-system-graph.yaml \
  --database .code-system-graph/code-system-graph.db
```

Use `status` before trusting an impact or change answer. A missing or stale observation means
coverage is incomplete; it does not mean that the repositories are independent.

## 4. Add CodeGraph context (optional)

CodeGraph adds symbols, callers, callees, and implementation flow inside each repository. Follow
[Use CodeGraph with a workspace](CODEGRAPH_INTEGRATION.md#set-up-codegraph-for-a-workspace) to
install it, then initialize every repository declared in the manifest:

```bash
codegraph init ./repo_1
codegraph init ./repo_2
```

The same guide includes verification and the exact MCP setup. Skip this step if you only need the
federated graph.

## 5. Ask a direct question

Search is the usual entry point:

```bash
csgraph query "orders" \
  --config code-system-graph.yaml \
  --workspace my-project \
  --database .code-system-graph/code-system-graph.db
```

Each result includes a stable node ID and an explanation of its score. Code System Graph does not
silently choose among ambiguous results. Copy the intended ID into a trace or impact command:

```bash
csgraph impact \
  --target <node-id> \
  --workspace my-project \
  --database .code-system-graph/code-system-graph.db
```

```bash
csgraph trace \
  --from <source-node-id> \
  --to <target-node-id> \
  --workspace my-project \
  --database .code-system-graph/code-system-graph.db
```

## 6. Connect an agent

The MCP server runs over stdio and is started by the agent when needed:

```bash
csgraph mcp \
  --config code-system-graph.yaml \
  --codegraph \
  --workspace my-project \
  --database .code-system-graph/code-system-graph.db
```

Do not run that command manually and leave it waiting. Register it with the agent instead. For
example, Codex can write its configuration with:

```bash
codex mcp add code-system-graph -- \
  csgraph mcp \
  --config /absolute/path/to/my-project/code-system-graph.yaml \
  --codegraph \
  --workspace my-project \
  --database /absolute/path/to/my-project/.code-system-graph/code-system-graph.db
```

Remove `--codegraph` from these commands if you skipped step 4.

See [Connect a coding agent](AGENT_SETUP.md) for every supported agent and the optional routing
hook.

Useful first prompts:

```text
Summarize the services and contracts in this workspace. Cite graph evidence.
Trace the order flow across repositories and call out missing links.
What depends on POST /orders?
Which tests and owners are connected to the orders contract?
What information is stale, ambiguous, or missing?
```

## Daily use

Choose either one-shot or watch mode. After a pull, branch switch, or relevant edit, run the
explicit one-shot incremental synchronizer:

```bash
csgraph sync --config code-system-graph.yaml --database .code-system-graph/code-system-graph.db
```

It refreshes any already-initialized local CodeGraph indexes, reuses unchanged native extraction
batches, publishes one snapshot, and exits. To refresh automatically during a development session:

```bash
csgraph sync --watch \
  --config code-system-graph.yaml \
  --database .code-system-graph/code-system-graph.db
```

Watch mode performs an initial pass and then repeats after relevant file or manifest changes. It is
a foreground process, not an installed service, so automatic refresh lasts only while the command
is running. The process exits cleanly on Ctrl-C and emits one JSON result per completed pass. Add
`--poll-interval-ms 2000` for network/virtual filesystems. To synchronize only one registered
repository while preserving the others:

```bash
csgraph sync \
  --repo repo_1 \
  --config code-system-graph.yaml \
  --database .code-system-graph/code-system-graph.db
```

Use `--force` only when you intentionally want to invalidate reuse for the selected scan scope, or
`--no-codegraph` when only the federated graph should be updated.

Before committing local work:

```bash
csgraph changes \
  --repo repo_1 \
  --scope staged \
  --workspace my-project \
  --database .code-system-graph/code-system-graph.db
```

This workflow requires Git. The command reads Git state and reports semantic impact. It does not
stage, test, commit, or push.

## When automatic linking is not enough

First inspect the warning, evidence, and ambiguity. Prefer fixing an incorrect or missing source
declaration. If the relationship is real but cannot be observed automatically, add a versioned
`manualLinks` declaration with a reason. See [Configuration](CONFIGURATION.md#manual-links).

Do not use a manual link to hide uncertainty without evidence.

## Troubleshooting

Run the conservative diagnostic:

```bash
csgraph doctor \
  --config code-system-graph.yaml \
  --database .code-system-graph/code-system-graph.db
```

Common problems:

| Symptom | Check |
| --- | --- |
| `csgraph` is not found | Add `$HOME/.cargo/bin` or the selected `$PREFIX/bin` to `PATH` |
| Database cannot be opened | Create its parent directory and check write permissions |
| Repository path is rejected | Resolve it relative to the manifest and review `allowedRoots` |
| Agent shows no tools | Verify its MCP config and run the agent's MCP status/list command |
| Results are stale | Run one-shot `sync`, or keep `sync --watch` active, then check `status` |
| A relationship is missing | Check extractor coverage, ambiguity warnings, and exact contract identities |
| Local source detail is unavailable | CodeGraph is optional; install/index it separately or use graph evidence only |

For a support handoff, create a new source-free diagnostic bundle:

```bash
csgraph diagnostics \
  --config code-system-graph.yaml \
  --database .code-system-graph/code-system-graph.db \
  --output ./csgraph-diagnostics.json
```

The command never uploads the file and refuses to overwrite an existing destination. Review it
before sharing.

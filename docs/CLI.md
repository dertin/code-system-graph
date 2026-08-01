# Command-Line Interface

Code System Graph commands are non-interactive. Structured results use deterministic JSON on stdout;
diagnostics use stderr. `--json` is accepted globally for automation, while structured commands
already default to JSON. `--quiet`, `--verbose`, and `--log-format text|json` are global.

Verbose diagnostics are enabled by `-v`/`--verbose` or `CODE_SYSTEM_GRAPH_DEBUG=1`. They emit
`command_started` and `command_finished` events with the command name, one correlation ID,
elapsed time, and success state. JSON diagnostics remain on stderr and do not contaminate
structured or MCP protocol stdout. `--quiet` suppresses these non-result diagnostics.

## Workspace lifecycle

```text
csgraph init [path] [--name <workspace>]
csgraph workspace add <name> --config <manifest> --database <db>
csgraph workspace remove <name> --database <db> --yes
csgraph workspace list --database <db>
csgraph repo add|remove|list ...
csgraph clean <workspace> --database <db> --force
```

Initialization never overwrites `code-system-graph.yaml`. When the workspace belongs to a Git
worktree, it preserves `.gitignore` and adds `.code-system-graph/` once for generated databases and
hook state. Otherwise it does not create an unnecessary `.gitignore`. The JSON result reports
`gitignore_path` as `null` when this does not apply, plus `gitignore_updated`. Removal and cleanup
require explicit confirmation.

## Index and diagnostics

```text
csgraph scan --database <db> [--config <manifest>] [--repo <alias>] [--changed|--force]
csgraph sync --database <db> [--config <manifest>] [--repo <alias>] [--watch]
csgraph config show [--config <manifest>] [--repo <alias>]
csgraph status --database <db> [--config <manifest>]
csgraph doctor --database <db> [--config <manifest>]
csgraph diagnostics --database <db> [--config <manifest>] --output <new-bundle.json>
csgraph backup|restore|migrate ...
```

Normal scan reuses unchanged source-owned batches. `--repo` recomputes only the selected alias and
retains the other repositories' previous batches. `--force` invalidates reuse for the selected
scope. Doctor reports unavailable observations as unknown rather than healthy.

`config show` resolves workspace and repository-local configuration without opening a database. Its
deterministic JSON reports protected exclusions, reactivable built-in defaults, configured
`excludes` and `includeDefaults` in canonical form with their source, and the ordered effective
rules. The optional `--repo` filter requires one exact manifest alias.

`scan` and plain `sync` each perform one pass and exit. Neither command installs a watcher or
background service. `sync` is incremental, but first runs `codegraph sync --quiet` with direct
process arguments for every selected repository that already contains `.codegraph/`. Missing or
failed CodeGraph indexes are explicit per-repository results and do not block the native atomic
snapshot. Use `--no-codegraph` to skip that layer. `sync --watch` performs an initial pass and then
automatically repeats while its foreground process is running. It emits one JSON object per
completed pass,
coalesces event bursts, and observes the manifest plus all declared checkout trees. It uses the
operating system's native backend on Linux/Unix, macOS, and Windows, automatically falls back to
polling if native watcher setup fails, and selects polling for repositories on WSL Windows mounts.
Set `--poll-interval-ms <n>` to force polling for network or virtual filesystems. The same effective
policy reported by `config show` filters scan discovery, OpenAPI auto-detection, and watch events;
protected generated state cannot create feedback loops. Watch mode stops when the process receives
Ctrl-C or otherwise exits.

`diagnostics` writes an explicit JSON support bundle containing the binary version, operating
system, architecture, generation time, whether `CODE_SYSTEM_GRAPH_DEBUG=1` was requested, and the
source-free doctor report. It refuses to overwrite an existing destination and creates the file
with mode `0600` on Unix. Review the bundle before sharing it; the command does not upload or send
anything.

## Intelligence

```text
csgraph query <question> --workspace <name> --database <db>
csgraph trace --from <node> --to <node> --workspace <name> --database <db>
csgraph traverse ...
csgraph communities ...
csgraph impact ...
csgraph changes ...
csgraph contracts list|show|validate|diff|explain-link ...
csgraph export --format json|graphml|markdown ...
```

Every operation has server-side bounds. Local Git collection and hosted PR requests propagate
SIGINT/SIGTERM cancellation to their child process or HTTP request.

## Pull requests

```text
csgraph pr list --provider github|bitbucket --owner <owner> --repository <repo> \
  --enabled --consent [--state open|closed|all] [--cursor <cursor>] [--limit <n>]
csgraph pr show --provider github|bitbucket --owner <owner> --repository <repo> \
  --number <n> --enabled --consent
csgraph pr overlap --left <semantic-input.json> --right <semantic-input.json>
```

Remote operations require both enablement and per-call consent. Tokens are read only from the
environment variable selected by `--token-env`. Bitbucket user API tokens additionally use the
email variable selected by `--user-env`. Overlap is local and source-free.

## Delivery and integration

```text
csgraph mcp --workspace <name> --database <db>
csgraph serve --workspace <name> [--host 127.0.0.1] [--port 4767]
csgraph hooks install|status|uninstall ... [--codegraph]
csgraph completions <shell>
```

`mcp` and `serve` accept `--codegraph [--codegraph-binary <path>]` as trusted process-level
policy. When enabled, impact requests are enriched automatically and administrative scans use the
same bounded corroboration. `CODE_SYSTEM_GRAPH_CODEGRAPH=1` enables the policy from the environment;
`CODE_SYSTEM_GRAPH_CODEGRAPH_BINARY` both enables it and selects the executable. Tool callers cannot
override either setting.

The hook `--codegraph` flag controls only installed routing guidance; use it exactly when the
agent's MCP command enables CodeGraph, and reinstall after changing that policy.

MCP reserves stdout for protocol traffic. HTTP is never started implicitly and non-loopback binds
require an ephemeral bearer token. Stable failure exit codes are documented by the generated
`ExitCode` schema and cover invalid input, not found, ambiguity, conflict, partial results,
unavailable capability, timeout, internal failure, and cancellation.

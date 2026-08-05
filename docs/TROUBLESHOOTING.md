# Troubleshooting

Start with the conservative, source-free diagnostic:

```bash
csgraph doctor \
  --config code-system-graph.yaml \
  --database .code-system-graph/code-system-graph.db
```

## Cursor does not show the server or `explore`

Keep the MCP entry in `<workspace>/.cursor/mcp.json`, not `~/.cursor/mcp.json`. Use absolute paths
for the `csgraph` executable and database. The config-file location controls Cursor scope; an
absolute database path alone does not make the server global.

After changing the file, reload the Cursor window or restart Cursor, then enable
`code-system-graph` in MCP settings. The command must include `--codegraph` for the server to
advertise `explore`. Do not run `codegraph install`: Code System Graph starts the provider itself.
A separate global CodeGraph entry would expose repository-local tools in every project.

See [Agent setup](AGENT_SETUP.md#cursor) for the complete JSON.

## One extracted string exceeds its budget

The safe default for `maxStringBytesPerValue` is 65,536 bytes. If a legitimate metadata value is
larger, set the smallest practical positive integer in the trusted workspace manifest:

```yaml
extractionBudgets:
  maxStringBytesPerValue: 131072
```

This ceiling applies workspace-wide but independently to each string value. It is different from
`maxAccumulatedStringBytesPerArtifact`, which limits all strings accumulated by one
artifact-extractor invocation, and `maxSerializedOutputBytesPerArtifact`, which limits that
invocation's complete encoded output. Raise only the resource named by the diagnostic. A budget
change invalidates extractor-batch reuse and requires a full scan rather than `--repo`.

For all fields and trust-boundary rules, see [Configuration](CONFIGURATION.md#security-budgets).

## Common checks

| Symptom | Check |
| --- | --- |
| `csgraph` is not found | Use an absolute executable path or add its installation directory to `PATH` |
| Database cannot be opened | Create its parent directory and check permissions |
| Agent shows no tools | Verify its workspace-local MCP config, reload it, and check MCP settings |
| Results are stale | Run `csgraph sync`, then `csgraph status` |
| A relationship is missing | Check ambiguity degradations and exact canonical contract identities |
| Local source detail is unavailable | Verify the `codegraph` executable is available and its index is initialized, current, and enabled with `--codegraph` |

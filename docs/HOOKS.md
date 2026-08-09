# Host Hooks

This is the technical lifecycle reference. Most users should start with
[Connect a coding agent](AGENT_SETUP.md#optional-native-activation).

Code System Graph host integration is optional. Installation never runs a scan, initializes CodeGraph,
reads source, or enables a network provider.

All editable agent integration content lives under
`crates/code-system-graph-hooks/agent-integration-template/`:

```text
agent-integration-template/
├── agent-plugin/       # Agent Plugins 1.0 package and canonical SKILL.md
└── native-hooks/       # signals, selector guidance, fallback rules, gate, and host text
```

The Agent Plugin skill is the only detailed MCP procedure. Native templates select that procedure
or provide a compatibility fallback; they do not copy it. Rust owns protocol-safe merging,
validation, markers, quoting, and atomic writes, but no editable routing prose or shell body.

## Lifecycle

```text
csgraph hooks install --host <host> --workspace <name> --repository <alias> [--root <path>] [--codegraph]
csgraph hooks status --host <host> --workspace <name> --repository <alias> [--root <path>] [--codegraph]
csgraph hooks uninstall --host <host> --workspace <name> --repository <alias> [--root <path>] [--codegraph]
```

Supported host identifiers are `claude-code`, `codex`, `gemini`, `antigravity`, and `cursor`.
Claude Code, Codex, and Gemini receive marker-owned JSON hook entries. Their output selects the
installed Code System Graph skill when available; the hook is not a second detailed procedure.
Antigravity and Cursor receive marker-owned static routing guidance because their supported
contracts do not expose the same prompt event. Do not install those static rules when the packaged
skill is already available.

Pass `--codegraph` only when the MCP server for that agent also uses `--codegraph` and advertises
`explore`. The hook cannot inspect another process's MCP tool list. Omit the flag for both commands
in the native-only profile, and reinstall the hook whenever this policy changes.

Install is atomic, idempotent, and surgical. Existing unrelated configuration is preserved and
changed existing files receive timestamped backups. State files are owner-readable only on Unix.
When the integration root belongs to a Git worktree, the installer also preserves `.gitignore` and
adds `.code-system-graph/` once so its private state stays local. It does not create `.gitignore`
for an unversioned root. Uninstall removes only Code System Graph-owned markers; it leaves any
reusable ignore rule in place.

## Advisory routing

The `code-system-graph-hooks` runtime accepts a bounded top-level host event on stdin, reads only `prompt`
and `session_id`, and emits host-shaped static guidance:

- with `--codegraph`, repository-local symbols and implementation detail route to `explore`,
  while cross-repository work uses Code System Graph first and `explore` only for local detail;
- without `--codegraph`, guidance never recommends `explore` and stays within persisted,
  source-free Code System Graph context;
- exact source returned by `explore` remains ephemeral and is never persisted by Code System Graph.

Prompt text is never persisted or repeated in output. A hash of host, root, and session is retained
for a bounded TTL to suppress duplicate guidance. Malformed or oversized advisory events fail open
with neutral host output.

Dynamic classification uses case-insensitive literal phrases, not model inference. The packaged
signals cover specific English and Spanish graph intents; prompts in other languages may receive
no dynamic guidance. This does not prevent explicit skill use, MCP use, or static fallback rules.
Keep signal phrases narrow: a programming context alone does not make generic fragments such as
`where is`, `dónde`, `code`, or `test` safe selectors.

## Strict mode

`hooks install --strict` additionally installs a marker-owned Git pre-commit block. It invokes
`csgraph changes --scope staged` for the explicitly supplied workspace and registered repository
alias. The commit is blocked when analysis fails or returns no exact staged fingerprint. The hook
does not stage files, execute tests, commit, push, or mutate Code System Graph configuration.

Strict mode is intentionally fail closed. Reinstalling advisory mode removes the owned strict block
without altering unrelated pre-commit commands.

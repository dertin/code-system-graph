# Delivery Interfaces

Code System Graph exposes one bounded application layer through CLI, MCP stdio, and optional HTTP. The
adapters share request/report types and do not reimplement graph, impact, change, or provider
logic.

## Defaults

- CLI output is deterministic and machine-readable when JSON is requested.
- MCP stdio starts with only read-only tools and source-free resources.
- HTTP is not started by `scan`, `status`, `mcp`, or any query command.
- HTTP binds to `127.0.0.1:4767` unless explicitly configured otherwise.
- Administrative MCP tools, remote PR providers, HTTP, and strict hooks are disabled by default.

## CLI

The CLI includes workspace/repository lifecycle, scan/status, exact-schema backup/restore, ranked
search, traversal/trace, communities, impact, local changes, opt-in pull-request inspection,
pull-request listing and overlap, contracts, bounded graph export, doctor diagnostics, explicit
workspace cleanup, hooks, HTTP serving, and shell completions. Scan supports normal changed-input
reuse, one-repository selection that preserves other persisted batches, and explicit forced
recomputation.
Structured commands emit one schema-v2 JSON document on stdout; diagnostics use stderr. `explore`
returns `ToolEnvelope<ExploreReport>` like the other typed HTTP/CLI tools. Graph export emits
the selected complete JSON, GraphML, or Markdown document. Interface failures use stable exit-code
classes for invalid input, not found, conflicts, partial results, unavailable capabilities,
timeouts, internal errors, and cancellation.

## MCP

The read-only catalog covers status, query, trace, impact, changes, contracts, communities,
source-context handoff, and explicitly consented pull-request inspection. Inputs have generated
JSON input schemas and non-overridable server bounds. Each call result contains one human-oriented
Markdown text block plus typed `structuredContent` as its complete machine-readable mirror; no
result `outputSchema` is advertised.

Stable resources:

- `code-system-graph://workspaces`
- `code-system-graph://workspace/{name}/overview`
- `code-system-graph://workspace/{name}/status`
- `code-system-graph://workspace/{name}/repositories`
- `code-system-graph://workspace/{name}/services`
- `code-system-graph://workspace/{name}/contracts`
- `code-system-graph://workspace/{name}/communities`
- `code-system-graph://workspace/{name}/schema`
- `code-system-graph://workspace/{name}/coverage`

Resource template, advertised through `resources/templates/list`:

- `code-system-graph://evidence/{id}`

The server rejects resource access outside its configured workspace. Lists and resource payloads
are bounded `text/markdown` and source-free. The schema catalog uses fenced JSON inside Markdown.
Evidence resources contain identity, provenance, confidence,
locators, extractor version, and observed commit only.

Administrative tools are omitted from discovery unless the server starts in the explicit admin
profile. Enabling admin changes capability discovery; it does not bypass application validation,
writer locking, or audit records.

## HTTP

The optional server exposes `/health`, `/v1/status`, and versioned POST routes for status, query,
trace, impact, changes, contracts, and communities. Non-loopback binding requires a bearer token.
Authentication compares token bytes in constant time and never serializes the configured secret.

Requests have body, concurrency, duration, and per-client rate bounds. Unsupported or mutating
routes return an error. Redirects, CORS widening, source delivery, and implicit administrative
access are not part of the HTTP contract.

## Hooks

Host integration is installed only by an explicit `hooks install` command. Advisory mode adds
brief routing guidance:

- repository-local implementation questions use CodeGraph first;
- cross-repository architecture, contracts, impact, changes, and PR overlap use Code System Graph first;
- exact source retrieval remains a CodeGraph handoff for selected repositories and anchors.

Claude Code, Codex, and Gemini use their documented JSON hook files. Antigravity and Cursor receive
marker-owned guidance because their supported integration surface does not provide the same prompt
hook contract. Hooks do not execute a query on every prompt and never auto-run scans or CodeGraph
initialization. Marker-owned configuration, backups, permissions, TTL state, duplicate detection,
status, and idempotent uninstall are part of the installation contract. Strict staged-change
gating is a separate opt-in pre-commit mode tied to the explicitly supplied registered repository
alias and exact staged state; advisory mode fails open.

## Deliberate limits

- No delivery adapter returns source bodies except the explicitly ephemeral `explore` contract;
  secrets, provider patches, and raw provider responses are never returned.
- GraphML and Markdown exports are deterministic views, not import or round-trip formats.
- Doctor reports unknown when an observation is absent; absence never becomes healthy.
- HTTP does not expose administrative, pull-request, or source-context routes in 1.1.0.
- Hook guidance cannot guarantee that a host follows the suggested provider routing.

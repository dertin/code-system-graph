# Initial Threat Model

## Assets

- Repository paths, ownership, architecture metadata, contract shapes, and change metadata.
- Local SQLite state, workspace manifests, audit records, and optional provider credentials.
- Integrity of evidence, freshness, links, impact results, hooks, and release artifacts.

Source bodies and secret values are explicitly excluded from durable Code System Graph state.

## Trust boundaries

- Workspace manifests, repositories, Git data, documentation, schemas, and parser inputs are
  untrusted.
- CodeGraph MCP/CLI output and remote pull-request provider output are untrusted.
- MCP and HTTP clients are untrusted; administrative capabilities are disabled by default.
- Dependency and release supply chains are external trust boundaries.

## Primary threats and controls

- Path traversal and symlink escape: canonical roots, allowlists, native path handling, and
  race-aware validation.
- Command injection: direct process argument construction with no shell interpolation.
- Parser and graph denial of service: byte, depth, time, node, edge, and output limits with
  cancellation.
- Secret disclosure: denylisted files and fields, value redaction, no source persistence, and
  diagnostic bundles only by explicit command.
- Graph poisoning and false safety: bilateral evidence, provenance, confidence, freshness,
  coverage, ambiguity, and conservative `UNKNOWN` results.
- Prompt injection in indexed text: treat content as inert data and never as agent instruction.
- Network abuse and SSRF: network disabled by default, provider allowlists, loopback HTTP, and
  bearer authentication outside loopback.
- Database corruption and tampering: permissions, foreign keys, transactions, integrity checks,
  backups, versioned migrations, and single-writer locks.
- MCP privilege escalation: read-only default surface, hidden opt-in administration, operation
  annotations, previews where applicable, and local audit events.
- Supply-chain compromise: lockfile, source and license policy, advisories, secret scanning,
  SBOM, checksums, and signed release workflow where available.

## Required validation

Security tests cover paths, symlinks, malformed and oversized parser inputs, process arguments,
MCP limits, redaction, authentication, stale evidence, and malicious Git repositories. Critical
normalizers and parsers receive fuzz targets before release.

# Security policy

## Supported versions

- `1.0.x`: receives security fixes.
- Older versions: none have been published.

Platform support applies only to targets listed in the release notes with completed native
validation.

## Reporting a vulnerability

Do not open a public issue containing exploit details, secrets, private repository data, or
personal information. Use the repository host's private security advisory channel. If that
channel is unavailable, contact the maintainers through the private address published in the
repository metadata.

Include the affected revision, environment, impact, reproduction steps, and any suggested
mitigation. Maintainers should acknowledge a complete report within five business days and
coordinate disclosure after a fix is available. Do not send real private-repository source,
credentials, production databases, or an unreviewed diagnostic bundle.

Maintainers will validate scope, assign severity, prepare a fix and regression coverage, and agree
on a coordinated disclosure date with the reporter. Public disclosure should occur only after
affected users have a practical mitigation or fixed release, unless active exploitation or an
overriding safety concern requires an accelerated notice. Credit is offered when requested and
legally permissible.

## Security expectations

- Never submit real credentials, tokens, source from private repositories, or production data.
- Treat manifests, repositories, Git data, parser inputs, provider output, and indexed documents
  as untrusted.
- Do not add shell interpolation, implicit network access, telemetry, or durable source storage.
- Administrative MCP capabilities remain disabled by default.
- Reject unsafe terminal-control and bidirectional formatting metadata before display or
  persistence, and validate graph references and numeric invariants before publication.
- Create support bundles only with the explicit `csgraph diagnostics --output <new-path>`
  command. Bundles are source-free, never overwrite, and use owner-only mode on Unix; review them
  before sharing.
- Network-capable pull-request providers require enablement and per-call consent. The optional
  HTTP server starts only when requested, binds to loopback by default, and requires authentication
  for non-loopback binds.
- Release policy includes `cargo deny`, `cargo audit`, gitleaks, parser/interface fuzzing,
  CycloneDX SBOM generation, and SHA-256 checksums. A configured workflow is not evidence that a
  target passed.

The initial threat model is in `docs/threat-model/initial.md`.

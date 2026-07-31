# ADR 0009: Evidence-Gated Release Engineering

## Status

Accepted for 1.0.0.

## Context

Code System Graph needs reproducible binaries, dependency provenance, integrity metadata, and a reversible
user-level lifecycle without turning a configured workflow into an unsupported platform claim.
The project is local-first and source-sensitive, so release diagnostics and validation artifacts
must remain source-free.

Local development has produced native Linux x86_64 evidence. Linux ARM64, macOS, and Windows
require equivalent target-specific validation before they can carry public support claims. The
repository currently has no GitHub remote, so configured workflows have not produced hosted
evidence.

## Decision

The workspace and release artifacts use version `1.0.0`. Release builds use the latest stable Rust
toolchain and the committed lockfile. Every crate declares an MSRV of 1.97.1, which is checked
separately. Formatting and strict Clippy checks use the latest nightly toolchain, while compilation,
tests, documentation, and release gates use stable.

The Linux package path:

- builds `csgraph` and `code-system-graph-hooks` with `cargo build --release --locked`;
- stages binaries, license, notices, security policy, documentation, and lifecycle scripts;
- normalizes tar entry order, timestamps, ownership, and numeric owner metadata;
- emits a CycloneDX JSON SBOM with Syft;
- emits SHA-256 checksums for the archive and SBOM.

The prefix installer defaults to `$HOME/.local`, backs up replaced binaries, installs through
temporary files, writes a restrictive owned manifest, and supports repeated installation as an
upgrade. The uninstaller removes only allowlisted manifest entries and retains backups and user
workspace data.

The release workflow defines independent build and test jobs for Linux x86_64 and ARM64, macOS
x86_64 and ARM64, and Windows x86_64. Workflow presence records intended coverage only. A platform
is release-validated only after a native job or equivalent controlled evidence passes and is
retained. The current completed package and installer evidence is limited to Linux x86_64.

The two binary crates declare Binstall metadata for the official combined archive. QuickInstall
and source-compilation fallbacks are disabled so installation cannot silently switch to a
third-party artifact or require the workspace MSRV. Binstall becomes the recommended installation
path only after the crates and matching release assets have been published and verified.

Release evidence includes formatting, Clippy, workspace tests, documentation warnings,
`cargo deny`, `cargo audit`, gitleaks, parser and public-interface fuzzing, scale workloads,
security tests, package smoke, and SBOM/checksum verification. Commands and measured results are
documented in `../RELEASE.md` and `../PERFORMANCE.md`.

Diagnostic bundles are explicit, source-free, non-overwriting, and owner-only on Unix. They may be
shared for support only after human review.

Hosted publication is separate from build and validation. Release records must identify the target,
toolchain, locked dependency graph, validation environment, package, SBOM, checksums, and signing
method. A configured but unexecuted workflow is never evidence of platform support.

Releases are initiated manually from a clean `main` branch. The release script pushes a signed tag,
publishes workspace crates in dependency order, and dispatches the GitHub Actions workflow for that
existing tag. The workflow validates, builds, attests, and publishes the GitHub Release. Repeated
execution resumes safely when only part of the crates.io publication completed.

## Consequences

- Linux x86_64 currently has measured scale, fuzz, policy, package, and lifecycle evidence.
- The archive and SBOM can be checked for transport integrity, but checksums alone do not
  authenticate the publisher.
- Native macOS and Windows build, test, package, and lifecycle validation must pass before support
  is published for those platforms.
- The POSIX shell lifecycle scripts do not provide native Windows installation evidence.
- Local Linux results cannot be generalized to another operating system or architecture.
- Build hosts require Rust, the requested target, Syft, tar, and SHA-256 tooling. Archive users do
  not require a Rust toolchain. Binstall users require Cargo and `cargo-binstall`, but not a Rust
  compiler compatible with the workspace MSRV.
- Validation must be repeated when release behavior, dependencies, packaging, or target support
  changes.
- GitHub-hosted evidence, tags, and release artifacts cannot exist until a remote is configured.

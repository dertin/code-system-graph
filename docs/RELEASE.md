# Release Engineering

This guide covers validation, packaging, and publication for Code System Graph. The source
version is `1.2.1`. Continuous integration runs in the
[public repository](https://github.com/dertin/code-system-graph/actions).

Platform claims below require native build, test, packaging, and archive-smoke evidence from the
release workflow.

## Platform validation

- Linux x86_64 (`x86_64-unknown-linux-gnu`): locally validated for build, test, scale workloads,
  package creation, checksum and SBOM generation, installation, repeated installation, and
  uninstall.
- Linux ARM64 (`aarch64-unknown-linux-gnu`): native CI validates build, tests, and archive contents.
- macOS x86_64 and ARM64: native CI validates build, serial tests, archive installation, repeated
  installation, and uninstall.
- Windows x86_64: native CI validates build, serial tests, ZIP contents, and binary startup.
  `cargo-binstall` installs the ZIP directly; the POSIX shell installer is not a Windows installer.

Performance evidence remains Linux x86_64-specific and does not imply characteristics on another
target.

## Included capabilities

Code System Graph includes:

- multi-repository workspace registration with lossless native-path identity;
- incremental, source-free extraction for package, HTTP, event, GraphQL, RPC, data,
  infrastructure, documentation, ownership, configuration, test, and implementation boundaries;
- one current graph per workspace published as a delta, a per-file stat cache, one read per changed
  file, and parallel extraction whose output is identical for every worker count;
- cross-language HTTP linking by canonical route shape with concrete-path matching, router
  prefixes composed across files, evaluated client URLs, client wrappers, test helpers and
  fixtures, in-process test clients, and per-repository `authorities`;
- an HTTP link report of linked, provider-less, ambiguous, and external calls in `status`, query
  results, and MCP coverage;
- deterministic linking, exact manual links and suppressions, and freshness;
- bounded search, trace, community analysis, compatibility, impact, and change analysis;
- opt-in CodeGraph integration through public MCP or CLI contracts;
- configurable per-repository corroboration bounds and opt-in Git-native ignore discovery;
- portable Agent Plugins 1.0.0 generation and ownership-checked local bindings for versioned base
  plugins with a read-only MCP and existing routing guidance;
- schema-v2 JSON over CLI/HTTP and one canonical schema-6 text block over MCP, with JSON
  as the default and equivalent Markdown available through `--response-format markdown`;
- bounded Explore source, symbols, callers/callees, federated handoffs, coverage, actions, and
  execution accounting;
- one immutable global `executionPolicy` covering scan, Explore, Query, tools, and resources, with
  separate scan and agent-delivery fingerprints;
- CLI, read-only MCP stdio, optional authenticated HTTP, exports, diagnostics, and host hooks;
- exact-schema SQLite backup, restore, and integrity validation;
- deterministic Linux package archives, CycloneDX SBOMs, and SHA-256 checksums.

## Local validation

Run the main Rust validation from a clean checkout. This requires stable, nightly with the rustfmt
and Clippy components, and Rust 1.96.0:

```text
cargo +nightly fmt --all -- --check
cargo +nightly clippy --workspace --exclude code-system-graph-fuzz --all-targets --all-features --locked -- -D warnings
cargo +stable check --workspace --exclude code-system-graph-fuzz --all-targets --all-features --locked
cargo +stable test --workspace --exclude code-system-graph-fuzz --all-targets --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo +stable doc --workspace --exclude code-system-graph-fuzz --all-features --no-deps --locked
cargo +1.96.0 check --workspace --exclude code-system-graph-fuzz --all-targets --all-features --locked
```

Dependency and source-policy tools can then run against the locked workspace:

```text
cargo deny check
cargo audit --deny warnings
```

When refreshing registry dependencies before a release, use
[cargo-cooldown](https://github.com/dertin/cargo-cooldown) with the workspace `cooldown.toml`
policy instead of plain `cargo update`:

```text
cargo install --locked cargo-cooldown
cargo cooldown update
```

The release-mode scale workloads are intentionally ignored during the normal test suite:

```text
cargo test -p code-system-graph-core --test scale_performance --release -- --ignored --nocapture
cargo test -p code-system-graph --test scale_registry_e2e --release -- --ignored --nocapture
```

See `PERFORMANCE.md` for workload definitions, measurements, and interpretation.

## Package validation

Creating the Linux x86_64 artifacts requires the latest stable Rust toolchain, the target, Syft,
GNU tar, and SHA-256 tooling. The workspace MSRV remains 1.96.0 and is validated separately:

```text
SOURCE_DATE_EPOCH=0 scripts/package-release.sh x86_64-unknown-linux-gnu
scripts/smoke-install.sh dist/code-system-graph-x86_64-unknown-linux-gnu-v1.2.1
sha256sum --check dist/code-system-graph-x86_64-unknown-linux-gnu-v1.2.1.sha256
```

The package contains `csgraph`, `code-system-graph-hooks`, the visible Agent integration template
tree (portable plugin plus native adapters), public documentation, license and notice files, and
install/uninstall scripts. The packaging command also emits a CycloneDX JSON SBOM and a checksum
file covering the archive and SBOM.

Both binary crates declare `cargo-binstall` metadata for this archive layout. The configuration
accepts only the official release archive and disables both QuickInstall and source-compilation
fallbacks. Publish both crates, attach the archive at the exact URL declared by their metadata,
and verify installation of both binaries from the public release before announcing the release.

The README and [Installation](INSTALLATION.md) recommend `cargo binstall` for prebuilt Linux
x86_64 binaries and `cargo install` from crates.io as the public installation paths:

```text
cargo binstall code-system-graph code-system-graph-hooks
```

This path requires Cargo and `cargo-binstall`, but it does not require a Rust compiler compatible
with the workspace MSRV.

SHA-256 verifies integrity relative to the checksum file; it does not authenticate a publisher.
See `INSTALLATION.md` for verification, installation, upgrade, rollback, and uninstall behavior.

## Manual release workflow

The release entry point follows the same tag-first process used by `cargo-cooldown`. It requires a
clean `main` branch aligned with `origin/main`, Cargo credentials for crates.io, authenticated `gh`,
`jq`, and a configured Git signing key. Run the safe preparation mode first; omitting the mode also
selects `prepare`:

```text
.github/workflows/release.sh 1.2.1 prepare
```

Preparation runs the complete publish-readiness suite and dry-runs all five packages without
creating a tag, publishing a crate, or dispatching a workflow. To perform the irreversible release,
pass `publish` explicitly:

```text
.github/workflows/release.sh 1.2.1 publish
```

Publish mode verifies that the workspace repository matches `origin`, creates and pushes the
signed tag, publishes the five crates in dependency order, waits for each dependency to become
available on crates.io, and dispatches `release.yml` with the existing tag. Rerunning the command
is safe after a partial crates.io publication because versions already present are skipped.

The workflow can also be dispatched manually from GitHub Actions with an existing `vX.Y.Z` tag. It
validates the tag and workspace version, runs the release tests and policy gates, builds native
archives for each configured target, generates the CycloneDX SBOM and checksums, attests every
asset, and creates or updates the GitHub Release. Dispatching only the workflow does not publish
the crates to crates.io, so the script remains the standard full-release entry point.

## Publication requirements

Before publishing artifacts:

1. reproduce the validation commands from a clean checkout;
2. retain the package, SBOM, checksums, and validation output;
3. establish publisher authentication and signing procedures;
4. confirm GitHub Actions release workflows succeed on `main` for the candidate commit;
5. complete native macOS and Windows validation before advertising support for those platforms.
6. publish the binary crates and verify their Binstall metadata against the attached release
   archives before changing the installation command.

Tags, crates.io packages, and GitHub Release assets are created only through the controlled
release script after the candidate commit passes the publication gates.

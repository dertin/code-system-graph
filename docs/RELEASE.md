# Code System Graph 1.0.0 Release

Code System Graph 1.0.0 is the first public release line. The source tree and package version are `1.0.0`.
Release validation has completed locally on native Linux x86_64.

The repository does not currently have a GitHub remote, so no GitHub-hosted build, test, tag, or
release result exists. Platform claims below distinguish completed local evidence from configured
but unexecuted automation.

## Platform validation

- Linux x86_64 (`x86_64-unknown-linux-gnu`): locally validated for build, test, scale workloads,
  package creation, checksum and SBOM generation, installation, repeated installation, and
  uninstall.
- Linux ARM64 (`aarch64-unknown-linux-gnu`): workflow coverage is configured but has not been
  executed on a native release host.
- macOS x86_64 and ARM64: workflow coverage is configured but has not been executed. Native macOS
  build, test, package, and lifecycle validation must pass before macOS support is published.
- Windows x86_64: workflow coverage is configured but has not been executed. Native Windows build,
  test, packaging, and installation validation must pass before Windows support is published. The
  POSIX shell installer is not a native Windows installer.

Linux x86_64 evidence does not imply support or performance characteristics on another target.

## Included capabilities

Code System Graph 1.0.0 includes:

- multi-repository workspace registration with lossless native-path identity;
- incremental, source-free extraction for package, HTTP, event, GraphQL, RPC, data,
  infrastructure, documentation, ownership, configuration, test, and implementation boundaries;
- deterministic linking, exact manual links and suppressions, immutable snapshots, and freshness;
- bounded search, trace, community analysis, compatibility, impact, and change analysis;
- opt-in CodeGraph integration through public MCP or CLI contracts;
- CLI, read-only MCP stdio, optional authenticated HTTP, exports, diagnostics, and host hooks;
- exact-schema SQLite backup, restore, and integrity validation;
- deterministic Linux package archives, CycloneDX SBOMs, and SHA-256 checksums.

## Local validation

Run the main Rust validation from a clean checkout. This requires stable, nightly with the rustfmt
and Clippy components, and Rust 1.97.1:

```text
cargo +nightly fmt --all -- --check
cargo +nightly clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo +stable check --workspace --all-targets --all-features --locked
cargo +stable test --workspace --all-targets --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo +stable doc --workspace --all-features --no-deps --locked
cargo +1.97.1 check --workspace --all-targets --all-features --locked
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
GNU tar, and SHA-256 tooling. The workspace MSRV remains 1.97.1 and is validated separately:

```text
SOURCE_DATE_EPOCH=0 scripts/package-release.sh x86_64-unknown-linux-gnu
scripts/smoke-install.sh dist/code-system-graph-x86_64-unknown-linux-gnu-v1.0.0
sha256sum --check dist/code-system-graph-x86_64-unknown-linux-gnu-v1.0.0.sha256
```

The package contains `csgraph`, `code-system-graph-hooks`, public documentation, license and notice files,
and install/uninstall scripts. The packaging command also emits a CycloneDX JSON SBOM and a
checksum file covering the archive and SBOM.

Both binary crates declare `cargo-binstall` metadata for this archive layout. The configuration
accepts only the official release archive and disables both QuickInstall and source-compilation
fallbacks. Before recommending Binstall, publish both crates, attach the archive at the exact URL
declared by their metadata, and verify installation of both binaries from the public release.

Until that public verification succeeds, the user documentation must continue to recommend
installation from a trusted checkout. Afterward, replace that command with:

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
`jq`, and a configured Git signing key:

```text
.github/workflows/release.sh 1.0.0
```

The script verifies that the workspace repository matches `origin`, creates and pushes the signed
tag, publishes the five crates in dependency order, waits for each dependency to become available
on crates.io, and dispatches `release.yml` with the existing tag. Rerunning the script is safe after
a partial crates.io publication because versions already present are skipped.

The workflow can also be dispatched manually from GitHub Actions with an existing `vX.Y.Z` tag. It
validates the tag and workspace version, runs the release tests and policy gates, builds native
archives for each configured target, generates the CycloneDX SBOM and checksums, attests every
asset, and creates or updates the GitHub Release. Dispatching only the workflow does not publish
the crates to crates.io, so the script remains the standard full-release entry point.

## Recorded Linux x86_64 evidence

The synthetic graph workload completed with 100,000 nodes and 500,000 confirmed edges:

- query p95: 48 ms over five searches;
- trace p95: below 1 ms at whole-millisecond resolution over five bounded traces;
- summary impact p95: 824 ms over five depth-one analyses;
- connected-components analysis: 337 ms;
- resident memory after the workload: 497,444 KiB.

The generated 100-repository workload discovered 600 inputs. A targeted scan after one repository
change completed in 165 ms, retained 595 unchanged inputs and at least 99 unchanged repositories,
and produced an 11 ms status p95 over ten samples.

Fuzzing, dependency policy, package lifecycle, backup and recovery, source-free diagnostics, and
the complete Rust test suite also passed locally on Linux x86_64. These observations are release
evidence from one environment, not universal latency or memory guarantees.

## Publication requirements

Before publishing artifacts:

1. reproduce the validation commands from a clean checkout;
2. retain the package, SBOM, checksums, and validation output;
3. establish publisher authentication and signing procedures;
4. create and verify the intended GitHub remote before relying on hosted workflows;
5. complete native macOS and Windows validation before advertising support for those platforms.
6. publish the binary crates and verify their Binstall metadata against the attached release
   archives before changing the installation command.

Tagging, hosted artifacts, and release pages can only be verified after a remote exists.

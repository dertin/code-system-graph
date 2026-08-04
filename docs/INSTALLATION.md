# Installation, Upgrade, and Uninstall

This guide is for people who want to run Code System Graph. Packaging and publication procedures
for maintainers are in [Release engineering](RELEASE.md).

## Current availability

Code System Graph `1.0.1` is published on [crates.io](https://crates.io/crates/code-system-graph)
and [GitHub Releases](https://github.com/dertin/code-system-graph/releases). Native release CI
validates Linux x86_64/ARM64, macOS x86_64/ARM64, and Windows x86_64 before their archives are
published.

## Install with cargo-binstall (recommended)

Requirements:

- Cargo;
- [cargo-binstall](https://github.com/cargo-bins/cargo-binstall);
- a supported target: Linux x86_64/ARM64, macOS x86_64/ARM64, or Windows x86_64.

No Rust compiler is required. cargo-binstall downloads the official release archive declared by the
crate metadata and installs:

- `csgraph`, the CLI and MCP server;
- `code-system-graph-hooks`, the optional agent-routing runtime.

```bash
cargo binstall code-system-graph code-system-graph-hooks
```

The crate name is `code-system-graph`; the CLI you run is `csgraph`. The hooks runtime installs as
`code-system-graph-hooks`.

Cargo normally writes both binaries to `$HOME/.cargo/bin`. Add that directory to `PATH`, then
verify:

```bash
csgraph --version
command -v code-system-graph-hooks
```

The second binary is an internal runtime, not a user-facing CLI. It is required only if you install
agent routing hooks, but installing both avoids a later partial setup. Manage it through
`csgraph hooks ...`.

Do not substitute an unofficial download URL or third-party binary mirror.

## Install from crates.io

Requirements:

- the latest stable Rust toolchain; the minimum supported Rust version (MSRV) is 1.97.1;
- Cargo.

```bash
cargo install code-system-graph code-system-graph-hooks
```

This builds and installs the same binaries as the binstall path. Verify with the commands above.

## Install from a trusted checkout

Requirements:

- the latest stable Rust toolchain; MSRV is 1.97.1;
- Cargo and Git;
- a trusted checkout of this repository.

From the repository root:

```bash
cargo +stable install --locked --path crates/code-system-graph-cli && cargo +stable install --locked --path crates/code-system-graph-hooks
```

Use this path for unreleased changes or local development.

## Install a prebuilt release archive

Download the archive for your exact target, the release CycloneDX SBOM, and `SHA256SUMS` from the
[official release page](https://github.com/dertin/code-system-graph/releases). Keep all three files
in one directory.

For a Linux x86_64 archive:

```bash
sha256sum --ignore-missing --check SHA256SUMS
tar -xzf code-system-graph-x86_64-unknown-linux-gnu-v1.0.1.tgz
PREFIX="$HOME/.local" ./code-system-graph-x86_64-unknown-linux-gnu-v1.0.1/install.sh
```

Replace the target in the archive name with `x86_64-apple-darwin`,
`aarch64-apple-darwin`, or `aarch64-unknown-linux-gnu` as appropriate. Unix archives include the
same installer.

The Windows archive is a ZIP file. Verify `SHA256SUMS`, extract
`code-system-graph-x86_64-pc-windows-msvc-v1.0.1.zip`, and add its `bin` directory containing
`csgraph.exe` and `code-system-graph-hooks.exe` to `PATH`.

`PREFIX` defaults to `$HOME/.local`. The installer places binaries under `$PREFIX/bin`, installed
documentation under `$PREFIX/share/doc/code-system-graph`, and a private ownership manifest under
`$PREFIX/share/code-system-graph`.

Add `$PREFIX/bin` to `PATH` if necessary and verify:

```bash
csgraph --version
```

SHA-256 verifies the files against the downloaded checksum; it does not prove publisher identity.
Do not substitute an unofficial archive or download URL.

## What installation does not do

Installing the binaries does not:

- scan any repository or create a graph database;
- create a workspace manifest;
- detect or modify coding-agent configuration;
- start MCP, HTTP, or background processes;
- install the independent [CodeGraph](https://github.com/colbymchenry/codegraph) project;
- access the network or a remote pull-request provider.

Continue with [Create your first workspace](GETTING_STARTED.md). To add repository-level symbol and
implementation context afterward, follow [Use CodeGraph with a workspace](CODEGRAPH_INTEGRATION.md).

## Upgrade

### cargo-binstall installation

```bash
cargo binstall --force code-system-graph code-system-graph-hooks
```

### crates.io installation

```bash
cargo install code-system-graph code-system-graph-hooks
```

Cargo replaces the installed binaries when a newer version is available on crates.io.

### Source installation

Update the trusted checkout, inspect the changes, and rerun the one-line source installation. Cargo
replaces the installed binaries.

### Release archive

Verify and extract the newer official package, then run its `install.sh` with the same `PREFIX`.
The package installer preserves replaced binaries under:

```text
$PREFIX/share/code-system-graph/backups/
```

The 1.0.x release line supports one exact initial database schema. An incompatible local database is
disposable: remove it and run a full scan. Backup and restore accept only that exact schema and
never migrate it.

## Uninstall

### Cargo or cargo-binstall installation

```bash
cargo uninstall code-system-graph
cargo uninstall code-system-graph-hooks
```

### Release archive

Run `uninstall.sh` from the verified extracted package with the same prefix:

```bash
PREFIX="$HOME/.local" ./code-system-graph-x86_64-unknown-linux-gnu-v1.0.1/uninstall.sh
```

Before uninstalling either installation type, remove any optional agent hooks:

```bash
csgraph hooks uninstall --host <agent> --workspace <workspace> --repository <alias>
```

Uninstalling binaries does not delete:

- `code-system-graph.yaml`;
- `.code-system-graph/` databases;
- agent configuration not owned by Code System Graph;
- retained package backups.

Delete workspace data separately only after confirming that it is no longer needed.

## Platform status

| Platform | Status |
| --- | --- |
| Linux x86_64 | Native CI validates build, tests, archive lifecycle, binstall, and uninstall |
| Linux ARM64 | Native CI validates build, tests, and archive contents |
| macOS x86_64 / ARM64 | Native CI validates build, serial tests, archive install, and uninstall |
| Windows x86_64 | Native CI validates build, serial tests, ZIP contents, and binary startup |
| Windows ARM64 | Not in the current release workflow |

See [Release engineering](RELEASE.md) for the evidence and publication requirements behind this
matrix.

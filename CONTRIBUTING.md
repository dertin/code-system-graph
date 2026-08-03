# Contributing

Thank you for contributing to Code System Graph. Bug reports, documentation improvements, tests, and
focused code changes are welcome.

## Licensing and originality

Contributions must be your original work or derived from material whose license is compatible with
Apache-2.0. Do not submit proprietary code, confidential information, trade secrets, or material
that you do not have the right to license.

Disclose the source and license of any adapted material in the pull request. Preserve applicable
copyright, attribution, and license notices. By submitting a contribution, you represent that you
have the right to provide it under the project's Apache-2.0 license.

## Development workflow

Formatting and Clippy use the latest nightly Rust toolchain. Compilation, tests, documentation,
and release builds use the latest stable toolchain. The workspace MSRV is 1.97.1, declared by
every crate through the workspace package metadata and checked separately in CI.

Install the additional quality toolchain once:

```text
rustup toolchain install nightly --profile minimal --component rustfmt --component clippy
```

Create a focused branch, keep each change reviewable, and include tests and documentation that
describe the resulting public behavior. Before opening a pull request, run:

```text
cargo +nightly fmt --all -- --check
cargo +nightly clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo +stable check --workspace --all-targets --all-features --locked
cargo +stable test --workspace --all-targets --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo +stable doc --workspace --all-features --no-deps --locked
cargo deny check
```

Changes must also compile with the MSRV:

```text
cargo +1.97.1 check --workspace --all-targets --all-features --locked
```

## Dependency updates

Registry dependency updates use [cargo-cooldown](https://github.com/dertin/cargo-cooldown) so
`Cargo.lock` does not pick up releases that are too new. Policy lives in `cooldown.toml` at the
workspace root:

- default cooldown: 14 days for crates.io packages;
- TLS, SSL, and cryptography-related crates: 2 days (see `[[allow.package]]` entries).

Install once:

```text
cargo install --locked cargo-cooldown
```

Refresh dependencies under cooldown instead of plain `cargo update`:

```text
cargo cooldown update
```

Use `cargo cooldown check`, `build`, `test`, or `run` when you want the same guard before local
work. CI and release validation continue to use plain Cargo with the committed `Cargo.lock`.

The fuzz workspace is a root workspace member and shares the repository `Cargo.lock`. The fuzz CI
workflow runs `scripts/validate-lockfile-cooldown.sh` with `COOLDOWN_LOCKFILE_BASELINE=ignore` so
every registry package in the lockfile must satisfy `cooldown.toml` before `cargo fuzz` downloads or
builds dependencies.

Add focused tests for relevant success and failure paths. For graph and provider behavior, cover
stale, incomplete, ambiguous, bounded, timeout, or cancellation outcomes when applicable.

## Code and documentation

- Follow the workspace formatting and lint configuration.
- Keep public behavior deterministic and preserve explicit freshness, coverage, and ambiguity.
- Avoid persisting source bodies, secret values, credentials, or remote provider payloads.
- Document public Rust APIs with English rustdoc.
- Write code comments and user documentation in English.
- Explain non-obvious constraints and tradeoffs instead of restating the code.
- Update `README.md`, `CHANGELOG.md`, or the relevant document under `docs/` when public behavior
  changes.

## Pull requests

A pull request should explain the problem, the chosen approach, user-visible changes, and the
validation performed. Keep unrelated refactoring separate. Call out compatibility, privacy,
security, storage, or migration implications explicitly.

Never include secrets, private source fixtures, release credentials, or dependencies with
incompatible licenses. Report suspected vulnerabilities privately according to `SECURITY.md`
instead of opening a public issue.

All contributions are reviewed for correctness, scope, tests, documentation, licensing, privacy,
and security before acceptance.

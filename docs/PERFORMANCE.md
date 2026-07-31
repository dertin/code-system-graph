# Performance and Scale Evidence

## Scope

Code System Graph has explicit release-mode acceptance workloads for graph analytics and 100-repository
incremental scanning. The measurements below were collected locally on Linux x86_64. They are
release evidence, not service-level objectives or guarantees for other hardware, operating
systems, repository contents, or graph topologies.

## Graph workload

The synthetic federated workload assigns 100,000 HTTP-operation nodes across 100 repositories and
connects them with 500,000 confirmed remote-call edges. The ignored acceptance test must run in a
release profile:

```text
cargo test -p code-system-graph-core --test scale_performance --release -- --ignored --nocapture
```

The measured operations are:

- five deterministic searches, reported as p95;
- five depth-eight traces from `node:0` to `node:8`, reported as p95;
- five downstream, depth-one, summary-only impact analyses, reported as p95;
- one federated connected-components analysis;
- Linux process resident memory read from `VmRSS` in `/proc/self/status`.

Measured Linux x86_64 result:

- query p95: 48 ms, below the 500 ms gate;
- trace p95: 0 ms, below the 1 second gate;
- summary impact p95: 824 ms, below the 2 second gate;
- connected-components analysis: 337 ms;
- resident memory: 497,444 KiB.

With five samples, p95 selects the slowest observed sample. The zero-millisecond trace result means
the elapsed duration rounded down when represented as whole milliseconds; it does not mean the
operation consumed no time.

## Registry and incremental workload

The registry acceptance test creates 100 repositories, each with a Cargo manifest and Rust source
file, performs an initial scan, changes one source file, and targets the changed repository:

```text
cargo test -p code-system-graph --test scale_registry_e2e --release -- --ignored --nocapture
```

After registry discovery optimization, the measured Linux x86_64 result was:

- initially discovered inputs: 600;
- targeted incremental scan: 165 ms;
- changed inputs: 5;
- unchanged inputs retained: 595;
- unchanged repositories retained: at least 99;
- status p95 over ten samples: 11 ms, below the 200 ms gate.

The changed-input count includes the source file and the source-owned extractor observations
affected by that repository change; it is not a count of changed repositories.

## Interpretation and reproducibility

- Always use `--release`; both tests reject debug builds.
- Record the Git revision, Rust version, target triple, operating system, sample counts, and raw
  test output with candidate evidence.
- Run on an otherwise representative workstation and disclose material contention or resource
  limits.
- Compare identical workloads. These tests exercise in-memory graph analytics and a generated
  filesystem registry; they do not model every production repository, SQLite history, remote
  provider, CodeGraph, or network condition.
- Re-run after changes to discovery, persistence, graph construction, query, traversal, impact, or
  community algorithms.

The repository does not currently have a GitHub remote, so cross-platform workflow results are not
available. No performance result is claimed for Linux ARM64, macOS, or Windows. Native validation
is required before publishing performance or support claims for macOS or Windows.

# Performance and Scale Evidence

## Scope

Code System Graph has explicit release-mode acceptance workloads for graph analytics, 100-repository
incremental scanning, and 200-repository synchronization. The measurements below were collected
locally on Linux x86_64. They are
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

- query p95: 43 ms, below the 500 ms gate;
- trace p95: 0 ms, below the 1 second gate;
- summary impact p95: 704 ms, below the 2 second gate;
- connected-components analysis: 313 ms;
- resident memory: 401,108 KiB.

With five samples, p95 selects the slowest observed sample. The zero-millisecond trace result means
the elapsed duration rounded down when represented as whole milliseconds; it does not mean the
operation consumed no time.

## Registry and incremental workload

The registry acceptance test creates 100 repositories, each with a Cargo manifest and Rust source
file, performs an initial scan, changes one source file, and targets the changed repository:

```text
cargo test -p code-system-graph --test scale_registry_e2e --release -- --ignored --nocapture
```

Measured Linux x86_64 result:

- initially discovered inputs: 600;
- targeted incremental scan: 145 ms;
- changed inputs: 5;
- unchanged inputs retained: 595;
- unchanged repositories retained: at least 99;
- status p95 over ten samples: 33 ms, below the 200 ms gate.

The changed-input count includes the source file and the source-owned extractor observations
affected by that repository change; it is not a count of changed repositories.

## Extraction-budget workload

The extraction acceptance test creates one repository with 5,000 files: 1,000 each of GraphQL,
Protobuf, Maven XML, JavaScript, and generated-client `FILES` manifests. It measures one direct
representative extractor invocation per file, then performs a complete persisted scan:

```text
cargo test -p code-system-graph --test extraction_scale_e2e --release --locked -- --ignored --nocapture
```

Measured Linux x86_64 result on October 2, 2026:

- files: 5,000;
- discovered artifact-extractor invocations: 9,000, because JavaScript files intentionally run
  through multiple focused extractors;
- complete scan: 1,190 ms;
- representative per-artifact extraction p50: 3 microseconds;
- representative per-artifact extraction p95: 10 microseconds;
- representative per-artifact extraction p99: 14 microseconds;
- test-process peak resident memory from Linux `VmHWM`: 23,820 KiB.

The memory value comes from the release test process after the scan. It excludes Cargo and compiler
processes. The per-artifact samples measure parser extraction only; the complete-scan figure also
includes discovery, fingerprinting, linking, community analysis, and SQLite publication.

## Synchronization workload

The synchronization acceptance test generates 200 repositories with 250 files each (50,000 files):
an Express server, a `fetch` client of the next repository, a Python `requests` test, and
TypeScript, Python, and Go modules. Every file is written with a modification time older than the
stat-cache racy window. The test runs a cold scan, an unchanged scan, a scan after adding one
route to one server file, and 20 further syncs that alternate that file between two contents:

```text
cargo test -p code-system-graph --test sync_scale_e2e --release -- --ignored --nocapture
```

Set `CODE_SYSTEM_GRAPH_SCALE_WORKLOAD` to a directory to generate the workload there instead of a
temporary directory, so another build can scan the same files. The test enforces these gates:

- the unchanged scan reads 0 content bytes and reuses the published snapshot;
- the one-file sync publishes no more rows than the changed repository segment owns: its nodes,
  every edge touching them, its evidence, fingerprints, extractor batches, and extractor runs;
- the database (main file plus WAL) after the 20 syncs stays within 5% of its size after the
  one-file sync.

Measured Linux x86_64 result on October 2, 2026:

- cold scan: 8,204 ms, 15,481,200 content bytes read, peak worker resident memory 695,578,624
  bytes;
- unchanged scan: 680 ms, 0 content bytes read, 50,000 stat-cache hits;
- one-file sync: 2,034 ms, 634 content bytes read, 2,022 published rows against a 2,532-row
  segment;
- database: 299,958,272 bytes after the one-file sync and 299,970,560 bytes after 20 more syncs.

An unchanged scan compares the snapshot identity, a hash of the manifest, the extraction contract,
the budgets, and every artifact fingerprint, with the current snapshot, so it loads neither the
previous fingerprints nor any extractor batch. An incremental scan publishes artifact rows as a
delta planned against the stored fingerprints instead of comparing every stored row.

`ExecutionSummary` reports `contentBytesRead` and `publishedRows` for every scan, so the same
figures are available from `csgraph scan` output outside the test.

## Interpretation and reproducibility

- Always use `--release`; scale acceptance tests reject debug builds.
- Record the Git revision, Rust version, target triple, operating system, sample counts, and raw
  test output with candidate evidence.
- Run on an otherwise representative workstation and disclose material contention or resource
  limits.
- Compare identical workloads. These tests exercise in-memory graph analytics and a generated
  filesystem registry; they do not model every production repository, SQLite history, remote
  provider, CodeGraph, or network condition.
- Re-run after changes to discovery, persistence, graph construction, query, traversal, impact, or
  community algorithms.

The canonical repository is `https://github.com/dertin/code-system-graph`. Linux x86_64 has native
performance-workload evidence. Release CI validates supported Linux, macOS, and Windows targets,
but those platform checks do not establish performance characteristics. No performance result is
claimed for targets without equivalent measured workload evidence.

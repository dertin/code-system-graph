# ADR 0003: SQLite storage

- Status: Accepted
- Date: 2026-07-29

## Context

Code System Graph is local-first and requires transactional snapshots, full-text search, portable
installation, concurrent readers, and deterministic recovery.

## Decision

Code System Graph uses bundled SQLite through `rusqlite`. Every connection enables foreign keys, WAL,
and a busy timeout. The unpublished 1.0.0 binary embeds one definitive initial schema and records its
exact version in `schema_metadata`.

Extractor output is immutable data until an application transaction atomically replaces the
batch keyed by repository snapshot, extractor, and source artifact. A snapshot becomes current
only after all selected extractors and linkers succeed. The previous valid snapshot therefore
survives interruption.

SQLite calls execute through a dedicated blocking boundary. The schema uses relational columns
for stable and queried attributes; JSON is reserved for genuinely variant attributes. FTS5
indexes user-facing labels and descriptions.

Repository and checkout paths use a platform tag plus lossless native bytes; lossy display paths
are diagnostic-only. Before the first public release, only the definitive 1.0.0 schema is accepted;
incompatible development databases are rebuilt. Online backups use SQLite's backup API and do not
overwrite an existing destination. A restrictive sidecar lock provides single-writer ownership and
bounded stale-lock recovery.

Restore validates the selected source's exact schema and preserves the replaced database as a
separately named safety backup. Migration policy will be designed after a schema has shipped.

Writer-lock metadata includes PID and process start time. Recovery requires both expiry and proof
that the original process instance is no longer alive, preventing long-running writers or reused
PIDs from being mistaken for stale locks.

## Consequences

- A single writer and WAL permit bounded concurrent readers.
- Backup, exact-schema validation, lock recovery, and crash behavior require integration tests.
- Source bodies and secret values are prohibited from durable storage.

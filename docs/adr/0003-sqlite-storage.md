# ADR 0003: SQLite storage

- Status: Accepted
- Date: 2026-07-29

## Context

Code System Graph is local-first and requires transactional snapshots, full-text search, portable
installation, concurrent readers, and deterministic recovery.

## Decision

Code System Graph uses bundled SQLite through `rusqlite`. Every connection enables foreign keys, WAL,
and a busy timeout. Each binary embeds one exact schema and records its identity, a BLAKE3 digest of
the schema definition, in `schema_metadata`.

Each workspace owns one current graph. Extractor output is immutable data until an application
transaction publishes the candidate graph as a delta against the stored rows: only inserted,
changed, and removed rows are written. A snapshot becomes current only after all selected
extractors and linkers succeed. The previous valid graph therefore survives interruption, and the
database does not grow with the number of scans.

SQLite calls execute through a dedicated blocking boundary. The schema uses relational columns
for stable and queried attributes; JSON is reserved for genuinely variant attributes. FTS5
indexes user-facing labels and descriptions.

Repository and checkout paths use a platform tag plus lossless native bytes; lossy display paths
are diagnostic-only. Only the exact embedded schema is accepted; other local databases are removed
and rebuilt by a full scan. Online backups use SQLite's backup API and do not
overwrite an existing destination. A restrictive sidecar lock provides single-writer ownership and
bounded stale-lock recovery.

Restore validates the selected source's exact schema and preserves the replaced database as a
separately named safety backup.

Writer-lock metadata includes PID and process start time. Recovery requires both expiry and proof
that the original process instance is no longer alive, preventing long-running writers or reused
PIDs from being mistaken for stale locks.

## Consequences

- A single writer and WAL permit bounded concurrent readers.
- Backup, exact-schema validation, lock recovery, and crash behavior require integration tests.
- Source bodies and secret values are prohibited from durable storage.

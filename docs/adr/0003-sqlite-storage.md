# ADR 0003: SQLite storage

- Status: Accepted
- Date: 2026-07-29

## Context

Code System Graph is local-first and requires transactional snapshots, full-text search, migrations,
portable installation, concurrent readers, and deterministic recovery.

## Decision

Code System Graph uses bundled SQLite through `rusqlite`. Every connection enables foreign keys, WAL,
and a busy timeout. Versioned SQL migrations are embedded in the binary and tracked in
`schema_metadata`.

Extractor output is immutable data until an application transaction atomically replaces the
batch keyed by repository snapshot, extractor, and source artifact. A snapshot becomes current
only after all selected extractors and linkers succeed. The previous valid snapshot therefore
survives interruption.

SQLite calls execute through a dedicated blocking boundary. The schema uses relational columns
for stable and queried attributes; JSON is reserved for genuinely variant attributes. FTS5
indexes user-facing labels and descriptions. Destructive migrations require a backup and
documented downgrade behavior.

Repository and checkout paths use a platform tag plus lossless native bytes; lossy display paths
are diagnostic-only. Ordered migrations reject a database newer than the running binary. Online
backups use SQLite's backup API and do not overwrite an existing destination. A restrictive
sidecar lock provides single-writer ownership and bounded stale-lock recovery.

Opening an older on-disk schema creates a versioned backup before applying migrations. Restore
validates the selected source, preserves the replaced database as a separately named safety
backup, and forward-migrates only after restoration succeeds. Read-only clients never migrate.

Writer-lock metadata includes PID and process start time. Recovery requires both expiry and proof
that the original process instance is no longer alive, preventing long-running writers or reused
PIDs from being mistaken for stale locks.

## Consequences

- A single writer and WAL permit bounded concurrent readers.
- Migration, backup, integrity, lock recovery, and crash behavior require integration tests.
- Source bodies and secret values are prohibited from durable storage.

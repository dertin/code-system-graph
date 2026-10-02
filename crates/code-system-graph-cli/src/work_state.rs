//! Disposable operational state for resumable scans and finite watchers.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use code_system_graph_model::{ArtifactFingerprint, StoredExtractorBatch, stable_id_bytes};
use code_system_graph_store_sqlite::set_owner_only_file;
use rusqlite::{Connection, ErrorCode, OpenFlags, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

const WORK_SCHEMA_VERSION: &str = "1.2.0";
const SQLITE_ARTIFACT_SUFFIXES: [&str; 4] = ["", "-wal", "-shm", "-journal"];

enum WorkOpenError {
    Recreate,
    Fatal(String),
}

pub(crate) struct WorkState {
    connection: Connection,
}

#[derive(Debug, Clone)]
pub(crate) struct WatcherLease {
    pub(crate) state: String,
    pub(crate) pid: Option<u32>,
    pub(crate) process_start_identity: Option<String>,
    pub(crate) heartbeat_unix_ms: Option<u64>,
    pub(crate) detail: Option<String>,
}

/// Identity of one file in the stat cache: checkout id and canonical relative path bytes.
pub(crate) type FileStatKey = (String, Vec<u8>);

/// Filesystem metadata that lets a scan reuse a content hash without reading the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileStat {
    pub(crate) size_bytes: u64,
    pub(crate) modified_unix_ns: i64,
    pub(crate) file_identity: String,
    pub(crate) content_hash: String,
}

/// Stat-cache rows to write and remove after one fingerprinting pass.
#[derive(Debug, Default)]
pub(crate) struct FileStatDelta {
    pub(crate) upserts: Vec<(FileStatKey, FileStat)>,
    pub(crate) removals: Vec<FileStatKey>,
}

/// Batch metadata stored beside the raw payload BLOB.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedBatchHeader {
    source: ArtifactFingerprint,
    extractor_version: String,
    budget_fingerprint: String,
    source_was_lossy: bool,
    output_count: u64,
}

impl WorkState {
    pub(crate) fn open(database: &Path, database_instance_id: &str) -> Result<Self, String> {
        let path = work_path(database);
        ensure_private_file(&path)?;
        let path = canonicalize_parent(&path)?;
        ensure_safe_sqlite_siblings(&path)?;
        match Self::open_existing(&path, database_instance_id) {
            Ok(state) => Ok(state),
            Err(WorkOpenError::Fatal(error)) => Err(error),
            Err(WorkOpenError::Recreate) => {
                remove_sidecar_files(&path)?;
                ensure_private_file(&path)?;
                Self::open_existing(&path, database_instance_id).map_err(|error| match error {
                    WorkOpenError::Recreate => {
                        "failed to recreate incompatible work sidecar".to_owned()
                    }
                    WorkOpenError::Fatal(error) => error,
                })
            }
        }
    }

    fn open_existing(path: &Path, database_instance_id: &str) -> Result<Self, WorkOpenError> {
        // Opening can fail for permissions, I/O, or locking reasons. None of those make
        // the sidecar disposable, so never remove it based on this operation alone.
        let flags = OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let connection = Connection::open_with_flags(path, flags)
            .map_err(|error| WorkOpenError::Fatal(error.to_string()))?;
        connection
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=FULL;
                 PRAGMA foreign_keys=ON;
                 CREATE TABLE IF NOT EXISTS work_metadata (
                     key TEXT PRIMARY KEY,
                     value TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS batch_cache (
                     cache_key TEXT PRIMARY KEY,
                     header_json BLOB NOT NULL,
                     payload BLOB NOT NULL,
                     size_bytes INTEGER NOT NULL CHECK(size_bytes > 0),
                     last_access_unix_ms INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_batch_cache_lru
                     ON batch_cache(last_access_unix_ms, cache_key);
                 CREATE TABLE IF NOT EXISTS file_stats (
                     checkout_id TEXT NOT NULL,
                     path BLOB NOT NULL,
                     size_bytes INTEGER NOT NULL,
                     modified_unix_ns INTEGER NOT NULL,
                     file_identity TEXT NOT NULL,
                     content_hash TEXT NOT NULL,
                     PRIMARY KEY(checkout_id, path)
                 );
                 CREATE TABLE IF NOT EXISTS watcher_lease (
                     workspace TEXT PRIMARY KEY,
                     state TEXT NOT NULL,
                     owner_token TEXT NOT NULL,
                     pid INTEGER,
                     process_start_identity TEXT,
                     session_started_unix_ms INTEGER,
                     heartbeat_unix_ms INTEGER,
                     last_success_unix_ms INTEGER,
                     detail TEXT
                 );
                 CREATE TABLE IF NOT EXISTS watch_scope (
                     workspace TEXT NOT NULL,
                     repository_alias TEXT NOT NULL,
                     target_json BLOB NOT NULL,
                     PRIMARY KEY(workspace, repository_alias)
                 );",
            )
            .map_err(|error| classify_existing_sidecar_error(&error))?;
        let version = connection
            .query_row(
                "SELECT value FROM work_metadata WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| classify_existing_sidecar_error(&error))?;
        match version.as_deref() {
            None => {
                connection
                    .execute(
                        "INSERT INTO work_metadata(key, value) VALUES ('schema_version', ?1)",
                        [WORK_SCHEMA_VERSION],
                    )
                    .map_err(|error| classify_existing_sidecar_error(&error))?;
                connection
                    .execute(
                        "INSERT INTO work_metadata(key, value)
                         VALUES ('database_instance_id', ?1)",
                        [database_instance_id],
                    )
                    .map_err(|error| classify_existing_sidecar_error(&error))?;
            }
            Some(WORK_SCHEMA_VERSION) => {
                let bound_instance = connection
                    .query_row(
                        "SELECT value FROM work_metadata WHERE key = 'database_instance_id'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(|error| classify_existing_sidecar_error(&error))?;
                if bound_instance.as_deref() != Some(database_instance_id) {
                    return Err(WorkOpenError::Recreate);
                }
            }
            Some(_) => return Err(WorkOpenError::Recreate),
        }
        ensure_safe_sqlite_siblings(path).map_err(WorkOpenError::Fatal)?;
        Ok(Self { connection })
    }

    pub(crate) fn load_batches(
        &mut self,
        fingerprints: &[&ArtifactFingerprint],
        budget_fingerprint: &str,
        extractor_version: &str,
        maximum_payload_bytes: u64,
        now_unix_ms: u64,
    ) -> Result<Vec<StoredExtractorBatch>, String> {
        let now_unix_ms = sqlite_integer(now_unix_ms, "cache timestamp")?;
        let maximum_payload_bytes = sqlite_integer(
            maximum_payload_bytes.min(i64::MAX as u64),
            "cache batch maximum",
        )?;
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| error.to_string())?;
        let mut batches = Vec::new();
        {
            let mut select = transaction
                .prepare_cached(
                    "SELECT header_json,
                            CASE WHEN length(payload) <= ?2 THEN payload END
                     FROM batch_cache WHERE cache_key = ?1",
                )
                .map_err(|error| error.to_string())?;
            let mut touch = transaction
                .prepare_cached(
                    "UPDATE batch_cache SET last_access_unix_ms = ?2 WHERE cache_key = ?1",
                )
                .map_err(|error| error.to_string())?;
            let mut remove = transaction
                .prepare_cached("DELETE FROM batch_cache WHERE cache_key = ?1")
                .map_err(|error| error.to_string())?;
            for fingerprint in fingerprints {
                let key = batch_cache_key(fingerprint, budget_fingerprint, extractor_version)?;
                let row = select
                    .query_row(params![key, maximum_payload_bytes], |row| {
                        Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Option<Vec<u8>>>(1)?))
                    })
                    .optional()
                    .map_err(|error| error.to_string())?;
                let Some((header, payload)) = row else {
                    continue;
                };
                let decoded = payload.and_then(|payload| {
                    serde_json::from_slice::<CachedBatchHeader>(&header)
                        .ok()
                        .map(|header| (header, payload))
                });
                let Some((header, payload)) = decoded else {
                    remove.execute([&key]).map_err(|error| error.to_string())?;
                    continue;
                };
                if header.source == **fingerprint
                    && header.budget_fingerprint == budget_fingerprint
                    && header.extractor_version == extractor_version
                {
                    touch
                        .execute(params![key, now_unix_ms])
                        .map_err(|error| error.to_string())?;
                    batches.push(StoredExtractorBatch {
                        source: header.source,
                        extractor_version: header.extractor_version,
                        budget_fingerprint: header.budget_fingerprint,
                        source_was_lossy: header.source_was_lossy,
                        output_count: header.output_count,
                        payload,
                    });
                }
            }
        }
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(batches)
    }

    /// Checkpoints completed batches in one transaction and returns how many were retained.
    pub(crate) fn put_batches(
        &mut self,
        batches: &[&StoredExtractorBatch],
        quota_bytes: u64,
        now_unix_ms: u64,
    ) -> Result<u64, String> {
        if batches.is_empty() {
            return Ok(0);
        }
        let now_unix_ms = sqlite_integer(now_unix_ms, "cache timestamp")?;
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| error.to_string())?;
        let mut written = 0_u64;
        {
            let mut insert = transaction
                .prepare_cached(
                    "INSERT INTO batch_cache(
                         cache_key, header_json, payload, size_bytes, last_access_unix_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5)
                     ON CONFLICT(cache_key) DO UPDATE SET
                         header_json = excluded.header_json,
                         payload = excluded.payload,
                         size_bytes = excluded.size_bytes,
                         last_access_unix_ms = excluded.last_access_unix_ms",
                )
                .map_err(|error| error.to_string())?;
            for batch in batches {
                let header = serde_json::to_vec(&CachedBatchHeader {
                    source: batch.source.clone(),
                    extractor_version: batch.extractor_version.clone(),
                    budget_fingerprint: batch.budget_fingerprint.clone(),
                    source_was_lossy: batch.source_was_lossy,
                    output_count: batch.output_count,
                })
                .map_err(|error| error.to_string())?;
                let size = u64::try_from(header.len().saturating_add(batch.payload.len()))
                    .unwrap_or(u64::MAX);
                if size > quota_bytes || size > i64::MAX as u64 {
                    continue;
                }
                let key = batch_cache_key(
                    &batch.source,
                    &batch.budget_fingerprint,
                    &batch.extractor_version,
                )?;
                insert
                    .execute(params![
                        key,
                        header,
                        batch.payload,
                        sqlite_integer(size, "cache entry size")?,
                        now_unix_ms
                    ])
                    .map_err(|error| error.to_string())?;
                written = written.saturating_add(1);
            }
        }
        evict_to_quota(&transaction, quota_bytes)?;
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(written)
    }

    pub(crate) fn load_file_stats(&self) -> Result<HashMap<FileStatKey, FileStat>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT checkout_id, path, size_bytes, modified_unix_ns, file_identity,
                        content_hash
                 FROM file_stats",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    (row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?),
                    FileStat {
                        size_bytes: u64::try_from(row.get::<_, i64>(2)?).unwrap_or(u64::MAX),
                        modified_unix_ns: row.get(3)?,
                        file_identity: row.get(4)?,
                        content_hash: row.get(5)?,
                    },
                ))
            })
            .map_err(|error| error.to_string())?;
        rows.collect::<Result<HashMap<_, _>, _>>()
            .map_err(|error| error.to_string())
    }

    pub(crate) fn apply_file_stat_delta(&mut self, delta: &FileStatDelta) -> Result<(), String> {
        if delta.upserts.is_empty() && delta.removals.is_empty() {
            return Ok(());
        }
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| error.to_string())?;
        {
            let mut remove = transaction
                .prepare_cached("DELETE FROM file_stats WHERE checkout_id = ?1 AND path = ?2")
                .map_err(|error| error.to_string())?;
            for (checkout_id, path) in &delta.removals {
                remove
                    .execute(params![checkout_id, path])
                    .map_err(|error| error.to_string())?;
            }
            let mut upsert = transaction
                .prepare_cached(
                    "INSERT INTO file_stats(
                         checkout_id, path, size_bytes, modified_unix_ns, file_identity,
                         content_hash
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(checkout_id, path) DO UPDATE SET
                         size_bytes = excluded.size_bytes,
                         modified_unix_ns = excluded.modified_unix_ns,
                         file_identity = excluded.file_identity,
                         content_hash = excluded.content_hash",
                )
                .map_err(|error| error.to_string())?;
            for ((checkout_id, path), stat) in &delta.upserts {
                upsert
                    .execute(params![
                        checkout_id,
                        path,
                        sqlite_integer(stat.size_bytes, "file size")?,
                        stat.modified_unix_ns,
                        stat.file_identity,
                        stat.content_hash
                    ])
                    .map_err(|error| error.to_string())?;
            }
        }
        transaction.commit().map_err(|error| error.to_string())
    }

    pub(crate) fn start_watcher(
        &mut self,
        workspace: &str,
        owner_token: &str,
        pid: u32,
        process_start_identity: &str,
        now_unix_ms: u64,
        stale_after_ms: u64,
    ) -> Result<(), String> {
        let pid = i64::from(pid);
        let now = sqlite_integer(now_unix_ms, "watcher timestamp")?;
        let stale_before = sqlite_integer(
            now_unix_ms.saturating_sub(stale_after_ms.saturating_mul(2)),
            "watcher stale timestamp",
        )?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let active_owner = transaction
            .query_row(
                "SELECT pid, process_start_identity, heartbeat_unix_ms
                 FROM watcher_lease
                 WHERE workspace = ?1 AND state = 'active'",
                params![workspace],
                |row| {
                    Ok((
                        row.get::<_, Option<i64>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if active_owner.is_some_and(|(active_pid, identity, heartbeat)| {
            let heartbeat_is_fresh = heartbeat.is_some_and(|value| value >= stale_before);
            let Some((active_pid, expected)) = active_pid
                .and_then(|value| u32::try_from(value).ok())
                .zip(identity.as_deref())
            else {
                return heartbeat_is_fresh;
            };
            crate::worker::process_identity(active_pid)
                .map_or(heartbeat_is_fresh, |actual| actual == expected)
        }) {
            return Err(format!(
                "watcher for workspace `{workspace}` is already active"
            ));
        }
        transaction
            .execute(
                "INSERT INTO watcher_lease(
                     workspace, state, owner_token, pid, process_start_identity, session_started_unix_ms,
                     heartbeat_unix_ms, last_success_unix_ms, detail
                 ) VALUES (?1, 'active', ?2, ?3, ?4, ?5, ?5, ?5, NULL)
                 ON CONFLICT(workspace) DO UPDATE SET
                     state = excluded.state,
                     owner_token = excluded.owner_token,
                     pid = excluded.pid,
                     process_start_identity = excluded.process_start_identity,
                     session_started_unix_ms = excluded.session_started_unix_ms,
                     heartbeat_unix_ms = excluded.heartbeat_unix_ms,
                     last_success_unix_ms = excluded.last_success_unix_ms,
                     detail = NULL",
                params![workspace, owner_token, pid, process_start_identity, now],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(crate) fn replace_watch_scope(
        &mut self,
        workspace: &str,
        targets: &[(String, Vec<u8>)],
    ) -> Result<(), String> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        transaction
            .execute("DELETE FROM watch_scope WHERE workspace = ?1", [workspace])
            .map_err(|error| error.to_string())?;
        for (alias, encoded) in targets {
            transaction
                .execute(
                    "INSERT INTO watch_scope(workspace, repository_alias, target_json)
                     VALUES (?1, ?2, ?3)",
                    params![workspace, alias, encoded],
                )
                .map_err(|error| error.to_string())?;
        }
        transaction.commit().map_err(|error| error.to_string())
    }

    pub(crate) fn load_watch_scope(
        &self,
        workspace: &str,
        maximum_target_bytes: u64,
    ) -> Result<Vec<Vec<u8>>, String> {
        let maximum_target_bytes = sqlite_integer(
            maximum_target_bytes.min(i64::MAX as u64),
            "watch target maximum",
        )?;
        let total = self
            .connection
            .query_row(
                "SELECT count(*) FROM watch_scope WHERE workspace = ?1",
                [workspace],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| error.to_string())?;
        let mut statement = self
            .connection
            .prepare(
                "SELECT target_json FROM watch_scope
                 WHERE workspace = ?1 AND length(target_json) <= ?2
                 ORDER BY repository_alias",
            )
            .map_err(|error| error.to_string())?;
        let encoded = statement
            .query_map(params![workspace, maximum_target_bytes], |row| {
                row.get::<_, Vec<u8>>(0)
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        if i64::try_from(encoded.len()).ok() != Some(total) {
            return Err("persisted watch target exceeded its protocol bound".to_owned());
        }
        Ok(encoded)
    }

    pub(crate) fn heartbeat_watcher(
        &self,
        workspace: &str,
        owner_token: &str,
        now_unix_ms: u64,
        successful_activity: bool,
    ) -> Result<(), String> {
        let now = sqlite_integer(now_unix_ms, "watcher timestamp")?;
        let changed = self
            .connection
            .execute(
                "UPDATE watcher_lease SET
                     heartbeat_unix_ms = ?3,
                     last_success_unix_ms = CASE WHEN ?4 THEN ?3 ELSE last_success_unix_ms END
                 WHERE workspace = ?1 AND owner_token = ?2 AND state = 'active'",
                params![workspace, owner_token, now, successful_activity],
            )
            .map_err(|error| error.to_string())?;
        if changed != 1 {
            return Err(format!(
                "watcher lease for workspace `{workspace}` is no longer owned"
            ));
        }
        Ok(())
    }

    pub(crate) fn finish_watcher(
        &self,
        workspace: &str,
        owner_token: &str,
        state: &str,
        detail: Option<&str>,
        now_unix_ms: u64,
    ) -> Result<(), String> {
        let now = sqlite_integer(now_unix_ms, "watcher timestamp")?;
        let changed = self
            .connection
            .execute(
                "UPDATE watcher_lease SET state = ?3, heartbeat_unix_ms = ?4, detail = ?5
                 WHERE workspace = ?1 AND owner_token = ?2",
                params![workspace, owner_token, state, now, detail],
            )
            .map_err(|error| error.to_string())?;
        if changed != 1 {
            return Err(format!(
                "watcher lease for workspace `{workspace}` is no longer owned"
            ));
        }
        Ok(())
    }

    pub(crate) fn watcher_lease(&self, workspace: &str) -> Result<Option<WatcherLease>, String> {
        self.connection
            .query_row(
                "SELECT state, pid, process_start_identity, heartbeat_unix_ms, detail
                 FROM watcher_lease WHERE workspace = ?1",
                [workspace],
                |row| {
                    let pid = row
                        .get::<_, Option<i64>>(1)?
                        .and_then(|value| u32::try_from(value).ok());
                    let heartbeat = row
                        .get::<_, Option<i64>>(3)?
                        .and_then(|value| u64::try_from(value).ok());
                    Ok(WatcherLease {
                        state: row.get(0)?,
                        pid,
                        process_start_identity: row.get(2)?,
                        heartbeat_unix_ms: heartbeat,
                        detail: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|error| error.to_string())
    }
}

fn evict_to_quota(transaction: &rusqlite::Transaction<'_>, quota_bytes: u64) -> Result<(), String> {
    let total = transaction
        .query_row(
            "SELECT COALESCE(SUM(size_bytes), 0) FROM batch_cache",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| error.to_string())?;
    let mut total = u64::try_from(total).map_err(|_| "negative work cache size".to_owned())?;
    if total <= quota_bytes {
        return Ok(());
    }
    let mut oldest = transaction
        .prepare(
            "SELECT cache_key, size_bytes FROM batch_cache ORDER BY last_access_unix_ms, cache_key",
        )
        .map_err(|error| error.to_string())?;
    let mut victims = Vec::new();
    let mut rows = oldest.query([]).map_err(|error| error.to_string())?;
    while total > quota_bytes {
        let Some(row) = rows.next().map_err(|error| error.to_string())? else {
            break;
        };
        let key = row.get::<_, String>(0).map_err(|error| error.to_string())?;
        let size = row.get::<_, i64>(1).map_err(|error| error.to_string())?;
        total = total.saturating_sub(u64::try_from(size).unwrap_or(0));
        victims.push(key);
    }
    drop(rows);
    let mut remove = transaction
        .prepare_cached("DELETE FROM batch_cache WHERE cache_key = ?1")
        .map_err(|error| error.to_string())?;
    for key in victims {
        remove.execute([key]).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn sqlite_integer(value: u64, field: &str) -> Result<i64, String> {
    i64::try_from(value).map_err(|_| format!("{field} is not representable by SQLite"))
}

fn batch_cache_key(
    fingerprint: &ArtifactFingerprint,
    budget_fingerprint: &str,
    extractor_version: &str,
) -> Result<String, String> {
    let material = serde_json::to_vec(&(fingerprint, budget_fingerprint, extractor_version))
        .map_err(|error| error.to_string())?;
    Ok(stable_id_bytes("work-batch-v1", &material))
}

pub(crate) fn work_path(database: &Path) -> PathBuf {
    let mut value = database.as_os_str().to_os_string();
    value.push(".work.db");
    PathBuf::from(value)
}

fn canonicalize_parent(path: &Path) -> Result<PathBuf, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("work sidecar path `{}` has no parent", path.display()))?;
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("work sidecar path `{}` has no file name", path.display()))?;
    let canonical_parent = fs::canonicalize(parent).map_err(|error| {
        format!(
            "failed to canonicalize work sidecar parent `{}`: {error}",
            parent.display()
        )
    })?;
    Ok(canonical_parent.join(file_name))
}

fn classify_existing_sidecar_error(error: &rusqlite::Error) -> WorkOpenError {
    match error {
        rusqlite::Error::SqliteFailure(details, _)
            if matches!(
                details.code,
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
            ) =>
        {
            WorkOpenError::Recreate
        }
        // SQL shape errors indicate an incompatible work sidecar schema for this binary.
        rusqlite::Error::SqlInputError { .. } => WorkOpenError::Recreate,
        _ => WorkOpenError::Fatal(error.to_string()),
    }
}

fn ensure_private_file(path: &Path) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if path.exists() {
        let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(format!("unsafe work sidecar path `{}`", path.display()));
        }
        set_owner_only_file(path).map_err(|error| error.to_string())?;
        return Ok(());
    }
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|error| error.to_string())?;
    set_owner_only_file(path).map_err(|error| error.to_string())
}

fn ensure_safe_sqlite_siblings(path: &Path) -> Result<(), String> {
    for suffix in &SQLITE_ARTIFACT_SUFFIXES[1..] {
        let candidate = sqlite_artifact_path(path, suffix);
        let metadata = match fs::symlink_metadata(&candidate) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(format!(
                "unsafe work sidecar companion path `{}`",
                candidate.display()
            ));
        }
        set_owner_only_file(&candidate).map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn remove_sidecar_files(path: &Path) -> Result<(), String> {
    let mut first_error = None;
    for suffix in SQLITE_ARTIFACT_SUFFIXES {
        let candidate = sqlite_artifact_path(path, suffix);
        match fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                first_error.get_or_insert_with(|| {
                    format!(
                        "failed to recreate incompatible work sidecar `{}`: {error}",
                        candidate.display()
                    )
                });
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn sqlite_artifact_path(path: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() {
        return path.to_path_buf();
    }
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{CheckoutId, NativePath, NativePathEncoding, RepoId};

    use super::*;

    #[cfg(unix)]
    #[test]
    fn work_state_should_open_below_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let canonical_parent = temporary.path().join("canonical");
        fs::create_dir(&canonical_parent).expect("canonical parent");
        let symlinked_parent = temporary.path().join("symlinked");
        symlink(&canonical_parent, &symlinked_parent).expect("symlinked parent");

        let database = symlinked_parent.join("graph.db");
        let state = WorkState::open(&database, "database-instance").expect("work state");
        drop(state);

        assert!(canonical_parent.join("graph.db.work.db").is_file());
    }

    fn fingerprint(hash: &str) -> ArtifactFingerprint {
        ArtifactFingerprint {
            repo_id: RepoId::new("repo:api"),
            checkout_id: CheckoutId::new("checkout:api"),
            path: NativePath {
                encoding: NativePathEncoding::Utf8,
                bytes: b"schema.graphql".to_vec(),
                display: "schema.graphql".to_owned(),
            },
            extractor: "code-system-graph.graphql.document".to_owned(),
            content_hash: hash.to_owned(),
            size_bytes: 4,
        }
    }

    #[test]
    fn batch_cache_should_match_all_deterministic_inputs_and_evict_lru() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut state = WorkState::open(&temporary.path().join("graph.db"), "database-instance")
            .expect("work state");
        let batch = StoredExtractorBatch {
            source: fingerprint("one"),
            extractor_version: "1.0.0".to_owned(),
            budget_fingerprint: "budget".to_owned(),
            source_was_lossy: false,
            output_count: 1,
            payload: b"[{}]".to_vec(),
        };
        assert_eq!(
            state.put_batches(&[&batch], 1_000_000, 1).expect("cache"),
            1
        );
        assert_eq!(
            state
                .load_batches(&[&fingerprint("one")], "budget", "1.0.0", 1_000_000, 2)
                .expect("load"),
            vec![batch.clone()]
        );
        assert_eq!(
            state
                .load_batches(&[&fingerprint("two")], "budget", "1.0.0", 1_000_000, 3)
                .expect("load")
                .as_slice(),
            &[]
        );
        assert_eq!(
            state.put_batches(&[&batch], 1, 4).expect("oversized skip"),
            0
        );

        let newer = StoredExtractorBatch {
            source: fingerprint("two"),
            ..batch.clone()
        };
        let quota = 2
            * (serde_json::to_vec(&fingerprint("one"))
                .expect("encode")
                .len() as u64);
        assert_eq!(state.put_batches(&[&newer], quota, 5).expect("evict"), 1);
        assert_eq!(
            state
                .load_batches(&[&fingerprint("one")], "budget", "1.0.0", 1_000_000, 6)
                .expect("evicted load"),
            Vec::new()
        );
    }

    #[test]
    fn incompatible_sidecar_should_be_recreated() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let database = temporary.path().join("graph.db");
        let state = WorkState::open(&database, "database-instance").expect("initial sidecar");
        state
            .connection
            .execute(
                "UPDATE work_metadata SET value = '999' WHERE key = 'schema_version'",
                [],
            )
            .expect("corrupt version");
        drop(state);
        WorkState::open(&database, "database-instance").expect("sidecar should be recreated");
    }

    #[test]
    fn sidecar_cleanup_should_remove_every_sqlite_artifact() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let sidecar = temporary.path().join("graph.db.work.db");
        for suffix in SQLITE_ARTIFACT_SUFFIXES {
            fs::write(sqlite_artifact_path(&sidecar, suffix), b"stale")
                .expect("create stale SQLite artifact");
        }

        remove_sidecar_files(&sidecar).expect("remove SQLite artifacts");

        for suffix in SQLITE_ARTIFACT_SUFFIXES {
            assert!(!sqlite_artifact_path(&sidecar, suffix).exists());
        }
    }

    #[test]
    fn sidecar_should_be_recreated_for_a_different_database_instance() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let database = temporary.path().join("graph.db");
        let initial = WorkState::open(&database, "database-one").expect("initial sidecar");
        drop(initial);

        let rebound = WorkState::open(&database, "database-two").expect("rebound sidecar");
        let stored = rebound
            .connection
            .query_row(
                "SELECT value FROM work_metadata WHERE key = 'database_instance_id'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("bound instance");
        assert_eq!(stored, "database-two");
    }

    #[test]
    fn watcher_lease_should_be_exclusive_and_owner_scoped() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let database = temporary.path().join("graph.db");
        let identity =
            crate::worker::process_identity(std::process::id()).expect("current process identity");
        let mut state = WorkState::open(&database, "database-instance").expect("work state");

        state
            .start_watcher(
                "workspace",
                "owner-one",
                std::process::id(),
                &identity,
                1_000,
                100,
            )
            .expect("first owner acquires lease");
        let second = state.start_watcher(
            "workspace",
            "owner-two",
            std::process::id(),
            &identity,
            1_001,
            100,
        );
        assert!(second.is_err());
        assert!(
            state
                .heartbeat_watcher("workspace", "owner-two", 1_002, true)
                .is_err()
        );
        assert!(
            state
                .finish_watcher("workspace", "owner-two", "stale", None, 1_003)
                .is_err()
        );
        state
            .finish_watcher("workspace", "owner-one", "expired_idle", None, 1_004)
            .expect("owner closes lease");
        state
            .start_watcher(
                "workspace",
                "owner-two",
                std::process::id(),
                &identity,
                1_005,
                100,
            )
            .expect("new owner acquires terminal lease");
        assert!(
            state
                .heartbeat_watcher("workspace", "owner-one", 1_006, false)
                .is_err()
        );
    }

    #[test]
    fn fresh_watcher_lease_should_fail_closed_when_process_identity_is_unavailable() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let database = temporary.path().join("graph.db");
        let mut state = WorkState::open(&database, "database-instance").expect("work state");

        state
            .start_watcher("workspace", "owner-one", u32::MAX, "missing", 1_000, 100)
            .expect("first owner acquires lease");
        let second = state.start_watcher("workspace", "owner-two", u32::MAX, "missing", 1_001, 100);
        assert!(second.is_err());
    }

    #[test]
    fn live_watcher_lease_should_remain_exclusive_after_heartbeat_is_old() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let database = temporary.path().join("graph.db");
        let identity =
            crate::worker::process_identity(std::process::id()).expect("current process identity");
        let mut state = WorkState::open(&database, "database-instance").expect("work state");

        state
            .start_watcher(
                "workspace",
                "owner-one",
                std::process::id(),
                &identity,
                1_000,
                10,
            )
            .expect("first owner acquires lease");
        let second = state.start_watcher(
            "workspace",
            "owner-two",
            std::process::id(),
            &identity,
            10_000,
            10,
        );
        assert!(second.is_err());
    }

    #[test]
    fn transient_open_failure_should_not_be_classified_as_disposable() {
        let error = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            Some("database is busy".to_owned()),
        );
        assert!(matches!(
            classify_existing_sidecar_error(&error),
            WorkOpenError::Fatal(_)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn sqlite_companion_symlink_should_be_rejected_before_open() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let database = temporary.path().join("graph.db");
        let state = WorkState::open(&database, "database-instance").expect("initial sidecar");
        drop(state);
        let sidecar = work_path(&database);
        let target = temporary.path().join("target");
        fs::write(&target, b"unchanged").expect("target fixture");
        let mut wal = sidecar.as_os_str().to_os_string();
        wal.push("-wal");
        symlink(&target, PathBuf::from(wal)).expect("malicious companion symlink");

        let error = WorkState::open(&database, "database-instance")
            .err()
            .expect("companion symlink must fail closed");
        assert!(error.contains("unsafe work sidecar companion"));
        assert_eq!(fs::read(&target).expect("target contents"), b"unchanged");
    }

    #[cfg(unix)]
    #[test]
    fn sqlite_open_should_refuse_a_replaced_main_sidecar_symlink() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let target = temporary.path().join("target.db");
        let marker = b"do-not-modify";
        fs::write(&target, marker).expect("target marker");
        let sidecar = work_path(&temporary.path().join("graph.db"));
        symlink(&target, &sidecar).expect("replacement symlink");

        assert!(matches!(
            WorkState::open_existing(&sidecar, "database-instance"),
            Err(WorkOpenError::Fatal(_))
        ));
        assert_eq!(fs::read(target).expect("target readback"), marker);
    }

    #[cfg(unix)]
    #[test]
    fn work_sidecar_should_remain_owner_only_in_a_shared_parent() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let unsafe_parent = temporary.path().join("shared");
        fs::create_dir(&unsafe_parent).expect("shared parent");
        fs::set_permissions(&unsafe_parent, fs::Permissions::from_mode(0o777))
            .expect("shared mode");
        let database = unsafe_parent.join("graph.db");

        let state = WorkState::open(&database, "database-instance").expect("work state");
        drop(state);
        let mode = fs::metadata(work_path(&database))
            .expect("sidecar metadata")
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }

    #[cfg(unix)]
    #[test]
    fn sidecar_and_live_sqlite_companions_should_remain_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let database = temporary.path().join("graph.db");
        let state = WorkState::open(&database, "database-instance").expect("work state");
        let sidecar = work_path(&database);
        state
            .connection
            .execute(
                "INSERT INTO work_metadata(key, value) VALUES ('permission-test', 'ok')",
                [],
            )
            .expect("sidecar write");
        ensure_safe_sqlite_siblings(&sidecar).expect("restrict live companions");

        for suffix in SQLITE_ARTIFACT_SUFFIXES {
            let candidate = sqlite_artifact_path(&sidecar, suffix);
            if candidate.exists() {
                let mode = std::fs::metadata(&candidate)
                    .expect("sidecar metadata")
                    .permissions()
                    .mode();
                assert_eq!(mode & 0o077, 0, "{} is not owner-only", candidate.display());
            }
        }
    }

    #[test]
    fn file_stat_delta_should_round_trip_and_remove_stale_rows() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut state = WorkState::open(&temporary.path().join("graph.db"), "database-instance")
            .expect("work state");
        let stat = FileStat {
            size_bytes: 4,
            modified_unix_ns: 1_000,
            file_identity: "1:2".to_owned(),
            content_hash: "hash".to_owned(),
        };
        let key = ("checkout:api".to_owned(), b"src/main.rs".to_vec());
        state
            .apply_file_stat_delta(&FileStatDelta {
                upserts: vec![(key.clone(), stat.clone())],
                removals: Vec::new(),
            })
            .expect("insert");
        assert_eq!(
            state.load_file_stats().expect("load").get(&key),
            Some(&stat)
        );
        state
            .apply_file_stat_delta(&FileStatDelta {
                upserts: Vec::new(),
                removals: vec![key],
            })
            .expect("remove");
        assert!(state.load_file_stats().expect("reload").is_empty());
    }
}

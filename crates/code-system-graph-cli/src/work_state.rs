//! Disposable operational state for resumable scans and finite watchers.

use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use code_system_graph_core::ExecutionSummary;
use code_system_graph_model::{
    ArtifactFingerprint, CommunitySnapshot, Edge, Evidence, ExtractorRun, LinkDecision, Node, NodeId, StoredExtractorBatch, stable_id_bytes
};
use code_system_graph_store_sqlite::{ManualLinkDisposition, ManualLinkRecord};
use rusqlite::{Connection, ErrorCode, OptionalExtension, TransactionBehavior, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

const WORK_SCHEMA_VERSION: &str = "1.0.0";

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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CandidateMetadata {
    snapshot_id: String,
    community_delta_count: usize,
    corroborated_symbol_count: usize,
    affected_test_count: usize,
    execution: ExecutionSummary,
    degradations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StagedManualLinkRecord {
    id: String,
    snapshot_id: String,
    source_node_id: NodeId,
    target_node_id: NodeId,
    kind: String,
    disposition: String,
    reason: String,
    decision: LinkDecision,
    config_version: u32,
}

#[derive(Debug, Clone)]
pub(crate) struct StagedSnapshot {
    pub(crate) snapshot_id: String,
    pub(crate) nodes: Vec<Node>,
    pub(crate) edges: Vec<Edge>,
    pub(crate) evidence: Vec<Evidence>,
    pub(crate) fingerprints: Vec<ArtifactFingerprint>,
    pub(crate) extractor_batches: Vec<StoredExtractorBatch>,
    pub(crate) extractor_runs: Vec<ExtractorRun>,
    pub(crate) manual_links: Vec<ManualLinkRecord>,
    pub(crate) community_snapshot: CommunitySnapshot,
    pub(crate) community_delta_count: usize,
    pub(crate) corroborated_symbol_count: usize,
    pub(crate) affected_test_count: usize,
    pub(crate) execution: ExecutionSummary,
    pub(crate) degradations: Vec<String>,
}

impl WorkState {
    pub(crate) fn open(database: &Path, database_instance_id: &str) -> Result<Self, String> {
        let path = work_path(database);
        ensure_private_file(&path)?;
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
        let connection =
            Connection::open(path).map_err(|error| WorkOpenError::Fatal(error.to_string()))?;
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
                     batch_json BLOB NOT NULL,
                     size_bytes INTEGER NOT NULL CHECK(size_bytes > 0),
                     last_access_unix_ms INTEGER NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS idx_batch_cache_lru
                     ON batch_cache(last_access_unix_ms, cache_key);
                 CREATE TABLE IF NOT EXISTS active_candidate (
                     workspace TEXT PRIMARY KEY,
                     compatibility_fingerprint TEXT NOT NULL,
                     phase TEXT NOT NULL,
                     started_at_unix_ms INTEGER NOT NULL,
                     updated_at_unix_ms INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS candidate_items (
                     workspace TEXT NOT NULL,
                     compatibility_fingerprint TEXT NOT NULL,
                     item_kind TEXT NOT NULL,
                     item_key TEXT NOT NULL,
                     item_json BLOB NOT NULL,
                     PRIMARY KEY(workspace, item_kind, item_key)
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

    pub(crate) fn begin_candidate(
        &mut self,
        workspace: &str,
        compatibility_fingerprint: &str,
        now_unix_ms: u64,
    ) -> Result<bool, String> {
        let now_unix_ms = sqlite_integer(now_unix_ms, "candidate timestamp")?;
        let previous = self
            .connection
            .query_row(
                "SELECT compatibility_fingerprint FROM active_candidate WHERE workspace = ?1",
                [workspace],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        let resumed = previous.as_deref() == Some(compatibility_fingerprint);
        if !resumed {
            self.connection
                .execute(
                    "DELETE FROM candidate_items WHERE workspace = ?1",
                    [workspace],
                )
                .map_err(|error| error.to_string())?;
        }
        self.connection
            .execute(
                "INSERT INTO active_candidate(
                     workspace, compatibility_fingerprint, phase, started_at_unix_ms,
                     updated_at_unix_ms
                 ) VALUES (?1, ?2, 'fingerprinted', ?3, ?3)
                 ON CONFLICT(workspace) DO UPDATE SET
                     compatibility_fingerprint = excluded.compatibility_fingerprint,
                     phase = excluded.phase,
                     started_at_unix_ms = CASE
                         WHEN active_candidate.compatibility_fingerprint = excluded.compatibility_fingerprint
                         THEN active_candidate.started_at_unix_ms
                         ELSE excluded.started_at_unix_ms
                     END,
                     updated_at_unix_ms = excluded.updated_at_unix_ms",
                params![workspace, compatibility_fingerprint, now_unix_ms],
            )
            .map_err(|error| error.to_string())?;
        Ok(resumed)
    }

    pub(crate) fn set_candidate_phase(
        &self,
        workspace: &str,
        phase: &str,
        now_unix_ms: u64,
    ) -> Result<(), String> {
        let now_unix_ms = sqlite_integer(now_unix_ms, "candidate timestamp")?;
        self.connection
            .execute(
                "UPDATE active_candidate SET phase = ?2, updated_at_unix_ms = ?3
                 WHERE workspace = ?1",
                params![workspace, phase, now_unix_ms],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    pub(crate) fn complete_candidate(&self, workspace: &str) -> Result<(), String> {
        self.connection
            .execute(
                "DELETE FROM candidate_items WHERE workspace = ?1",
                [workspace],
            )
            .map_err(|error| error.to_string())?;
        self.connection
            .execute(
                "DELETE FROM active_candidate WHERE workspace = ?1",
                [workspace],
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the candidate boundary records every immutable snapshot collection explicitly"
    )]
    pub(crate) fn store_candidate_snapshot(
        &mut self,
        workspace: &str,
        compatibility_fingerprint: &str,
        snapshot: &StagedSnapshot,
        now_unix_ms: u64,
    ) -> Result<(), String> {
        let current = self
            .connection
            .query_row(
                "SELECT compatibility_fingerprint FROM active_candidate WHERE workspace = ?1",
                [workspace],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if current.as_deref() != Some(compatibility_fingerprint) {
            return Err("active candidate fingerprint changed before staging".to_owned());
        }
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM candidate_items WHERE workspace = ?1",
                [workspace],
            )
            .map_err(|error| error.to_string())?;
        let metadata = CandidateMetadata {
            snapshot_id: snapshot.snapshot_id.clone(),
            community_delta_count: snapshot.community_delta_count,
            corroborated_symbol_count: snapshot.corroborated_symbol_count,
            affected_test_count: snapshot.affected_test_count,
            execution: snapshot.execution.clone(),
            degradations: snapshot.degradations.clone(),
        };
        insert_candidate_item(
            &transaction,
            workspace,
            compatibility_fingerprint,
            "metadata",
            "snapshot-v1",
            &metadata,
        )?;
        insert_candidate_values(
            &transaction,
            workspace,
            compatibility_fingerprint,
            "node",
            &snapshot.nodes,
            |node| node.id.as_str().to_owned(),
        )?;
        insert_candidate_values(
            &transaction,
            workspace,
            compatibility_fingerprint,
            "edge",
            &snapshot.edges,
            |edge| edge.id.as_str().to_owned(),
        )?;
        insert_candidate_values(
            &transaction,
            workspace,
            compatibility_fingerprint,
            "evidence",
            &snapshot.evidence,
            |evidence| evidence.id.as_str().to_owned(),
        )?;
        insert_candidate_values(
            &transaction,
            workspace,
            compatibility_fingerprint,
            "fingerprint",
            &snapshot.fingerprints,
            candidate_value_key,
        )?;
        insert_candidate_values(
            &transaction,
            workspace,
            compatibility_fingerprint,
            "extractor_batch",
            &snapshot.extractor_batches,
            candidate_value_key,
        )?;
        insert_candidate_values(
            &transaction,
            workspace,
            compatibility_fingerprint,
            "extractor_run",
            &snapshot.extractor_runs,
            |run| run.id.clone(),
        )?;
        for record in &snapshot.manual_links {
            let staged = StagedManualLinkRecord::from(record);
            insert_candidate_item(
                &transaction,
                workspace,
                compatibility_fingerprint,
                "manual_link",
                &record.id,
                &staged,
            )?;
        }
        insert_candidate_item(
            &transaction,
            workspace,
            compatibility_fingerprint,
            "community",
            "snapshot-v1",
            &snapshot.community_snapshot,
        )?;
        let now = sqlite_integer(now_unix_ms, "candidate timestamp")?;
        transaction
            .execute(
                "UPDATE active_candidate SET phase = 'ready', updated_at_unix_ms = ?2
                 WHERE workspace = ?1 AND compatibility_fingerprint = ?3",
                params![workspace, now, compatibility_fingerprint],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())
    }

    pub(crate) fn load_candidate_snapshot(
        &self,
        workspace: &str,
        compatibility_fingerprint: &str,
    ) -> Result<Option<StagedSnapshot>, String> {
        let phase = self
            .connection
            .query_row(
                "SELECT phase FROM active_candidate
                 WHERE workspace = ?1 AND compatibility_fingerprint = ?2",
                params![workspace, compatibility_fingerprint],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if phase.as_deref() != Some("ready") {
            return Ok(None);
        }
        let metadata = load_single_candidate_value::<CandidateMetadata>(
            &self.connection,
            workspace,
            compatibility_fingerprint,
            "metadata",
        )?;
        let Some(metadata) = metadata else {
            return Ok(None);
        };
        let manual_links = load_candidate_values::<StagedManualLinkRecord>(
            &self.connection,
            workspace,
            compatibility_fingerprint,
            "manual_link",
        )?
        .into_iter()
        .map(ManualLinkRecord::try_from)
        .collect::<Result<Vec<_>, _>>()?;
        let Some(community_snapshot) = load_single_candidate_value(
            &self.connection,
            workspace,
            compatibility_fingerprint,
            "community",
        )?
        else {
            return Ok(None);
        };
        Ok(Some(StagedSnapshot {
            snapshot_id: metadata.snapshot_id,
            nodes: load_candidate_values(
                &self.connection,
                workspace,
                compatibility_fingerprint,
                "node",
            )?,
            edges: load_candidate_values(
                &self.connection,
                workspace,
                compatibility_fingerprint,
                "edge",
            )?,
            evidence: load_candidate_values(
                &self.connection,
                workspace,
                compatibility_fingerprint,
                "evidence",
            )?,
            fingerprints: load_candidate_values(
                &self.connection,
                workspace,
                compatibility_fingerprint,
                "fingerprint",
            )?,
            extractor_batches: load_candidate_values(
                &self.connection,
                workspace,
                compatibility_fingerprint,
                "extractor_batch",
            )?,
            extractor_runs: load_candidate_values(
                &self.connection,
                workspace,
                compatibility_fingerprint,
                "extractor_run",
            )?,
            manual_links,
            community_snapshot,
            community_delta_count: metadata.community_delta_count,
            corroborated_symbol_count: metadata.corroborated_symbol_count,
            affected_test_count: metadata.affected_test_count,
            execution: metadata.execution,
            degradations: metadata.degradations,
        }))
    }

    pub(crate) fn load_batches(
        &mut self,
        fingerprints: &[ArtifactFingerprint],
        budget_fingerprint: &str,
        extractor_version: &str,
        maximum_payload_bytes: u64,
        now_unix_ms: u64,
    ) -> Result<Vec<StoredExtractorBatch>, String> {
        let now_unix_ms = sqlite_integer(now_unix_ms, "cache timestamp")?;
        // `Vec<u8>` is represented as comma-separated JSON integers. Four encoded bytes per
        // payload byte plus bounded metadata is a conservative ceiling that lets SQLite reject
        // an oversized cache row before copying its BLOB into this process.
        let maximum_encoded_bytes = maximum_payload_bytes
            .saturating_mul(4)
            .saturating_add(1_048_576)
            .min(i64::MAX as u64);
        let maximum_encoded_bytes = sqlite_integer(maximum_encoded_bytes, "cache batch maximum")?;
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| error.to_string())?;
        let mut batches = Vec::new();
        for fingerprint in fingerprints {
            let key = batch_cache_key(fingerprint, budget_fingerprint, extractor_version)?;
            let encoded = transaction
                .query_row(
                    "SELECT CASE WHEN length(batch_json) <= ?2 THEN batch_json END
                     FROM batch_cache WHERE cache_key = ?1",
                    params![key, maximum_encoded_bytes],
                    |row| row.get::<_, Option<Vec<u8>>>(0),
                )
                .optional()
                .map_err(|error| error.to_string())?;
            let Some(Some(encoded)) = encoded else {
                if encoded.is_some() {
                    transaction
                        .execute("DELETE FROM batch_cache WHERE cache_key = ?1", [&key])
                        .map_err(|error| error.to_string())?;
                }
                continue;
            };
            let batch: StoredExtractorBatch = if let Ok(batch) = serde_json::from_slice(&encoded) {
                batch
            } else {
                transaction
                    .execute("DELETE FROM batch_cache WHERE cache_key = ?1", [&key])
                    .map_err(|error| error.to_string())?;
                continue;
            };
            if batch.source == *fingerprint
                && batch.budget_fingerprint == budget_fingerprint
                && batch.extractor_version == extractor_version
            {
                transaction
                    .execute(
                        "UPDATE batch_cache SET last_access_unix_ms = ?2 WHERE cache_key = ?1",
                        params![key, now_unix_ms],
                    )
                    .map_err(|error| error.to_string())?;
                batches.push(batch);
            }
        }
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(batches)
    }

    pub(crate) fn put_batch(
        &mut self,
        batch: &StoredExtractorBatch,
        quota_bytes: u64,
        now_unix_ms: u64,
    ) -> Result<bool, String> {
        let encoded = serde_json::to_vec(batch).map_err(|error| error.to_string())?;
        let size = u64::try_from(encoded.len()).unwrap_or(u64::MAX);
        if size == 0 || size > quota_bytes || size > i64::MAX as u64 {
            return Ok(false);
        }
        let size = sqlite_integer(size, "cache entry size")?;
        let now_unix_ms = sqlite_integer(now_unix_ms, "cache timestamp")?;
        let key = batch_cache_key(
            &batch.source,
            &batch.budget_fingerprint,
            &batch.extractor_version,
        )?;
        let transaction = self
            .connection
            .transaction()
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "INSERT INTO batch_cache(cache_key, batch_json, size_bytes, last_access_unix_ms)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(cache_key) DO UPDATE SET
                     batch_json = excluded.batch_json,
                     size_bytes = excluded.size_bytes,
                     last_access_unix_ms = excluded.last_access_unix_ms",
                params![key, encoded, size, now_unix_ms],
            )
            .map_err(|error| error.to_string())?;
        let mut total = cache_size(&transaction)?;
        while total > quota_bytes {
            let removed = transaction
                .execute(
                    "DELETE FROM batch_cache WHERE cache_key = (
                         SELECT cache_key FROM batch_cache
                         ORDER BY last_access_unix_ms, cache_key LIMIT 1
                     )",
                    [],
                )
                .map_err(|error| error.to_string())?;
            if removed == 0 {
                break;
            }
            total = cache_size(&transaction)?;
        }
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(true)
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

fn cache_size(transaction: &rusqlite::Transaction<'_>) -> Result<u64, String> {
    let size = transaction
        .query_row(
            "SELECT COALESCE(SUM(size_bytes), 0) FROM batch_cache",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| error.to_string())?;
    u64::try_from(size).map_err(|_| "negative work cache size".to_owned())
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

fn candidate_value_key<T: Serialize>(value: &T) -> String {
    serde_json::to_vec(value).map_or_else(
        |_| "unencodable-candidate-value".to_owned(),
        |encoded| stable_id_bytes("work-candidate-item-v1", &encoded),
    )
}

fn insert_candidate_values<T, F>(
    transaction: &rusqlite::Transaction<'_>,
    workspace: &str,
    compatibility_fingerprint: &str,
    item_kind: &str,
    values: &[T],
    key: F,
) -> Result<(), String>
where
    T: Serialize,
    F: Fn(&T) -> String,
{
    for value in values {
        insert_candidate_item(
            transaction,
            workspace,
            compatibility_fingerprint,
            item_kind,
            &key(value),
            value,
        )?;
    }
    Ok(())
}

fn insert_candidate_item<T: Serialize>(
    transaction: &rusqlite::Transaction<'_>,
    workspace: &str,
    compatibility_fingerprint: &str,
    item_kind: &str,
    item_key: &str,
    value: &T,
) -> Result<(), String> {
    let encoded = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    transaction
        .execute(
            "INSERT INTO candidate_items(
                 workspace, compatibility_fingerprint, item_kind, item_key, item_json
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(workspace, item_kind, item_key) DO UPDATE SET
                 compatibility_fingerprint = excluded.compatibility_fingerprint,
                 item_json = excluded.item_json",
            params![
                workspace,
                compatibility_fingerprint,
                item_kind,
                item_key,
                encoded
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn load_candidate_values<T: DeserializeOwned>(
    connection: &Connection,
    workspace: &str,
    compatibility_fingerprint: &str,
    item_kind: &str,
) -> Result<Vec<T>, String> {
    let mut statement = connection
        .prepare(
            "SELECT item_json FROM candidate_items
             WHERE workspace = ?1 AND compatibility_fingerprint = ?2 AND item_kind = ?3
             ORDER BY item_key",
        )
        .map_err(|error| error.to_string())?;
    let encoded = statement
        .query_map(
            params![workspace, compatibility_fingerprint, item_kind],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    encoded
        .into_iter()
        .map(|value| serde_json::from_slice(&value).map_err(|error| error.to_string()))
        .collect()
}

fn load_single_candidate_value<T: DeserializeOwned>(
    connection: &Connection,
    workspace: &str,
    compatibility_fingerprint: &str,
    item_kind: &str,
) -> Result<Option<T>, String> {
    let mut values =
        load_candidate_values(connection, workspace, compatibility_fingerprint, item_kind)?;
    match values.len() {
        0 => Ok(None),
        1 => Ok(values.pop()),
        _ => Err(format!(
            "candidate has multiple `{item_kind}` singleton rows"
        )),
    }
}

impl From<&ManualLinkRecord> for StagedManualLinkRecord {
    fn from(value: &ManualLinkRecord) -> Self {
        Self {
            id: value.id.clone(),
            snapshot_id: value.snapshot_id.clone(),
            source_node_id: value.source_node_id.clone(),
            target_node_id: value.target_node_id.clone(),
            kind: value.kind.clone(),
            disposition: match value.disposition {
                ManualLinkDisposition::Active => "active".to_owned(),
                ManualLinkDisposition::Suppression => "suppression".to_owned(),
            },
            reason: value.reason.clone(),
            decision: value.decision.clone(),
            config_version: value.config_version,
        }
    }
}

impl TryFrom<StagedManualLinkRecord> for ManualLinkRecord {
    type Error = String;

    fn try_from(value: StagedManualLinkRecord) -> Result<Self, Self::Error> {
        let disposition = match value.disposition.as_str() {
            "active" => ManualLinkDisposition::Active,
            "suppression" => ManualLinkDisposition::Suppression,
            other => return Err(format!("invalid staged manual-link disposition `{other}`")),
        };
        Ok(Self {
            id: value.id,
            snapshot_id: value.snapshot_id,
            source_node_id: value.source_node_id,
            target_node_id: value.target_node_id,
            kind: value.kind,
            disposition,
            reason: value.reason,
            decision: value.decision,
            config_version: value.config_version,
        })
    }
}

pub(crate) fn work_path(database: &Path) -> PathBuf {
    let mut value = database.as_os_str().to_os_string();
    value.push(".work-v1.db");
    PathBuf::from(value)
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
        // SQL shape errors can only come from an obsolete development sidecar because
        // all statements above are fixed by this binary's single v1 schema.
        rusqlite::Error::SqlInputError { .. } => WorkOpenError::Recreate,
        _ => WorkOpenError::Fatal(error.to_string()),
    }
}

fn ensure_private_file(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if path.exists() {
        let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(format!("unsafe work sidecar path `{}`", path.display()));
        }
        set_owner_only(path)?;
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
    set_owner_only(path)
}

fn ensure_safe_sqlite_siblings(path: &Path) -> Result<(), String> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut value = path.as_os_str().to_os_string();
        value.push(suffix);
        let candidate = PathBuf::from(value);
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
        set_owner_only(&candidate)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| error.to_string())
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn set_owner_only(path: &Path) -> Result<(), String> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;

    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SetFileSecurityW
    };

    let mut path_wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if path_wide.contains(&0) {
        return Err("work sidecar path contains a NUL code unit".to_owned());
    }
    path_wide.push(0);
    // Protected DACL with one full-control ACE for the object owner. `OW` is the Windows
    // Owner-Rights SID, avoiding localized account names and inherited broad principals.
    let descriptor_sddl = "D:P(A;;FA;;;OW)\0".encode_utf16().collect::<Vec<_>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: both UTF-16 inputs are NUL-terminated, `descriptor` is a valid out-pointer, and the
    // returned LocalAlloc allocation is released exactly once below.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(format!(
            "failed to build owner-only work sidecar ACL: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `path_wide` is NUL-terminated and `descriptor` was initialized successfully above.
    let applied = unsafe {
        SetFileSecurityW(
            path_wide.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    // SAFETY: the descriptor was allocated by LocalAlloc inside the conversion API.
    let _released = unsafe { LocalFree(descriptor.cast::<c_void>()) };
    if applied == 0 {
        return Err(format!(
            "failed to apply owner-only work sidecar ACL: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn set_owner_only(_path: &Path) -> Result<(), String> {
    Err("owner-only work sidecar permissions are unsupported on this platform".to_owned())
}

fn remove_sidecar_files(path: &Path) -> Result<(), String> {
    for suffix in ["", "-wal", "-shm"] {
        let candidate = if suffix.is_empty() {
            path.to_path_buf()
        } else {
            let mut value = path.as_os_str().to_os_string();
            value.push(suffix);
            PathBuf::from(value)
        };
        if candidate.exists() {
            fs::remove_file(&candidate).map_err(|error| {
                format!(
                    "failed to recreate incompatible work sidecar `{}`: {error}",
                    candidate.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{
        CheckoutId, CommunityAlgorithm, CommunityConfig, CommunityScope, NativePath, NativePathEncoding, RepoId
    };

    use super::*;

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
        assert!(state.put_batch(&batch, 1_000_000, 1).expect("cache"));
        assert_eq!(
            state
                .load_batches(&[fingerprint("one")], "budget", "1.0.0", 1_000_000, 2)
                .expect("load"),
            vec![batch.clone()]
        );
        assert!(
            state
                .load_batches(&[fingerprint("two")], "budget", "1.0.0", 1_000_000, 3)
                .expect("load")
                .is_empty()
        );
        assert!(!state.put_batch(&batch, 1, 4).expect("oversized skip"));
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

        for suffix in ["", "-wal", "-shm"] {
            let candidate = PathBuf::from(format!("{}{}", sidecar.display(), suffix));
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
    fn ready_candidate_should_round_trip_and_be_removed_after_publication() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut state = WorkState::open(&temporary.path().join("graph.db"), "database-instance")
            .expect("work state");
        assert!(
            !state
                .begin_candidate("workspace", "compatible", 1)
                .expect("begin")
        );
        let snapshot = StagedSnapshot {
            snapshot_id: "snapshot:one".to_owned(),
            nodes: Vec::new(),
            edges: Vec::new(),
            evidence: Vec::new(),
            fingerprints: vec![fingerprint("one")],
            extractor_batches: Vec::new(),
            extractor_runs: Vec::new(),
            manual_links: Vec::new(),
            community_snapshot: CommunitySnapshot {
                snapshot_id: "snapshot:one".to_owned(),
                engine_version: "1.0.0".to_owned(),
                config: CommunityConfig {
                    algorithm: CommunityAlgorithm::Louvain,
                    scope: CommunityScope::Workspace,
                    seed: 0,
                    resolution: 1.0,
                    minimum_confidence: 0.0,
                    edge_weights: Vec::new(),
                    max_iterations: 1,
                },
                communities: Vec::new(),
            },
            community_delta_count: 0,
            corroborated_symbol_count: 0,
            affected_test_count: 0,
            execution: ExecutionSummary {
                checkpoints_written: 1,
                ..ExecutionSummary::default()
            },
            degradations: Vec::new(),
        };
        state
            .store_candidate_snapshot("workspace", "compatible", &snapshot, 2)
            .expect("stage");
        let loaded = state
            .load_candidate_snapshot("workspace", "compatible")
            .expect("load")
            .expect("ready candidate");
        assert_eq!(loaded.snapshot_id, snapshot.snapshot_id);
        assert_eq!(loaded.fingerprints, snapshot.fingerprints);
        state.complete_candidate("workspace").expect("complete");
        assert!(
            state
                .load_candidate_snapshot("workspace", "compatible")
                .expect("load after completion")
                .is_none()
        );
    }
}

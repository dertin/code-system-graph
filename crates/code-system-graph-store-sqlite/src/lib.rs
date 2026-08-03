//! `SQLite` persistence adapter for `Code System Graph` snapshots.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use code_system_graph_model::{
    ArtifactFingerprint, CheckoutId, Community, CommunityAlgorithm, CommunityConfig, CommunityId, CommunityMetrics, CommunitySnapshot, Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, EvidenceId, ExtractorRun, LinkDecision, LinkStatus, NativePath, Node, NodeId, NodeKind, RepoFreshness, RepoFreshnessState, RepoId, RepositoryRecord, StoredExtractorBatch, WorkspaceId, WorkspaceRecord, contains_unsafe_metadata_characters, stable_id
};
use rusqlite::backup::Backup;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use sysinfo::{Pid, ProcessesToUpdate, System};
use thiserror::Error;

const INITIAL_SCHEMA: &str = include_str!("../../../migrations/0001_initial.sql");
const LATEST_SCHEMA_VERSION: i64 = 1;

/// Returns the newest on-disk schema version supported by this binary.
#[must_use]
pub const fn latest_schema_version() -> i64 {
    LATEST_SCHEMA_VERSION
}

/// Source-free runtime capabilities observed from one open store connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreDiagnostics {
    /// Active `SQLite` journal mode.
    pub journal_mode: String,
    /// Whether foreign-key enforcement is active.
    pub foreign_keys_enabled: bool,
    /// Whether the managed FTS5 node index exists.
    pub fts5_index_available: bool,
}
const COMMUNITY_ALGORITHM_JSON_MAX_BYTES: usize = 256;
const COMMUNITY_CONFIG_JSON_MAX_BYTES: usize = 65_536;
const COMMUNITY_METRICS_JSON_MAX_BYTES: usize = 65_536;
const COMMUNITY_EXPLANATION_JSON_MAX_BYTES: usize = 1_048_576;
const NODE_SEARCH_QUERY_MAX_BYTES: usize = 1_024;
const MANUAL_LINK_ID_MAX_BYTES: usize = 2_048;
const MANUAL_LINK_KIND_MAX_BYTES: usize = 256;
const MANUAL_LINK_REASON_MAX_BYTES: usize = 4_096;
const MANUAL_LINK_DECISION_JSON_MAX_BYTES: usize = 262_144;
const PROVIDER_COMPONENT_MAX_BYTES: usize = 256;
const PROVIDER_CAPABILITIES_JSON_MAX_BYTES: usize = 65_536;
const PROVIDER_CAPABILITY_COUNT_MAX: usize = 256;
const QUERY_CACHE_FINGERPRINT_MAX_BYTES: usize = 256;
const QUERY_CACHE_RESULT_MAX_BYTES: usize = 1_048_576;

/// Embedded `SQLite` store with transactional snapshot publication.
pub struct SqliteStore {
    connection: Connection,
}

/// Exclusive writer lock represented by a restrictive sidecar file.
#[derive(Debug)]
pub struct StoreLock {
    path: PathBuf,
    content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LockMetadata {
    pid: u32,
    process_started_at: u64,
    token: String,
}

struct StoredWorkspaceIdentity {
    id: String,
    manifest_hash: String,
    config_path: Option<NativePath>,
}

struct StoredRepositoryRow {
    alias: String,
    repo_id: String,
    checkout_id: String,
    path_encoding: String,
    path_bytes: Vec<u8>,
    path_display: String,
    common_encoding: Option<String>,
    common_bytes: Option<Vec<u8>>,
    common_display: Option<String>,
    normalized_remote: Option<String>,
    head_commit: Option<String>,
    is_linked_worktree: bool,
    working_tree_dirty: bool,
}

/// Counts and identity of a published snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSnapshotSummary {
    /// Stable snapshot identifier.
    pub snapshot_id: String,
    /// Number of graph nodes.
    pub node_count: usize,
    /// Number of graph edges.
    pub edge_count: usize,
    /// Number of evidence records.
    pub evidence_count: usize,
}

/// One ranked full-text match from the current graph snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredNodeSearchHit {
    /// Matched graph node.
    pub node: Node,
    /// Native FTS5 `bm25` score; lower values are more relevant.
    pub fts_rank: f64,
}

/// Whether a manual record creates a link or suppresses an automatically inferred link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualLinkDisposition {
    /// Include the declared link in the snapshot.
    Active,
    /// Suppress an automatically inferred link with the same endpoints and kind.
    Suppression,
}

impl ManualLinkDisposition {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Suppression => "suppression",
        }
    }

    fn from_stored(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "suppression" => Some(Self::Suppression),
            _ => None,
        }
    }
}

/// Source-free manual link declaration retained with one immutable graph snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct ManualLinkRecord {
    /// Stable identifier for this configured declaration.
    pub id: String,
    /// Snapshot that owns this historical record.
    pub snapshot_id: String,
    /// Existing source node in the same snapshot.
    pub source_node_id: NodeId,
    /// Existing target node in the same snapshot.
    pub target_node_id: NodeId,
    /// Normalized link kind used to match active or inferred links.
    pub kind: String,
    /// Whether the declaration creates or suppresses the link.
    pub disposition: ManualLinkDisposition,
    /// Required operator rationale without source excerpts or provider payloads.
    pub reason: String,
    /// Complete versioned linker decision retained for explainability.
    pub decision: LinkDecision,
    /// Version of the configuration format that produced this record.
    pub config_version: u32,
}

/// Source-free capability discovery result for one provider version and repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderCapabilityRecord {
    /// Workspace containing the registered repository.
    pub workspace_name: String,
    /// Stable repository identifier.
    pub repo_id: RepoId,
    /// Provider implementation name.
    pub provider: String,
    /// Probed provider version.
    pub provider_version: String,
    /// Normalized capability names, without provider responses or source.
    pub capabilities: Vec<String>,
    /// Observation timestamp in Unix milliseconds.
    pub observed_at_unix_ms: u64,
}

/// Bounded source-free cached result for one immutable snapshot and exact request fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryCacheRecord {
    /// Workspace owning the immutable snapshot.
    pub workspace_name: String,
    /// Snapshot used to compute the result.
    pub snapshot_id: String,
    /// Stable fingerprint of the complete public query input.
    pub input_fingerprint: String,
    /// Versioned JSON result without source bodies.
    pub result_summary_json: Vec<u8>,
    /// Observation timestamp in Unix milliseconds.
    pub stored_at_unix_ms: u64,
    /// Optional expiry timestamp in Unix milliseconds.
    pub expires_at_unix_ms: Option<u64>,
}

/// Immutable inputs published as one atomic snapshot transaction.
#[derive(Debug, Clone, Copy)]
pub struct SnapshotBatch<'a> {
    /// Validated workspace registry.
    pub workspace: &'a WorkspaceRecord,
    /// Stable snapshot identifier.
    pub snapshot_id: &'a str,
    /// Complete graph node set.
    pub nodes: &'a [Node],
    /// Complete graph edge set.
    pub edges: &'a [Edge],
    /// Complete evidence set.
    pub evidence: &'a [Evidence],
    /// Current extractor-relevant artifact fingerprints.
    pub fingerprints: &'a [ArtifactFingerprint],
    /// Reusable source-owned extractor outputs.
    pub extractor_batches: &'a [StoredExtractorBatch],
    /// Extractor execution metrics.
    pub extractor_runs: &'a [ExtractorRun],
    /// Source-free manual link declarations for this exact snapshot.
    pub manual_links: &'a [ManualLinkRecord],
    /// Optional community analysis for this exact graph snapshot.
    pub community_snapshot: Option<&'a CommunitySnapshot>,
}

/// Result of restoring a database backup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    /// Backup used as the restore source.
    pub source_path: PathBuf,
    /// Safety backup of the replaced database, when it existed.
    pub safety_backup_path: Option<PathBuf>,
    /// Exact schema version restored.
    pub schema_version: i64,
}

/// Compact persisted workspace registry entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRegistrySummary {
    /// Stable workspace identifier.
    pub id: WorkspaceId,
    /// User-facing workspace name.
    pub name: String,
    /// Current merged manifest fingerprint.
    pub manifest_hash: String,
    /// Canonical workspace manifest path.
    pub config_path: Option<NativePath>,
    /// Number of registered repository aliases.
    pub repository_count: usize,
}

/// Error returned by the `SQLite` adapter.
#[derive(Debug, Error)]
pub enum StoreError {
    /// `SQLite` operation failed.
    #[error("SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A domain tag could not be encoded or decoded.
    #[error("stored domain value is invalid: {0}")]
    Serialization(#[from] serde_json::Error),
    /// No current snapshot exists for a workspace.
    #[error("workspace `{0}` has no current snapshot")]
    CurrentSnapshotMissing(String),
    /// Persisted workspace registry is absent or incomplete.
    #[error("workspace registry `{0}` is missing required identity data")]
    RegistryIncomplete(String),
    /// Filesystem operation failed.
    #[error("filesystem operation failed for `{path}`: {source}")]
    Io {
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Underlying operating-system error.
        source: std::io::Error,
    },
    /// A database uses anything other than the definitive unpublished 1.0.0 schema.
    #[error(
        "database is incompatible with the definitive 1.0.0 schema; remove it and run a full scan to rebuild"
    )]
    ObsoleteDevelopmentDatabase,
    /// Another writer owns a non-stale lock.
    #[error("store writer lock is already held at `{0}`")]
    LockHeld(PathBuf),
    /// Integer cannot be represented by the target domain type.
    #[error("stored integer `{field}` is outside the supported range: {value}")]
    IntegerOutOfRange {
        /// Field being converted.
        field: &'static str,
        /// Invalid signed value.
        value: i128,
    },
    /// Backup failed validation or aliases the destination.
    #[error("invalid backup `{path}`: {reason}")]
    InvalidBackup {
        /// Backup path.
        path: PathBuf,
        /// Actionable validation reason.
        reason: String,
    },
    /// Bounded FTS query is empty or has an invalid limit.
    #[error("invalid node search query: {0}")]
    InvalidSearchQuery(String),
    /// A graph snapshot violates identity, reference, or safe-metadata invariants.
    #[error("graph snapshot is invalid: {0}")]
    InvalidGraphSnapshot(String),
    /// A requested immutable graph snapshot does not exist.
    #[error("graph snapshot `{0}` does not exist")]
    SnapshotMissing(String),
    /// A requested graph snapshot has no persisted community analysis.
    #[error("graph snapshot `{0}` has no community analysis")]
    CommunitySnapshotMissing(String),
    /// Community analysis targets a different graph snapshot.
    #[error(
        "community snapshot `{community_snapshot_id}` does not match graph snapshot \
         `{batch_snapshot_id}`"
    )]
    CommunitySnapshotMismatch {
        /// Graph snapshot being published.
        batch_snapshot_id: String,
        /// Graph snapshot named by the community analysis.
        community_snapshot_id: String,
    },
    /// A community identifier occurs more than once in one analysis.
    #[error("community `{community_id}` is duplicated in snapshot `{snapshot_id}`")]
    DuplicateCommunityId {
        /// Graph snapshot being validated.
        snapshot_id: String,
        /// Repeated community identifier.
        community_id: String,
    },
    /// A node occurs more than once in one community membership list.
    #[error(
        "node `{node_id}` is duplicated in community `{community_id}` for snapshot `{snapshot_id}`"
    )]
    DuplicateCommunityMembership {
        /// Graph snapshot being validated.
        snapshot_id: String,
        /// Community containing the repeated member.
        community_id: String,
        /// Repeated member node.
        node_id: String,
    },
    /// A community membership references a node absent from its graph snapshot.
    #[error(
        "community `{community_id}` references missing node `{node_id}` in snapshot `{snapshot_id}`"
    )]
    CommunityMembershipNodeMissing {
        /// Graph snapshot being validated.
        snapshot_id: String,
        /// Community containing the invalid member.
        community_id: String,
        /// Missing graph node.
        node_id: String,
    },
    /// A variable JSON payload exceeds its schema bound.
    #[error("community JSON field `{field}` is {actual_bytes} bytes; maximum is {max_bytes}")]
    CommunityJsonTooLarge {
        /// Logical JSON field.
        field: &'static str,
        /// Encoded UTF-8 byte length.
        actual_bytes: usize,
        /// Maximum accepted byte length.
        max_bytes: usize,
    },
    /// Persisted rows cannot be decoded into a valid domain value.
    #[error("stored {entity} is malformed: {reason}")]
    MalformedStoredData {
        /// Stored entity being decoded.
        entity: String,
        /// Specific invariant or decoding failure.
        reason: String,
    },
    /// A store-local persistence record violates safety, size, or reference invariants.
    #[error("invalid persistence record: {0}")]
    InvalidPersistenceRecord(String),
}

impl StoreLock {
    /// Acquires an exclusive writer lock and recovers an expired lock once.
    ///
    /// The sidecar path is the database path with `.lock` appended to its extension. On Unix it
    /// is created with mode `0600`. Dropping the returned guard removes only a lock whose token
    /// still matches this owner.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::LockHeld`] for an active lock or [`StoreError::Io`] when lock
    /// metadata cannot be created, inspected, or replaced.
    pub fn acquire(database_path: &Path, stale_after: Duration) -> Result<Self, StoreError> {
        let path = lock_path(database_path);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|source| StoreError::Io {
                path: path.clone(),
                source: std::io::Error::other(source),
            })?;
        let token = stable_id(
            "lock",
            &format!(
                "{}:{}:{}",
                std::process::id(),
                now.as_secs(),
                now.subsec_nanos()
            ),
        );
        let content = LockMetadata {
            pid: std::process::id(),
            process_started_at: process_start_time(std::process::id()).unwrap_or_default(),
            token,
        }
        .encode();
        for attempt in 0..2 {
            match create_lock_file(&path) {
                Ok(mut file) => {
                    file.write_all(content.as_bytes())
                        .and_then(|()| file.sync_all())
                        .map_err(|source| StoreError::Io {
                            path: path.clone(),
                            source,
                        })?;
                    return Ok(Self { path, content });
                }
                Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                    if attempt == 0 && lock_is_stale(&path, stale_after)? {
                        fs::remove_file(&path).map_err(|source| StoreError::Io {
                            path: path.clone(),
                            source,
                        })?;
                        continue;
                    }
                    return Err(StoreError::LockHeld(path));
                }
                Err(source) => {
                    return Err(StoreError::Io {
                        path: path.clone(),
                        source,
                    });
                }
            }
        }
        Err(StoreError::LockHeld(path))
    }
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let owned = fs::read_to_string(&self.path).is_ok_and(|content| content == self.content);
        if owned {
            let _result = fs::remove_file(&self.path);
        }
    }
}

impl SqliteStore {
    /// Opens an exact 1.0.0 store or initializes a new empty database.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if `SQLite` cannot open, initialize, or validate the database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let connection = open_exact(path.as_ref())?;
        Ok(Self { connection })
    }

    /// Creates a validated online backup of the exact 1.0.0 schema.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the source is invalid or the destination already exists.
    pub fn backup_file(database_path: &Path, destination: &Path) -> Result<(), StoreError> {
        ensure_distinct_paths(database_path, destination)?;
        if destination.exists() {
            return Err(StoreError::Io {
                path: destination.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "backup destination already exists",
                ),
            });
        }
        validate_pinned_backup_source(database_path)?;
        prepare_database_file(destination)?;
        let mut destination_connection = Connection::open(destination)?;
        let result = copy_validated_backup_source(database_path, &mut destination_connection)
            .and_then(|()| restrict_store_permissions(destination));
        if result.is_err() {
            remove_database_artifacts(destination);
        }
        result
    }

    /// Restores a validated backup with the exact 1.0.0 schema.
    ///
    /// The existing destination is first preserved as a non-overwriting safety backup.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if validation, locking, safety backup, or restore fails.
    pub fn restore_from(
        database_path: &Path,
        backup_path: &Path,
    ) -> Result<RestoreReport, StoreError> {
        ensure_distinct_paths(database_path, backup_path)?;
        let _lock = StoreLock::acquire(database_path, Duration::from_mins(5))?;
        validate_pinned_backup_source(backup_path)?;
        let database_existed = database_path.exists();
        let safety_backup_path = if database_existed {
            let existing = Connection::open(database_path)?;
            let safety_path = next_backup_path(database_path, "pre-restore", Some(backup_path))?;
            backup_connection(&existing, &safety_path)?;
            restrict_store_permissions(&safety_path)?;
            Some(safety_path)
        } else {
            None
        };
        if database_existed {
            ensure_restorable_permissions(database_path)?;
        }
        prepare_database_file(database_path)?;
        let mut destination = Connection::open(database_path)?;
        let restore_result = (|| {
            copy_validated_backup_source(backup_path, &mut destination)?;
            configure_connection(&destination)?;
            validate_backup(&destination, database_path)?;
            destination.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
            restrict_store_permissions(database_path)?;
            schema_version(&destination)
        })();
        match restore_result {
            Ok(schema_version) => Ok(RestoreReport {
                source_path: backup_path.to_path_buf(),
                safety_backup_path,
                schema_version,
            }),
            Err(error) => {
                if database_existed {
                    if let Some(ref safety_path) = safety_backup_path {
                        let _ = rollback_restore_from_safety_backup(database_path, safety_path);
                    }
                } else {
                    remove_database_artifacts(database_path);
                }
                Err(error)
            }
        }
    }

    /// Opens an existing store without writes or implicit schema changes.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the database is absent, corrupt, or not the exact 1.0.0 schema.
    pub fn open_read_only(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let connection = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        validate_exact_schema(&connection)?;
        Ok(Self { connection })
    }

    /// Creates an in-memory store for isolated tests and ephemeral operations.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if `SQLite` cannot configure or initialize the database.
    pub fn in_memory() -> Result<Self, StoreError> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(mut connection: Connection) -> Result<Self, StoreError> {
        configure_connection(&connection)?;
        initialize_empty_schema(&mut connection)?;
        validate_exact_schema(&connection)?;
        Ok(Self { connection })
    }

    /// Returns the exact initial schema version recorded by this store.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when schema metadata cannot be read.
    pub fn schema_version(&self) -> Result<i64, StoreError> {
        schema_version(&self.connection)
    }

    /// Returns the opaque identity that binds disposable operational state to this database.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when exact schema metadata cannot be read.
    pub fn database_instance_id(&self) -> Result<String, StoreError> {
        database_instance_id(&self.connection)
    }

    /// Atomically replaces repository registrations for one workspace.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on serialization, constraint, or transaction failure.
    pub fn save_workspace_registry(
        &mut self,
        workspace: &WorkspaceRecord,
    ) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        upsert_registry(&transaction, workspace)?;
        transaction.commit()?;
        Ok(())
    }

    /// Returns whether a workspace name is already persisted.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the registry cannot be queried.
    pub fn workspace_exists(&self, workspace: &str) -> Result<bool, StoreError> {
        Ok(self.connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM workspaces WHERE name = ?1)",
            [workspace],
            |row| row.get(0),
        )?)
    }

    /// Removes one workspace and garbage-collects unreferenced repository records.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the transactional removal fails.
    pub fn remove_workspace(&mut self, workspace: &str) -> Result<bool, StoreError> {
        let transaction = self.connection.transaction()?;
        let removed = transaction.execute("DELETE FROM workspaces WHERE name = ?1", [workspace])?;
        transaction.execute(
            "DELETE FROM repository_checkouts
             WHERE NOT EXISTS (
                SELECT 1 FROM workspace_repositories wr
                WHERE wr.checkout_id = repository_checkouts.id
             )",
            [],
        )?;
        transaction.execute(
            "DELETE FROM repositories
             WHERE NOT EXISTS (
                SELECT 1 FROM workspace_repositories wr
                WHERE wr.repo_id = repositories.id
             )",
            [],
        )?;
        transaction.commit()?;
        Ok(removed > 0)
    }

    /// Lists persisted workspaces in deterministic name order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when registry rows or counts cannot be read.
    pub fn list_workspaces(&self) -> Result<Vec<WorkspaceRegistrySummary>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT
                w.id, w.name, w.manifest_hash, w.config_path_encoding, w.config_path,
                w.config_path_display, COUNT(wr.alias)
             FROM workspaces w
             LEFT JOIN workspace_repositories wr ON wr.workspace_name = w.name
             WHERE w.id IS NOT NULL
             GROUP BY
                w.id, w.name, w.manifest_hash, w.config_path_encoding, w.config_path,
                w.config_path_display
             ORDER BY w.name",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<Vec<u8>>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(id, name, manifest_hash, encoding, bytes, display, repository_count)| {
                    let config_path = match (encoding, bytes, display) {
                        (Some(encoding), Some(bytes), Some(display)) => Some(NativePath {
                            encoding: serde_json::from_str(&encoding)?,
                            bytes,
                            display,
                        }),
                        _ => None,
                    };
                    Ok(WorkspaceRegistrySummary {
                        id: WorkspaceId::new(id),
                        name,
                        manifest_hash,
                        config_path,
                        repository_count: count_to_usize(
                            "workspace_repositories",
                            repository_count,
                        )?,
                    })
                },
            )
            .collect()
    }

    /// Loads a workspace and its repository registrations in alias order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::RegistryIncomplete`] when required identity data is missing, or
    /// another [`StoreError`] when stored values are invalid.
    pub fn load_workspace_registry(&self, workspace: &str) -> Result<WorkspaceRecord, StoreError> {
        let identity = load_workspace_identity(&self.connection, workspace)?;
        let repositories = load_workspace_repositories(&self.connection, workspace)?;
        Ok(WorkspaceRecord {
            id: WorkspaceId::new(identity.id),
            name: workspace.to_owned(),
            manifest_hash: identity.manifest_hash,
            config_path: identity.config_path,
            repositories,
        })
    }

    /// Creates a consistent online backup at a new destination path.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the destination already exists or the backup cannot complete.
    pub fn backup_to(&self, destination: &Path) -> Result<(), StoreError> {
        backup_connection(&self.connection, destination)
    }

    /// Loads per-repository freshness from the current workspace snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when no current snapshot exists or stored freshness is invalid.
    pub fn load_current_freshness(
        &self,
        workspace: &str,
    ) -> Result<Vec<RepoFreshness>, StoreError> {
        let snapshot_id = self.current_snapshot_id(workspace)?;
        let mut statement = self.connection.prepare(
            "SELECT repo_id, checkout_id, head_commit, manifest_hash, state, reason
             FROM repository_snapshot_freshness
             WHERE snapshot_id = ?1
             ORDER BY repo_id, checkout_id",
        )?;
        let rows = statement
            .query_map([snapshot_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(repo_id, checkout_id, head_commit, manifest_hash, state, reason)| {
                    Ok(RepoFreshness {
                        repo_id: RepoId::new(repo_id),
                        checkout_id: CheckoutId::new(checkout_id),
                        head_commit,
                        manifest_hash,
                        state: serde_json::from_str::<RepoFreshnessState>(&state)?,
                        reason,
                    })
                },
            )
            .collect()
    }

    /// Loads extractor-relevant fingerprints from the current snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when no current snapshot exists or stored paths are invalid.
    pub fn load_current_artifact_fingerprints(
        &self,
        workspace: &str,
    ) -> Result<Vec<ArtifactFingerprint>, StoreError> {
        let snapshot_id = self.current_snapshot_id(workspace)?;
        let mut statement = self.connection.prepare(
            "SELECT
                repo_id, checkout_id, path_encoding, relative_path, path_display,
                extractor, content_hash, size_bytes
             FROM artifact_fingerprints
             WHERE snapshot_id = ?1
             ORDER BY repo_id, checkout_id, path_encoding, relative_path, extractor",
        )?;
        let rows = statement
            .query_map([snapshot_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(
                    repo_id,
                    checkout_id,
                    encoding,
                    bytes,
                    display,
                    extractor,
                    content_hash,
                    stored_size_bytes,
                )| {
                    Ok(ArtifactFingerprint {
                        repo_id: RepoId::new(repo_id),
                        checkout_id: CheckoutId::new(checkout_id),
                        path: NativePath {
                            encoding: serde_json::from_str(&encoding)?,
                            bytes,
                            display,
                        },
                        extractor,
                        content_hash,
                        size_bytes: u64::try_from(stored_size_bytes).map_err(|_| {
                            StoreError::IntegerOutOfRange {
                                field: "artifact_fingerprints.size_bytes",
                                value: i128::from(stored_size_bytes),
                            }
                        })?,
                    })
                },
            )
            .collect()
    }

    /// Loads reusable source-owned extractor outputs from the current snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when no current snapshot exists or stored values are invalid.
    pub fn load_current_extractor_batches(
        &self,
        workspace: &str,
    ) -> Result<Vec<StoredExtractorBatch>, StoreError> {
        self.load_current_extractor_batches_with_limit(workspace, i64::MAX as u64)
    }

    /// Loads current extractor batches whose payload fits within `maximum_payload_bytes`.
    ///
    /// Oversized rows are omitted before `SQLite` copies their BLOB into the process. Callers can
    /// consequently treat them as cache misses and recompute them under the active policy.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when no current snapshot exists or stored values are invalid.
    pub fn load_current_extractor_batches_with_limit(
        &self,
        workspace: &str,
        maximum_payload_bytes: u64,
    ) -> Result<Vec<StoredExtractorBatch>, StoreError> {
        let snapshot_id = self.current_snapshot_id(workspace)?;
        let maximum_payload_bytes = i64::try_from(maximum_payload_bytes).unwrap_or(i64::MAX);
        let has_obsolete_contract = self.connection.query_row(
            "SELECT EXISTS(
                SELECT 1
                FROM extractor_batches
                WHERE snapshot_id = ?1
                  AND (extractor_version <> '1.0.0' OR budget_fingerprint = '')
             )",
            [&snapshot_id],
            |row| row.get::<_, bool>(0),
        )?;
        if has_obsolete_contract {
            return Err(StoreError::ObsoleteDevelopmentDatabase);
        }
        let mut statement = self.connection.prepare(
            "SELECT
                repo_id, checkout_id, path_encoding, relative_path, path_display,
                extractor, content_hash, size_bytes, extractor_version, budget_fingerprint,
                source_was_lossy, output_count, payload
             FROM extractor_batches
             WHERE snapshot_id = ?1 AND length(payload) <= ?2
             ORDER BY repo_id, checkout_id, path_encoding, relative_path, extractor",
        )?;
        let rows = statement
            .query_map(params![snapshot_id, maximum_payload_bytes], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, bool>(10)?,
                    row.get::<_, i64>(11)?,
                    row.get::<_, Vec<u8>>(12)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(
                    repo_id,
                    checkout_id,
                    encoding,
                    bytes,
                    display,
                    extractor,
                    content_hash,
                    stored_size_bytes,
                    extractor_version,
                    budget_fingerprint,
                    source_was_lossy,
                    stored_output_count,
                    payload,
                )| {
                    Ok(StoredExtractorBatch {
                        source: ArtifactFingerprint {
                            repo_id: RepoId::new(repo_id),
                            checkout_id: CheckoutId::new(checkout_id),
                            path: NativePath {
                                encoding: serde_json::from_str(&encoding)?,
                                bytes,
                                display,
                            },
                            extractor,
                            content_hash,
                            size_bytes: stored_metric_to_u64(
                                "extractor_batches.size_bytes",
                                stored_size_bytes,
                            )?,
                        },
                        extractor_version,
                        budget_fingerprint,
                        source_was_lossy,
                        output_count: stored_metric_to_u64(
                            "extractor_batches.output_count",
                            stored_output_count,
                        )?,
                        payload,
                    })
                },
            )
            .collect()
    }

    /// Loads extractor run metrics from the current snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when no current snapshot exists or run metrics are invalid.
    pub fn load_current_extractor_runs(
        &self,
        workspace: &str,
    ) -> Result<Vec<ExtractorRun>, StoreError> {
        let snapshot_id = self.current_snapshot_id(workspace)?;
        let mut statement = self.connection.prepare(
            "SELECT
                id, repo_id, checkout_id, extractor, extractor_version, status,
                discovered_files, parsed_files, skipped_files, elapsed_ms
             FROM extractor_runs
             WHERE snapshot_id = ?1
             ORDER BY repo_id, checkout_id, extractor, id",
        )?;
        let rows = statement
            .query_map([&snapshot_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(
                    id,
                    repo_id,
                    checkout_id,
                    extractor,
                    extractor_version,
                    status,
                    discovered_files,
                    parsed_files,
                    skipped_files,
                    elapsed_ms,
                )| {
                    Ok(ExtractorRun {
                        id,
                        snapshot_id: snapshot_id.clone(),
                        repo_id: RepoId::new(repo_id),
                        checkout_id: CheckoutId::new(checkout_id),
                        extractor,
                        extractor_version,
                        status: serde_json::from_str(&status)?,
                        discovered_files: stored_metric_to_u64(
                            "extractor_runs.discovered_files",
                            discovered_files,
                        )?,
                        parsed_files: stored_metric_to_u64(
                            "extractor_runs.parsed_files",
                            parsed_files,
                        )?,
                        skipped_files: stored_metric_to_u64(
                            "extractor_runs.skipped_files",
                            skipped_files,
                        )?,
                        elapsed_ms: stored_metric_to_u64("extractor_runs.elapsed_ms", elapsed_ms)?,
                    })
                },
            )
            .collect()
    }

    /// Replaces manual link declarations for one existing snapshot in a transaction.
    ///
    /// Records for other snapshots are retained, preserving historical declarations. Passing an
    /// empty slice clears only the selected snapshot's declarations.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the snapshot or endpoint nodes are absent, metadata is unsafe
    /// or oversized, records conflict, or the transaction fails.
    pub fn persist_manual_links(
        &mut self,
        snapshot_id: &str,
        records: &[ManualLinkRecord],
    ) -> Result<(), StoreError> {
        validate_manual_links(snapshot_id, records, None)?;
        self.require_snapshot(snapshot_id)?;
        let transaction = self.connection.transaction()?;
        for record in records {
            for (field, node_id) in [
                ("source", record.source_node_id.as_str()),
                ("target", record.target_node_id.as_str()),
            ] {
                let exists = transaction.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM nodes WHERE snapshot_id = ?1 AND id = ?2
                     )",
                    params![snapshot_id, node_id],
                    |row| row.get::<_, bool>(0),
                )?;
                if !exists {
                    return Err(StoreError::InvalidPersistenceRecord(format!(
                        "manual link `{}` {field} node `{node_id}` is absent from snapshot \
                         `{snapshot_id}`",
                        record.id
                    )));
                }
            }
        }
        transaction.execute(
            "DELETE FROM manual_links WHERE snapshot_id = ?1",
            [snapshot_id],
        )?;
        insert_manual_links(&transaction, records)?;
        transaction.commit()?;
        Ok(())
    }

    /// Loads manual link declarations for one immutable snapshot in stable identifier order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::SnapshotMissing`] when the snapshot is absent or another
    /// [`StoreError`] when persisted rows are malformed.
    pub fn load_manual_links(
        &self,
        snapshot_id: &str,
    ) -> Result<Vec<ManualLinkRecord>, StoreError> {
        self.require_snapshot(snapshot_id)?;
        let mut statement = self.connection.prepare(
            "SELECT id, source_node_id, target_node_id, kind, disposition, reason, decision_json,
                    config_version
             FROM manual_links
             WHERE snapshot_id = ?1
             ORDER BY id",
        )?;
        let rows = statement
            .query_map([snapshot_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Vec<u8>>(6)?,
                    row.get::<_, i64>(7)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let records = rows
            .into_iter()
            .map(
                |(id, source, target, kind, disposition, reason, decision_json, config_version)| {
                    let disposition =
                        ManualLinkDisposition::from_stored(&disposition).ok_or_else(|| {
                            malformed_stored_data(
                                format!("manual link `{id}` in snapshot `{snapshot_id}`"),
                                format!("unknown disposition `{disposition}`"),
                            )
                        })?;
                    let config_version = u32::try_from(config_version).map_err(|_| {
                        StoreError::IntegerOutOfRange {
                            field: "manual_links.config_version",
                            value: i128::from(config_version),
                        }
                    })?;
                    let decision = serde_json::from_slice(&decision_json).map_err(|error| {
                        malformed_stored_data(
                            format!("manual link `{id}` decision in snapshot `{snapshot_id}`"),
                            error.to_string(),
                        )
                    })?;
                    Ok(ManualLinkRecord {
                        id,
                        snapshot_id: snapshot_id.to_owned(),
                        source_node_id: NodeId::new(source),
                        target_node_id: NodeId::new(target),
                        kind,
                        disposition,
                        reason,
                        decision,
                        config_version,
                    })
                },
            )
            .collect::<Result<Vec<_>, StoreError>>()?;
        validate_manual_links(snapshot_id, &records, None).map_err(|error| {
            malformed_stored_data(
                format!("manual links in snapshot `{snapshot_id}`"),
                error.to_string(),
            )
        })?;
        Ok(records)
    }

    /// Inserts or replaces one source-free provider capability report.
    ///
    /// The repository must currently be registered in the selected workspace. Replacement is
    /// scoped by workspace, repository, provider, and provider version.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for invalid metadata, an unregistered repository, serialization,
    /// integer conversion, or constraint failures.
    pub fn upsert_provider_capabilities(
        &mut self,
        record: &ProviderCapabilityRecord,
    ) -> Result<(), StoreError> {
        validate_provider_capability_record(record)?;
        let registered = self.connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM workspace_repositories
                WHERE workspace_name = ?1 AND repo_id = ?2
             )",
            params![record.workspace_name, record.repo_id.as_str()],
            |row| row.get::<_, bool>(0),
        )?;
        if !registered {
            return Err(StoreError::InvalidPersistenceRecord(format!(
                "repository `{}` is not registered in workspace `{}`",
                record.repo_id.as_str(),
                record.workspace_name
            )));
        }
        let capabilities_json = serde_json::to_string(&record.capabilities)?;
        self.connection.execute(
            "INSERT INTO provider_capabilities(
                workspace_name, repo_id, provider, provider_version, capabilities_json,
                observed_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(workspace_name, repo_id, provider, provider_version) DO UPDATE SET
                capabilities_json = excluded.capabilities_json,
                observed_at_unix_ms = excluded.observed_at_unix_ms",
            params![
                record.workspace_name,
                record.repo_id.as_str(),
                record.provider,
                record.provider_version,
                capabilities_json,
                metric_to_i64(
                    "provider_capabilities.observed_at_unix_ms",
                    record.observed_at_unix_ms
                )?,
            ],
        )?;
        Ok(())
    }

    /// Loads one source-free provider capability report.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when lookup metadata is unsafe or oversized, or persisted data is
    /// malformed. An unknown scope returns `Ok(None)`.
    pub fn load_provider_capabilities(
        &self,
        workspace: &str,
        repo_id: &RepoId,
        provider: &str,
        provider_version: &str,
    ) -> Result<Option<ProviderCapabilityRecord>, StoreError> {
        validate_safe_metadata("workspace name", workspace, 1, MANUAL_LINK_ID_MAX_BYTES)?;
        validate_safe_metadata(
            "repository identifier",
            repo_id.as_str(),
            1,
            MANUAL_LINK_ID_MAX_BYTES,
        )?;
        validate_safe_metadata("provider name", provider, 1, PROVIDER_COMPONENT_MAX_BYTES)?;
        validate_safe_metadata(
            "provider version",
            provider_version,
            1,
            PROVIDER_COMPONENT_MAX_BYTES,
        )?;
        let stored = self
            .connection
            .query_row(
                "SELECT capabilities_json, observed_at_unix_ms
                 FROM provider_capabilities
                 WHERE workspace_name = ?1
                   AND repo_id = ?2
                   AND provider = ?3
                   AND provider_version = ?4",
                params![workspace, repo_id.as_str(), provider, provider_version],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        let Some((capabilities_json, observed_at_unix_ms)) = stored else {
            return Ok(None);
        };
        let capabilities = serde_json::from_str(&capabilities_json).map_err(|error| {
            malformed_stored_data(
                format!(
                    "provider capabilities `{workspace}/{}/{provider}/{provider_version}`",
                    repo_id.as_str()
                ),
                error.to_string(),
            )
        })?;
        let record = ProviderCapabilityRecord {
            workspace_name: workspace.to_owned(),
            repo_id: repo_id.clone(),
            provider: provider.to_owned(),
            provider_version: provider_version.to_owned(),
            capabilities,
            observed_at_unix_ms: stored_metric_to_u64(
                "provider_capabilities.observed_at_unix_ms",
                observed_at_unix_ms,
            )?,
        };
        validate_provider_capability_record(&record).map_err(|error| {
            malformed_stored_data("provider capability record", error.to_string())
        })?;
        Ok(Some(record))
    }

    /// Inserts or replaces one bounded query result for an immutable snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when metadata, JSON, timestamps, snapshot ownership, or size bounds
    /// are invalid.
    pub fn put_query_cache(&mut self, record: &QueryCacheRecord) -> Result<(), StoreError> {
        validate_query_cache_record(record)?;
        let snapshot_exists = self.connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM repo_snapshots
                WHERE id = ?1 AND workspace_name = ?2
             )",
            params![record.snapshot_id, record.workspace_name],
            |row| row.get::<_, bool>(0),
        )?;
        if !snapshot_exists {
            return Err(StoreError::InvalidPersistenceRecord(format!(
                "query cache snapshot `{}` is absent from workspace `{}`",
                record.snapshot_id, record.workspace_name
            )));
        }
        self.connection.execute(
            "INSERT INTO query_cache(
                workspace_name, snapshot_id, input_fingerprint, result_summary_json,
                stored_at_unix_ms, expires_at_unix_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(workspace_name, snapshot_id, input_fingerprint) DO UPDATE SET
                result_summary_json = excluded.result_summary_json,
                stored_at_unix_ms = excluded.stored_at_unix_ms,
                expires_at_unix_ms = excluded.expires_at_unix_ms",
            params![
                record.workspace_name,
                record.snapshot_id,
                record.input_fingerprint,
                record.result_summary_json,
                metric_to_i64("query_cache.stored_at_unix_ms", record.stored_at_unix_ms)?,
                record
                    .expires_at_unix_ms
                    .map(|value| metric_to_i64("query_cache.expires_at_unix_ms", value))
                    .transpose()?,
            ],
        )?;
        Ok(())
    }

    /// Loads one unexpired cached query result for an exact immutable-snapshot fingerprint.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when lookup metadata or persisted cache data is malformed.
    pub fn load_query_cache(
        &self,
        workspace: &str,
        snapshot_id: &str,
        input_fingerprint: &str,
        now_unix_ms: u64,
    ) -> Result<Option<QueryCacheRecord>, StoreError> {
        validate_safe_metadata("workspace name", workspace, 1, MANUAL_LINK_ID_MAX_BYTES)?;
        validate_safe_metadata(
            "snapshot identifier",
            snapshot_id,
            1,
            MANUAL_LINK_ID_MAX_BYTES,
        )?;
        validate_safe_metadata(
            "query cache input fingerprint",
            input_fingerprint,
            1,
            QUERY_CACHE_FINGERPRINT_MAX_BYTES,
        )?;
        let stored = self
            .connection
            .query_row(
                "SELECT result_summary_json, stored_at_unix_ms, expires_at_unix_ms
                 FROM query_cache
                 WHERE workspace_name = ?1
                   AND snapshot_id = ?2
                   AND input_fingerprint = ?3
                   AND (expires_at_unix_ms IS NULL OR expires_at_unix_ms >= ?4)",
                params![
                    workspace,
                    snapshot_id,
                    input_fingerprint,
                    metric_to_i64("query_cache.now_unix_ms", now_unix_ms)?
                ],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((result_summary_json, stored_at_unix_ms, expires_at_unix_ms)) = stored else {
            return Ok(None);
        };
        let record = QueryCacheRecord {
            workspace_name: workspace.to_owned(),
            snapshot_id: snapshot_id.to_owned(),
            input_fingerprint: input_fingerprint.to_owned(),
            result_summary_json,
            stored_at_unix_ms: stored_metric_to_u64(
                "query_cache.stored_at_unix_ms",
                stored_at_unix_ms,
            )?,
            expires_at_unix_ms: expires_at_unix_ms
                .map(|value| stored_metric_to_u64("query_cache.expires_at_unix_ms", value))
                .transpose()?,
        };
        validate_query_cache_record(&record)
            .map_err(|error| malformed_stored_data("query cache record", error.to_string()))?;
        Ok(Some(record))
    }

    /// Deletes every cached query result for one workspace.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the delete cannot be completed.
    pub fn clear_query_cache(&mut self, workspace: &str) -> Result<usize, StoreError> {
        Ok(self.connection.execute(
            "DELETE FROM query_cache WHERE workspace_name = ?1",
            [workspace],
        )?)
    }

    /// Loads counts and identity for the current snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when no current snapshot exists or counts cannot be read.
    pub fn current_snapshot_summary(
        &self,
        workspace: &str,
    ) -> Result<StoredSnapshotSummary, StoreError> {
        let snapshot_id = self.current_snapshot_id(workspace)?;
        let (node_count, edge_count, evidence_count) = self.connection.query_row(
            "SELECT
                (SELECT COUNT(*) FROM nodes WHERE snapshot_id = ?1),
                (SELECT COUNT(*) FROM edges WHERE snapshot_id = ?1),
                (SELECT COUNT(*) FROM evidence WHERE snapshot_id = ?1)",
            [&snapshot_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        Ok(StoredSnapshotSummary {
            snapshot_id,
            node_count: count_to_usize("nodes", node_count)?,
            edge_count: count_to_usize("edges", edge_count)?,
            evidence_count: count_to_usize("evidence", evidence_count)?,
        })
    }

    /// Atomically replaces the current workspace snapshot.
    ///
    /// The candidate is invisible as current until all nodes, evidence, edges, and evidence
    /// references are committed successfully.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] on community validation, constraint, serialization, or transaction
    /// failure.
    pub fn publish_snapshot(&mut self, batch: SnapshotBatch<'_>) -> Result<(), StoreError> {
        self.publish_snapshot_with_progress(batch, |_| {})
    }

    /// Publishes one atomic snapshot while reporting each completed durable row insertion.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] under the same conditions as [`Self::publish_snapshot`].
    pub fn publish_snapshot_with_progress<F>(
        &mut self,
        batch: SnapshotBatch<'_>,
        mut progress: F,
    ) -> Result<(), StoreError>
    where
        F: FnMut(u64),
    {
        let SnapshotBatch {
            workspace,
            snapshot_id,
            nodes,
            edges,
            evidence,
            fingerprints,
            extractor_batches,
            extractor_runs,
            manual_links,
            community_snapshot,
        } = batch;
        validate_graph_snapshot(nodes, edges, evidence)?;
        validate_artifact_fingerprints(fingerprints)?;
        validate_manual_links(snapshot_id, manual_links, Some(nodes))?;
        validate_community_snapshot(snapshot_id, nodes, community_snapshot)?;
        let transaction = self.connection.transaction()?;
        upsert_registry(&transaction, workspace)?;
        transaction.execute(
            "DELETE FROM query_cache WHERE workspace_name = ?1",
            [&workspace.name],
        )?;
        transaction.execute(
            "UPDATE repo_snapshots SET is_current = 0 WHERE workspace_name = ?1",
            [&workspace.name],
        )?;
        transaction.execute("DELETE FROM repo_snapshots WHERE id = ?1", [snapshot_id])?;
        transaction.execute(
            "INSERT INTO repo_snapshots(id, workspace_name, is_current) VALUES (?1, ?2, 0)",
            params![snapshot_id, workspace.name],
        )?;

        insert_graph(
            &transaction,
            snapshot_id,
            nodes,
            edges,
            evidence,
            &mut progress,
        )?;
        insert_manual_links(&transaction, manual_links)?;
        progress(u64::try_from(manual_links.len()).unwrap_or(u64::MAX));
        insert_incremental_state(
            &transaction,
            snapshot_id,
            fingerprints,
            extractor_runs,
            &mut progress,
        )?;
        insert_extractor_batches(&transaction, snapshot_id, extractor_batches, &mut progress)?;
        insert_freshness(&transaction, snapshot_id, workspace)?;
        if let Some(community_snapshot) = community_snapshot {
            insert_community_snapshot(&transaction, community_snapshot, &mut progress)?;
        }
        transaction.execute(
            "UPDATE repo_snapshots SET is_current = 1 WHERE id = ?1",
            [snapshot_id],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Loads nodes and edges from the current workspace snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when no current snapshot exists or stored data is invalid.
    pub fn load_current_graph(
        &self,
        workspace: &str,
    ) -> Result<(Vec<Node>, Vec<Edge>), StoreError> {
        let snapshot_id = self.current_snapshot_id(workspace)?;
        self.load_graph_snapshot(&snapshot_id)
    }

    /// Loads nodes and edges from one immutable graph snapshot.
    ///
    /// Rows are returned in stable identifier order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::SnapshotMissing`] when the snapshot does not exist, or another
    /// [`StoreError`] when stored rows cannot be decoded.
    pub fn load_graph_snapshot(
        &self,
        snapshot_id: &str,
    ) -> Result<(Vec<Node>, Vec<Edge>), StoreError> {
        self.require_snapshot(snapshot_id)?;
        let mut node_statement = self.connection.prepare(
            "SELECT id, kind, repo_id, stable_key, label
             FROM nodes WHERE snapshot_id = ?1 ORDER BY id",
        )?;
        let node_rows = node_statement
            .query_map([&snapshot_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let nodes = node_rows
            .into_iter()
            .map(|(id, kind, repo_id, stable_key, label)| {
                Ok(Node {
                    id: NodeId::new(id),
                    kind: serde_json::from_str::<NodeKind>(&kind)?,
                    repo_id: repo_id.map(RepoId::new),
                    stable_key,
                    label,
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;

        let mut edge_statement = self.connection.prepare(
            "SELECT id, source_node_id, target_node_id, kind, confidence, epistemic_status
             FROM edges WHERE snapshot_id = ?1 ORDER BY id",
        )?;
        let edge_rows = edge_statement
            .query_map([&snapshot_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, f32>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut edges = Vec::with_capacity(edge_rows.len());
        for (id, source, target, kind, confidence, status) in edge_rows {
            let mut evidence_statement = self.connection.prepare(
                "SELECT evidence_id FROM edge_evidence
                 WHERE snapshot_id = ?1 AND edge_id = ?2 ORDER BY evidence_id",
            )?;
            let evidence = evidence_statement
                .query_map(params![snapshot_id, id], |row| row.get::<_, String>(0))?
                .map(|result| result.map(code_system_graph_model::EvidenceId::new))
                .collect::<Result<Vec<_>, _>>()?;
            edges.push(Edge {
                id: EdgeId::new(id),
                source: NodeId::new(source),
                target: NodeId::new(target),
                kind: serde_json::from_str::<EdgeKind>(&kind)?,
                confidence,
                status: serde_json::from_str::<EpistemicStatus>(&status)?,
                evidence,
            });
        }
        Ok((nodes, edges))
    }

    /// Loads evidence metadata for the current workspace snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the current snapshot is absent or stored tags are malformed.
    pub fn load_current_evidence(&self, workspace: &str) -> Result<Vec<Evidence>, StoreError> {
        let snapshot_id = self.current_snapshot_id(workspace)?;
        self.load_evidence_snapshot(&snapshot_id)
    }

    /// Loads evidence metadata for one immutable graph snapshot.
    ///
    /// Line ranges, extractor versions, commits, and notes predate their normalized storage
    /// columns and therefore remain absent in this projection.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::SnapshotMissing`] when the snapshot is absent or
    /// [`StoreError::MalformedStoredData`] when a stored provenance tag is invalid.
    pub fn load_evidence_snapshot(&self, snapshot_id: &str) -> Result<Vec<Evidence>, StoreError> {
        self.require_snapshot(snapshot_id)?;
        let mut statement = self.connection.prepare(
            "SELECT id, repo_id, file_path, start_line, end_line, extractor, extractor_version,
                    provenance, confidence, observed_at_commit, content_hash
             FROM evidence WHERE snapshot_id = ?1 ORDER BY id",
        )?;
        let rows = statement
            .query_map([snapshot_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<u32>>(3)?,
                    row.get::<_, Option<u32>>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, f32>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(
                |(
                    id,
                    repo_id,
                    file_path,
                    start_line,
                    end_line,
                    extractor,
                    extractor_version,
                    provenance,
                    confidence,
                    observed_at_commit,
                    content_hash,
                )| {
                    let provenance = serde_json::from_str(&provenance).map_err(|error| {
                        malformed_stored_data(format!("evidence `{id}`"), error.to_string())
                    })?;
                    Ok(Evidence {
                        id: EvidenceId::new(id),
                        repo_id: repo_id.map(RepoId::new),
                        file_path,
                        start_line,
                        end_line,
                        extractor,
                        extractor_version,
                        provenance,
                        confidence,
                        observed_at_commit,
                        content_hash,
                        note: None,
                    })
                },
            )
            .collect()
    }

    /// Searches current snapshot node labels and stable keys using bounded FTS5.
    ///
    /// The input is always treated as a quoted phrase rather than raw FTS syntax.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] for empty input, limits outside `1..=100`, invalid stored tags, or
    /// `SQLite` failures.
    pub fn search_current_nodes(
        &self,
        workspace: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Node>, StoreError> {
        if !(1..=100).contains(&limit) {
            return Err(StoreError::InvalidSearchQuery(
                "limit must be between 1 and 100".to_owned(),
            ));
        }
        self.search_current_nodes_ranked(workspace, query, limit)
            .map(|hits| hits.into_iter().map(|hit| hit.node).collect())
    }

    /// Searches current nodes and includes the native FTS5 relevance score.
    ///
    /// The non-empty query is limited to 1,024 UTF-8 bytes, treated as a quoted phrase, and bound
    /// as a parameter. The result limit is `1..=500`. Hits are ordered by ascending `bm25` score
    /// and then stable node identifier.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::InvalidSearchQuery`] for empty input or limits outside `1..=500`.
    /// Invalid stored node tags or non-finite ranks are rejected as malformed data.
    pub fn search_current_nodes_ranked(
        &self,
        workspace: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<StoredNodeSearchHit>, StoreError> {
        let query = query.trim();
        if query.is_empty() {
            return Err(StoreError::InvalidSearchQuery(
                "query must not be empty".to_owned(),
            ));
        }
        if query.len() > NODE_SEARCH_QUERY_MAX_BYTES {
            return Err(StoreError::InvalidSearchQuery(format!(
                "query must not exceed {NODE_SEARCH_QUERY_MAX_BYTES} UTF-8 bytes"
            )));
        }
        if !(1..=500).contains(&limit) {
            return Err(StoreError::InvalidSearchQuery(
                "limit must be between 1 and 500".to_owned(),
            ));
        }
        let phrase = format!("\"{}\"", query.replace('"', "\"\""));
        let limit = i64::try_from(limit).map_err(|_| StoreError::IntegerOutOfRange {
            field: "search.limit",
            value: i128::try_from(limit).unwrap_or(i128::MAX),
        })?;
        let mut statement = self.connection.prepare(
            "SELECT n.id, n.kind, n.repo_id, n.stable_key, n.label, bm25(nodes_fts)
             FROM nodes_fts
             JOIN repo_snapshots snapshot ON snapshot.id = nodes_fts.snapshot_id
             JOIN nodes n
               ON n.snapshot_id = nodes_fts.snapshot_id AND n.id = nodes_fts.node_id
             WHERE snapshot.workspace_name = ?1
               AND snapshot.is_current = 1
               AND nodes_fts MATCH ?2
             ORDER BY bm25(nodes_fts), n.id
             LIMIT ?3",
        )?;
        let rows = statement
            .query_map(params![workspace, phrase, limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, f64>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(id, kind, repo_id, stable_key, label, fts_rank)| {
                if !fts_rank.is_finite() {
                    return Err(malformed_stored_data(
                        format!("FTS hit `{id}`"),
                        "rank is not finite",
                    ));
                }
                let kind = serde_json::from_str(&kind).map_err(|error| {
                    malformed_stored_data(format!("node `{id}`"), error.to_string())
                })?;
                Ok(StoredNodeSearchHit {
                    node: Node {
                        id: NodeId::new(id),
                        kind,
                        repo_id: repo_id.map(RepoId::new),
                        stable_key,
                        label,
                    },
                    fts_rank,
                })
            })
            .collect()
    }

    /// Loads the community analysis associated with the current workspace snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when no current graph snapshot or community analysis exists, or when
    /// persisted rows are malformed.
    pub fn load_current_community_snapshot(
        &self,
        workspace: &str,
    ) -> Result<CommunitySnapshot, StoreError> {
        let snapshot_id = self.current_snapshot_id(workspace)?;
        self.load_community_snapshot(&snapshot_id)
    }

    /// Loads one immutable community analysis in deterministic identifier order.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::SnapshotMissing`] when the graph snapshot does not exist,
    /// [`StoreError::CommunitySnapshotMissing`] when it has no analysis, or
    /// [`StoreError::MalformedStoredData`] when persisted rows violate domain invariants.
    pub fn load_community_snapshot(
        &self,
        snapshot_id: &str,
    ) -> Result<CommunitySnapshot, StoreError> {
        self.require_snapshot(snapshot_id)?;
        load_community_snapshot(&self.connection, snapshot_id)
    }

    /// Loads an immutable community analysis only when it belongs to the requested workspace.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::SnapshotMissing`] when the snapshot is absent or belongs to another
    /// workspace, preserving workspace isolation at read-only delivery boundaries.
    pub fn load_workspace_community_snapshot(
        &self,
        workspace: &str,
        snapshot_id: &str,
    ) -> Result<CommunitySnapshot, StoreError> {
        let belongs = self
            .connection
            .query_row(
                "SELECT 1 FROM repo_snapshots WHERE id = ?1 AND workspace_name = ?2",
                params![snapshot_id, workspace],
                |_| Ok(()),
            )
            .optional()?;
        belongs.ok_or_else(|| StoreError::SnapshotMissing(snapshot_id.to_owned()))?;
        load_community_snapshot(&self.connection, snapshot_id)
    }

    fn current_snapshot_id(&self, workspace: &str) -> Result<String, StoreError> {
        self.connection
            .query_row(
                "SELECT id FROM repo_snapshots
                 WHERE workspace_name = ?1 AND is_current = 1",
                [workspace],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::CurrentSnapshotMissing(workspace.to_owned()))
    }

    fn require_snapshot(&self, snapshot_id: &str) -> Result<(), StoreError> {
        let exists = self
            .connection
            .query_row(
                "SELECT 1 FROM repo_snapshots WHERE id = ?1",
                [snapshot_id],
                |_| Ok(()),
            )
            .optional()?;
        exists.ok_or_else(|| StoreError::SnapshotMissing(snapshot_id.to_owned()))
    }

    /// Runs `SQLite`'s quick integrity check.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if `SQLite` cannot perform the check.
    pub fn integrity_check(&self) -> Result<bool, StoreError> {
        let result = self
            .connection
            .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))?;
        if result != "ok" || self.schema_version()? != LATEST_SCHEMA_VERSION {
            return Ok(false);
        }
        let foreign_key_violation = self
            .connection
            .query_row(
                "SELECT 1 FROM pragma_foreign_key_check LIMIT 1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        Ok(foreign_key_violation.is_none())
    }

    /// Observes `SQLite` safety and indexing capabilities without modifying the store.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when `SQLite` cannot provide the requested diagnostics.
    pub fn diagnostics(&self) -> Result<StoreDiagnostics, StoreError> {
        let journal_mode = self
            .connection
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))?;
        let foreign_keys_enabled = self
            .connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))?
            == 1;
        let fts5_index_available = self
            .connection
            .query_row(
                "SELECT 1 FROM sqlite_schema
                 WHERE type = 'table' AND name = 'nodes_fts'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some();
        Ok(StoreDiagnostics {
            journal_mode,
            foreign_keys_enabled,
            fts5_index_available,
        })
    }
}

fn load_workspace_identity(
    connection: &Connection,
    workspace: &str,
) -> Result<StoredWorkspaceIdentity, StoreError> {
    let (id, manifest_hash, encoding, bytes, display) = connection
        .query_row(
            "SELECT
                id, manifest_hash, config_path_encoding, config_path, config_path_display
             FROM workspaces WHERE name = ?1",
            [workspace],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| StoreError::RegistryIncomplete(workspace.to_owned()))?;
    Ok(StoredWorkspaceIdentity {
        id: id.ok_or_else(|| StoreError::RegistryIncomplete(workspace.to_owned()))?,
        manifest_hash,
        config_path: decode_optional_path(encoding, bytes, display)?,
    })
}

fn load_workspace_repositories(
    connection: &Connection,
    workspace: &str,
) -> Result<Vec<RepositoryRecord>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT
            wr.alias, r.id, c.id, c.path_encoding, c.canonical_path, c.path_display,
            r.git_common_path_encoding, r.git_common_path, r.git_common_path_display,
            r.normalized_remote, c.head_commit, c.is_linked_worktree, c.working_tree_dirty
         FROM workspace_repositories wr
         JOIN repositories r ON r.id = wr.repo_id
         JOIN repository_checkouts c ON c.id = wr.checkout_id
         WHERE wr.workspace_name = ?1
         ORDER BY wr.alias",
    )?;
    let rows = statement
        .query_map([workspace], |row| {
            Ok(StoredRepositoryRow {
                alias: row.get(0)?,
                repo_id: row.get(1)?,
                checkout_id: row.get(2)?,
                path_encoding: row.get(3)?,
                path_bytes: row.get(4)?,
                path_display: row.get(5)?,
                common_encoding: row.get(6)?,
                common_bytes: row.get(7)?,
                common_display: row.get(8)?,
                normalized_remote: row.get(9)?,
                head_commit: row.get(10)?,
                is_linked_worktree: row.get::<_, i64>(11)? != 0,
                working_tree_dirty: row.get::<_, i64>(12)? != 0,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(repository_from_row).collect()
}

fn repository_from_row(row: StoredRepositoryRow) -> Result<RepositoryRecord, StoreError> {
    Ok(RepositoryRecord {
        id: RepoId::new(row.repo_id),
        checkout_id: CheckoutId::new(row.checkout_id),
        alias: row.alias,
        canonical_path: NativePath {
            encoding: serde_json::from_str(&row.path_encoding)?,
            bytes: row.path_bytes,
            display: row.path_display,
        },
        git_common_dir: decode_optional_path(
            row.common_encoding,
            row.common_bytes,
            row.common_display,
        )?,
        normalized_remote: row.normalized_remote,
        head_commit: row.head_commit,
        is_linked_worktree: row.is_linked_worktree,
        working_tree_dirty: row.working_tree_dirty,
    })
}

fn decode_optional_path(
    encoding: Option<String>,
    bytes: Option<Vec<u8>>,
    display: Option<String>,
) -> Result<Option<NativePath>, StoreError> {
    match (encoding, bytes, display) {
        (Some(encoding), Some(bytes), Some(display)) => Ok(Some(NativePath {
            encoding: serde_json::from_str(&encoding)?,
            bytes,
            display,
        })),
        _ => Ok(None),
    }
}

fn open_exact(path: &Path) -> Result<Connection, StoreError> {
    prepare_database_file(path)?;
    let mut connection = Connection::open(path)?;
    configure_connection(&connection)?;
    let current = existing_schema_version(&connection)?;
    if current == 0 && database_is_empty(&connection)? {
        initialize_empty_schema(&mut connection)?;
    }
    restrict_store_permissions(path)?;
    validate_exact_schema(&connection)?;
    Ok(connection)
}

fn configure_connection(connection: &Connection) -> Result<(), StoreError> {
    connection.pragma_update(None, "foreign_keys", true)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(())
}

fn existing_schema_version(connection: &Connection) -> Result<i64, StoreError> {
    let metadata_exists = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name = 'schema_metadata'",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if metadata_exists == 0 {
        return Ok(0);
    }
    schema_version(connection)
}

fn validate_exact_schema(connection: &Connection) -> Result<(), StoreError> {
    if existing_schema_version(connection)? != LATEST_SCHEMA_VERSION {
        return Err(StoreError::ObsoleteDevelopmentDatabase);
    }
    let expected = expected_schema_contract()?;
    if schema_contract(connection)? != expected {
        return Err(StoreError::ObsoleteDevelopmentDatabase);
    }
    Ok(())
}

type SchemaContractEntry = (String, String, String, String);

fn schema_contract(connection: &Connection) -> Result<Vec<SchemaContractEntry>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT type, name, tbl_name, COALESCE(sql, '')
         FROM sqlite_schema
         WHERE name NOT LIKE 'sqlite_%'
         ORDER BY type, name, tbl_name, sql",
    )?;
    Ok(statement
        .query_map([], |row| {
            let sql: String = row.get(3)?;
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                normalize_schema_sql(&sql),
            ))
        })?
        .collect::<Result<_, _>>()?)
}

fn normalize_schema_sql(sql: &str) -> String {
    if sql.is_empty() {
        return String::new();
    }

    let mut normalized = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut pending_space = false;

    while let Some(ch) = chars.next() {
        match ch {
            '\'' => {
                if pending_space {
                    normalized.push(' ');
                    pending_space = false;
                }
                normalized.push('\'');
                while let Some(inner) = chars.next() {
                    normalized.push(inner);
                    if inner == '\'' {
                        if chars.peek() == Some(&'\'') {
                            normalized.push(chars.next().expect("peeked doubled quote"));
                        } else {
                            break;
                        }
                    }
                }
            }
            '"' => {
                if pending_space {
                    normalized.push(' ');
                    pending_space = false;
                }
                normalized.push('"');
                while let Some(inner) = chars.next() {
                    normalized.push(inner);
                    if inner == '"' {
                        if chars.peek() == Some(&'"') {
                            normalized.push(chars.next().expect("peeked doubled quote"));
                        } else {
                            break;
                        }
                    }
                }
            }
            '-' if chars.peek() == Some(&'-') => {
                chars.next();
                for comment in chars.by_ref() {
                    if comment == '\n' {
                        pending_space = true;
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                while let Some(comment) = chars.next() {
                    if comment == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        pending_space = true;
                        break;
                    }
                }
            }
            ch if ch.is_whitespace() => pending_space = true,
            ch => {
                if pending_space {
                    normalized.push(' ');
                    pending_space = false;
                }
                normalized.push(ch);
            }
        }
    }

    normalized.trim().to_owned()
}

fn expected_schema_contract() -> Result<Vec<SchemaContractEntry>, StoreError> {
    let connection = Connection::open_in_memory()?;
    connection.execute_batch(INITIAL_SCHEMA)?;
    schema_contract(&connection)
}

fn database_is_empty(connection: &Connection) -> Result<bool, StoreError> {
    let tables = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    Ok(tables == 0)
}

fn backup_connection(source: &Connection, destination: &Path) -> Result<(), StoreError> {
    if destination.exists() {
        return Err(StoreError::Io {
            path: destination.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "backup destination already exists",
            ),
        });
    }
    prepare_database_file(destination)?;
    let mut destination_connection = Connection::open(destination)?;
    let result = backup_connection_to(source, &mut destination_connection)
        .and_then(|()| restrict_store_permissions(destination));
    if result.is_err() {
        remove_database_artifacts(destination);
    }
    result
}

fn validate_pinned_backup_source(source_path: &Path) -> Result<(), StoreError> {
    let source = open_validated_backup_source(source_path)?;
    source.execute_batch("BEGIN DEFERRED")?;
    let result = validate_backup(&source, source_path);
    match result {
        Ok(()) => source.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = source.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    Ok(())
}

fn remove_database_artifacts(database_path: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let sidecar = if suffix.is_empty() {
            database_path.to_path_buf()
        } else {
            let mut value = database_path.as_os_str().to_os_string();
            value.push(suffix);
            PathBuf::from(value)
        };
        let _ = fs::remove_file(sidecar);
    }
}

fn copy_validated_backup_source(
    source_path: &Path,
    destination: &mut Connection,
) -> Result<(), StoreError> {
    let source = open_validated_backup_source(source_path)?;
    source.execute_batch("BEGIN DEFERRED")?;
    let result = (|| {
        validate_backup(&source, source_path)?;
        backup_connection_to(&source, destination)
    })();
    match result {
        Ok(()) => source.execute_batch("COMMIT")?,
        Err(error) => {
            let _ = source.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    Ok(())
}

fn backup_connection_to(
    source: &Connection,
    destination: &mut Connection,
) -> Result<(), StoreError> {
    let backup = Backup::new(source, destination)?;
    backup.run_to_completion(128, Duration::from_millis(5), None)?;
    Ok(())
}

fn next_backup_path(
    database: &Path,
    label: &str,
    exclude: Option<&Path>,
) -> Result<PathBuf, StoreError> {
    let original_extension = database
        .extension()
        .map(|extension| extension.to_string_lossy().into_owned());
    for sequence in 0..10_000_u32 {
        let suffix = if sequence == 0 {
            format!("{label}.backup")
        } else {
            format!("{label}.{sequence}.backup")
        };
        let extension = original_extension
            .as_ref()
            .map_or_else(|| suffix.clone(), |original| format!("{original}.{suffix}"));
        let mut candidate = database.to_path_buf();
        candidate.set_extension(extension);
        if candidate.exists() {
            continue;
        }
        if exclude.is_some_and(|excluded| paths_refer_to_same_file(&candidate, excluded)) {
            continue;
        }
        return Ok(candidate);
    }
    Err(StoreError::Io {
        path: database.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "no available backup filename",
        ),
    })
}

fn paths_refer_to_same_file(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn ensure_restorable_permissions(database_path: &Path) -> Result<(), StoreError> {
    restrict_store_permissions(database_path)
}

fn rollback_restore_from_safety_backup(
    database_path: &Path,
    safety_backup_path: &Path,
) -> Result<(), StoreError> {
    let source = Connection::open(safety_backup_path)?;
    let mut destination = Connection::open(database_path)?;
    backup_connection_to(&source, &mut destination)?;
    configure_connection(&destination)?;
    restrict_store_permissions(database_path)
}

fn ensure_distinct_paths(database: &Path, backup: &Path) -> Result<(), StoreError> {
    let same_path = if database.exists() && backup.exists() {
        let database = fs::canonicalize(database).map_err(|source| StoreError::Io {
            path: database.to_path_buf(),
            source,
        })?;
        let backup = fs::canonicalize(backup).map_err(|source| StoreError::Io {
            path: backup.to_path_buf(),
            source,
        })?;
        database == backup
    } else {
        database == backup
    };
    if same_path {
        return Err(StoreError::InvalidBackup {
            path: backup.to_path_buf(),
            reason: "backup and destination resolve to the same path".to_owned(),
        });
    }
    Ok(())
}

fn open_validated_backup_source(path: &Path) -> Result<Connection, StoreError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.pragma_update(None, "query_only", true)?;
    connection.pragma_update(None, "trusted_schema", false)?;
    connection.pragma_update(None, "foreign_keys", true)?;
    Ok(connection)
}

fn prepare_database_file(path: &Path) -> Result<(), StoreError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| StoreError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    if path.exists() {
        let metadata = fs::symlink_metadata(path).map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(StoreError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "database path must be a regular file",
                ),
            });
        }
        return Ok(());
    }
    let mut options = OpenOptions::new();
    options.create_new(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    options.open(path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(())
}

fn restrict_store_permissions(database_path: &Path) -> Result<(), StoreError> {
    set_owner_only(database_path)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = database_path.as_os_str().to_os_string();
        sidecar.push(suffix);
        let sidecar_path = PathBuf::from(sidecar);
        let metadata = match fs::symlink_metadata(&sidecar_path) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(StoreError::Io {
                    path: sidecar_path,
                    source,
                });
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(StoreError::Io {
                path: sidecar_path,
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "database sidecar path must be a regular file",
                ),
            });
        }
        set_owner_only(&sidecar_path)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
#[cfg_attr(windows, allow(unsafe_code))]
fn set_owner_only(path: &Path) -> Result<(), StoreError> {
    #[cfg(windows)]
    {
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
            return Err(StoreError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "database path contains a NUL code unit",
                ),
            });
        }
        path_wide.push(0);
        let descriptor_sddl = "D:P(A;;FA;;;OW)\0".encode_utf16().collect::<Vec<_>>();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: both UTF-16 inputs are NUL-terminated, `descriptor` is a valid out-pointer, and
        // the returned LocalAlloc allocation is released exactly once below.
        let converted = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                descriptor_sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        };
        if converted == 0 {
            return Err(StoreError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::last_os_error(),
            });
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
            return Err(StoreError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(StoreError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "owner-only database permissions are unsupported on this platform",
            ),
        })
    }
}

fn validate_backup(connection: &Connection, path: &Path) -> Result<(), StoreError> {
    let integrity =
        connection.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))?;
    if integrity != "ok" {
        return Err(StoreError::InvalidBackup {
            path: path.to_path_buf(),
            reason: format!("integrity check returned `{integrity}`"),
        });
    }
    let foreign_key_violation = connection
        .query_row(
            "SELECT 1 FROM pragma_foreign_key_check LIMIT 1",
            [],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    if foreign_key_violation.is_some() {
        return Err(StoreError::InvalidBackup {
            path: path.to_path_buf(),
            reason: "foreign key check reported violations".to_owned(),
        });
    }
    let version = existing_schema_version(connection)?;
    if version == 0 {
        return Err(StoreError::InvalidBackup {
            path: path.to_path_buf(),
            reason: "schema metadata is missing".to_owned(),
        });
    }
    if version != LATEST_SCHEMA_VERSION {
        return Err(StoreError::InvalidBackup {
            path: path.to_path_buf(),
            reason: format!(
                "schema version {version} is not the exact supported version {LATEST_SCHEMA_VERSION}"
            ),
        });
    }
    validate_exact_schema(connection).map_err(|error| match error {
        StoreError::ObsoleteDevelopmentDatabase => StoreError::InvalidBackup {
            path: path.to_path_buf(),
            reason: "schema objects do not match the exact supported contract".to_owned(),
        },
        other => other,
    })
}

fn validate_artifact_fingerprints(fingerprints: &[ArtifactFingerprint]) -> Result<(), StoreError> {
    const MAX_PATH_DISPLAY_BYTES: usize = 4 * 1024;

    for fingerprint in fingerprints {
        validate_safe_metadata(
            "artifact path display",
            &fingerprint.path.display,
            1,
            MAX_PATH_DISPLAY_BYTES,
        )?;
        validate_safe_metadata("extractor", &fingerprint.extractor, 1, 256)?;
        validate_safe_metadata("artifact content hash", &fingerprint.content_hash, 1, 128)?;
    }
    Ok(())
}

fn validate_graph_snapshot(
    nodes: &[Node],
    edges: &[Edge],
    evidence: &[Evidence],
) -> Result<(), StoreError> {
    const MAX_METADATA_BYTES: usize = 64 * 1024;

    let validate_metadata = |field: &str, value: &str| {
        if value.len() > MAX_METADATA_BYTES {
            return Err(StoreError::InvalidGraphSnapshot(format!(
                "{field} exceeds {MAX_METADATA_BYTES} bytes"
            )));
        }
        if contains_unsafe_metadata_characters(value) {
            return Err(StoreError::InvalidGraphSnapshot(format!(
                "{field} contains unsafe control or bidirectional characters"
            )));
        }
        Ok(())
    };

    let mut node_ids = BTreeSet::new();
    for node in nodes {
        if !node_ids.insert(node.id.as_str()) {
            return Err(StoreError::InvalidGraphSnapshot(format!(
                "duplicate node identifier `{}`",
                node.id.as_str()
            )));
        }
        validate_metadata("node identifier", node.id.as_str())?;
        validate_metadata("node stable key", &node.stable_key)?;
        validate_metadata("node label", &node.label)?;
    }

    let mut evidence_ids = BTreeSet::new();
    for item in evidence {
        if !evidence_ids.insert(item.id.as_str()) {
            return Err(StoreError::InvalidGraphSnapshot(format!(
                "duplicate evidence identifier `{}`",
                item.id.as_str()
            )));
        }
        validate_metadata("evidence identifier", item.id.as_str())?;
        validate_metadata("evidence extractor", &item.extractor)?;
        validate_metadata("evidence extractor version", &item.extractor_version)?;
        for (field, value) in [
            ("evidence file path", item.file_path.as_deref()),
            (
                "evidence observed commit",
                item.observed_at_commit.as_deref(),
            ),
            ("evidence content hash", item.content_hash.as_deref()),
            ("evidence note", item.note.as_deref()),
        ] {
            if let Some(value) = value {
                validate_metadata(field, value)?;
            }
        }
        if !item.confidence.is_finite() || !(0.0..=1.0).contains(&item.confidence) {
            return Err(StoreError::InvalidGraphSnapshot(
                "evidence confidence must be finite and between zero and one".to_owned(),
            ));
        }
        if matches!((item.start_line, item.end_line), (Some(start), Some(end)) if start > end) {
            return Err(StoreError::InvalidGraphSnapshot(
                "evidence line range is reversed".to_owned(),
            ));
        }
    }

    let mut edge_ids = BTreeSet::new();
    for edge in edges {
        if !edge_ids.insert(edge.id.as_str()) {
            return Err(StoreError::InvalidGraphSnapshot(format!(
                "duplicate edge identifier `{}`",
                edge.id.as_str()
            )));
        }
        validate_metadata("edge identifier", edge.id.as_str())?;
        if !node_ids.contains(edge.source.as_str()) || !node_ids.contains(edge.target.as_str()) {
            return Err(StoreError::InvalidGraphSnapshot(format!(
                "edge `{}` references a missing node",
                edge.id.as_str()
            )));
        }
        if !edge.confidence.is_finite() || !(0.0..=1.0).contains(&edge.confidence) {
            return Err(StoreError::InvalidGraphSnapshot(format!(
                "edge `{}` has invalid confidence",
                edge.id.as_str()
            )));
        }
        let mut edge_evidence = BTreeSet::new();
        for evidence_id in &edge.evidence {
            if !edge_evidence.insert(evidence_id.as_str()) {
                return Err(StoreError::InvalidGraphSnapshot(format!(
                    "edge `{}` repeats evidence `{}`",
                    edge.id.as_str(),
                    evidence_id.as_str()
                )));
            }
            if !evidence_ids.contains(evidence_id.as_str()) {
                return Err(StoreError::InvalidGraphSnapshot(format!(
                    "edge `{}` references missing evidence",
                    edge.id.as_str()
                )));
            }
        }
    }

    Ok(())
}

fn validate_community_snapshot(
    snapshot_id: &str,
    nodes: &[Node],
    community_snapshot: Option<&CommunitySnapshot>,
) -> Result<(), StoreError> {
    let Some(community_snapshot) = community_snapshot else {
        return Ok(());
    };
    if community_snapshot.snapshot_id != snapshot_id {
        return Err(StoreError::CommunitySnapshotMismatch {
            batch_snapshot_id: snapshot_id.to_owned(),
            community_snapshot_id: community_snapshot.snapshot_id.clone(),
        });
    }
    let node_ids = nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect::<BTreeSet<_>>();
    let mut community_ids = BTreeSet::new();
    for community in &community_snapshot.communities {
        if !community_ids.insert(community.id.as_str()) {
            return Err(StoreError::DuplicateCommunityId {
                snapshot_id: snapshot_id.to_owned(),
                community_id: community.id.as_str().to_owned(),
            });
        }
        let mut member_ids = BTreeSet::new();
        for member in &community.members {
            if !member_ids.insert(member.as_str()) {
                return Err(StoreError::DuplicateCommunityMembership {
                    snapshot_id: snapshot_id.to_owned(),
                    community_id: community.id.as_str().to_owned(),
                    node_id: member.as_str().to_owned(),
                });
            }
            if !node_ids.contains(member.as_str()) {
                return Err(StoreError::CommunityMembershipNodeMissing {
                    snapshot_id: snapshot_id.to_owned(),
                    community_id: community.id.as_str().to_owned(),
                    node_id: member.as_str().to_owned(),
                });
            }
        }
    }
    Ok(())
}

fn insert_community_snapshot<F>(
    transaction: &rusqlite::Transaction<'_>,
    snapshot: &CommunitySnapshot,
    progress: &mut F,
) -> Result<(), StoreError>
where
    F: FnMut(u64),
{
    let algorithm = serde_json::to_string(&snapshot.config.algorithm)?;
    ensure_community_json_bound(
        "community_snapshots.algorithm",
        &algorithm,
        COMMUNITY_ALGORITHM_JSON_MAX_BYTES,
    )?;
    let config_json = serde_json::to_string(&snapshot.config)?;
    ensure_community_json_bound(
        "community_snapshots.config_json",
        &config_json,
        COMMUNITY_CONFIG_JSON_MAX_BYTES,
    )?;
    transaction.execute(
        "INSERT INTO community_snapshots(snapshot_id, engine_version, algorithm, config_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            snapshot.snapshot_id,
            snapshot.engine_version,
            algorithm,
            config_json
        ],
    )?;
    progress(1);
    for community in &snapshot.communities {
        insert_community(transaction, &snapshot.snapshot_id, community)?;
        progress(1);
    }
    Ok(())
}

fn insert_community(
    transaction: &rusqlite::Transaction<'_>,
    snapshot_id: &str,
    community: &Community,
) -> Result<(), StoreError> {
    let metrics_json = serde_json::to_string(&community.metrics)?;
    ensure_community_json_bound(
        "communities.metrics_json",
        &metrics_json,
        COMMUNITY_METRICS_JSON_MAX_BYTES,
    )?;
    let explanation_json = serde_json::to_string(&serde_json::json!({
        "central_nodes": &community.central_nodes,
        "repositories": &community.repositories,
        "services": &community.services,
        "inbound_contracts": &community.inbound_contracts,
        "outbound_contracts": &community.outbound_contracts,
        "label_evidence": &community.label_evidence,
        "limitations": &community.limitations,
    }))?;
    ensure_community_json_bound(
        "communities.explanation_json",
        &explanation_json,
        COMMUNITY_EXPLANATION_JSON_MAX_BYTES,
    )?;
    transaction.execute(
        "INSERT INTO communities(snapshot_id, id, label, metrics_json, explanation_json)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            snapshot_id,
            community.id.as_str(),
            community.label,
            metrics_json,
            explanation_json
        ],
    )?;
    for (member_order, member) in community.members.iter().enumerate() {
        let member_order =
            i64::try_from(member_order).map_err(|_| StoreError::IntegerOutOfRange {
                field: "community_memberships.member_order",
                value: i128::try_from(member_order).unwrap_or(i128::MAX),
            })?;
        transaction.execute(
            "INSERT INTO community_memberships(
                snapshot_id, community_id, node_id, member_order
             ) VALUES (?1, ?2, ?3, ?4)",
            params![
                snapshot_id,
                community.id.as_str(),
                member.as_str(),
                member_order
            ],
        )?;
    }
    Ok(())
}

fn ensure_community_json_bound(
    field: &'static str,
    json: &str,
    max_bytes: usize,
) -> Result<(), StoreError> {
    let actual_bytes = json.len();
    if actual_bytes > max_bytes {
        return Err(StoreError::CommunityJsonTooLarge {
            field,
            actual_bytes,
            max_bytes,
        });
    }
    Ok(())
}

fn load_community_snapshot(
    connection: &Connection,
    snapshot_id: &str,
) -> Result<CommunitySnapshot, StoreError> {
    let header = connection
        .query_row(
            "SELECT engine_version, algorithm, config_json
             FROM community_snapshots WHERE snapshot_id = ?1",
            [snapshot_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| StoreError::CommunitySnapshotMissing(snapshot_id.to_owned()))?;
    let (engine_version, algorithm_json, config_json) = header;
    ensure_community_json_bound(
        "community_snapshots.algorithm",
        &algorithm_json,
        COMMUNITY_ALGORITHM_JSON_MAX_BYTES,
    )?;
    ensure_community_json_bound(
        "community_snapshots.config_json",
        &config_json,
        COMMUNITY_CONFIG_JSON_MAX_BYTES,
    )?;
    let algorithm =
        serde_json::from_str::<CommunityAlgorithm>(&algorithm_json).map_err(|error| {
            malformed_stored_data(
                format!("community snapshot `{snapshot_id}` algorithm"),
                error.to_string(),
            )
        })?;
    let config = serde_json::from_str::<CommunityConfig>(&config_json).map_err(|error| {
        malformed_stored_data(
            format!("community snapshot `{snapshot_id}` config"),
            error.to_string(),
        )
    })?;
    if config.algorithm != algorithm {
        return Err(malformed_stored_data(
            format!("community snapshot `{snapshot_id}`"),
            "algorithm does not match config",
        ));
    }

    let mut statement = connection.prepare(
        "SELECT id, label, metrics_json, explanation_json
         FROM communities WHERE snapshot_id = ?1 ORDER BY id",
    )?;
    let rows = statement
        .query_map([snapshot_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut communities = Vec::with_capacity(rows.len());
    for (id, label, metrics_json, explanation_json) in rows {
        communities.push(load_community(
            connection,
            snapshot_id,
            id,
            label,
            &metrics_json,
            &explanation_json,
        )?);
    }
    Ok(CommunitySnapshot {
        snapshot_id: snapshot_id.to_owned(),
        engine_version,
        config,
        communities,
    })
}

fn load_community(
    connection: &Connection,
    snapshot_id: &str,
    id: String,
    label: String,
    metrics_json: &str,
    explanation_json: &str,
) -> Result<Community, StoreError> {
    ensure_community_json_bound(
        "communities.metrics_json",
        metrics_json,
        COMMUNITY_METRICS_JSON_MAX_BYTES,
    )?;
    ensure_community_json_bound(
        "communities.explanation_json",
        explanation_json,
        COMMUNITY_EXPLANATION_JSON_MAX_BYTES,
    )?;
    let entity = format!("community `{id}` in snapshot `{snapshot_id}`");
    let metrics = serde_json::from_str::<CommunityMetrics>(metrics_json)
        .map_err(|error| malformed_stored_data(&entity, error.to_string()))?;
    let mut details = serde_json::from_str::<serde_json::Value>(explanation_json)
        .map_err(|error| malformed_stored_data(&entity, error.to_string()))?;
    let details = details
        .as_object_mut()
        .ok_or_else(|| malformed_stored_data(&entity, "explanation JSON must be an object"))?;
    let central_nodes =
        serde_json::from_value(remove_json_field(details, "central_nodes", &entity)?)
            .map_err(|error| malformed_stored_data(&entity, error.to_string()))?;
    let repositories = serde_json::from_value(remove_json_field(details, "repositories", &entity)?)
        .map_err(|error| malformed_stored_data(&entity, error.to_string()))?;
    let services = serde_json::from_value(remove_json_field(details, "services", &entity)?)
        .map_err(|error| malformed_stored_data(&entity, error.to_string()))?;
    let inbound_contracts =
        serde_json::from_value(remove_json_field(details, "inbound_contracts", &entity)?)
            .map_err(|error| malformed_stored_data(&entity, error.to_string()))?;
    let outbound_contracts =
        serde_json::from_value(remove_json_field(details, "outbound_contracts", &entity)?)
            .map_err(|error| malformed_stored_data(&entity, error.to_string()))?;
    let label_evidence =
        serde_json::from_value(remove_json_field(details, "label_evidence", &entity)?)
            .map_err(|error| malformed_stored_data(&entity, error.to_string()))?;
    let limitations = serde_json::from_value(remove_json_field(details, "limitations", &entity)?)
        .map_err(|error| malformed_stored_data(&entity, error.to_string()))?;
    if !details.is_empty() {
        return Err(malformed_stored_data(
            &entity,
            "explanation JSON contains unknown fields",
        ));
    }

    let members = load_community_members(connection, snapshot_id, &id, &entity)?;
    if metrics.size != members.len() {
        return Err(malformed_stored_data(
            &entity,
            "metrics size does not match normalized membership count",
        ));
    }
    Ok(Community {
        id: CommunityId::new(id),
        label,
        members,
        central_nodes,
        repositories,
        services,
        inbound_contracts,
        outbound_contracts,
        metrics,
        label_evidence,
        limitations,
    })
}

fn load_community_members(
    connection: &Connection,
    snapshot_id: &str,
    community_id: &str,
    entity: &str,
) -> Result<Vec<NodeId>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT node_id, member_order
         FROM community_memberships
         WHERE snapshot_id = ?1 AND community_id = ?2
         ORDER BY member_order, node_id",
    )?;
    let rows = statement
        .query_map(params![snapshot_id, community_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut members = Vec::with_capacity(rows.len());
    for (expected_order, (node_id, stored_order)) in rows.into_iter().enumerate() {
        let expected_order =
            i64::try_from(expected_order).map_err(|_| StoreError::IntegerOutOfRange {
                field: "community_memberships.member_order",
                value: i128::try_from(expected_order).unwrap_or(i128::MAX),
            })?;
        if stored_order != expected_order {
            return Err(malformed_stored_data(
                entity,
                "membership order is not contiguous",
            ));
        }
        let node_exists = connection
            .query_row(
                "SELECT 1 FROM nodes WHERE snapshot_id = ?1 AND id = ?2",
                params![snapshot_id, node_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !node_exists {
            return Err(malformed_stored_data(
                entity,
                format!("membership references missing node `{node_id}`"),
            ));
        }
        members.push(NodeId::new(node_id));
    }
    Ok(members)
}

fn remove_json_field(
    object: &mut serde_json::Map<String, serde_json::Value>,
    field: &str,
    entity: &str,
) -> Result<serde_json::Value, StoreError> {
    object
        .remove(field)
        .ok_or_else(|| malformed_stored_data(entity, format!("missing JSON field `{field}`")))
}

fn malformed_stored_data(entity: impl Into<String>, reason: impl Into<String>) -> StoreError {
    StoreError::MalformedStoredData {
        entity: entity.into(),
        reason: reason.into(),
    }
}

fn validate_safe_metadata(
    field: &str,
    value: &str,
    min_bytes: usize,
    max_bytes: usize,
) -> Result<(), StoreError> {
    if !(min_bytes..=max_bytes).contains(&value.len()) || value.trim().is_empty() {
        return Err(StoreError::InvalidPersistenceRecord(format!(
            "{field} must contain between {min_bytes} and {max_bytes} non-blank UTF-8 bytes"
        )));
    }
    if contains_unsafe_metadata_characters(value) {
        return Err(StoreError::InvalidPersistenceRecord(format!(
            "{field} contains unsafe control or bidirectional characters"
        )));
    }
    Ok(())
}

fn validate_manual_links(
    snapshot_id: &str,
    records: &[ManualLinkRecord],
    nodes: Option<&[Node]>,
) -> Result<(), StoreError> {
    validate_safe_metadata(
        "snapshot identifier",
        snapshot_id,
        1,
        MANUAL_LINK_ID_MAX_BYTES,
    )?;
    let node_ids = nodes.map(|nodes| {
        nodes
            .iter()
            .map(|node| node.id.as_str())
            .collect::<BTreeSet<_>>()
    });
    let mut record_ids = BTreeSet::new();
    let mut declarations = BTreeSet::new();
    for record in records {
        if record.snapshot_id != snapshot_id {
            return Err(StoreError::InvalidPersistenceRecord(format!(
                "manual link `{}` belongs to snapshot `{}`, not `{snapshot_id}`",
                record.id, record.snapshot_id
            )));
        }
        validate_safe_metadata(
            "manual link identifier",
            &record.id,
            1,
            MANUAL_LINK_ID_MAX_BYTES,
        )?;
        validate_safe_metadata(
            "manual link source node",
            record.source_node_id.as_str(),
            1,
            MANUAL_LINK_ID_MAX_BYTES,
        )?;
        validate_safe_metadata(
            "manual link target node",
            record.target_node_id.as_str(),
            1,
            MANUAL_LINK_ID_MAX_BYTES,
        )?;
        validate_safe_metadata(
            "manual link kind",
            &record.kind,
            1,
            MANUAL_LINK_KIND_MAX_BYTES,
        )?;
        validate_safe_metadata(
            "manual link reason",
            &record.reason,
            1,
            MANUAL_LINK_REASON_MAX_BYTES,
        )?;
        validate_manual_link_decision(record)?;
        if !(1..=2_147_483_647).contains(&record.config_version) {
            return Err(StoreError::InvalidPersistenceRecord(format!(
                "manual link `{}` config version is outside 1..=2147483647",
                record.id
            )));
        }
        if !record_ids.insert(record.id.as_str()) {
            return Err(StoreError::InvalidPersistenceRecord(format!(
                "manual link identifier `{}` is duplicated in snapshot `{snapshot_id}`",
                record.id
            )));
        }
        if !declarations.insert((
            record.source_node_id.as_str(),
            record.target_node_id.as_str(),
            record.kind.as_str(),
        )) {
            return Err(StoreError::InvalidPersistenceRecord(format!(
                "manual link endpoints and kind are duplicated in snapshot `{snapshot_id}`"
            )));
        }
        if node_ids.as_ref().is_some_and(|node_ids| {
            !node_ids.contains(record.source_node_id.as_str())
                || !node_ids.contains(record.target_node_id.as_str())
        }) {
            return Err(StoreError::InvalidPersistenceRecord(format!(
                "manual link `{}` references a node absent from snapshot `{snapshot_id}`",
                record.id
            )));
        }
    }
    Ok(())
}

fn validate_manual_link_decision(record: &ManualLinkRecord) -> Result<(), StoreError> {
    let decision_json = serde_json::to_vec(&record.decision)?;
    if decision_json.len() > MANUAL_LINK_DECISION_JSON_MAX_BYTES {
        return Err(StoreError::InvalidPersistenceRecord(format!(
            "manual link `{}` decision exceeds {MANUAL_LINK_DECISION_JSON_MAX_BYTES} bytes",
            record.id
        )));
    }
    let relation =
        serde_json::from_value::<EdgeKind>(serde_json::Value::String(record.kind.clone()))
            .map_err(|error| {
                StoreError::InvalidPersistenceRecord(format!(
                    "manual link `{}` kind is invalid: {error}",
                    record.id
                ))
            })?;
    let expected_status = match record.disposition {
        ManualLinkDisposition::Active => LinkStatus::Confirmed,
        ManualLinkDisposition::Suppression => LinkStatus::Suppressed,
    };
    if record.decision.source != record.source_node_id
        || record.decision.target != record.target_node_id
        || record.decision.relation != relation
        || record.decision.status != expected_status
        || record.decision.reasons.first() != Some(&record.reason)
    {
        return Err(StoreError::InvalidPersistenceRecord(format!(
            "manual link `{}` decision does not match its persisted declaration",
            record.id
        )));
    }
    Ok(())
}

fn validate_provider_capability_record(
    record: &ProviderCapabilityRecord,
) -> Result<(), StoreError> {
    validate_safe_metadata(
        "workspace name",
        &record.workspace_name,
        1,
        MANUAL_LINK_ID_MAX_BYTES,
    )?;
    validate_safe_metadata(
        "repository identifier",
        record.repo_id.as_str(),
        1,
        MANUAL_LINK_ID_MAX_BYTES,
    )?;
    validate_safe_metadata(
        "provider name",
        &record.provider,
        1,
        PROVIDER_COMPONENT_MAX_BYTES,
    )?;
    validate_safe_metadata(
        "provider version",
        &record.provider_version,
        1,
        PROVIDER_COMPONENT_MAX_BYTES,
    )?;
    if record.capabilities.len() > PROVIDER_CAPABILITY_COUNT_MAX {
        return Err(StoreError::InvalidPersistenceRecord(format!(
            "provider capability count exceeds {PROVIDER_CAPABILITY_COUNT_MAX}"
        )));
    }
    let mut capability_names = BTreeSet::new();
    for capability in &record.capabilities {
        validate_safe_metadata(
            "provider capability name",
            capability,
            1,
            PROVIDER_COMPONENT_MAX_BYTES,
        )?;
        if !capability_names.insert(capability.as_str()) {
            return Err(StoreError::InvalidPersistenceRecord(format!(
                "provider capability `{capability}` is duplicated"
            )));
        }
    }
    let capabilities_json = serde_json::to_string(&record.capabilities)?;
    if capabilities_json.len() > PROVIDER_CAPABILITIES_JSON_MAX_BYTES {
        return Err(StoreError::InvalidPersistenceRecord(format!(
            "provider capability JSON exceeds {PROVIDER_CAPABILITIES_JSON_MAX_BYTES} bytes"
        )));
    }
    metric_to_i64(
        "provider_capabilities.observed_at_unix_ms",
        record.observed_at_unix_ms,
    )?;
    Ok(())
}

fn validate_query_cache_record(record: &QueryCacheRecord) -> Result<(), StoreError> {
    validate_safe_metadata(
        "workspace name",
        &record.workspace_name,
        1,
        MANUAL_LINK_ID_MAX_BYTES,
    )?;
    validate_safe_metadata(
        "snapshot identifier",
        &record.snapshot_id,
        1,
        MANUAL_LINK_ID_MAX_BYTES,
    )?;
    validate_safe_metadata(
        "query cache input fingerprint",
        &record.input_fingerprint,
        1,
        QUERY_CACHE_FINGERPRINT_MAX_BYTES,
    )?;
    if record.result_summary_json.is_empty()
        || record.result_summary_json.len() > QUERY_CACHE_RESULT_MAX_BYTES
        || serde_json::from_slice::<serde_json::Value>(&record.result_summary_json).is_err()
    {
        return Err(StoreError::InvalidPersistenceRecord(format!(
            "query cache result must be valid JSON between 1 and {QUERY_CACHE_RESULT_MAX_BYTES} bytes"
        )));
    }
    if record
        .expires_at_unix_ms
        .is_some_and(|expires| expires < record.stored_at_unix_ms)
    {
        return Err(StoreError::InvalidPersistenceRecord(
            "query cache expiry precedes storage timestamp".to_owned(),
        ));
    }
    Ok(())
}

fn insert_manual_links(
    transaction: &rusqlite::Transaction<'_>,
    records: &[ManualLinkRecord],
) -> Result<(), StoreError> {
    for record in records {
        transaction.execute(
            "INSERT INTO manual_links(
                snapshot_id, id, source_node_id, target_node_id, kind, disposition, reason,
                decision_json, config_version
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.snapshot_id,
                record.id,
                record.source_node_id.as_str(),
                record.target_node_id.as_str(),
                record.kind,
                record.disposition.as_str(),
                record.reason,
                serde_json::to_vec(&record.decision)?,
                i64::from(record.config_version),
            ],
        )?;
    }
    Ok(())
}

fn insert_graph<F>(
    transaction: &rusqlite::Transaction<'_>,
    snapshot_id: &str,
    nodes: &[Node],
    edges: &[Edge],
    evidence: &[Evidence],
    progress: &mut F,
) -> Result<(), StoreError>
where
    F: FnMut(u64),
{
    for node in nodes {
        transaction.execute(
            "INSERT INTO nodes(
                snapshot_id, id, kind, repo_id, stable_key, label
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                snapshot_id,
                node.id.as_str(),
                serde_json::to_string(&node.kind)?,
                node.repo_id.as_ref().map(RepoId::as_str),
                node.stable_key,
                node.label,
            ],
        )?;
        progress(1);
        transaction.execute(
            "INSERT INTO nodes_fts(snapshot_id, node_id, label, stable_key)
             VALUES (?1, ?2, ?3, ?4)",
            params![snapshot_id, node.id.as_str(), node.label, node.stable_key],
        )?;
        progress(1);
    }
    for item in evidence {
        transaction.execute(
            "INSERT INTO evidence(
                snapshot_id, id, repo_id, file_path, start_line, end_line, extractor,
                extractor_version, provenance, confidence, observed_at_commit, content_hash
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                snapshot_id,
                item.id.as_str(),
                item.repo_id.as_ref().map(RepoId::as_str),
                item.file_path,
                item.start_line,
                item.end_line,
                item.extractor,
                item.extractor_version,
                serde_json::to_string(&item.provenance)?,
                item.confidence,
                item.observed_at_commit,
                item.content_hash,
            ],
        )?;
    }
    for edge in edges {
        transaction.execute(
            "INSERT INTO edges(
                snapshot_id, id, source_node_id, target_node_id, kind, confidence,
                epistemic_status
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                snapshot_id,
                edge.id.as_str(),
                edge.source.as_str(),
                edge.target.as_str(),
                serde_json::to_string(&edge.kind)?,
                edge.confidence,
                serde_json::to_string(&edge.status)?,
            ],
        )?;
        progress(1);
        for evidence_id in &edge.evidence {
            transaction.execute(
                "INSERT INTO edge_evidence(snapshot_id, edge_id, evidence_id)
                 VALUES (?1, ?2, ?3)",
                params![snapshot_id, edge.id.as_str(), evidence_id.as_str()],
            )?;
            progress(1);
        }
    }
    Ok(())
}

fn insert_extractor_batches<F>(
    transaction: &rusqlite::Transaction<'_>,
    snapshot_id: &str,
    batches: &[StoredExtractorBatch],
    progress: &mut F,
) -> Result<(), StoreError>
where
    F: FnMut(u64),
{
    for batch in batches {
        transaction.execute(
            "INSERT INTO extractor_batches(
                snapshot_id, repo_id, checkout_id, path_encoding, relative_path, path_display,
                extractor, content_hash, size_bytes, extractor_version, budget_fingerprint,
                source_was_lossy, output_count, payload
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                snapshot_id,
                batch.source.repo_id.as_str(),
                batch.source.checkout_id.as_str(),
                serde_json::to_string(&batch.source.path.encoding)?,
                batch.source.path.bytes,
                batch.source.path.display,
                batch.source.extractor,
                batch.source.content_hash,
                metric_to_i64("extractor_batches.size_bytes", batch.source.size_bytes)?,
                batch.extractor_version,
                batch.budget_fingerprint,
                batch.source_was_lossy,
                metric_to_i64("extractor_batches.output_count", batch.output_count)?,
                batch.payload,
            ],
        )?;
        progress(1);
    }
    Ok(())
}

fn insert_incremental_state<F>(
    transaction: &rusqlite::Transaction<'_>,
    snapshot_id: &str,
    fingerprints: &[ArtifactFingerprint],
    extractor_runs: &[ExtractorRun],
    progress: &mut F,
) -> Result<(), StoreError>
where
    F: FnMut(u64),
{
    for fingerprint in fingerprints {
        let size_bytes = metric_to_i64("artifact_fingerprints.size_bytes", fingerprint.size_bytes)?;
        transaction.execute(
            "INSERT INTO artifact_fingerprints(
                snapshot_id, repo_id, checkout_id, path_encoding, relative_path,
                path_display, extractor, content_hash, size_bytes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                snapshot_id,
                fingerprint.repo_id.as_str(),
                fingerprint.checkout_id.as_str(),
                serde_json::to_string(&fingerprint.path.encoding)?,
                fingerprint.path.bytes,
                fingerprint.path.display,
                fingerprint.extractor,
                fingerprint.content_hash,
                size_bytes,
            ],
        )?;
        progress(1);
    }
    for run in extractor_runs {
        insert_extractor_run(transaction, snapshot_id, run, fingerprints)?;
        progress(1);
    }
    Ok(())
}

fn insert_extractor_run(
    transaction: &rusqlite::Transaction<'_>,
    snapshot_id: &str,
    run: &ExtractorRun,
    fingerprints: &[ArtifactFingerprint],
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO extractor_runs(
            id, snapshot_id, repo_id, extractor, extractor_version, status,
            discovered_files, parsed_files, skipped_files, elapsed_ms, checkout_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            run.id,
            snapshot_id,
            run.repo_id.as_str(),
            run.extractor,
            run.extractor_version,
            serde_json::to_string(&run.status)?,
            metric_to_i64("extractor_runs.discovered_files", run.discovered_files)?,
            metric_to_i64("extractor_runs.parsed_files", run.parsed_files)?,
            metric_to_i64("extractor_runs.skipped_files", run.skipped_files)?,
            metric_to_i64("extractor_runs.elapsed_ms", run.elapsed_ms)?,
            run.checkout_id.as_str(),
        ],
    )?;
    for fingerprint in fingerprints.iter().filter(|fingerprint| {
        fingerprint.repo_id == run.repo_id
            && fingerprint.checkout_id == run.checkout_id
            && fingerprint.extractor == run.extractor
    }) {
        transaction.execute(
            "INSERT INTO extractor_run_inputs(
                run_id, repo_id, checkout_id, path_encoding, relative_path,
                extractor, content_hash
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run.id,
                fingerprint.repo_id.as_str(),
                fingerprint.checkout_id.as_str(),
                serde_json::to_string(&fingerprint.path.encoding)?,
                fingerprint.path.bytes,
                fingerprint.extractor,
                fingerprint.content_hash,
            ],
        )?;
    }
    Ok(())
}

fn insert_freshness(
    transaction: &rusqlite::Transaction<'_>,
    snapshot_id: &str,
    workspace: &WorkspaceRecord,
) -> Result<(), StoreError> {
    for repository in &workspace.repositories {
        let freshness_state = if repository.working_tree_dirty {
            RepoFreshnessState::WorkingTreeChanged
        } else {
            RepoFreshnessState::Fresh
        };
        transaction.execute(
            "INSERT INTO repository_snapshot_freshness(
                snapshot_id, repo_id, checkout_id, head_commit, manifest_hash, state, reason
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL)",
            params![
                snapshot_id,
                repository.id.as_str(),
                repository.checkout_id.as_str(),
                repository.head_commit,
                workspace.manifest_hash,
                serde_json::to_string(&freshness_state)?,
            ],
        )?;
    }
    transaction.execute(
        "DELETE FROM provider_capabilities
         WHERE workspace_name = ?1
           AND NOT EXISTS (
                SELECT 1 FROM workspace_repositories
                WHERE workspace_name = ?1
                  AND repo_id = provider_capabilities.repo_id
           )",
        [&workspace.name],
    )?;
    Ok(())
}

fn metric_to_i64(field: &'static str, value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::IntegerOutOfRange {
        field,
        value: i128::from(value),
    })
}

fn stored_metric_to_u64(field: &'static str, value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::IntegerOutOfRange {
        field,
        value: i128::from(value),
    })
}

fn count_to_usize(field: &'static str, value: i64) -> Result<usize, StoreError> {
    usize::try_from(value).map_err(|_| StoreError::IntegerOutOfRange {
        field,
        value: i128::from(value),
    })
}

fn initialize_empty_schema(connection: &mut Connection) -> Result<(), StoreError> {
    if !database_is_empty(connection)? {
        return Err(StoreError::ObsoleteDevelopmentDatabase);
    }
    let transaction = connection.transaction()?;
    transaction.execute_batch(INITIAL_SCHEMA)?;
    transaction.execute(
        "INSERT INTO schema_metadata(version, instance_id)
         VALUES (?1, lower(hex(randomblob(32))))",
        [LATEST_SCHEMA_VERSION],
    )?;
    transaction.commit()?;
    Ok(())
}

fn schema_version(connection: &Connection) -> Result<i64, StoreError> {
    Ok(connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_metadata",
        [],
        |row| row.get(0),
    )?)
}

fn database_instance_id(connection: &Connection) -> Result<String, StoreError> {
    Ok(connection.query_row(
        "SELECT instance_id FROM schema_metadata WHERE version = ?1",
        [LATEST_SCHEMA_VERSION],
        |row| row.get(0),
    )?)
}

fn upsert_registry(
    transaction: &rusqlite::Transaction<'_>,
    workspace: &WorkspaceRecord,
) -> Result<(), StoreError> {
    let config_path_encoding = workspace
        .config_path
        .as_ref()
        .map(|path| serde_json::to_string(&path.encoding))
        .transpose()?;
    transaction.execute(
        "INSERT INTO workspaces(
            name, manifest_hash, id, config_path_encoding, config_path, config_path_display
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(name) DO UPDATE SET
             manifest_hash = excluded.manifest_hash,
             id = excluded.id,
             config_path_encoding = excluded.config_path_encoding,
             config_path = excluded.config_path,
             config_path_display = excluded.config_path_display",
        params![
            workspace.name,
            workspace.manifest_hash,
            workspace.id.as_str(),
            config_path_encoding,
            workspace
                .config_path
                .as_ref()
                .map(|path| path.bytes.as_slice()),
            workspace
                .config_path
                .as_ref()
                .map(|path| path.display.as_str()),
        ],
    )?;
    transaction.execute(
        "DELETE FROM workspace_repositories WHERE workspace_name = ?1",
        [&workspace.name],
    )?;
    for repository in &workspace.repositories {
        let common_encoding = repository
            .git_common_dir
            .as_ref()
            .map(|path| serde_json::to_string(&path.encoding))
            .transpose()?;
        transaction.execute(
            "INSERT INTO repositories(
                id, normalized_remote, git_common_path_encoding, git_common_path,
                git_common_path_display
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                normalized_remote = excluded.normalized_remote,
                git_common_path_encoding = excluded.git_common_path_encoding,
                git_common_path = excluded.git_common_path,
                git_common_path_display = excluded.git_common_path_display",
            params![
                repository.id.as_str(),
                repository.normalized_remote,
                common_encoding,
                repository
                    .git_common_dir
                    .as_ref()
                    .map(|path| path.bytes.as_slice()),
                repository
                    .git_common_dir
                    .as_ref()
                    .map(|path| path.display.as_str()),
            ],
        )?;
        transaction.execute(
            "INSERT INTO repository_checkouts(
                id, repo_id, path_encoding, canonical_path, path_display, head_commit,
                is_linked_worktree, working_tree_dirty
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
                repo_id = excluded.repo_id,
                path_encoding = excluded.path_encoding,
                canonical_path = excluded.canonical_path,
                path_display = excluded.path_display,
                head_commit = excluded.head_commit,
                is_linked_worktree = excluded.is_linked_worktree,
                working_tree_dirty = excluded.working_tree_dirty",
            params![
                repository.checkout_id.as_str(),
                repository.id.as_str(),
                serde_json::to_string(&repository.canonical_path.encoding)?,
                repository.canonical_path.bytes,
                repository.canonical_path.display,
                repository.head_commit,
                i64::from(repository.is_linked_worktree),
                i64::from(repository.working_tree_dirty),
            ],
        )?;
        transaction.execute(
            "INSERT INTO workspace_repositories(workspace_name, alias, repo_id, checkout_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                workspace.name,
                repository.alias,
                repository.id.as_str(),
                repository.checkout_id.as_str(),
            ],
        )?;
    }
    Ok(())
}

fn lock_path(database_path: &Path) -> PathBuf {
    let mut path = database_path.to_path_buf();
    let extension = database_path.extension().map_or_else(
        || "lock".to_owned(),
        |value| format!("{}.lock", value.to_string_lossy()),
    );
    path.set_extension(extension);
    path
}

#[cfg(unix)]
fn create_lock_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_lock_file(path: &Path) -> std::io::Result<std::fs::File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

impl LockMetadata {
    fn encode(&self) -> String {
        format!(
            "version=1\npid={}\nprocess_started_at={}\ntoken={}\n",
            self.pid, self.process_started_at, self.token
        )
    }

    fn decode(content: &str) -> Option<Self> {
        let mut version = None;
        let mut pid = None;
        let mut process_started_at = None;
        let mut token = None;
        for line in content.lines() {
            let (key, value) = line.split_once('=')?;
            match key {
                "version" => version = Some(value),
                "pid" => pid = value.parse().ok(),
                "process_started_at" => process_started_at = value.parse().ok(),
                "token" => token = Some(value.to_owned()),
                _ => return None,
            }
        }
        (version == Some("1") && token.as_ref().is_some_and(|value| !value.is_empty())).then_some(
            Self {
                pid: pid?,
                process_started_at: process_started_at?,
                token: token?,
            },
        )
    }
}

fn process_start_time(pid: u32) -> Option<u64> {
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).map(sysinfo::Process::start_time)
}

fn lock_owner_is_alive(metadata: &LockMetadata) -> bool {
    process_start_time(metadata.pid).is_some_and(|started_at| {
        metadata.process_started_at == 0 || metadata.process_started_at == started_at
    })
}

fn lock_is_stale(path: &Path, stale_after: Duration) -> Result<bool, StoreError> {
    let modified = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    let old_enough = SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age >= stale_after);
    if !old_enough {
        return Ok(false);
    }
    let content = fs::read_to_string(path).map_err(|source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(LockMetadata::decode(&content).is_none_or(|metadata| !lock_owner_is_alive(&metadata)))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::Duration;

    use code_system_graph_model::{
        ArtifactFingerprint, CheckoutId, Community, CommunityAlgorithm, CommunityConfig, CommunityId, CommunityMetrics, CommunityScope, CommunitySnapshot, Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, EvidenceId, ExtractorRun, ExtractorRunStatus, NativePath, NativePathEncoding, Node, NodeId, NodeKind, Provenance, RepoFreshnessState, RepoId, RepositoryRecord, StoredExtractorBatch, WorkspaceId, WorkspaceRecord
    };
    use rusqlite::{Connection, params};

    use super::{
        INITIAL_SCHEMA, ManualLinkDisposition, ManualLinkRecord, ProviderCapabilityRecord, QueryCacheRecord, SnapshotBatch, SqliteStore, StoreError, StoreLock, lock_path
    };

    const LOCK_HELPER_ENV: &str = "CODE_SYSTEM_GRAPH_LOCK_HELPER";
    const LOCK_DATABASE_ENV: &str = "CODE_SYSTEM_GRAPH_LOCK_DATABASE";
    const LOCK_READY_ENV: &str = "CODE_SYSTEM_GRAPH_LOCK_READY";
    const UMASK_HELPER_ENV: &str = "CODE_SYSTEM_GRAPH_UMASK_PERMISSION_HELPER";
    const UMASK_READY_ENV: &str = "CODE_SYSTEM_GRAPH_UMASK_PERMISSION_READY";
    const UMASK_DATABASE_ENV: &str = "CODE_SYSTEM_GRAPH_UMASK_PERMISSION_DATABASE";

    fn fixture() -> (Vec<Node>, Vec<Edge>, Vec<Evidence>) {
        let evidence = Evidence {
            id: EvidenceId::new("evidence:1"),
            repo_id: Some(RepoId::new("repo:web")),
            file_path: Some("src/client.ts".to_owned()),
            start_line: Some(7),
            end_line: Some(9),
            extractor: "test".to_owned(),
            extractor_version: "1.0.0".to_owned(),
            provenance: Provenance::Extracted,
            confidence: 1.0,
            observed_at_commit: Some("0123456789abcdef".to_owned()),
            content_hash: Some("hash".to_owned()),
            note: None,
        };
        let nodes = vec![
            Node {
                id: NodeId::new("node:web"),
                kind: NodeKind::HttpOperation,
                repo_id: Some(RepoId::new("repo:web")),
                stable_key: "http:web".to_owned(),
                label: "POST /orders".to_owned(),
            },
            Node {
                id: NodeId::new("node:api"),
                kind: NodeKind::HttpOperation,
                repo_id: Some(RepoId::new("repo:api")),
                stable_key: "http:api".to_owned(),
                label: "POST /orders".to_owned(),
            },
        ];
        let edges = vec![Edge {
            id: EdgeId::new("edge:1"),
            source: NodeId::new("node:web"),
            target: NodeId::new("node:api"),
            kind: EdgeKind::CallsRemote,
            confidence: 1.0,
            status: EpistemicStatus::Confirmed,
            evidence: vec![evidence.id.clone()],
        }];
        (nodes, edges, vec![evidence])
    }

    fn workspace() -> WorkspaceRecord {
        WorkspaceRecord {
            id: WorkspaceId::new("workspace:commerce"),
            name: "commerce".to_owned(),
            manifest_hash: "manifest-hash".to_owned(),
            config_path: Some(NativePath {
                encoding: NativePathEncoding::Utf8,
                bytes: b"/workspace/code-system-graph.yaml".to_vec(),
                display: "/workspace/code-system-graph.yaml".to_owned(),
            }),
            repositories: vec![RepositoryRecord {
                id: RepoId::new("repo:web"),
                checkout_id: CheckoutId::new("checkout:web"),
                alias: "web".to_owned(),
                canonical_path: NativePath {
                    encoding: NativePathEncoding::Utf8,
                    bytes: b"/workspace/web".to_vec(),
                    display: "/workspace/web".to_owned(),
                },
                git_common_dir: None,
                normalized_remote: None,
                head_commit: Some("abc123".to_owned()),
                is_linked_worktree: false,
                working_tree_dirty: false,
            }],
        }
    }

    fn incremental_fixture() -> (ArtifactFingerprint, ExtractorRun) {
        let fingerprint = ArtifactFingerprint {
            repo_id: RepoId::new("repo:web"),
            checkout_id: CheckoutId::new("checkout:web"),
            path: NativePath {
                encoding: NativePathEncoding::Utf8,
                bytes: b"openapi.yaml".to_vec(),
                display: "openapi.yaml".to_owned(),
            },
            extractor: "code-system-graph.http.openapi".to_owned(),
            content_hash: "content:1".to_owned(),
            size_bytes: 42,
        };
        let run = ExtractorRun {
            id: "run:1".to_owned(),
            snapshot_id: "snapshot:1".to_owned(),
            repo_id: fingerprint.repo_id.clone(),
            checkout_id: fingerprint.checkout_id.clone(),
            extractor: fingerprint.extractor.clone(),
            extractor_version: "0.1.0".to_owned(),
            status: ExtractorRunStatus::Success,
            discovered_files: 1,
            parsed_files: 1,
            skipped_files: 0,
            elapsed_ms: 2,
        };
        (fingerprint, run)
    }

    fn manual_link_fixture(snapshot_id: &str) -> Vec<ManualLinkRecord> {
        vec![
            ManualLinkRecord {
                id: "manual-link:active".to_owned(),
                snapshot_id: snapshot_id.to_owned(),
                source_node_id: NodeId::new("node:web"),
                target_node_id: NodeId::new("node:api"),
                kind: "calls_remote".to_owned(),
                disposition: ManualLinkDisposition::Active,
                reason: "Declared by workspace configuration".to_owned(),
                decision: manual_link_decision(
                    "node:web",
                    "node:api",
                    "confirmed",
                    "Declared by workspace configuration",
                ),
                config_version: 1,
            },
            ManualLinkRecord {
                id: "manual-link:suppression".to_owned(),
                snapshot_id: snapshot_id.to_owned(),
                source_node_id: NodeId::new("node:api"),
                target_node_id: NodeId::new("node:web"),
                kind: "calls_remote".to_owned(),
                disposition: ManualLinkDisposition::Suppression,
                reason: "Known false positive".to_owned(),
                decision: manual_link_decision(
                    "node:api",
                    "node:web",
                    "suppressed",
                    "Known false positive",
                ),
                config_version: 1,
            },
        ]
    }

    fn manual_link_decision(
        source: &str,
        target: &str,
        status: &str,
        reason: &str,
    ) -> code_system_graph_model::LinkDecision {
        serde_json::from_value(serde_json::json!({
            "source": source,
            "target": target,
            "relation": "calls_remote",
            "matcher": "manual_exact",
            "matcher_version": "1.0.0",
            "score": 1.0,
            "confidence": 1.0,
            "reasons": [reason],
            "rejected_alternatives": [],
            "evidence": [{
                "id": format!("evidence:{source}:{target}"),
                "provenance": "manual"
            }],
            "status": status
        }))
        .unwrap_or_else(|error| panic!("manual decision fixture must decode: {error}"))
    }

    fn community_fixture(snapshot_id: &str) -> CommunitySnapshot {
        CommunitySnapshot {
            snapshot_id: snapshot_id.to_owned(),
            engine_version: "1.0.0".to_owned(),
            config: CommunityConfig {
                algorithm: CommunityAlgorithm::Louvain,
                scope: CommunityScope::Federated,
                seed: 7,
                resolution: 1.0,
                minimum_confidence: 0.5,
                edge_weights: Vec::new(),
                max_iterations: 20,
            },
            communities: vec![Community {
                id: CommunityId::new("community:orders"),
                label: "orders".to_owned(),
                members: vec![NodeId::new("node:api"), NodeId::new("node:web")],
                central_nodes: vec![NodeId::new("node:api")],
                repositories: vec![RepoId::new("repo:web")],
                services: Vec::new(),
                inbound_contracts: vec![NodeId::new("node:api")],
                outbound_contracts: vec![NodeId::new("node:web")],
                metrics: CommunityMetrics {
                    size: 2,
                    density: 0.5,
                    cohesion: 1.0,
                    coupling: 0.0,
                    cross_community_edges: 0,
                },
                label_evidence: Vec::new(),
                limitations: vec!["fixture coverage".to_owned()],
            }],
        }
    }

    #[test]
    fn publish_snapshot_should_round_trip_current_graph() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let (nodes, edges, evidence) = fixture();
        let workspace = workspace();
        let mut published_rows = 0_u64;
        let result = store
            .publish_snapshot_with_progress(
                SnapshotBatch {
                    workspace: &workspace,
                    snapshot_id: "snapshot:1",
                    nodes: &nodes,
                    edges: &edges,
                    evidence: &evidence,
                    fingerprints: &[],
                    extractor_batches: &[],
                    extractor_runs: &[],
                    manual_links: &[],
                    community_snapshot: None,
                },
                |rows| {
                    published_rows = published_rows
                        .checked_add(rows)
                        .expect("bounded fixture progress");
                },
            )
            .and_then(|()| {
                Ok((
                    store.load_current_graph("commerce")?,
                    store.load_current_evidence("commerce")?,
                ))
            });
        let counts = result.map(|((stored_nodes, stored_edges), stored_evidence)| {
            (
                stored_nodes.len(),
                stored_edges.len(),
                stored_evidence[0].start_line,
                stored_evidence[0].end_line,
                stored_evidence[0].extractor_version.clone(),
                stored_evidence[0].observed_at_commit.clone(),
            )
        });

        assert!(matches!(
            counts,
            Ok((
                2,
                1,
                Some(7),
                Some(9),
                version,
                Some(commit)
            )) if version == "1.0.0" && commit == "0123456789abcdef"
        ));
        assert!(published_rows >= 4);
    }

    #[test]
    fn publish_snapshot_should_reject_graph_poisoning_before_transaction() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let (nodes, mut edges, evidence) = fixture();
        edges[0].target = NodeId::new("node:missing");
        let workspace = workspace();

        let result = store.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:poisoned",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: None,
        });

        assert!(matches!(result, Err(StoreError::InvalidGraphSnapshot(_))));
    }

    #[test]
    fn interrupted_publication_should_preserve_previous_snapshot() {
        let mut store = SqliteStore::in_memory().expect("test store");
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        store
            .publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:previous",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })
            .expect("initial publication");
        let mut rows = 0_u64;
        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = store.publish_snapshot_with_progress(
                SnapshotBatch {
                    workspace: &workspace,
                    snapshot_id: "snapshot:interrupted",
                    nodes: &nodes,
                    edges: &edges,
                    evidence: &evidence,
                    fingerprints: &[],
                    extractor_batches: &[],
                    extractor_runs: &[],
                    manual_links: &[],
                    community_snapshot: None,
                },
                |completed| {
                    rows = rows.saturating_add(completed);
                    assert!(rows < 2, "injected publication interruption");
                },
            );
        }));

        assert!(interrupted.is_err());
        assert_eq!(
            store
                .current_snapshot_summary("commerce")
                .expect("previous snapshot remains")
                .snapshot_id,
            "snapshot:previous"
        );
    }

    #[test]
    fn publish_snapshot_should_reject_bidi_labels() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let (mut nodes, edges, evidence) = fixture();
        nodes[0].label = "safe\u{202e}txt.exe".to_owned();
        let workspace = workspace();

        let result = store.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:bidi",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: None,
        });

        assert!(matches!(result, Err(StoreError::InvalidGraphSnapshot(_))));
    }

    #[test]
    fn publish_snapshot_should_atomically_round_trip_graph_and_communities() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        let expected = community_fixture("snapshot:community");
        let result = store
            .publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:community",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: Some(&expected),
            })
            .and_then(|()| {
                Ok((
                    store.load_current_graph("commerce")?,
                    store.load_current_community_snapshot("commerce")?,
                ))
            });

        assert!(matches!(
            result,
            Ok(((stored_nodes, stored_edges), stored_communities))
                if stored_nodes.len() == 2
                    && stored_edges.len() == 1
                    && stored_communities == expected
        ));
    }

    #[test]
    fn invalid_community_membership_should_preserve_current_snapshot() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        let first = store.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:valid",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: None,
        });
        assert!(first.is_ok(), "valid fixture failed: {first:?}");
        let mut invalid = community_fixture("snapshot:invalid");
        invalid.communities[0]
            .members
            .push(NodeId::new("node:missing"));
        invalid.communities[0].metrics.size = 3;
        let failure = store.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:invalid",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: Some(&invalid),
        });
        let current = store.current_snapshot_summary("commerce");

        assert!(matches!(
            (failure, current),
            (
                Err(StoreError::CommunityMembershipNodeMissing {
                    snapshot_id,
                    community_id,
                    node_id,
                }),
                Ok(summary),
            ) if snapshot_id == "snapshot:invalid"
                && community_id == "community:orders"
                && node_id == "node:missing"
                && summary.snapshot_id == "snapshot:valid"
        ));
    }

    #[test]
    fn duplicate_community_ids_should_return_domain_error() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        let mut communities = community_fixture("snapshot:duplicate");
        communities
            .communities
            .push(communities.communities[0].clone());

        let result = store.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:duplicate",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: Some(&communities),
        });

        assert!(matches!(
            result,
            Err(StoreError::DuplicateCommunityId {
                snapshot_id,
                community_id,
            }) if snapshot_id == "snapshot:duplicate"
                && community_id == "community:orders"
        ));
    }

    #[test]
    fn historical_graph_and_community_snapshots_should_remain_loadable() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        let historical_communities = community_fixture("snapshot:historical");
        let first = store.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:historical",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: Some(&historical_communities),
        });
        assert!(first.is_ok(), "historical fixture failed: {first:?}");
        let second = store.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:current",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: None,
        });
        assert!(second.is_ok(), "current fixture failed: {second:?}");
        let result = store
            .load_graph_snapshot("snapshot:historical")
            .and_then(|graph| Ok((graph, store.load_community_snapshot("snapshot:historical")?)));

        assert!(matches!(
            result,
            Ok(((stored_nodes, stored_edges), stored_communities))
                if stored_nodes.len() == 2
                    && stored_edges.len() == 1
                    && stored_communities == historical_communities
        ));
    }

    #[test]
    fn load_community_snapshot_should_reject_inconsistent_stored_metrics() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        let communities = community_fixture("snapshot:malformed");
        let setup = store
            .publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:malformed",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: Some(&communities),
            })
            .and_then(|()| {
                store.connection.execute(
                    "UPDATE communities SET metrics_json = ?1
                     WHERE snapshot_id = ?2 AND id = ?3",
                    rusqlite::params![r#"{"size":1,"density":0.5,"cohesion":1.0,"coupling":0.0,"cross_community_edges":0}"#, "snapshot:malformed", "community:orders"],
                )?;
                Ok(())
            });
        assert!(setup.is_ok(), "malformed fixture failed: {setup:?}");

        let result = store.load_community_snapshot("snapshot:malformed");

        assert!(matches!(
            result,
            Err(StoreError::MalformedStoredData { reason, .. })
                if reason == "metrics size does not match normalized membership count"
        ));
    }

    #[test]
    fn integrity_check_should_pass_after_initial_schema_creation() {
        let result = SqliteStore::in_memory().and_then(|store| store.integrity_check());

        assert!(matches!(result, Ok(true)));
    }

    #[test]
    fn integrity_check_should_detect_foreign_key_corruption() {
        let store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let setup = store.connection.execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO workspaces(name, manifest_hash, id)
             VALUES ('corrupt', 'hash', 'workspace:corrupt');
             INSERT INTO repo_snapshots(id, workspace_name, is_current)
             VALUES ('snapshot:corrupt', 'corrupt', 1);
             INSERT INTO edges(
                snapshot_id, id, source_node_id, target_node_id, kind, confidence,
                epistemic_status
             ) VALUES (
                'snapshot:corrupt', 'edge:corrupt', 'missing:a', 'missing:b',
                '\"calls_remote\"', 1.0, '\"confirmed\"'
             );
             PRAGMA foreign_keys = ON;",
        );
        assert!(setup.is_ok(), "corruption fixture failed: {setup:?}");
        let result = store.integrity_check();

        assert!(matches!(result, Ok(false)));
    }

    #[test]
    fn fresh_database_should_apply_initial_schema() {
        let result = SqliteStore::in_memory().and_then(|store| store.schema_version());

        assert!(matches!(result, Ok(1)));
    }

    #[test]
    fn initial_schema_should_include_all_persistence_domains()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = SqliteStore::in_memory()?;
        let table_count = store.connection.query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type = 'table'
               AND name IN (
                    'workspaces',
                    'repositories',
                    'repo_snapshots',
                    'nodes',
                    'evidence',
                    'edges',
                    'artifact_fingerprints',
                    'extractor_batches',
                    'community_snapshots',
                    'manual_links',
                    'provider_capabilities',
                    'query_cache'
               )",
            [],
            |row| row.get::<_, i64>(0),
        )?;

        assert_eq!((store.schema_version()?, table_count), (1, 12));
        Ok(())
    }

    #[test]
    fn exact_initial_schema_validation_should_be_repeatable()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut connection = rusqlite::Connection::open_in_memory()?;
        super::initialize_empty_schema(&mut connection)?;
        super::validate_exact_schema(&connection)?;
        super::validate_exact_schema(&connection)?;

        assert_eq!(super::schema_version(&connection)?, 1);
        Ok(())
    }

    #[test]
    fn current_schema_should_enforce_tables_indexes_triggers_and_foreign_keys()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = SqliteStore::in_memory()?;
        let strict_tables = store.connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(strict), 0)
             FROM pragma_table_list
             WHERE name IN ('manual_links', 'provider_capabilities', 'query_cache')",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;
        let indexes = store.connection.query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type = 'index'
               AND name IN (
                    'repo_snapshots_id_workspace_idx',
                    'manual_links_source_idx',
                    'manual_links_target_idx',
                    'manual_links_disposition_idx',
                    'provider_capabilities_repo_provider_idx',
                    'query_cache_snapshot_idx',
                    'query_cache_workspace_expiry_idx',
                    'query_cache_workspace_stored_idx'
               )",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let triggers = store.connection.query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE type = 'trigger'
               AND name IN (
                    'provider_capabilities_require_registration',
                    'provider_capabilities_update_require_registration',
                    'query_cache_bound_workspace_entries'
               )",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let foreign_keys = ["manual_links", "provider_capabilities", "query_cache"]
            .into_iter()
            .try_fold(0_i64, |count, table| {
                store
                    .connection
                    .query_row(
                        &format!("SELECT COUNT(*) FROM pragma_foreign_key_list('{table}')"),
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .map(|table_count| count + table_count)
            })?;

        assert_eq!(
            (strict_tables, indexes, triggers, foreign_keys),
            ((3, 3), 8, 3, 9)
        );
        Ok(())
    }

    #[test]
    fn exact_schema_should_reject_unknown_version() {
        let connection = match rusqlite::Connection::open_in_memory() {
            Ok(connection) => connection,
            Err(error) => panic!("test connection must initialize: {error}"),
        };
        let setup = connection.execute_batch(
            "CREATE TABLE schema_metadata (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
             );
             INSERT INTO schema_metadata(version) VALUES (999);",
        );
        assert!(setup.is_ok(), "newer schema fixture failed: {setup:?}");
        let result = SqliteStore::from_connection(connection);

        assert!(matches!(
            result,
            Err(StoreError::ObsoleteDevelopmentDatabase)
        ));
    }

    #[test]
    fn exact_schema_should_reject_missing_schema_objects() -> Result<(), Box<dyn std::error::Error>>
    {
        let store = SqliteStore::in_memory()?;
        store
            .connection
            .execute_batch("DROP TRIGGER query_cache_bound_workspace_entries")?;

        assert!(matches!(
            super::validate_exact_schema(&store.connection),
            Err(StoreError::ObsoleteDevelopmentDatabase)
        ));
        Ok(())
    }

    #[test]
    fn database_instance_identity_should_be_opaque_and_stable()
    -> Result<(), Box<dyn std::error::Error>> {
        let store = SqliteStore::in_memory()?;
        let first = store.database_instance_id()?;
        let second = store.database_instance_id()?;

        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
        Ok(())
    }

    #[test]
    fn version_one_database_without_definitive_batch_columns_should_require_rebuild()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("obsolete.db");
        let connection = rusqlite::Connection::open(&database)?;
        connection.execute_batch(INITIAL_SCHEMA)?;
        connection.execute_batch(
            "ALTER TABLE extractor_batches DROP COLUMN budget_fingerprint;
             ALTER TABLE extractor_batches DROP COLUMN source_was_lossy;",
        )?;
        drop(connection);

        let result = SqliteStore::open(&database);

        assert!(matches!(
            result,
            Err(StoreError::ObsoleteDevelopmentDatabase)
        ));
        Ok(())
    }

    #[test]
    fn read_only_open_should_reject_non_exact_schema() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("uninitialized.db");
        let connection = rusqlite::Connection::open(&database)?;
        connection.execute_batch(
            "CREATE TABLE schema_metadata (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
             );",
        )?;
        drop(connection);

        let result = SqliteStore::open_read_only(&database);

        assert!(matches!(
            result,
            Err(StoreError::ObsoleteDevelopmentDatabase)
        ));
        Ok(())
    }

    #[test]
    fn workspace_registry_should_round_trip_native_paths() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let expected = workspace();
        let result = store
            .save_workspace_registry(&expected)
            .and_then(|()| store.load_workspace_registry("commerce"));

        assert!(matches!(result, Ok(actual) if actual == expected));
    }

    #[test]
    fn workspace_registry_should_list_repository_counts() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let result = store
            .save_workspace_registry(&workspace())
            .and_then(|()| store.list_workspaces());

        assert!(matches!(
            result,
            Ok(workspaces)
                if workspaces.len() == 1
                    && workspaces[0].name == "commerce"
                    && workspaces[0].repository_count == 1
        ));
    }

    #[test]
    fn workspace_registry_should_remove_and_collect_orphans() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let result = store
            .save_workspace_registry(&workspace())
            .and_then(|()| store.remove_workspace("commerce"))
            .and_then(|removed| {
                let repositories =
                    store
                        .connection
                        .query_row("SELECT COUNT(*) FROM repositories", [], |row| {
                            row.get::<_, i64>(0)
                        })?;
                Ok((removed, store.workspace_exists("commerce")?, repositories))
            });

        assert!(matches!(result, Ok((true, false, 0))));
    }

    #[test]
    fn failed_snapshot_should_preserve_previous_current_snapshot() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        let first = store.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:valid",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: None,
        });
        assert!(first.is_ok(), "valid fixture failed: {first:?}");
        let mut invalid_edges = edges;
        invalid_edges[0].evidence = vec![EvidenceId::new("evidence:missing")];
        let failed = store.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:invalid",
            nodes: &nodes,
            edges: &invalid_edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: None,
        });
        let result = failed
            .and_then(|()| store.load_current_graph("commerce"))
            .map(|_| false)
            .or_else(|_| {
                store
                    .load_current_graph("commerce")
                    .map(|(_, current_edges)| current_edges[0].id.as_str() == "edge:1")
            });

        assert!(matches!(result, Ok(true)));
    }

    #[test]
    fn online_backup_should_preserve_current_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("source.db");
        let backup = temporary.path().join("backup.db");
        let mut store = SqliteStore::open(&database)?;
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        store.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:1",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: None,
        })?;
        store.backup_to(&backup)?;
        let backup_store = SqliteStore::open(&backup)?;
        let result = backup_store
            .load_current_graph("commerce")
            .map(|(stored_nodes, stored_edges)| (stored_nodes.len(), stored_edges.len()));

        assert!(matches!(result, Ok((2, 1))));
        Ok(())
    }

    #[test]
    fn backup_file_should_not_leave_destination_when_source_is_invalid()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let missing_source = temporary.path().join("missing.db");
        let destination = temporary.path().join("backup.db");

        let result = SqliteStore::backup_file(&missing_source, &destination);

        assert!(result.is_err());
        assert!(
            !destination.exists(),
            "failed backup must not leave a destination placeholder"
        );
        Ok(())
    }

    #[test]
    fn backup_file_should_allow_retry_after_failed_source_validation()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let source = temporary.path().join("source.db");
        let destination = temporary.path().join("backup.db");
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        {
            let mut store = SqliteStore::open(&source)?;
            store.publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:0",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })?;
        }

        let first = SqliteStore::backup_file(&temporary.path().join("missing.db"), &destination);

        assert!(first.is_err());
        assert!(
            !destination.exists(),
            "first failed backup must not block the retry path"
        );

        let second = SqliteStore::backup_file(&source, &destination);

        assert!(second.is_ok());
        Ok(())
    }

    #[test]
    fn restore_should_not_leave_destination_when_backup_is_invalid()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let backup = temporary.path().join("backup.db");
        fs::write(&backup, b"not-a-database")?;

        let result = SqliteStore::restore_from(&database, &backup);

        assert!(result.is_err());
        assert!(
            !database.exists(),
            "failed restore to a new path must not leave a destination placeholder"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn open_should_restrict_sidecars_created_during_schema_initialization()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("fresh.db");
        SqliteStore::open(&database)?;

        for suffix in ["-wal", "-shm"] {
            let mut sidecar = database.as_os_str().to_os_string();
            sidecar.push(suffix);
            let sidecar_path = std::path::PathBuf::from(sidecar);
            if sidecar_path.exists() {
                assert_eq!(
                    fs::metadata(&sidecar_path)?.permissions().mode() & 0o777,
                    0o600,
                    "sidecar `{}` must be owner-only after initialization",
                    sidecar_path.display()
                );
            }
        }
        Ok(())
    }

    #[test]
    fn open_should_initialize_once_without_migration_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");

        let initial = SqliteStore::open(&database)?;
        assert_eq!(initial.schema_version()?, 1);
        drop(initial);
        let repeated = SqliteStore::open(&database)?;
        assert_eq!(repeated.schema_version()?, 1);
        Ok(())
    }

    #[test]
    fn restore_should_preserve_replaced_database_as_safety_backup()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let backup = temporary.path().join("selected-backup.db");
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        {
            let mut store = SqliteStore::open(&database)?;
            store.publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:before",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })?;
            store.backup_to(&backup)?;
            store.publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:after",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })?;
        }

        let report = SqliteStore::restore_from(&database, &backup)?;
        let restored =
            SqliteStore::open_read_only(&database)?.current_snapshot_summary("commerce")?;
        let safety_path = report
            .safety_backup_path
            .as_ref()
            .ok_or_else(|| std::io::Error::other("restore safety backup missing"))?;
        let replaced =
            SqliteStore::open_read_only(safety_path)?.current_snapshot_summary("commerce")?;

        assert_eq!(
            (
                restored.snapshot_id,
                replaced.snapshot_id,
                report.schema_version,
            ),
            ("snapshot:before".to_owned(), "snapshot:after".to_owned(), 1)
        );
        Ok(())
    }

    #[test]
    fn restore_should_reject_missing_backup_that_collides_with_default_safety_path()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let backup = temporary.path().join("store.db.pre-restore.backup");
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        {
            let mut store = SqliteStore::open(&database)?;
            store.publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:before",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })?;
        }
        assert!(!backup.exists());

        let result = SqliteStore::restore_from(&database, &backup);

        assert!(result.is_err());
        let unchanged =
            SqliteStore::open_read_only(&database)?.current_snapshot_summary("commerce")?;
        assert_eq!(unchanged.snapshot_id, "snapshot:before");
        assert!(
            !backup.exists(),
            "missing input backup must not be materialized as a safety backup"
        );
        Ok(())
    }

    #[test]
    fn rollback_restore_should_recover_database_from_safety_backup()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let safety = temporary.path().join("store.db.pre-restore.backup");
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        {
            let mut store = SqliteStore::open(&database)?;
            store.publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:before",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })?;
            store.backup_to(&safety)?;
            store.publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:after",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })?;
        }

        super::rollback_restore_from_safety_backup(&database, &safety)?;
        let recovered =
            SqliteStore::open_read_only(&database)?.current_snapshot_summary("commerce")?;

        assert_eq!(recovered.snapshot_id, "snapshot:before");
        Ok(())
    }

    #[test]
    fn backup_validation_should_use_read_only_untrusted_schema_connection()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let backup = temporary.path().join("backup.db");
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        {
            let mut store = SqliteStore::open(&database)?;
            store.publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:0",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })?;
            store.backup_to(&backup)?;
        }

        let connection = super::open_validated_backup_source(&backup)?;
        let query_only =
            connection.query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))?;
        let trusted_schema =
            connection.query_row("PRAGMA trusted_schema", [], |row| row.get::<_, i64>(0))?;
        let write_result =
            connection.execute("CREATE TABLE injected_table(id INTEGER PRIMARY KEY)", []);

        assert_eq!((query_only, trusted_schema), (1, 0));
        assert!(
            write_result.is_err(),
            "validated backup source must stay read-only"
        );
        Ok(())
    }

    #[test]
    fn restore_should_reject_backup_with_extra_trigger() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let backup = temporary.path().join("backup.db");
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        {
            let mut store = SqliteStore::open(&database)?;
            store.publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:0",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })?;
            store.backup_to(&backup)?;
        }
        let connection = rusqlite::Connection::open(&backup)?;
        connection.execute(
            "CREATE TRIGGER injected AFTER INSERT ON workspaces BEGIN SELECT 1; END",
            [],
        )?;

        let result = SqliteStore::restore_from(&database, &backup);

        assert!(matches!(result, Err(StoreError::InvalidBackup { .. })));
        Ok(())
    }

    #[test]
    fn restore_should_reject_backup_with_foreign_key_violations()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let backup = temporary.path().join("backup.db");
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        {
            let mut store = SqliteStore::open(&database)?;
            store.publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:0",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })?;
            store.backup_to(&backup)?;
        }
        let connection = rusqlite::Connection::open(&backup)?;
        connection.execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO edges(
                snapshot_id, id, source_node_id, target_node_id, kind, confidence,
                epistemic_status
             ) VALUES (
                'snapshot:0', 'edge:corrupt', 'missing:a', 'missing:b',
                '\"calls_remote\"', 1.0, '\"confirmed\"'
             );
             PRAGMA foreign_keys = ON;",
        )?;

        let result = SqliteStore::restore_from(&database, &backup);

        assert!(matches!(
            result,
            Err(StoreError::InvalidBackup { reason, .. })
                if reason == "foreign key check reported violations"
        ));
        Ok(())
    }

    #[test]
    fn restore_should_copy_from_pinned_source_snapshot() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let backup = temporary.path().join("backup.db");
        let restore_target = temporary.path().join("restored.db");
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        {
            let mut store = SqliteStore::open(temporary.path().join("store.db"))?;
            store.publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:0",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })?;
            store.backup_to(&backup)?;
        }

        let coordination = Arc::new((Mutex::new(false), Condvar::new()));
        let backup_for_corruptor = backup.clone();
        let restore_target_for_restorer = restore_target.clone();
        let coordination_for_corruptor = Arc::clone(&coordination);
        let coordination_for_restorer = Arc::clone(&coordination);

        let corruptor = std::thread::spawn(move || {
            let (lock, cv) = &*coordination_for_corruptor;
            let mut ready = lock.lock().expect("coordination lock");
            while !*ready {
                ready = cv.wait(ready).expect("coordination wait");
            }
            let connection = Connection::open(&backup_for_corruptor)?;
            connection.execute_batch(
                "PRAGMA foreign_keys = OFF;
                 INSERT INTO edges(
                    snapshot_id, id, source_node_id, target_node_id, kind, confidence,
                    epistemic_status
                 ) VALUES (
                    'snapshot:0', 'edge:corrupt', 'missing:a', 'missing:b',
                    '\"calls_remote\"', 1.0, '\"confirmed\"'
                 );
                 PRAGMA foreign_keys = ON;",
            )
        });

        let restorer = std::thread::spawn(move || -> Result<(), StoreError> {
            let source = super::open_validated_backup_source(&backup)?;
            source.execute_batch("BEGIN DEFERRED")?;
            let restore_result = (|| {
                super::validate_backup(&source, &backup)?;
                {
                    let (lock, cv) = &*coordination_for_restorer;
                    let mut ready = lock.lock().expect("coordination lock");
                    *ready = true;
                    cv.notify_one();
                }
                std::thread::sleep(Duration::from_millis(50));
                super::prepare_database_file(&restore_target_for_restorer)?;
                let mut destination = Connection::open(&restore_target_for_restorer)?;
                super::backup_connection_to(&source, &mut destination)?;
                super::configure_connection(&destination)?;
                super::validate_backup(&destination, &restore_target_for_restorer)
            })();
            match restore_result {
                Ok(()) => source.execute_batch("COMMIT")?,
                Err(error) => {
                    let _ = source.execute_batch("ROLLBACK");
                    return Err(error);
                }
            }
            Ok(())
        });

        corruptor.join().expect("corruptor thread panicked")?;
        restorer.join().expect("restorer thread panicked")?;

        let store = SqliteStore::open_read_only(&restore_target)?;
        assert!(store.integrity_check()?);
        Ok(())
    }

    #[test]
    fn restore_should_reject_backup_with_altered_trigger_sql()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let backup = temporary.path().join("backup.db");
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        {
            let mut store = SqliteStore::open(&database)?;
            store.publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:0",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })?;
            store.backup_to(&backup)?;
        }
        let connection = rusqlite::Connection::open(&backup)?;
        connection.execute_batch(
            "DROP TRIGGER query_cache_bound_workspace_entries;
             CREATE TRIGGER query_cache_bound_workspace_entries
             BEFORE INSERT ON query_cache
             BEGIN
                 SELECT RAISE(ABORT, 'tampered');
             END;",
        )?;

        let result = SqliteStore::restore_from(&database, &backup);

        assert!(matches!(result, Err(StoreError::InvalidBackup { .. })));
        Ok(())
    }

    #[test]
    fn restore_should_reject_corrupt_backup() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let backup = temporary.path().join("backup.db");
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        {
            let mut store = SqliteStore::open(&database)?;
            store.publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:0",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })?;
            store.backup_to(&backup)?;
        }
        let mut bytes = fs::read(&backup)?;
        bytes.truncate(bytes.len() / 4);
        fs::write(&backup, bytes)?;

        let result = SqliteStore::restore_from(&database, &backup);

        assert!(
            matches!(
                result,
                Err(StoreError::InvalidBackup { .. }
                    | StoreError::Io { .. }
                    | StoreError::Sqlite(_))
            ),
            "unexpected restore result: {result:?}"
        );
        Ok(())
    }

    #[test]
    fn normalize_schema_sql_should_collapse_whitespace() {
        assert_eq!(
            super::normalize_schema_sql("CREATE  TABLE\nfoo ( id INTEGER )"),
            "CREATE TABLE foo ( id INTEGER )"
        );
    }

    #[test]
    fn normalize_schema_sql_should_preserve_whitespace_inside_literals() {
        let with_space =
            "SELECT RAISE(ABORT, 'provider capability repository is not registered in workspace')";
        let with_newline =
            "SELECT RAISE(ABORT, 'provider\ncapability repository is not registered in workspace')";

        assert_ne!(
            super::normalize_schema_sql(with_space),
            super::normalize_schema_sql(with_newline)
        );
    }

    #[test]
    fn restore_should_reject_backup_with_literal_whitespace_tampering()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let backup = temporary.path().join("backup.db");
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        {
            let mut store = SqliteStore::open(&database)?;
            store.publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:0",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })?;
            store.backup_to(&backup)?;
        }
        let original_sql: String = rusqlite::Connection::open(&backup)?.query_row(
            "SELECT sql FROM sqlite_schema
             WHERE name = 'provider_capabilities_require_registration'",
            [],
            |row| row.get(0),
        )?;
        let tampered_sql = original_sql.replace(
            "'provider capability repository is not registered in workspace'",
            "'provider\ncapability repository is not registered in workspace'",
        );
        let connection = rusqlite::Connection::open(&backup)?;
        connection.execute_batch(&format!(
            "DROP TRIGGER provider_capabilities_require_registration; {tampered_sql};"
        ))?;

        let result = SqliteStore::restore_from(&database, &backup);

        assert!(matches!(result, Err(StoreError::InvalidBackup { .. })));
        Ok(())
    }

    #[test]
    fn umask_permission_helper() -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var(UMASK_HELPER_ENV).as_deref() != Ok("1") {
            return Ok(());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let database = std::env::var_os(UMASK_DATABASE_ENV)
                .map(std::path::PathBuf::from)
                .ok_or_else(|| std::io::Error::other("umask database environment is missing"))?;
            let ready = std::env::var_os(UMASK_READY_ENV)
                .map(std::path::PathBuf::from)
                .ok_or_else(|| std::io::Error::other("umask ready environment is missing"))?;
            let backup = database.with_extension("db.backup");
            let workspace = workspace();
            let (nodes, edges, evidence) = fixture();
            {
                let mut store = SqliteStore::open(&database)?;
                store.publish_snapshot(SnapshotBatch {
                    workspace: &workspace,
                    snapshot_id: "snapshot:umask",
                    nodes: &nodes,
                    edges: &edges,
                    evidence: &evidence,
                    fingerprints: &[],
                    extractor_batches: &[],
                    extractor_runs: &[],
                    manual_links: &[],
                    community_snapshot: None,
                })?;
                store.backup_to(&backup)?;
            }
            for path in [
                database.clone(),
                backup,
                {
                    let mut wal = database.as_os_str().to_os_string();
                    wal.push("-wal");
                    std::path::PathBuf::from(wal)
                },
                {
                    let mut shm = database.as_os_str().to_os_string();
                    shm.push("-shm");
                    std::path::PathBuf::from(shm)
                },
            ] {
                if path.exists() {
                    assert_eq!(
                        fs::metadata(&path)?.permissions().mode() & 0o777,
                        0o600,
                        "permissions differ for `{}`",
                        path.display()
                    );
                }
            }
            fs::write(ready, "ready")?;
            std::thread::sleep(Duration::from_secs(30));
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn database_files_should_use_owner_only_permissions_under_permissive_umask()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("umask.db");
        let ready = temporary.path().join("ready");
        let executable = std::env::current_exe()?;
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "umask 000; exec {} --exact tests::umask_permission_helper --nocapture",
                executable.display()
            ))
            .env(UMASK_HELPER_ENV, "1")
            .env(UMASK_DATABASE_ENV, &database)
            .env(UMASK_READY_ENV, &ready)
            .spawn()?;
        let mut helper_ready = false;
        for _attempt in 0..200 {
            if ready.exists() {
                helper_ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if !helper_ready {
            child.kill()?;
            let _status = child.wait()?;
            return Err(std::io::Error::other("umask helper did not become ready").into());
        }
        child.kill()?;
        let _status = child.wait()?;
        Ok(())
    }

    #[test]
    fn wal_should_allow_readers_during_snapshot_publication()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("concurrent.db");
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        let mut writer = SqliteStore::open(&database)?;
        writer.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:0",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: None,
        })?;
        let reader_paths = [database.clone(), database];
        let readers = reader_paths.map(|path| {
            std::thread::spawn(move || -> Result<(), String> {
                for _iteration in 0..25 {
                    SqliteStore::open_read_only(&path)
                        .and_then(|store| store.load_current_graph("commerce"))
                        .map_err(|error| error.to_string())?;
                }
                Ok(())
            })
        });
        for sequence in 1..=10 {
            writer.publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: &format!("snapshot:{sequence}"),
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })?;
        }
        let reader_results = readers.map(|reader| {
            reader
                .join()
                .map_err(|_| "reader thread panicked".to_owned())
                .and_then(|result| result)
        });

        assert!(reader_results.into_iter().all(|result| result.is_ok()));
        Ok(())
    }

    #[test]
    fn writer_lock_should_reject_second_active_owner() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let first = StoreLock::acquire(&database, Duration::from_mins(1))?;
        let second = StoreLock::acquire(&database, Duration::from_mins(1));

        assert!(matches!(second, Err(StoreError::LockHeld(_))));
        drop(first);
        Ok(())
    }

    #[test]
    fn writer_lock_should_recover_expired_sidecar() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let sidecar = lock_path(&database);
        fs::write(&sidecar, "expired-owner")?;
        std::thread::sleep(Duration::from_millis(5));
        let lock = StoreLock::acquire(&database, Duration::from_millis(1));

        assert!(lock.is_ok(), "stale lock was not recovered: {lock:?}");
        Ok(())
    }

    #[test]
    fn writer_lock_should_not_reclaim_live_owner_after_threshold()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let first = StoreLock::acquire(&database, Duration::ZERO)?;
        std::thread::sleep(Duration::from_millis(2));
        let second = StoreLock::acquire(&database, Duration::ZERO);

        assert!(matches!(second, Err(StoreError::LockHeld(_))));
        drop(first);
        Ok(())
    }

    #[test]
    fn multiprocess_lock_helper() -> Result<(), Box<dyn std::error::Error>> {
        if std::env::var(LOCK_HELPER_ENV).as_deref() != Ok("1") {
            return Ok(());
        }
        let database = std::env::var_os(LOCK_DATABASE_ENV)
            .map(std::path::PathBuf::from)
            .ok_or_else(|| std::io::Error::other("lock database environment is missing"))?;
        let ready = std::env::var_os(LOCK_READY_ENV)
            .map(std::path::PathBuf::from)
            .ok_or_else(|| std::io::Error::other("lock ready environment is missing"))?;
        let _lock = StoreLock::acquire(&database, Duration::from_hours(1))?;
        fs::write(ready, "ready")?;
        std::thread::sleep(Duration::from_secs(30));
        Ok(())
    }

    #[test]
    fn multiprocess_lock_should_recover_after_abrupt_owner_exit()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("store.db");
        let ready = temporary.path().join("ready");
        let executable = std::env::current_exe()?;
        let mut child = Command::new(executable)
            .args(["--exact", "tests::multiprocess_lock_helper", "--nocapture"])
            .env(LOCK_HELPER_ENV, "1")
            .env(LOCK_DATABASE_ENV, &database)
            .env(LOCK_READY_ENV, &ready)
            .spawn()?;
        let mut helper_ready = false;
        for _attempt in 0..200 {
            if ready.exists() {
                helper_ready = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if !helper_ready {
            child.kill()?;
            let _status = child.wait()?;
            return Err(std::io::Error::other("lock helper did not become ready").into());
        }
        let while_alive = StoreLock::acquire(&database, Duration::ZERO);
        child.kill()?;
        let _status = child.wait()?;
        let after_exit = StoreLock::acquire(&database, Duration::ZERO);

        assert_eq!(
            (
                matches!(while_alive, Err(StoreError::LockHeld(_))),
                after_exit.is_ok(),
            ),
            (true, true)
        );
        Ok(())
    }

    #[test]
    fn published_snapshot_should_record_repository_freshness() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        let result = store
            .publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:1",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })
            .and_then(|()| store.load_current_freshness("commerce"));

        assert!(matches!(
            result,
            Ok(freshness)
                if freshness.len() == 1
                    && freshness[0].state == RepoFreshnessState::Fresh
        ));
    }

    #[test]
    fn incremental_artifacts_and_runs_should_round_trip() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        let (fingerprint, run) = incremental_fixture();
        let extractor_batch = StoredExtractorBatch {
            source: fingerprint.clone(),
            extractor_version: "1.0.0".to_owned(),
            budget_fingerprint: "extraction-budgets:test".to_owned(),
            source_was_lossy: false,
            output_count: 1,
            payload: br#"{"observations":["GET:/orders"]}"#.to_vec(),
        };
        let result = store
            .publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:1",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: std::slice::from_ref(&fingerprint),
                extractor_batches: std::slice::from_ref(&extractor_batch),
                extractor_runs: std::slice::from_ref(&run),
                manual_links: &[],
                community_snapshot: None,
            })
            .and_then(|()| {
                Ok((
                    store.load_current_artifact_fingerprints("commerce")?,
                    store.load_current_extractor_batches("commerce")?,
                    store.load_current_extractor_runs("commerce")?,
                ))
            });

        assert!(matches!(
            result,
            Ok((fingerprints, batches, runs))
                if fingerprints == vec![fingerprint]
                    && batches == vec![extractor_batch]
                    && runs == vec![run]
        ));
        assert!(
            store
                .load_current_extractor_batches_with_limit("commerce", 1)
                .expect("bounded batch read")
                .is_empty(),
            "SQLite must omit an oversized payload before copying its BLOB"
        );
    }

    #[test]
    fn obsolete_development_batch_contract_should_require_full_rebuild() {
        let mut store = SqliteStore::in_memory().expect("test store must initialize");
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        let (fingerprint, run) = incremental_fixture();
        let extractor_batch = StoredExtractorBatch {
            source: fingerprint.clone(),
            extractor_version: "1.0.0".to_owned(),
            budget_fingerprint: "extraction-budgets:test".to_owned(),
            source_was_lossy: false,
            output_count: 1,
            payload: br#"{"observations":[]}"#.to_vec(),
        };
        store
            .publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:legacy",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: std::slice::from_ref(&fingerprint),
                extractor_batches: std::slice::from_ref(&extractor_batch),
                extractor_runs: std::slice::from_ref(&run),
                manual_links: &[],
                community_snapshot: None,
            })
            .expect("fixture snapshot should publish");
        store
            .connection
            .execute(
                "UPDATE extractor_batches SET extractor_version = '1.0.0.5'",
                [],
            )
            .expect("fixture contract should be replaced");

        assert!(matches!(
            store.load_current_extractor_batches_with_limit("commerce", 1),
            Err(StoreError::ObsoleteDevelopmentDatabase)
        ));
    }

    #[test]
    fn manual_links_should_round_trip_and_preserve_snapshot_history()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut store = SqliteStore::in_memory()?;
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        let historical_links = manual_link_fixture("snapshot:historical-links");
        store.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:historical-links",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &historical_links,
            community_snapshot: None,
        })?;
        store.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:current-links",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: None,
        })?;
        let mut current_links = manual_link_fixture("snapshot:current-links");
        current_links.truncate(1);
        store.persist_manual_links("snapshot:current-links", &current_links)?;

        let stored_historical = store.load_manual_links("snapshot:historical-links")?;
        let stored_current = store.load_manual_links("snapshot:current-links")?;

        assert_eq!(
            (stored_historical, stored_current),
            (historical_links, current_links)
        );
        Ok(())
    }

    #[test]
    fn provider_capabilities_should_upsert_and_round_trip_without_provider_payloads()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut store = SqliteStore::in_memory()?;
        store.save_workspace_registry(&workspace())?;
        let mut expected = ProviderCapabilityRecord {
            workspace_name: "commerce".to_owned(),
            repo_id: RepoId::new("repo:web"),
            provider: "codegraph".to_owned(),
            provider_version: "1.2.3".to_owned(),
            capabilities: vec!["call_paths".to_owned(), "symbols".to_owned()],
            observed_at_unix_ms: 100,
        };
        store.upsert_provider_capabilities(&expected)?;
        expected.capabilities = vec!["symbols".to_owned()];
        expected.observed_at_unix_ms = 200;
        store.upsert_provider_capabilities(&expected)?;

        let actual = store.load_provider_capabilities(
            "commerce",
            &RepoId::new("repo:web"),
            "codegraph",
            "1.2.3",
        )?;

        assert_eq!(actual, Some(expected));
        Ok(())
    }

    #[test]
    fn clear_query_cache_should_remove_only_selected_workspace()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut store = SqliteStore::in_memory()?;
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        store.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:cache-commerce",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: None,
        })?;
        let mut other_workspace = workspace.clone();
        other_workspace.id = WorkspaceId::new("workspace:other");
        other_workspace.name = "other".to_owned();
        store.publish_snapshot(SnapshotBatch {
            workspace: &other_workspace,
            snapshot_id: "snapshot:cache-other",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: None,
        })?;
        for (workspace_name, snapshot_id) in [
            ("commerce", "snapshot:cache-commerce"),
            ("other", "snapshot:cache-other"),
        ] {
            store.connection.execute(
                "INSERT INTO query_cache(
                    workspace_name, snapshot_id, input_fingerprint, result_summary_json,
                    stored_at_unix_ms
                 ) VALUES (?1, ?2, 'input:1', ?3, 1)",
                params![workspace_name, snapshot_id, b"{}".as_slice()],
            )?;
        }

        let removed = store.clear_query_cache("commerce")?;
        let commerce_count = store.connection.query_row(
            "SELECT COUNT(*) FROM query_cache WHERE workspace_name = 'commerce'",
            [],
            |row| row.get::<_, i64>(0),
        )?;
        let other_count = store.connection.query_row(
            "SELECT COUNT(*) FROM query_cache WHERE workspace_name = 'other'",
            [],
            |row| row.get::<_, i64>(0),
        )?;

        assert_eq!((removed, commerce_count, other_count), (1, 0, 1));
        Ok(())
    }

    #[test]
    fn query_cache_should_round_trip_exact_unexpired_result()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut store = SqliteStore::in_memory()?;
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        store.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:query-cache",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: None,
        })?;
        let expected = QueryCacheRecord {
            workspace_name: "commerce".to_owned(),
            snapshot_id: "snapshot:query-cache".to_owned(),
            input_fingerprint: "query:one".to_owned(),
            result_summary_json: br#"{"schema_version":1}"#.to_vec(),
            stored_at_unix_ms: 100,
            expires_at_unix_ms: Some(200),
        };
        store.put_query_cache(&expected)?;

        let current =
            store.load_query_cache("commerce", "snapshot:query-cache", "query:one", 150)?;
        let expired =
            store.load_query_cache("commerce", "snapshot:query-cache", "query:one", 201)?;

        assert_eq!((current, expired), (Some(expected), None));
        Ok(())
    }

    #[test]
    fn query_cache_should_bound_entries_per_workspace() -> Result<(), Box<dyn std::error::Error>> {
        let mut store = SqliteStore::in_memory()?;
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        store.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:bounded-cache",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: None,
        })?;
        for sequence in 0..=1024 {
            store.connection.execute(
                "INSERT INTO query_cache(
                    workspace_name, snapshot_id, input_fingerprint, result_summary_json,
                    stored_at_unix_ms
                 ) VALUES ('commerce', 'snapshot:bounded-cache', ?1, ?2, ?3)",
                params![
                    format!("input:{sequence}"),
                    b"{}".as_slice(),
                    i64::from(sequence)
                ],
            )?;
        }
        let bounded = store.connection.query_row(
            "SELECT COUNT(*), MIN(stored_at_unix_ms) FROM query_cache
             WHERE workspace_name = 'commerce'",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )?;

        assert_eq!(bounded, (1024, 1));
        Ok(())
    }

    #[test]
    fn fts_should_search_only_current_snapshot_nodes() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        let result = store
            .publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:1",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })
            .and_then(|()| store.search_current_nodes("commerce", "orders", 10));

        assert!(matches!(result, Ok(matches) if matches.len() == 2));
    }

    #[test]
    fn ranked_fts_should_order_equal_scores_by_stable_node_id() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        let setup = store.publish_snapshot(SnapshotBatch {
            workspace: &workspace,
            snapshot_id: "snapshot:ranked",
            nodes: &nodes,
            edges: &edges,
            evidence: &evidence,
            fingerprints: &[],
            extractor_batches: &[],
            extractor_runs: &[],
            manual_links: &[],
            community_snapshot: None,
        });
        assert!(setup.is_ok(), "ranked FTS fixture failed: {setup:?}");

        let first = store.search_current_nodes_ranked("commerce", "orders", 500);
        let second = store.search_current_nodes_ranked("commerce", "orders", 500);

        assert!(matches!(
            (first, second),
            (Ok(first), Ok(second))
                if first == second
                    && first.len() == 2
                    && first[0].node.id.as_str() == "node:api"
                    && first[1].node.id.as_str() == "node:web"
                    && first[0].fts_rank.to_bits() == first[1].fts_rank.to_bits()
        ));
    }

    #[test]
    fn fts_should_cleanup_replaced_and_removed_snapshots() {
        let mut store = match SqliteStore::in_memory() {
            Ok(store) => store,
            Err(error) => panic!("test store must initialize: {error}"),
        };
        let workspace = workspace();
        let (nodes, edges, evidence) = fixture();
        let publish = |store: &mut SqliteStore| {
            store.publish_snapshot(SnapshotBatch {
                workspace: &workspace,
                snapshot_id: "snapshot:same",
                nodes: &nodes,
                edges: &edges,
                evidence: &evidence,
                fingerprints: &[],
                extractor_batches: &[],
                extractor_runs: &[],
                manual_links: &[],
                community_snapshot: None,
            })
        };
        let result = publish(&mut store)
            .and_then(|()| publish(&mut store))
            .and_then(|()| {
                let before_remove =
                    store
                        .connection
                        .query_row("SELECT COUNT(*) FROM nodes_fts", [], |row| {
                            row.get::<_, i64>(0)
                        })?;
                store.remove_workspace("commerce")?;
                let after_remove = store
                    .connection
                    .query_row("SELECT COUNT(*) FROM nodes_fts", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .map_err(StoreError::from)?;
                Ok((before_remove, after_remove))
            });

        assert!(matches!(result, Ok((2, 0))));
    }
}

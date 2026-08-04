#[cfg(any(unix, windows))]
use std::cell::Cell;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use code_system_graph_model::{
    CheckoutId, Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, EvidenceId, NativePath, NativePathEncoding, Node, NodeId, NodeKind, Provenance, RepoId, RepositoryRecord, WorkspaceId, WorkspaceRecord
};
use rusqlite::Connection;

#[cfg(unix)]
use super::{StagedDatabase, ensure_distinct_paths};
use super::{open_backup_source, restore_database};
use crate::{SnapshotBatch, SqliteStore, StoreError};

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

fn seed_snapshot(store: &mut SqliteStore, snapshot_id: &str) -> Result<(), StoreError> {
    let workspace = workspace();
    let (nodes, edges, evidence) = fixture();
    store.publish_snapshot(SnapshotBatch {
        workspace: &workspace,
        snapshot_id,
        nodes: &nodes,
        edges: &edges,
        evidence: &evidence,
        fingerprints: &[],
        extractor_batches: &[],
        extractor_runs: &[],
        manual_links: &[],
        community_snapshot: None,
    })
}

fn seeded_store(path: &Path, snapshot_id: &str) -> Result<SqliteStore, StoreError> {
    let mut store = SqliteStore::open(path)?;
    seed_snapshot(&mut store, snapshot_id)?;
    Ok(store)
}

fn seed_backup(database: &Path, backup: &Path, snapshot_id: &str) -> Result<(), StoreError> {
    seeded_store(database, snapshot_id)?.backup_to(backup)
}

#[test]
fn online_backup_should_preserve_current_snapshot() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("source.db");
    let backup = temporary.path().join("backup.db");
    seeded_store(&database, "snapshot:1")?.backup_to(&backup)?;
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

#[cfg(unix)]
#[test]
fn distinct_paths_should_detect_missing_files_through_symlinked_parent()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir()?;
    let real_parent = temporary.path().join("real");
    let alias_parent = temporary.path().join("alias");
    fs::create_dir(&real_parent)?;
    symlink(&real_parent, &alias_parent)?;

    let result = ensure_distinct_paths(
        &real_parent.join("store.db"),
        &alias_parent.join("store.db"),
    );

    assert!(matches!(result, Err(StoreError::InvalidBackup { .. })));
    Ok(())
}

#[test]
fn backup_file_should_allow_retry_after_failed_source_validation()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("source.db");
    let destination = temporary.path().join("backup.db");
    drop(seeded_store(&source, "snapshot:0")?);

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

#[test]
fn restore_should_not_overwrite_destination_created_during_staging()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("source.db");
    let backup = temporary.path().join("backup.db");
    let destination = temporary.path().join("restored.db");
    seed_backup(&source, &backup, "snapshot:0")?;

    let result = restore_database(
        &destination,
        &backup,
        || {},
        |_| {},
        |_| {
            fs::write(&destination, b"foreign-destination").map_err(|source| StoreError::Io {
                path: destination.clone(),
                source,
            })
        },
        || Ok(()),
    );

    assert!(result.is_err());
    assert_eq!(fs::read(&destination)?, b"foreign-destination");
    Ok(())
}

#[cfg(unix)]
#[test]
fn backup_should_not_remove_preexisting_dangling_symlink() -> Result<(), Box<dyn std::error::Error>>
{
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("source.db");
    let destination = temporary.path().join("backup.db");
    drop(seeded_store(&source, "snapshot:0")?);
    symlink(temporary.path().join("missing-target"), &destination)?;

    let result = SqliteStore::backup_file(&source, &destination);

    assert!(result.is_err());
    assert!(
        fs::symlink_metadata(&destination)?.file_type().is_symlink(),
        "failed backup must preserve a destination it did not create"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn staging_directory_should_be_owner_only_before_sqlite_writes()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir()?;
    let destination = fs::canonicalize(temporary.path())?.join("backup.db");

    let (staging, connection) = StagedDatabase::create(&destination)?;
    let mode = fs::metadata(staging.directory.path())?.permissions().mode() & 0o777;

    assert_eq!(mode, 0o700);
    drop(connection);
    drop(staging);
    Ok(())
}

#[test]
fn restore_should_preserve_replaced_database_as_safety_backup()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("selected-backup.db");
    {
        let mut store = seeded_store(&database, "snapshot:before")?;
        store.backup_to(&backup)?;
        seed_snapshot(&mut store, "snapshot:after")?;
    }

    let report = SqliteStore::restore_from(&database, &backup)?;
    let restored = SqliteStore::open_read_only(&database)?.current_snapshot_summary("commerce")?;
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
fn restore_should_refuse_while_read_only_store_is_open() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("selected-backup.db");
    {
        let mut store = seeded_store(&database, "snapshot:before")?;
        store.backup_to(&backup)?;
        seed_snapshot(&mut store, "snapshot:after")?;
    }
    let stale = SqliteStore::open_read_only(&database)?;

    let blocked = SqliteStore::restore_from(&database, &backup);
    let stale_snapshot = stale.current_snapshot_summary("commerce")?;
    drop(stale);
    let report = SqliteStore::restore_from(&database, &backup)?;
    let restored = SqliteStore::open_read_only(&database)?.current_snapshot_summary("commerce")?;

    assert!(matches!(blocked, Err(StoreError::LockHeld(_))));
    assert_eq!(stale_snapshot.snapshot_id, "snapshot:after");
    assert_eq!(restored.snapshot_id, "snapshot:before");
    assert!(report.safety_backup_path.is_some());
    Ok(())
}

#[test]
fn open_should_reject_hard_link_aliases() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let alias = temporary.path().join("store-alias.db");
    let backup = temporary.path().join("selected-backup.db");
    {
        let mut store = seeded_store(&database, "snapshot:before")?;
        store.backup_to(&backup)?;
        seed_snapshot(&mut store, "snapshot:after")?;
    }
    fs::hard_link(&database, &alias)?;
    let aliased = SqliteStore::open_read_only(&alias);
    fs::remove_file(&alias)?;
    let restored = SqliteStore::restore_from(&database, &backup)?;

    assert!(matches!(
        aliased,
        Err(StoreError::Io { source, .. })
            if source.kind() == std::io::ErrorKind::InvalidInput
    ));
    assert!(restored.safety_backup_path.is_some());
    Ok(())
}

#[test]
fn restore_should_replace_invalid_schema_and_preserve_it_as_safety_backup()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("selected-backup.db");
    {
        let mut store = seeded_store(&database, "snapshot:before")?;
        store.backup_to(&backup)?;
        seed_snapshot(&mut store, "snapshot:damaged")?;
    }
    Connection::open(&database)?
        .execute_batch("DROP TRIGGER query_cache_bound_workspace_entries")?;

    let report = SqliteStore::restore_from(&database, &backup)?;
    let restored = SqliteStore::open_read_only(&database)?.current_snapshot_summary("commerce")?;
    let safety_path = report
        .safety_backup_path
        .ok_or_else(|| std::io::Error::other("restore safety backup missing"))?;
    let preserved_trigger_count = Connection::open(safety_path)?.query_row(
        "SELECT count(*) FROM sqlite_schema
         WHERE type = 'trigger' AND name = 'query_cache_bound_workspace_entries'",
        [],
        |row| row.get::<_, i64>(0),
    )?;

    assert_eq!(
        (restored.snapshot_id.as_str(), preserved_trigger_count),
        ("snapshot:before", 0)
    );
    Ok(())
}

#[test]
fn restore_should_replace_page_corruption_and_preserve_a_safety_copy()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("selected-backup.db");
    seed_backup(&database, &backup, "snapshot:0")?;
    let mut damaged = fs::read(&database)?;
    damaged.truncate(damaged.len() / 2);
    fs::write(&database, damaged)?;

    let report = SqliteStore::restore_from(&database, &backup)?;
    let restored = SqliteStore::open_read_only(&database)?.current_snapshot_summary("commerce")?;

    assert_eq!(restored.snapshot_id, "snapshot:0");
    assert!(report.safety_backup_path.is_some());
    Ok(())
}

#[test]
fn corrupt_restore_should_reject_sidecars_created_before_publication()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("selected-backup.db");
    let wal = temporary.path().join("store.db-wal");
    seed_backup(&database, &backup, "snapshot:0")?;
    let mut damaged = fs::read(&database)?;
    damaged.truncate(damaged.len() / 2);
    fs::write(&database, &damaged)?;

    let result = restore_database(
        &database,
        &backup,
        || {},
        |_| {},
        |_| Ok(()),
        || {
            fs::write(&wal, b"uncheckpointed-pages").map_err(|source| StoreError::Io {
                path: wal.clone(),
                source,
            })
        },
    );

    assert!(result.is_err());
    assert_eq!(fs::read(&database)?, damaged);
    assert_eq!(fs::read(&wal)?, b"uncheckpointed-pages");
    Ok(())
}

#[cfg(any(unix, windows))]
#[test]
fn restore_should_lock_destination_before_safety_backup_is_reported()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("backup.db");
    seed_backup(&database, &backup, "snapshot:0")?;

    let competing_writer_was_blocked = Cell::new(false);
    restore_database(
        &database,
        &backup,
        || {},
        |_| {
            let contender = Connection::open(&database).expect("open competing writer");
            contender
                .busy_timeout(Duration::ZERO)
                .expect("configure competing writer");
            let write = contender.execute_batch("BEGIN IMMEDIATE");
            competing_writer_was_blocked.set(write.is_err());
        },
        |_| Ok(()),
        || Ok(()),
    )?;

    assert!(
        competing_writer_was_blocked.get(),
        "another SQLite writer must be blocked before the safety backup is exposed"
    );
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn restore_should_keep_destination_locked_through_publication()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("backup.db");
    seed_backup(&database, &backup, "snapshot:0")?;

    let competing_writer_was_blocked = Cell::new(false);
    restore_database(
        &database,
        &backup,
        || {},
        |_| {},
        |_| Ok(()),
        || {
            let contender = Connection::open(&database)?;
            contender.busy_timeout(Duration::ZERO)?;
            let write = contender.execute_batch("BEGIN IMMEDIATE");
            competing_writer_was_blocked.set(write.is_err());
            Ok(())
        },
    )?;

    assert!(
        competing_writer_was_blocked.get(),
        "another SQLite writer must remain blocked through publication"
    );
    Ok(())
}

#[cfg(windows)]
#[test]
fn restore_should_reject_destination_changes_after_lock_release()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("backup.db");
    seed_backup(&database, &backup, "snapshot:0")?;

    let result = restore_database(
        &database,
        &backup,
        || {},
        |_| {},
        |_| Ok(()),
        || {
            let connection = Connection::open(&database)?;
            let journal_mode =
                connection.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))?;
            assert_eq!(journal_mode, "delete");
            connection.pragma_update(None, "user_version", 73)?;
            let user_version =
                connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
            assert_eq!(user_version, 73);
            drop(connection);
            let persisted =
                Connection::open(&database)?
                    .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
            assert_eq!(persisted, 73);
            Ok(())
        },
    );
    let user_version =
        Connection::open(&database)?
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;

    assert!(result.is_err());
    assert_eq!(user_version, 73);
    Ok(())
}

#[test]
fn restore_should_reject_missing_backup_that_collides_with_default_safety_path()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("store.db.pre-restore.backup");
    drop(seeded_store(&database, "snapshot:before")?);
    assert!(!backup.exists());

    let result = SqliteStore::restore_from(&database, &backup);

    assert!(result.is_err());
    let unchanged = SqliteStore::open_read_only(&database)?.current_snapshot_summary("commerce")?;
    assert_eq!(unchanged.snapshot_id, "snapshot:before");
    assert!(
        !backup.exists(),
        "missing input backup must not be materialized as a safety backup"
    );
    Ok(())
}

#[test]
fn backup_validation_should_use_read_only_untrusted_schema_connection()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("backup.db");
    seed_backup(&database, &backup, "snapshot:0")?;

    let connection = open_backup_source(&backup)?;
    let query_only = connection.query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))?;
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

#[cfg(unix)]
#[test]
fn backup_validation_should_allow_symlinked_parent_components()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir()?;
    let real = temporary.path().join("real");
    let alias = temporary.path().join("alias");
    fs::create_dir(&real)?;
    symlink(&real, &alias)?;
    let database = real.join("store.db");
    let backup = real.join("backup.db");
    seed_backup(&database, &backup, "snapshot:0")?;

    let connection = open_backup_source(&alias.join("backup.db"))?;
    let query_only = connection.query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))?;

    assert_eq!(query_only, 1);
    Ok(())
}

#[test]
fn restore_should_reject_backup_with_extra_trigger() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("backup.db");
    seed_backup(&database, &backup, "snapshot:0")?;
    let connection = Connection::open(&backup)?;
    connection.execute(
        "CREATE TRIGGER injected AFTER INSERT ON workspaces BEGIN SELECT 1; END",
        [],
    )?;

    let result = SqliteStore::restore_from(&database, &backup);

    assert!(matches!(result, Err(StoreError::InvalidBackup { .. })));
    let unchanged = SqliteStore::open_read_only(&database)?.current_snapshot_summary("commerce")?;
    assert_eq!(unchanged.snapshot_id, "snapshot:0");
    Ok(())
}

#[cfg(unix)]
#[test]
fn restore_should_not_replace_destination_swapped_during_staging()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("backup.db");
    seed_backup(&database, &backup, "snapshot:0")?;

    let result = restore_database(
        &database,
        &backup,
        || {},
        |_| {},
        |_| {
            fs::remove_file(&database).map_err(|source| StoreError::Io {
                path: database.clone(),
                source,
            })?;
            fs::write(&database, b"foreign-destination").map_err(|source| StoreError::Io {
                path: database.clone(),
                source,
            })
        },
        || Ok(()),
    );

    assert!(result.is_err());
    assert_eq!(fs::read(&database)?, b"foreign-destination");
    Ok(())
}

#[test]
fn failed_restore_should_preserve_original_without_publication()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("backup.db");
    let expected_safety = temporary.path().join("store.db.pre-restore.backup");
    {
        let mut store = seeded_store(&database, "snapshot:before")?;
        store.backup_to(&backup)?;
        seed_snapshot(&mut store, "snapshot:after")?;
    }

    let result = restore_database(
        &database,
        &backup,
        || {},
        |_| {},
        |_| {
            Err(StoreError::InvalidBackup {
                path: database.clone(),
                reason: "injected post-copy failure".to_owned(),
            })
        },
        || Ok(()),
    );

    assert!(matches!(result, Err(StoreError::InvalidBackup { .. })));
    let restored = SqliteStore::open_read_only(&database)?.current_snapshot_summary("commerce")?;
    assert_eq!(restored.snapshot_id, "snapshot:after");
    assert!(
        expected_safety.exists(),
        "failed restore must retain the independently published safety backup"
    );
    Ok(())
}

#[test]
fn restore_should_reject_backup_with_foreign_key_violations()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("backup.db");
    seed_backup(&database, &backup, "snapshot:0")?;
    let connection = Connection::open(&backup)?;
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
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("backup.db");
    let restore_target = temporary.path().join("restored.db");
    seed_backup(&database, &backup, "snapshot:0")?;

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
        restore_database(
            &restore_target_for_restorer,
            &backup,
            || {
                let (lock, cv) = &*coordination_for_restorer;
                let mut ready = lock.lock().expect("coordination lock");
                *ready = true;
                cv.notify_one();
                drop(ready);
                std::thread::sleep(Duration::from_millis(50));
            },
            |_| {},
            |_| Ok(()),
            || Ok(()),
        )?;
        Ok(())
    });

    corruptor.join().expect("corruptor thread panicked")?;
    restorer.join().expect("restorer thread panicked")?;

    let store = SqliteStore::open_read_only(&restore_target)?;
    assert!(store.integrity_check()?);
    Ok(())
}

#[test]
fn restore_should_reject_backup_with_altered_trigger_sql() -> Result<(), Box<dyn std::error::Error>>
{
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("backup.db");
    seed_backup(&database, &backup, "snapshot:0")?;
    let connection = Connection::open(&backup)?;
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
fn restore_should_reject_backup_with_missing_required_table()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("backup.db");
    seed_backup(&database, &backup, "snapshot:0")?;
    let connection = Connection::open(&backup)?;
    connection.execute("DROP TABLE workspaces", [])?;

    let result = SqliteStore::restore_from(&database, &backup);

    assert!(matches!(result, Err(StoreError::InvalidBackup { .. })));
    let unchanged = SqliteStore::open_read_only(&database)?.current_snapshot_summary("commerce")?;
    assert_eq!(unchanged.snapshot_id, "snapshot:0");
    Ok(())
}

#[test]
fn restore_should_reject_corrupt_backup() -> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("backup.db");
    seed_backup(&database, &backup, "snapshot:0")?;
    let mut bytes = fs::read(&backup)?;
    bytes.truncate(bytes.len() / 4);
    fs::write(&backup, bytes)?;

    let result = SqliteStore::restore_from(&database, &backup);

    assert!(
        matches!(
            result,
            Err(StoreError::InvalidBackup { .. } | StoreError::Io { .. } | StoreError::Sqlite(_))
        ),
        "unexpected restore result: {result:?}"
    );
    Ok(())
}

#[test]
fn restore_should_reject_backup_with_literal_whitespace_tampering()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("store.db");
    let backup = temporary.path().join("backup.db");
    seed_backup(&database, &backup, "snapshot:0")?;
    let original_sql: String = Connection::open(&backup)?.query_row(
        "SELECT sql FROM sqlite_schema
         WHERE name = 'provider_capabilities_require_registration'",
        [],
        |row| row.get(0),
    )?;
    let tampered_sql = original_sql.replace(
        "'provider capability repository is not registered in workspace'",
        "'provider\ncapability repository is not registered in workspace'",
    );
    let connection = Connection::open(&backup)?;
    connection.execute_batch(&format!(
        "DROP TRIGGER provider_capabilities_require_registration; {tampered_sql};"
    ))?;

    let result = SqliteStore::restore_from(&database, &backup);

    assert!(matches!(result, Err(StoreError::InvalidBackup { .. })));
    Ok(())
}

//! Acceptance tests for targeted and forced scan controls.

use code_system_graph::{
    ApplicationError, ScanOverrides, WatcherState, application_exit_code, finish_watcher_lease, scan_workspace, scan_workspace_with_overrides, scan_workspace_with_worker_executable, start_watcher_lease, status_workspace
};
use code_system_graph_core::ExitCode;
use code_system_graph_store_sqlite::SqliteStore;

fn openapi(path: &str) -> String {
    format!("openapi: 3.1.0\ninfo:\n  title: API\n  version: 1\npaths:\n  {path}:\n    get: {{}}\n")
}

#[test]
fn targeted_scan_should_reuse_unselected_repository_batches() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    for alias in ["a", "b"] {
        std::fs::create_dir(temporary.path().join(alias))?;
        std::fs::write(
            temporary.path().join(alias).join("openapi.yaml"),
            openapi(&format!("/{alias}/before")),
        )?;
    }
    std::fs::write(
        temporary.path().join("a").join("test_force.py"),
        "def test_force_reextracts_cached_source():\n    pass\n",
    )?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: targeted\nrepos:\n  a:\n    path: a\n    openapi: openapi.yaml\n  b:\n    path: b\n    openapi: openapi.yaml\n",
    )?;
    let database = temporary.path().join("graph.db");
    scan_workspace(&manifest, &database)?;
    let before =
        SqliteStore::open_read_only(&database)?.load_current_artifact_fingerprints("targeted")?;

    for alias in ["a", "b"] {
        std::fs::write(
            temporary.path().join(alias).join("openapi.yaml"),
            openapi(&format!("/{alias}/after")),
        )?;
    }
    scan_workspace_with_overrides(
        &manifest,
        &database,
        &ScanOverrides {
            repository: Some("a".to_owned()),
            ..ScanOverrides::default()
        },
    )?;
    let after =
        SqliteStore::open_read_only(&database)?.load_current_artifact_fingerprints("targeted")?;

    let unchanged = before
        .iter()
        .filter(|old| {
            after.iter().any(|new| {
                old.repo_id == new.repo_id
                    && old.extractor == new.extractor
                    && old.content_hash == new.content_hash
            })
        })
        .count();
    let changed = before.len().saturating_sub(unchanged);
    assert_eq!(changed, 1);
    assert!(unchanged >= 2);

    let connection = rusqlite::Connection::open(&database)?;
    let corrupted = connection.execute(
        "UPDATE extractor_batches
         SET output_count = 0, payload = CAST('[]' AS BLOB)
         WHERE snapshot_id = (
             SELECT id FROM repo_snapshots
             WHERE workspace_name = 'targeted' AND is_current = 1
         )
         AND path_display = 'test_force.py'
         AND extractor = 'code-system-graph.source.python'",
        [],
    )?;
    assert_eq!(corrupted, 1);
    drop(connection);

    let forced = scan_workspace_with_overrides(
        &manifest,
        &database,
        &ScanOverrides {
            repository: Some("a".to_owned()),
            force: true,
            ..ScanOverrides::default()
        },
    )?;
    assert!(forced.changed_input_count > 0);
    let batches =
        SqliteStore::open_read_only(&database)?.load_current_extractor_batches("targeted")?;
    let forced_source = batches
        .iter()
        .find(|batch| {
            batch.source.path.display == "test_force.py"
                && batch.source.extractor == "code-system-graph.source.python"
        })
        .expect("forced source batch should remain persisted");
    assert_eq!(forced_source.output_count, 1);
    Ok(())
}

#[test]
fn execution_policy_change_should_not_invalidate_batches_or_require_full_scan() -> anyhow::Result<()>
{
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir(temporary.path().join("api"))?;
    std::fs::write(
        temporary.path().join("api/openapi.yaml"),
        openapi("/orders"),
    )?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    let base = "version: 1\nname: operational-policy\nrepos:\n  api:\n    path: api\n    openapi: openapi.yaml\n";
    std::fs::write(&manifest, base)?;
    let database = temporary.path().join("graph.db");
    let initial = scan_workspace(&manifest, &database)?;

    std::fs::write(
        &manifest,
        "version: 1\nname: operational-policy\nexecutionPolicy:\n  maxScanWallTimeMs: 28800000\n  maxNoProgressTimeMs: 600000\nrepos:\n  api:\n    path: api\n    openapi: openapi.yaml\n",
    )?;
    let changed_policy = scan_workspace_with_overrides(
        &manifest,
        &database,
        &ScanOverrides {
            repository: Some("api".to_owned()),
            ..ScanOverrides::default()
        },
    )?;

    assert!(changed_policy.reused_snapshot);
    assert_eq!(changed_policy.snapshot_id, initial.snapshot_id);
    Ok(())
}

#[test]
fn status_should_report_persisted_finite_watcher_lifecycle() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir(temporary.path().join("api"))?;
    std::fs::write(
        temporary.path().join("api/openapi.yaml"),
        openapi("/orders"),
    )?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: watcher-state\nrepos:\n  api:\n    path: api\n    openapi: openapi.yaml\n",
    )?;
    let database = temporary.path().join("graph.db");
    scan_workspace(&manifest, &database)?;

    assert_eq!(
        status_workspace(&manifest, &database)?.watcher.state,
        WatcherState::NeverStarted
    );
    let (workspace, owner_token, _) = start_watcher_lease(&manifest, &database)?;
    assert_eq!(
        status_workspace(&manifest, &database)?.watcher.state,
        WatcherState::Active
    );
    assert!(matches!(
        start_watcher_lease(&manifest, &database),
        Err(ApplicationError::WatcherAlreadyActive(workspace)) if workspace == "watcher-state"
    ));
    finish_watcher_lease(
        &database,
        &workspace,
        &owner_token,
        "expired_idle",
        Some("idle deadline"),
    )?;
    assert_eq!(
        status_workspace(&manifest, &database)?.watcher.state,
        WatcherState::ExpiredIdle
    );
    Ok(())
}

#[test]
fn supervised_validation_failures_should_keep_invalid_input_classification() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("api");
    std::fs::create_dir(&repository)?;
    std::fs::write(repository.join("openapi.yaml"), openapi("/orders"))?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: typed-worker-errors\nrepos:\n  api:\n    path: api\n    openapi: openapi.yaml\n",
    )?;
    let database = temporary.path().join("graph.db");

    let errors = [
        scan_workspace_with_overrides(
            &manifest,
            &database,
            &ScanOverrides {
                repository: Some("missing".to_owned()),
                ..ScanOverrides::default()
            },
        )
        .expect_err("unknown repository"),
        scan_workspace_with_overrides(
            &manifest,
            &database,
            &ScanOverrides {
                workspace: Some("wrong".to_owned()),
                ..ScanOverrides::default()
            },
        )
        .expect_err("workspace mismatch"),
    ];
    for error in &errors {
        assert_eq!(application_exit_code(error), ExitCode::InvalidInput);
        assert!(matches!(
            error,
            ApplicationError::SupervisedApplication { .. }
        ));
    }

    std::fs::write(repository.join("openapi.yaml"), "openapi: [")?;
    let malformed = scan_workspace(&manifest, &database).expect_err("malformed OpenAPI");
    assert_eq!(application_exit_code(&malformed), ExitCode::InvalidInput);
    assert!(matches!(
        malformed,
        ApplicationError::SupervisedApplication { .. }
    ));
    Ok(())
}

#[test]
fn embedding_host_should_be_able_to_select_its_worker_executable() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir(temporary.path().join("api"))?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: explicit-worker\nrepos:\n  api:\n    path: api\n",
    )?;
    let summary = scan_workspace_with_worker_executable(
        &manifest,
        &temporary.path().join("graph.db"),
        &ScanOverrides::default(),
        std::path::Path::new(env!("CARGO_BIN_EXE_csgraph")),
    )?;

    assert_eq!(summary.workspace, "explicit-worker");
    Ok(())
}

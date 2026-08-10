//! Acceptance tests for targeted and forced scan controls.

#[cfg(unix)]
use std::fmt::Write as _;

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

#[cfg(unix)]
#[test]
fn targeted_codegraph_scan_should_not_schedule_unselected_repositories() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir()?;
    let mut manifest = String::from("version: 1\nname: focused-codegraph\nrepos:\n");
    for alias in ["a", "b"] {
        let repository = temporary.path().join(alias);
        std::fs::create_dir_all(repository.join("src"))?;
        std::fs::write(
            repository.join("openapi.yaml"),
            openapi(&format!("/{alias}")),
        )?;
        std::fs::write(
            repository.join("src/lib.rs"),
            format!(
                "use axum::{{Router, routing::get}};\npub fn router() -> Router {{ Router::new().route(\"/{alias}\", get(handler)) }}\nasync fn handler() {{}}\n"
            ),
        )?;
        write!(
            manifest,
            "  {alias}:\n    path: {alias}\n    openapi: openapi.yaml\n"
        )?;
    }
    let manifest_path = temporary.path().join("code-system-graph.yaml");
    let database = temporary.path().join("graph.db");
    std::fs::write(&manifest_path, manifest)?;
    scan_workspace(&manifest_path, &database)?;

    let invocation_log = temporary.path().join("codegraph.log");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/codegraph/fake/codegraph.py")
        .canonicalize()?;
    let binary = temporary.path().join("codegraph-focused");
    std::fs::write(
        &binary,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec python3 '{}' \"$@\"\n",
            invocation_log.display(),
            fixture.display()
        ),
    )?;
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(
        temporary.path().join("a/openapi.yaml"),
        openapi("/a/changed"),
    )?;

    let summary = scan_workspace_with_overrides(
        &manifest_path,
        &database,
        &ScanOverrides {
            codegraph: true,
            codegraph_binary: Some(binary),
            repository: Some("a".to_owned()),
            ..ScanOverrides::default()
        },
    )?;
    let invocations = std::fs::read_to_string(invocation_log)?;

    assert!(summary.changed_input_count > 0);
    assert!(invocations.contains(temporary.path().join("a").to_string_lossy().as_ref()));
    assert!(!invocations.contains(temporary.path().join("b").to_string_lossy().as_ref()));
    Ok(())
}

#[test]
fn duplicate_http_providers_should_degrade_without_aborting_unrelated_links() -> anyhow::Result<()>
{
    let temporary = tempfile::tempdir()?;
    for alias in ["api-a", "api-b", "web"] {
        std::fs::create_dir(temporary.path().join(alias))?;
    }
    std::fs::write(
        temporary.path().join("api-a/openapi.yaml"),
        "openapi: 3.1.0\ninfo: { title: A, version: 1 }\npaths:\n  /orders:\n    get: {}\n  /health:\n    get: {}\n",
    )?;
    std::fs::write(
        temporary.path().join("api-b/openapi.yaml"),
        openapi("/orders"),
    )?;
    for source in ["orders.ts", "health.ts"] {
        std::fs::write(temporary.path().join("web").join(source), "export {};\n")?;
    }
    let base_manifest = "version: 1\nname: http-ambiguity\nrepos:\n  api-a:\n    path: api-a\n    openapi: openapi.yaml\n  api-b:\n    path: api-b\n    openapi: openapi.yaml\n  web:\n    path: web\n    httpConsumers:\n      - method: GET\n        path: /orders\n        source: orders.ts\n      - method: GET\n        path: /health\n        source: health.ts\n";
    let manifest = temporary.path().join("code-system-graph.yaml");
    let database = temporary.path().join("graph.db");
    std::fs::write(&manifest, base_manifest)?;

    let summary = scan_workspace(&manifest, &database)?;
    let store = SqliteStore::open_read_only(&database)?;
    let (nodes, edges) = store.load_current_graph("http-ambiguity")?;
    let automatic_calls = edges
        .iter()
        .filter(|edge| edge.kind == code_system_graph_model::EdgeKind::CallsRemote)
        .count();
    assert_eq!(automatic_calls, 1);
    assert!(
        summary
            .degradations
            .iter()
            .any(|item| item.contains("ambiguous HTTP provider for GET /orders"))
    );

    let consumer = nodes
        .iter()
        .find(|node| node.stable_key.contains(":consumer:GET:/orders"))
        .expect("orders consumer node");
    let registry = store.load_workspace_registry("http-ambiguity")?;
    let api_a = registry
        .repositories
        .iter()
        .find(|repository| repository.alias == "api-a")
        .expect("api-a registry entry");
    let provider = nodes
        .iter()
        .find(|node| {
            node.repo_id.as_ref() == Some(&api_a.id)
                && node.stable_key.contains(":provider:GET:/orders")
        })
        .expect("api-a orders provider");
    let resolved_manifest = format!(
        "{base_manifest}manualLinks:\n  - from: {}\n    to: {}\n    relation: calls_remote\n    contract: GET /orders\n    reason: Select the authoritative orders provider\n",
        consumer.id.as_str(),
        provider.id.as_str()
    );
    drop(store);
    std::fs::write(&manifest, resolved_manifest)?;

    scan_workspace(&manifest, &database)?;
    let (_, resolved_edges) =
        SqliteStore::open_read_only(&database)?.load_current_graph("http-ambiguity")?;
    assert_eq!(
        resolved_edges
            .iter()
            .filter(|edge| edge.kind == code_system_graph_model::EdgeKind::CallsRemote)
            .count(),
        2
    );
    Ok(())
}

#[test]
fn scan_execution_policy_change_should_publish_a_new_snapshot() -> anyhow::Result<()> {
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

    assert!(!changed_policy.reused_snapshot);
    assert_ne!(changed_policy.snapshot_id, initial.snapshot_id);

    std::fs::write(
        &manifest,
        "version: 1\nname: operational-policy\nexecutionPolicy:\n  maxScanWallTimeMs: 28800000\n  maxNoProgressTimeMs: 600000\n  maxMcpToolResponseBytes: 600000\nrepos:\n  api:\n    path: api\n    openapi: openapi.yaml\n",
    )?;
    let changed_delivery = scan_workspace(&manifest, &database)?;
    assert!(changed_delivery.reused_snapshot);
    assert_eq!(changed_delivery.snapshot_id, changed_policy.snapshot_id);
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
    assert!(errors[0].to_string().contains("missing"));
    assert!(errors[1].to_string().contains("wrong"));
    assert!(errors[1].to_string().contains("typed-worker-errors"));

    std::fs::write(repository.join("openapi.yaml"), "openapi: [")?;
    let malformed = scan_workspace(&manifest, &database).expect_err("malformed OpenAPI");
    assert_eq!(application_exit_code(&malformed), ExitCode::InvalidInput);
    assert!(matches!(
        malformed,
        ApplicationError::SupervisedApplication { .. }
    ));
    assert!(malformed.to_string().contains("invalid OpenAPI document"));
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

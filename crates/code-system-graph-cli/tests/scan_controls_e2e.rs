//! Acceptance tests for targeted and forced scan controls.

use code_system_graph::{ScanOverrides, scan_workspace, scan_workspace_with_overrides};
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

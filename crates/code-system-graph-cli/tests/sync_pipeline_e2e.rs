//! Acceptance tests for the single-read, stat-cached, parallel extraction pipeline.

use std::path::Path;
use std::time::{Duration, SystemTime};

use code_system_graph::{ScanOverrides, scan_workspace, scan_workspace_with_overrides};
use code_system_graph_store_sqlite::SqliteStore;

const SOURCES: [(&str, &str); 6] = [
    (
        "api/src/server.ts",
        "import express from 'express';\nconst app = express();\napp.get('/v1/orders/:id', (req, res) => res.json({}));\napp.post('/v1/orders', (req, res) => res.json({}));\n",
    ),
    (
        "api/src/publisher.ts",
        "export async function announce(producer) {\n  await producer.send({ topic: 'orders.created', messages: [] });\n}\n",
    ),
    (
        "api/src/db.py",
        "def load(cursor):\n    cursor.execute(\"SELECT id FROM orders WHERE id = %s\", (1,))\n",
    ),
    (
        "tests/tests/test_orders.py",
        "import requests\n\ndef test_create_order():\n    requests.post(\"http://localhost:8080/v1/orders\", json={})\n",
    ),
    (
        "tests/tests/test_lookup.py",
        "import requests\n\ndef test_lookup_order():\n    requests.get(\"http://localhost:8080/v1/orders/42\")\n",
    ),
    ("api/README.md", "# Orders API\n\nServes `/v1/orders`.\n"),
];

fn write_workspace(root: &Path, workers: u64) -> anyhow::Result<std::path::PathBuf> {
    for (path, contents) in SOURCES {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents)?;
    }
    let manifest = root.join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        format!(
            "version: 1\nname: pipeline\nexecutionPolicy:\n  maxExtractionWorkers: {workers}\nrepos:\n  api:\n    path: api\n  tests:\n    path: tests\n"
        ),
    )?;
    Ok(manifest)
}

fn age_sources(root: &Path, age: Duration) -> anyhow::Result<()> {
    let modified = SystemTime::now() - age;
    for (path, _) in SOURCES {
        std::fs::File::options()
            .write(true)
            .open(root.join(path))?
            .set_modified(modified)?;
    }
    Ok(())
}

#[test]
fn published_graph_should_not_depend_on_extraction_worker_count() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let mut graphs = Vec::new();
    for workers in [1, 8] {
        let manifest = write_workspace(temporary.path(), workers)?;
        let database = temporary.path().join(format!("graph-{workers}.db"));
        let summary = scan_workspace(&manifest, &database)?;
        assert_eq!(
            summary.execution.extraction_workers,
            workers.min(
                std::thread::available_parallelism()
                    .map_or(1, std::num::NonZeroUsize::get)
                    .try_into()?
            )
        );
        let (nodes, edges) =
            SqliteStore::open_read_only(&database)?.load_current_graph("pipeline")?;
        assert_ne!(nodes, Vec::new());
        graphs.push((nodes, edges));
    }

    assert_eq!(graphs[0], graphs[1]);
    Ok(())
}

#[test]
fn unchanged_files_should_be_fingerprinted_from_the_stat_cache() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let manifest = write_workspace(temporary.path(), 4)?;
    let database = temporary.path().join("graph.db");
    age_sources(temporary.path(), Duration::from_mins(10))?;

    let initial = scan_workspace(&manifest, &database)?;
    let unchanged = scan_workspace(&manifest, &database)?;

    assert_eq!(initial.execution.stat_cache_hits, 0);
    assert!(unchanged.reused_snapshot);
    assert_eq!(unchanged.snapshot_id, initial.snapshot_id);
    assert!(unchanged.execution.stat_cache_hits >= u64::try_from(SOURCES.len())?);
    assert!(
        unchanged
            .execution
            .phases
            .iter()
            .any(|phase| phase.phase == code_system_graph_core::JobPhase::Fingerprinting)
    );

    let edited = temporary.path().join("api/src/server.ts");
    let original = std::fs::read_to_string(&edited)?;
    std::fs::write(
        &edited,
        original.replace("/v1/orders/:id", "/v1/orders/:oid"),
    )?;
    std::fs::File::options()
        .write(true)
        .open(&edited)?
        .set_modified(SystemTime::now() - Duration::from_mins(5))?;
    let changed = scan_workspace(&manifest, &database)?;

    assert!(changed.changed_input_count > 0);
    assert_ne!(changed.snapshot_id, initial.snapshot_id);
    Ok(())
}

#[test]
fn recently_modified_files_should_always_be_read() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let manifest = write_workspace(temporary.path(), 2)?;
    let database = temporary.path().join("graph.db");

    scan_workspace(&manifest, &database)?;
    let second = scan_workspace(&manifest, &database)?;

    assert_eq!(second.execution.stat_cache_hits, 0);
    assert!(second.reused_snapshot);
    Ok(())
}

#[test]
fn touched_repository_scans_should_converge_with_a_full_scan() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let manifest = write_workspace(temporary.path(), 2)?;
    let database = temporary.path().join("graph.db");
    scan_workspace(&manifest, &database)?;

    let publisher = temporary.path().join("api/src/publisher.ts");
    std::fs::write(
        &publisher,
        std::fs::read_to_string(&publisher)?.replace("orders.created", "orders.updated"),
    )?;
    let test = temporary.path().join("tests/tests/test_orders.py");
    std::fs::write(
        &test,
        std::fs::read_to_string(&test)?.replace("test_create_order", "test_place_order"),
    )?;
    let targeted = scan_workspace_with_overrides(
        &manifest,
        &database,
        &ScanOverrides {
            touched_repositories: vec!["api".to_owned()],
            ..ScanOverrides::default()
        },
    )?;
    let (nodes, _) = SqliteStore::open_read_only(&database)?.load_current_graph("pipeline")?;
    let graph_json = serde_json::to_string(&nodes)?;

    assert!(!targeted.reused_snapshot);
    assert!(graph_json.contains("orders.updated"));
    assert!(graph_json.contains("test_create_order"));
    assert!(!graph_json.contains("test_place_order"));

    let full = scan_workspace(&manifest, &database)?;
    let fresh_database = temporary.path().join("fresh.db");
    scan_workspace(&manifest, &fresh_database)?;

    assert!(!full.reused_snapshot);
    assert_eq!(
        SqliteStore::open_read_only(&database)?.load_current_graph("pipeline")?,
        SqliteStore::open_read_only(&fresh_database)?.load_current_graph("pipeline")?
    );
    Ok(())
}

//! Release-mode registry and incremental-scan acceptance at 100 repositories.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use code_system_graph::{
    ScanOverrides, scan_workspace, scan_workspace_with_overrides, status_workspace
};
use code_system_graph_store_sqlite::SqliteStore;

const REPOSITORY_COUNT: usize = 100;
const LARGE_REPOSITORY_COUNT: usize = 32;
const LARGE_FILES_PER_REPOSITORY: usize = 3_125;

#[test]
#[ignore = "run explicitly with --release for the scale acceptance workload"]
fn hundred_repository_registry_should_scan_incrementally() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let mut manifest = String::from("version: 1\nname: scale-registry\nrepos:\n");
    for index in 0..REPOSITORY_COUNT {
        let alias = format!("repo-{index:03}");
        let repository = temporary.path().join(&alias);
        std::fs::create_dir(&repository)?;
        std::fs::write(
            repository.join("Cargo.toml"),
            format!(
                "[package]\nname = \"scale-{index:03}\"\nversion = \"1.0.0\"\nedition = \"2024\"\n"
            ),
        )?;
        std::fs::create_dir(repository.join("src"))?;
        std::fs::write(
            repository.join("src/lib.rs"),
            format!("pub fn boundary_{index:03}() {{}}\n"),
        )?;
        writeln!(manifest, "  {alias}:\n    path: {alias}")?;
    }
    let manifest_path = temporary.path().join("code-system-graph.yaml");
    let database = temporary.path().join("code-system-graph.db");
    std::fs::write(&manifest_path, manifest)?;

    let initial = scan_workspace(&manifest_path, &database)?;
    let before = SqliteStore::open_read_only(&database)?
        .load_current_artifact_fingerprints("scale-registry")?;
    assert!(initial.discovered_input_count >= REPOSITORY_COUNT);

    let changed = temporary.path().join("repo-042/src/lib.rs");
    std::fs::write(&changed, "pub fn boundary_042_changed() {}\n")?;
    let started = Instant::now();
    let incremental = scan_workspace_with_overrides(
        &manifest_path,
        &database,
        &ScanOverrides {
            repository: Some("repo-042".to_owned()),
            ..ScanOverrides::default()
        },
    )?;
    let incremental_elapsed = started.elapsed();
    let after = SqliteStore::open_read_only(&database)?
        .load_current_artifact_fingerprints("scale-registry")?;
    let unchanged = before
        .iter()
        .filter(|old| {
            after.iter().any(|new| {
                old.repo_id == new.repo_id
                    && old.extractor == new.extractor
                    && old.path == new.path
                    && old.content_hash == new.content_hash
            })
        })
        .count();
    let unchanged_repositories = before
        .iter()
        .filter(|old| {
            after.iter().any(|new| {
                old.repo_id == new.repo_id
                    && old.extractor == new.extractor
                    && old.path == new.path
                    && old.content_hash == new.content_hash
            })
        })
        .map(|fingerprint| fingerprint.repo_id.clone())
        .collect::<BTreeSet<_>>();
    assert!(incremental.changed_input_count > 0);
    assert!(unchanged_repositories.len() >= REPOSITORY_COUNT - 1);

    let status_p95 = percentile_95((0..10).map(|_| {
        let started = Instant::now();
        let status = status_workspace(&manifest_path, &database);
        assert!(status.is_ok(), "scale status failed: {status:?}");
        started.elapsed()
    }));
    eprintln!(
        "registry_scale repos={REPOSITORY_COUNT} initial_inputs={} incremental_ms={} \
         changed_inputs={} unchanged_inputs={} unchanged_repos={} status_p95_ms={}",
        initial.changed_input_count,
        incremental_elapsed.as_millis(),
        incremental.changed_input_count,
        unchanged,
        unchanged_repositories.len(),
        status_p95.as_millis()
    );
    assert!(
        status_p95 < Duration::from_millis(200),
        "status p95 was {status_p95:?}"
    );
    Ok(())
}

#[test]
#[ignore = "run explicitly with --release for the 100,000-file execution-policy acceptance workload"]
fn interconnected_100k_file_workspace_should_complete_under_finite_defaults() -> anyhow::Result<()>
{
    let temporary = tempfile::tempdir()?;
    let mut manifest = String::from("version: 1\nname: large-workspace\nrepos:\n");
    for repository_index in 0..LARGE_REPOSITORY_COUNT {
        let alias = format!("repo-{repository_index:02}");
        let next = (repository_index + 1) % LARGE_REPOSITORY_COUNT;
        let repository = temporary.path().join(&alias);
        std::fs::create_dir_all(repository.join("contracts"))?;
        std::fs::create_dir_all(repository.join("src"))?;
        std::fs::write(
            repository.join("contracts/openapi.yaml"),
            format!(
                "openapi: 3.1.0\ninfo:\n  title: {alias}\n  version: 1\npaths:\n  /edge-{repository_index:02}:\n    get: {{}}\n"
            ),
        )?;
        std::fs::write(
            repository.join("src/client.ts"),
            format!("export const next = () => fetch('/edge-{next:02}');\n"),
        )?;
        for directory_index in 0..25 {
            let directory = repository.join(format!("corpus-{directory_index:02}"));
            std::fs::create_dir(&directory)?;
            for file_index in 0..125 {
                std::fs::write(
                    directory.join(format!("artifact-{file_index:03}.txt")),
                    format!("repository={repository_index};artifact={file_index}\n"),
                )?;
            }
        }
        writeln!(
            manifest,
            "  {alias}:\n    path: {alias}\n    openapi: contracts/openapi.yaml\n    httpConsumers:\n      - method: GET\n        path: /edge-{next:02}\n        source: src/client.ts"
        )?;
    }
    let manifest_path = temporary.path().join("code-system-graph.yaml");
    let database = temporary.path().join("code-system-graph.db");
    std::fs::write(&manifest_path, manifest)?;

    let started = Instant::now();
    let initial = scan_workspace(&manifest_path, &database)?;
    let elapsed = started.elapsed();
    assert_eq!(LARGE_REPOSITORY_COUNT * LARGE_FILES_PER_REPOSITORY, 100_000);
    assert!(elapsed < Duration::from_hours(6));
    assert!(initial.edge_count >= LARGE_REPOSITORY_COUNT);

    std::fs::write(
        temporary.path().join("repo-00/src/client.ts"),
        "export const next = () => fetch('/edge-01?revision=2');\n",
    )?;
    let incremental = scan_workspace_with_overrides(
        &manifest_path,
        &database,
        &ScanOverrides {
            repository: Some("repo-00".to_owned()),
            ..ScanOverrides::default()
        },
    )?;
    assert!(incremental.changed_input_count > 0);

    let mut work_path = database.as_os_str().to_os_string();
    work_path.push(".work-v1.db");
    let work_path = std::path::PathBuf::from(work_path);
    let work_size = std::fs::metadata(&work_path)?.len();
    let cache_bytes: u64 = rusqlite::Connection::open(&work_path)?
        .query_row(
            "SELECT COALESCE(SUM(size_bytes), 0) FROM batch_cache",
            [],
            |row| row.get::<_, i64>(0),
        )?
        .try_into()?;
    eprintln!(
        "workspace_scale repos={LARGE_REPOSITORY_COUNT} files=100000 elapsed_ms={} \
         rss_bytes={} artifact_p50_ms={} artifact_p95_ms={} artifact_p99_ms={} \
         staging_bytes={work_size} cache_bytes={cache_bytes} incremental_cache_hits={}",
        elapsed.as_millis(),
        initial.execution.peak_worker_memory_bytes,
        initial.execution.artifact_duration_p50_ms,
        initial.execution.artifact_duration_p95_ms,
        initial.execution.artifact_duration_p99_ms,
        incremental.execution.checkpoint_hits,
    );
    assert!(cache_bytes < 10_737_418_240);
    Ok(())
}

fn percentile_95(samples: impl Iterator<Item = Duration>) -> Duration {
    let mut values = samples.collect::<Vec<_>>();
    values.sort_unstable();
    let index = values
        .len()
        .saturating_mul(95)
        .div_ceil(100)
        .saturating_sub(1);
    values[index]
}

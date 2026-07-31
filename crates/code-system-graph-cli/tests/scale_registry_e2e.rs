//! Release-mode registry and incremental-scan acceptance at 100 repositories.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use code_system_graph::{
    ScanOverrides, scan_workspace, scan_workspace_with_overrides, status_workspace
};
use code_system_graph_store_sqlite::SqliteStore;

const REPOSITORY_COUNT: usize = 100;

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

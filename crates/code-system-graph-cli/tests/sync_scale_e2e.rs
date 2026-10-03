//! Release-mode sync acceptance at 200 repositories and 50,000 files: content reads, publication
//! writes, and database growth across repeated syncs.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use code_system_graph::scan_workspace;
use code_system_graph_model::RepoId;
use code_system_graph_store_sqlite::SqliteStore;

const WORKSPACE: &str = "sync-scale";
const REPOSITORY_COUNT: usize = 200;
const FILES_PER_REPOSITORY: usize = 250;
const SYNC_ROUNDS: u64 = 20;
const CHANGED_REPOSITORY: &str = "repo-042";
/// Directory where the workload is generated instead of a temporary one, so another build can
/// scan the same files.
const WORKLOAD_DIRECTORY: &str = "CODE_SYSTEM_GRAPH_SCALE_WORKLOAD";

#[test]
#[ignore = "run explicitly with --release for the 200-repository, 50,000-file sync workload"]
fn two_hundred_repository_sync_should_meet_read_write_and_growth_gates() -> anyhow::Result<()> {
    if cfg!(debug_assertions) {
        anyhow::bail!("run this acceptance test with --release");
    }
    let temporary = tempfile::tempdir()?;
    let root = std::env::var_os(WORKLOAD_DIRECTORY)
        .map_or_else(|| temporary.path().join("workload"), PathBuf::from);
    let manifest = write_workload(&root)?;
    let database = temporary.path().join("graph.db");

    let started = Instant::now();
    let cold = scan_workspace(&manifest, &database)?;
    let cold_elapsed = started.elapsed();

    let started = Instant::now();
    let unchanged = scan_workspace(&manifest, &database)?;
    let unchanged_elapsed = started.elapsed();

    let server = root.join(CHANGED_REPOSITORY).join("src/server.ts");
    let original = std::fs::read_to_string(&server)?;
    let extended = original.replace(
        "app.get('/svc-042/health', health);",
        "app.get('/svc-042/health', health);\napp.get('/svc-042/audit/:id', getItem);",
    );
    anyhow::ensure!(extended != original, "the changed route was not inserted");
    write_aged(&server, &extended, Duration::from_mins(5))?;
    let started = Instant::now();
    let one_file = scan_workspace(&manifest, &database)?;
    let one_file_elapsed = started.elapsed();
    let segment = segment_rows(&database, CHANGED_REPOSITORY)?;
    let first_size = database_bytes(&database)?;

    for round in 0..SYNC_ROUNDS {
        let contents = if round % 2 == 0 { &original } else { &extended };
        write_aged(
            &server,
            contents,
            Duration::from_mins(4).saturating_sub(Duration::from_secs(round)),
        )?;
        scan_workspace(&manifest, &database)?;
    }
    let final_size = database_bytes(&database)?;

    eprintln!(
        "sync_scale repos={REPOSITORY_COUNT} files={} cold_ms={} cold_peak_worker_rss_bytes={} \
         cold_bytes_read={} nodes={} edges={} unchanged_ms={} unchanged_bytes_read={} \
         unchanged_stat_hits={} one_file_ms={} one_file_bytes_read={} one_file_published_rows={} \
         segment_rows={segment} first_db_bytes={first_size} final_db_bytes={final_size}",
        REPOSITORY_COUNT * FILES_PER_REPOSITORY,
        cold_elapsed.as_millis(),
        cold.execution.peak_worker_memory_bytes,
        cold.execution.content_bytes_read,
        cold.node_count,
        cold.edge_count,
        unchanged_elapsed.as_millis(),
        unchanged.execution.content_bytes_read,
        unchanged.execution.stat_cache_hits,
        one_file_elapsed.as_millis(),
        one_file.execution.content_bytes_read,
        one_file.execution.published_rows,
    );
    for phase in &cold.execution.phases {
        eprintln!(
            "sync_scale cold_phase={:?} ms={} rss_bytes={}",
            phase.phase, phase.duration_ms, phase.resident_memory_bytes
        );
    }
    assert!(unchanged.reused_snapshot);
    assert_eq!(unchanged.execution.content_bytes_read, 0);
    assert!(!one_file.reused_snapshot);
    assert!(
        one_file.execution.published_rows <= segment,
        "one changed file published {} rows; the repository segment has {segment}",
        one_file.execution.published_rows
    );
    assert!(
        final_size.saturating_mul(100) <= first_size.saturating_mul(105),
        "database grew from {first_size} to {final_size} bytes after {SYNC_ROUNDS} syncs"
    );
    Ok(())
}

fn write_workload(root: &Path) -> anyhow::Result<PathBuf> {
    let mut manifest = format!("version: 1\nname: {WORKSPACE}\nrepos:\n");
    for index in 0..REPOSITORY_COUNT {
        let alias = format!("repo-{index:03}");
        let repository = root.join(&alias);
        let next = (index + 1) % REPOSITORY_COUNT;
        let mut files = vec![
            (
                "src/server.ts".to_owned(),
                format!(
                    "import express from 'express';\n\nconst app = express();\n\nfunction getItem(req, res) {{\n  res.json({{ id: req.params.id }});\n}}\n\nfunction health(req, res) {{\n  res.json({{ ok: true }});\n}}\n\napp.get('/svc-{index:03}/items/:id', getItem);\napp.get('/svc-{index:03}/health', health);\n\nexport default app;\n"
                ),
            ),
            (
                "src/client.ts".to_owned(),
                format!(
                    "const BASE_URL = 'http://localhost:8080';\n\nexport async function loadNext(id: string) {{\n  return fetch(`${{BASE_URL}}/svc-{next:03}/items/${{id}}`);\n}}\n"
                ),
            ),
            (
                "tests/test_api.py".to_owned(),
                format!(
                    "import requests\n\nBASE_URL = \"http://localhost:8080\"\n\n\ndef test_item():\n    requests.get(f\"{{BASE_URL}}/svc-{index:03}/items/42\")\n"
                ),
            ),
        ];
        for module in 0..FILES_PER_REPOSITORY - files.len() {
            files.push(module_file(module));
        }
        for (path, contents) in files {
            write_aged(&repository.join(path), &contents, Duration::from_mins(10))?;
        }
        writeln!(manifest, "  {alias}:\n    path: {alias}")?;
    }
    let manifest_path = root.join("code-system-graph.yaml");
    std::fs::write(&manifest_path, manifest)?;
    Ok(manifest_path)
}

fn module_file(module: usize) -> (String, String) {
    match module % 3 {
        0 => (
            format!("src/modules/module_{module:03}.ts"),
            format!(
                "export interface Line{module} {{\n  id: string;\n  total: number;\n}}\n\nexport function total{module}(lines: Line{module}[]): number {{\n  return lines.reduce((sum, line) => sum + line.total, 0);\n}}\n"
            ),
        ),
        1 => (
            format!("lib/module_{module:03}.py"),
            format!(
                "def total_{module}(lines):\n    return sum(line[\"total\"] for line in lines)\n\n\ndef largest_{module}(lines):\n    return max(lines, key=lambda line: line[\"total\"], default=None)\n"
            ),
        ),
        _ => (
            format!("pkg/module_{module:03}.go"),
            format!(
                "package pkg\n\nfunc Total{module}(values []int) int {{\n\ttotal := 0\n\tfor _, value := range values {{\n\t\ttotal += value\n\t}}\n\treturn total\n}}\n"
            ),
        ),
    }
}

fn write_aged(path: &Path, contents: &str, age: Duration) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)?;
    std::fs::File::options()
        .write(true)
        .open(path)?
        .set_modified(SystemTime::now() - age)?;
    Ok(())
}

fn database_bytes(database: &Path) -> anyhow::Result<u64> {
    let mut total = std::fs::metadata(database)?.len();
    let mut wal = database.as_os_str().to_os_string();
    wal.push("-wal");
    if let Ok(metadata) = std::fs::metadata(PathBuf::from(wal)) {
        total = total.saturating_add(metadata.len());
    }
    Ok(total)
}

/// Rows owned by one repository: its nodes, every edge touching them, its evidence, and its
/// fingerprints, extractor batches, and extractor runs.
fn segment_rows(database: &Path, alias: &str) -> anyhow::Result<u64> {
    let store = SqliteStore::open_read_only(database)?;
    let repository = store
        .load_workspace_registry(WORKSPACE)?
        .repositories
        .into_iter()
        .find(|repository| repository.alias == alias)
        .map(|repository| repository.id)
        .ok_or_else(|| anyhow::anyhow!("repository {alias} is not registered"))?;
    let owned = |repo: Option<&RepoId>| repo == Some(&repository);
    let (nodes, edges) = store.load_current_graph(WORKSPACE)?;
    let node_ids = nodes
        .iter()
        .filter(|node| owned(node.repo_id.as_ref()))
        .map(|node| &node.id)
        .collect::<std::collections::BTreeSet<_>>();
    let edge_count = edges
        .iter()
        .filter(|edge| node_ids.contains(&edge.source) || node_ids.contains(&edge.target))
        .count();
    let evidence_count = store
        .load_current_evidence(WORKSPACE)?
        .iter()
        .filter(|evidence| owned(evidence.repo_id.as_ref()))
        .count();
    let fingerprint_count = store
        .load_current_artifact_fingerprints(WORKSPACE)?
        .iter()
        .filter(|fingerprint| fingerprint.repo_id == repository)
        .count();
    let run_count = store
        .load_current_extractor_runs(WORKSPACE)?
        .iter()
        .filter(|run| run.repo_id == repository)
        .count();
    Ok(u64::try_from(
        node_ids.len() + edge_count + evidence_count + 2 * fingerprint_count + run_count,
    )?)
}

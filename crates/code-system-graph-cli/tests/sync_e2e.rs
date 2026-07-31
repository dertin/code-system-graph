//! End-to-end acceptance tests for one-shot and watched incremental synchronization.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Context;
use code_system_graph::{CodeGraphSyncState, SyncSummary};

fn write_workspace(
    root: &std::path::Path,
) -> anyhow::Result<(std::path::PathBuf, std::path::PathBuf)> {
    let repository = root.join("api");
    std::fs::create_dir(&repository)?;
    std::fs::write(
        repository.join("openapi.yaml"),
        "openapi: 3.1.0\ninfo: { title: API, version: 1 }\npaths:\n  /before:\n    get: {}\n",
    )?;
    let manifest = root.join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: sync-e2e\nrepos:\n  api:\n    path: api\n    openapi: openapi.yaml\n",
    )?;
    Ok((manifest, root.join("graph.db")))
}

fn run_sync(manifest: &std::path::Path, database: &std::path::Path) -> anyhow::Result<SyncSummary> {
    let output = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .arg("sync")
        .arg("--config")
        .arg(manifest)
        .arg("--database")
        .arg(database)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "sync failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[test]
fn one_shot_sync_should_report_optional_codegraph_and_reuse_snapshot() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (manifest, database) = write_workspace(temporary.path())?;

    let first = run_sync(&manifest, &database)?;
    assert_eq!(first.schema_version, 1);
    assert!(first.codegraph.enabled);
    assert_eq!(first.codegraph.repository_count, 1);
    assert_eq!(first.codegraph.skipped_count, 1);
    assert_eq!(
        first.codegraph.repositories[0].state,
        CodeGraphSyncState::SkippedNotInitialized
    );
    assert!(!first.scan.reused_snapshot);

    let second = run_sync(&manifest, &database)?;
    assert!(second.scan.reused_snapshot);
    assert_eq!(second.scan.changed_input_count, 0);
    assert_eq!(second.scan.snapshot_id, first.scan.snapshot_id);
    Ok(())
}

#[test]
fn native_watch_sync_should_publish_after_a_source_change() -> anyhow::Result<()> {
    assert_watch_sync(&[])
}

#[test]
fn polling_watch_sync_should_publish_after_a_same_size_change() -> anyhow::Result<()> {
    assert_watch_sync(&["--poll-interval-ms", "100"])
}

fn assert_watch_sync(extra_arguments: &[&str]) -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (manifest, database) = write_workspace(temporary.path())?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .arg("sync")
        .arg("--watch")
        .arg("--no-codegraph")
        .arg("--debounce-ms")
        .arg("100")
        .args(extra_arguments)
        .arg("--config")
        .arg(&manifest)
        .arg("--database")
        .arg(&database)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("watch stdout was not piped"))?;
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    let result = (|| -> anyhow::Result<()> {
        let initial = receiver
            .recv_timeout(Duration::from_secs(15))
            .context("watch did not publish its initial pass")??;
        let initial: SyncSummary = serde_json::from_str(&initial)?;
        assert!(!initial.scan.reused_snapshot);
        std::fs::write(
            temporary.path().join("api/openapi.yaml"),
            "openapi: 3.1.0\ninfo: { title: API, version: 1 }\npaths:\n  /after:\n    get: {}\n",
        )?;
        let updated = receiver
            .recv_timeout(Duration::from_secs(15))
            .context("watch did not publish after the source change")??;
        let updated: SyncSummary = serde_json::from_str(&updated)?;
        assert!(updated.scan.changed_input_count > 0);
        assert_ne!(updated.scan.snapshot_id, initial.scan.snapshot_id);
        Ok(())
    })();

    child.kill()?;
    let _status = child.wait()?;
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("watch stdout reader panicked"))?;
    result
}

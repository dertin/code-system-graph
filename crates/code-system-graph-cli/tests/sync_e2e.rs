//! End-to-end acceptance tests for one-shot and watched incremental synchronization.

use std::io::{BufRead, BufReader, Read};
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
    run_sync_with_arguments(manifest, database, &[])
}

fn run_sync_with_arguments(
    manifest: &std::path::Path,
    database: &std::path::Path,
    arguments: &[&std::ffi::OsStr],
) -> anyhow::Result<SyncSummary> {
    let output = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .arg("sync")
        .args(arguments)
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

#[cfg(unix)]
#[test]
fn initialized_current_codegraph_should_enrich_native_changes_without_republishing_noops()
-> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir()?;
    let (manifest, database) = write_workspace(temporary.path())?;
    std::fs::create_dir(temporary.path().join("api/src"))?;
    std::fs::write(
        temporary.path().join("api/src/lib.rs"),
        "use axum::{Router, routing::get};\npub fn router() -> Router { Router::new().route(\"/before\", get(handler)) }\nasync fn handler() {}\n",
    )?;
    std::fs::create_dir(temporary.path().join("api/.codegraph"))?;
    let invocation_log = temporary.path().join("codegraph.log");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/codegraph/fake/codegraph.py")
        .canonicalize()?;
    let binary = temporary.path().join("codegraph-current");
    std::fs::write(
        &binary,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec python3 '{}' \"$@\"\n",
            invocation_log.display(),
            fixture.display()
        ),
    )?;
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
    let arguments = [
        std::ffi::OsStr::new("--codegraph-binary"),
        binary.as_os_str(),
    ];

    let first = run_sync_with_arguments(&manifest, &database, &arguments)?;
    let second = run_sync_with_arguments(&manifest, &database, &arguments)?;
    let after_second = std::fs::read_to_string(&invocation_log)?;
    let affected_after_second = after_second
        .lines()
        .filter(|line| line.starts_with("affected "))
        .count();

    std::fs::write(
        temporary.path().join("api/openapi.yaml"),
        "openapi: 3.1.0\ninfo: { title: API, version: 1 }\npaths:\n  /after:\n    get: {}\n",
    )?;
    let third = run_sync_with_arguments(&manifest, &database, &arguments)?;
    let after_third = std::fs::read_to_string(&invocation_log)?;
    let affected_after_third = after_third
        .lines()
        .filter(|line| line.starts_with("affected "))
        .count();
    let fourth = run_sync_with_arguments(&manifest, &database, &arguments)?;
    let final_invocations = std::fs::read_to_string(&invocation_log)?;
    let affected_final = final_invocations
        .lines()
        .filter(|line| line.starts_with("affected "))
        .count();

    assert!(!first.scan.reused_snapshot);
    assert!(affected_after_second > 0);
    assert!(second.scan.reused_snapshot);
    assert_eq!(second.scan.changed_input_count, 0);
    assert_eq!(second.scan.snapshot_id, first.scan.snapshot_id);
    assert_eq!(second.codegraph.synchronized_count, 1);
    assert_eq!(second.codegraph.changed_count, 0);
    assert_eq!(second.codegraph.unchanged_count, 1);
    assert!(third.scan.changed_input_count > 0);
    assert!(!third.scan.reused_snapshot);
    assert_eq!(third.codegraph.changed_count, 0);
    assert!(affected_after_third > affected_after_second);
    assert!(fourth.scan.reused_snapshot);
    assert_eq!(fourth.scan.snapshot_id, third.scan.snapshot_id);
    assert_eq!(affected_final, affected_after_third);
    assert!(
        !final_invocations
            .lines()
            .any(|line| line.starts_with("sync "))
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn stale_codegraph_index_should_corroborate_even_without_native_changes() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir()?;
    let (manifest, database) = write_workspace(temporary.path())?;
    std::fs::create_dir(temporary.path().join("api/src"))?;
    std::fs::write(
        temporary.path().join("api/src/lib.rs"),
        "use axum::{Router, routing::get};\npub fn router() -> Router { Router::new().route(\"/before\", get(handler)) }\nasync fn handler() {}\n",
    )?;
    let initial = run_sync_with_arguments(
        &manifest,
        &database,
        &[std::ffi::OsStr::new("--no-codegraph")],
    )?;
    std::fs::create_dir(temporary.path().join("api/.codegraph"))?;
    let invocation_log = temporary.path().join("codegraph.log");
    let synchronized_marker = temporary.path().join("synchronized");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/codegraph/fake/codegraph.py")
        .canonicalize()?;
    let binary = temporary.path().join("codegraph-stale");
    std::fs::write(
        &binary,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1\" in\n  status)\n    if [ -f '{}' ]; then modified=0; else modified=1; fi\n    printf '{{\"initialized\":true,\"version\":\"1.5.0\",\"pendingChanges\":{{\"added\":0,\"modified\":%s,\"removed\":0}},\"worktreeMismatch\":null,\"index\":{{\"reindexRecommended\":false,\"state\":\"complete\"}}}}\\n' \"$modified\"\n    ;;\n  sync) : > '{}' ;;\n  *) exec python3 '{}' \"$@\" ;;\nesac\n",
            invocation_log.display(),
            synchronized_marker.display(),
            synchronized_marker.display(),
            fixture.display()
        ),
    )?;
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700))?;
    let arguments = [
        std::ffi::OsStr::new("--codegraph-binary"),
        binary.as_os_str(),
    ];

    let synchronized = run_sync_with_arguments(&manifest, &database, &arguments)?;
    let current = run_sync_with_arguments(&manifest, &database, &arguments)?;
    let invocations = std::fs::read_to_string(&invocation_log)?;

    assert_eq!(synchronized.scan.changed_input_count, 0);
    assert!(!synchronized.scan.reused_snapshot);
    assert_eq!(synchronized.codegraph.changed_count, 1);
    assert_eq!(synchronized.scan.snapshot_id, initial.scan.snapshot_id);
    assert!(invocations.lines().any(|line| line.starts_with("sync ")));
    assert!(invocations.lines().any(|line| line.starts_with("query ")));
    assert!(current.scan.reused_snapshot);
    assert_eq!(current.codegraph.unchanged_count, 1);
    Ok(())
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
        let initial = watch_sync_summary(&initial)?;
        assert!(!initial.scan.reused_snapshot);
        std::fs::write(
            temporary.path().join("api/openapi.yaml"),
            "openapi: 3.1.0\ninfo: { title: API, version: 1 }\npaths:\n  /after:\n    get: {}\n",
        )?;
        let updated = loop {
            let candidate = receiver
                .recv_timeout(Duration::from_secs(15))
                .context("watch did not publish after the source change")??;
            let candidate = watch_sync_summary(&candidate)?;
            if candidate.scan.changed_input_count > 0
                && candidate.scan.snapshot_id != initial.scan.snapshot_id
            {
                break candidate;
            }
        };
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

#[cfg(target_os = "linux")]
#[test]
fn native_watch_should_reload_gitignore_before_filtering_future_events() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (manifest, database) = write_workspace(temporary.path())?;
    let repository = temporary.path().join("api");
    std::fs::create_dir(repository.join("src"))?;
    let source = repository.join("src/lib.rs");
    std::fs::write(&source, "pub fn before() {}\n")?;
    std::fs::write(repository.join(".gitignore"), "src/\n")?;
    std::fs::write(
        &manifest,
        "version: 1\nname: sync-e2e\nrepos:\n  api:\n    path: api\n    openapi: openapi.yaml\n    useGitignore: true\n",
    )?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .arg("sync")
        .arg("--watch")
        .arg("--no-codegraph")
        .arg("--debounce-ms")
        .arg("100")
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
        let initial = watch_sync_summary(&initial)?;
        std::fs::write(repository.join(".gitignore"), "")?;
        let unignored = next_changed_watch_summary(&receiver, &initial)?;

        std::fs::write(&source, "pub fn after() {}\n")?;
        let edited = next_changed_watch_summary(&receiver, &unignored)?;

        assert_ne!(unignored.scan.snapshot_id, initial.scan.snapshot_id);
        assert_ne!(edited.scan.snapshot_id, unignored.scan.snapshot_id);
        Ok(())
    })();

    child.kill()?;
    let _status = child.wait()?;
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("watch stdout reader panicked"))?;
    result
}

fn next_changed_watch_summary(
    receiver: &mpsc::Receiver<Result<String, std::io::Error>>,
    previous: &SyncSummary,
) -> anyhow::Result<SyncSummary> {
    loop {
        let candidate = receiver
            .recv_timeout(Duration::from_secs(15))
            .context("watch did not publish after the expected change")??;
        let candidate = watch_sync_summary(&candidate)?;
        if candidate.scan.changed_input_count > 0
            && candidate.scan.snapshot_id != previous.scan.snapshot_id
        {
            return Ok(candidate);
        }
    }
}

#[test]
fn watch_failure_should_not_persist_or_emit_parser_literals() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("api");
    std::fs::create_dir(&repository)?;
    let graphql = repository.join("schema.graphql");
    std::fs::write(&graphql, "type Query { viewer: String }\n")?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    let database = temporary.path().join("graph.db");
    std::fs::write(
        &manifest,
        "version: 1\nname: watch-secret-e2e\nrepos:\n  api:\n    path: api\n",
    )?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .arg("sync")
        .arg("--watch")
        .arg("--no-codegraph")
        .arg("--debounce-ms")
        .arg("50")
        .arg("--config")
        .arg(&manifest)
        .arg("--database")
        .arg(&database)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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
    let secret = "private-graphql-literal-6e21c8";
    let result = (|| -> anyhow::Result<String> {
        let initial = receiver
            .recv_timeout(Duration::from_secs(15))
            .context("watch did not publish its initial pass")??;
        let _summary = watch_sync_summary(&initial)?;
        std::fs::write(&graphql, format!("\"{secret}\"\n"))?;
        loop {
            let line = receiver
                .recv_timeout(Duration::from_secs(15))
                .context("watch did not terminate after malformed GraphQL")??;
            let value: serde_json::Value = serde_json::from_str(&line)?;
            if value.get("type").and_then(serde_json::Value::as_str) == Some("termination") {
                break Ok(line);
            }
        }
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let termination = result?;
    let status = child.wait()?;
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .ok_or_else(|| anyhow::anyhow!("watch stderr was not piped"))?
        .read_to_end(&mut stderr)?;
    reader
        .join()
        .map_err(|_| anyhow::anyhow!("watch stdout reader panicked"))?;
    assert!(!status.success());

    let status_output = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .arg("status")
        .arg("--config")
        .arg(&manifest)
        .arg("--database")
        .arg(&database)
        .output()?;
    anyhow::ensure!(status_output.status.success(), "status command failed");
    let sidecar = std::path::PathBuf::from(format!("{}.work-v1.db", database.display()));
    let persisted = std::fs::read(sidecar)?;
    for (surface, bytes) in [
        ("termination JSONL", termination.as_bytes()),
        ("watch stderr", stderr.as_slice()),
        ("status output", status_output.stdout.as_slice()),
        ("work sidecar", persisted.as_slice()),
    ] {
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "{surface} exposed parser input"
        );
    }
    Ok(())
}

fn watch_sync_summary(line: &str) -> anyhow::Result<SyncSummary> {
    let value: serde_json::Value = serde_json::from_str(line)?;
    anyhow::ensure!(value.get("type").and_then(serde_json::Value::as_str) == Some("sync_result"));
    Ok(serde_json::from_value(
        value
            .get("summary")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("watch result omitted summary"))?,
    )?)
}

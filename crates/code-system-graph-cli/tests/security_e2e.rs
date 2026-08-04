//! Security and redacted-diagnostics acceptance tests.

use std::path::PathBuf;
use std::process::Command;

use code_system_graph::{create_diagnostic_bundle, scan_workspace};
use code_system_graph_store_sqlite::SqliteStore;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/platform-demo/code-system-graph.yaml")
}

#[test]
fn diagnostic_bundle_should_be_source_free_private_and_non_overwriting() -> anyhow::Result<()> {
    let fixture = fixture();
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");
    let output = temporary.path().join("diagnostics.json");
    scan_workspace(&fixture, &database)?;

    let bundle = create_diagnostic_bundle(&fixture, &database, &output)?;
    let serialized = std::fs::read_to_string(&output)?;
    assert_eq!(bundle.binary_version, env!("CARGO_PKG_VERSION"));
    assert!(!serialized.contains("CheckoutButton"));
    assert!(!serialized.contains("source_body"));
    assert!(create_diagnostic_bundle(&fixture, &database, &output).is_err());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        assert_eq!(
            std::fs::metadata(&output)?.permissions().mode() & 0o777,
            0o600
        );
    }
    Ok(())
}

#[test]
fn json_diagnostics_should_stay_on_stderr_with_correlation_id() -> anyhow::Result<()> {
    let output = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["--verbose", "--log-format", "json", "completions", "bash"])
        .output()?;
    let stderr = String::from_utf8(output.stderr)?;
    let events = stderr
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;

    assert!(output.status.success());
    assert!(String::from_utf8(output.stdout)?.contains("_csgraph"));
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["event"], "command_started");
    assert_eq!(events[0]["correlation_id"], events[1]["correlation_id"]);
    assert_eq!(events[1]["success"], true);
    Ok(())
}

#[test]
fn non_utf8_artifact_should_degrade_without_aborting_workspace_scan() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("repository");
    std::fs::create_dir_all(repository.join("db"))?;
    std::fs::create_dir_all(repository.join("venv/lib/python3.12/site-packages"))?;
    std::fs::write(
        repository.join("db/schema.sql"),
        b"-- invalid encoding: \xF1\nCREATE TABLE users (id INTEGER);\n",
    )?;
    std::fs::write(
        repository.join("venv/lib/python3.12/site-packages/pyproject.toml"),
        "malformed = \"third-party environment",
    )?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: non-utf8\nrepos:\n  root:\n    path: repository\n",
    )?;
    let database = temporary.path().join("graph.db");

    let first = scan_workspace(&manifest, &database)?;
    let second = scan_workspace(&manifest, &database)?;
    let expected_artifact = ["db", "schema.sql"]
        .iter()
        .collect::<std::path::PathBuf>()
        .display()
        .to_string();

    assert!(!first.reused_snapshot);
    assert!(second.reused_snapshot);
    assert_eq!(first.discovered_input_count, 1);
    assert!(first.degradations.iter().any(|message| {
        message.contains(&expected_artifact)
            && message.contains("invalid UTF-8")
            && message.contains("incomplete")
    }));
    assert_eq!(first.degradation_count, first.degradations.len());
    assert_eq!(first.degradations, second.degradations);
    Ok(())
}

#[test]
fn identical_artifacts_should_share_one_aggregated_extractor_run() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("repository/db");
    std::fs::create_dir_all(&repository)?;
    let sql = "CREATE TABLE users (id INTEGER);\n";
    std::fs::write(repository.join("001_schema.sql"), sql)?;
    std::fs::write(repository.join("002_schema.sql"), sql)?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: duplicate-content\nrepos:\n  root:\n    path: repository\n",
    )?;

    let database = temporary.path().join("graph.db");
    let report = scan_workspace(&manifest, &database)?;
    let runs =
        SqliteStore::open_read_only(&database)?.load_current_extractor_runs("duplicate-content")?;

    assert_eq!(report.discovered_input_count, 2);
    assert!(report.node_count > 0);
    assert_eq!(runs.len(), 1);
    assert_eq!(
        (
            runs[0].discovered_files,
            runs[0].parsed_files,
            runs[0].skipped_files,
        ),
        (2, 2, 0)
    );
    Ok(())
}

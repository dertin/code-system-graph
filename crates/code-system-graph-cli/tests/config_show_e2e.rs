//! End-to-end coverage for effective native discovery configuration.

use std::num::NonZeroUsize;
use std::process::Command;

use code_system_graph::ExtendedConfigReport;
use code_system_graph_core::{ExecutionPolicy, ExtractionBudgets};

#[test]
fn config_show_should_report_defaults_and_selected_repository_rules() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir_all(temporary.path().join("api"))?;
    std::fs::create_dir_all(temporary.path().join("worker"))?;
    std::fs::write(
        temporary.path().join("worker/.code-system-graph.yaml"),
        "version: 1\nexcludes:\n  - generated/**\n",
    )?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: configuration\nrepos:\n  api:\n    path: api\n    excludes:\n      - ./coverage//./**\n    includeDefaults:\n      - ./vendor//internal-sdk/./**\n  worker:\n    path: worker\n",
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["config", "show", "--config"])
        .arg(&manifest)
        .args(["--repo", "api"])
        .output()?;
    let report: ExtendedConfigReport = serde_json::from_slice(&output.stdout)?;

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(report.schema_version, 2);
    assert_eq!(report.workspace, "configuration");
    assert_eq!(report.extraction_budgets, ExtractionBudgets::default());
    assert_eq!(report.execution_policy, ExecutionPolicy::default());
    assert_eq!(
        report.scan_fingerprint,
        report.execution_policy.scan_fingerprint()
    );
    assert_eq!(report.repositories.len(), 1);
    assert_eq!(report.repositories[0].alias, "api");
    assert!(!report.repositories[0].ignore_policy.use_gitignore.value);
    assert!(matches!(
        report.repositories[0].ignore_policy.use_gitignore.source,
        code_system_graph::ConfigSource::Default
    ));
    assert_eq!(
        report.repositories[0]
            .ignore_policy
            .configured_excludes
            .patterns,
        vec!["coverage/**"]
    );
    assert_eq!(
        report.repositories[0]
            .ignore_policy
            .include_defaults
            .patterns,
        vec!["vendor/internal-sdk/**"]
    );
    assert!(
        report.repositories[0]
            .ignore_policy
            .default_excludes
            .iter()
            .any(|pattern| pattern.contains("node_modules"))
    );
    assert!(
        report.repositories[0]
            .ignore_policy
            .protected_excludes
            .iter()
            .any(|pattern| pattern.contains(".code-system-graph"))
    );
    Ok(())
}

#[test]
fn config_show_should_report_effective_execution_policy() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir_all(temporary.path().join("api"))?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: configuration\nexecutionPolicy:\n  maxScanWallTimeMs: 28800000\n  maxNoProgressTimeMs: 600000\n  maxCodeGraphCorroborationAnchorsPerRepo: 12\nrepos:\n  api:\n    path: api\n",
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["config", "show", "--config"])
        .arg(&manifest)
        .output()?;
    let report: ExtendedConfigReport = serde_json::from_slice(&output.stdout)?;

    assert!(output.status.success());
    assert_eq!(report.execution_policy.max_scan_wall_time_ms, 28_800_000);
    assert_eq!(report.execution_policy.max_no_progress_time_ms, 600_000);
    assert_eq!(
        report
            .execution_policy
            .max_codegraph_corroboration_anchors_per_repo
            .bounded()
            .map(NonZeroUsize::get),
        Some(12)
    );
    assert_eq!(
        report.execution_policy.max_worker_memory_bytes,
        ExecutionPolicy::default().max_worker_memory_bytes
    );
    assert_eq!(
        report.scan_fingerprint,
        report.execution_policy.scan_fingerprint()
    );
    Ok(())
}

#[test]
fn config_show_should_report_effective_budget_overrides() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir_all(temporary.path().join("api"))?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: configuration\nextractionBudgets:\n  maxInputBytesPerArtifact: 1234\nrepos:\n  api:\n    path: api\n",
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["config", "show", "--config"])
        .arg(&manifest)
        .output()?;
    let report: ExtendedConfigReport = serde_json::from_slice(&output.stdout)?;

    assert!(output.status.success());
    assert_eq!(
        report.extraction_budgets.max_input_bytes_per_artifact,
        1_234
    );
    assert_eq!(
        report.extraction_budgets.max_ast_depth_per_artifact,
        ExtractionBudgets::default().max_ast_depth_per_artifact
    );
    Ok(())
}

#[test]
fn repository_local_config_should_reject_extraction_budget_overrides() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir_all(temporary.path().join("api"))?;
    std::fs::write(
        temporary.path().join("api/.code-system-graph.yaml"),
        "version: 1\nextractionBudgets:\n  maxInputBytesPerArtifact: 999999999\n",
    )?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: configuration\nrepos:\n  api:\n    path: api\n",
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["config", "show", "--config"])
        .arg(&manifest)
        .output()?;

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("extractionBudgets"));
    Ok(())
}

#[test]
fn repository_local_config_should_reject_execution_policy_overrides() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir_all(temporary.path().join("api"))?;
    std::fs::write(
        temporary.path().join("api/.code-system-graph.yaml"),
        "version: 1\nexecutionPolicy:\n  maxScanWallTimeMs: 999999999\n",
    )?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: configuration\nrepos:\n  api:\n    path: api\n",
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["config", "show", "--config"])
        .arg(&manifest)
        .output()?;

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("executionPolicy"));
    Ok(())
}

#[test]
fn analyzed_repository_should_not_control_advanced_global_policy() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("repository");
    std::fs::create_dir(&repository)?;
    let manifest = repository.join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: untrusted-policy\nextractionBudgets:\n  maxInputBytesPerArtifact: 999999999\nexecutionPolicy:\n  maxScanWallTimeMs: 999999999\nrepos:\n  root:\n    path: .\n",
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["config", "show", "--config"])
        .arg(&manifest)
        .output()?;
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success());
    assert!(stderr.contains("advanced global resource policy"));
    assert!(stderr.contains("move the workspace manifest outside every analyzed checkout"));
    Ok(())
}

#[test]
fn config_show_should_report_repository_local_rule_source() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir_all(temporary.path().join("worker"))?;
    std::fs::write(
        temporary.path().join("worker/.code-system-graph.yaml"),
        "version: 1\nexcludes:\n  - generated/**\n",
    )?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: configuration\nrepos:\n  worker:\n    path: worker\n",
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["config", "show", "--config"])
        .arg(&manifest)
        .output()?;
    let report: ExtendedConfigReport = serde_json::from_slice(&output.stdout)?;

    assert!(matches!(
        report.repositories[0]
            .ignore_policy
            .configured_excludes
            .source,
        code_system_graph::ConfigSource::RepositoryLocal
    ));
    Ok(())
}

#[test]
fn config_show_should_report_repository_local_gitignore_source() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir_all(temporary.path().join("worker"))?;
    std::fs::write(
        temporary.path().join("worker/.code-system-graph.yaml"),
        "version: 1\nuseGitignore: true\n",
    )?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: configuration\nrepos:\n  worker:\n    path: worker\n",
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["config", "show", "--config"])
        .arg(&manifest)
        .output()?;
    let report: ExtendedConfigReport = serde_json::from_slice(&output.stdout)?;

    assert!(output.status.success());
    assert!(report.repositories[0].ignore_policy.use_gitignore.value);
    assert!(matches!(
        report.repositories[0].ignore_policy.use_gitignore.source,
        code_system_graph::ConfigSource::RepositoryLocal
    ));
    Ok(())
}

#[test]
fn config_show_should_reject_unknown_repository_alias() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir_all(temporary.path().join("api"))?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: configuration\nrepos:\n  api:\n    path: api\n",
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["config", "show", "--config"])
        .arg(&manifest)
        .args(["--repo", "missing"])
        .output()?;

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing"));
    Ok(())
}

#[test]
fn config_show_should_reject_unsupported_glob_syntax() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir_all(temporary.path().join("api"))?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: configuration\nrepos:\n  api:\n    path: api\n    excludes:\n      - \"src/[ab]/**\"\n",
    )?;

    let output = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["config", "show", "--config"])
        .arg(&manifest)
        .output()?;

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported glob syntax"));
    Ok(())
}

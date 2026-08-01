//! End-to-end coverage for effective native discovery configuration.

use std::process::Command;

use code_system_graph::ConfigReport;

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
    let report: ConfigReport = serde_json::from_slice(&output.stdout)?;

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(report.schema_version, 1);
    assert_eq!(report.workspace, "configuration");
    assert_eq!(report.repositories.len(), 1);
    assert_eq!(report.repositories[0].alias, "api");
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
    let report: ConfigReport = serde_json::from_slice(&output.stdout)?;

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

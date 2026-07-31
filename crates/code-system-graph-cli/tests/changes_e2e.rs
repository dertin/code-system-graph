//! End-to-end acceptance tests for bounded local Git change delivery.

use std::path::Path;
use std::process::Command;

use code_system_graph::{ChangesInput, analyze_workspace_changes, scan_workspace};
use code_system_graph_core::{
    ChangeAnalysisOptions, ChangeConclusion, ChangeScope, ChangeSourceLayer, ChangeValidity, ChangeValidityInput, ChangedFileStatus, validate_change_analysis
};
use code_system_graph_model::ToolStatus;

fn git(repository: &Path, arguments: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("git")
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .status()?;
    anyhow::ensure!(status.success(), "git command failed: {arguments:?}");
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the acceptance flow keeps Git setup, analysis, and invalidation auditable"
)]
async fn local_changes_should_be_fingerprinted_read_only_and_source_free() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("service");
    std::fs::create_dir_all(repository.join("src"))?;
    std::fs::write(
        repository.join("Cargo.toml"),
        "[package]\nname = \"service\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )?;
    std::fs::write(
        repository.join("src/lib.rs"),
        "pub fn answer() -> u32 {\n    41\n}\n",
    )?;
    git(&repository, &["init", "--quiet"])?;
    git(&repository, &["add", "."])?;
    git(
        &repository,
        &[
            "-c",
            "user.name=Code System Graph Test",
            "-c",
            "user.email=code-system-graph@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "Initialize fixture",
        ],
    )?;

    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: change-test\nrepos:\n  service:\n    path: service\n",
    )?;
    let database = temporary.path().join("graph.db");
    scan_workspace(&manifest, &database)?;
    std::fs::write(
        repository.join("src/lib.rs"),
        "pub fn answer() -> u32 {\n    42\n}\n",
    )?;

    let first = analyze_workspace_changes(
        &database,
        "change-test",
        &ChangesInput {
            repository: "service".to_owned(),
            scope: ChangeScope::All,
        },
        &ChangeAnalysisOptions::default(),
        None,
    )
    .await?;
    let second = analyze_workspace_changes(
        &database,
        "change-test",
        &ChangesInput {
            repository: "service".to_owned(),
            scope: ChangeScope::All,
        },
        &ChangeAnalysisOptions::default(),
        None,
    )
    .await?;
    let first_report = first
        .data
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("change report"))?;
    let second_report = second
        .data
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("second change report"))?;
    let first_set = &first_report.change_set;
    let second_set = &second_report.change_set;

    assert_eq!(first.status, ToolStatus::Degraded);
    assert_eq!(
        first_set.exact_diff_fingerprint,
        second_set.exact_diff_fingerprint
    );
    assert_eq!(first_set.files.len(), 1);
    assert_eq!(first_set.files[0].status, ChangedFileStatus::Modified);
    assert_eq!(first_set.files[0].source, ChangeSourceLayer::Worktree);
    assert_eq!(first_set.files[0].hunks.len(), 1);
    assert_eq!(first_report.summary.conclusion, ChangeConclusion::Unknown);
    let serialized = serde_json::to_string(first_report)?;
    assert!(!serialized.contains("pub fn"));
    assert!(!serialized.contains("answer()"));

    std::fs::write(
        repository.join("src/lib.rs"),
        "pub fn answer() -> u32 {\n    43\n}\n",
    )?;
    let changed_again = analyze_workspace_changes(
        &database,
        "change-test",
        &ChangesInput {
            repository: "service".to_owned(),
            scope: ChangeScope::All,
        },
        &ChangeAnalysisOptions::default(),
        None,
    )
    .await?;
    let current = ChangeValidityInput::from(
        &changed_again
            .data
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("updated change report"))?
            .change_set,
    );
    assert!(matches!(
        validate_change_analysis(first_report, &current),
        ChangeValidity::Stale { .. }
    ));
    Ok(())
}

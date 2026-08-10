//! End-to-end acceptance tests for ephemeral repository exploration.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use code_system_graph::{ExploreInput, explore_repository, scan_workspace};
use code_system_graph_core::ExecutionPolicy;
use code_system_graph_model::ToolStatus;

fn fake_codegraph(directory: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let binary = directory.join("codegraph-ok");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/codegraph/fake/codegraph.py"),
        &binary,
    )?;
    let mut permissions = std::fs::metadata(&binary)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions)?;
    Ok(binary)
}

fn mono_repository_fixture(
    directory: &Path,
) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let repository = directory.join("api");
    std::fs::create_dir_all(&repository)?;
    std::fs::write(repository.join("lib.rs"), "fn create_order() {}\n")?;
    let manifest = directory.join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: explore-test\nrepos:\n  api:\n    path: api\n",
    )?;
    let database = directory.join("graph.db");
    scan_workspace(&manifest, &database)?;
    Ok((manifest, database))
}

#[tokio::test]
async fn explore_should_default_to_the_only_registered_repository()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let (_manifest, database) = mono_repository_fixture(temporary.path())?;
    let binary = fake_codegraph(temporary.path())?;
    let envelope = explore_repository(
        &database,
        "explore-test",
        &ExploreInput {
            workspace: "explore-test".to_owned(),
            repository: None,
            query: "create_order callers".to_owned(),
            max_files: Some(4),
        },
        Some(binary.into_os_string()),
        &ExecutionPolicy::default(),
    )
    .await;

    let report = envelope.data.ok_or("explore report")?;
    assert_eq!(report.source_markdown, "ephemeral local context");
    assert_eq!(report.resolved_symbols.len(), 1);
    assert_eq!(report.local_relationships.len(), 2);
    assert_eq!(report.execution.provider_operations, 4);
    Ok(())
}

#[tokio::test]
async fn explore_should_require_an_alias_for_multi_repository_workspaces()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/platform-demo/code-system-graph.yaml");
    scan_workspace(&manifest, &database)?;
    let envelope = explore_repository(
        &database,
        "commerce-platform",
        &ExploreInput {
            workspace: "commerce-platform".to_owned(),
            repository: None,
            query: "create order".to_owned(),
            max_files: Some(4),
        },
        None,
        &ExecutionPolicy::default(),
    )
    .await;

    assert!(
        envelope.status == ToolStatus::Error
            && envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("repository is required"))
    );
    Ok(())
}

#[tokio::test]
async fn explore_should_select_an_explicit_alias_in_multi_repository_workspaces()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/platform-demo/code-system-graph.yaml");
    scan_workspace(&manifest, &database)?;
    let binary = fake_codegraph(temporary.path())?;
    let envelope = explore_repository(
        &database,
        "commerce-platform",
        &ExploreInput {
            workspace: "commerce-platform".to_owned(),
            repository: Some("api".to_owned()),
            query: "create order".to_owned(),
            max_files: Some(4),
        },
        Some(binary.into_os_string()),
        &ExecutionPolicy::default(),
    )
    .await;

    assert!(envelope.data.is_some());
    Ok(())
}

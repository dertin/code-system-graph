//! End-to-end validation for constrained workspace manifest mutations.

use code_system_graph::{
    add_repository_to_manifest, initialize_workspace, remove_repository_from_manifest, scan_workspace
};
use code_system_graph_core::parse_manifest;

const MANIFEST: &str = r"# preserved comment
version: 1
name: mutation-test
repos:
  api:
    path: api
";

#[test]
fn init_should_create_valid_manifest_once_without_overwriting() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("new-workspace");
    let report = initialize_workspace(&workspace, Some("platform"))?;
    let source = std::fs::read_to_string(&report.manifest_path)?;
    let manifest = parse_manifest(&source)?;
    let repeated = initialize_workspace(&workspace, Some("replacement"));

    assert_eq!(
        (
            report.workspace.as_str(),
            report.manifest_version,
            report.gitignore_path.is_none(),
            report.gitignore_updated,
            !workspace.join(".gitignore").exists(),
            manifest.name.as_str(),
            manifest.repos.len(),
            repeated.is_err(),
        ),
        ("platform", 1, true, false, true, "platform", 1, true)
    );
    Ok(())
}

#[test]
fn init_should_preserve_gitignore_and_add_generated_state_rule_once() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("existing-workspace");
    std::fs::create_dir_all(&workspace)?;
    std::fs::create_dir(workspace.join(".git"))?;
    std::fs::write(workspace.join(".git/HEAD"), "ref: refs/heads/main\n")?;
    let gitignore_path = workspace.join(".gitignore");
    std::fs::write(&gitignore_path, b"target/")?;

    let first = initialize_workspace(&workspace, Some("platform"))?;
    let first_source = std::fs::read(&gitignore_path)?;
    std::fs::remove_file(first.manifest_path)?;
    let second = initialize_workspace(&workspace, Some("platform"))?;
    let second_source = std::fs::read(&gitignore_path)?;

    assert_eq!(
        (
            first.gitignore_updated,
            second.gitignore_updated,
            first_source,
            second_source,
        ),
        (
            true,
            false,
            b"target/\n.code-system-graph/\n".to_vec(),
            b"target/\n.code-system-graph/\n".to_vec(),
        )
    );
    Ok(())
}

#[test]
fn init_should_recognize_a_linked_worktree_marker() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("linked-worktree");
    std::fs::create_dir_all(&workspace)?;
    std::fs::write(
        workspace.join(".git"),
        "gitdir: ../metadata/worktrees/linked\n",
    )?;

    let report = initialize_workspace(&workspace, Some("platform"))?;

    assert_eq!(
        (
            report.gitignore_path,
            report.gitignore_updated,
            std::fs::read_to_string(workspace.join(".gitignore"))?,
        ),
        (
            Some(workspace.join(".gitignore")),
            true,
            ".code-system-graph/\n".to_owned(),
        )
    );
    Ok(())
}

#[test]
fn init_should_not_create_parent_gitignore_for_child_repositories() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("multi-repository-workspace");
    std::fs::create_dir_all(workspace.join("repo_1/.git"))?;
    std::fs::create_dir_all(workspace.join("repo_2/.git"))?;

    let report = initialize_workspace(&workspace, Some("platform"))?;

    assert_eq!(
        (
            report.gitignore_path,
            report.gitignore_updated,
            workspace.join(".gitignore").exists(),
        ),
        (None, false, false)
    );
    Ok(())
}

#[test]
fn scan_should_exclude_code_system_graph_generated_state() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("generated-state-workspace");
    let report = initialize_workspace(&workspace, Some("platform"))?;
    let state_directory = workspace.join(".code-system-graph");
    std::fs::create_dir_all(&state_directory)?;
    let database = state_directory.join("code-system-graph.db");

    let first = scan_workspace(&report.manifest_path, &database)?;
    std::fs::write(
        state_directory.join("must-not-be-scanned.rs"),
        "fn generated_state() {}",
    )?;
    let second = scan_workspace(&report.manifest_path, &database)?;

    assert_eq!(second.discovered_input_count, first.discovered_input_count);
    assert_eq!(second.changed_input_count, 0);
    assert!(second.reused_snapshot);
    Ok(())
}

#[test]
fn repo_add_and_remove_should_preview_backup_and_preserve_content() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir_all(temporary.path().join("api"))?;
    std::fs::create_dir_all(temporary.path().join("worker"))?;
    let manifest_path = temporary.path().join("code-system-graph.yaml");
    std::fs::write(&manifest_path, MANIFEST)?;

    let add_preview = add_repository_to_manifest(&manifest_path, "worker", "worker", true)?;
    let unchanged_after_preview = std::fs::read_to_string(&manifest_path)?;
    let added = add_repository_to_manifest(&manifest_path, "worker", "worker", false)?;
    let added_source = std::fs::read_to_string(&manifest_path)?;
    let parsed_added = parse_manifest(&added_source)?;
    let add_backup = added
        .backup
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("add backup missing"))?;
    let add_backup_source = std::fs::read_to_string(add_backup)?;

    let remove_preview = remove_repository_from_manifest(&manifest_path, "worker", true)?;
    let unchanged_after_remove_preview = std::fs::read_to_string(&manifest_path)?;
    let removed = remove_repository_from_manifest(&manifest_path, "worker", false)?;
    let removed_source = std::fs::read_to_string(&manifest_path)?;
    let parsed_removed = parse_manifest(&removed_source)?;

    assert_eq!(
        (
            add_preview.applied,
            add_preview.rendered_manifest.is_some(),
            unchanged_after_preview == MANIFEST,
            added.applied,
            parsed_added.repos.contains_key("worker"),
            add_backup_source == MANIFEST,
            remove_preview.applied,
            unchanged_after_remove_preview == added_source,
            removed.applied,
            parsed_removed.repos.contains_key("worker"),
            removed_source.starts_with("# preserved comment\n"),
        ),
        (
            false, true, true, true, true, true, false, true, true, false, true,
        )
    );
    Ok(())
}

#[test]
fn repo_cli_should_preview_and_require_remove_confirmation() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir_all(temporary.path().join("api"))?;
    std::fs::create_dir_all(temporary.path().join("worker"))?;
    let manifest_path = temporary.path().join("code-system-graph.yaml");
    std::fs::write(&manifest_path, MANIFEST)?;
    let executable = env!("CARGO_BIN_EXE_csgraph");

    let preview = std::process::Command::new(executable)
        .args(["repo", "add", "--config"])
        .arg(&manifest_path)
        .args(["worker", "worker", "--dry-run"])
        .output()?;
    let unchanged = std::fs::read_to_string(&manifest_path)?;
    let add = std::process::Command::new(executable)
        .args(["repo", "add", "--config"])
        .arg(&manifest_path)
        .args(["worker", "worker"])
        .output()?;
    let denied_remove = std::process::Command::new(executable)
        .args(["repo", "remove", "--config"])
        .arg(&manifest_path)
        .arg("worker")
        .output()?;
    let after_denied = std::fs::read_to_string(&manifest_path)?;
    let remove = std::process::Command::new(executable)
        .args(["repo", "remove", "--config"])
        .arg(&manifest_path)
        .args(["worker", "--yes"])
        .output()?;
    let final_manifest = parse_manifest(&std::fs::read_to_string(&manifest_path)?)?;

    assert_eq!(
        (
            preview.status.success(),
            unchanged == MANIFEST,
            add.status.success(),
            denied_remove.status.success(),
            after_denied.contains("  worker:"),
            remove.status.success(),
            final_manifest.repos.contains_key("worker"),
        ),
        (true, true, true, false, true, true, false)
    );
    Ok(())
}

#[test]
fn workspace_cli_should_add_list_and_confirm_removal() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir_all(temporary.path().join("api"))?;
    let manifest_path = temporary.path().join("code-system-graph.yaml");
    let database = temporary.path().join("registry.db");
    std::fs::write(&manifest_path, MANIFEST)?;
    let executable = env!("CARGO_BIN_EXE_csgraph");

    let add = std::process::Command::new(executable)
        .args(["workspace", "add", "mutation-test", "--config"])
        .arg(&manifest_path)
        .arg("--database")
        .arg(&database)
        .output()?;
    let duplicate = std::process::Command::new(executable)
        .args(["workspace", "add", "mutation-test", "--config"])
        .arg(&manifest_path)
        .arg("--database")
        .arg(&database)
        .output()?;
    let list = std::process::Command::new(executable)
        .args(["workspace", "list", "--database"])
        .arg(&database)
        .output()?;
    let listed: serde_json::Value = serde_json::from_slice(&list.stdout)?;
    let denied_remove = std::process::Command::new(executable)
        .args(["workspace", "remove", "mutation-test", "--database"])
        .arg(&database)
        .output()?;
    let remove = std::process::Command::new(executable)
        .args(["workspace", "remove", "mutation-test", "--database"])
        .arg(&database)
        .arg("--yes")
        .output()?;
    let final_list = std::process::Command::new(executable)
        .args(["workspace", "list", "--database"])
        .arg(&database)
        .output()?;
    let final_value: serde_json::Value = serde_json::from_slice(&final_list.stdout)?;

    assert_eq!(
        (
            add.status.success(),
            duplicate.status.success(),
            listed[0]["name"].as_str(),
            listed[0]["repository_count"].as_u64(),
            listed[0]["config_path"].as_str().is_some(),
            denied_remove.status.success(),
            remove.status.success(),
            final_value.as_array().map(Vec::len),
        ),
        (
            true,
            false,
            Some("mutation-test"),
            Some(1),
            true,
            false,
            true,
            Some(0)
        )
    );
    Ok(())
}

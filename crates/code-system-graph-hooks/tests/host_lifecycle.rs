//! Host integration lifecycle acceptance tests.

use code_system_graph_hooks::{HookMode, HostKind, InstallRequest, install, status, uninstall};

fn request(root: &std::path::Path, host: HostKind) -> InstallRequest {
    InstallRequest {
        root: root.to_path_buf(),
        host,
        mode: HookMode::Advisory,
        code_system_graph_binary: root.join("bin/csgraph"),
        database: root.join(".code-system-graph/code-system-graph.db"),
        workspace: "acceptance".to_owned(),
        repository: "service".to_owned(),
        codegraph_enabled: false,
    }
}

#[test]
fn uninstall_on_clean_repository_without_hooks_directory_should_be_noop()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir_all(temporary.path().join(".git/hooks"))?;
    std::fs::write(temporary.path().join(".git/HEAD"), "ref: refs/heads/main\n")?;
    let removal = uninstall(&request(temporary.path(), HostKind::Cursor))?;
    assert!(!removal.changed);
    assert!(removal.removed_files.is_empty());
    Ok(())
}

#[test]
fn every_supported_host_should_install_idempotently_and_uninstall_surgically()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir_all(temporary.path().join(".git/hooks"))?;
    std::fs::write(temporary.path().join(".git/HEAD"), "ref: refs/heads/main\n")?;
    let hosts = [
        HostKind::ClaudeCode,
        HostKind::Codex,
        HostKind::Gemini,
        HostKind::Antigravity,
        HostKind::Cursor,
    ];

    for host in hosts {
        let request = request(temporary.path(), host);
        let first = install(&request)?;
        let second = install(&request)?;

        assert!(first.changed);
        assert!(!second.changed);
        assert!(status(&request)?.installed);
        assert!(first.host_file.is_file());

        let removed = uninstall(&request)?;
        assert!(removed.changed);
        assert!(!status(&request)?.installed);
    }
    Ok(())
}

#[test]
fn hook_install_should_preserve_gitignore_and_add_generated_state_once()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir(temporary.path().join(".git"))?;
    std::fs::write(temporary.path().join(".git/HEAD"), "ref: refs/heads/main\n")?;
    std::fs::write(temporary.path().join(".gitignore"), "target/")?;
    let request = request(temporary.path(), HostKind::Codex);

    let first = install(&request)?;
    let second = install(&request)?;

    assert!(first.gitignore_updated);
    assert!(!second.gitignore_updated);
    assert_eq!(
        std::fs::read_to_string(
            first
                .gitignore_path
                .ok_or("Git worktree did not receive an ignore path")?,
        )?,
        "target/\n.code-system-graph/\n"
    );
    Ok(())
}

#[test]
fn hook_install_should_preserve_non_utf8_gitignore_bytes() -> Result<(), Box<dyn std::error::Error>>
{
    let temporary = tempfile::tempdir()?;
    std::fs::create_dir(temporary.path().join(".git"))?;
    std::fs::write(temporary.path().join(".git/HEAD"), "ref: refs/heads/main\n")?;
    std::fs::write(temporary.path().join(".gitignore"), b"target/\n\xff\xfe\n")?;
    let request = request(temporary.path(), HostKind::Codex);

    let first = install(&request)?;
    let second = install(&request)?;

    assert!(first.gitignore_updated);
    assert!(!second.gitignore_updated);
    let content = std::fs::read(temporary.path().join(".gitignore"))?;
    assert_eq!(&content[..11], b"target/\n\xff\xfe\n");
    assert!(content.ends_with(b".code-system-graph/\n"));
    Ok(())
}

#[test]
fn cursor_advisory_guidance_should_support_non_git_workspace_roots()
-> Result<(), Box<dyn std::error::Error>> {
    let temporary = tempfile::tempdir()?;
    let request = request(temporary.path(), HostKind::Cursor);

    let installed = install(&request)?;

    assert!(installed.changed);
    assert!(installed.gitignore_path.is_none());
    assert!(status(&request)?.installed);
    assert!(installed.host_file.is_file());
    assert!(uninstall(&request)?.changed);
    assert!(!status(&request)?.installed);
    Ok(())
}

#[test]
fn hook_runtime_should_emit_host_context_without_echoing_prompt()
-> Result<(), Box<dyn std::error::Error>> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let temporary = tempfile::tempdir()?;
    let prompt = "Show cross-repo contract impact for private-feature-name";
    let mut child = Command::new(env!("CARGO_BIN_EXE_code-system-graph-hooks"))
        .args([
            "route",
            "--host",
            "claude-code",
            "--root",
            temporary.path().to_string_lossy().as_ref(),
            "--codegraph-enabled",
            "false",
            "--marker",
            "code-system-graph-hooks:v1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    serde_json::to_writer(
        child
            .stdin
            .as_mut()
            .ok_or_else(|| std::io::Error::other("missing hook stdin"))?,
        &serde_json::json!({"prompt": prompt, "session_id": "acceptance"}),
    )?;
    child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("missing hook stdin"))?
        .flush()?;
    let output = child.wait_with_output()?;
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)?;

    assert!(output.status.success());
    assert_eq!(
        json.pointer("/hookSpecificOutput/hookEventName"),
        Some(&serde_json::json!("UserPromptSubmit"))
    );
    assert!(!String::from_utf8(output.stdout)?.contains("private-feature-name"));
    Ok(())
}

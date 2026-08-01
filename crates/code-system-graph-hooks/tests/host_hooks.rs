//! Cross-host self-tests for surgical installation and runtime policy.

use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use code_system_graph_hooks::{
    HookMode, HostKind, InstallRequest, RoutingIntent, RoutingRequest, classify_prompt, install, route, status, uninstall
};
use serde_json::json;
use tempfile::TempDir;

const HOSTS: &[HostKind] = &[
    HostKind::ClaudeCode,
    HostKind::Codex,
    HostKind::Gemini,
    HostKind::Antigravity,
    HostKind::Cursor,
];

#[test]
fn every_host_install_is_surgical_idempotent_and_uninstallable() -> Result<(), Box<dyn Error>> {
    for &host in HOSTS {
        let fixture = Fixture::new(host)?;
        fixture.seed_host_file()?;
        let request = fixture.request(HookMode::Advisory);

        let first = install(&request)?;
        assert!(first.changed, "first install did not change {host:?}");
        assert!(
            !first.backups.is_empty(),
            "existing host file was not backed up for {host:?}"
        );
        assert!(
            !first.warnings.is_empty(),
            "duplicate Code System Graph integration was not reported for {host:?}"
        );
        assert!(
            fs::read_to_string(&first.host_file)?.contains("keep-me"),
            "unrelated host content was replaced for {host:?}"
        );
        assert_eq!(
            fs::read_to_string(
                first
                    .gitignore_path
                    .as_deref()
                    .ok_or("Git worktree did not receive an ignore path")?,
            )?,
            ".code-system-graph/\n",
            "generated hook state was not ignored for {host:?}"
        );

        let second = install(&request)?;
        assert!(!second.changed, "second install changed {host:?}");
        assert!(
            !second.gitignore_updated,
            "second install duplicated the ignore rule for {host:?}"
        );
        assert!(
            second.backups.is_empty(),
            "idempotent install created a backup for {host:?}"
        );
        assert!(status(&request)?.installed, "status missed {host:?}");

        let removal = uninstall(&request)?;
        assert!(removal.changed, "uninstall changed nothing for {host:?}");
        if first.host_file.exists() {
            let remaining = fs::read_to_string(&first.host_file)?;
            assert!(
                remaining.contains("keep-me"),
                "uninstall deleted unrelated content for {host:?}"
            );
            assert!(
                !remaining.contains("# BEGIN code-system-graph-hooks:v1"),
                "uninstall left marker content for {host:?}"
            );
            assert!(
                !remaining.contains("--marker code-system-graph-hooks:v1"),
                "uninstall left JSON marker content for {host:?}"
            );
        }
        assert!(
            !status(&request)?.installed,
            "status remained installed for {host:?}"
        );
    }
    Ok(())
}

#[test]
fn every_host_reinstalls_changed_codegraph_policy() -> Result<(), Box<dyn Error>> {
    for &host in HOSTS {
        let fixture = Fixture::new(host)?;
        let native = fixture.request(HookMode::Advisory);
        install(&native)?;
        let host_file = host_file(fixture.root(), host);
        let native_content = fs::read_to_string(&host_file)?;
        if matches!(
            host,
            HostKind::ClaudeCode | HostKind::Codex | HostKind::Gemini
        ) {
            assert!(native_content.contains("--codegraph-enabled false"));
        } else {
            assert!(!native_content.contains("explore"));
        }

        let enriched = InstallRequest {
            codegraph_enabled: true,
            ..native.clone()
        };
        assert!(
            install(&enriched)?.changed,
            "policy change did not update {host:?}"
        );
        assert!(status(&enriched)?.installed);
        assert!(!status(&native)?.installed);

        let enriched_content = fs::read_to_string(&host_file)?;
        if matches!(
            host,
            HostKind::ClaudeCode | HostKind::Codex | HostKind::Gemini
        ) {
            assert!(enriched_content.contains("--codegraph-enabled true"));
        } else {
            assert!(enriched_content.contains("explore"));
        }
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn every_host_state_is_restrictive_and_strict_gate_fails_closed() -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::{PermissionsExt, symlink};

    for &host in HOSTS {
        let fixture = Fixture::new(host)?;
        let fake = fixture.root().join("fake-code-system-graph");
        fs::write(&fake, "#!/bin/sh\nexit 9\n")?;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o700))?;
        let request = InstallRequest {
            code_system_graph_binary: fake,
            ..fixture.request(HookMode::Strict)
        };
        let report = install(&request)?;
        let mode = fs::metadata(&report.state_file)?.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "state permissions differ for {host:?}");

        let gate = fixture.root().join(".git/hooks/pre-commit");
        let gate_source = fs::read_to_string(&gate)?;
        assert!(
            gate_source.contains("changes --scope staged"),
            "strict gate omitted staged changes for {host:?}"
        );
        assert!(
            gate_source.contains("exact_diff_fingerprint"),
            "strict gate omitted exact fingerprint for {host:?}"
        );
        assert!(
            !gate_source.contains(" scan ")
                && !gate_source.contains("codegraph init")
                && !gate_source.contains("codegraph sync"),
            "strict gate contains a forbidden operation for {host:?}"
        );

        let runner = fixture.root().join("run-pre-commit");
        symlink(&gate, &runner)?;
        let output = Command::new(&runner).current_dir(fixture.root()).output()?;
        assert!(
            !output.status.success(),
            "strict failure did not block for {host:?}"
        );
    }
    Ok(())
}

#[test]
fn every_host_routes_with_ttl_without_persisting_prompt_source() -> Result<(), Box<dyn Error>> {
    for &host in HOSTS {
        let fixture = Fixture::new(host)?;
        let prompt = "Show cross-repo API contract impact; source_password=never-store-this";
        let request = RoutingRequest {
            host,
            root: fixture.root().to_path_buf(),
            event: json!({"session_id": "session-1", "prompt": prompt}),
            codegraph_enabled: false,
            ttl_seconds: 300,
        };
        let first = route(&request)?;
        assert_eq!(first.intent, RoutingIntent::Federated);
        assert!(first.guidance.is_some(), "first route omitted {host:?}");
        let second = route(&request)?;
        assert!(second.deduplicated, "TTL did not deduplicate {host:?}");
        assert!(
            second.guidance.is_none(),
            "TTL repeated guidance for {host:?}"
        );

        let state = fs::read_to_string(fixture.root().join(format!(
            ".code-system-graph/hooks/dedup-{}.json",
            host.as_str()
        )))?;
        assert!(
            !state.contains(prompt) && !state.contains("source_password"),
            "routing state retained prompt source for {host:?}"
        );
    }
    Ok(())
}

#[test]
fn every_host_runtime_fails_open_on_invalid_input() -> Result<(), Box<dyn Error>> {
    let binary = env!("CARGO_BIN_EXE_code-system-graph-hooks");
    for &host in HOSTS {
        let fixture = Fixture::new(host)?;
        let root = fixture.root().to_string_lossy().into_owned();
        let mut child = Command::new(binary)
            .args([
                "route",
                "--host",
                host.as_str(),
                "--root",
                &root,
                "--codegraph-enabled",
                "false",
                "--marker",
                "code-system-graph-hooks:v1",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or("child process stdin was unavailable")?;
        stdin.write_all(b"not-json")?;
        drop(stdin);
        let output = child.wait_with_output()?;
        assert!(output.status.success(), "advisory hook blocked {host:?}");
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert!(
            value.get("decision").and_then(serde_json::Value::as_str) != Some("deny")
                && value.get("continue").and_then(serde_json::Value::as_bool) != Some(false),
            "advisory output denied {host:?}"
        );
    }
    Ok(())
}

#[test]
fn classifier_prefers_federated_signals_over_local_signals() {
    assert_eq!(
        classify_prompt("Find this function's impact across repos"),
        RoutingIntent::Federated
    );
}

struct Fixture {
    directory: TempDir,
    host: HostKind,
}

impl Fixture {
    fn new(host: HostKind) -> Result<Self, Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        fs::create_dir_all(directory.path().join(".git/hooks"))?;
        Ok(Self { directory, host })
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn request(&self, mode: HookMode) -> InstallRequest {
        InstallRequest {
            root: self.root().to_path_buf(),
            host: self.host,
            mode,
            code_system_graph_binary: PathBuf::from("/usr/bin/csgraph"),
            database: self.root().join("mesh.sqlite"),
            workspace: "workspace-a".to_owned(),
            repository: "service-a".to_owned(),
            codegraph_enabled: false,
        }
    }

    fn seed_host_file(&self) -> Result<(), Box<dyn Error>> {
        let path = host_file(self.root(), self.host);
        let parent = path.parent().ok_or("host file has no parent")?;
        fs::create_dir_all(parent)?;
        let source = match self.host {
            HostKind::ClaudeCode | HostKind::Codex => {
                r#"{
  "keep-me": true,
  "hooks": {
    "UserPromptSubmit": [
      {"hooks": [{"type": "command", "command": "other-code-system-graph-hook"}]}
    ]
  }
}
"#
            }
            HostKind::Gemini => {
                r#"{
  "keep-me": true,
  "hooks": {
    "BeforeAgent": [
      {"hooks": [{"type": "command", "command": "other-code-system-graph-hook"}]}
    ]
  }
}
"#
            }
            HostKind::Antigravity => "keep-me\nOther Code System Graph guidance.\n",
            HostKind::Cursor => {
                "---\ndescription: keep-me\nalwaysApply: true\n---\n\nOther Code System Graph guidance.\n"
            }
            _ => return Err("unsupported test host".into()),
        };
        fs::write(path, source)?;
        Ok(())
    }
}

fn host_file(root: &Path, host: HostKind) -> PathBuf {
    root.join(match host {
        HostKind::ClaudeCode => ".claude/settings.local.json",
        HostKind::Codex => ".codex/hooks.json",
        HostKind::Gemini => ".gemini/settings.json",
        HostKind::Antigravity => ".agents/rules/code-system-graph-routing.md",
        HostKind::Cursor => ".cursor/rules/code-system-graph-routing.mdc",
        _ => ".code-system-graph/unsupported-host",
    })
}

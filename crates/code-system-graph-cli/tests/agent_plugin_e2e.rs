//! Portable generation, existing-plugin binding, and MCP-profile acceptance coverage.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};

use code_system_graph::{
    AgentPluginCreateMode, AgentPluginCreateReport, AgentPluginUninstallReport, scan_workspace
};
use rmcp::ServiceExt;
use rmcp::model::{ClientCapabilities, ClientInfo, Implementation};

const PLUGIN_SCHEMA: &str = include_str!(
    "../../code-system-graph-hooks/agent-integration-template/agent-plugin/schemas/1.0.0/plugin.schema.json"
);
const MCP_SCHEMA: &str = include_str!(
    "../../code-system-graph-hooks/agent-integration-template/agent-plugin/schemas/1.0.0/mcp.schema.json"
);
const LOCAL_BINDING: &str = ".local/code-system-graph/mcp-binding.json";

fn canonical_string(path: &std::path::Path) -> anyhow::Result<String> {
    Ok(std::fs::canonicalize(path)?.to_string_lossy().into_owned())
}

fn assert_generator_build(metadata: &serde_json::Value) {
    assert_eq!(metadata["version"], env!("CARGO_PKG_VERSION"));
    let fingerprint = metadata["binaryFingerprint"]
        .as_str()
        .expect("binary fingerprint");
    assert!(fingerprint.starts_with("blake3:"));
    assert_eq!(fingerprint.len(), "blake3:".len() + 64);
    assert!(metadata["sourceCommit"].is_string() || metadata["sourceCommit"].is_null());
    assert!(metadata["sourceDirty"].is_boolean() || metadata["sourceDirty"].is_null());
}

fn fixture_workspace(
    root: &std::path::Path,
) -> anyhow::Result<(std::path::PathBuf, std::path::PathBuf)> {
    fixture_workspace_named(root, "plugin-workspace")
}

fn fixture_workspace_named(
    root: &std::path::Path,
    workspace: &str,
) -> anyhow::Result<(std::path::PathBuf, std::path::PathBuf)> {
    let repository = root.join("répo with spaces");
    std::fs::create_dir_all(repository.join("src"))?;
    std::fs::write(
        repository.join("Cargo.toml"),
        "[package]\nname = \"plugin-fixture\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
    )?;
    std::fs::write(repository.join("src/lib.rs"), "pub fn boundary() {}\n")?;
    let manifest = root.join("code system graph.yaml");
    std::fs::write(
        &manifest,
        format!(
            "version: 1\nname: {}\nrepos:\n  app:\n    path: répo with spaces\n",
            serde_json::to_string(workspace)?
        ),
    )?;
    let database = root.join("graph data.db");
    scan_workspace(&manifest, &database)?;
    Ok((manifest, database))
}

#[test]
fn plugin_create_should_quote_workspace_names_in_skill_frontmatter() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (manifest, database) = fixture_workspace_named(temporary.path(), "plugin: workspace")?;
    let output = temporary.path().join("portable-plugin");

    let created = create_with_cli(&manifest, &database, &output, &[])?;
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let report: AgentPluginCreateReport = serde_json::from_slice(&created.stdout)?;
    let skill =
        std::fs::read_to_string(output.join(format!("skills/{}/SKILL.md", report.skill_name)))?;
    let frontmatter = skill
        .strip_prefix("---\n")
        .and_then(|value| value.split_once("\n---\n"))
        .map(|(frontmatter, _)| frontmatter)
        .ok_or_else(|| anyhow::anyhow!("generated skill frontmatter is missing"))?;
    let parsed: std::collections::BTreeMap<String, String> = serde_saphyr::from_str(frontmatter)?;

    assert_eq!(parsed.get("name"), Some(&report.skill_name));
    assert!(
        parsed
            .get("description")
            .is_some_and(|description| description.contains("`plugin: workspace`"))
    );
    Ok(())
}

fn create_with_cli(
    manifest: &std::path::Path,
    database: &std::path::Path,
    output: &std::path::Path,
    extra: &[&str],
) -> anyhow::Result<std::process::Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_csgraph"));
    command
        .args(["plugin", "create", "--output"])
        .arg(output)
        .arg("--config")
        .arg(manifest)
        .arg("--database")
        .arg(database)
        .args(extra);
    Ok(command.output()?)
}

fn fixture_base_plugin(root: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let base = root.join("hugint base plugin");
    std::fs::create_dir_all(base.join(".codex-plugin"))?;
    std::fs::create_dir_all(base.join("skills"))?;
    std::fs::write(
        base.join("plugin.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
            "name": "hugint-agent-plugin",
            "version": "0.2.1",
            "description": "Portable Hugint base"
        }))?,
    )?;
    let codegraph_server = serde_json::json!({
        "type": "stdio",
        "command": "codegraph",
        "args": ["serve", "--mcp"]
    });
    std::fs::write(
        base.join("mcp.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "$schema": "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json",
            "mcpServers": {
                "codegraph": codegraph_server
            }
        }))?,
    )?;
    std::fs::write(
        base.join(".codex-plugin/plugin.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "name": "hugint-agent-plugin",
            "version": "0.2.1+codex.local",
            "skills": "./skills/",
            "mcpServers": {
                "codegraph": codegraph_server
            }
        }))?,
    )?;
    std::fs::write(base.join(".gitignore"), "/.local/\n")?;
    std::fs::write(base.join("README.md"), "portable base\n")?;
    Ok(base)
}

fn bind_existing_with_cli(
    base: &std::path::Path,
    manifest: &std::path::Path,
    database: &std::path::Path,
    extra: &[&str],
) -> anyhow::Result<std::process::Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_csgraph"));
    command
        .args(["plugin", "create", "--output"])
        .arg(base)
        .args(["--mcp-server-name", "hugint-code-system-graph"])
        .args(["--routing-skill", "hugint-system-graph"])
        .arg("--config")
        .arg(manifest)
        .arg("--database")
        .arg(database)
        .args(extra);
    Ok(command.output()?)
}

fn uninstall_existing_with_cli(base: &std::path::Path) -> anyhow::Result<std::process::Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["plugin", "uninstall", "--output"])
        .arg(base)
        .args(["--mcp-server-name", "hugint-code-system-graph"])
        .args(["--routing-skill", "hugint-system-graph"])
        .output()?)
}

fn expanded_mcp_arguments(
    document: &serde_json::Value,
    server_name: &str,
    plugin_root: &std::path::Path,
) -> Vec<String> {
    document["mcpServers"][server_name]["args"]
        .as_array()
        .expect("MCP args")
        .iter()
        .map(|value| {
            value
                .as_str()
                .expect("string arg")
                .replace("${PLUGIN_ROOT}", &plugin_root.to_string_lossy())
        })
        .collect()
}

fn assert_openai_skill_metadata(
    path: &std::path::Path,
    display_name: &str,
    skill_name: &str,
) -> anyhow::Result<()> {
    let metadata: serde_json::Value = serde_saphyr::from_str(&std::fs::read_to_string(path)?)?;
    assert_eq!(metadata["interface"]["display_name"], display_name);
    assert_eq!(
        metadata["interface"]["short_description"],
        "Workspace graph navigation and impact"
    );
    assert!(
        metadata["interface"]["default_prompt"]
            .as_str()
            .is_some_and(|prompt| prompt.contains(&format!("${skill_name}")))
    );
    Ok(())
}

#[cfg(unix)]
fn fake_codegraph(directory: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let binary = directory.join("codegraph fake");
    std::fs::copy(
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/codegraph/fake/codegraph.py"),
        &binary,
    )?;
    let mut permissions = std::fs::metadata(&binary)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions)?;
    Ok(binary)
}

#[test]
fn plugin_create_should_render_official_structure_and_be_idempotent() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (manifest, database) = fixture_workspace(temporary.path())?;
    let output = temporary.path().join("portable plugin");

    let first = create_with_cli(&manifest, &database, &output, &[])?;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: AgentPluginCreateReport = serde_json::from_slice(&first.stdout)?;
    assert!(first.changed);
    assert_eq!(first.mode, AgentPluginCreateMode::CompletePlugin);
    assert_eq!(first.workspace, "plugin-workspace");
    assert!(
        first
            .plugin_name
            .starts_with("code-system-graph-plugin-workspace-")
    );
    assert_eq!(first.plugin_name, first.mcp_server_name);
    assert_eq!(first.plugin_name, first.skill_name);
    assert_eq!(first.activation_scope, "client_managed_project_local");
    assert_eq!(
        first.workspace_root,
        std::fs::canonicalize(temporary.path())?.to_string_lossy()
    );
    assert!(!first.codegraph_enabled);
    assert_eq!(
        first.binding,
        canonical_string(&output.join(LOCAL_BINDING))?
    );
    assert_eq!(first.files.len(), 7);

    let plugin: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output.join("plugin.json"))?)?;
    let plugin_schema: serde_json::Value = serde_json::from_str(PLUGIN_SCHEMA)?;
    assert!(jsonschema::validator_for(&plugin_schema)?.is_valid(&plugin));
    assert_eq!(
        plugin["$schema"],
        "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json"
    );
    assert_eq!(plugin["name"], first.plugin_name);
    assert_eq!(plugin["version"], "1.0.3");
    let mcp: serde_json::Value = serde_json::from_slice(&std::fs::read(output.join("mcp.json"))?)?;
    let mcp_schema: serde_json::Value = serde_json::from_str(MCP_SCHEMA)?;
    assert!(jsonschema::validator_for(&mcp_schema)?.is_valid(&mcp));
    let server = &mcp["mcpServers"][&first.mcp_server_name];
    assert_eq!(server["command"], "csgraph");
    assert_eq!(server["type"], "stdio");
    assert_eq!(
        server["args"],
        serde_json::json!([
            "mcp",
            "--binding",
            "${PLUGIN_ROOT}/.local/code-system-graph/mcp-binding.json"
        ])
    );
    let skill_path = output.join(format!("skills/{}/SKILL.md", first.skill_name));
    let skill = std::fs::read_to_string(skill_path)?;
    assert!(skill.starts_with(&format!("---\nname: \"{}\"\n", first.skill_name)));
    assert!(!skill.contains("\ncompatibility:"));
    assert!(!skill.contains("\nmetadata:"));
    assert!(!skill.contains("{{"));
    assert!(!skill.contains(&temporary.path().to_string_lossy().to_string()));
    assert!(!skill.contains("workspace.md"));
    assert!(!skill.contains("plugin create"));
    assert!(!skill.contains("local binding"));
    assert!(skill.contains("reports another workspace, stop using it"));
    assert!(skill.contains("Call `status` before"));
    assert!(skill.contains("Do not run `scan`, `sync`, `codegraph init`"));
    assert!(
        !output
            .join(format!("skills/{}/agents/openai.yaml", first.skill_name))
            .exists()
    );
    let operating_guide = std::fs::read_to_string(output.join(format!(
        "skills/{}/references/operating-guide.md",
        first.skill_name
    )))?;
    assert!(operating_guide.contains("Repository onboarding"));
    assert!(operating_guide.contains("Inspect the installed `csgraph` and `codegraph` versions"));
    assert!(!operating_guide.contains("This plugin"));
    assert!(!operating_guide.contains("workspace.md"));
    assert!(!operating_guide.contains("{{"));
    let gitignore = std::fs::read_to_string(output.join(".gitignore"))?;
    assert_eq!(gitignore, "/.local/\n");
    let binding: serde_json::Value = serde_json::from_slice(&std::fs::read(
        output.join(".local/code-system-graph/mcp-binding.json"),
    )?)?;
    assert_eq!(binding["generator"], "csgraph plugin binding");
    assert_generator_build(&binding["generatorBuild"]);
    assert_eq!(binding["workspace"], "plugin-workspace");
    assert_eq!(binding["config"], canonical_string(&manifest)?);
    assert_eq!(binding["database"], canonical_string(&database)?);
    assert!(!binding["codegraphEnabled"].as_bool().expect("boolean"));
    assert!(std::fs::read_to_string(output.join("LICENSE"))?.contains("Apache License"));

    let second = create_with_cli(&manifest, &database, &output, &[])?;
    assert!(second.status.success());
    let second: AgentPluginCreateReport = serde_json::from_slice(&second.stdout)?;
    assert!(!second.changed);
    Ok(())
}

#[test]
fn plugin_create_should_use_the_same_portable_identity_for_workspace_clones() -> anyhow::Result<()>
{
    let temporary = tempfile::tempdir()?;
    let first_root = temporary.path().join("first");
    let second_root = temporary.path().join("second");
    std::fs::create_dir_all(&first_root)?;
    std::fs::create_dir_all(&second_root)?;
    let (first_manifest, first_database) = fixture_workspace(&first_root)?;
    let (second_manifest, second_database) = fixture_workspace(&second_root)?;

    let first = create_with_cli(
        &first_manifest,
        &first_database,
        &first_root.join("plugin"),
        &[],
    )?;
    let second = create_with_cli(
        &second_manifest,
        &second_database,
        &second_root.join("plugin"),
        &[],
    )?;
    let first: AgentPluginCreateReport = serde_json::from_slice(&first.stdout)?;
    let second: AgentPluginCreateReport = serde_json::from_slice(&second.stdout)?;

    assert_eq!(first.plugin_name, second.plugin_name);
    assert_eq!(first.mcp_server_name, second.mcp_server_name);
    assert_eq!(first.skill_name, second.skill_name);
    for relative in [
        ".gitignore".to_owned(),
        "LICENSE".to_owned(),
        "mcp.json".to_owned(),
        "plugin.json".to_owned(),
        format!("skills/{}/SKILL.md", first.skill_name),
        format!("skills/{}/references/operating-guide.md", first.skill_name),
    ] {
        assert_eq!(
            std::fs::read(first_root.join("plugin").join(&relative))?,
            std::fs::read(second_root.join("plugin").join(&relative))?,
            "portable file differs: {relative}"
        );
    }
    assert_ne!(
        std::fs::read(first_root.join("plugin").join(LOCAL_BINDING))?,
        std::fs::read(second_root.join("plugin").join(LOCAL_BINDING))?
    );
    Ok(())
}

#[test]
fn plugin_create_should_reject_conflicts_without_changing_them() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (manifest, database) = fixture_workspace(temporary.path())?;
    let output = temporary.path().join("portable-plugin");
    std::fs::create_dir(&output)?;
    std::fs::write(output.join("owned.txt"), "keep me")?;

    let result = create_with_cli(&manifest, &database, &output, &[])?;

    assert!(!result.status.success());
    assert_eq!(result.status.code(), Some(5));
    assert_eq!(
        std::fs::read_to_string(output.join("owned.txt"))?,
        "keep me"
    );
    assert!(!output.join("plugin.json").exists());
    Ok(())
}

#[test]
fn plugin_create_should_preserve_invalid_manifest_exit_code() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (manifest, database) = fixture_workspace(temporary.path())?;
    std::fs::write(
        &manifest,
        "version: 1\nname: ''\nrepos:\n  app:\n    path: répo with spaces\n",
    )?;

    let result = create_with_cli(
        &manifest,
        &database,
        &temporary.path().join("portable-plugin"),
        &[],
    )?;

    assert!(!result.status.success());
    assert_eq!(result.status.code(), Some(2));
    Ok(())
}

#[test]
fn plugin_create_should_allow_and_report_a_stale_snapshot() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (manifest, database) = fixture_workspace(temporary.path())?;
    std::fs::write(
        &manifest,
        "version: 1\nname: plugin-workspace\nrepos:\n  app:\n    path: répo with spaces\n    useGitignore: true\n",
    )?;
    let output = temporary.path().join("stale-plugin");

    let result = create_with_cli(&manifest, &database, &output, &[])?;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: AgentPluginCreateReport = serde_json::from_slice(&result.stdout)?;

    assert!(report.changed);
    assert_ne!(
        report.snapshot_freshness,
        code_system_graph_model::OverallFreshness::Fresh
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn plugin_create_should_reject_a_symlink_output_without_touching_target() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (manifest, database) = fixture_workspace(temporary.path())?;
    let target = temporary.path().join("target");
    std::fs::create_dir(&target)?;
    let output = temporary.path().join("plugin-link");
    std::os::unix::fs::symlink(&target, &output)?;

    let result = create_with_cli(&manifest, &database, &output, &[])?;

    assert!(!result.status.success());
    assert!(std::fs::read_dir(&target)?.next().is_none());
    Ok(())
}

#[tokio::test]
async fn generated_mcp_should_handshake_with_read_only_profile() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (manifest, database) = fixture_workspace(temporary.path())?;
    let output = temporary.path().join("portable-plugin");
    let created = create_with_cli(&manifest, &database, &output, &[])?;
    assert!(created.status.success());
    let mcp: serde_json::Value = serde_json::from_slice(&std::fs::read(output.join("mcp.json"))?)?;
    let report: AgentPluginCreateReport = serde_json::from_slice(&created.stdout)?;
    let arguments = expanded_mcp_arguments(&mcp, &report.mcp_server_name, &output);
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("stdout"))?;
    let client = ClientInfo::new(
        ClientCapabilities::default(),
        Implementation::new("agent-plugin-e2e", env!("CARGO_PKG_VERSION")),
    );
    let mut service = client.serve((stdout, stdin)).await?;
    let names = service
        .list_all_tools()
        .await?
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect::<Vec<_>>();

    assert!(names.contains(&"status".to_owned()));
    assert!(names.contains(&"query".to_owned()));
    assert!(!names.contains(&"scan".to_owned()));
    assert!(!names.contains(&"explore".to_owned()));
    let _ = service.close().await;
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await??;
    assert!(status.success());
    Ok(())
}

#[test]
fn plugin_create_should_write_the_path_resolved_codegraph_binding() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (manifest, database) = fixture_workspace(temporary.path())?;
    let output = temporary.path().join("portable-plugin-codegraph-path");
    let created = create_with_cli(&manifest, &database, &output, &["--codegraph"])?;
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let binding: serde_json::Value = serde_json::from_slice(&std::fs::read(
        output.join(".local/code-system-graph/mcp-binding.json"),
    )?)?;
    assert!(binding["codegraphEnabled"].as_bool().expect("boolean"));
    assert!(binding["codegraphBinary"].is_null());
    assert!(!std::fs::read_to_string(output.join("mcp.json"))?.contains("{{"));
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn generated_codegraph_profile_should_expose_explore_without_admin_tools()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let (manifest, database) = fixture_workspace(temporary.path())?;
    let binary = fake_codegraph(temporary.path())?;
    let output = temporary.path().join("portable-plugin-codegraph");
    let created = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["plugin", "create", "--output"])
        .arg(&output)
        .arg("--config")
        .arg(&manifest)
        .arg("--database")
        .arg(&database)
        .arg("--codegraph")
        .arg("--codegraph-binary")
        .arg(&binary)
        .output()?;
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let binding_path = output.join(".local/code-system-graph/mcp-binding.json");
    let binding: serde_json::Value = serde_json::from_slice(&std::fs::read(&binding_path)?)?;
    assert!(binding["codegraphEnabled"].as_bool().expect("boolean"));
    assert_eq!(binding["codegraphBinary"], canonical_string(&binary)?);
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["mcp", "--binding"])
        .arg(binding_path)
        .env("CODE_SYSTEM_GRAPH_MCP_ADMIN", "0")
        .env("CODE_SYSTEM_GRAPH_CODEGRAPH", "0")
        .env("CODE_SYSTEM_GRAPH_CODEGRAPH_BINARY", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("stdout"))?;
    let client = ClientInfo::new(
        ClientCapabilities::default(),
        Implementation::new("agent-plugin-codegraph-e2e", env!("CARGO_PKG_VERSION")),
    );
    let mut service = client.serve((stdout, stdin)).await?;
    let names = service
        .list_all_tools()
        .await?
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect::<Vec<_>>();

    assert!(names.contains(&"explore".to_owned()));
    assert!(!names.contains(&"scan".to_owned()));
    let _ = service.close().await;
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await??;
    assert!(status.success());
    Ok(())
}

#[test]
fn plugin_create_should_compose_a_managed_integration_into_an_existing_plugin() -> anyhow::Result<()>
{
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let (manifest, database) = fixture_workspace(&workspace)?;
    let base = fixture_base_plugin(temporary.path())?;
    let original_mcp = std::fs::read(base.join("mcp.json"))?;
    let original_codex = std::fs::read(base.join(".codex-plugin/plugin.json"))?;

    let first = bind_existing_with_cli(&base, &manifest, &database, &[])?;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first: AgentPluginCreateReport = serde_json::from_slice(&first.stdout)?;
    assert!(first.changed);
    assert_eq!(first.mode, AgentPluginCreateMode::ExistingPlugin);
    assert_eq!(first.plugin_name, "hugint-agent-plugin");
    assert_eq!(first.mcp_server_name, "hugint-code-system-graph");
    assert_eq!(first.skill_name, "hugint-system-graph");
    assert_ne!(std::fs::read(base.join("mcp.json"))?, original_mcp);
    assert_ne!(
        std::fs::read(base.join(".codex-plugin/plugin.json"))?,
        original_codex
    );
    let portable: serde_json::Value =
        serde_json::from_slice(&std::fs::read(base.join("mcp.json"))?)?;
    assert!(portable["mcpServers"]["codegraph"].is_object());
    assert!(portable["mcpServers"]["hugint-code-system-graph"].is_object());
    let binding_path = base.join(".local/code-system-graph/mcp-binding.json");
    assert_eq!(first.binding, canonical_string(&binding_path)?);
    let binding: serde_json::Value = serde_json::from_slice(&std::fs::read(&binding_path)?)?;
    assert_eq!(binding["generator"], "csgraph plugin binding");
    assert_generator_build(&binding["generatorBuild"]);
    assert_eq!(binding["workspace"], "plugin-workspace");
    assert_eq!(binding["database"], canonical_string(&database)?);
    let receipt_path = base.join(".local/code-system-graph/plugin-integration.json");
    assert!(receipt_path.is_file());
    let receipt: serde_json::Value = serde_json::from_slice(&std::fs::read(receipt_path)?)?;
    assert_generator_build(&receipt["generatorBuild"]);
    assert!(base.join("skills/hugint-system-graph/SKILL.md").is_file());
    assert_openai_skill_metadata(
        &base.join("skills/hugint-system-graph/agents/openai.yaml"),
        "Code System Graph (plugin-workspace)",
        "hugint-system-graph",
    )?;
    assert!(!base.join(".local/assembled").exists());
    assert!(!base.join("skills/code-system-graph").exists());

    let second = bind_existing_with_cli(&base, &manifest, &database, &[])?;
    assert!(second.status.success());
    let second: AgentPluginCreateReport = serde_json::from_slice(&second.stdout)?;
    assert!(!second.changed);
    Ok(())
}

#[test]
fn existing_portable_plugin_should_not_receive_codex_only_skill_metadata() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let (manifest, database) = fixture_workspace(&workspace)?;
    let base = fixture_base_plugin(temporary.path())?;
    std::fs::remove_file(base.join(".codex-plugin/plugin.json"))?;

    let created = bind_existing_with_cli(&base, &manifest, &database, &[])?;

    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    assert!(base.join("skills/hugint-system-graph/SKILL.md").is_file());
    assert!(
        !base
            .join("skills/hugint-system-graph/agents/openai.yaml")
            .exists()
    );
    Ok(())
}

#[test]
fn plugin_uninstall_should_remove_only_unchanged_managed_components_and_allow_reinstall()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let (manifest, database) = fixture_workspace(&workspace)?;
    let base = fixture_base_plugin(temporary.path())?;
    let original_mcp = std::fs::read(base.join("mcp.json"))?;
    let original_codex = std::fs::read(base.join(".codex-plugin/plugin.json"))?;

    assert!(
        bind_existing_with_cli(&base, &manifest, &database, &[])?
            .status
            .success()
    );
    let removed = uninstall_existing_with_cli(&base)?;
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let report: AgentPluginUninstallReport = serde_json::from_slice(&removed.stdout)?;
    assert!(report.changed);
    assert_eq!(report.plugin_name, "hugint-agent-plugin");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(base.join("mcp.json"))?)?,
        serde_json::from_slice::<serde_json::Value>(&original_mcp)?
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(
            base.join(".codex-plugin/plugin.json")
        )?)?,
        serde_json::from_slice::<serde_json::Value>(&original_codex)?
    );
    assert!(!base.join("skills/hugint-system-graph").exists());
    assert!(!base.join(".local/code-system-graph").exists());
    assert_eq!(
        std::fs::read_to_string(base.join("README.md"))?,
        "portable base\n"
    );

    let reinstalled = bind_existing_with_cli(&base, &manifest, &database, &[])?;
    assert!(
        reinstalled.status.success(),
        "{}",
        String::from_utf8_lossy(&reinstalled.stderr)
    );
    assert!(base.join("skills/hugint-system-graph/SKILL.md").is_file());
    assert!(base.join(LOCAL_BINDING).is_file());
    Ok(())
}

#[test]
fn plugin_uninstall_should_refuse_to_delete_a_modified_managed_skill() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let (manifest, database) = fixture_workspace(&workspace)?;
    let base = fixture_base_plugin(temporary.path())?;
    assert!(
        bind_existing_with_cli(&base, &manifest, &database, &[])?
            .status
            .success()
    );
    let skill = base.join("skills/hugint-system-graph/SKILL.md");
    std::fs::write(&skill, "user-owned replacement\n")?;

    let refused = uninstall_existing_with_cli(&base)?;
    assert!(!refused.status.success());
    assert_eq!(refused.status.code(), Some(5));
    assert_eq!(std::fs::read_to_string(&skill)?, "user-owned replacement\n");
    assert!(base.join(LOCAL_BINDING).is_file());
    let portable: serde_json::Value =
        serde_json::from_slice(&std::fs::read(base.join("mcp.json"))?)?;
    assert!(portable["mcpServers"]["hugint-code-system-graph"].is_object());
    Ok(())
}

#[test]
fn plugin_create_should_replace_only_an_owned_existing_plugin_binding() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let (manifest, database) = fixture_workspace(&workspace)?;
    let base = fixture_base_plugin(temporary.path())?;
    assert!(
        bind_existing_with_cli(&base, &manifest, &database, &[])?
            .status
            .success()
    );
    let binding_path = base.join(".local/code-system-graph/mcp-binding.json");
    let mut binding: serde_json::Value = serde_json::from_slice(&std::fs::read(&binding_path)?)?;
    binding["generator"] = serde_json::json!("csgraph plugin compose");
    binding["workspace"] = serde_json::json!("locally-edited");
    binding
        .as_object_mut()
        .expect("binding object")
        .remove("generatorBuild");
    std::fs::write(&binding_path, serde_json::to_vec_pretty(&binding)?)?;

    let conflict = bind_existing_with_cli(&base, &manifest, &database, &[])?;
    assert!(!conflict.status.success());
    let unchanged: serde_json::Value = serde_json::from_slice(&std::fs::read(&binding_path)?)?;
    assert_eq!(unchanged["workspace"], "locally-edited");

    let replaced = bind_existing_with_cli(&base, &manifest, &database, &["--replace-generated"])?;
    assert!(
        replaced.status.success(),
        "{}",
        String::from_utf8_lossy(&replaced.stderr)
    );
    let restored: serde_json::Value = serde_json::from_slice(&std::fs::read(&binding_path)?)?;
    assert_eq!(restored["workspace"], "plugin-workspace");
    Ok(())
}

#[test]
fn plugin_create_should_not_replace_unmanaged_local_state() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let (manifest, database) = fixture_workspace(&workspace)?;
    let base = fixture_base_plugin(temporary.path())?;
    let output = base.join(".local/code-system-graph");
    std::fs::create_dir_all(&output)?;
    std::fs::write(output.join("owned-by-user.txt"), "keep\n")?;

    let result = bind_existing_with_cli(&base, &manifest, &database, &["--replace-generated"])?;
    assert!(!result.status.success());
    assert_eq!(
        std::fs::read_to_string(output.join("owned-by-user.txt"))?,
        "keep\n"
    );
    Ok(())
}

#[tokio::test]
async fn existing_plugin_binding_should_start_the_read_only_mcp() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let (manifest, database) = fixture_workspace(&workspace)?;
    let base = fixture_base_plugin(temporary.path())?;
    let created = bind_existing_with_cli(&base, &manifest, &database, &[])?;
    assert!(created.status.success());
    let binding = base.join(".local/code-system-graph/mcp-binding.json");
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["mcp", "--binding"])
        .arg(binding)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("stdout"))?;
    let client = ClientInfo::new(
        ClientCapabilities::default(),
        Implementation::new("agent-plugin-binding-e2e", env!("CARGO_PKG_VERSION")),
    );
    let mut service = client.serve((stdout, stdin)).await?;
    let names = service
        .list_all_tools()
        .await?
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect::<Vec<_>>();
    assert!(names.contains(&"status".to_owned()));
    assert!(!names.contains(&"scan".to_owned()));
    assert!(!names.contains(&"explore".to_owned()));
    let _ = service.close().await;
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await??;
    assert!(status.success());
    Ok(())
}

//! MCP stdio protocol and capability-discovery acceptance tests.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;

use code_system_graph::scan_workspace;
use code_system_graph_store_sqlite::SqliteStore;
use rmcp::ServiceExt;
use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientInfo, Implementation, ReadResourceRequestParams, ResourceContents
};

#[cfg(unix)]
fn fake_codegraph(directory: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    let binary = directory.join("codegraph-ok");
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

#[tokio::test]
async fn stdio_should_initialize_without_noise_and_hide_admin_tools() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");
    let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/platform-demo/code-system-graph.yaml");
    scan_workspace(&fixture, &database)?;
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args([
            "mcp",
            "--database",
            database.to_string_lossy().as_ref(),
            "--workspace",
            "commerce-platform",
        ])
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
        Implementation::new("code-system-graph-e2e", env!("CARGO_PKG_VERSION")),
    );
    let mut service = client.serve((stdout, stdin)).await?;
    let tools = service.list_all_tools().await?;
    let names = tools
        .iter()
        .map(|tool| tool.name.as_ref())
        .collect::<Vec<_>>();

    assert!(names.contains(&"status"));
    assert!(names.contains(&"contracts"));
    assert!(!names.contains(&"explore"));
    assert!(!names.contains(&"scan"));
    assert!(!names.contains(&"update_workspace"));
    assert!(!names.contains(&"write_manual_link"));
    assert!(!names.contains(&"clean_cache"));
    assert!(!names.contains(&"recompute_communities"));

    let resources = service.list_all_resources().await?;
    let schema_uri = "code-system-graph://workspace/commerce-platform/schema";
    assert!(resources.iter().any(|resource| resource.uri == schema_uri));
    let schema = service
        .read_resource(ReadResourceRequestParams::new(schema_uri))
        .await?;
    let ResourceContents::TextResourceContents { text, .. } = &schema.contents[0] else {
        panic!("schema resource was not textual");
    };
    let schema: serde_json::Value = serde_json::from_str(text)?;
    assert!(schema["schemas"].is_object());
    assert!(schema["application_interfaces"]["schemas"].is_array());

    let contracts =
        service
            .call_tool(CallToolRequestParams::new("contracts").with_arguments(
                serde_json::from_value(serde_json::json!({
                    "workspace": "commerce-platform",
                    "action": "validate_all",
                    "limit": 100
                }))?,
            ))
            .await?;
    assert_ne!(contracts.is_error, Some(true));
    assert!(contracts.structured_content.is_some());

    let _ = service.close().await;
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await??;
    assert!(status.success());
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_explore_should_proxy_bounded_ephemeral_codegraph_context() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("api");
    std::fs::create_dir_all(repository.join("src"))?;
    std::fs::write(
        repository.join("Cargo.toml"),
        "[package]\nname = \"mcp-explore-api\"\nversion = \"1.0.0\"\nedition = \"2024\"\n",
    )?;
    std::fs::write(repository.join("src/lib.rs"), "fn create_order() {}\n")?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: mcp-explore\nrepos:\n  api:\n    path: api\n",
    )?;
    let database = temporary.path().join("graph.db");
    scan_workspace(&manifest, &database)?;
    let target = SqliteStore::open_read_only(&database)?
        .load_current_graph("mcp-explore")?
        .0
        .first()
        .ok_or_else(|| anyhow::anyhow!("impact target"))?
        .id
        .as_str()
        .to_owned();
    let binary = fake_codegraph(temporary.path())?;
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args([
            "mcp",
            "--database",
            database.to_string_lossy().as_ref(),
            "--workspace",
            "mcp-explore",
        ])
        .env("CODE_SYSTEM_GRAPH_CODEGRAPH_BINARY", binary)
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
        Implementation::new("code-system-graph-explore-e2e", env!("CARGO_PKG_VERSION")),
    );
    let mut service = client.serve((stdout, stdin)).await?;
    let explored =
        service
            .call_tool(CallToolRequestParams::new("explore").with_arguments(
                serde_json::from_value(serde_json::json!({
                    "workspace": "mcp-explore",
                    "query": "create_order callers",
                    "max_files": 4
                }))?,
            ))
            .await?;

    assert_eq!(
        explored
            .structured_content
            .as_ref()
            .and_then(|value| value["data"]["content"].as_str()),
        Some("ephemeral local context")
    );
    let impact = service
        .call_tool(
            CallToolRequestParams::new("impact").with_arguments(serde_json::from_value(
                serde_json::json!({
                    "target": {"kind": "node_id", "value": target},
                    "direction": "upstream"
                }),
            )?),
        )
        .await?;
    assert_ne!(impact.is_error, Some(true));
    assert!(
        impact
            .structured_content
            .as_ref()
            .is_some_and(|value| value["data"]["local_impact_summaries"].is_array())
    );
    let _ = service.close().await;
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await??;
    assert!(status.success());
    Ok(())
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "The protocol acceptance keeps one complete administrative stdio lifecycle together"
)]
async fn explicit_admin_stdio_should_apply_bounded_audited_mutations() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    for alias in ["repo-a", "repo-b", "repo-c"] {
        let repository = temporary.path().join(alias);
        std::fs::create_dir_all(repository.join("src"))?;
        std::fs::write(
            repository.join("Cargo.toml"),
            format!("[package]\nname = \"{alias}\"\nversion = \"1.0.0\"\nedition = \"2024\"\n"),
        )?;
        std::fs::write(repository.join("src/lib.rs"), "pub fn boundary() {}\n")?;
    }
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: admin-e2e\nrepos:\n  repo-a:\n    path: repo-a\n  repo-b:\n    path: repo-b\n",
    )?;
    let database = temporary.path().join("graph.db");
    scan_workspace(&manifest, &database)?;
    let (nodes, _) = SqliteStore::open_read_only(&database)?.load_current_graph("admin-e2e")?;
    let source = nodes
        .first()
        .ok_or_else(|| anyhow::anyhow!("source node"))?
        .id
        .as_str()
        .to_owned();
    let target = nodes
        .iter()
        .find(|node| node.id.as_str() != source)
        .ok_or_else(|| anyhow::anyhow!("target node"))?
        .id
        .as_str()
        .to_owned();

    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_csgraph"));
    command.args([
        "mcp",
        "--database",
        database.to_string_lossy().as_ref(),
        "--workspace",
        "admin-e2e",
        "--admin",
    ]);
    #[cfg(unix)]
    command.env(
        "CODE_SYSTEM_GRAPH_CODEGRAPH_BINARY",
        fake_codegraph(temporary.path())?,
    );
    let mut child = command
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
        Implementation::new("code-system-graph-admin-e2e", env!("CARGO_PKG_VERSION")),
    );
    let mut service = client.serve((stdout, stdin)).await?;
    let names = service
        .list_all_tools()
        .await?
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect::<Vec<_>>();
    for name in ["update_workspace", "write_manual_link", "clean_cache"] {
        assert!(names.iter().any(|candidate| candidate == name));
    }

    let manual = service
        .call_tool(
            CallToolRequestParams::new("write_manual_link").with_arguments(serde_json::from_value(
                serde_json::json!({
                    "action": "add",
                    "workspace": "admin-e2e",
                    "from": source,
                    "to": target,
                    "relation": "documents",
                    "reason": "Explicit E2E relationship"
                }),
            )?),
        )
        .await?;
    assert_ne!(manual.is_error, Some(true));
    assert_ne!(
        manual
            .structured_content
            .as_ref()
            .map(|value| &value["status"]),
        Some(&serde_json::Value::String("error".to_owned()))
    );

    let update = service
        .call_tool(
            CallToolRequestParams::new("update_workspace").with_arguments(serde_json::from_value(
                serde_json::json!({
                    "workspace": "admin-e2e",
                    "action": "add_repository",
                    "alias": "repo-c",
                    "repository_path": "repo-c"
                }),
            )?),
        )
        .await?;
    assert_ne!(update.is_error, Some(true));

    let clean = service
        .call_tool(CallToolRequestParams::new("clean_cache").with_arguments(
            serde_json::from_value(serde_json::json!({"workspace": "admin-e2e"}))?,
        ))
        .await?;
    assert_ne!(clean.is_error, Some(true));

    let scan = service
        .call_tool(
            CallToolRequestParams::new("scan").with_arguments(serde_json::from_value(
                serde_json::json!({
                    "workspace": "admin-e2e"
                }),
            )?),
        )
        .await?;
    assert_ne!(scan.is_error, Some(true));
    assert_ne!(
        scan.structured_content
            .as_ref()
            .map(|value| &value["status"]),
        Some(&serde_json::Value::String("error".to_owned()))
    );

    let _ = service.close().await;
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await??;
    assert!(status.success());
    let source = std::fs::read_to_string(&manifest)?;
    assert!(source.contains("manualLinks:"));
    assert!(source.contains("  repo-c:"));
    let store = SqliteStore::open_read_only(&database)?;
    let snapshot = store.current_snapshot_summary("admin-e2e")?;
    assert_eq!(store.load_manual_links(&snapshot.snapshot_id)?.len(), 1);
    Ok(())
}

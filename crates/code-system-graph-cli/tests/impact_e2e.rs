//! End-to-end acceptance tests for impact delivery and local enrichment.

use std::path::{Path, PathBuf};

#[cfg(unix)]
use code_system_graph::impact_workspace_with_codegraph;
use code_system_graph::{impact_workspace, scan_workspace};
use code_system_graph_core::{
    ImpactDirection, ImpactOptions, ImpactRequest, ImpactTarget, parse_manifest, register_workspace
};
use code_system_graph_model::{NodeId, ToolStatus, stable_id};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/platform-demo/code-system-graph.yaml")
}

fn repository_id(config: &Path, alias: &str) -> Result<String, Box<dyn std::error::Error>> {
    let source = std::fs::read_to_string(config)?;
    let manifest = parse_manifest(&source)?;
    let registry = register_workspace(config, &source, &manifest)?;
    registry
        .record
        .repositories
        .iter()
        .find(|repository| repository.alias == alias)
        .map(|repository| repository.id.as_str().to_owned())
        .ok_or_else(|| format!("fixture repository `{alias}` is missing").into())
}

fn request(target: String) -> ImpactRequest {
    ImpactRequest {
        target: ImpactTarget::NodeId {
            node_id: NodeId::new(target),
        },
        direction: ImpactDirection::Upstream,
        options: ImpactOptions {
            limit: 25,
            ..ImpactOptions::default()
        },
    }
}

#[test]
fn application_and_cli_should_return_bounded_impact_json() -> Result<(), Box<dyn std::error::Error>>
{
    let fixture = fixture();
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");
    scan_workspace(&fixture, &database)?;
    let api_repo = repository_id(&fixture, "api")?;
    let target = stable_id(
        "node",
        &format!("http:{api_repo}:provider:POST:/api/orders"),
    );
    let envelope = impact_workspace(&database, "commerce-platform", &request(target.clone()))?;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["impact", "--database"])
        .arg(&database)
        .args([
            "--workspace",
            "commerce-platform",
            "--target",
            &target,
            "--direction",
            "upstream",
            "--limit",
            "25",
        ])
        .output()?;
    let cli: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let report = envelope.data.expect("impact report");

    assert!(
        output.status.success()
            && matches!(envelope.status, ToolStatus::Ok | ToolStatus::Degraded)
            && !report.direct_consumers.is_empty()
            && cli["data"]["risk_model_version"] == "1.0.0"
            && cli["data"]["coverage"]["remediation"].is_array()
    );
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn codegraph_enrichment_should_remain_bounded_and_source_free()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;

    let fixture = fixture();
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");
    scan_workspace(&fixture, &database)?;
    let binary = temporary.path().join("codegraph-success");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/codegraph/fake/codegraph.py"),
        &binary,
    )?;
    let mut permissions = std::fs::metadata(&binary)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions)?;
    let api_repo = repository_id(&fixture, "api")?;
    let target = stable_id(
        "node",
        &format!("http:{api_repo}:provider:POST:/api/orders"),
    );
    let envelope = impact_workspace_with_codegraph(
        &database,
        "commerce-platform",
        &request(target),
        Some(binary.into_os_string()),
    )
    .await?;
    let report = envelope.data.expect("impact report");
    let serialized = serde_json::to_string(&report)?;

    assert!(
        !report.local_impact_summaries.is_empty()
            && report
                .local_impact_summaries
                .iter()
                .all(|summary| summary.affected_count <= 25)
            && !serialized.contains("fn create_order")
    );
    Ok(())
}

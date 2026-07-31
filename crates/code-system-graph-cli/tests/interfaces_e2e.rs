//! End-to-end acceptance tests for shared delivery interfaces.

use std::path::PathBuf;
use std::process::Command;

use code_system_graph::{contracts_workspace, doctor_workspace, export_workspace, scan_workspace};
use code_system_graph_core::{
    ContractAction, ContractRequest, DoctorStatus, ExportFormat, ExportRequest
};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/platform-demo/code-system-graph.yaml")
}

#[test]
fn contracts_exports_and_doctor_should_share_source_free_snapshot_services() -> anyhow::Result<()> {
    let fixture = fixture();
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");
    scan_workspace(&fixture, &database)?;

    let contracts = contracts_workspace(
        &database,
        "commerce-platform",
        &ContractRequest {
            action: ContractAction::List,
            limit: 25,
            ..ContractRequest::default()
        },
    )?;
    let json = export_workspace(
        &database,
        "commerce-platform",
        &ExportRequest {
            format: ExportFormat::Json,
            max_nodes: 100_000,
            max_edges: 1_000_000,
        },
    )?;
    let graphml = export_workspace(
        &database,
        "commerce-platform",
        &ExportRequest {
            format: ExportFormat::GraphMl,
            max_nodes: 100_000,
            max_edges: 1_000_000,
        },
    )?;
    let doctor = doctor_workspace(&fixture, &database)?;
    let serialized = serde_json::to_string(&contracts)?;

    assert!(!contracts.contracts.is_empty());
    assert!(json.content.starts_with('{'));
    assert!(graphml.content.starts_with("<?xml"));
    assert!(!json.truncated);
    assert!(matches!(
        doctor.status,
        DoctorStatus::Healthy | DoctorStatus::Unknown
    ));
    assert!(!serialized.contains("source_body"));

    let output = Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args([
            "contracts",
            "--database",
            database.to_string_lossy().as_ref(),
            "--workspace",
            "commerce-platform",
            "show",
            "node:missing",
        ])
        .output()?;
    assert_eq!(output.status.code(), Some(3));
    assert!(output.stdout.is_empty());
    Ok(())
}

#[test]
fn cli_should_generate_completions_without_protocol_noise() -> anyhow::Result<()> {
    let binary = env!("CARGO_BIN_EXE_csgraph");
    let output = Command::new(binary)
        .args(["completions", "bash"])
        .output()?;

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(String::from_utf8(output.stdout)?.contains("_csgraph"));
    Ok(())
}

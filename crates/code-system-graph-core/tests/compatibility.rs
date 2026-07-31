//! Acceptance fixtures for HTTP, package, and database compatibility.

use code_system_graph_core::{
    CompatibilityStatus, compare_database_contracts, compare_http_contracts, compare_package_contracts, extract_data_artifact, extract_openapi, extract_package_manifest
};
use code_system_graph_model::RepoId;

#[test]
fn breaking_fixtures_should_produce_evidenced_findings() -> Result<(), Box<dyn std::error::Error>> {
    let repository = RepoId::new("repo:contracts");
    let http_before = extract_openapi(
        &repository,
        "before.yaml",
        include_str!("../../../fixtures/contracts/compatibility/http/before.yaml"),
    )?;
    let http_after = extract_openapi(
        &repository,
        "after.yaml",
        include_str!("../../../fixtures/contracts/compatibility/http/after.yaml"),
    )?;
    let package_before = extract_package_manifest(
        "package.json",
        include_str!("../../../fixtures/contracts/compatibility/packages/before.json"),
    )?;
    let package_after = extract_package_manifest(
        "package.json",
        include_str!("../../../fixtures/contracts/compatibility/packages/after.json"),
    )?;
    let database_before = extract_data_artifact(
        "schema.sql",
        include_str!("../../../fixtures/contracts/compatibility/database/before.sql"),
    )?;
    let database_after = extract_data_artifact(
        "schema.sql",
        include_str!("../../../fixtures/contracts/compatibility/database/after.sql"),
    )?;
    let reports = [
        compare_http_contracts(&http_before, &http_after),
        compare_package_contracts(&package_before, &package_after),
        compare_database_contracts(&database_before, &database_after),
    ];

    assert!(reports.iter().all(|report| {
        report.status == CompatibilityStatus::Breaking
            && report.before_fingerprint != report.after_fingerprint
            && report.findings.iter().any(|finding| {
                finding.status == CompatibilityStatus::Breaking
                    && !finding.evidence.is_empty()
                    && !finding.recommended_validations.is_empty()
            })
    }));
    Ok(())
}

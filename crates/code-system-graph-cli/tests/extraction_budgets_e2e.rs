//! End-to-end extraction-budget reuse, secrecy, and atomic-publication coverage.

use code_system_graph::{
    ApplicationError, ScanOverrides, scan_workspace, scan_workspace_with_overrides
};
use code_system_graph_core::ExtractionResource;
use code_system_graph_store_sqlite::SqliteStore;

fn manifest(max_work: Option<u64>) -> String {
    let budgets = max_work.map_or_else(String::new, |maximum| {
        format!("extractionBudgets:\n  maxWorkUnitsPerArtifact: {maximum}\n")
    });
    format!("version: 1\nname: extraction-budget-e2e\n{budgets}repos:\n  api:\n    path: api\n")
}

fn graphql_with_exact_bytes(size: usize) -> String {
    let prefix = "type Query { viewer: String }\n#";
    assert!(size > prefix.len());
    format!("{prefix}{}", "x".repeat(size - prefix.len()))
}

#[test]
fn configured_openapi_budget_should_apply_during_extraction() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("api");
    std::fs::create_dir_all(&repository)?;
    std::fs::write(
        repository.join("openapi.yaml"),
        "openapi: 3.1.0\npaths:\n  /orders:\n    get: {}\n    post: {}\n",
    )?;
    let config = temporary.path().join("code-system-graph.yaml");
    let database = temporary.path().join("code-system-graph.db");
    std::fs::write(
        &config,
        "version: 1\nname: openapi-budget-e2e\nextractionBudgets:\n  maxObservationsPerArtifact: 1\nrepos:\n  api:\n    path: api\n    openapi: openapi.yaml\n",
    )?;

    assert!(matches!(
        scan_workspace(&config, &database),
        Err(ApplicationError::ExtractionLimit(error))
            if error.resource == ExtractionResource::Observations
                && error.artifact == "openapi.yaml"
                && error.extractor == "code-system-graph.http.openapi"
    ));
    Ok(())
}

#[test]
fn configured_source_value_budget_should_apply_before_focused_observations() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("api");
    std::fs::create_dir_all(repository.join("src"))?;
    std::fs::write(
        repository.join("src/routes.rs"),
        "fn focused_source_boundary() {}",
    )?;
    let config = temporary.path().join("code-system-graph.yaml");
    let database = temporary.path().join("code-system-graph.db");
    std::fs::write(
        &config,
        "version: 1\nname: source-budget-e2e\nextractionBudgets:\n  maxIdentifierBytesPerValue: 3\nrepos:\n  api:\n    path: api\n",
    )?;

    let result = scan_workspace(&config, &database);
    assert!(
        matches!(
            &result,
            Err(ApplicationError::ExtractionLimit(error))
                if error.resource == ExtractionResource::IdentifierBytesPerValue
                    && error.artifact == "src/routes.rs"
                    && error.extractor == "code-system-graph.source.rust"
        ),
        "unexpected source budget result: {result:?}"
    );
    Ok(())
}

#[test]
fn bounded_artifact_read_should_accept_below_and_exact_but_reject_maximum_plus_one()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("api");
    std::fs::create_dir_all(&repository)?;
    let artifact = repository.join("schema.graphql");
    let config = temporary.path().join("code-system-graph.yaml");
    let database = temporary.path().join("code-system-graph.db");
    std::fs::write(
        &config,
        "version: 1\nname: input-budget-e2e\nextractionBudgets:\n  maxInputBytesPerArtifact: 256\nrepos:\n  api:\n    path: api\n",
    )?;

    std::fs::write(&artifact, graphql_with_exact_bytes(255))?;
    assert!(scan_workspace(&config, &database).is_ok());
    std::fs::write(&artifact, graphql_with_exact_bytes(256))?;
    let exact = scan_workspace(&config, &database)?;
    std::fs::write(&artifact, graphql_with_exact_bytes(257))?;

    assert!(matches!(
        scan_workspace(&config, &database),
        Err(ApplicationError::ExtractionLimit(error))
            if error.observed == 257 && error.maximum == 256
    ));
    assert_eq!(
        SqliteStore::open_read_only(&database)?
            .current_snapshot_summary("input-budget-e2e")?
            .snapshot_id,
        exact.snapshot_id
    );
    Ok(())
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the atomicity scenario is clearest as one end-to-end test"
)]
fn changed_budgets_should_reextract_and_fail_atomically_without_leaking_literals()
-> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("api");
    std::fs::create_dir_all(&repository)?;
    std::fs::write(
        repository.join("schema.graphql"),
        r#"
            type User @key(fields: "private-federation-value") {
              lookup(token: String = "private-default-value"): String
            }
            type Query { viewer: User }
        "#,
    )?;
    let config = temporary.path().join("code-system-graph.yaml");
    let database = temporary.path().join("code-system-graph.db");
    std::fs::write(&config, manifest(None))?;

    let initial = scan_workspace(&config, &database)?;
    let initial_store = SqliteStore::open_read_only(&database)?;
    let initial_batches = initial_store.load_current_extractor_batches("extraction-budget-e2e")?;
    let initial_fingerprint = initial_batches
        .iter()
        .find(|batch| batch.source.extractor == "code-system-graph.graphql.document")
        .map(|batch| batch.budget_fingerprint.clone())
        .ok_or_else(|| anyhow::anyhow!("GraphQL batch was not persisted"))?;
    let persisted_bytes = initial_batches
        .iter()
        .flat_map(|batch| batch.payload.iter().copied())
        .collect::<Vec<_>>();
    let persisted_text = String::from_utf8_lossy(&persisted_bytes);
    assert!(!persisted_text.contains("private-federation-value"));
    assert!(!persisted_text.contains("private-default-value"));
    for suffix in [".work-v1.db", ".work-v1.db-wal", ".work-v1.db-shm"] {
        let path = temporary
            .path()
            .join(format!("code-system-graph.db{suffix}"));
        if path.exists() {
            let content = std::fs::read(path)?;
            for needle in [
                b"private-federation-value".as_slice(),
                b"private-default-value".as_slice(),
            ] {
                assert!(!content.windows(needle.len()).any(|window| window == needle));
            }
        }
    }
    drop(initial_store);

    std::fs::write(&config, manifest(Some(200_000)))?;
    let changed = scan_workspace(&config, &database)?;
    let changed_store = SqliteStore::open_read_only(&database)?;
    let changed_batches = changed_store.load_current_extractor_batches("extraction-budget-e2e")?;
    let changed_fingerprint = changed_batches
        .iter()
        .find(|batch| batch.source.extractor == "code-system-graph.graphql.document")
        .map(|batch| batch.budget_fingerprint.clone())
        .ok_or_else(|| anyhow::anyhow!("changed GraphQL batch was not persisted"))?;

    assert!(!changed.reused_snapshot);
    assert_ne!(changed.snapshot_id, initial.snapshot_id);
    assert_ne!(changed_fingerprint, initial_fingerprint);
    assert!(
        changed_batches
            .iter()
            .all(|batch| batch.extractor_version == "1.0.0")
    );
    drop(changed_store);

    std::fs::write(&config, manifest(Some(300_000)))?;
    let partial = scan_workspace_with_overrides(
        &config,
        &database,
        &ScanOverrides {
            repository: Some("api".to_owned()),
            ..ScanOverrides::default()
        },
    );
    assert!(matches!(
        partial,
        Err(ApplicationError::PartialScanBudgetChanged)
    ));
    assert_eq!(
        SqliteStore::open_read_only(&database)?
            .current_snapshot_summary("extraction-budget-e2e")?
            .snapshot_id,
        changed.snapshot_id
    );

    std::fs::write(
        &config,
        manifest(Some(1)).replace("maxWorkUnitsPerArtifact", "maxObservationsPerArtifact"),
    )?;
    let rejected = scan_workspace(&config, &database);
    assert!(
        matches!(
            &rejected,
            Err(ApplicationError::Graphql(_) | ApplicationError::ExtractionLimit(_))
        ),
        "unexpected rejection: {rejected:?}"
    );
    let surviving_store = SqliteStore::open_read_only(&database)?;
    let surviving = surviving_store.current_snapshot_summary("extraction-budget-e2e")?;
    let surviving_batches =
        surviving_store.load_current_extractor_batches("extraction-budget-e2e")?;
    assert_eq!(surviving.snapshot_id, changed.snapshot_id);
    assert_eq!(surviving_batches, changed_batches);
    Ok(())
}

#[test]
fn complete_batches_should_resume_from_sidecar_after_failed_pass() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("api");
    std::fs::create_dir_all(&repository)?;
    std::fs::write(repository.join("a.graphql"), "scalar A\n")?;
    std::fs::write(
        repository.join("z.graphql"),
        "type User @key(fields: \"id\") { lookup(token: String = \"x\"): String }\ntype Query { viewer: User }\n",
    )?;
    let config = temporary.path().join("code-system-graph.yaml");
    let database = temporary.path().join("code-system-graph.db");
    std::fs::write(
        &config,
        "version: 1\nname: resume-e2e\nextractionBudgets:\n  maxObservationsPerArtifact: 1\nrepos:\n  api:\n    path: api\n",
    )?;

    assert!(scan_workspace(&config, &database).is_err());
    std::fs::write(repository.join("z.graphql"), "scalar Z\n")?;
    let resumed = scan_workspace(&config, &database)?;

    assert!(resumed.execution.checkpoint_hits >= 1);
    assert!(resumed.execution.checkpoints_written >= 1);
    assert!(
        SqliteStore::open_read_only(&database)?
            .current_snapshot_summary("resume-e2e")
            .is_ok()
    );
    Ok(())
}

//! Acceptance coverage for configurable native discovery exclusions.

use code_system_graph::scan_workspace;
use code_system_graph_store_sqlite::SqliteStore;

#[test]
fn scan_should_apply_excludes_reopen_defaults_and_keep_explicit_artifacts() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("repo");
    for directory in [
        "src",
        "coverage",
        "vendor/internal-sdk/src",
        "vendor/external/src",
        "vendor/contracts",
    ] {
        std::fs::create_dir_all(repository.join(directory))?;
    }
    for source in [
        "src/lib.rs",
        "coverage/missed.rs",
        "vendor/internal-sdk/src/lib.rs",
        "vendor/external/src/lib.rs",
    ] {
        std::fs::write(repository.join(source), "pub fn observed() {}\n")?;
    }
    std::fs::write(
        repository.join("vendor/contracts/openapi.yaml"),
        "openapi: 3.1.0\ninfo:\n  title: Explicit\n  version: 1\npaths: {}\n",
    )?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: ignored\nrepos:\n  app:\n    path: repo\n    openapi: vendor/contracts/openapi.yaml\n    excludes:\n      - ./coverage//./**\n    includeDefaults:\n      - ./vendor//internal-sdk/./**\n",
    )?;
    let database = temporary.path().join("graph.db");

    scan_workspace(&manifest, &database)?;
    let paths = SqliteStore::open_read_only(&database)?
        .load_current_artifact_fingerprints("ignored")?
        .into_iter()
        .map(|fingerprint| fingerprint.path.display)
        .collect::<Vec<_>>();

    assert_eq!(
        (
            paths.contains(&"src/lib.rs".to_owned()),
            paths.contains(&"coverage/missed.rs".to_owned()),
            paths.contains(&"vendor/internal-sdk/src/lib.rs".to_owned()),
            paths.contains(&"vendor/external/src/lib.rs".to_owned()),
            paths.contains(&"vendor/contracts/openapi.yaml".to_owned()),
        ),
        (true, false, true, false, true)
    );
    Ok(())
}

#[test]
fn scan_should_invalidate_incremental_state_when_policy_changes() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("repo/src");
    std::fs::create_dir_all(&repository)?;
    std::fs::write(repository.join("lib.rs"), "pub fn observed() {}\n")?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: fingerprint\nrepos:\n  app:\n    path: repo\n",
    )?;
    let database = temporary.path().join("graph.db");
    let before = scan_workspace(&manifest, &database)?;
    std::fs::write(
        &manifest,
        "version: 1\nname: fingerprint\nrepos:\n  app:\n    path: repo\n    excludes:\n      - src/**\n",
    )?;

    let after = scan_workspace(&manifest, &database)?;

    assert!(!before.reused_snapshot);
    assert!(!after.reused_snapshot);
    assert!(after.discovered_input_count < before.discovered_input_count);
    Ok(())
}

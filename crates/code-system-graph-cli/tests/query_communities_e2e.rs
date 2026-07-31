//! End-to-end acceptance tests for query, traversal, and community delivery.

use std::path::{Path, PathBuf};

use code_system_graph::{
    CommunityInput, SearchInput, communities_workspace, scan_workspace, search_workspace, traverse_workspace
};
use code_system_graph_core::{
    TraversalAlgorithm, TraversalDirection, TraversalFilters, TraversalOptions, TraversalRequest, parse_manifest, register_workspace
};
use code_system_graph_model::{NodeId, ToolStatus, stable_id};
use code_system_graph_store_sqlite::SqliteStore;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/platform-demo/code-system-graph.yaml")
}

fn repository_id(config: &Path, alias: &str) -> anyhow::Result<String> {
    let source = std::fs::read_to_string(config)?;
    let manifest = parse_manifest(&source)?;
    let registry = register_workspace(config, &source, &manifest)?;
    registry
        .record
        .repositories
        .iter()
        .find(|repository| repository.alias == alias)
        .map(|repository| repository.id.as_str().to_owned())
        .ok_or_else(|| anyhow::anyhow!("fixture repository `{alias}` is missing"))
}

#[test]
fn queries_should_be_ranked_bounded_and_reproducible() -> anyhow::Result<()> {
    let fixture = fixture();
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");
    let first_scan = scan_workspace(&fixture, &database)?;
    let repeated_scan = scan_workspace(&fixture, &database)?;
    let input = SearchInput {
        query: "orders".to_owned(),
        node_kinds: Vec::new(),
        repo_ids: Vec::new(),
        service_ids: Vec::new(),
        community_ids: Vec::new(),
        offset: 0,
        limit: 5,
    };
    let first = search_workspace(&database, "commerce-platform", &input)?;
    let second = search_workspace(&database, "commerce-platform", &input)?;
    let first_report = first.data.as_ref().expect("search report");
    let second_report = second.data.as_ref().expect("second search report");
    let current_communities = SqliteStore::open_read_only(&database)?
        .load_current_community_snapshot("commerce-platform")?;

    assert_eq!(
        (
            first_scan.community_count,
            repeated_scan.reused_snapshot,
            first.status,
            first_report.hits.is_empty(),
            &first_report.hits,
            current_communities.snapshot_id.as_str(),
        ),
        (
            first_scan.community_count,
            true,
            ToolStatus::Ok,
            false,
            &second_report.hits,
            first_scan.snapshot_id.as_str(),
        )
    );
    Ok(())
}

#[test]
fn traversal_and_community_comparison_should_be_explainable() -> anyhow::Result<()> {
    let fixture = fixture();
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");
    let scan = scan_workspace(&fixture, &database)?;
    let web_repo = repository_id(&fixture, "web")?;
    let api_repo = repository_id(&fixture, "api")?;
    let traversal = traverse_workspace(
        &database,
        "commerce-platform",
        &TraversalRequest {
            start: NodeId::new(stable_id(
                "node",
                &format!("http:{web_repo}:consumer:POST:/api/orders"),
            )),
            target: NodeId::new(stable_id(
                "node",
                &format!("http:{api_repo}:provider:POST:/api/orders"),
            )),
            filters: TraversalFilters::default(),
            options: TraversalOptions {
                algorithm: TraversalAlgorithm::Dijkstra,
                direction: TraversalDirection::Outgoing,
                ..TraversalOptions::default()
            },
        },
    )?;
    let communities = communities_workspace(
        &database,
        "commerce-platform",
        &CommunityInput {
            community_id: None,
            compare_snapshot_id: Some(scan.snapshot_id),
            offset: 0,
            limit: 100,
        },
    )?;
    let traversal_report = traversal.data.expect("traversal report");
    let community_report = communities.data.expect("community report");
    assert!(
        community_report
            .communities
            .windows(2)
            .all(|pair| pair[0].metrics.size >= pair[1].metrics.size)
    );

    assert_eq!(
        (
            traversal.status,
            traversal_report.paths.len(),
            traversal_report.paths[0].segments.len(),
            community_report.total_communities,
            community_report.delta.expect("community delta").changes,
        ),
        (ToolStatus::Ok, 1, 1, scan.community_count, Vec::new(),)
    );
    Ok(())
}

#[test]
fn cli_should_expose_query_and_community_json() -> anyhow::Result<()> {
    let fixture = fixture();
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("graph.db");
    scan_workspace(&fixture, &database)?;
    let web_repo = repository_id(&fixture, "web")?;
    let api_repo = repository_id(&fixture, "api")?;
    let from = stable_id(
        "node",
        &format!("http:{web_repo}:consumer:POST:/api/orders"),
    );
    let to = stable_id(
        "node",
        &format!("http:{api_repo}:provider:POST:/api/orders"),
    );
    let query = std::process::Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["query", "orders", "--database"])
        .arg(&database)
        .args(["--workspace", "commerce-platform", "--limit", "2"])
        .output()?;
    let communities = std::process::Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["communities", "list", "--database"])
        .arg(&database)
        .args(["--workspace", "commerce-platform", "--limit", "2"])
        .output()?;
    let traversal = std::process::Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["traverse", "--database"])
        .arg(&database)
        .args([
            "--workspace",
            "commerce-platform",
            "--from",
            &from,
            "--to",
            &to,
            "--algorithm",
            "dijkstra",
        ])
        .output()?;
    let query_json: serde_json::Value = serde_json::from_slice(&query.stdout)?;
    let community_json: serde_json::Value = serde_json::from_slice(&communities.stdout)?;
    let traversal_json: serde_json::Value = serde_json::from_slice(&traversal.stdout)?;

    assert!(
        query.status.success()
            && communities.status.success()
            && traversal.status.success()
            && query_json["data"]["hits"].is_array()
            && community_json["data"]["communities"].is_array()
            && traversal_json["data"]["paths"].is_array()
    );
    Ok(())
}

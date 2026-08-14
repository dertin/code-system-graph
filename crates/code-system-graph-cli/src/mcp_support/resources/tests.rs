use super::*;
fn repository_freshness(name: &str) -> RepoFreshness {
    RepoFreshness {
        repo_id: code_system_graph_model::RepoId::new(format!("repo:{name}")),
        checkout_id: code_system_graph_model::CheckoutId::new(format!("checkout:{name}")),
        head_commit: Some(format!("commit-{name}")),
        manifest_hash: "manifest".to_owned(),
        state: RepoFreshnessState::WorkingTreeChanged,
        reason: Some(format!("{name} is stale")),
    }
}

fn extractor_run(name: &str) -> ExtractorRun {
    ExtractorRun {
        id: format!("run:{name}"),
        snapshot_id: "snapshot:one".to_owned(),
        repo_id: code_system_graph_model::RepoId::new(format!("repo:{name}")),
        checkout_id: code_system_graph_model::CheckoutId::new(format!("checkout:{name}")),
        extractor: name.to_owned(),
        extractor_version: "1".to_owned(),
        status: code_system_graph_model::ExtractorRunStatus::Success,
        discovered_files: 2,
        parsed_files: 2,
        skipped_files: 0,
        elapsed_ms: 1,
    }
}

fn empty_freshness_resource() -> FreshnessResource {
    bounded_freshness(
        FreshnessSummary {
            overall: OverallFreshness::Fresh,
            stale_repositories: Vec::new(),
            reasons: Vec::new(),
        },
        1,
    )
}

fn entities_resource() -> EntitiesResource {
    EntitiesResource {
        schema_version: 2,
        workspace: "workspace".to_owned(),
        entities: bounded_collection(Vec::new(), 1),
    }
}

fn workspace_resource_fixtures() -> Vec<(&'static str, ResourceDocument)> {
    vec![
        (
            "workspaces",
            ResourceDocument::Workspaces(WorkspacesResource {
                schema_version: 2,
                workspaces: bounded_collection(
                    [WorkspaceResourceItem {
                        name: "workspace".to_owned(),
                        configured: true,
                    }],
                    1,
                ),
            }),
        ),
        (
            "snapshot",
            ResourceDocument::Overview(OverviewResource {
                schema_version: 2,
                workspace: "workspace".to_owned(),
                snapshot: OverviewSnapshot {
                    id: "snapshot:one".to_owned(),
                    nodes: 1,
                    edges: 2,
                    evidence: 3,
                },
            }),
        ),
        (
            "Integrity ok",
            ResourceDocument::Status(StatusResource {
                schema_version: 2,
                status: GraphStatusReport {
                    workspace: "workspace".to_owned(),
                    schema_version: 2,
                    integrity_ok: true,
                    snapshot: SnapshotMetrics {
                        snapshot_id: "snapshot:one".to_owned(),
                        node_count: 1,
                        edge_count: 2,
                        evidence_count: 3,
                    },
                    repositories: Vec::new(),
                },
                repositories: bounded_collection(Vec::new(), 1),
                freshness: empty_freshness_resource(),
            }),
        ),
        (
            "repositories",
            ResourceDocument::Repositories(RepositoriesResource {
                schema_version: 2,
                workspace: "workspace".to_owned(),
                repositories: bounded_collection(Vec::new(), 1),
            }),
        ),
    ]
}

fn graph_resource_fixtures() -> Vec<(&'static str, ResourceDocument)> {
    vec![
        ("Entities", ResourceDocument::Services(entities_resource())),
        ("Entities", ResourceDocument::Contracts(entities_resource())),
        (
            "Communities",
            ResourceDocument::Communities(CommunitiesResource {
                schema_version: 2,
                workspace: "workspace".to_owned(),
                snapshot_id: "snapshot:one".to_owned(),
                engine_version: "1".to_owned(),
                config: community_config_view(
                    CommunityConfig {
                        algorithm: code_system_graph_model::CommunityAlgorithm::Louvain,
                        scope: code_system_graph_model::CommunityScope::Federated,
                        seed: 0,
                        resolution: 1.0,
                        minimum_confidence: 0.5,
                        edge_weights: Vec::new(),
                        max_iterations: 100,
                    },
                    1,
                ),
                communities: bounded_collection(Vec::new(), 1),
            }),
        ),
        (
            "Extractor runs",
            ResourceDocument::Coverage(CoverageResource {
                schema_version: 2,
                workspace: "workspace".to_owned(),
                runs: bounded_collection(Vec::new(), 1),
                freshness: empty_freshness_resource(),
            }),
        ),
        (
            "ev:one",
            ResourceDocument::Evidence(EvidenceResource {
                schema_version: 2,
                workspace: "workspace".to_owned(),
                evidence: Evidence {
                    id: code_system_graph_model::EvidenceId::new("ev:one"),
                    repo_id: None,
                    file_path: Some("src/lib.rs".to_owned()),
                    start_line: Some(1),
                    end_line: Some(1),
                    extractor: "fixture".to_owned(),
                    extractor_version: "1".to_owned(),
                    provenance: code_system_graph_model::Provenance::Extracted,
                    confidence: 1.0,
                    observed_at_commit: None,
                    content_hash: None,
                    note: None,
                },
            }),
        ),
        (
            "```json",
            ResourceDocument::SchemaCatalog(serde_json::json!({
                "query": {"type": "object"}
            })),
        ),
    ]
}

#[test]
fn every_typed_resource_variant_should_render_its_own_markdown_fixture() {
    let goldens = [
        "mcp-resource-golden:4a0739c1a776d2951cc3f3da46f8818ffd62562b4c023a49103abc31f09ee5a5",
        "mcp-resource-golden:5bf552e3d0bdda5e42c1cf1ee9ae5de13e263870165cc5b1ed9f5850b801ada2",
        "mcp-resource-golden:0bcdb56b1f90dc69cdb60ebd3816e410baa7e44e855fd97b668f2404fe6aa3bb",
        "mcp-resource-golden:c21d183ace7081c78f80ecb6f3ae117c7fdde9219436dbc25cabe99fe794941a",
        "mcp-resource-golden:1fc4418c3ef31389ac48f43c3785de986be8621a1e06bac9361f7cec220b2c66",
        "mcp-resource-golden:cd051baf008bc31e8c87d86bf6a95d71ffb931eaddc9a4b7a90cc3a5d71caf0d",
        "mcp-resource-golden:d5f3b700d3f70bb59c0a03407f752f754a5233ce8751b56cefffafb9f023145b",
        "mcp-resource-golden:0489a80e514c31d3f392dd464843579563baa7f8e3971e92d7a1fc466e457c68",
        "mcp-resource-golden:e4a27ab1170b53c17cb04f9ba84dee61806da6b493fa258444221da65897fd22",
        "mcp-resource-golden:4869cf77e9b9e91b47a11f153130722dd46277844b4302f6265dad76f9e30cce",
    ];
    let resources = workspace_resource_fixtures()
        .into_iter()
        .chain(graph_resource_fixtures());

    for (index, (expected, resource)) in resources.enumerate() {
        let rendered = resource.render(4_096);
        assert_eq!(
            code_system_graph_model::stable_id("mcp-resource-golden", &rendered),
            goldens[index],
            "resource fixture {index} changed its full Markdown"
        );
        assert!(rendered.contains(expected), "{expected}: {rendered}");
    }
}
#[test]
fn resource_freshness_collections_should_apply_item_limits() {
    let value = bounded_freshness(
        FreshnessSummary {
            overall: OverallFreshness::Partial,
            stale_repositories: vec![
                code_system_graph_model::RepoId::new("repo:one"),
                code_system_graph_model::RepoId::new("repo:two"),
            ],
            reasons: vec!["one".to_owned(), "two".to_owned()],
        },
        1,
    );

    assert_eq!(value.stale_repositories.total, 2);
    assert_eq!(value.stale_repositories.retained(), 1);
    assert!(value.stale_repositories.truncated());
    assert_eq!(value.reasons.total, 2);
    assert_eq!(value.reasons.retained(), 1);
    assert!(value.reasons.truncated());
}

#[test]
fn status_resource_should_limit_repositories_with_exact_metadata() {
    let repositories = vec![repository_freshness("one"), repository_freshness("two")];
    let value = status_resource_value(
        GraphStatusReport {
            workspace: "workspace".to_owned(),
            schema_version: 2,
            integrity_ok: true,
            snapshot: SnapshotMetrics {
                snapshot_id: "snapshot:one".to_owned(),
                node_count: 0,
                edge_count: 0,
                evidence_count: 0,
            },
            repositories: repositories.clone(),
        },
        freshness_summary(&repositories),
        1,
    );

    assert_eq!(value.repositories.total, 2);
    assert_eq!(value.repositories.retained(), 1);
    assert!(value.repositories.truncated());
    assert_eq!(
        value.status.repositories,
        [] as [code_system_graph_model::RepoFreshness; 0]
    );
}

#[test]
fn coverage_resource_should_limit_runs_with_exact_metadata() {
    let value = coverage_resource_value(
        "workspace",
        vec![extractor_run("one"), extractor_run("two")],
        FreshnessSummary {
            overall: OverallFreshness::Fresh,
            stale_repositories: Vec::new(),
            reasons: Vec::new(),
        },
        1,
    );

    assert_eq!(value.runs.total, 2);
    assert_eq!(value.runs.retained(), 1);
    assert!(value.runs.truncated());
    assert_eq!(value.runs.items.len(), 1);
}

#[test]
fn community_view_should_limit_every_nested_collection() {
    let community = Community {
        id: CommunityId::new("community:one"),
        label: "one".to_owned(),
        members: vec![NodeId::new("node:one"), NodeId::new("node:two")],
        central_nodes: vec![NodeId::new("node:one"), NodeId::new("node:two")],
        repositories: vec![RepoId::new("repo:one"), RepoId::new("repo:two")],
        services: vec![NodeId::new("service:one"), NodeId::new("service:two")],
        inbound_contracts: vec![NodeId::new("in:one"), NodeId::new("in:two")],
        outbound_contracts: vec![NodeId::new("out:one"), NodeId::new("out:two")],
        metrics: CommunityMetrics {
            size: 2,
            density: 1.0,
            cohesion: 1.0,
            coupling: 0.0,
            cross_community_edges: 0,
        },
        label_evidence: vec![
            CommunityLabelEvidence {
                node_id: NodeId::new("node:one"),
                term: "one".to_owned(),
            },
            CommunityLabelEvidence {
                node_id: NodeId::new("node:two"),
                term: "two".to_owned(),
            },
        ],
        limitations: vec!["one".to_owned(), "two".to_owned()],
    };
    let view = community_view(community, 1);

    for collection in [
        &view.members,
        &view.central_nodes,
        &view.services,
        &view.inbound_contracts,
        &view.outbound_contracts,
    ] {
        assert_eq!(collection.total, 2);
        assert_eq!(collection.retained(), 1);
        assert!(collection.truncated());
    }
    assert_eq!(view.repositories.retained(), 1);
    assert_eq!(view.label_evidence.retained(), 1);
    assert_eq!(view.limitations.retained(), 1);
}

#[test]
fn schema_catalog_should_apply_resource_item_limit() {
    let catalog = bounded_schema_catalog(1);

    assert_eq!(catalog["schema_retained"], 1);
    assert_eq!(
        catalog["schemas"].as_object().map(serde_json::Map::len),
        Some(1)
    );
    assert_eq!(catalog["schemas_truncated"], true);
    assert!(
        catalog["schema_total"]
            .as_u64()
            .is_some_and(|total| total > 1)
    );
}

use code_system_graph_core::{
    AgentNextAction, SearchCoverage, SearchExplanation, SearchHit, SearchReport
};
use code_system_graph_model::{
    CheckoutId, Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, EvidenceId, FreshnessSummary, Node, NodeId, NodeKind, OverallFreshness, Provenance, RepoFreshness, RepoFreshnessState, RepoId, ToolEnvelope, ToolStatus, TraceReport, TraceSegment
};

use super::agent_views::AgentPresentationContext;
use super::{
    AgentToolResult, GraphStatusReport, SnapshotMetrics, SourceContextRelation, SourceContextReport
};

fn node(id: &str, label: &str, kind: NodeKind, repository: &str) -> Node {
    Node {
        id: NodeId::new(id),
        kind,
        repo_id: Some(RepoId::new(repository)),
        stable_key: format!("{id}@v1"),
        label: label.to_owned(),
    }
}

fn edge(id: &str, source: &Node, target: &Node, kind: EdgeKind, evidence: &[&str]) -> Edge {
    Edge {
        id: EdgeId::new(id),
        source: source.id.clone(),
        target: target.id.clone(),
        kind,
        confidence: 0.98,
        status: EpistemicStatus::Confirmed,
        evidence: evidence.iter().map(|id| EvidenceId::new(*id)).collect(),
    }
}

fn evidence(id: &str, repository: &str, path: &str, line: u32) -> Evidence {
    Evidence {
        id: EvidenceId::new(id),
        repo_id: Some(RepoId::new(repository)),
        file_path: Some(path.to_owned()),
        start_line: Some(line),
        end_line: Some(line),
        extractor: "fixture".to_owned(),
        extractor_version: "1.1.0".to_owned(),
        provenance: Provenance::Extracted,
        confidence: 0.98,
        observed_at_commit: Some("abc123".to_owned()),
        content_hash: Some("content".to_owned()),
        note: Some("The call target was resolved from the declared API operation.".to_owned()),
    }
}

fn freshness() -> FreshnessSummary {
    FreshnessSummary {
        overall: OverallFreshness::Fresh,
        stale_repositories: Vec::new(),
        reasons: Vec::new(),
    }
}

struct Fixture {
    context: AgentPresentationContext,
    api: Node,
    database: Node,
    billing: Node,
    orphan: Node,
    reads: Edge,
    calls: Edge,
    reads_evidence: Evidence,
    calls_evidence: Evidence,
}

fn fixture() -> Fixture {
    let api = node(
        "node:orders-api",
        "GET /orders/{id}",
        NodeKind::HttpOperation,
        "repo:orders",
    );
    let database = node(
        "node:orders-table",
        "orders",
        NodeKind::DatabaseTable,
        "repo:orders",
    );
    let billing = node(
        "node:billing-client",
        "BillingOrderClient",
        NodeKind::SymbolRef,
        "repo:billing",
    );
    let orphan = node(
        "node:orphan",
        "Legacy note",
        NodeKind::Document,
        "repo:orders",
    );
    let reads = edge(
        "edge:reads",
        &api,
        &database,
        EdgeKind::ReadsTable,
        &["evidence:reads"],
    );
    let calls = edge(
        "edge:calls",
        &billing,
        &api,
        EdgeKind::CallsRemote,
        &["evidence:calls"],
    );
    let reads_evidence = evidence("evidence:reads", "repo:orders", "src/orders/query.rs", 41);
    let calls_evidence = evidence(
        "evidence:calls",
        "repo:billing",
        "src/clients/orders.ts",
        18,
    );
    let context = AgentPresentationContext::fixture(
        vec![
            (RepoId::new("repo:orders"), "orders-service".to_owned()),
            (RepoId::new("repo:billing"), "billing-service".to_owned()),
        ],
        vec![
            api.clone(),
            database.clone(),
            billing.clone(),
            orphan.clone(),
        ],
        vec![reads.clone(), calls.clone()],
        vec![reads_evidence.clone(), calls_evidence.clone()],
    );
    Fixture {
        context,
        api,
        database,
        billing,
        orphan,
        reads,
        calls,
        reads_evidence,
        calls_evidence,
    }
}

fn explanation() -> SearchExplanation {
    SearchExplanation {
        matched_fields: vec!["label".to_owned(), "stable key".to_owned()],
        exact_score: 12.0,
        prefix_score: 2.0,
        suffix_score: 0.0,
        fts_score: 1.0,
        type_score: 0.0,
        scope_score: 0.0,
        centrality_score: 0.5,
        community_score: 0.0,
        evidence_score: 1.0,
        freshness_penalty: 0.0,
    }
}

fn search_envelope(fixture: &Fixture) -> ToolEnvelope<SearchReport> {
    ToolEnvelope {
        schema_version: 2,
        status: ToolStatus::Degraded,
        data: Some(SearchReport {
            hits: vec![
                SearchHit {
                    node: fixture.api.clone(),
                    score: 16.5,
                    explanation: explanation(),
                },
                SearchHit {
                    node: fixture.orphan.clone(),
                    score: 3.25,
                    explanation: explanation(),
                },
            ],
            total_matches: 6,
            offset: 0,
            limit: 2,
            truncated: true,
            coverage: SearchCoverage {
                input_nodes: 4,
                eligible_nodes: 4,
                matched_nodes: 6,
                fts_scored_nodes: 4,
                freshness_unknown_repositories: Vec::new(),
                gaps: vec!["One optional source index was unavailable.".to_owned()],
            },
            next_actions: vec![AgentNextAction {
                tool: "source_context".to_owned(),
                arguments: std::collections::BTreeMap::from([(
                    "node_id".to_owned(),
                    fixture.api.id.as_str().to_owned(),
                )]),
                rationale: "Inspect all incoming and outgoing relationships.".to_owned(),
            }],
        }),
        freshness: freshness(),
        warnings: Vec::new(),
    }
}

fn single_search_envelope(node: Node) -> ToolEnvelope<SearchReport> {
    ToolEnvelope {
        schema_version: 2,
        status: ToolStatus::Ok,
        data: Some(SearchReport {
            hits: vec![SearchHit {
                node,
                score: 10.0,
                explanation: explanation(),
            }],
            total_matches: 1,
            offset: 0,
            limit: 5,
            truncated: false,
            coverage: SearchCoverage {
                input_nodes: 1,
                eligible_nodes: 1,
                matched_nodes: 1,
                fts_scored_nodes: 1,
                freshness_unknown_repositories: Vec::new(),
                gaps: Vec::new(),
            },
            next_actions: Vec::new(),
        }),
        freshness: freshness(),
        warnings: Vec::new(),
    }
}

#[test]
fn query_markdown_is_semantic_bounded_and_cross_repository_first() {
    let fixture = fixture();
    let envelope = search_envelope(&fixture);
    let result = AgentToolResult::Query(&envelope);
    let (markdown, is_error) = result.render(4_096, &fixture.context);

    assert!(!is_error);
    assert!(markdown.len() < 4_096, "{} bytes", markdown.len());
    assert!(markdown.contains("found 6 ranked results"), "{markdown}");
    assert!(
        markdown.contains(
            "symbol `BillingOrderClient` in repository `billing-service` → **calls remotely** → HTTP operation `GET /orders/{id}` in repository `orders-service`"
        ),
        "{markdown}"
    );
    assert!(markdown.contains("cross-repository"), "{markdown}");
    assert!(
        markdown.contains("`billing-service/src/clients/orders.ts:18`"),
        "{markdown}"
    );
    assert!(
        markdown.contains("No semantic dependency is attached to this entity"),
        "{markdown}"
    );
    assert!(
        !markdown.contains("16.5"),
        "score leaked into prose: {markdown}"
    );
    assert_no_transport_syntax(&markdown);
}

#[test]
fn repository_query_does_not_suggest_broad_source_exploration() {
    let repository = node(
        "node:platform",
        "hugint-platform",
        NodeKind::Repository,
        "repo:platform",
    );
    let documentation = node(
        "node:platform-readme",
        "hugint-platform README",
        NodeKind::Document,
        "repo:platform",
    );
    let context = AgentPresentationContext::fixture(
        vec![(RepoId::new("repo:platform"), "hugint-platform".to_owned())],
        vec![repository.clone(), documentation.clone()],
        Vec::new(),
        Vec::new(),
    );
    let envelope = ToolEnvelope {
        schema_version: 2,
        status: ToolStatus::Ok,
        data: Some(SearchReport {
            hits: vec![
                SearchHit {
                    node: repository.clone(),
                    score: 10.0,
                    explanation: explanation(),
                },
                SearchHit {
                    node: documentation,
                    score: 5.0,
                    explanation: explanation(),
                },
            ],
            total_matches: 2,
            offset: 0,
            limit: 5,
            truncated: false,
            coverage: SearchCoverage {
                input_nodes: 2,
                eligible_nodes: 2,
                matched_nodes: 2,
                fts_scored_nodes: 2,
                freshness_unknown_repositories: Vec::new(),
                gaps: Vec::new(),
            },
            next_actions: vec![
                AgentNextAction {
                    tool: "source_context".to_owned(),
                    arguments: std::collections::BTreeMap::from([(
                        "node_id".to_owned(),
                        repository.id.as_str().to_owned(),
                    )]),
                    rationale: "Inspect persisted relationships.".to_owned(),
                },
                AgentNextAction {
                    tool: "explore".to_owned(),
                    arguments: std::collections::BTreeMap::from([(
                        "repository".to_owned(),
                        "hugint-platform".to_owned(),
                    )]),
                    rationale: "Explore local source.".to_owned(),
                },
            ],
        }),
        freshness: freshness(),
        warnings: Vec::new(),
    };
    let result = AgentToolResult::Query(&envelope);
    let (markdown, _) = result.render(4_096, &context);
    let structured = result.structured_content(&context);

    assert!(!markdown.contains("use `explore`"), "{markdown}");
    assert_eq!(
        structured["data"]["next_actions"]
            .as_array()
            .expect("next actions")
            .iter()
            .map(|action| action["tool"].as_str().expect("tool"))
            .collect::<Vec<_>>(),
        ["source_context"]
    );
}

#[test]
fn repository_query_projects_cross_repository_table_usage_with_unique_owner() {
    let schema_repository = node(
        "node:schema-repository",
        "schema-db",
        NodeKind::Repository,
        "repo:schema",
    );
    let schema_artifact = node(
        "node:schema-artifact",
        "migrations/001_orders.sql",
        NodeKind::Artifact,
        "repo:schema",
    );
    let api_repository = node(
        "node:api-repository",
        "orders-api",
        NodeKind::Repository,
        "repo:api",
    );
    let writer = node(
        "node:persist-order",
        "persist_order",
        NodeKind::SymbolRef,
        "repo:api",
    );
    let table = Node {
        id: NodeId::new("node:orders-table"),
        kind: NodeKind::DatabaseTable,
        repo_id: None,
        stable_key: "table::commerce:orders".to_owned(),
        label: "orders".to_owned(),
    };
    let contains = edge(
        "edge:schema-contains-orders",
        &schema_artifact,
        &table,
        EdgeKind::Contains,
        &["evidence:schema"],
    );
    let write_edge = edge(
        "edge:api-writes-orders",
        &writer,
        &table,
        EdgeKind::WritesTable,
        &["evidence:writes"],
    );
    let context = AgentPresentationContext::fixture(
        vec![
            (RepoId::new("repo:schema"), "schema-db".to_owned()),
            (RepoId::new("repo:api"), "orders-api".to_owned()),
        ],
        vec![
            schema_repository,
            schema_artifact,
            api_repository.clone(),
            writer,
            table,
        ],
        vec![contains, write_edge],
        vec![
            evidence(
                "evidence:schema",
                "repo:schema",
                "migrations/001_orders.sql",
                1,
            ),
            evidence("evidence:writes", "repo:api", "src/orders.rs", 12),
        ],
    );
    let envelope = single_search_envelope(api_repository);
    let result = AgentToolResult::Query(&envelope);
    let (markdown, _) = result.render(8_192, &context);
    let structured = result.structured_content(&context);

    assert!(
        markdown.contains(
            "symbol `persist_order` in repository `orders-api` → **writes table** → database table `commerce.orders` defined in repository `schema-db`"
        ),
        "{markdown}"
    );
    assert!(markdown.contains("cross-repository"), "{markdown}");
    assert!(
        markdown.contains("`orders-api/src/orders.rs:12`"),
        "{markdown}"
    );
    assert!(
        markdown.contains("`schema-db/migrations/001_orders.sql:1`"),
        "{markdown}"
    );
    assert_eq!(
        structured["data"]["cross_repository_relations"][0]["target"]["repository_attribution"],
        "structural_owner"
    );
    assert_eq!(
        structured["data"]["cross_repository_relations"][0]["evidence"][0]["role"],
        "observed_relation"
    );
    assert_eq!(
        structured["data"]["cross_repository_relations"][0]["evidence"][1]["role"],
        "structural_attribution"
    );
}

#[test]
fn ambiguous_table_owners_remain_workspace_scoped() {
    let table = Node {
        id: NodeId::new("node:shared-table"),
        kind: NodeKind::DatabaseTable,
        repo_id: None,
        stable_key: "table::public:users".to_owned(),
        label: "users".to_owned(),
    };
    let left = node(
        "node:left-schema",
        "left.sql",
        NodeKind::Artifact,
        "repo:left",
    );
    let right = node(
        "node:right-schema",
        "right.sql",
        NodeKind::Artifact,
        "repo:right",
    );
    let reader = node(
        "node:reader",
        "load_users",
        NodeKind::SymbolRef,
        "repo:reader",
    );
    let relations = vec![
        edge("edge:left-owner", &left, &table, EdgeKind::Contains, &[]),
        edge("edge:right-owner", &right, &table, EdgeKind::Contains, &[]),
        edge(
            "edge:read-users",
            &reader,
            &table,
            EdgeKind::ReadsTable,
            &["evidence:read"],
        ),
    ];
    let context = AgentPresentationContext::fixture(
        vec![
            (RepoId::new("repo:left"), "schema-a".to_owned()),
            (RepoId::new("repo:right"), "schema-b".to_owned()),
            (RepoId::new("repo:reader"), "reader".to_owned()),
        ],
        vec![left, right, reader, table.clone()],
        relations,
        vec![evidence("evidence:read", "repo:reader", "reader.py", 4)],
    );
    let envelope = single_search_envelope(table);
    let result = AgentToolResult::Query(&envelope);
    let (markdown, _) = result.render(8_192, &context);
    let structured = result.structured_content(&context);

    assert!(
        markdown.contains("ambiguous repository ownership"),
        "{markdown}"
    );
    assert!(markdown.contains("`schema-a`"), "{markdown}");
    assert!(markdown.contains("`schema-b`"), "{markdown}");
    assert!(
        !markdown.contains("## Cross-repository relationships"),
        "{markdown}"
    );
    assert_eq!(
        structured["data"]["results"][0]["entity"]["repository_attribution"],
        "ambiguous"
    );
}

#[test]
fn internal_package_dependency_uses_both_unique_manifest_owners() {
    let checkout_manifest = node(
        "node:checkout-manifest",
        "package.json",
        NodeKind::Artifact,
        "repo:checkout",
    );
    let contracts_manifest = node(
        "node:contracts-manifest",
        "package.json",
        NodeKind::Artifact,
        "repo:contracts",
    );
    let checkout_package = Node {
        id: NodeId::new("node:checkout-package"),
        kind: NodeKind::Package,
        repo_id: None,
        stable_key: "package:npm:checkout-web".to_owned(),
        label: "npm:checkout-web".to_owned(),
    };
    let contracts_package = Node {
        id: NodeId::new("node:contracts-package"),
        kind: NodeKind::Package,
        repo_id: None,
        stable_key: "package:npm:@matrix/order-contracts".to_owned(),
        label: "npm:@matrix/order-contracts".to_owned(),
    };
    let dependency = edge(
        "edge:package-dependency",
        &checkout_package,
        &contracts_package,
        EdgeKind::DependsOnPackage,
        &["evidence:dependency"],
    );
    let context = AgentPresentationContext::fixture(
        vec![
            (RepoId::new("repo:checkout"), "checkout-web".to_owned()),
            (RepoId::new("repo:contracts"), "order-contracts".to_owned()),
        ],
        vec![
            checkout_manifest.clone(),
            contracts_manifest.clone(),
            checkout_package.clone(),
            contracts_package.clone(),
        ],
        vec![
            edge(
                "edge:checkout-owner",
                &checkout_manifest,
                &checkout_package,
                EdgeKind::Contains,
                &[],
            ),
            edge(
                "edge:contracts-owner",
                &contracts_manifest,
                &contracts_package,
                EdgeKind::Contains,
                &[],
            ),
            dependency,
        ],
        vec![evidence(
            "evidence:dependency",
            "repo:checkout",
            "package.json",
            5,
        )],
    );
    let envelope = single_search_envelope(contracts_package);
    let result = AgentToolResult::Query(&envelope);
    let (markdown, _) = result.render(8_192, &context);

    assert!(
        markdown.contains(
            "repository `checkout-web` → **depends on** → repository `order-contracts` through package `npm:@matrix/order-contracts`"
        ),
        "{markdown}"
    );
    assert!(markdown.contains("cross-repository"), "{markdown}");
}

#[test]
fn repository_query_collapses_exact_event_path_into_one_cross_repo_sentence() {
    let publisher_repository = node(
        "node:orders-repository",
        "orders-api",
        NodeKind::Repository,
        "repo:orders",
    );
    let subscriber_repository = node(
        "node:worker-repository",
        "fulfillment-worker",
        NodeKind::Repository,
        "repo:worker",
    );
    let publisher = node(
        "node:publisher",
        "publish_order_created",
        NodeKind::SymbolRef,
        "repo:orders",
    );
    let subscriber = node(
        "node:subscriber",
        "handle_order_created",
        NodeKind::SymbolRef,
        "repo:worker",
    );
    let channel = Node {
        id: NodeId::new("node:orders-created"),
        kind: NodeKind::EventChannel,
        repo_id: None,
        stable_key: "event:Kafka::orders.created".to_owned(),
        label: "orders.created".to_owned(),
    };
    let publish_edge = edge(
        "edge:publishes",
        &publisher,
        &channel,
        EdgeKind::Publishes,
        &["evidence:publish"],
    );
    let subscribe_edge = edge(
        "edge:subscribes",
        &subscriber,
        &channel,
        EdgeKind::Subscribes,
        &["evidence:subscribe"],
    );
    let context = AgentPresentationContext::fixture(
        vec![
            (RepoId::new("repo:orders"), "orders-api".to_owned()),
            (RepoId::new("repo:worker"), "fulfillment-worker".to_owned()),
        ],
        vec![
            publisher_repository.clone(),
            subscriber_repository,
            publisher,
            subscriber,
            channel,
        ],
        vec![publish_edge, subscribe_edge],
        vec![
            evidence("evidence:publish", "repo:orders", "src/events.rs", 20),
            evidence("evidence:subscribe", "repo:worker", "worker.py", 8),
        ],
    );
    let envelope = single_search_envelope(publisher_repository);
    let result = AgentToolResult::Query(&envelope);
    let (markdown, _) = result.render(8_192, &context);
    let structured = result.structured_content(&context);

    assert!(
        markdown.contains(
            "repository `orders-api` → **publishes event orders.created to** → repository `fulfillment-worker`"
        ),
        "{markdown}"
    );
    assert!(
        markdown.contains("`orders-api/src/events.rs:20`"),
        "{markdown}"
    );
    assert!(
        markdown.contains("`fulfillment-worker/worker.py:8`"),
        "{markdown}"
    );
    assert!(
        markdown.contains("Derived from observed publisher"),
        "{markdown}"
    );
    assert!(
        markdown.contains("ambiguous, confidence 0.50")
            && markdown.contains("without a namespace or cluster identity it remains ambiguous"),
        "{markdown}"
    );
    assert_eq!(
        markdown.matches("→ **publishes** →").count(),
        0,
        "{markdown}"
    );
    assert_eq!(
        structured["data"]["cross_repository_relations"][0]["derivation"],
        "event_delivery_path"
    );
}

#[test]
fn query_omits_structural_columns_when_the_semantic_table_is_present() {
    let reader = node(
        "node:reader",
        "load_orders",
        NodeKind::SymbolRef,
        "repo:reader",
    );
    let table = Node {
        id: NodeId::new("node:orders"),
        kind: NodeKind::DatabaseTable,
        repo_id: None,
        stable_key: "table::commerce:orders".to_owned(),
        label: "orders".to_owned(),
    };
    let column = Node {
        id: NodeId::new("node:orders-id"),
        kind: NodeKind::DatabaseColumn,
        repo_id: None,
        stable_key: "column:table::commerce:orders:id".to_owned(),
        label: "id".to_owned(),
    };
    let context = AgentPresentationContext::fixture(
        vec![(RepoId::new("repo:reader"), "reader".to_owned())],
        vec![reader.clone(), table.clone(), column.clone()],
        vec![
            edge("edge:column", &table, &column, EdgeKind::Contains, &[]),
            edge("edge:read", &reader, &table, EdgeKind::ReadsTable, &[]),
        ],
        Vec::new(),
    );
    let mut envelope = single_search_envelope(table);
    let report = envelope.data.as_mut().expect("search report");
    report.hits.push(SearchHit {
        node: column,
        score: 5.0,
        explanation: explanation(),
    });
    report.total_matches = 2;
    let result = AgentToolResult::Query(&envelope);
    let structured = result.structured_content(&context);

    assert_eq!(
        structured["data"]["results"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(structured["data"]["subordinate_matches_omitted"], 1);
}

#[test]
fn structured_query_mirrors_the_entities_and_relations_in_markdown() {
    let fixture = fixture();
    let envelope = search_envelope(&fixture);
    let result = AgentToolResult::Query(&envelope);
    let delivery = result.deliver(4_096, &fixture.context);
    let markdown = delivery.markdown;
    let structured = delivery.structured_content;

    assert_eq!(structured["schema_version"], 5);
    assert_eq!(structured["tool"], "query");
    assert_eq!(
        structured["data"]["results"][0]["entity"]["repository_alias"],
        "orders-service"
    );
    assert_eq!(
        structured["data"]["results"][0]["relation_previews"][0]["relationship"],
        "calls remotely"
    );
    assert_eq!(structured["data"]["results"][0]["score"], 16.5);
    assert!(markdown.contains("orders-service"));
    assert!(markdown.contains("calls remotely"));
}

#[test]
fn duplicate_query_hits_are_collapsed_under_four_kibibytes() {
    let fixture = fixture();
    let mut envelope = search_envelope(&fixture);
    let report = envelope.data.as_mut().expect("search report fixture");
    report.hits = (0..5)
        .map(|index| SearchHit {
            node: fixture.api.clone(),
            score: 16.5 - f64::from(index),
            explanation: explanation(),
        })
        .collect();
    report.total_matches = 6;
    report.limit = 5;

    let result = AgentToolResult::Query(&envelope);
    let (markdown, _) = result.render(64 * 1024, &fixture.context);
    let structured = result.structured_content(&fixture.context);
    assert!(
        markdown.len() < 4_096,
        "{} bytes:\n{markdown}",
        markdown.len()
    );
    assert_eq!(structured["data"]["raw_returned_matches"], 5);
    assert_eq!(
        structured["data"]["results"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(structured["data"]["collapsed_duplicate_matches"], 4);
    assert!(markdown.contains("4 duplicate graph representations were merged"));
    assert_no_transport_syntax(&markdown);
}

#[test]
fn five_distinct_query_results_keep_the_complete_transport_under_ten_kibibytes() {
    let fixture = fixture();
    let mut envelope = search_envelope(&fixture);
    let report = envelope.data.as_mut().expect("search report fixture");
    report.hits = (0..5)
        .map(|index| SearchHit {
            node: node(
                &format!("node:service-{index}"),
                &format!("architecture-service-{index}"),
                NodeKind::Service,
                "repo:orders",
            ),
            score: 16.5 - f64::from(index),
            explanation: explanation(),
        })
        .collect();
    report.total_matches = 5;
    report.limit = 5;
    report.truncated = false;

    let result = AgentToolResult::Query(&envelope);
    let (markdown, _) = result.render(64 * 1024, &fixture.context);
    let structured = result.structured_content(&fixture.context);
    let transport = serde_json::json!({
        "content": [{"type": "text", "text": markdown}],
        "structuredContent": structured,
        "isError": false
    });
    let bytes = serde_json::to_vec(&transport).expect("serializable MCP result");

    assert_eq!(
        transport["structuredContent"]["data"]["results"]
            .as_array()
            .map(Vec::len),
        Some(5)
    );
    assert!(bytes.len() < 10 * 1024, "{} bytes", bytes.len());
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "The fixture demonstrates duplicate roles, structural suppression, and a cross-repository preview together"
)]
fn query_collapses_file_roles_and_omits_structural_contains_noise() {
    let artifact = node(
        "node:artifact-architecture",
        "docs/architecture.md",
        NodeKind::Artifact,
        "repo:studio",
    );
    let document = node(
        "node:document-architecture",
        "docs/architecture.md",
        NodeKind::Document,
        "repo:studio",
    );
    let remote_repository = node(
        "node:repository-transpiler",
        "repo:internal-transpiler-hash",
        NodeKind::Repository,
        "repo:transpiler",
    );
    let contains = edge(
        "edge:contains-document",
        &artifact,
        &document,
        EdgeKind::Contains,
        &[],
    );
    let documents = edge(
        "edge:documents-transpiler",
        &document,
        &remote_repository,
        EdgeKind::Documents,
        &["evidence:repository-link"],
    );
    let repository_link = evidence(
        "evidence:repository-link",
        "repo:studio",
        "docs/architecture.md",
        12,
    );
    let context = AgentPresentationContext::fixture(
        vec![
            (RepoId::new("repo:studio"), "hugint-studio".to_owned()),
            (
                RepoId::new("repo:transpiler"),
                "hugint-transpiler".to_owned(),
            ),
        ],
        vec![artifact.clone(), document.clone(), remote_repository],
        vec![contains, documents],
        vec![repository_link],
    );
    let envelope = ToolEnvelope {
        schema_version: 2,
        status: ToolStatus::Ok,
        data: Some(SearchReport {
            hits: vec![
                SearchHit {
                    node: artifact,
                    score: 10.0,
                    explanation: explanation(),
                },
                SearchHit {
                    node: document,
                    score: 9.0,
                    explanation: explanation(),
                },
            ],
            total_matches: 2,
            offset: 0,
            limit: 5,
            truncated: false,
            coverage: SearchCoverage {
                input_nodes: 3,
                eligible_nodes: 3,
                matched_nodes: 2,
                fts_scored_nodes: 0,
                freshness_unknown_repositories: Vec::new(),
                gaps: Vec::new(),
            },
            next_actions: Vec::new(),
        }),
        freshness: freshness(),
        warnings: Vec::new(),
    };
    let result = AgentToolResult::Query(&envelope);
    let (markdown, _) = result.render(4_096, &context);
    let structured = result.structured_content(&context);

    assert_eq!(
        structured["data"]["results"].as_array().map(Vec::len),
        Some(1)
    );
    assert_eq!(structured["data"]["collapsed_duplicate_matches"], 1);
    assert_eq!(
        structured["data"]["results"][0]["roles"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );
    assert_eq!(
        structured["data"]["results"][0]["relation_previews"][0]["relationship"],
        "documents"
    );
    assert!(
        markdown.contains("Roles: document and artifact"),
        "{markdown}"
    );
    assert!(markdown.contains("cross-repository"), "{markdown}");
    assert!(!markdown.contains("**contains**"), "{markdown}");
}

#[path = "presentation_goldens/delivery.rs"]
mod delivery;
fn assert_no_transport_syntax(markdown: &str) {
    for forbidden in [
        "SearchHit {",
        "NodeId(",
        "RepoId(",
        "EdgeId(",
        "EvidenceId(",
        "## Offset",
        "## Limit",
        "## Truncated",
        "## Total matches",
        "```json",
    ] {
        assert!(
            !markdown.contains(forbidden),
            "found {forbidden:?} in:\n{markdown}"
        );
    }
}

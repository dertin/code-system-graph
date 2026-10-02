use super::*;

#[test]
fn source_context_separates_outgoing_and_incoming_observed_edges() {
    let fixture = fixture();
    let report = SourceContextReport {
        workspace: "hugint".to_owned(),
        entity: fixture.api.clone(),
        relations: vec![
            SourceContextRelation {
                edge: fixture.reads.clone(),
                source: fixture.api.clone(),
                target: fixture.database.clone(),
            },
            SourceContextRelation {
                edge: fixture.calls.clone(),
                source: fixture.billing.clone(),
                target: fixture.api.clone(),
            },
        ],
        evidence: vec![
            fixture.reads_evidence.clone(),
            fixture.calls_evidence.clone(),
        ],
        total_relations: 2,
        structural_relations_omitted: 0,
        relations_truncated: false,
        total_evidence: 2,
        evidence_truncated: false,
    };
    let envelope = ToolEnvelope {
        schema_version: 2,
        status: ToolStatus::Ok,
        data: Some(report),
        freshness: freshness(),
        warnings: Vec::new(),
    };
    let result = AgentToolResult::SourceContext(&envelope);
    let (markdown, is_error) = result.render(8_192, &fixture.context);
    let structured = result.structured_content(&fixture.context);

    assert!(!is_error);
    assert!(markdown.contains("## Outgoing relationships"), "{markdown}");
    assert!(markdown.contains("**reads table**"), "{markdown}");
    assert!(markdown.contains("## Incoming relationships"), "{markdown}");
    assert!(markdown.contains("**calls remotely**"), "{markdown}");
    assert_eq!(
        structured["data"]["outgoing_relations"]
            .as_array()
            .expect("outgoing relations")
            .len(),
        1
    );
    assert_eq!(
        structured["data"]["incoming_relations"]
            .as_array()
            .expect("incoming relations")
            .len(),
        1
    );
    assert!(structured["data"].get("relations").is_none());
    assert!(structured["data"].get("evidence").is_none());
    assert_no_transport_syntax(&markdown);
}

#[test]
fn source_context_markdown_lists_every_retained_evidence_location() {
    let mut fixture = fixture();
    let second_evidence = evidence(
        "evidence:reads-second",
        "repo:orders",
        "src/orders/query.rs",
        73,
    );
    fixture.reads.evidence.push(second_evidence.id.clone());
    fixture.context = AgentPresentationContext::fixture(
        vec![
            (RepoId::new("repo:orders"), "orders-service".to_owned()),
            (RepoId::new("repo:billing"), "billing-service".to_owned()),
        ],
        vec![
            fixture.api.clone(),
            fixture.database.clone(),
            fixture.billing.clone(),
            fixture.orphan.clone(),
        ],
        vec![fixture.reads.clone(), fixture.calls.clone()],
        vec![
            fixture.reads_evidence.clone(),
            second_evidence.clone(),
            fixture.calls_evidence.clone(),
        ],
    );
    let report = SourceContextReport {
        workspace: "hugint".to_owned(),
        entity: fixture.api.clone(),
        relations: vec![SourceContextRelation {
            edge: fixture.reads.clone(),
            source: fixture.api.clone(),
            target: fixture.database.clone(),
        }],
        evidence: vec![fixture.reads_evidence.clone(), second_evidence],
        total_relations: 1,
        structural_relations_omitted: 0,
        relations_truncated: false,
        total_evidence: 2,
        evidence_truncated: false,
    };
    let envelope = ToolEnvelope {
        schema_version: 2,
        status: ToolStatus::Ok,
        data: Some(report),
        freshness: freshness(),
        warnings: Vec::new(),
    };

    let (markdown, _) = AgentToolResult::SourceContext(&envelope).render(8_192, &fixture.context);

    assert!(
        markdown.contains(
            "`orders-service/src/orders/query.rs:41`, `orders-service/src/orders/query.rs:73`"
        ),
        "{markdown}"
    );
    assert_no_transport_syntax(&markdown);
}

#[test]
fn source_context_explains_missing_and_truncated_evidence() {
    let fixture = fixture();
    let mut edge_without_retained_evidence = fixture.reads.clone();
    edge_without_retained_evidence.evidence.clear();
    let report = SourceContextReport {
        workspace: "hugint".to_owned(),
        entity: fixture.api.clone(),
        relations: vec![SourceContextRelation {
            edge: edge_without_retained_evidence,
            source: fixture.api.clone(),
            target: fixture.database.clone(),
        }],
        evidence: Vec::new(),
        total_relations: 1,
        structural_relations_omitted: 0,
        relations_truncated: false,
        total_evidence: 3,
        evidence_truncated: true,
    };
    let envelope = ToolEnvelope {
        schema_version: 2,
        status: ToolStatus::Degraded,
        data: Some(report),
        freshness: FreshnessSummary {
            overall: OverallFreshness::Stale,
            stale_repositories: vec![RepoId::new("repo:orders")],
            reasons: vec!["orders-service changed after the snapshot.".to_owned()],
        },
        warnings: Vec::new(),
    };
    let (markdown, _) = AgentToolResult::SourceContext(&envelope).render(8_192, &fixture.context);
    assert!(
        markdown.contains("No evidence location is included"),
        "{markdown}"
    );
    assert!(
        markdown.contains("Some supporting evidence locations were omitted"),
        "{markdown}"
    );
    assert!(!markdown.contains("## Known gaps"), "{markdown}");
    assert!(!markdown.contains("Graph freshness is stale"), "{markdown}");
    assert!(
        !markdown.contains("orders-service changed after the snapshot"),
        "repository-specific freshness details belong in status, not every contextual response: {markdown}"
    );
    assert_no_transport_syntax(&markdown);
}

#[test]
fn trace_reads_as_an_observed_chain() {
    let fixture = fixture();
    let envelope = ToolEnvelope {
        schema_version: 2,
        status: ToolStatus::Ok,
        data: Some(TraceReport {
            segments: vec![
                TraceSegment {
                    source: fixture.billing.clone(),
                    edge: fixture.calls.clone(),
                    target: fixture.api.clone(),
                },
                TraceSegment {
                    source: fixture.api.clone(),
                    edge: fixture.reads.clone(),
                    target: fixture.database.clone(),
                },
            ],
            truncated: false,
            coverage_gaps: Vec::new(),
        }),
        freshness: freshness(),
        warnings: Vec::new(),
    };
    let (markdown, _) = AgentToolResult::Trace(&envelope).render(8_192, &fixture.context);
    assert!(
        markdown.contains("Found a chain of 2 observed relationships"),
        "{markdown}"
    );
    assert!(markdown.contains("## Hop 1"), "{markdown}");
    assert!(markdown.contains("**calls remotely**"), "{markdown}");
    assert!(markdown.contains("## Hop 2"), "{markdown}");
    assert!(markdown.contains("**reads table**"), "{markdown}");
    assert_no_transport_syntax(&markdown);
}

#[test]
fn response_limit_keeps_complete_semantic_blocks() {
    let fixture = fixture();
    let envelope = search_envelope(&fixture);
    let (markdown, _) = AgentToolResult::Query(&envelope).render(240, &fixture.context);
    assert!(
        markdown.starts_with("# Architecture search results"),
        "{markdown}"
    );
    assert!(
        markdown.contains("Response shortened at a complete semantic section"),
        "{markdown}"
    );
    assert!(
        !markdown.contains("## 1."),
        "a partial result block leaked: {markdown}"
    );
}

#[test]
fn healthy_status_is_under_750_bytes_and_uses_aliases() {
    let fixture = fixture();
    let envelope = ToolEnvelope {
        schema_version: 2,
        status: ToolStatus::Ok,
        data: Some(GraphStatusReport {
            workspace: "hugint".to_owned(),
            schema_version: 5,
            integrity_ok: true,
            snapshot: SnapshotMetrics {
                snapshot_id: "snapshot:current".to_owned(),
                node_count: 120,
                edge_count: 85,
                evidence_count: 73,
            },
            repositories: vec![RepoFreshness {
                repo_id: RepoId::new("repo:orders"),
                checkout_id: CheckoutId::new("checkout:orders"),
                head_commit: Some("abc123".to_owned()),
                manifest_hash: "manifest".to_owned(),
                state: RepoFreshnessState::Fresh,
                reason: None,
            }],
        }),
        freshness: freshness(),
        warnings: Vec::new(),
    };
    let (markdown, is_error) = AgentToolResult::Status(&envelope).render(4_096, &fixture.context);

    assert!(!is_error);
    assert!(markdown.len() < 750, "{} bytes: {markdown}", markdown.len());
    assert!(markdown.contains("fresh graph"), "{markdown}");
    assert!(!markdown.contains("repo:orders"), "{markdown}");
    assert_no_transport_syntax(&markdown);
}

#[test]
fn compact_error_remains_human_readable() {
    let fixture = fixture();
    let envelope = ToolEnvelope::<SearchReport> {
        schema_version: 2,
        status: ToolStatus::Error,
        data: None,
        freshness: FreshnessSummary {
            overall: OverallFreshness::Unknown,
            stale_repositories: Vec::new(),
            reasons: vec!["The graph snapshot could not be loaded.".to_owned()],
        },
        warnings: vec!["Database is unavailable.".to_owned()],
    };
    let (markdown, is_error) = AgentToolResult::Query(&envelope).render(512, &fixture.context);
    assert!(is_error);
    assert!(
        markdown.contains("could not produce semantic result data"),
        "{markdown}"
    );
    assert!(markdown.contains("Database is unavailable"), "{markdown}");
    assert_no_transport_syntax(&markdown);
}

//! Acceptance gates for breaking propagation and stale false-safe prevention.

use std::collections::BTreeMap;

use code_system_graph_core::{
    CompatibilityInput, ImpactCompatibilityStatus, ImpactContext, ImpactDirection, ImpactOptions, ImpactRequest, ImpactTarget, RiskLevel, analyze_impact
};
use code_system_graph_model::{
    CheckoutId, Edge, EdgeId, EdgeKind, EpistemicStatus, Node, NodeId, NodeKind, RepoFreshness, RepoFreshnessState, RepoId
};

fn node(id: &str, repository: &str, kind: NodeKind) -> Node {
    Node {
        id: NodeId::new(id),
        kind,
        repo_id: Some(RepoId::new(repository)),
        stable_key: id.to_owned(),
        label: id.to_owned(),
    }
}

fn edge(id: &str, source: &str, target: &str) -> Edge {
    Edge {
        id: EdgeId::new(id),
        source: NodeId::new(source),
        target: NodeId::new(target),
        kind: EdgeKind::Consumes,
        confidence: 1.0,
        status: EpistemicStatus::Confirmed,
        evidence: Vec::new(),
    }
}

fn freshness(repository: &str, state: RepoFreshnessState) -> RepoFreshness {
    RepoFreshness {
        repo_id: RepoId::new(repository),
        checkout_id: CheckoutId::new(format!("checkout:{repository}")),
        head_commit: Some("0123456789abcdef".to_owned()),
        manifest_hash: "manifest".to_owned(),
        state,
        reason: None,
    }
}

fn context(state: RepoFreshnessState) -> ImpactContext {
    ImpactContext {
        nodes: vec![
            node("contract", "repo:api", NodeKind::HttpOperation),
            node("direct", "repo:web", NodeKind::Service),
            node("transitive", "repo:e2e", NodeKind::TestCase),
        ],
        edges: vec![
            edge("edge:direct", "direct", "contract"),
            edge("edge:transitive", "transitive", "direct"),
        ],
        communities: None,
        freshness: vec![
            freshness("repo:api", state),
            freshness("repo:web", RepoFreshnessState::Fresh),
            freshness("repo:e2e", RepoFreshnessState::Fresh),
        ],
        compatibility: vec![CompatibilityInput {
            contract_node_id: NodeId::new("contract"),
            status: ImpactCompatibilityStatus::Breaking,
            evidence: vec!["http.operation_removed".to_owned()],
            recommended_validations: vec!["run HTTP contract tests".to_owned()],
        }],
        local_enrichment: Vec::new(),
        public_contracts: vec![NodeId::new("contract")],
        criticality: Vec::new(),
        centrality: BTreeMap::new(),
        service_memberships: BTreeMap::new(),
        environments: Vec::new(),
        recommended_commands: Vec::new(),
        graph_complete: true,
        coverage_gaps: Vec::new(),
    }
}

fn request() -> ImpactRequest {
    ImpactRequest {
        target: ImpactTarget::NodeId(NodeId::new("contract")),
        direction: ImpactDirection::Upstream,
        options: ImpactOptions::default(),
    }
}

#[test]
fn partial_impact_options_should_inherit_individual_defaults()
-> Result<(), Box<dyn std::error::Error>> {
    let request: ImpactRequest = serde_json::from_value(serde_json::json!({
        "target": {"kind": "node_id", "value": "contract"},
        "direction": "upstream",
        "options": {"limit": 7}
    }))?;

    assert_eq!(
        (request.options.max_depth, request.options.limit),
        (ImpactOptions::default().max_depth, 7)
    );
    assert!(request.options.include_depth_buckets);
    Ok(())
}

#[test]
fn breaking_contract_should_propagate_direct_and_transitive_impact()
-> Result<(), Box<dyn std::error::Error>> {
    let report = analyze_impact(&request(), &context(RepoFreshnessState::Fresh))?;

    assert!(
        report.risk == RiskLevel::High
            && report
                .direct_consumers
                .iter()
                .any(|item| item.node.id == NodeId::new("direct"))
            && report
                .transitive_consumers
                .iter()
                .any(|item| item.node.id == NodeId::new("transitive"))
            && report
                .reasons
                .iter()
                .any(|factor| factor.code == "breaking_compatibility")
    );
    Ok(())
}

#[test]
fn stale_contract_repository_should_never_return_a_false_safe_score()
-> Result<(), Box<dyn std::error::Error>> {
    let report = analyze_impact(&request(), &context(RepoFreshnessState::CommitsBehind))?;

    assert!(
        report.risk == RiskLevel::Unknown
            && report.risk_score.is_none()
            && !report.coverage.stale_repositories.is_empty()
            && !report.coverage.remediation.is_empty()
    );
    Ok(())
}

//! Release-mode scale acceptance for the `Code System Graph` 1.0.0 workstation targets.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use code_system_graph_core::{
    FederatedGraph, ImpactContext, ImpactDirection, ImpactOptions, ImpactRequest, ImpactTarget, SearchFilters, SearchRequest, analyze_communities, analyze_impact, search
};
use code_system_graph_model::{
    CommunityAlgorithm, CommunityConfig, CommunityScope, Edge, EdgeId, EdgeKind, EpistemicStatus, Node, NodeId, NodeKind, RepoId
};

const REPOSITORY_COUNT: usize = 100;
const NODE_COUNT: usize = 100_000;
const EDGE_COUNT: usize = 500_000;
const SAMPLES: usize = 5;

#[test]
#[ignore = "run explicitly with --release for the scale acceptance workload"]
fn scale_workload_should_meet_release_candidate_latency_targets() {
    let (nodes, edges) = scale_graph();

    let query_p95 = percentile_95((0..SAMPLES).map(|sample| {
        timed(|| {
            let report = search(
                &nodes,
                &SearchRequest {
                    query: format!("boundary-{}", sample * 10_000),
                    filters: SearchFilters::default(),
                    fts_scores: BTreeMap::new(),
                    centrality_scores: BTreeMap::new(),
                    service_memberships: BTreeMap::new(),
                    community_memberships: BTreeMap::new(),
                    evidence: BTreeMap::new(),
                    freshness: BTreeMap::new(),
                    offset: 0,
                    limit: 20,
                },
            );
            assert!(report.is_ok(), "scale search failed: {report:?}");
        })
    }));

    let graph = FederatedGraph::new(nodes.clone(), edges.clone())
        .unwrap_or_else(|error| panic!("scale graph must be valid: {error}"));
    let trace_p95 = percentile_95((0..SAMPLES).map(|_| {
        timed(|| {
            let report = graph.trace(&NodeId::new("node:0"), &NodeId::new("node:8"), 8);
            assert!(report.is_ok(), "scale trace failed: {report:?}");
        })
    }));
    drop(graph);

    let impact_context = impact_context(nodes.clone(), edges.clone());
    let impact_p95 = percentile_95((0..SAMPLES).map(|_| {
        timed(|| {
            let report = analyze_impact(
                &ImpactRequest {
                    target: ImpactTarget::NodeId {
                        node_id: NodeId::new("node:0"),
                    },
                    direction: ImpactDirection::Downstream,
                    options: ImpactOptions {
                        max_depth: 1,
                        node_limit: NODE_COUNT,
                        edge_limit: EDGE_COUNT,
                        summary_only: true,
                        include_depth_buckets: false,
                        ..ImpactOptions::default()
                    },
                },
                &impact_context,
            );
            assert!(report.is_ok(), "scale impact failed: {report:?}");
        })
    }));
    drop(impact_context);

    let community_elapsed = timed(|| {
        let report = analyze_communities(
            "snapshot:scale",
            &nodes,
            &edges,
            CommunityConfig {
                algorithm: CommunityAlgorithm::ConnectedComponents,
                scope: CommunityScope::Federated,
                seed: 1,
                resolution: 1.0,
                minimum_confidence: 0.8,
                edge_weights: Vec::new(),
                max_iterations: 10,
            },
        );
        assert!(
            report.is_ok(),
            "scale community analysis failed: {report:?}"
        );
    });

    eprintln!(
        "graph_scale repos={REPOSITORY_COUNT} nodes={NODE_COUNT} edges={EDGE_COUNT} \
         query_p95_ms={} trace_p95_ms={} impact_p95_ms={} community_ms={} rss_kib={:?}",
        query_p95.as_millis(),
        trace_p95.as_millis(),
        impact_p95.as_millis(),
        community_elapsed.as_millis(),
        resident_memory_kib()
    );
    assert!(
        query_p95 < Duration::from_millis(500),
        "warm query p95 was {query_p95:?}"
    );
    assert!(
        trace_p95 < Duration::from_secs(1),
        "trace p95 was {trace_p95:?}"
    );
    assert!(
        impact_p95 < Duration::from_secs(2),
        "summary impact p95 was {impact_p95:?}"
    );
}

fn scale_graph() -> (Vec<Node>, Vec<Edge>) {
    let nodes = (0..NODE_COUNT)
        .map(|index| Node {
            id: NodeId::new(format!("node:{index}")),
            kind: NodeKind::HttpOperation,
            repo_id: Some(RepoId::new(format!("repo:{}", index % REPOSITORY_COUNT))),
            stable_key: format!("http:boundary-{index}"),
            label: format!("GET /boundary-{index}"),
        })
        .collect::<Vec<_>>();
    let edges = (0..EDGE_COUNT)
        .map(|index| {
            let (source, target) = if index < NODE_COUNT - 1 {
                (index, index + 1)
            } else {
                (0, (index % (NODE_COUNT - 1)) + 1)
            };
            Edge {
                id: EdgeId::new(format!("edge:{index}")),
                source: NodeId::new(format!("node:{source}")),
                target: NodeId::new(format!("node:{target}")),
                kind: EdgeKind::CallsRemote,
                confidence: 1.0,
                status: EpistemicStatus::Confirmed,
                evidence: Vec::new(),
            }
        })
        .collect();
    (nodes, edges)
}

fn impact_context(nodes: Vec<Node>, edges: Vec<Edge>) -> ImpactContext {
    ImpactContext {
        nodes,
        edges,
        communities: None,
        freshness: Vec::new(),
        compatibility: Vec::new(),
        local_enrichment: Vec::new(),
        public_contracts: Vec::new(),
        criticality: Vec::new(),
        centrality: BTreeMap::new(),
        service_memberships: BTreeMap::new(),
        environments: Vec::new(),
        recommended_commands: Vec::new(),
        graph_complete: true,
        coverage_gaps: Vec::new(),
    }
}

fn timed(operation: impl FnOnce()) -> Duration {
    let started = Instant::now();
    operation();
    started.elapsed()
}

fn percentile_95(samples: impl Iterator<Item = Duration>) -> Duration {
    let mut values = samples.collect::<Vec<_>>();
    values.sort_unstable();
    let index = values
        .len()
        .saturating_mul(95)
        .div_ceil(100)
        .saturating_sub(1);
    values[index]
}

#[cfg(target_os = "linux")]
fn resident_memory_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[cfg(not(target_os = "linux"))]
fn resident_memory_kib() -> Option<u64> {
    None
}

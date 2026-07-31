//! Deterministic community detection and comparison for federated graphs.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use code_system_graph_model::{
    Community, CommunityAlgorithm, CommunityChange, CommunityChangeKind, CommunityConfig, CommunityDelta, CommunityEdgeWeight, CommunityId, CommunityLabelEvidence, CommunityMetrics, CommunityScope, CommunitySnapshot, Edge, EdgeKind, EpistemicStatus, Node, NodeId, NodeKind, RepoId, stable_id
};
use thiserror::Error;

const ENGINE_VERSION: &str = "1.0.0";
const EPSILON: f64 = 1.0e-12;
const PAGE_RANK_DAMPING: f64 = 0.85;
const PAGE_RANK_ITERATIONS: usize = 64;
const RELATED_OVERLAP: f64 = 0.25;
const STABLE_OVERLAP: f64 = 0.80;
const EDGE_KIND_COUNT: usize = 25;

/// Failure returned when community-analysis inputs violate their documented invariants.
#[derive(Debug, Error, PartialEq)]
pub enum CommunityError {
    /// The Louvain resolution is not finite and strictly positive.
    #[error("community resolution must be finite and positive, found {0}")]
    InvalidResolution(f64),
    /// The iteration limit is outside the supported inclusive range.
    #[error("community max_iterations must be in 1..=10_000, found {0}")]
    InvalidMaxIterations(u32),
    /// The configured confidence threshold is not finite or outside zero through one.
    #[error("community minimum_confidence must be finite and in 0..=1, found {0}")]
    InvalidMinimumConfidence(f32),
    /// An input edge has a non-finite or out-of-range confidence.
    #[error("edge `{edge_id}` confidence must be finite and in 0..=1, found {confidence}")]
    InvalidEdgeConfidence {
        /// Stable identity of the invalid edge.
        edge_id: String,
        /// Invalid confidence value.
        confidence: f32,
    },
    /// A configured relationship weight is negative or non-finite.
    #[error("community edge weight for `{kind:?}` must be finite and non-negative, found {weight}")]
    InvalidEdgeWeight {
        /// Relationship kind carrying the invalid weight.
        kind: EdgeKind,
        /// Invalid relationship weight.
        weight: f64,
    },
    /// More than one explicit weight was supplied for the same relationship kind.
    #[error("community edge weight for `{0:?}` is configured more than once")]
    DuplicateEdgeWeight(EdgeKind),
    /// More than one node uses the same stable identifier.
    #[error("node identifier `{0}` occurs more than once")]
    DuplicateNodeId(String),
    /// Service scope did not resolve to an exact service stable key.
    #[error("service scope stable key `{0}` does not identify a service node")]
    UnknownService(String),
}

#[derive(Debug, Clone)]
struct AcceptedEdge {
    source: usize,
    target: usize,
    weight: f64,
}

#[derive(Debug)]
struct WeightedGraph {
    nodes: Vec<Node>,
    adjacency: Vec<BTreeMap<usize, f64>>,
    undirected_edges: Vec<(usize, usize, f64)>,
    accepted_edges: Vec<AcceptedEdge>,
    weighted_degree: Vec<f64>,
    neighbor_count: Vec<usize>,
    total_undirected_weight: f64,
}

/// Detects deterministic communities in one immutable graph snapshot.
///
/// Only confirmed edges meeting `minimum_confidence` participate. Relationship
/// weights are multiplied by edge confidence, parallel directions are summed,
/// and the resulting weighted graph is symmetrized before clustering. Central
/// nodes combine weighted `PageRank` (65%) and normalized weighted degree (35%),
/// with detected high-degree god nodes disclosed and ranked first.
///
/// # Errors
///
/// Returns [`CommunityError`] when configuration values, edge confidences, node
/// identities, or an exact service scope are invalid.
pub fn analyze_communities(
    snapshot_id: impl Into<String>,
    nodes: &[Node],
    edges: &[Edge],
    config: CommunityConfig,
) -> Result<CommunitySnapshot, CommunityError> {
    let weights = validate_inputs(nodes, edges, &config)?;
    let selected = scoped_node_ids(nodes, edges, &config, &weights)?;
    let graph = build_graph(nodes, edges, &config, &weights, &selected);
    let assignments = match config.algorithm {
        CommunityAlgorithm::ConnectedComponents => connected_components(&graph),
        CommunityAlgorithm::WeightedClustering => {
            weighted_label_propagation(&graph, config.seed, config.max_iterations)
        }
        CommunityAlgorithm::Louvain => louvain(
            &graph,
            config.seed,
            config.resolution,
            config.max_iterations,
        ),
    };
    let communities = describe_communities(&graph, &assignments);

    Ok(CommunitySnapshot {
        snapshot_id: snapshot_id.into(),
        engine_version: ENGINE_VERSION.to_owned(),
        config,
        communities,
    })
}

/// Compares two community snapshots using deterministic member-set overlap.
///
/// Jaccard overlap of at least 0.25 establishes a material relationship. One
/// old community related to multiple new communities is a split, while the
/// inverse is a merge. Remaining one-to-one relationships below 0.80 are
/// materially changed; unrelated communities are created or removed.
#[must_use]
pub fn compare_community_snapshots(
    before: &CommunitySnapshot,
    after: &CommunitySnapshot,
) -> CommunityDelta {
    let overlaps = overlap_matrix(&before.communities, &after.communities);
    let before_related =
        related_after(&overlaps, before.communities.len(), after.communities.len());
    let after_related =
        related_before(&overlaps, before.communities.len(), after.communities.len());
    let (mut changes, mut matched_before, mut matched_after) =
        classify_structural_changes(before, after, &overlaps, &before_related, &after_related);
    classify_pairwise_changes(
        before,
        after,
        &overlaps,
        &mut matched_before,
        &mut matched_after,
        &mut changes,
    );
    classify_unmatched(before, after, &matched_before, &matched_after, &mut changes);
    changes.sort_by(compare_changes);
    CommunityDelta {
        before_snapshot_id: before.snapshot_id.clone(),
        after_snapshot_id: after.snapshot_id.clone(),
        changes,
    }
}

fn classify_structural_changes(
    before: &CommunitySnapshot,
    after: &CommunitySnapshot,
    overlaps: &BTreeMap<(usize, usize), f64>,
    before_related: &[Vec<usize>],
    after_related: &[Vec<usize>],
) -> (Vec<CommunityChange>, BTreeSet<usize>, BTreeSet<usize>) {
    let mut changes = Vec::new();
    let mut matched_before = BTreeSet::new();
    let mut matched_after = BTreeSet::new();
    for (before_index, related) in before_related.iter().enumerate() {
        if related.len() >= 2 {
            matched_before.insert(before_index);
            matched_after.extend(related.iter().copied());
            changes.push(CommunityChange {
                kind: CommunityChangeKind::Split,
                before: vec![before.communities[before_index].id.clone()],
                after: related
                    .iter()
                    .map(|index| after.communities[*index].id.clone())
                    .collect(),
                overlap: maximum_overlap(overlaps, related, |index| (before_index, index)),
                explanation: format!(
                    "One prior community overlaps {} successor communities at or above {:.2}.",
                    related.len(),
                    RELATED_OVERLAP
                ),
            });
        }
    }
    for (after_index, related) in after_related.iter().enumerate() {
        if related.len() >= 2 {
            matched_after.insert(after_index);
            matched_before.extend(related.iter().copied());
            changes.push(CommunityChange {
                kind: CommunityChangeKind::Merged,
                before: related
                    .iter()
                    .map(|index| before.communities[*index].id.clone())
                    .collect(),
                after: vec![after.communities[after_index].id.clone()],
                overlap: maximum_overlap(overlaps, related, |index| (index, after_index)),
                explanation: format!(
                    "{} prior communities overlap one successor at or above {:.2}.",
                    related.len(),
                    RELATED_OVERLAP
                ),
            });
        }
    }
    (changes, matched_before, matched_after)
}

fn maximum_overlap(
    overlaps: &BTreeMap<(usize, usize), f64>,
    related: &[usize],
    key: impl Fn(usize) -> (usize, usize),
) -> f64 {
    related
        .iter()
        .filter_map(|&index| overlaps.get(&key(index)).copied())
        .fold(0.0, f64::max)
}

fn classify_pairwise_changes(
    before: &CommunitySnapshot,
    after: &CommunitySnapshot,
    overlaps: &BTreeMap<(usize, usize), f64>,
    matched_before: &mut BTreeSet<usize>,
    matched_after: &mut BTreeSet<usize>,
    changes: &mut Vec<CommunityChange>,
) {
    let mut candidates: Vec<(usize, usize, f64)> = overlaps
        .iter()
        .filter(|(_, overlap)| **overlap >= RELATED_OVERLAP)
        .map(|(&(left, right), &overlap)| (left, right, overlap))
        .collect();
    candidates.sort_by(|left, right| {
        right
            .2
            .total_cmp(&left.2)
            .then_with(|| {
                before.communities[left.0]
                    .id
                    .cmp(&before.communities[right.0].id)
            })
            .then_with(|| {
                after.communities[left.1]
                    .id
                    .cmp(&after.communities[right.1].id)
            })
    });
    for (before_index, after_index, overlap) in candidates {
        if matched_before.contains(&before_index) || matched_after.contains(&after_index) {
            continue;
        }
        matched_before.insert(before_index);
        matched_after.insert(after_index);
        if overlap + EPSILON < STABLE_OVERLAP {
            changes.push(CommunityChange {
                kind: CommunityChangeKind::MateriallyChanged,
                before: vec![before.communities[before_index].id.clone()],
                after: vec![after.communities[after_index].id.clone()],
                overlap,
                explanation: format!("Best one-to-one member overlap is {overlap:.3}, below the stable threshold {STABLE_OVERLAP:.2}."),
            });
        }
    }
}

fn classify_unmatched(
    before: &CommunitySnapshot,
    after: &CommunitySnapshot,
    matched_before: &BTreeSet<usize>,
    matched_after: &BTreeSet<usize>,
    changes: &mut Vec<CommunityChange>,
) {
    for (index, community) in before.communities.iter().enumerate() {
        if !matched_before.contains(&index) {
            changes.push(CommunityChange {
                kind: CommunityChangeKind::Removed,
                before: vec![community.id.clone()],
                after: Vec::new(),
                overlap: 0.0,
                explanation: format!("No successor community reaches the related-overlap threshold {RELATED_OVERLAP:.2}."),
            });
        }
    }
    for (index, community) in after.communities.iter().enumerate() {
        if !matched_after.contains(&index) {
            changes.push(CommunityChange {
                kind: CommunityChangeKind::Created,
                before: Vec::new(),
                after: vec![community.id.clone()],
                overlap: 0.0,
                explanation: format!(
                    "No prior community reaches the related-overlap threshold {RELATED_OVERLAP:.2}."
                ),
            });
        }
    }
}

fn validate_inputs(
    nodes: &[Node],
    edges: &[Edge],
    config: &CommunityConfig,
) -> Result<[f64; EDGE_KIND_COUNT], CommunityError> {
    if !config.resolution.is_finite() || config.resolution <= 0.0 {
        return Err(CommunityError::InvalidResolution(config.resolution));
    }
    if !(1..=10_000).contains(&config.max_iterations) {
        return Err(CommunityError::InvalidMaxIterations(config.max_iterations));
    }
    if !config.minimum_confidence.is_finite() || !(0.0..=1.0).contains(&config.minimum_confidence) {
        return Err(CommunityError::InvalidMinimumConfidence(
            config.minimum_confidence,
        ));
    }
    let mut node_ids = BTreeSet::new();
    for node in nodes {
        if !node_ids.insert(node.id.as_str()) {
            return Err(CommunityError::DuplicateNodeId(node.id.as_str().to_owned()));
        }
    }
    for edge in edges {
        if !edge.confidence.is_finite() || !(0.0..=1.0).contains(&edge.confidence) {
            return Err(CommunityError::InvalidEdgeConfidence {
                edge_id: edge.id.as_str().to_owned(),
                confidence: edge.confidence,
            });
        }
    }

    let mut weights = [1.0; EDGE_KIND_COUNT];
    let mut configured = [false; EDGE_KIND_COUNT];
    for CommunityEdgeWeight { kind, weight } in &config.edge_weights {
        if !weight.is_finite() || *weight < 0.0 {
            return Err(CommunityError::InvalidEdgeWeight {
                kind: *kind,
                weight: *weight,
            });
        }
        let index = edge_kind_index(*kind);
        if configured[index] {
            return Err(CommunityError::DuplicateEdgeWeight(*kind));
        }
        configured[index] = true;
        weights[index] = *weight;
    }
    Ok(weights)
}

fn scoped_node_ids(
    nodes: &[Node],
    edges: &[Edge],
    config: &CommunityConfig,
    weights: &[f64; EDGE_KIND_COUNT],
) -> Result<BTreeSet<NodeId>, CommunityError> {
    match &config.scope {
        CommunityScope::Federated | CommunityScope::Workspace => {
            Ok(nodes.iter().map(|node| node.id.clone()).collect())
        }
        CommunityScope::Repository(repo_id) => Ok(nodes
            .iter()
            .filter(|node| node.repo_id.as_ref() == Some(repo_id))
            .map(|node| node.id.clone())
            .collect()),
        CommunityScope::Service(stable_key) => {
            let services: BTreeSet<NodeId> = nodes
                .iter()
                .filter(|node| {
                    node.kind == NodeKind::Service && node.stable_key.as_str() == stable_key
                })
                .map(|node| node.id.clone())
                .collect();
            if services.is_empty() {
                return Err(CommunityError::UnknownService(stable_key.clone()));
            }
            let known: BTreeSet<&str> = nodes.iter().map(|node| node.id.as_str()).collect();
            let mut selected = services.clone();
            for edge in edges {
                if edge.status != EpistemicStatus::Confirmed
                    || edge.confidence < config.minimum_confidence
                    || weights[edge_kind_index(edge.kind)] <= 0.0
                    || !known.contains(edge.source.as_str())
                    || !known.contains(edge.target.as_str())
                {
                    continue;
                }
                if services.contains(&edge.source) {
                    selected.insert(edge.target.clone());
                }
                if services.contains(&edge.target) {
                    selected.insert(edge.source.clone());
                }
            }
            Ok(selected)
        }
    }
}

fn build_graph(
    nodes: &[Node],
    edges: &[Edge],
    config: &CommunityConfig,
    weights: &[f64; EDGE_KIND_COUNT],
    selected: &BTreeSet<NodeId>,
) -> WeightedGraph {
    let mut graph_nodes: Vec<Node> = nodes
        .iter()
        .filter(|node| selected.contains(&node.id))
        .cloned()
        .collect();
    graph_nodes.sort_by(|left, right| left.id.cmp(&right.id));
    let index: BTreeMap<&str, usize> = graph_nodes
        .iter()
        .enumerate()
        .map(|(position, node)| (node.id.as_str(), position))
        .collect();
    let mut ordered_edges: Vec<&Edge> = edges.iter().collect();
    ordered_edges.sort_by(|left, right| {
        left.source
            .cmp(&right.source)
            .then_with(|| left.target.cmp(&right.target))
            .then_with(|| edge_kind_index(left.kind).cmp(&edge_kind_index(right.kind)))
            .then_with(|| left.id.cmp(&right.id))
            .then_with(|| left.confidence.total_cmp(&right.confidence))
    });

    let mut accepted_edges = Vec::new();
    let mut undirected = BTreeMap::<(usize, usize), f64>::new();
    for edge in ordered_edges {
        if edge.status != EpistemicStatus::Confirmed || edge.confidence < config.minimum_confidence
        {
            continue;
        }
        let Some(&source) = index.get(edge.source.as_str()) else {
            continue;
        };
        let Some(&target) = index.get(edge.target.as_str()) else {
            continue;
        };
        let weight = weights[edge_kind_index(edge.kind)] * f64::from(edge.confidence);
        if weight <= 0.0 {
            continue;
        }
        accepted_edges.push(AcceptedEdge {
            source,
            target,
            weight,
        });
        let pair = if source <= target {
            (source, target)
        } else {
            (target, source)
        };
        *undirected.entry(pair).or_default() += weight;
    }

    let undirected_edges: Vec<(usize, usize, f64)> = undirected
        .into_iter()
        .map(|((source, target), weight)| (source, target, weight))
        .collect();
    let mut adjacency = vec![BTreeMap::new(); graph_nodes.len()];
    let mut weighted_degree = vec![0.0; graph_nodes.len()];
    let mut neighbors = vec![BTreeSet::new(); graph_nodes.len()];
    let mut total_undirected_weight = 0.0;
    for &(source, target, weight) in &undirected_edges {
        total_undirected_weight += weight;
        if source == target {
            adjacency[source].insert(target, weight);
            weighted_degree[source] += 2.0 * weight;
        } else {
            adjacency[source].insert(target, weight);
            adjacency[target].insert(source, weight);
            weighted_degree[source] += weight;
            weighted_degree[target] += weight;
            neighbors[source].insert(target);
            neighbors[target].insert(source);
        }
    }
    let neighbor_count = neighbors.into_iter().map(|items| items.len()).collect();
    WeightedGraph {
        nodes: graph_nodes,
        adjacency,
        undirected_edges,
        accepted_edges,
        weighted_degree,
        neighbor_count,
        total_undirected_weight,
    }
}

fn connected_components(graph: &WeightedGraph) -> Vec<usize> {
    let mut assignments = vec![usize::MAX; graph.nodes.len()];
    let mut component = 0;
    for start in 0..graph.nodes.len() {
        if assignments[start] != usize::MAX {
            continue;
        }
        assignments[start] = component;
        let mut pending = vec![start];
        while let Some(node) = pending.pop() {
            for (&neighbor, &weight) in &graph.adjacency[node] {
                if weight > 0.0 && assignments[neighbor] == usize::MAX {
                    assignments[neighbor] = component;
                    pending.push(neighbor);
                }
            }
        }
        component += 1;
    }
    assignments
}

fn weighted_label_propagation(graph: &WeightedGraph, seed: u64, max_iterations: u32) -> Vec<usize> {
    let mut assignments: Vec<usize> = (0..graph.nodes.len()).collect();
    let order = seeded_node_order(graph, seed);
    for _ in 0..max_iterations {
        let mut changed = false;
        for &node in &order {
            let mut scores = BTreeMap::<usize, f64>::new();
            for (&neighbor, &weight) in &graph.adjacency[node] {
                if neighbor != node {
                    *scores.entry(assignments[neighbor]).or_default() += weight;
                }
            }
            let current = assignments[node];
            let current_score = scores.get(&current).copied().unwrap_or_default();
            let mut best = current;
            let mut best_score = current_score;
            for (candidate, score) in scores {
                if score > best_score + EPSILON
                    || ((score - best_score).abs() <= EPSILON
                        && score > current_score + EPSILON
                        && seeded_label_key(graph, seed, candidate)
                            < seeded_label_key(graph, seed, best))
                {
                    best = candidate;
                    best_score = score;
                }
            }
            if best != current {
                assignments[node] = best;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    assignments
}

fn louvain(graph: &WeightedGraph, seed: u64, resolution: f64, max_iterations: u32) -> Vec<usize> {
    let mut assignments: Vec<usize> = (0..graph.nodes.len()).collect();
    if graph.total_undirected_weight <= EPSILON {
        return assignments;
    }
    let order = seeded_node_order(graph, seed);
    for _ in 0..max_iterations {
        let mut changed = false;
        for &node in &order {
            let current = assignments[node];
            let baseline = modularity(graph, &assignments, resolution);
            let mut candidates = BTreeSet::from([current]);
            for &neighbor in graph.adjacency[node].keys() {
                candidates.insert(assignments[neighbor]);
            }
            if let Some(empty) = first_empty_label(&assignments) {
                candidates.insert(empty);
            }
            let mut best = current;
            let mut best_modularity = baseline;
            for candidate in candidates {
                if candidate == current {
                    continue;
                }
                assignments[node] = candidate;
                let candidate_modularity = modularity(graph, &assignments, resolution);
                assignments[node] = current;
                if candidate_modularity > best_modularity + EPSILON
                    || ((candidate_modularity - best_modularity).abs() <= EPSILON
                        && candidate_modularity > baseline + EPSILON
                        && seeded_label_key(graph, seed, candidate)
                            < seeded_label_key(graph, seed, best))
                {
                    best = candidate;
                    best_modularity = candidate_modularity;
                }
            }
            if best != current && best_modularity > baseline + EPSILON {
                assignments[node] = best;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    assignments
}

/// Computes generalized undirected modularity:
/// `sum_c(internal_weight_c / m - resolution * (degree_c / (2m))^2)`.
fn modularity(graph: &WeightedGraph, assignments: &[usize], resolution: f64) -> f64 {
    let total = graph.total_undirected_weight;
    if total <= EPSILON {
        return 0.0;
    }
    let mut internal = BTreeMap::<usize, f64>::new();
    let mut degree = BTreeMap::<usize, f64>::new();
    for (node, &community) in assignments.iter().enumerate() {
        *degree.entry(community).or_default() += graph.weighted_degree[node];
    }
    for &(source, target, weight) in &graph.undirected_edges {
        if assignments[source] == assignments[target] {
            *internal.entry(assignments[source]).or_default() += weight;
        }
    }
    degree
        .into_iter()
        .map(|(community, community_degree)| {
            let inside = internal.get(&community).copied().unwrap_or_default();
            inside / total - resolution * (community_degree / (2.0 * total)).powi(2)
        })
        .sum()
}

fn first_empty_label(assignments: &[usize]) -> Option<usize> {
    let mut used = vec![false; assignments.len()];
    for &assignment in assignments {
        if let Some(slot) = used.get_mut(assignment) {
            *slot = true;
        }
    }
    used.iter().position(|value| !value)
}

fn seeded_node_order(graph: &WeightedGraph, seed: u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..graph.nodes.len()).collect();
    order.sort_by(|&left, &right| {
        seeded_key(seed, graph.nodes[left].id.as_str())
            .cmp(&seeded_key(seed, graph.nodes[right].id.as_str()))
            .then_with(|| graph.nodes[left].id.cmp(&graph.nodes[right].id))
    });
    order
}

fn seeded_label_key(graph: &WeightedGraph, seed: u64, label: usize) -> String {
    graph.nodes.get(label).map_or_else(
        || stable_id("community-order", &format!("{seed}:empty:{label}")),
        |node| seeded_key(seed, node.id.as_str()),
    )
}

fn seeded_key(seed: u64, value: &str) -> String {
    stable_id("community-order", &format!("{seed}:{value}"))
}

fn describe_communities(graph: &WeightedGraph, assignments: &[usize]) -> Vec<Community> {
    let mut grouped = BTreeMap::<usize, Vec<usize>>::new();
    for (node, &community) in assignments.iter().enumerate() {
        grouped.entry(community).or_default().push(node);
    }
    let mut groups: Vec<Vec<usize>> = grouped.into_values().collect();
    for members in &mut groups {
        members.sort_by(|&left, &right| graph.nodes[left].id.cmp(&graph.nodes[right].id));
    }
    groups.sort_by(|left, right| compare_member_groups(graph, left, right));
    let page_rank = weighted_page_rank(graph);
    let god_nodes = detect_god_nodes(graph);

    groups
        .iter()
        .map(|members| describe_community(graph, members, &page_rank, &god_nodes))
        .collect()
}

fn describe_community(
    graph: &WeightedGraph,
    members: &[usize],
    page_rank: &[f64],
    god_nodes: &BTreeSet<usize>,
) -> Community {
    let member_set: BTreeSet<usize> = members.iter().copied().collect();
    let central = central_nodes(graph, members, page_rank, god_nodes);
    let (label, label_evidence) = structural_label(graph, members, &central);
    let mut repositories = BTreeSet::<RepoId>::new();
    let mut services = BTreeSet::<NodeId>::new();
    for &member in members {
        if let Some(repo_id) = &graph.nodes[member].repo_id {
            repositories.insert(repo_id.clone());
        }
        if graph.nodes[member].kind == NodeKind::Service {
            services.insert(graph.nodes[member].id.clone());
        }
    }
    let mut inbound_contracts = BTreeSet::new();
    let mut outbound_contracts = BTreeSet::new();
    let mut cohesion = 0.0;
    let mut coupling = 0.0;
    let mut cross_community_edges = 0;
    let mut internal_pairs = BTreeSet::new();
    for edge in &graph.accepted_edges {
        let source_inside = member_set.contains(&edge.source);
        let target_inside = member_set.contains(&edge.target);
        if source_inside && target_inside {
            cohesion += edge.weight;
            if edge.source != edge.target {
                internal_pairs.insert((edge.source, edge.target));
            }
        } else if source_inside || target_inside {
            coupling += edge.weight;
            cross_community_edges += 1;
            if target_inside && is_contract_kind(graph.nodes[edge.target].kind) {
                inbound_contracts.insert(graph.nodes[edge.target].id.clone());
            }
            if source_inside && is_contract_kind(graph.nodes[edge.source].kind) {
                outbound_contracts.insert(graph.nodes[edge.source].id.clone());
            }
        }
    }
    let possible = members
        .len()
        .saturating_mul(members.len().saturating_sub(1));
    let density = if possible == 0 {
        0.0
    } else {
        usize_to_f64(internal_pairs.len()) / usize_to_f64(possible)
    };
    let mut limitations = Vec::new();
    if members.len() == 1 {
        limitations
            .push("Community contains a single node; structural metrics are limited.".to_owned());
    }
    for &node in members {
        if god_nodes.contains(&node) {
            limitations.push(format!(
                "High-degree god node `{}` may dominate community structure.",
                graph.nodes[node].id.as_str()
            ));
        }
    }
    if cohesion <= EPSILON {
        limitations.push("Community has no positive internal accepted edge weight.".to_owned());
    }
    limitations.sort();
    let member_ids: Vec<NodeId> = members
        .iter()
        .map(|&member| graph.nodes[member].id.clone())
        .collect();
    let canonical_members = member_ids
        .iter()
        .map(|id| format!("{}:{}", id.as_str().len(), id.as_str()))
        .collect::<Vec<_>>()
        .join("|");

    Community {
        id: CommunityId::new(stable_id("community", &canonical_members)),
        label,
        members: member_ids,
        central_nodes: central
            .into_iter()
            .map(|node| graph.nodes[node].id.clone())
            .collect(),
        repositories: repositories.into_iter().collect(),
        services: services.into_iter().collect(),
        inbound_contracts: inbound_contracts.into_iter().collect(),
        outbound_contracts: outbound_contracts.into_iter().collect(),
        metrics: CommunityMetrics {
            size: members.len(),
            density,
            cohesion,
            coupling,
            cross_community_edges,
        },
        label_evidence,
        limitations,
    }
}

fn weighted_page_rank(graph: &WeightedGraph) -> Vec<f64> {
    let count = graph.nodes.len();
    if count == 0 {
        return Vec::new();
    }
    let count_f64 = usize_to_f64(count);
    let mut ranks = vec![1.0 / count_f64; count];
    let mut outgoing = vec![0.0; count];
    for edge in &graph.accepted_edges {
        outgoing[edge.source] += edge.weight;
    }
    for _ in 0..PAGE_RANK_ITERATIONS {
        let dangling: f64 = ranks
            .iter()
            .enumerate()
            .filter(|(node, _)| outgoing[*node] <= EPSILON)
            .map(|(_, rank)| *rank)
            .sum();
        let base = (1.0 - PAGE_RANK_DAMPING) / count_f64 + PAGE_RANK_DAMPING * dangling / count_f64;
        let mut next = vec![base; count];
        for edge in &graph.accepted_edges {
            if outgoing[edge.source] > EPSILON {
                next[edge.target] +=
                    PAGE_RANK_DAMPING * ranks[edge.source] * edge.weight / outgoing[edge.source];
            }
        }
        ranks = next;
    }
    ranks
}

fn detect_god_nodes(graph: &WeightedGraph) -> BTreeSet<usize> {
    if graph.nodes.len() < 4 {
        return BTreeSet::new();
    }
    let mut degrees = graph.neighbor_count.clone();
    degrees.sort_unstable();
    let median = degrees[degrees.len() / 2];
    let majority = graph.nodes.len().div_ceil(2);
    graph
        .neighbor_count
        .iter()
        .enumerate()
        .filter(|(_, degree)| **degree >= majority && **degree >= median.saturating_mul(2).max(3))
        .map(|(node, _)| node)
        .collect()
}

fn central_nodes(
    graph: &WeightedGraph,
    members: &[usize],
    page_rank: &[f64],
    god_nodes: &BTreeSet<usize>,
) -> Vec<usize> {
    let maximum_degree = members
        .iter()
        .map(|&node| graph.weighted_degree[node])
        .fold(0.0, f64::max);
    let mut central = members.to_vec();
    central.sort_by(|&left, &right| {
        god_nodes
            .contains(&right)
            .cmp(&god_nodes.contains(&left))
            .then_with(|| {
                let left_score =
                    centrality_score(page_rank[left], graph.weighted_degree[left], maximum_degree);
                let right_score = centrality_score(
                    page_rank[right],
                    graph.weighted_degree[right],
                    maximum_degree,
                );
                right_score.total_cmp(&left_score)
            })
            .then_with(|| graph.nodes[left].id.cmp(&graph.nodes[right].id))
    });
    central.truncate(3);
    central
}

fn centrality_score(page_rank: f64, weighted_degree: f64, maximum_degree: f64) -> f64 {
    let normalized_degree = if maximum_degree <= EPSILON {
        0.0
    } else {
        weighted_degree / maximum_degree
    };
    0.65 * page_rank + 0.35 * normalized_degree
}

fn structural_label(
    graph: &WeightedGraph,
    members: &[usize],
    central: &[usize],
) -> (String, Vec<CommunityLabelEvidence>) {
    let central_set: BTreeSet<usize> = central.iter().copied().collect();
    let mut terms = BTreeMap::<String, (u32, NodeId, u32)>::new();
    for &member in members {
        let node = &graph.nodes[member];
        let category_score = match node.kind {
            NodeKind::Service => 400,
            NodeKind::Repository => 300,
            kind if is_contract_kind(kind) => 200,
            _ => 0,
        };
        let central_score = if central_set.contains(&member) {
            100
        } else {
            0
        };
        let score = category_score + central_score;
        if score == 0 {
            continue;
        }
        let mut node_terms = normalized_terms(&node.label);
        node_terms.extend(normalized_terms(&node.stable_key));
        for term in node_terms {
            let entry = terms.entry(term).or_insert_with(|| (0, node.id.clone(), 0));
            entry.0 = entry.0.saturating_add(score);
            if score > entry.2 || (score == entry.2 && node.id < entry.1) {
                entry.1 = node.id.clone();
                entry.2 = score;
            }
        }
    }
    let mut ranked: Vec<(String, u32, NodeId)> = terms
        .into_iter()
        .map(|(term, (score, node_id, _))| (term, score, node_id))
        .collect();
    ranked.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
            .then_with(|| left.2.cmp(&right.2))
    });
    ranked.truncate(3);
    if ranked.is_empty() {
        let fallback = members
            .first()
            .map_or("community", |&member| graph.nodes[member].label.as_str());
        return (format!("Community: {fallback}"), Vec::new());
    }
    let label = ranked
        .iter()
        .map(|(term, _, _)| title_case(term))
        .collect::<Vec<_>>()
        .join(" / ");
    let evidence = ranked
        .into_iter()
        .map(|(term, _, node_id)| CommunityLabelEvidence { node_id, term })
        .collect();
    (label, evidence)
}

fn normalized_terms(value: &str) -> BTreeSet<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|term| {
            (3..=32).contains(&term.len())
                && term.chars().any(char::is_alphabetic)
                && !is_stop_term(term)
        })
        .collect()
}

fn is_stop_term(term: &str) -> bool {
    matches!(
        term,
        "api"
            | "app"
            | "application"
            | "artifact"
            | "community"
            | "default"
            | "http"
            | "https"
            | "operation"
            | "repo"
            | "repository"
            | "rpc"
            | "service"
            | "src"
            | "test"
            | "tests"
            | "version"
    )
}

fn title_case(term: &str) -> String {
    let mut characters = term.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + characters.as_str()
    })
}

fn is_contract_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::HttpOperation
            | NodeKind::GraphqlOperation
            | NodeKind::RpcMethod
            | NodeKind::EventChannel
            | NodeKind::EventSchema
            | NodeKind::Database
            | NodeKind::DatabaseTable
    )
}

fn compare_member_groups(graph: &WeightedGraph, left: &[usize], right: &[usize]) -> Ordering {
    left.iter()
        .map(|&node| graph.nodes[node].id.as_str())
        .cmp(right.iter().map(|&node| graph.nodes[node].id.as_str()))
}

fn overlap_matrix(before: &[Community], after: &[Community]) -> BTreeMap<(usize, usize), f64> {
    let mut overlaps = BTreeMap::new();
    for (before_index, old) in before.iter().enumerate() {
        let old_members: BTreeSet<&str> = old.members.iter().map(NodeId::as_str).collect();
        for (after_index, new) in after.iter().enumerate() {
            let new_members: BTreeSet<&str> = new.members.iter().map(NodeId::as_str).collect();
            let intersection = old_members.intersection(&new_members).count();
            let union = old_members.union(&new_members).count();
            let overlap = if union == 0 {
                1.0
            } else {
                usize_to_f64(intersection) / usize_to_f64(union)
            };
            overlaps.insert((before_index, after_index), overlap);
        }
    }
    overlaps
}

fn related_after(
    overlaps: &BTreeMap<(usize, usize), f64>,
    before_count: usize,
    after_count: usize,
) -> Vec<Vec<usize>> {
    (0..before_count)
        .map(|before| {
            (0..after_count)
                .filter(|after| {
                    overlaps
                        .get(&(before, *after))
                        .is_some_and(|overlap| *overlap >= RELATED_OVERLAP)
                })
                .collect()
        })
        .collect()
}

fn related_before(
    overlaps: &BTreeMap<(usize, usize), f64>,
    before_count: usize,
    after_count: usize,
) -> Vec<Vec<usize>> {
    (0..after_count)
        .map(|after| {
            (0..before_count)
                .filter(|before| {
                    overlaps
                        .get(&(*before, after))
                        .is_some_and(|overlap| *overlap >= RELATED_OVERLAP)
                })
                .collect()
        })
        .collect()
}

fn compare_changes(left: &CommunityChange, right: &CommunityChange) -> Ordering {
    change_kind_index(left.kind)
        .cmp(&change_kind_index(right.kind))
        .then_with(|| left.before.cmp(&right.before))
        .then_with(|| left.after.cmp(&right.after))
        .then_with(|| left.overlap.total_cmp(&right.overlap))
        .then_with(|| left.explanation.cmp(&right.explanation))
}

const fn change_kind_index(kind: CommunityChangeKind) -> usize {
    match kind {
        CommunityChangeKind::Created => 0,
        CommunityChangeKind::Removed => 1,
        CommunityChangeKind::Split => 2,
        CommunityChangeKind::Merged => 3,
        CommunityChangeKind::MateriallyChanged => 4,
    }
}

const fn edge_kind_index(kind: EdgeKind) -> usize {
    match kind {
        EdgeKind::Contains => 0,
        EdgeKind::Provides => 1,
        EdgeKind::Consumes => 2,
        EdgeKind::CallsRemote => 3,
        EdgeKind::Publishes => 4,
        EdgeKind::Subscribes => 5,
        EdgeKind::DeliversTo => 6,
        EdgeKind::DependsOnPackage => 7,
        EdgeKind::DependsOnRepository => 8,
        EdgeKind::ReadsTable => 9,
        EdgeKind::WritesTable => 10,
        EdgeKind::Deploys => 11,
        EdgeKind::Configures => 12,
        EdgeKind::Documents => 13,
        EdgeKind::OwnedBy => 14,
        EdgeKind::ImplementedBy => 15,
        EdgeKind::Validates => 16,
        EdgeKind::ChangedIn => 17,
        EdgeKind::Affects => 18,
        EdgeKind::Precedes => 19,
        EdgeKind::Reverts => 20,
        EdgeKind::CompatibleWith => 21,
        EdgeKind::IncompatibleWith => 22,
        EdgeKind::MemberOf => 23,
        EdgeKind::ManualLink => 24,
    }
}

#[expect(
    clippy::cast_precision_loss,
    reason = "graph cardinalities are bounded by addressable memory and only form normalized metrics"
)]
fn usize_to_f64(value: usize) -> f64 {
    value as f64
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{EdgeId, NodeId};

    use super::*;

    fn node(id: &str, kind: NodeKind, repo: &str, label: &str) -> Node {
        Node {
            id: NodeId::new(id),
            kind,
            repo_id: Some(RepoId::new(repo)),
            stable_key: format!("stable:{label}"),
            label: label.to_owned(),
        }
    }

    fn edge(id: &str, source: &str, target: &str, kind: EdgeKind, confidence: f32) -> Edge {
        Edge {
            id: EdgeId::new(id),
            source: NodeId::new(source),
            target: NodeId::new(target),
            kind,
            confidence,
            status: EpistemicStatus::Confirmed,
            evidence: Vec::new(),
        }
    }

    fn config(algorithm: CommunityAlgorithm) -> CommunityConfig {
        CommunityConfig {
            algorithm,
            scope: CommunityScope::Federated,
            seed: 17,
            resolution: 1.0,
            minimum_confidence: 0.0,
            edge_weights: Vec::new(),
            max_iterations: 100,
        }
    }

    fn two_cluster_graph() -> (Vec<Node>, Vec<Edge>) {
        let nodes = ["a", "b", "c", "d", "e", "f"]
            .into_iter()
            .map(|id| node(id, NodeKind::Service, "repo:one", id))
            .collect();
        let mut edges = Vec::new();
        let mut number = 0;
        for cluster in [["a", "b", "c"], ["d", "e", "f"]] {
            for source in cluster {
                for target in cluster {
                    if source != target {
                        edges.push(edge(
                            &format!("e{number}"),
                            source,
                            target,
                            EdgeKind::CallsRemote,
                            1.0,
                        ));
                        number += 1;
                    }
                }
            }
        }
        edges.push(edge("bridge", "c", "d", EdgeKind::ManualLink, 1.0));
        (nodes, edges)
    }

    #[test]
    fn connected_components_should_return_maximal_regions() {
        let nodes = vec![
            node("a", NodeKind::Service, "repo:one", "alpha"),
            node("b", NodeKind::HttpOperation, "repo:one", "alpha endpoint"),
            node("c", NodeKind::Service, "repo:two", "beta"),
        ];
        let edges = vec![edge("ab", "a", "b", EdgeKind::Provides, 1.0)];

        let result = analyze_communities(
            "snapshot",
            &nodes,
            &edges,
            config(CommunityAlgorithm::ConnectedComponents),
        )
        .expect("analysis should succeed");

        assert_eq!(
            result
                .communities
                .iter()
                .map(|community| community.metrics.size)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
    }

    #[test]
    fn weighted_clustering_should_respect_relationship_weights() {
        let (nodes, edges) = two_cluster_graph();
        let mut analysis_config = config(CommunityAlgorithm::WeightedClustering);
        analysis_config.edge_weights.push(CommunityEdgeWeight {
            kind: EdgeKind::ManualLink,
            weight: 0.01,
        });

        let result = analyze_communities("snapshot", &nodes, &edges, analysis_config)
            .expect("analysis should succeed");

        assert_eq!(result.communities.len(), 2);
    }

    #[test]
    fn louvain_should_optimize_modularity_into_weighted_clusters() {
        let (nodes, edges) = two_cluster_graph();
        let mut analysis_config = config(CommunityAlgorithm::Louvain);
        analysis_config.edge_weights.push(CommunityEdgeWeight {
            kind: EdgeKind::ManualLink,
            weight: 0.05,
        });

        let result = analyze_communities("snapshot", &nodes, &edges, analysis_config)
            .expect("analysis should succeed");

        assert_eq!(result.communities.len(), 2);
    }

    #[test]
    fn seeded_analysis_should_be_byte_reproducible() {
        let (mut nodes, mut edges) = two_cluster_graph();
        let analysis_config = config(CommunityAlgorithm::Louvain);
        let first = analyze_communities("snapshot", &nodes, &edges, analysis_config.clone())
            .expect("first analysis should succeed");
        nodes.reverse();
        edges.reverse();
        let second = analyze_communities("snapshot", &nodes, &edges, analysis_config)
            .expect("second analysis should succeed");

        assert_eq!(
            serde_json::to_vec(&first).expect("snapshot should serialize"),
            serde_json::to_vec(&second).expect("snapshot should serialize")
        );
    }

    #[test]
    fn repository_scope_should_exclude_other_repositories() {
        let nodes = vec![
            node("a", NodeKind::Service, "repo:one", "alpha"),
            node("b", NodeKind::Artifact, "repo:one", "alpha manifest"),
            node("c", NodeKind::Service, "repo:two", "beta"),
        ];
        let edges = vec![
            edge("ab", "a", "b", EdgeKind::Contains, 1.0),
            edge("ac", "a", "c", EdgeKind::CallsRemote, 1.0),
        ];
        let mut analysis_config = config(CommunityAlgorithm::ConnectedComponents);
        analysis_config.scope = CommunityScope::Repository(RepoId::new("repo:one"));

        let result = analyze_communities("snapshot", &nodes, &edges, analysis_config)
            .expect("analysis should succeed");

        assert_eq!(result.communities[0].members.len(), 2);
    }

    #[test]
    fn service_scope_should_include_only_directly_attached_nodes() {
        let mut service = node("service", NodeKind::Service, "repo:one", "billing");
        service.stable_key = "service:billing".to_owned();
        let nodes = vec![
            service,
            node("contract", NodeKind::HttpOperation, "repo:one", "charge"),
            node("indirect", NodeKind::Artifact, "repo:one", "schema"),
        ];
        let edges = vec![
            edge("first", "service", "contract", EdgeKind::Provides, 1.0),
            edge("second", "contract", "indirect", EdgeKind::Contains, 1.0),
        ];
        let mut analysis_config = config(CommunityAlgorithm::ConnectedComponents);
        analysis_config.scope = CommunityScope::Service("service:billing".to_owned());

        let result = analyze_communities("snapshot", &nodes, &edges, analysis_config)
            .expect("analysis should succeed");

        assert_eq!(
            result.communities[0].members,
            vec![NodeId::new("contract"), NodeId::new("service")]
        );
    }

    #[test]
    fn metrics_should_report_cohesion_density_and_cross_coupling() {
        let (nodes, edges) = two_cluster_graph();
        let mut analysis_config = config(CommunityAlgorithm::Louvain);
        analysis_config.edge_weights.push(CommunityEdgeWeight {
            kind: EdgeKind::ManualLink,
            weight: 0.05,
        });

        let result = analyze_communities("snapshot", &nodes, &edges, analysis_config)
            .expect("analysis should succeed");
        let first = &result.communities[0];

        assert!(
            (first.metrics.density - 1.0).abs() <= EPSILON
                && first.metrics.cohesion > first.metrics.coupling
                && first.metrics.cross_community_edges == 1
        );
    }

    #[test]
    fn labels_should_prefer_service_and_contract_terms_with_evidence() {
        let nodes = vec![
            node("service", NodeKind::Service, "repo:one", "Billing Gateway"),
            node(
                "contract",
                NodeKind::HttpOperation,
                "repo:one",
                "Create Invoice",
            ),
        ];
        let edges = vec![edge(
            "provides",
            "service",
            "contract",
            EdgeKind::Provides,
            1.0,
        )];

        let result = analyze_communities(
            "snapshot",
            &nodes,
            &edges,
            config(CommunityAlgorithm::ConnectedComponents),
        )
        .expect("analysis should succeed");

        assert!(
            result.communities[0].label.contains("Billing")
                && !result.communities[0].label_evidence.is_empty()
        );
    }

    #[test]
    fn god_nodes_should_be_central_and_disclosed() {
        let mut nodes = vec![node("hub", NodeKind::Service, "repo:one", "gateway")];
        let mut edges = Vec::new();
        for index in 0..5 {
            let leaf = format!("leaf-{index}");
            nodes.push(node(&leaf, NodeKind::Artifact, "repo:one", &leaf));
            edges.push(edge(
                &format!("edge-{index}"),
                "hub",
                &leaf,
                EdgeKind::Contains,
                1.0,
            ));
        }

        let result = analyze_communities(
            "snapshot",
            &nodes,
            &edges,
            config(CommunityAlgorithm::ConnectedComponents),
        )
        .expect("analysis should succeed");

        assert!(
            result.communities[0].central_nodes[0] == NodeId::new("hub")
                && result.communities[0]
                    .limitations
                    .iter()
                    .any(|limitation| limitation.contains("god node"))
        );
    }

    fn community(id: &str, members: &[&str]) -> Community {
        Community {
            id: CommunityId::new(id),
            label: id.to_owned(),
            members: members.iter().map(|member| NodeId::new(*member)).collect(),
            central_nodes: Vec::new(),
            repositories: Vec::new(),
            services: Vec::new(),
            inbound_contracts: Vec::new(),
            outbound_contracts: Vec::new(),
            metrics: CommunityMetrics {
                size: members.len(),
                density: 0.0,
                cohesion: 0.0,
                coupling: 0.0,
                cross_community_edges: 0,
            },
            label_evidence: Vec::new(),
            limitations: Vec::new(),
        }
    }

    fn snapshot(id: &str, communities: Vec<Community>) -> CommunitySnapshot {
        CommunitySnapshot {
            snapshot_id: id.to_owned(),
            engine_version: ENGINE_VERSION.to_owned(),
            config: config(CommunityAlgorithm::ConnectedComponents),
            communities,
        }
    }

    #[test]
    fn comparison_should_classify_created_community() {
        let before = snapshot("before", Vec::new());
        let after = snapshot("after", vec![community("new", &["a"])]);

        let delta = compare_community_snapshots(&before, &after);

        assert_eq!(delta.changes[0].kind, CommunityChangeKind::Created);
    }

    #[test]
    fn comparison_should_classify_removed_community() {
        let before = snapshot("before", vec![community("old", &["a"])]);
        let after = snapshot("after", Vec::new());

        let delta = compare_community_snapshots(&before, &after);

        assert_eq!(delta.changes[0].kind, CommunityChangeKind::Removed);
    }

    #[test]
    fn comparison_should_classify_split_community() {
        let before = snapshot("before", vec![community("old", &["a", "b", "c", "d"])]);
        let after = snapshot(
            "after",
            vec![
                community("left", &["a", "b"]),
                community("right", &["c", "d"]),
            ],
        );

        let delta = compare_community_snapshots(&before, &after);

        assert_eq!(delta.changes[0].kind, CommunityChangeKind::Split);
    }

    #[test]
    fn comparison_should_classify_merged_community() {
        let before = snapshot(
            "before",
            vec![
                community("left", &["a", "b"]),
                community("right", &["c", "d"]),
            ],
        );
        let after = snapshot("after", vec![community("new", &["a", "b", "c", "d"])]);

        let delta = compare_community_snapshots(&before, &after);

        assert_eq!(delta.changes[0].kind, CommunityChangeKind::Merged);
    }

    #[test]
    fn comparison_should_classify_materially_changed_community() {
        let before = snapshot("before", vec![community("old", &["a", "b", "c", "d"])]);
        let after = snapshot("after", vec![community("new", &["a", "b", "c", "e"])]);

        let delta = compare_community_snapshots(&before, &after);

        assert_eq!(
            delta.changes[0].kind,
            CommunityChangeKind::MateriallyChanged
        );
    }

    #[test]
    fn validation_should_reject_non_finite_confidence() {
        let nodes = vec![
            node("a", NodeKind::Service, "repo:one", "alpha"),
            node("b", NodeKind::Service, "repo:one", "beta"),
        ];
        let edges = vec![edge("invalid", "a", "b", EdgeKind::CallsRemote, f32::NAN)];

        let error = analyze_communities(
            "snapshot",
            &nodes,
            &edges,
            config(CommunityAlgorithm::ConnectedComponents),
        )
        .expect_err("invalid confidence should fail");

        assert!(matches!(
            error,
            CommunityError::InvalidEdgeConfidence { .. }
        ));
    }
}

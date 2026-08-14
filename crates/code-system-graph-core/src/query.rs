//! Deterministic ranked search and bounded graph traversal.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::time::{Duration, Instant};

use code_system_graph_model::{
    CommunityId, Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, Node, NodeId, NodeKind, RepoFreshnessState, RepoId, TraceSegment
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_SEARCH_RESULTS: usize = 100;
const MAX_SEARCH_OFFSET: usize = 1_000_000;
const MAX_TRAVERSAL_DEPTH: usize = 128;
const MAX_TRAVERSAL_NODES: usize = 100_000;
const MAX_TRAVERSAL_EDGES: usize = 1_000_000;
const MAX_TRAVERSAL_TIMEOUT_MS: u64 = 60_000;
const MAX_K_PATHS: usize = 16;
const MIN_CONFIDENCE_FOR_WEIGHT: f64 = 0.01;

/// Filters applied before search candidates are ranked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchFilters {
    /// Node kinds eligible for the result; an empty list accepts every kind.
    #[serde(default)]
    pub node_kinds: Vec<NodeKind>,
    /// Repositories eligible for the result; an empty list accepts every repository.
    #[serde(default)]
    pub repo_ids: Vec<RepoId>,
    /// Node identifiers in the selected workspace scope; an empty list accepts every node.
    #[serde(default)]
    pub workspace_nodes: Vec<NodeId>,
    /// Service identifiers that must contain an eligible node.
    #[serde(default)]
    pub service_ids: Vec<NodeId>,
    /// Community identifiers that must contain an eligible node.
    #[serde(default)]
    pub community_ids: Vec<CommunityId>,
}

/// Input for one deterministic ranked search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchRequest {
    /// User query matched against stable keys, labels, and node kinds.
    pub query: String,
    /// Scope and type filters.
    #[serde(default)]
    pub filters: SearchFilters,
    /// Normalized full-text scores keyed by node identifier.
    #[serde(default)]
    pub fts_scores: BTreeMap<NodeId, f64>,
    /// Normalized centrality scores keyed by node identifier.
    #[serde(default)]
    pub centrality_scores: BTreeMap<NodeId, f64>,
    /// Optional service memberships keyed by member node identifier.
    #[serde(default)]
    pub service_memberships: BTreeMap<NodeId, Vec<NodeId>>,
    /// Optional community memberships keyed by member node identifier.
    #[serde(default)]
    pub community_memberships: BTreeMap<NodeId, Vec<CommunityId>>,
    /// Evidence used only to derive bounded quality scores, never returned in hits.
    #[serde(default)]
    pub evidence: BTreeMap<NodeId, Vec<Evidence>>,
    /// Repository freshness used to penalize stale or incomplete results.
    #[serde(default)]
    pub freshness: BTreeMap<RepoId, RepoFreshnessState>,
    /// Zero-based result offset.
    pub offset: usize,
    /// Maximum number of results, inclusively bounded by 100.
    pub limit: usize,
}

/// Score components and match facts for one search hit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchExplanation {
    /// Fields that matched the normalized query.
    pub matched_fields: Vec<String>,
    /// Score contributed by exact stable-key or label matching.
    pub exact_score: f64,
    /// Score contributed by normalized prefix matching.
    pub prefix_score: f64,
    /// Score contributed by normalized suffix matching.
    pub suffix_score: f64,
    /// Score contributed by the supplied full-text score.
    pub fts_score: f64,
    /// Score contributed by matching the node kind.
    pub type_score: f64,
    /// Score contributed by explicit service or community scope.
    pub scope_score: f64,
    /// Score contributed by supplied centrality.
    pub centrality_score: f64,
    /// Score contributed by optional community membership.
    pub community_score: f64,
    /// Score contributed by bounded evidence quality.
    pub evidence_score: f64,
    /// Score subtracted for stale, partial, unknown, or unavailable inputs.
    pub freshness_penalty: f64,
}

/// One ranked search result without source bodies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchHit {
    /// Matched graph node.
    pub node: Node,
    /// Deterministic aggregate score.
    pub score: f64,
    /// Auditable score decomposition.
    pub explanation: SearchExplanation,
}

/// Coverage and degradation details for a search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchCoverage {
    /// Distinct input nodes considered.
    pub input_nodes: usize,
    /// Nodes remaining after scope filters.
    pub eligible_nodes: usize,
    /// Eligible nodes with a lexical or full-text match.
    pub matched_nodes: usize,
    /// Nodes for which a full-text score was supplied.
    pub fts_scored_nodes: usize,
    /// Repositories whose freshness was required but not supplied.
    pub freshness_unknown_repositories: Vec<RepoId>,
    /// Missing optional signals or other coverage limitations.
    pub gaps: Vec<String>,
}

/// Deterministic non-executing navigation suggested to an agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentNextAction {
    /// Public tool to call next.
    pub tool: String,
    /// Exact real identifiers or bounded query arguments for the tool.
    pub arguments: BTreeMap<String, String>,
    /// Short explanation of why the action is useful.
    pub rationale: String,
}

/// Paginated ranked search result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchReport {
    /// Hits in deterministic score order.
    pub hits: Vec<SearchHit>,
    /// Total matches before pagination.
    pub total_matches: usize,
    /// Applied zero-based offset.
    pub offset: usize,
    /// Applied page-size limit.
    pub limit: usize,
    /// Whether additional matches exist after this page.
    pub truncated: bool,
    /// Search coverage and degradation details.
    pub coverage: SearchCoverage,
    /// Navigation based only on observed repositories and entity identifiers.
    #[serde(default)]
    pub next_actions: Vec<AgentNextAction>,
}

/// Traversal strategy used to find confirmed paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TraversalAlgorithm {
    /// Deterministic shortest path by hop count.
    Bfs,
    /// Deterministic shortest path by conservative edge weight.
    Dijkstra,
    /// Deterministic bounded enumeration of loopless paths by total weight.
    KShortest,
}

/// Direction in which graph relationships may be traversed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TraversalDirection {
    /// Follow relationships from source to target.
    Outgoing,
    /// Follow relationships from target to source.
    Incoming,
    /// Follow relationships in either direction.
    Both,
}

/// Filters applied to nodes and edges during traversal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TraversalFilters {
    /// Eligible relationship kinds; an empty list accepts every kind.
    #[serde(default)]
    pub edge_kinds: Vec<EdgeKind>,
    /// Minimum edge confidence in the inclusive range from zero to one.
    pub min_confidence: f64,
    /// Whether paths may include test-case nodes.
    pub include_tests: bool,
    /// Whether paths may include artifacts identified as generated.
    pub include_generated_artifacts: bool,
    /// Deployment namespace required when a deployment stable key represents one.
    pub environment_namespace: Option<String>,
}

impl Default for TraversalFilters {
    fn default() -> Self {
        Self {
            edge_kinds: Vec::new(),
            min_confidence: 0.0,
            include_tests: true,
            include_generated_artifacts: true,
            environment_namespace: None,
        }
    }
}

/// Positive base cost assigned to one relationship kind.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EdgeKindCost {
    /// Relationship kind receiving the custom cost.
    pub kind: EdgeKind,
    /// Positive finite base cost divided by edge confidence.
    pub cost: f64,
}

/// Bounds and costs controlling one graph traversal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TraversalOptions {
    /// Traversal strategy.
    pub algorithm: TraversalAlgorithm,
    /// Relationship direction.
    pub direction: TraversalDirection,
    /// Maximum number of segments in a path.
    pub max_depth: usize,
    /// Maximum number of cross-repository segments in a path.
    pub max_cross_repo_hops: usize,
    /// Maximum number of distinct nodes observed.
    pub node_limit: usize,
    /// Maximum number of relationship examinations.
    pub edge_limit: usize,
    /// Wall-clock budget in milliseconds.
    pub timeout_ms: u64,
    /// Number of paths requested for `k_shortest`; inclusively bounded by 16.
    pub k: usize,
    /// Optional positive base costs keyed by relationship kind.
    #[serde(default)]
    pub edge_kind_costs: Vec<EdgeKindCost>,
}

impl Default for TraversalOptions {
    fn default() -> Self {
        Self {
            algorithm: TraversalAlgorithm::Bfs,
            direction: TraversalDirection::Outgoing,
            max_depth: 8,
            max_cross_repo_hops: 4,
            node_limit: 10_000,
            edge_limit: 50_000,
            timeout_ms: 1_000,
            k: 1,
            edge_kind_costs: Vec::new(),
        }
    }
}

/// Input for one bounded confirmed-path traversal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TraversalRequest {
    /// Starting node identifier.
    pub start: NodeId,
    /// Destination node identifier.
    pub target: NodeId,
    /// Node and edge filters.
    #[serde(default)]
    pub filters: TraversalFilters,
    /// Traversal limits, algorithm, and costs.
    #[serde(default)]
    pub options: TraversalOptions,
}

/// Whether a path segment remains within one repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PathSegmentScope {
    /// Both endpoints have the same repository ownership.
    Local,
    /// Endpoint repository ownership differs.
    CrossRepository,
}

/// One ordered segment in a confirmed traversal path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PathSegment {
    /// Existing model trace segment in traversal order.
    pub trace: TraceSegment,
    /// Local or cross-repository classification.
    pub scope: PathSegmentScope,
    /// Whether the underlying directed edge was followed in reverse.
    pub reversed: bool,
    /// Positive finite cost used by weighted algorithms.
    pub weight: f64,
}

/// One loopless confirmed path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TraversalPath {
    /// Ordered path segments.
    pub segments: Vec<PathSegment>,
    /// Sum of positive finite segment weights.
    pub total_weight: f64,
    /// Number of cross-repository segments.
    pub cross_repo_hops: usize,
}

/// Effective traversal limits echoed in a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TraversalLimits {
    /// Maximum path depth.
    pub max_depth: usize,
    /// Maximum cross-repository hops per path.
    pub max_cross_repo_hops: usize,
    /// Maximum distinct observed nodes.
    pub node_limit: usize,
    /// Maximum examined edges.
    pub edge_limit: usize,
    /// Timeout in milliseconds.
    pub timeout_ms: u64,
    /// Maximum returned paths.
    pub max_paths: usize,
}

/// Bounded traversal result with explicit uncertainty and frontier data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TraversalReport {
    /// Confirmed paths in deterministic algorithm order.
    pub paths: Vec<TraversalPath>,
    /// Deepest reachable confirmed nodes observed before completion or truncation.
    pub frontier: Vec<Node>,
    /// Encountered non-confirmed edges, excluded from every returned path.
    pub candidate_unresolved_edges: Vec<Edge>,
    /// Missing paths, malformed representable scopes, or exhausted bounds.
    pub coverage_gaps: Vec<String>,
    /// Whether any configured bound stopped exploration.
    pub truncated: bool,
    /// Number of distinct confirmed nodes observed.
    pub visited_nodes: usize,
    /// Number of edges examined, including unresolved candidates.
    pub examined_edges: usize,
    /// Effective limits used for this traversal.
    pub limits: TraversalLimits,
}

/// Validation or consistency error from ranked search or traversal.
#[derive(Debug, Error, PartialEq)]
pub enum QueryError {
    /// Search text is empty after normalization.
    #[error("search query must contain at least one letter or digit")]
    EmptyQuery,
    /// Search pagination is outside the supported bound.
    #[error(
        "search limit must be in 1..={MAX_SEARCH_RESULTS} and offset must not exceed {MAX_SEARCH_OFFSET}"
    )]
    InvalidPagination,
    /// A supplied score is not finite or is negative.
    #[error("invalid {signal} score for node `{node}`")]
    InvalidScore {
        /// Name of the invalid signal.
        signal: &'static str,
        /// Node carrying the invalid score.
        node: String,
    },
    /// More than one node has the same identifier.
    #[error("duplicate node identifier `{0}`")]
    DuplicateNode(String),
    /// More than one edge has the same identifier.
    #[error("duplicate edge identifier `{0}`")]
    DuplicateEdge(String),
    /// An edge endpoint is absent from the supplied graph.
    #[error("edge `{edge}` references missing node `{node}`")]
    DanglingEdge {
        /// Stable edge identifier.
        edge: String,
        /// Missing node identifier.
        node: String,
    },
    /// A traversal anchor is absent from the supplied graph.
    #[error("traversal anchor `{0}` is not present")]
    UnknownAnchor(String),
    /// One or more traversal limits are zero or exceed hard bounds.
    #[error("traversal limits are outside supported bounds")]
    InvalidTraversalLimits,
    /// Minimum confidence is not finite or lies outside zero through one.
    #[error("minimum confidence must be finite and in the inclusive range 0..=1")]
    InvalidMinimumConfidence,
    /// An edge has invalid confidence for conservative weighting.
    #[error("edge `{0}` confidence must be finite and in the inclusive range 0..=1")]
    InvalidEdgeConfidence(String),
    /// An edge-kind cost is not finite and strictly positive.
    #[error("edge-kind cost for `{0:?}` must be finite and positive")]
    InvalidEdgeCost(EdgeKind),
    /// Internal path reconstruction found inconsistent graph state.
    #[error("path references missing node `{0}`")]
    InconsistentPath(String),
}

/// Performs deterministic ranked search over graph nodes.
///
/// Duplicate node identifiers and invalid numeric signals are rejected. Returned hits include
/// node metadata and score explanations only; evidence and source bodies are never returned.
///
/// # Errors
///
/// Returns [`QueryError`] when the query, pagination, node identities, or numeric signals are
/// invalid.
#[must_use = "search results and validation errors must be handled"]
pub fn search(nodes: &[Node], request: &SearchRequest) -> Result<SearchReport, QueryError> {
    let normalized_query = normalize(&request.query);
    validate_search(nodes, request, &normalized_query)?;
    let distinct = distinct_nodes(nodes)?;
    let mut unknown_freshness = BTreeSet::new();
    let mut hits = Vec::new();
    let mut eligible_nodes = 0;

    for node in distinct.values().copied() {
        if !matches_search_filters(node, request) {
            continue;
        }
        eligible_nodes += 1;
        if let Some(hit) = score_node(node, request, &normalized_query, &mut unknown_freshness) {
            hits.push(hit);
        }
    }
    sort_search_hits(&mut hits);
    let total_matches = hits.len();
    let page_end = request
        .offset
        .saturating_add(request.limit)
        .min(total_matches);
    let page = hits
        .into_iter()
        .skip(request.offset.min(total_matches))
        .take(page_end.saturating_sub(request.offset))
        .collect();
    let coverage = search_coverage(
        distinct.len(),
        eligible_nodes,
        total_matches,
        request,
        unknown_freshness,
    );
    Ok(SearchReport {
        hits: page,
        total_matches,
        offset: request.offset,
        limit: request.limit,
        truncated: page_end < total_matches,
        coverage,
        next_actions: Vec::new(),
    })
}

fn validate_search(
    nodes: &[Node],
    request: &SearchRequest,
    normalized_query: &str,
) -> Result<(), QueryError> {
    if normalized_query.is_empty() {
        return Err(QueryError::EmptyQuery);
    }
    if request.limit == 0
        || request.limit > MAX_SEARCH_RESULTS
        || request.offset > MAX_SEARCH_OFFSET
    {
        return Err(QueryError::InvalidPagination);
    }
    let _ = distinct_nodes(nodes)?;
    validate_score_map(&request.fts_scores, "full-text")?;
    validate_score_map(&request.centrality_scores, "centrality")?;
    for (node_id, evidence) in &request.evidence {
        if evidence
            .iter()
            .any(|item| !item.confidence.is_finite() || !(0.0..=1.0).contains(&item.confidence))
        {
            return Err(QueryError::InvalidScore {
                signal: "evidence confidence",
                node: node_id.as_str().to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_score_map(
    scores: &BTreeMap<NodeId, f64>,
    signal: &'static str,
) -> Result<(), QueryError> {
    for (node_id, score) in scores {
        if !score.is_finite() || *score < 0.0 {
            return Err(QueryError::InvalidScore {
                signal,
                node: node_id.as_str().to_owned(),
            });
        }
    }
    Ok(())
}

fn distinct_nodes(nodes: &[Node]) -> Result<BTreeMap<NodeId, &Node>, QueryError> {
    let mut distinct = BTreeMap::new();
    for node in nodes {
        if distinct.insert(node.id.clone(), node).is_some() {
            return Err(QueryError::DuplicateNode(node.id.as_str().to_owned()));
        }
    }
    Ok(distinct)
}

fn matches_search_filters(node: &Node, request: &SearchRequest) -> bool {
    let filters = &request.filters;
    if !filters.node_kinds.is_empty() && !filters.node_kinds.contains(&node.kind) {
        return false;
    }
    if !filters.repo_ids.is_empty()
        && node
            .repo_id
            .as_ref()
            .is_none_or(|repo_id| !filters.repo_ids.contains(repo_id))
    {
        return false;
    }
    if !filters.workspace_nodes.is_empty() && !filters.workspace_nodes.contains(&node.id) {
        return false;
    }
    if !membership_matches(
        request.service_memberships.get(&node.id),
        &filters.service_ids,
    ) {
        return false;
    }
    membership_matches(
        request.community_memberships.get(&node.id),
        &filters.community_ids,
    )
}

fn membership_matches<T: PartialEq>(memberships: Option<&Vec<T>>, required: &[T]) -> bool {
    required.is_empty()
        || memberships.is_some_and(|values| {
            required
                .iter()
                .any(|required_value| values.contains(required_value))
        })
}

fn score_node(
    node: &Node,
    request: &SearchRequest,
    query: &str,
    unknown_freshness: &mut BTreeSet<RepoId>,
) -> Option<SearchHit> {
    let stable_key = normalize(&node.stable_key);
    let label = normalize(&node.label);
    let kind = node_kind_name(node.kind);
    let mut explanation = lexical_explanation(query, &stable_key, &label, kind);
    let supplied_fts = request.fts_scores.get(&node.id).copied().unwrap_or(0.0);
    if explanation.matched_fields.is_empty() && supplied_fts <= 0.0 {
        return None;
    }
    explanation.fts_score = supplied_fts.min(1.0) * 2.0;
    explanation.type_score = type_signal(query, kind);
    explanation.scope_score = scope_signal(node, request);
    explanation.centrality_score = request
        .centrality_scores
        .get(&node.id)
        .copied()
        .unwrap_or(0.0)
        .min(1.0)
        * 0.5;
    explanation.community_score = community_signal(node, request);
    explanation.evidence_score = evidence_signal(node, request);
    explanation.freshness_penalty = freshness_penalty(node, request, unknown_freshness);
    let score = explanation_score(&explanation).max(0.0);
    Some(SearchHit {
        node: node.clone(),
        score,
        explanation,
    })
}

fn lexical_explanation(
    query: &str,
    stable_key: &str,
    label: &str,
    kind: &str,
) -> SearchExplanation {
    let mut matched_fields = Vec::new();
    let exact = stable_key == query || label == query;
    if stable_key == query {
        matched_fields.push("stable_key_exact".to_owned());
    }
    if label == query {
        matched_fields.push("label_exact".to_owned());
    }
    let prefix = !exact && (stable_key.starts_with(query) || label.starts_with(query));
    if prefix {
        matched_fields.push("normalized_prefix".to_owned());
    }
    let suffix = !exact && (stable_key.ends_with(query) || label.ends_with(query));
    if suffix {
        matched_fields.push("normalized_suffix".to_owned());
    }
    if kind == query {
        matched_fields.push("node_kind".to_owned());
    }
    SearchExplanation {
        matched_fields,
        exact_score: if exact { 4.0 } else { 0.0 },
        prefix_score: if prefix { 1.5 } else { 0.0 },
        suffix_score: if suffix { 1.0 } else { 0.0 },
        fts_score: 0.0,
        type_score: 0.0,
        scope_score: 0.0,
        centrality_score: 0.0,
        community_score: 0.0,
        evidence_score: 0.0,
        freshness_penalty: 0.0,
    }
}

fn type_signal(query: &str, kind: &str) -> f64 {
    if kind == query {
        0.5
    } else if kind.starts_with(query) || kind.ends_with(query) {
        0.25
    } else {
        0.0
    }
}

fn scope_signal(node: &Node, request: &SearchRequest) -> f64 {
    let mut score: f64 = 0.0;
    if !request.filters.repo_ids.is_empty() {
        score += 0.15;
    }
    if !request.filters.workspace_nodes.is_empty() {
        score += 0.1;
    }
    if !request.filters.service_ids.is_empty()
        && membership_matches(
            request.service_memberships.get(&node.id),
            &request.filters.service_ids,
        )
    {
        score += 0.15;
    }
    score.min(0.4)
}

fn community_signal(node: &Node, request: &SearchRequest) -> f64 {
    let Some(memberships) = request.community_memberships.get(&node.id) else {
        return 0.0;
    };
    if request.filters.community_ids.is_empty() {
        if memberships.is_empty() { 0.0 } else { 0.1 }
    } else if membership_matches(Some(memberships), &request.filters.community_ids) {
        0.4
    } else {
        0.0
    }
}

fn evidence_signal(node: &Node, request: &SearchRequest) -> f64 {
    let Some(items) = request.evidence.get(&node.id) else {
        return 0.0;
    };
    if items.is_empty() {
        return 0.0;
    }
    let (confidence_sum, denominator) = items.iter().fold((0.0, 0.0), |(sum, count), item| {
        (sum + f64::from(item.confidence), count + 1.0)
    });
    (confidence_sum / denominator).min(1.0) * 0.75
}

fn freshness_penalty(node: &Node, request: &SearchRequest, unknown: &mut BTreeSet<RepoId>) -> f64 {
    let Some(repo_id) = node.repo_id.as_ref() else {
        return 0.0;
    };
    let Some(state) = request.freshness.get(repo_id) else {
        unknown.insert(repo_id.clone());
        return 0.2;
    };
    match state {
        RepoFreshnessState::Fresh => 0.0,
        RepoFreshnessState::WorkingTreeChanged => 0.05,
        RepoFreshnessState::CommitsBehind => 0.1,
        RepoFreshnessState::ConfigChanged
        | RepoFreshnessState::ExtractorChanged
        | RepoFreshnessState::CodegraphPending => 0.15,
        RepoFreshnessState::Partial | RepoFreshnessState::Unknown => 0.2,
        RepoFreshnessState::Corrupt => 0.4,
        RepoFreshnessState::Unavailable => 0.5,
    }
}

fn explanation_score(explanation: &SearchExplanation) -> f64 {
    explanation.exact_score
        + explanation.prefix_score
        + explanation.suffix_score
        + explanation.fts_score
        + explanation.type_score
        + explanation.scope_score
        + explanation.centrality_score
        + explanation.community_score
        + explanation.evidence_score
        - explanation.freshness_penalty
}

fn sort_search_hits(hits: &mut [SearchHit]) {
    hits.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.node.stable_key.cmp(&right.node.stable_key))
            .then_with(|| left.node.id.cmp(&right.node.id))
    });
}

fn search_coverage(
    input_nodes: usize,
    eligible_nodes: usize,
    matched_nodes: usize,
    request: &SearchRequest,
    unknown: BTreeSet<RepoId>,
) -> SearchCoverage {
    let mut gaps = Vec::new();
    if request.fts_scores.is_empty() {
        gaps.push("Full-text scores were not provided.".to_owned());
    }
    if request.centrality_scores.is_empty() {
        gaps.push("Centrality scores were not provided.".to_owned());
    }
    if request.evidence.is_empty() {
        gaps.push("Node evidence was not provided.".to_owned());
    }
    if request.community_memberships.is_empty() {
        gaps.push("Community memberships were not provided.".to_owned());
    }
    if !unknown.is_empty() {
        gaps.push("Freshness was unavailable for one or more repositories.".to_owned());
    }
    SearchCoverage {
        input_nodes,
        eligible_nodes,
        matched_nodes,
        fts_scored_nodes: request.fts_scores.len(),
        freshness_unknown_repositories: unknown.into_iter().collect(),
        gaps,
    }
}

fn normalize(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut separator_pending = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            if separator_pending && !result.is_empty() {
                result.push(' ');
            }
            result.push(character);
            separator_pending = false;
        } else {
            separator_pending = true;
        }
    }
    result
}

fn node_kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Repository => "repository",
        NodeKind::Service => "service",
        NodeKind::Package => "package",
        NodeKind::Artifact => "artifact",
        NodeKind::SymbolRef => "symbol ref",
        NodeKind::TestCase => "test case",
        NodeKind::HttpOperation => "http operation",
        NodeKind::GraphqlOperation => "graphql operation",
        NodeKind::RpcMethod => "rpc method",
        NodeKind::EventChannel => "event channel",
        NodeKind::EventSchema => "event schema",
        NodeKind::Database => "database",
        NodeKind::DatabaseTable => "database table",
        NodeKind::DatabaseColumn => "database column",
        NodeKind::ConfigKey => "config key",
        NodeKind::Deployment => "deployment",
        NodeKind::Document => "document",
        NodeKind::Adr => "adr",
        NodeKind::Owner => "owner",
        NodeKind::ChangeSet => "change set",
        NodeKind::PullRequest => "pull request",
        NodeKind::Community => "community",
    }
}

/// Finds confirmed paths using the requested bounded traversal strategy.
///
/// Non-confirmed edges are never traversed. Encountered inferred, ambiguous, stale, or incomplete
/// edges are returned separately as unresolved candidates.
///
/// # Errors
///
/// Returns [`QueryError`] for invalid graph identities, dangling endpoints, missing anchors,
/// invalid confidence values, invalid costs, or unsupported limits.
#[must_use = "traversal results and validation errors must be handled"]
pub fn traverse(
    nodes: &[Node],
    edges: &[Edge],
    request: &TraversalRequest,
) -> Result<TraversalReport, QueryError> {
    validate_traversal_request(request)?;
    let graph = TraversalGraph::new(nodes, edges)?;
    graph.require_anchor(&request.start)?;
    graph.require_anchor(&request.target)?;
    let mut runtime = TraversalRuntime::new(request);
    let paths = if request.start == request.target {
        vec![TraversalPath {
            segments: Vec::new(),
            total_weight: 0.0,
            cross_repo_hops: 0,
        }]
    } else {
        match request.options.algorithm {
            TraversalAlgorithm::Bfs => bfs(&graph, request, &mut runtime)?,
            TraversalAlgorithm::Dijkstra => weighted_paths(&graph, request, &mut runtime, 1)?,
            TraversalAlgorithm::KShortest => {
                weighted_paths(&graph, request, &mut runtime, request.options.k)?
            }
        }
    };
    build_traversal_report(&graph, request, runtime, paths)
}

fn validate_traversal_request(request: &TraversalRequest) -> Result<(), QueryError> {
    let options = &request.options;
    if options.max_depth == 0
        || options.max_depth > MAX_TRAVERSAL_DEPTH
        || options.node_limit == 0
        || options.node_limit > MAX_TRAVERSAL_NODES
        || options.edge_limit == 0
        || options.edge_limit > MAX_TRAVERSAL_EDGES
        || options.timeout_ms > MAX_TRAVERSAL_TIMEOUT_MS
        || options.k == 0
        || options.k > MAX_K_PATHS
    {
        return Err(QueryError::InvalidTraversalLimits);
    }
    let confidence = request.filters.min_confidence;
    if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
        return Err(QueryError::InvalidMinimumConfidence);
    }
    let mut cost_kinds = Vec::new();
    for item in &options.edge_kind_costs {
        if !item.cost.is_finite() || item.cost <= 0.0 || cost_kinds.contains(&item.kind) {
            return Err(QueryError::InvalidEdgeCost(item.kind));
        }
        cost_kinds.push(item.kind);
    }
    Ok(())
}

struct TraversalGraph<'a> {
    nodes: BTreeMap<NodeId, &'a Node>,
    outgoing: BTreeMap<NodeId, Vec<&'a Edge>>,
    incoming: BTreeMap<NodeId, Vec<&'a Edge>>,
}

impl<'a> TraversalGraph<'a> {
    fn new(nodes: &'a [Node], edges: &'a [Edge]) -> Result<Self, QueryError> {
        let nodes = distinct_nodes(nodes)?;
        let mut edge_ids = BTreeSet::new();
        let mut outgoing: BTreeMap<NodeId, Vec<&Edge>> = BTreeMap::new();
        let mut incoming: BTreeMap<NodeId, Vec<&Edge>> = BTreeMap::new();
        for edge in edges {
            if !edge_ids.insert(edge.id.clone()) {
                return Err(QueryError::DuplicateEdge(edge.id.as_str().to_owned()));
            }
            validate_graph_edge(edge, &nodes)?;
            outgoing.entry(edge.source.clone()).or_default().push(edge);
            incoming.entry(edge.target.clone()).or_default().push(edge);
        }
        for adjacent in outgoing.values_mut().chain(incoming.values_mut()) {
            adjacent.sort_by(|left, right| left.id.cmp(&right.id));
        }
        Ok(Self {
            nodes,
            outgoing,
            incoming,
        })
    }

    fn require_anchor(&self, node_id: &NodeId) -> Result<(), QueryError> {
        if self.nodes.contains_key(node_id) {
            Ok(())
        } else {
            Err(QueryError::UnknownAnchor(node_id.as_str().to_owned()))
        }
    }

    fn adjacent(&self, node_id: &NodeId, direction: TraversalDirection) -> Vec<Adjacent<'a>> {
        let mut result = Vec::new();
        if matches!(
            direction,
            TraversalDirection::Outgoing | TraversalDirection::Both
        ) {
            result.extend(
                self.outgoing
                    .get(node_id)
                    .into_iter()
                    .flatten()
                    .map(|edge| Adjacent {
                        edge,
                        next: &edge.target,
                        reversed: false,
                    }),
            );
        }
        if matches!(
            direction,
            TraversalDirection::Incoming | TraversalDirection::Both
        ) {
            result.extend(
                self.incoming
                    .get(node_id)
                    .into_iter()
                    .flatten()
                    .map(|edge| Adjacent {
                        edge,
                        next: &edge.source,
                        reversed: true,
                    }),
            );
        }
        result.sort_by(adjacent_order);
        result.dedup_by(|left, right| {
            left.edge.id == right.edge.id
                && left.next == right.next
                && left.reversed == right.reversed
        });
        result
    }
}

fn validate_graph_edge(edge: &Edge, nodes: &BTreeMap<NodeId, &Node>) -> Result<(), QueryError> {
    for endpoint in [&edge.source, &edge.target] {
        if !nodes.contains_key(endpoint) {
            return Err(QueryError::DanglingEdge {
                edge: edge.id.as_str().to_owned(),
                node: endpoint.as_str().to_owned(),
            });
        }
    }
    let confidence = f64::from(edge.confidence);
    if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
        return Err(QueryError::InvalidEdgeConfidence(
            edge.id.as_str().to_owned(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Adjacent<'a> {
    edge: &'a Edge,
    next: &'a NodeId,
    reversed: bool,
}

fn adjacent_order(left: &Adjacent<'_>, right: &Adjacent<'_>) -> Ordering {
    left.edge
        .id
        .cmp(&right.edge.id)
        .then_with(|| left.next.cmp(right.next))
        .then_with(|| left.reversed.cmp(&right.reversed))
}

#[derive(Clone)]
struct PathState<'a> {
    nodes: Vec<NodeId>,
    steps: Vec<Adjacent<'a>>,
    total_weight: f64,
    cross_repo_hops: usize,
}

impl<'a> PathState<'a> {
    fn start(node_id: &NodeId) -> Self {
        Self {
            nodes: vec![node_id.clone()],
            steps: Vec::new(),
            total_weight: 0.0,
            cross_repo_hops: 0,
        }
    }

    fn current(&self) -> Option<&NodeId> {
        self.nodes.last()
    }

    fn contains(&self, node_id: &NodeId) -> bool {
        self.nodes.contains(node_id)
    }

    fn extended(&self, adjacent: Adjacent<'a>, weight: f64, cross_repo: bool) -> Self {
        let mut result = self.clone();
        result.nodes.push(adjacent.next.clone());
        result.steps.push(adjacent);
        result.total_weight += weight;
        result.cross_repo_hops += usize::from(cross_repo);
        result
    }
}

struct TraversalRuntime {
    started: Instant,
    timeout: Duration,
    observed_nodes: BTreeSet<NodeId>,
    examined_edges: usize,
    candidate_edges: BTreeMap<EdgeId, Edge>,
    frontier_depth: usize,
    frontier: BTreeSet<NodeId>,
    coverage_gaps: BTreeSet<String>,
    truncated: bool,
}

impl TraversalRuntime {
    fn new(request: &TraversalRequest) -> Self {
        Self {
            started: Instant::now(),
            timeout: Duration::from_millis(request.options.timeout_ms),
            observed_nodes: BTreeSet::from([request.start.clone()]),
            examined_edges: 0,
            candidate_edges: BTreeMap::new(),
            frontier_depth: 0,
            frontier: BTreeSet::from([request.start.clone()]),
            coverage_gaps: BTreeSet::new(),
            truncated: false,
        }
    }

    fn timed_out(&mut self) -> bool {
        if self.started.elapsed() >= self.timeout {
            self.truncated = true;
            self.coverage_gaps
                .insert("Traversal stopped at the timeout bound.".to_owned());
            true
        } else {
            false
        }
    }

    fn examine_edge(&mut self, edge_limit: usize) -> bool {
        if self.examined_edges >= edge_limit {
            self.truncated = true;
            self.coverage_gaps
                .insert("Traversal stopped at the edge limit.".to_owned());
            false
        } else {
            self.examined_edges += 1;
            true
        }
    }

    fn observe_node(&mut self, node_id: &NodeId, node_limit: usize) -> bool {
        if self.observed_nodes.contains(node_id) {
            return true;
        }
        if self.observed_nodes.len() >= node_limit {
            self.truncated = true;
            self.coverage_gaps
                .insert("Traversal stopped at the node limit.".to_owned());
            false
        } else {
            self.observed_nodes.insert(node_id.clone());
            true
        }
    }

    fn record_frontier(&mut self, node_id: &NodeId, depth: usize) {
        match depth.cmp(&self.frontier_depth) {
            Ordering::Greater => {
                self.frontier_depth = depth;
                self.frontier.clear();
                self.frontier.insert(node_id.clone());
            }
            Ordering::Equal => {
                self.frontier.insert(node_id.clone());
            }
            Ordering::Less => {}
        }
    }

    fn record_candidate(&mut self, edge: &Edge) {
        self.candidate_edges
            .entry(edge.id.clone())
            .or_insert_with(|| edge.clone());
    }

    fn record_depth_bound(&mut self) {
        self.truncated = true;
        self.coverage_gaps
            .insert("Traversal stopped at the depth limit.".to_owned());
    }

    fn record_cross_repo_bound(&mut self) {
        self.truncated = true;
        self.coverage_gaps
            .insert("A path branch exceeded the cross-repository hop limit.".to_owned());
    }
}

fn bfs(
    graph: &TraversalGraph<'_>,
    request: &TraversalRequest,
    runtime: &mut TraversalRuntime,
) -> Result<Vec<TraversalPath>, QueryError> {
    let mut queue = VecDeque::from([PathState::start(&request.start)]);
    while let Some(state) = queue.pop_front() {
        if runtime.timed_out() {
            break;
        }
        let Some(current) = state.current() else {
            continue;
        };
        runtime.record_frontier(current, state.steps.len());
        if current == &request.target {
            return Ok(vec![materialize_path(graph, &state, request)?]);
        }
        if state.steps.len() >= request.options.max_depth {
            runtime.record_depth_bound();
            continue;
        }
        expand_bfs_state(graph, request, runtime, &state, &mut queue)?;
        if runtime.truncated && queue.is_empty() {
            break;
        }
    }
    Ok(Vec::new())
}

fn expand_bfs_state<'a>(
    graph: &TraversalGraph<'a>,
    request: &TraversalRequest,
    runtime: &mut TraversalRuntime,
    state: &PathState<'a>,
    queue: &mut VecDeque<PathState<'a>>,
) -> Result<(), QueryError> {
    let Some(current) = state.current() else {
        return Ok(());
    };
    for adjacent in graph.adjacent(current, request.options.direction) {
        if runtime.timed_out() || !runtime.examine_edge(request.options.edge_limit) {
            break;
        }
        if !edge_passes_filters(adjacent.edge, &request.filters) {
            continue;
        }
        if adjacent.edge.status != EpistemicStatus::Confirmed {
            runtime.record_candidate(adjacent.edge);
            continue;
        }
        if state.contains(adjacent.next)
            || !node_passes_filters(graph, adjacent.next, &request.filters, runtime)
        {
            continue;
        }
        let cross_repo = segment_is_cross_repo(graph, current, adjacent.next)?;
        if state.cross_repo_hops + usize::from(cross_repo) > request.options.max_cross_repo_hops {
            runtime.record_cross_repo_bound();
            continue;
        }
        if !runtime.observe_node(adjacent.next, request.options.node_limit) {
            continue;
        }
        let weight = edge_weight(adjacent.edge, &request.options)?;
        queue.push_back(state.extended(adjacent, weight, cross_repo));
    }
    Ok(())
}

fn weighted_paths(
    graph: &TraversalGraph<'_>,
    request: &TraversalRequest,
    runtime: &mut TraversalRuntime,
    wanted: usize,
) -> Result<Vec<TraversalPath>, QueryError> {
    let mut candidates = vec![PathState::start(&request.start)];
    let mut paths = Vec::new();
    while !candidates.is_empty() && paths.len() < wanted {
        if runtime.timed_out() {
            break;
        }
        let selected = select_best_state(&candidates);
        let state = candidates.remove(selected);
        let Some(current) = state.current() else {
            continue;
        };
        runtime.record_frontier(current, state.steps.len());
        if current == &request.target {
            paths.push(materialize_path(graph, &state, request)?);
            continue;
        }
        if state.steps.len() >= request.options.max_depth {
            runtime.record_depth_bound();
            continue;
        }
        expand_weighted_state(graph, request, runtime, &state, &mut candidates)?;
    }
    paths.sort_by(path_order);
    Ok(paths)
}

fn select_best_state(candidates: &[PathState<'_>]) -> usize {
    let mut best = 0;
    for index in 1..candidates.len() {
        if path_state_order(&candidates[index], &candidates[best]) == Ordering::Less {
            best = index;
        }
    }
    best
}

fn path_state_order(left: &PathState<'_>, right: &PathState<'_>) -> Ordering {
    left.total_weight
        .total_cmp(&right.total_weight)
        .then_with(|| left.nodes.cmp(&right.nodes))
        .then_with(|| {
            left.steps
                .iter()
                .map(|step| &step.edge.id)
                .cmp(right.steps.iter().map(|step| &step.edge.id))
        })
        .then_with(|| {
            left.steps
                .iter()
                .map(|step| step.reversed)
                .cmp(right.steps.iter().map(|step| step.reversed))
        })
}

fn expand_weighted_state<'a>(
    graph: &TraversalGraph<'a>,
    request: &TraversalRequest,
    runtime: &mut TraversalRuntime,
    state: &PathState<'a>,
    candidates: &mut Vec<PathState<'a>>,
) -> Result<(), QueryError> {
    let Some(current) = state.current() else {
        return Ok(());
    };
    for adjacent in graph.adjacent(current, request.options.direction) {
        if runtime.timed_out() || !runtime.examine_edge(request.options.edge_limit) {
            break;
        }
        if !edge_passes_filters(adjacent.edge, &request.filters) {
            continue;
        }
        if adjacent.edge.status != EpistemicStatus::Confirmed {
            runtime.record_candidate(adjacent.edge);
            continue;
        }
        if state.contains(adjacent.next)
            || !node_passes_filters(graph, adjacent.next, &request.filters, runtime)
        {
            continue;
        }
        let cross_repo = segment_is_cross_repo(graph, current, adjacent.next)?;
        if state.cross_repo_hops + usize::from(cross_repo) > request.options.max_cross_repo_hops {
            runtime.record_cross_repo_bound();
            continue;
        }
        if !runtime.observe_node(adjacent.next, request.options.node_limit) {
            continue;
        }
        let weight = edge_weight(adjacent.edge, &request.options)?;
        candidates.push(state.extended(adjacent, weight, cross_repo));
    }
    Ok(())
}

fn edge_passes_filters(edge: &Edge, filters: &TraversalFilters) -> bool {
    (filters.edge_kinds.is_empty() || filters.edge_kinds.contains(&edge.kind))
        && f64::from(edge.confidence) >= filters.min_confidence
}

fn node_passes_filters(
    graph: &TraversalGraph<'_>,
    node_id: &NodeId,
    filters: &TraversalFilters,
    runtime: &mut TraversalRuntime,
) -> bool {
    let Some(node) = graph.nodes.get(node_id).copied() else {
        return false;
    };
    if !filters.include_tests && node.kind == NodeKind::TestCase {
        return false;
    }
    if !filters.include_generated_artifacts
        && node.kind == NodeKind::Artifact
        && is_generated_artifact(node)
    {
        return false;
    }
    let Some(namespace) = filters.environment_namespace.as_deref() else {
        return true;
    };
    if node.kind != NodeKind::Deployment {
        return true;
    }
    if let Some(candidate) = deployment_namespace(&node.stable_key) {
        candidate == namespace
    } else {
        runtime.coverage_gaps.insert(format!(
            "Deployment `{}` has no representable environment namespace.",
            node.id.as_str()
        ));
        false
    }
}

fn deployment_namespace(stable_key: &str) -> Option<&str> {
    let mut parts = stable_key.splitn(4, ':');
    if parts.next()? != "deployment" {
        return None;
    }
    let _technology = parts.next()?;
    let namespace = parts.next()?;
    let _name = parts.next()?;
    if namespace.is_empty() {
        None
    } else {
        Some(namespace)
    }
}

fn is_generated_artifact(node: &Node) -> bool {
    let key = node.stable_key.to_ascii_lowercase().replace('\\', "/");
    [
        "/generated/",
        "/target/",
        "/dist/",
        "/build/",
        "/vendor/",
        ".generated.",
        "_generated.",
        "/gen/",
    ]
    .iter()
    .any(|marker| key.contains(marker))
}

fn segment_is_cross_repo(
    graph: &TraversalGraph<'_>,
    source: &NodeId,
    target: &NodeId,
) -> Result<bool, QueryError> {
    let source_node = graph
        .nodes
        .get(source)
        .copied()
        .ok_or_else(|| QueryError::InconsistentPath(source.as_str().to_owned()))?;
    let target_node = graph
        .nodes
        .get(target)
        .copied()
        .ok_or_else(|| QueryError::InconsistentPath(target.as_str().to_owned()))?;
    Ok(source_node.repo_id != target_node.repo_id)
}

fn edge_weight(edge: &Edge, options: &TraversalOptions) -> Result<f64, QueryError> {
    let base = options
        .edge_kind_costs
        .iter()
        .find(|item| item.kind == edge.kind)
        .map_or(1.0, |item| item.cost);
    let confidence = f64::from(edge.confidence).max(MIN_CONFIDENCE_FOR_WEIGHT);
    let weight = base / confidence;
    if weight.is_finite() && weight > 0.0 {
        Ok(weight)
    } else {
        Err(QueryError::InvalidEdgeCost(edge.kind))
    }
}

fn materialize_path(
    graph: &TraversalGraph<'_>,
    state: &PathState<'_>,
    request: &TraversalRequest,
) -> Result<TraversalPath, QueryError> {
    let mut segments = Vec::with_capacity(state.steps.len());
    for (index, step) in state.steps.iter().enumerate() {
        let source_id = state
            .nodes
            .get(index)
            .ok_or_else(|| QueryError::InconsistentPath(request.start.as_str().to_owned()))?;
        let target_id = state
            .nodes
            .get(index + 1)
            .ok_or_else(|| QueryError::InconsistentPath(request.target.as_str().to_owned()))?;
        let source = graph
            .nodes
            .get(source_id)
            .copied()
            .ok_or_else(|| QueryError::InconsistentPath(source_id.as_str().to_owned()))?;
        let target = graph
            .nodes
            .get(target_id)
            .copied()
            .ok_or_else(|| QueryError::InconsistentPath(target_id.as_str().to_owned()))?;
        let cross_repo = source.repo_id != target.repo_id;
        segments.push(PathSegment {
            trace: TraceSegment {
                source: source.clone(),
                edge: step.edge.clone(),
                target: target.clone(),
            },
            scope: if cross_repo {
                PathSegmentScope::CrossRepository
            } else {
                PathSegmentScope::Local
            },
            reversed: step.reversed,
            weight: edge_weight(step.edge, &request.options)?,
        });
    }
    Ok(TraversalPath {
        segments,
        total_weight: state.total_weight,
        cross_repo_hops: state.cross_repo_hops,
    })
}

fn path_order(left: &TraversalPath, right: &TraversalPath) -> Ordering {
    left.total_weight
        .total_cmp(&right.total_weight)
        .then_with(|| left.segments.len().cmp(&right.segments.len()))
        .then_with(|| {
            left.segments
                .iter()
                .map(|segment| &segment.trace.edge.id)
                .cmp(right.segments.iter().map(|segment| &segment.trace.edge.id))
        })
        .then_with(|| {
            left.segments
                .iter()
                .map(|segment| &segment.trace.target.id)
                .cmp(
                    right
                        .segments
                        .iter()
                        .map(|segment| &segment.trace.target.id),
                )
        })
}

fn build_traversal_report(
    graph: &TraversalGraph<'_>,
    request: &TraversalRequest,
    mut runtime: TraversalRuntime,
    paths: Vec<TraversalPath>,
) -> Result<TraversalReport, QueryError> {
    if paths.is_empty() {
        runtime.coverage_gaps.insert(
            "No confirmed path was observed within available coverage and bounds.".to_owned(),
        );
    }
    if !runtime.candidate_edges.is_empty() {
        runtime
            .coverage_gaps
            .insert("Unresolved candidate edges were excluded from confirmed paths.".to_owned());
    }
    let frontier = runtime
        .frontier
        .iter()
        .map(|node_id| {
            graph
                .nodes
                .get(node_id)
                .copied()
                .cloned()
                .ok_or_else(|| QueryError::InconsistentPath(node_id.as_str().to_owned()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(TraversalReport {
        paths,
        frontier,
        candidate_unresolved_edges: runtime.candidate_edges.into_values().collect(),
        coverage_gaps: runtime.coverage_gaps.into_iter().collect(),
        truncated: runtime.truncated,
        visited_nodes: runtime.observed_nodes.len(),
        examined_edges: runtime.examined_edges,
        limits: TraversalLimits {
            max_depth: request.options.max_depth,
            max_cross_repo_hops: request.options.max_cross_repo_hops,
            node_limit: request.options.node_limit,
            edge_limit: request.options.edge_limit,
            timeout_ms: request.options.timeout_ms,
            max_paths: if request.options.algorithm == TraversalAlgorithm::KShortest {
                request.options.k
            } else {
                1
            },
        },
    })
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{EdgeId, EvidenceId, Provenance};

    use super::*;

    fn node(id: &str, kind: NodeKind, repo: Option<&str>) -> Node {
        Node {
            id: NodeId::new(id),
            kind,
            repo_id: repo.map(RepoId::new),
            stable_key: id.to_owned(),
            label: id.trim_start_matches("node:").to_owned(),
        }
    }

    fn edge(
        id: &str,
        source: &str,
        target: &str,
        confidence: f32,
        status: EpistemicStatus,
    ) -> Edge {
        Edge {
            id: EdgeId::new(id),
            source: NodeId::new(source),
            target: NodeId::new(target),
            kind: EdgeKind::CallsRemote,
            confidence,
            status,
            evidence: Vec::new(),
        }
    }

    fn evidence(confidence: f32) -> Evidence {
        Evidence {
            id: EvidenceId::new("evidence:1"),
            repo_id: Some(RepoId::new("repo:a")),
            file_path: Some("src/lib.rs".to_owned()),
            start_line: Some(1),
            end_line: Some(1),
            extractor: "test".to_owned(),
            extractor_version: "1.0.0".to_owned(),
            provenance: Provenance::Extracted,
            confidence,
            observed_at_commit: None,
            content_hash: None,
            note: None,
        }
    }

    fn search_request(query: &str, offset: usize, limit: usize) -> SearchRequest {
        SearchRequest {
            query: query.to_owned(),
            filters: SearchFilters::default(),
            fts_scores: BTreeMap::new(),
            centrality_scores: BTreeMap::new(),
            service_memberships: BTreeMap::new(),
            community_memberships: BTreeMap::new(),
            evidence: BTreeMap::new(),
            freshness: BTreeMap::new(),
            offset,
            limit,
        }
    }

    fn traversal_request(
        start: &str,
        target: &str,
        algorithm: TraversalAlgorithm,
    ) -> TraversalRequest {
        TraversalRequest {
            start: NodeId::new(start),
            target: NodeId::new(target),
            filters: TraversalFilters::default(),
            options: TraversalOptions {
                algorithm,
                ..TraversalOptions::default()
            },
        }
    }

    fn report_or_panic(result: Result<TraversalReport, QueryError>) -> TraversalReport {
        match result {
            Ok(report) => report,
            Err(error) => panic!("unexpected traversal error: {error}"),
        }
    }

    fn search_or_panic(result: Result<SearchReport, QueryError>) -> SearchReport {
        match result {
            Ok(report) => report,
            Err(error) => panic!("unexpected search error: {error}"),
        }
    }

    #[test]
    fn search_is_deterministic_and_paginates_without_duplicates() {
        let nodes = vec![
            node("node:alpha-b", NodeKind::Service, Some("repo:a")),
            node("node:alpha-a", NodeKind::Service, Some("repo:a")),
            node("node:alpha-c", NodeKind::Service, Some("repo:a")),
        ];
        let first = search_or_panic(search(&nodes, &search_request("alpha", 0, 2)));
        let second = search_or_panic(search(&nodes, &search_request("alpha", 2, 2)));
        let ids = first
            .hits
            .iter()
            .chain(&second.hits)
            .map(|hit| hit.node.id.as_str())
            .collect::<BTreeSet<_>>();

        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn search_explains_signals_and_freshness_penalty() {
        let nodes = vec![
            node("node:billing", NodeKind::Service, Some("repo:a")),
            node("node:billing-old", NodeKind::Service, Some("repo:b")),
        ];
        let mut request = search_request("billing", 0, 10);
        request.fts_scores.insert(NodeId::new("node:billing"), 0.8);
        request
            .centrality_scores
            .insert(NodeId::new("node:billing"), 0.5);
        request
            .evidence
            .insert(NodeId::new("node:billing"), vec![evidence(0.9)]);
        request
            .freshness
            .insert(RepoId::new("repo:a"), RepoFreshnessState::Fresh);
        request
            .freshness
            .insert(RepoId::new("repo:b"), RepoFreshnessState::Unavailable);
        let report = search_or_panic(search(&nodes, &request));

        assert!(
            report.hits[0].explanation.fts_score > 0.0
                && report.hits[0].explanation.centrality_score > 0.0
                && report.hits[0].explanation.evidence_score > 0.0
                && report.hits[1].explanation.freshness_penalty > 0.0
        );
    }

    #[test]
    fn bfs_honors_direction_and_returns_frontier() {
        let nodes = vec![
            node("node:a", NodeKind::Service, Some("repo:a")),
            node("node:b", NodeKind::Service, Some("repo:a")),
            node("node:c", NodeKind::Service, Some("repo:a")),
        ];
        let edges = vec![
            edge(
                "edge:1",
                "node:a",
                "node:b",
                1.0,
                EpistemicStatus::Confirmed,
            ),
            edge(
                "edge:2",
                "node:b",
                "node:c",
                1.0,
                EpistemicStatus::Confirmed,
            ),
        ];
        let mut request = traversal_request("node:c", "node:a", TraversalAlgorithm::Bfs);
        request.options.direction = TraversalDirection::Incoming;
        request.options.max_depth = 1;
        let report = report_or_panic(traverse(&nodes, &edges, &request));

        assert!(
            report.paths.is_empty()
                && report.truncated
                && report
                    .frontier
                    .iter()
                    .any(|item| item.id == NodeId::new("node:b"))
        );
    }

    #[test]
    fn bfs_excludes_tests_and_filters_edge_kinds() {
        let nodes = vec![
            node("node:a", NodeKind::Service, None),
            node("node:test", NodeKind::TestCase, None),
            node("node:b", NodeKind::Service, None),
        ];
        let mut validating = edge(
            "edge:1",
            "node:a",
            "node:test",
            1.0,
            EpistemicStatus::Confirmed,
        );
        validating.kind = EdgeKind::Validates;
        let edges = vec![
            validating,
            edge(
                "edge:2",
                "node:test",
                "node:b",
                1.0,
                EpistemicStatus::Confirmed,
            ),
        ];
        let mut request = traversal_request("node:a", "node:b", TraversalAlgorithm::Bfs);
        request.filters.include_tests = false;
        request.filters.edge_kinds = vec![EdgeKind::Validates, EdgeKind::CallsRemote];
        let report = report_or_panic(traverse(&nodes, &edges, &request));

        assert_eq!(report.paths, Vec::new());
    }

    #[test]
    fn dijkstra_uses_confidence_weighted_shortest_path() {
        let nodes = vec![
            node("node:a", NodeKind::Service, None),
            node("node:b", NodeKind::Service, None),
            node("node:c", NodeKind::Service, None),
        ];
        let edges = vec![
            edge(
                "edge:1",
                "node:a",
                "node:c",
                0.2,
                EpistemicStatus::Confirmed,
            ),
            edge(
                "edge:2",
                "node:a",
                "node:b",
                1.0,
                EpistemicStatus::Confirmed,
            ),
            edge(
                "edge:3",
                "node:b",
                "node:c",
                1.0,
                EpistemicStatus::Confirmed,
            ),
        ];
        let request = traversal_request("node:a", "node:c", TraversalAlgorithm::Dijkstra);
        let report = report_or_panic(traverse(&nodes, &edges, &request));
        let edge_ids = report.paths[0]
            .segments
            .iter()
            .map(|segment| segment.trace.edge.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(edge_ids, vec!["edge:2", "edge:3"]);
    }

    #[test]
    fn k_shortest_is_loopless_and_deterministic() {
        let nodes = vec![
            node("node:a", NodeKind::Service, None),
            node("node:b", NodeKind::Service, None),
            node("node:c", NodeKind::Service, None),
            node("node:d", NodeKind::Service, None),
        ];
        let edges = vec![
            edge(
                "edge:1",
                "node:a",
                "node:b",
                1.0,
                EpistemicStatus::Confirmed,
            ),
            edge(
                "edge:2",
                "node:b",
                "node:d",
                1.0,
                EpistemicStatus::Confirmed,
            ),
            edge(
                "edge:3",
                "node:a",
                "node:c",
                1.0,
                EpistemicStatus::Confirmed,
            ),
            edge(
                "edge:4",
                "node:c",
                "node:d",
                1.0,
                EpistemicStatus::Confirmed,
            ),
            edge(
                "edge:5",
                "node:b",
                "node:a",
                1.0,
                EpistemicStatus::Confirmed,
            ),
        ];
        let mut request = traversal_request("node:a", "node:d", TraversalAlgorithm::KShortest);
        request.options.k = 2;
        let first = report_or_panic(traverse(&nodes, &edges, &request));
        let second = report_or_panic(traverse(&nodes, &edges, &request));

        assert_eq!(first.paths, second.paths);
    }

    #[test]
    fn unresolved_candidates_are_never_promoted_to_paths() {
        let nodes = vec![
            node("node:a", NodeKind::Service, None),
            node("node:b", NodeKind::Service, None),
        ];
        let edges = vec![edge(
            "edge:candidate",
            "node:a",
            "node:b",
            1.0,
            EpistemicStatus::Incomplete,
        )];
        let request = traversal_request("node:a", "node:b", TraversalAlgorithm::Bfs);
        let report = report_or_panic(traverse(&nodes, &edges, &request));

        assert!(report.paths.is_empty() && report.candidate_unresolved_edges.len() == 1);
    }

    #[test]
    fn traversal_reports_node_limit_and_timeout() {
        let nodes = vec![
            node("node:a", NodeKind::Service, None),
            node("node:b", NodeKind::Service, None),
            node("node:c", NodeKind::Service, None),
        ];
        let edges = vec![
            edge(
                "edge:1",
                "node:a",
                "node:b",
                1.0,
                EpistemicStatus::Confirmed,
            ),
            edge(
                "edge:2",
                "node:b",
                "node:c",
                1.0,
                EpistemicStatus::Confirmed,
            ),
        ];
        let mut limited = traversal_request("node:a", "node:c", TraversalAlgorithm::Bfs);
        limited.options.node_limit = 1;
        let limited_report = report_or_panic(traverse(&nodes, &edges, &limited));
        let mut timed = traversal_request("node:a", "node:c", TraversalAlgorithm::Bfs);
        timed.options.timeout_ms = 0;
        let timed_report = report_or_panic(traverse(&nodes, &edges, &timed));

        assert!(limited_report.truncated && timed_report.truncated);
    }
}

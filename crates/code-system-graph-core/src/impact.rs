//! Deterministic, conservative cross-repository impact and risk analysis.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use code_system_graph_model::{
    CommunityId, CommunitySnapshot, Edge, EdgeId, EdgeKind, EpistemicStatus, EvidenceId, Node, NodeId, NodeKind, RepoFreshness, RepoFreshnessState, RepoId
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const RISK_MODEL_VERSION: &str = "1.0.0";
const MAX_DEPTH: usize = 128;
const MAX_NODES: usize = 100_000;
const MAX_EDGES: usize = 1_000_000;
const MAX_LIMIT: usize = 10_000;
const MAX_OFFSET: usize = 1_000_000;

/// Conservative risk classification for an impact result.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RiskLevel {
    /// Fresh, complete coverage found only bounded low-risk factors.
    Low,
    /// Material but bounded impact was found.
    Medium,
    /// Broad, breaking, or otherwise severe impact was found.
    High,
    /// Explicit critical-domain evidence and a sufficiently high score were found.
    Critical,
    /// Coverage is insufficient for a numeric risk conclusion.
    Unknown,
}

/// Direction in which impact propagates from the resolved target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImpactDirection {
    /// Follow incoming relationships to dependents and consumers.
    Upstream,
    /// Follow outgoing relationships to dependencies and providers.
    Downstream,
    /// Follow both incoming and outgoing relationships.
    Both,
}

/// Confidence-aware classification of one affected graph entity.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ImpactClassification {
    /// A confirmed high-confidence edge directly links the entity to the target.
    DirectlyDependent,
    /// A confirmed high-confidence path transitively links the entity to the target.
    TransitivelyAffected,
    /// Candidate, inferred, ambiguous, or low-confidence evidence may link the entity.
    PossiblyAffected,
    /// Stale or incomplete evidence prevents a stronger classification.
    UnknownDueToCoverage,
}

/// Selector used to resolve exactly one graph target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case", untagged)]
pub enum ImpactTarget {
    /// Resolve an exact graph node identifier.
    NodeId {
        /// Exact persisted node identifier returned by query or source context.
        node_id: NodeId,
    },
    /// Resolve an exact versioned stable key.
    StableKey {
        /// Exact stable key returned by query or another structured result.
        stable_key: String,
    },
}

impl<'de> Deserialize<'de> for ImpactTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct NodeIdTarget {
            node_id: NodeId,
        }

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct StableKeyTarget {
            stable_key: String,
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum WireTarget {
            NodeId(NodeIdTarget),
            StableKey(StableKeyTarget),
        }

        match WireTarget::deserialize(deserializer)? {
            WireTarget::NodeId(target) => Ok(Self::NodeId {
                node_id: target.node_id,
            }),
            WireTarget::StableKey(target) => Ok(Self::StableKey {
                stable_key: target.stable_key,
            }),
        }
    }
}

/// Exact graph target selected for analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResolvedTarget {
    /// Resolved graph node.
    pub node: Node,
    /// Canonical selector form used to resolve the node.
    pub resolved_by: String,
}

/// One directed relationship in an impact path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ImpactPathStep {
    /// Node from which this traversal step began.
    pub from: NodeId,
    /// Node reached by this traversal step.
    pub to: NodeId,
    /// Existing graph relationship used by this step.
    pub edge_id: EdgeId,
    /// Relationship kind.
    pub kind: EdgeKind,
    /// Whether the underlying edge was followed from target to source.
    pub reversed: bool,
    /// Edge confidence as supplied by the graph.
    pub confidence: f32,
    /// Edge epistemic status as supplied by the graph.
    pub status: EpistemicStatus,
}

/// One deterministically selected affected graph entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ImpactItem {
    /// Affected node.
    pub node: Node,
    /// Conservative impact classification.
    pub classification: ImpactClassification,
    /// Number of graph relationships from the target.
    pub depth: usize,
    /// Deterministic shortest and strongest path selected for this node.
    pub path: Vec<ImpactPathStep>,
    /// Sorted evidence identifiers supporting the selected path.
    pub evidence: Vec<EvidenceId>,
}

/// One explainable component of the versioned risk model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RiskFactor {
    /// Stable machine-readable factor code.
    pub code: String,
    /// Non-negative score contribution before the final 100-point cap.
    pub weight: f32,
    /// Bounded human-readable explanation.
    pub explanation: String,
    /// Sorted evidence identifiers or explicit context references.
    pub evidence: Vec<String>,
}

/// Aggregate impact for one repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepositoryImpact {
    /// Affected repository.
    pub repo_id: RepoId,
    /// Strongest impact classification observed in the repository.
    pub classification: ImpactClassification,
    /// Minimum path depth among affected nodes.
    pub minimum_depth: usize,
    /// Number of affected graph nodes before pagination.
    pub affected_nodes: usize,
}

/// Aggregate impact for one service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceImpact {
    /// Service node.
    pub service: Node,
    /// Strongest impact classification observed for service members.
    pub classification: ImpactClassification,
    /// Minimum path depth among affected service members.
    pub minimum_depth: usize,
    /// Number of affected service members.
    pub affected_nodes: usize,
}

/// Impact on a modeled public or private contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractImpact {
    /// Affected contract node.
    pub contract: Node,
    /// Conservative impact classification.
    pub classification: ImpactClassification,
    /// Minimum path depth.
    pub depth: usize,
    /// Whether the contract was explicitly declared public in the analysis context.
    pub public: bool,
}

/// Aggregate impact for one detected graph community.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CommunityImpact {
    /// Community identity.
    pub community_id: CommunityId,
    /// Deterministic community label.
    pub label: String,
    /// Strongest member impact classification.
    pub classification: ImpactClassification,
    /// Number of affected community members.
    pub affected_members: usize,
    /// Accepted edge weight crossing the community boundary.
    pub coupling: f64,
    /// Explicit community limitations copied from the immutable snapshot.
    pub limitations: Vec<String>,
}

/// Availability state of optional repository-local enrichment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LocalEnrichmentStatus {
    /// The local provider completed with current inputs.
    Available,
    /// The local provider returned only part of the requested result.
    Partial,
    /// The local index does not match current repository inputs.
    Stale,
    /// The local capability or repository index is unavailable.
    Unavailable,
}

/// One repository-local symbol supplied as optional input data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LocalImpactItem {
    /// Provider-local symbol name.
    pub symbol: String,
    /// Repository-relative source path.
    pub file_path: String,
    /// One-based source start line when available.
    pub start_line: Option<usize>,
    /// Local traversal depth.
    pub depth: usize,
}

/// Optional local impact input produced before this pure analysis call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LocalEnrichmentInput {
    /// Repository to which this local result belongs.
    pub repo_id: RepoId,
    /// Exact local anchor requested from the provider.
    pub anchor: String,
    /// Provider/index availability.
    pub status: LocalEnrichmentStatus,
    /// Bounded affected local symbols.
    pub affected: Vec<LocalImpactItem>,
    /// Bounded affected test paths.
    pub affected_tests: Vec<String>,
    /// Whether provider or adapter bounds truncated this local result.
    pub truncated: bool,
    /// Explicit provider degradations and remediation.
    pub degradations: Vec<String>,
}

/// Visible repository-local enrichment included in the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LocalImpactSummary {
    /// Repository to which the local result belongs.
    pub repo_id: RepoId,
    /// Exact local anchor requested from the provider.
    pub anchor: String,
    /// Provider/index availability.
    pub status: LocalEnrichmentStatus,
    /// Number of local symbols returned.
    pub affected_count: usize,
    /// Maximum local depth observed.
    pub maximum_depth: usize,
    /// Whether the local result was truncated.
    pub truncated: bool,
    /// Explicit provider degradations and remediation.
    pub degradations: Vec<String>,
}

/// Origin of a recommended test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TestRecommendationSource {
    /// A graph `TestCase` linked by `Validates`.
    Graph,
    /// An affected-test result supplied by optional local enrichment.
    LocalEnrichment,
}

/// Ranked, non-executing recommendation for impact validation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TestRecommendation {
    /// Stable graph node when the recommendation came from a modeled test case.
    pub test_node_id: Option<NodeId>,
    /// Test label or repository-relative test path.
    pub test: String,
    /// Repository containing the test when known.
    pub repo_id: Option<RepoId>,
    /// Recommendation origin.
    pub source: TestRecommendationSource,
    /// One-based deterministic rank.
    pub rank: usize,
    /// Reasons this test is relevant.
    pub reasons: Vec<String>,
    /// Owner nodes linked to the test or validated impact target.
    pub owners: Vec<Node>,
    /// Explicit commands supplied by context; these are never executed.
    pub recommended_commands: Vec<String>,
}

/// Coverage details that control whether a numeric risk score is permitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CoverageSummary {
    /// Whether all required graph, freshness, and enrichment inputs are sufficient.
    pub sufficient_for_score: bool,
    /// Relevant repositories discovered from the target and affected nodes.
    pub relevant_repositories: Vec<RepoId>,
    /// Relevant repositories with fresh inputs.
    pub fresh_repositories: Vec<RepoId>,
    /// Relevant repositories with stale or changed inputs.
    pub stale_repositories: Vec<RepoId>,
    /// Relevant repositories with partial inputs.
    pub partial_repositories: Vec<RepoId>,
    /// Relevant repositories whose checkout or data is unavailable.
    pub unavailable_repositories: Vec<RepoId>,
    /// Relevant repositories without a freshness record.
    pub missing_repositories: Vec<RepoId>,
    /// Number of candidate or low-confidence edges selected by traversal.
    pub possible_edges: usize,
    /// Number of stale or incomplete edges selected by traversal.
    pub unknown_edges: usize,
    /// Explicit coverage limitations in deterministic order.
    pub gaps: Vec<String>,
    /// Concrete remediation guidance in deterministic order.
    pub remediation: Vec<String>,
    /// Total affected items before pagination.
    pub total_items: usize,
}

/// Configured bound that stopped complete traversal or result delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TruncationInfo {
    /// Stable machine-readable bound name.
    pub bound: String,
    /// Configured bound value.
    pub limit: usize,
    /// Number observed when the bound stopped analysis.
    pub observed: usize,
    /// Explanation of the uncertainty introduced by truncation.
    pub explanation: String,
}

/// Counts of impact classifications at one exact path depth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ImpactDepthBucket {
    /// Exact path depth represented by this bucket.
    pub depth: usize,
    /// Directly dependent item count.
    pub directly_dependent: usize,
    /// Transitively affected item count.
    pub transitively_affected: usize,
    /// Possibly affected item count.
    pub possibly_affected: usize,
    /// Coverage-unknown item count.
    pub unknown_due_to_coverage: usize,
}

/// Deterministic bounds and presentation controls for impact analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ImpactOptions {
    /// Maximum graph path depth.
    #[serde(default = "default_impact_max_depth")]
    pub max_depth: usize,
    /// Maximum number of distinct affected nodes.
    #[serde(default = "default_impact_node_limit")]
    pub node_limit: usize,
    /// Maximum number of graph edges examined.
    #[serde(default = "default_impact_edge_limit")]
    pub edge_limit: usize,
    /// Minimum confidence required for a confirmed edge.
    #[serde(default = "default_confirmed_confidence")]
    pub confirmed_confidence: f32,
    /// Zero-based offset over the stable combined impact order.
    #[serde(default)]
    pub offset: usize,
    /// Maximum number of detailed items returned.
    #[serde(default = "default_impact_limit")]
    pub limit: usize,
    /// Omit detailed impact-item lists while retaining aggregates and counts.
    #[serde(default)]
    pub summary_only: bool,
    /// Include exact-depth classification buckets.
    #[serde(default = "default_include_depth_buckets")]
    pub include_depth_buckets: bool,
}

impl Default for ImpactOptions {
    fn default() -> Self {
        Self {
            max_depth: default_impact_max_depth(),
            node_limit: default_impact_node_limit(),
            edge_limit: default_impact_edge_limit(),
            confirmed_confidence: default_confirmed_confidence(),
            offset: 0,
            limit: default_impact_limit(),
            summary_only: false,
            include_depth_buckets: default_include_depth_buckets(),
        }
    }
}

const fn default_impact_max_depth() -> usize {
    8
}

const fn default_impact_node_limit() -> usize {
    10_000
}

const fn default_impact_edge_limit() -> usize {
    50_000
}

const fn default_confirmed_confidence() -> f32 {
    0.8
}

const fn default_impact_limit() -> usize {
    100
}

const fn default_include_depth_buckets() -> bool {
    true
}

/// Request for one deterministic impact analysis.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ImpactRequest {
    /// Exact node identifier or stable key selector.
    pub target: ImpactTarget,
    /// Requested propagation direction.
    pub direction: ImpactDirection,
    /// Traversal and presentation controls.
    #[serde(default)]
    pub options: ImpactOptions,
}

impl<'de> Deserialize<'de> for ImpactRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireImpactRequest {
            #[serde(default)]
            target: Option<ImpactTarget>,
            #[serde(default)]
            node_id: Option<NodeId>,
            #[serde(default, alias = "key")]
            stable_key: Option<String>,
            #[serde(default)]
            direction: Option<ImpactDirection>,
            #[serde(default)]
            options: ImpactOptions,
        }

        let wire = WireImpactRequest::deserialize(deserializer)?;
        let selector_count = usize::from(wire.target.is_some())
            + usize::from(wire.node_id.is_some())
            + usize::from(wire.stable_key.is_some());
        if selector_count != 1 {
            return Err(serde::de::Error::custom(
                "exactly one of target, node_id, stable_key, or key is required",
            ));
        }
        let target = wire
            .target
            .or_else(|| wire.node_id.map(|node_id| ImpactTarget::NodeId { node_id }))
            .or_else(|| {
                wire.stable_key
                    .map(|stable_key| ImpactTarget::StableKey { stable_key })
            })
            .ok_or_else(|| {
                serde::de::Error::missing_field("target, node_id, stable_key, or key")
            })?;
        Ok(Self {
            target,
            direction: wire.direction.unwrap_or(ImpactDirection::Both),
            options: wire.options,
        })
    }
}

/// Compatibility classification supplied to the impact engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImpactCompatibilityStatus {
    /// A supported compatibility rule proves a breaking change.
    Breaking,
    /// The change can be breaking but requires runtime or policy confirmation.
    PotentiallyBreaking,
    /// Modeled rules prove compatibility under complete inputs.
    Compatible,
    /// Compatibility coverage is insufficient.
    Unknown,
}

/// Compatibility result associated with a graph contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CompatibilityInput {
    /// Contract node to which this result applies.
    pub contract_node_id: NodeId,
    /// Conservative compatibility classification.
    pub status: ImpactCompatibilityStatus,
    /// Stable rule codes or bounded evidence references.
    pub evidence: Vec<String>,
    /// Recommended compatibility validations.
    pub recommended_validations: Vec<String>,
}

/// Explicit critical-domain tag; labels are never interpreted as tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CriticalityTag {
    /// Explicitly designated critical system or process.
    Critical,
    /// Authentication or authorization boundary.
    Authentication,
    /// Security-sensitive boundary.
    Security,
    /// Payment or financial boundary.
    Payment,
    /// Data governance, privacy, or storage boundary.
    DataBoundary,
}

/// Explicit, evidence-backed critical-domain assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CriticalityAssignment {
    /// Tagged graph node.
    pub node_id: NodeId,
    /// Explicit critical-domain tag.
    pub tag: CriticalityTag,
    /// Non-empty evidence or policy reference supplied by the caller.
    pub evidence: Vec<String>,
}

/// Explicit deployment environment associated with a graph node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EnvironmentAssignment {
    /// Deployment, service, contract, or repository node.
    pub node_id: NodeId,
    /// Canonical environment name supplied by configuration.
    pub environment: String,
}

/// Explicit non-executing validation command supplied by configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RecommendedCommand {
    /// Repository in which the command is valid.
    pub repo_id: RepoId,
    /// Command displayed to the caller but never executed by this module.
    pub command: String,
    /// Bounded explanation of the command's purpose.
    pub description: String,
}

/// Complete immutable input context for pure impact analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ImpactContext {
    /// Federated graph nodes.
    pub nodes: Vec<Node>,
    /// Federated graph relationships.
    pub edges: Vec<Edge>,
    /// Optional immutable community analysis for this graph snapshot.
    pub communities: Option<CommunitySnapshot>,
    /// Repository freshness records.
    pub freshness: Vec<RepoFreshness>,
    /// Compatibility results computed before this call.
    pub compatibility: Vec<CompatibilityInput>,
    /// Optional repository-local impact and affected-test inputs.
    pub local_enrichment: Vec<LocalEnrichmentInput>,
    /// Nodes explicitly declared to be public contracts.
    pub public_contracts: Vec<NodeId>,
    /// Explicit critical-domain assignments.
    pub criticality: Vec<CriticalityAssignment>,
    /// Normalized centrality scores keyed by graph node.
    pub centrality: BTreeMap<NodeId, f32>,
    /// Service memberships keyed by member node.
    pub service_memberships: BTreeMap<NodeId, Vec<NodeId>>,
    /// Explicit deployment environment assignments.
    pub environments: Vec<EnvironmentAssignment>,
    /// Explicit non-executing validation commands.
    pub recommended_commands: Vec<RecommendedCommand>,
    /// Whether graph extraction and linking completed for the requested workspace.
    pub graph_complete: bool,
    /// Additional known coverage gaps.
    pub coverage_gaps: Vec<String>,
}

/// Complete deterministic impact and risk result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ImpactReport {
    /// Version of the deterministic risk model.
    pub risk_model_version: String,
    /// Exact target selected for analysis.
    pub target: ResolvedTarget,
    /// Applied propagation direction.
    pub direction: ImpactDirection,
    /// Conservative aggregate risk.
    pub risk: RiskLevel,
    /// Positive score in `0..=100`, omitted whenever risk is unknown.
    pub risk_score: Option<f32>,
    /// Deterministically ordered risk factors.
    pub reasons: Vec<RiskFactor>,
    /// Confirmed depth-one dependents in the selected page.
    pub direct_consumers: Vec<ImpactItem>,
    /// Confirmed depth-two-or-greater impacts in the selected page.
    pub transitive_consumers: Vec<ImpactItem>,
    /// Candidate, inferred, ambiguous, or low-confidence impacts in the selected page.
    pub possibly_affected: Vec<ImpactItem>,
    /// Impacts whose selected path contains stale or incomplete evidence.
    pub unknown_due_to_coverage: Vec<ImpactItem>,
    /// Repository aggregates computed before pagination.
    pub affected_repositories: Vec<RepositoryImpact>,
    /// Service aggregates computed before pagination.
    pub affected_services: Vec<ServiceImpact>,
    /// Contract impacts computed before pagination.
    pub affected_contracts: Vec<ContractImpact>,
    /// Community aggregates computed before pagination.
    pub affected_communities: Vec<CommunityImpact>,
    /// Optional local-enrichment summaries.
    pub local_impact_summaries: Vec<LocalImpactSummary>,
    /// Ranked test recommendations.
    pub test_recommendations: Vec<TestRecommendation>,
    /// Optional exact-depth counts computed before pagination.
    pub depth_buckets: Vec<ImpactDepthBucket>,
    /// Coverage and remediation details.
    pub coverage: CoverageSummary,
    /// First configured bound that introduced uncertainty.
    pub truncation: Option<TruncationInfo>,
}

/// Validation error returned before impact analysis begins.
#[derive(Debug, Error, PartialEq)]
pub enum ImpactError {
    /// More than one graph node has the same identifier.
    #[error("duplicate node identifier `{0}`")]
    DuplicateNode(String),
    /// More than one graph relationship has the same identifier.
    #[error("duplicate edge identifier `{0}`")]
    DuplicateEdge(String),
    /// More than one freshness record exists for the same repository.
    #[error("duplicate freshness record for repository `{0}`")]
    DuplicateFreshness(String),
    /// A graph relationship references a missing endpoint.
    #[error("edge `{edge}` references missing node `{node}`")]
    DanglingEdge {
        /// Relationship identifier.
        edge: String,
        /// Missing endpoint identifier.
        node: String,
    },
    /// An input edge has non-finite or out-of-range confidence.
    #[error("edge `{0}` confidence must be finite and in the inclusive range 0..=1")]
    InvalidEdgeConfidence(String),
    /// The configured confirmed-confidence threshold is invalid.
    #[error("confirmed confidence must be finite and in the inclusive range 0..=1")]
    InvalidConfirmedConfidence,
    /// One or more traversal or pagination bounds are invalid.
    #[error("impact traversal or pagination bounds are outside supported limits")]
    InvalidBounds,
    /// A supplied centrality score is non-finite or out of range.
    #[error("centrality for node `{0}` must be finite and in the inclusive range 0..=1")]
    InvalidCentrality(String),
    /// An explicit criticality assignment has no evidence.
    #[error("criticality assignment for node `{0}` requires explicit evidence")]
    CriticalityWithoutEvidence(String),
    /// An input context references a node absent from the graph.
    #[error("{context} references missing node `{node}`")]
    UnknownContextNode {
        /// Context collection containing the reference.
        context: &'static str,
        /// Missing node identifier.
        node: String,
    },
    /// No graph node matches the requested target.
    #[error("impact target was not found")]
    UnknownTarget,
    /// A stable-key selector resolves to more than one node.
    #[error("stable key `{0}` resolves to more than one node")]
    AmbiguousTarget(String),
}

#[derive(Debug, Clone)]
struct TraversalState {
    node_id: NodeId,
    certainty: PathCertainty,
    path: Vec<ImpactPathStep>,
    evidence: BTreeSet<EvidenceId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PathCertainty {
    Confirmed,
    Possible,
    Unknown,
}

#[derive(Debug, Clone)]
struct Adjacency<'a> {
    neighbor: NodeId,
    edge: &'a Edge,
    reversed: bool,
}

#[derive(Debug, Clone, Copy)]
struct Aggregate {
    classification: ImpactClassification,
    minimum_depth: usize,
    affected_nodes: usize,
}

/// Performs pure deterministic impact propagation, aggregation, and conservative risk analysis.
///
/// The function never invokes providers or executes commands. Optional compatibility, local
/// intelligence, environment, criticality, and command data must already be present in `context`.
///
/// # Errors
///
/// Returns [`ImpactError`] when graph identities, references, confidence values, centrality
/// scores, criticality evidence, target resolution, or configured bounds are invalid.
#[must_use = "impact reports and validation errors must be handled"]
pub fn analyze_impact(
    request: &ImpactRequest,
    context: &ImpactContext,
) -> Result<ImpactReport, ImpactError> {
    let nodes = validate_context(request, context)?;
    let target = resolve_target(&request.target, &nodes)?;
    let (mut items, truncation, possible_edges, unknown_edges) =
        traverse(request, context, &nodes, &target.node)?;
    sort_items(&mut items);

    let repositories = aggregate_repositories(&items);
    let services = aggregate_services(&items, context, &nodes);
    let contracts = aggregate_contracts(&items, context);
    let communities = aggregate_communities(&items, context);
    let local_summaries = summarize_local_enrichment(context);
    let tests = recommend_tests(&items, context, &nodes);
    let depth_buckets = if request.options.include_depth_buckets {
        build_depth_buckets(&items)
    } else {
        Vec::new()
    };
    let coverage = coverage_summary(
        &target.node,
        &items,
        context,
        truncation.as_ref(),
        possible_edges,
        unknown_edges,
    );
    let (risk, risk_score, reasons) = assess_risk(
        &target.node,
        &items,
        &repositories,
        &services,
        &contracts,
        &communities,
        &tests,
        context,
        &coverage,
        truncation.as_ref(),
    );

    let page = if request.options.summary_only {
        Vec::new()
    } else {
        items
            .iter()
            .skip(request.options.offset.min(items.len()))
            .take(request.options.limit)
            .cloned()
            .collect()
    };
    let (direct, transitive, possible, unknown) = split_classifications(page);

    Ok(ImpactReport {
        risk_model_version: RISK_MODEL_VERSION.to_owned(),
        target,
        direction: request.direction,
        risk,
        risk_score,
        reasons,
        direct_consumers: direct,
        transitive_consumers: transitive,
        possibly_affected: possible,
        unknown_due_to_coverage: unknown,
        affected_repositories: repositories,
        affected_services: services,
        affected_contracts: contracts,
        affected_communities: communities,
        local_impact_summaries: local_summaries,
        test_recommendations: tests,
        depth_buckets,
        coverage,
        truncation,
    })
}

fn validate_context<'a>(
    request: &ImpactRequest,
    context: &'a ImpactContext,
) -> Result<BTreeMap<NodeId, &'a Node>, ImpactError> {
    let options = &request.options;
    if options.max_depth == 0
        || options.max_depth > MAX_DEPTH
        || options.node_limit == 0
        || options.node_limit > MAX_NODES
        || options.edge_limit == 0
        || options.edge_limit > MAX_EDGES
        || options.limit == 0
        || options.limit > MAX_LIMIT
        || options.offset > MAX_OFFSET
    {
        return Err(ImpactError::InvalidBounds);
    }
    if !options.confirmed_confidence.is_finite()
        || !(0.0..=1.0).contains(&options.confirmed_confidence)
    {
        return Err(ImpactError::InvalidConfirmedConfidence);
    }

    let mut nodes = BTreeMap::new();
    for node in &context.nodes {
        if nodes.insert(node.id.clone(), node).is_some() {
            return Err(ImpactError::DuplicateNode(node.id.as_str().to_owned()));
        }
    }
    let mut edge_ids = BTreeSet::new();
    for edge in &context.edges {
        if !edge_ids.insert(edge.id.clone()) {
            return Err(ImpactError::DuplicateEdge(edge.id.as_str().to_owned()));
        }
        if !edge.confidence.is_finite() || !(0.0..=1.0).contains(&edge.confidence) {
            return Err(ImpactError::InvalidEdgeConfidence(
                edge.id.as_str().to_owned(),
            ));
        }
        for endpoint in [&edge.source, &edge.target] {
            if !nodes.contains_key(endpoint) {
                return Err(ImpactError::DanglingEdge {
                    edge: edge.id.as_str().to_owned(),
                    node: endpoint.as_str().to_owned(),
                });
            }
        }
    }
    let mut freshness_repositories = BTreeSet::new();
    for record in &context.freshness {
        if !freshness_repositories.insert(&record.repo_id) {
            return Err(ImpactError::DuplicateFreshness(
                record.repo_id.as_str().to_owned(),
            ));
        }
    }
    for (node_id, centrality) in &context.centrality {
        validate_node_reference(&nodes, node_id, "centrality")?;
        if !centrality.is_finite() || !(0.0..=1.0).contains(centrality) {
            return Err(ImpactError::InvalidCentrality(node_id.as_str().to_owned()));
        }
    }
    for assignment in &context.criticality {
        validate_node_reference(&nodes, &assignment.node_id, "criticality")?;
        if assignment.evidence.is_empty()
            || assignment
                .evidence
                .iter()
                .any(|value| value.trim().is_empty())
        {
            return Err(ImpactError::CriticalityWithoutEvidence(
                assignment.node_id.as_str().to_owned(),
            ));
        }
    }
    for compatibility in &context.compatibility {
        validate_node_reference(&nodes, &compatibility.contract_node_id, "compatibility")?;
    }
    for node_id in &context.public_contracts {
        validate_node_reference(&nodes, node_id, "public_contracts")?;
    }
    for assignment in &context.environments {
        validate_node_reference(&nodes, &assignment.node_id, "environments")?;
    }
    for (member, services) in &context.service_memberships {
        validate_node_reference(&nodes, member, "service_memberships")?;
        for service in services {
            validate_node_reference(&nodes, service, "service_memberships")?;
        }
    }
    Ok(nodes)
}

fn validate_node_reference(
    nodes: &BTreeMap<NodeId, &Node>,
    node_id: &NodeId,
    context: &'static str,
) -> Result<(), ImpactError> {
    if nodes.contains_key(node_id) {
        Ok(())
    } else {
        Err(ImpactError::UnknownContextNode {
            context,
            node: node_id.as_str().to_owned(),
        })
    }
}

fn resolve_target(
    selector: &ImpactTarget,
    nodes: &BTreeMap<NodeId, &Node>,
) -> Result<ResolvedTarget, ImpactError> {
    match selector {
        ImpactTarget::NodeId { node_id } => nodes
            .get(node_id)
            .map(|node| ResolvedTarget {
                node: (*node).clone(),
                resolved_by: "node_id".to_owned(),
            })
            .ok_or(ImpactError::UnknownTarget),
        ImpactTarget::StableKey { stable_key } => {
            let mut matches = nodes.values().filter(|node| node.stable_key == *stable_key);
            let Some(node) = matches.next() else {
                return Err(ImpactError::UnknownTarget);
            };
            if matches.next().is_some() {
                return Err(ImpactError::AmbiguousTarget(stable_key.clone()));
            }
            Ok(ResolvedTarget {
                node: (*node).clone(),
                resolved_by: "stable_key".to_owned(),
            })
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "The bounded BFS state transitions remain together for auditability"
)]
fn traverse(
    request: &ImpactRequest,
    context: &ImpactContext,
    nodes: &BTreeMap<NodeId, &Node>,
    target: &Node,
) -> Result<(Vec<ImpactItem>, Option<TruncationInfo>, usize, usize), ImpactError> {
    let adjacency = build_adjacency(&context.edges, request.direction);
    let mut queue = VecDeque::from([TraversalState {
        node_id: target.id.clone(),
        certainty: PathCertainty::Confirmed,
        path: Vec::new(),
        evidence: BTreeSet::new(),
    }]);
    let mut best = BTreeMap::<NodeId, (PathCertainty, usize)>::new();
    best.insert(target.id.clone(), (PathCertainty::Confirmed, 0));
    let mut items = BTreeMap::<NodeId, ImpactItem>::new();
    let mut examined_edges = 0_usize;
    let mut possible_edges = 0_usize;
    let mut unknown_edges = 0_usize;
    let mut truncation = None;

    while let Some(state) = queue.pop_front() {
        let depth = state.path.len();
        let neighbors = adjacency.get(&state.node_id).map_or(&[][..], Vec::as_slice);
        if depth == request.options.max_depth {
            if neighbors.iter().any(|entry| {
                is_propagating(entry.edge.kind)
                    && !is_structural_direction_pivot(&state.path, entry)
                    && !state.path.iter().any(|step| step.from == entry.neighbor)
            }) {
                truncation.get_or_insert_with(|| TruncationInfo {
                    bound: "max_depth".to_owned(),
                    limit: request.options.max_depth,
                    observed: depth,
                    explanation: "additional graph relationships exist beyond maximum depth"
                        .to_owned(),
                });
            }
            continue;
        }
        for entry in neighbors {
            if !is_propagating(entry.edge.kind) || is_structural_direction_pivot(&state.path, entry)
            {
                continue;
            }
            examined_edges = examined_edges.saturating_add(1);
            if examined_edges > request.options.edge_limit {
                truncation.get_or_insert_with(|| TruncationInfo {
                    bound: "edge_limit".to_owned(),
                    limit: request.options.edge_limit,
                    observed: examined_edges,
                    explanation: "edge examination limit stopped impact propagation".to_owned(),
                });
                break;
            }
            if entry.neighbor == target.id
                || state
                    .path
                    .iter()
                    .any(|step| step.from == entry.neighbor || step.to == entry.neighbor)
            {
                continue;
            }
            let edge_certainty = edge_certainty(entry.edge, request.options.confirmed_confidence);
            match edge_certainty {
                PathCertainty::Confirmed => {}
                PathCertainty::Possible => possible_edges = possible_edges.saturating_add(1),
                PathCertainty::Unknown => unknown_edges = unknown_edges.saturating_add(1),
            }
            let certainty = state.certainty.max(edge_certainty);
            let next_depth = depth.saturating_add(1);
            if best
                .get(&entry.neighbor)
                .is_some_and(|existing| *existing <= (certainty, next_depth))
            {
                continue;
            }
            if !items.contains_key(&entry.neighbor) && items.len() >= request.options.node_limit {
                truncation.get_or_insert_with(|| TruncationInfo {
                    bound: "node_limit".to_owned(),
                    limit: request.options.node_limit,
                    observed: items.len().saturating_add(1),
                    explanation: "distinct-node limit stopped impact propagation".to_owned(),
                });
                break;
            }
            let mut path = state.path.clone();
            path.push(ImpactPathStep {
                from: state.node_id.clone(),
                to: entry.neighbor.clone(),
                edge_id: entry.edge.id.clone(),
                kind: entry.edge.kind,
                reversed: entry.reversed,
                confidence: entry.edge.confidence,
                status: entry.edge.status,
            });
            let mut evidence = state.evidence.clone();
            evidence.extend(entry.edge.evidence.iter().cloned());
            let classification = classify(certainty, next_depth);
            let Some(node) = nodes.get(&entry.neighbor) else {
                return Err(ImpactError::DanglingEdge {
                    edge: entry.edge.id.as_str().to_owned(),
                    node: entry.neighbor.as_str().to_owned(),
                });
            };
            best.insert(entry.neighbor.clone(), (certainty, next_depth));
            items.insert(
                entry.neighbor.clone(),
                ImpactItem {
                    node: (*node).clone(),
                    classification,
                    depth: next_depth,
                    path: path.clone(),
                    evidence: evidence.iter().cloned().collect(),
                },
            );
            queue.push_back(TraversalState {
                node_id: entry.neighbor.clone(),
                certainty,
                path,
                evidence,
            });
        }
        if truncation
            .as_ref()
            .is_some_and(|value| value.bound == "edge_limit" || value.bound == "node_limit")
        {
            break;
        }
    }
    Ok((
        items.into_values().collect(),
        truncation,
        possible_edges,
        unknown_edges,
    ))
}

fn is_structural_direction_pivot(path: &[ImpactPathStep], next: &Adjacency<'_>) -> bool {
    path.last().is_some_and(|previous| {
        previous.kind == EdgeKind::Contains
            && next.edge.kind == EdgeKind::Contains
            && previous.reversed != next.reversed
    })
}

fn build_adjacency(
    edges: &[Edge],
    direction: ImpactDirection,
) -> BTreeMap<NodeId, Vec<Adjacency<'_>>> {
    let mut adjacency = BTreeMap::<NodeId, Vec<Adjacency<'_>>>::new();
    for edge in edges {
        if matches!(
            direction,
            ImpactDirection::Downstream | ImpactDirection::Both
        ) {
            adjacency
                .entry(edge.source.clone())
                .or_default()
                .push(Adjacency {
                    neighbor: edge.target.clone(),
                    edge,
                    reversed: false,
                });
        }
        if matches!(direction, ImpactDirection::Upstream | ImpactDirection::Both) {
            adjacency
                .entry(edge.target.clone())
                .or_default()
                .push(Adjacency {
                    neighbor: edge.source.clone(),
                    edge,
                    reversed: true,
                });
        }
    }
    for entries in adjacency.values_mut() {
        entries.sort_by(|left, right| {
            left.neighbor
                .cmp(&right.neighbor)
                .then_with(|| left.edge.id.cmp(&right.edge.id))
                .then_with(|| left.reversed.cmp(&right.reversed))
        });
    }
    adjacency
}

fn is_propagating(kind: EdgeKind) -> bool {
    !matches!(
        kind,
        EdgeKind::Validates
            | EdgeKind::OwnedBy
            | EdgeKind::Documents
            | EdgeKind::MemberOf
            | EdgeKind::Precedes
            | EdgeKind::Reverts
            | EdgeKind::CompatibleWith
    )
}

fn edge_certainty(edge: &Edge, confirmed_confidence: f32) -> PathCertainty {
    match edge.status {
        EpistemicStatus::Confirmed if edge.confidence >= confirmed_confidence => {
            PathCertainty::Confirmed
        }
        EpistemicStatus::Confirmed | EpistemicStatus::Inferred | EpistemicStatus::Ambiguous => {
            PathCertainty::Possible
        }
        EpistemicStatus::Stale | EpistemicStatus::Incomplete => PathCertainty::Unknown,
    }
}

fn classify(certainty: PathCertainty, depth: usize) -> ImpactClassification {
    match (certainty, depth) {
        (PathCertainty::Confirmed, 1) => ImpactClassification::DirectlyDependent,
        (PathCertainty::Confirmed, _) => ImpactClassification::TransitivelyAffected,
        (PathCertainty::Possible, _) => ImpactClassification::PossiblyAffected,
        (PathCertainty::Unknown, _) => ImpactClassification::UnknownDueToCoverage,
    }
}

fn classification_rank(classification: ImpactClassification) -> u8 {
    match classification {
        ImpactClassification::DirectlyDependent => 0,
        ImpactClassification::TransitivelyAffected => 1,
        ImpactClassification::PossiblyAffected => 2,
        ImpactClassification::UnknownDueToCoverage => 3,
    }
}

fn stronger(left: ImpactClassification, right: ImpactClassification) -> ImpactClassification {
    if classification_rank(left) <= classification_rank(right) {
        left
    } else {
        right
    }
}

fn sort_items(items: &mut [ImpactItem]) {
    items.sort_by(|left, right| {
        classification_rank(left.classification)
            .cmp(&classification_rank(right.classification))
            .then_with(|| left.depth.cmp(&right.depth))
            .then_with(|| left.node.repo_id.cmp(&right.node.repo_id))
            .then_with(|| left.node.stable_key.cmp(&right.node.stable_key))
            .then_with(|| left.node.id.cmp(&right.node.id))
            .then_with(|| path_key(&left.path).cmp(&path_key(&right.path)))
    });
}

fn path_key(path: &[ImpactPathStep]) -> Vec<(&str, bool)> {
    path.iter()
        .map(|step| (step.edge_id.as_str(), step.reversed))
        .collect()
}

fn aggregate_repositories(items: &[ImpactItem]) -> Vec<RepositoryImpact> {
    let mut aggregates = BTreeMap::<RepoId, Aggregate>::new();
    for item in items {
        let Some(repo_id) = &item.node.repo_id else {
            continue;
        };
        update_aggregate(&mut aggregates, repo_id.clone(), item);
    }
    aggregates
        .into_iter()
        .map(|(repo_id, aggregate)| RepositoryImpact {
            repo_id,
            classification: aggregate.classification,
            minimum_depth: aggregate.minimum_depth,
            affected_nodes: aggregate.affected_nodes,
        })
        .collect()
}

fn aggregate_services(
    items: &[ImpactItem],
    context: &ImpactContext,
    nodes: &BTreeMap<NodeId, &Node>,
) -> Vec<ServiceImpact> {
    let mut aggregates = BTreeMap::<NodeId, Aggregate>::new();
    for item in items {
        if item.node.kind == NodeKind::Service {
            update_aggregate(&mut aggregates, item.node.id.clone(), item);
        }
        if let Some(service_ids) = context.service_memberships.get(&item.node.id) {
            for service_id in service_ids {
                update_aggregate(&mut aggregates, service_id.clone(), item);
            }
        }
    }
    aggregates
        .into_iter()
        .filter_map(|(service_id, aggregate)| {
            nodes.get(&service_id).map(|service| ServiceImpact {
                service: (*service).clone(),
                classification: aggregate.classification,
                minimum_depth: aggregate.minimum_depth,
                affected_nodes: aggregate.affected_nodes,
            })
        })
        .collect()
}

fn update_aggregate<K: Ord>(aggregates: &mut BTreeMap<K, Aggregate>, key: K, item: &ImpactItem) {
    aggregates
        .entry(key)
        .and_modify(|aggregate| {
            aggregate.classification = stronger(aggregate.classification, item.classification);
            aggregate.minimum_depth = aggregate.minimum_depth.min(item.depth);
            aggregate.affected_nodes = aggregate.affected_nodes.saturating_add(1);
        })
        .or_insert(Aggregate {
            classification: item.classification,
            minimum_depth: item.depth,
            affected_nodes: 1,
        });
}

fn aggregate_contracts(items: &[ImpactItem], context: &ImpactContext) -> Vec<ContractImpact> {
    let public = context.public_contracts.iter().collect::<BTreeSet<_>>();
    items
        .iter()
        .filter(|item| is_contract(item.node.kind))
        .map(|item| ContractImpact {
            contract: item.node.clone(),
            classification: item.classification,
            depth: item.depth,
            public: public.contains(&item.node.id),
        })
        .collect()
}

fn is_contract(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::HttpOperation
            | NodeKind::GraphqlOperation
            | NodeKind::RpcMethod
            | NodeKind::EventChannel
            | NodeKind::EventSchema
            | NodeKind::DatabaseTable
            | NodeKind::DatabaseColumn
            | NodeKind::ConfigKey
    )
}

fn aggregate_communities(items: &[ImpactItem], context: &ImpactContext) -> Vec<CommunityImpact> {
    let Some(snapshot) = &context.communities else {
        return Vec::new();
    };
    let impacted = items
        .iter()
        .map(|item| (&item.node.id, item))
        .collect::<BTreeMap<_, _>>();
    let mut result = Vec::new();
    for community in &snapshot.communities {
        let mut aggregate = None::<Aggregate>;
        for member in &community.members {
            if let Some(item) = impacted.get(member) {
                let current = aggregate.get_or_insert(Aggregate {
                    classification: item.classification,
                    minimum_depth: item.depth,
                    affected_nodes: 0,
                });
                current.classification = stronger(current.classification, item.classification);
                current.minimum_depth = current.minimum_depth.min(item.depth);
                current.affected_nodes = current.affected_nodes.saturating_add(1);
            }
        }
        if let Some(aggregate) = aggregate {
            result.push(CommunityImpact {
                community_id: community.id.clone(),
                label: community.label.clone(),
                classification: aggregate.classification,
                affected_members: aggregate.affected_nodes,
                coupling: community.metrics.coupling,
                limitations: community.limitations.clone(),
            });
        }
    }
    result.sort_by(|left, right| left.community_id.cmp(&right.community_id));
    result
}

fn summarize_local_enrichment(context: &ImpactContext) -> Vec<LocalImpactSummary> {
    let mut summaries = context
        .local_enrichment
        .iter()
        .map(|input| LocalImpactSummary {
            repo_id: input.repo_id.clone(),
            anchor: input.anchor.clone(),
            status: input.status,
            affected_count: input.affected.len(),
            maximum_depth: input
                .affected
                .iter()
                .map(|item| item.depth)
                .max()
                .unwrap_or(0),
            truncated: input.truncated,
            degradations: sorted_unique(input.degradations.clone()),
        })
        .collect::<Vec<_>>();
    summaries.sort_by(|left, right| {
        left.repo_id
            .cmp(&right.repo_id)
            .then_with(|| left.anchor.cmp(&right.anchor))
    });
    summaries
}

fn recommend_tests(
    items: &[ImpactItem],
    context: &ImpactContext,
    nodes: &BTreeMap<NodeId, &Node>,
) -> Vec<TestRecommendation> {
    let impacted = items
        .iter()
        .map(|item| (&item.node.id, item))
        .collect::<BTreeMap<_, _>>();
    let mut recommendations = Vec::new();
    for edge in &context.edges {
        if edge.kind != EdgeKind::Validates {
            continue;
        }
        let (test_id, validated_id) = if nodes
            .get(&edge.source)
            .is_some_and(|node| node.kind == NodeKind::TestCase)
        {
            (&edge.source, &edge.target)
        } else if nodes
            .get(&edge.target)
            .is_some_and(|node| node.kind == NodeKind::TestCase)
        {
            (&edge.target, &edge.source)
        } else {
            continue;
        };
        let Some(item) = impacted.get(validated_id) else {
            continue;
        };
        let Some(test) = nodes.get(test_id) else {
            continue;
        };
        recommendations.push(TestRecommendation {
            test_node_id: Some(test.id.clone()),
            test: test.label.clone(),
            repo_id: test.repo_id.clone(),
            source: TestRecommendationSource::Graph,
            rank: 0,
            reasons: vec![format!(
                "validates impacted node `{}` at depth {}",
                item.node.stable_key, item.depth
            )],
            owners: owners_for(&test.id, validated_id, context, nodes),
            recommended_commands: commands_for(test.repo_id.as_ref(), context),
        });
    }
    for local in &context.local_enrichment {
        for test in &local.affected_tests {
            recommendations.push(TestRecommendation {
                test_node_id: None,
                test: test.clone(),
                repo_id: Some(local.repo_id.clone()),
                source: TestRecommendationSource::LocalEnrichment,
                rank: 0,
                reasons: vec![format!(
                    "optional local enrichment for anchor `{}` reported this test",
                    local.anchor
                )],
                owners: Vec::new(),
                recommended_commands: commands_for(Some(&local.repo_id), context),
            });
        }
    }
    recommendations.sort_by(|left, right| {
        test_source_rank(left.source)
            .cmp(&test_source_rank(right.source))
            .then_with(|| left.repo_id.cmp(&right.repo_id))
            .then_with(|| left.test.cmp(&right.test))
            .then_with(|| left.test_node_id.cmp(&right.test_node_id))
    });
    recommendations.dedup_by(|left, right| {
        left.test_node_id == right.test_node_id
            && left.repo_id == right.repo_id
            && left.test == right.test
    });
    for (index, recommendation) in recommendations.iter_mut().enumerate() {
        recommendation.rank = index.saturating_add(1);
    }
    recommendations
}

fn test_source_rank(source: TestRecommendationSource) -> u8 {
    match source {
        TestRecommendationSource::Graph => 0,
        TestRecommendationSource::LocalEnrichment => 1,
    }
}

fn owners_for(
    test_id: &NodeId,
    validated_id: &NodeId,
    context: &ImpactContext,
    nodes: &BTreeMap<NodeId, &Node>,
) -> Vec<Node> {
    let mut owner_ids = BTreeSet::new();
    for edge in &context.edges {
        if edge.kind == EdgeKind::OwnedBy
            && (&edge.source == test_id || &edge.source == validated_id)
            && nodes
                .get(&edge.target)
                .is_some_and(|node| node.kind == NodeKind::Owner)
        {
            owner_ids.insert(edge.target.clone());
        }
    }
    owner_ids
        .iter()
        .filter_map(|owner_id| nodes.get(owner_id).map(|node| (*node).clone()))
        .collect()
}

fn commands_for(repo_id: Option<&RepoId>, context: &ImpactContext) -> Vec<String> {
    let mut commands = context
        .recommended_commands
        .iter()
        .filter(|command| repo_id.is_some_and(|repo| repo == &command.repo_id))
        .map(|command| command.command.clone())
        .collect::<Vec<_>>();
    commands.sort();
    commands.dedup();
    commands
}

fn build_depth_buckets(items: &[ImpactItem]) -> Vec<ImpactDepthBucket> {
    let mut buckets = BTreeMap::<usize, ImpactDepthBucket>::new();
    for item in items {
        let bucket = buckets.entry(item.depth).or_insert(ImpactDepthBucket {
            depth: item.depth,
            directly_dependent: 0,
            transitively_affected: 0,
            possibly_affected: 0,
            unknown_due_to_coverage: 0,
        });
        match item.classification {
            ImpactClassification::DirectlyDependent => {
                bucket.directly_dependent = bucket.directly_dependent.saturating_add(1);
            }
            ImpactClassification::TransitivelyAffected => {
                bucket.transitively_affected = bucket.transitively_affected.saturating_add(1);
            }
            ImpactClassification::PossiblyAffected => {
                bucket.possibly_affected = bucket.possibly_affected.saturating_add(1);
            }
            ImpactClassification::UnknownDueToCoverage => {
                bucket.unknown_due_to_coverage = bucket.unknown_due_to_coverage.saturating_add(1);
            }
        }
    }
    buckets.into_values().collect()
}

#[expect(
    clippy::too_many_lines,
    reason = "Coverage states and their paired remediations remain visibly exhaustive"
)]
fn coverage_summary(
    target: &Node,
    items: &[ImpactItem],
    context: &ImpactContext,
    truncation: Option<&TruncationInfo>,
    possible_edges: usize,
    unknown_edges: usize,
) -> CoverageSummary {
    let mut relevant = items
        .iter()
        .filter_map(|item| item.node.repo_id.clone())
        .collect::<BTreeSet<_>>();
    relevant.extend(target.repo_id.iter().cloned());
    let freshness = context
        .freshness
        .iter()
        .map(|record| (&record.repo_id, record.state))
        .collect::<BTreeMap<_, _>>();
    let mut fresh = Vec::new();
    let mut stale = Vec::new();
    let mut partial = Vec::new();
    let mut unavailable = Vec::new();
    let mut missing = Vec::new();
    for repo_id in &relevant {
        match freshness.get(repo_id) {
            Some(RepoFreshnessState::Fresh) => fresh.push(repo_id.clone()),
            Some(
                RepoFreshnessState::WorkingTreeChanged
                | RepoFreshnessState::CommitsBehind
                | RepoFreshnessState::ConfigChanged
                | RepoFreshnessState::ExtractorChanged
                | RepoFreshnessState::CodegraphPending,
            ) => stale.push(repo_id.clone()),
            Some(RepoFreshnessState::Partial) => partial.push(repo_id.clone()),
            Some(RepoFreshnessState::Unavailable | RepoFreshnessState::Corrupt) => {
                unavailable.push(repo_id.clone());
            }
            Some(RepoFreshnessState::Unknown) | None => missing.push(repo_id.clone()),
        }
    }
    let mut gaps = context.coverage_gaps.clone();
    let mut remediation = Vec::new();
    if !context.coverage_gaps.is_empty() {
        remediation.push("resolve each caller-supplied coverage gap and rerun analysis".to_owned());
    }
    if !context.graph_complete {
        gaps.push("federated graph extraction or linking is incomplete".to_owned());
        remediation.push("complete a fresh workspace scan and relink the graph".to_owned());
    }
    if !stale.is_empty() {
        gaps.push("one or more relevant repositories are stale".to_owned());
        remediation.push("rescan stale repositories at their current revisions".to_owned());
    }
    if !partial.is_empty() {
        gaps.push("one or more relevant repositories have partial coverage".to_owned());
        remediation.push("resolve extractor limitations and complete partial scans".to_owned());
    }
    if !unavailable.is_empty() {
        gaps.push("one or more relevant repositories are unavailable or corrupt".to_owned());
        remediation.push("restore unavailable repositories or valid graph snapshots".to_owned());
    }
    if !missing.is_empty() {
        gaps.push("freshness is missing or unknown for relevant repositories".to_owned());
        remediation.push("record current freshness for every relevant repository".to_owned());
    }
    if possible_edges > 0 {
        gaps.push(
            "candidate, inferred, ambiguous, or low-confidence relationships were used".to_owned(),
        );
        remediation.push("corroborate candidate relationships with direct evidence".to_owned());
    }
    if unknown_edges > 0 {
        gaps.push("stale or incomplete relationships were used".to_owned());
        remediation
            .push("refresh or complete evidence for coverage-unknown relationships".to_owned());
    }
    let impacted_ids = items
        .iter()
        .map(|item| &item.node.id)
        .chain(std::iter::once(&target.id))
        .collect::<BTreeSet<_>>();
    let unknown_compatibility = context.compatibility.iter().filter(|input| {
        impacted_ids.contains(&input.contract_node_id)
            && input.status == ImpactCompatibilityStatus::Unknown
    });
    let mut compatibility_unknown = false;
    for input in unknown_compatibility {
        compatibility_unknown = true;
        gaps.push(format!(
            "compatibility is unknown for contract node `{}`",
            input.contract_node_id.as_str()
        ));
        remediation.extend(input.recommended_validations.iter().cloned());
    }
    for local in &context.local_enrichment {
        if local.status != LocalEnrichmentStatus::Available || local.truncated {
            gaps.push(format!(
                "local enrichment for repository `{}` is {:?}{}",
                local.repo_id.as_str(),
                local.status,
                if local.truncated {
                    " and truncated"
                } else {
                    ""
                }
            ));
            remediation.extend(local.degradations.iter().cloned());
        }
    }
    if truncation.is_some() {
        gaps.push("configured traversal bounds truncated impact analysis".to_owned());
        remediation.push("increase impact bounds or narrow the target scope".to_owned());
    }
    gaps = sorted_unique(gaps);
    remediation = sorted_unique(remediation);
    CoverageSummary {
        sufficient_for_score: context.graph_complete
            && stale.is_empty()
            && partial.is_empty()
            && unavailable.is_empty()
            && missing.is_empty()
            && possible_edges == 0
            && unknown_edges == 0
            && !compatibility_unknown
            && truncation.is_none()
            && context
                .local_enrichment
                .iter()
                .all(|local| local.status == LocalEnrichmentStatus::Available && !local.truncated)
            && context.coverage_gaps.is_empty(),
        relevant_repositories: relevant.into_iter().collect(),
        fresh_repositories: fresh,
        stale_repositories: stale,
        partial_repositories: partial,
        unavailable_repositories: unavailable,
        missing_repositories: missing,
        possible_edges,
        unknown_edges,
        gaps,
        remediation,
        total_items: items.len(),
    }
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "Risk inputs stay explicit to make every scored dimension auditable"
)]
fn assess_risk(
    target: &Node,
    items: &[ImpactItem],
    repositories: &[RepositoryImpact],
    services: &[ServiceImpact],
    contracts: &[ContractImpact],
    communities: &[CommunityImpact],
    tests: &[TestRecommendation],
    context: &ImpactContext,
    coverage: &CoverageSummary,
    truncation: Option<&TruncationInfo>,
) -> (RiskLevel, Option<f32>, Vec<RiskFactor>) {
    let impacted_ids = items
        .iter()
        .map(|item| &item.node.id)
        .chain(std::iter::once(&target.id))
        .collect::<BTreeSet<_>>();
    let direct_count = items
        .iter()
        .filter(|item| item.classification == ImpactClassification::DirectlyDependent)
        .count();
    let transitive_count = items
        .iter()
        .filter(|item| item.classification == ImpactClassification::TransitivelyAffected)
        .count();
    let mut factors = Vec::new();
    if direct_count > 0 {
        push_factor(
            &mut factors,
            "direct_consumers",
            usize_to_f32(direct_count).mul_add(4.0, 0.0).min(24.0),
            format!("{direct_count} confirmed direct dependents"),
            direct_evidence(items),
        );
    }
    if transitive_count > 0 {
        push_factor(
            &mut factors,
            "transitive_fanout",
            usize_to_f32(transitive_count).mul_add(1.5, 0.0).min(15.0),
            format!("{transitive_count} confirmed transitive impacts"),
            Vec::new(),
        );
    }
    let mut repository_ids = repositories
        .iter()
        .map(|impact| impact.repo_id.clone())
        .collect::<BTreeSet<_>>();
    repository_ids.extend(target.repo_id.iter().cloned());
    if repository_ids.len() > 1 {
        push_factor(
            &mut factors,
            "cross_repository_count",
            usize_to_f32(repository_ids.len().saturating_sub(1))
                .mul_add(4.0, 0.0)
                .min(16.0),
            format!("impact spans {} repositories", repository_ids.len()),
            repository_ids
                .iter()
                .map(|repo_id| repo_id.as_str().to_owned())
                .collect(),
        );
    }
    if !services.is_empty() {
        push_factor(
            &mut factors,
            "service_impact",
            usize_to_f32(services.len()).mul_add(2.0, 0.0).min(10.0),
            format!("impact reaches {} services", services.len()),
            services
                .iter()
                .map(|impact| impact.service.stable_key.clone())
                .collect(),
        );
    }
    let public_contracts = contracts.iter().filter(|contract| contract.public).count()
        + usize::from(context.public_contracts.contains(&target.id));
    if public_contracts > 0 {
        push_factor(
            &mut factors,
            "public_contract",
            12.0,
            format!("{public_contracts} explicitly public contracts are involved"),
            contracts
                .iter()
                .filter(|contract| contract.public)
                .map(|contract| contract.contract.stable_key.clone())
                .collect(),
        );
    }
    add_compatibility_factors(&mut factors, &impacted_ids, context);
    if let Some(centrality) = context
        .centrality
        .get(&target.id)
        .filter(|value| **value >= 0.75)
    {
        push_factor(
            &mut factors,
            "centrality",
            *centrality * 12.0,
            format!("target centrality is {centrality:.3}"),
            vec![target.id.as_str().to_owned()],
        );
    }
    if !communities.is_empty() {
        let cross_edges = communities
            .iter()
            .filter(|community| community.coupling > 0.0)
            .count();
        if cross_edges > 0 {
            push_factor(
                &mut factors,
                "community_process_fanout",
                usize_to_f32(cross_edges).mul_add(3.0, 0.0).min(9.0),
                format!("{cross_edges} affected communities cross structural boundaries"),
                communities
                    .iter()
                    .map(|community| community.community_id.as_str().to_owned())
                    .collect(),
            );
        }
    }
    add_criticality_factors(&mut factors, &impacted_ids, context);
    if tests.is_empty() && !items.is_empty() {
        push_factor(
            &mut factors,
            "missing_tests",
            10.0,
            "no linked or locally supplied affected tests were found".to_owned(),
            Vec::new(),
        );
    }
    let owners = owner_count(&impacted_ids, context);
    if owners == 0 && !items.is_empty() {
        push_factor(
            &mut factors,
            "missing_owners",
            8.0,
            "no Owner node is linked by OwnedBy to an impacted node".to_owned(),
            Vec::new(),
        );
    }
    let environments = context
        .environments
        .iter()
        .filter(|assignment| impacted_ids.contains(&assignment.node_id))
        .map(|assignment| assignment.environment.as_str())
        .collect::<BTreeSet<_>>();
    if environments.len() > 1 {
        push_factor(
            &mut factors,
            "cross_environment",
            10.0,
            format!("impact spans {} explicit environments", environments.len()),
            environments.into_iter().map(str::to_owned).collect(),
        );
    }
    if !coverage.sufficient_for_score {
        push_factor(
            &mut factors,
            "coverage_unknown",
            0.0,
            "coverage is insufficient for a numeric risk conclusion".to_owned(),
            coverage.gaps.clone(),
        );
    }
    if let Some(info) = truncation {
        push_factor(
            &mut factors,
            "truncated",
            0.0,
            info.explanation.clone(),
            vec![format!("{}={}", info.bound, info.limit)],
        );
    }
    factors.sort_by(|left, right| {
        right
            .weight
            .partial_cmp(&left.weight)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.explanation.cmp(&right.explanation))
    });
    if !coverage.sufficient_for_score {
        return (RiskLevel::Unknown, None, factors);
    }
    let score = factors
        .iter()
        .map(|factor| factor.weight)
        .sum::<f32>()
        .clamp(1.0, 100.0);
    let explicit_critical = factors.iter().any(|factor| {
        matches!(
            factor.code.as_str(),
            "critical_tag" | "security_tag" | "payment_tag" | "data_boundary_tag"
        ) && !factor.evidence.is_empty()
    });
    let level = if score >= 85.0 && explicit_critical {
        RiskLevel::Critical
    } else if score >= 50.0 {
        RiskLevel::High
    } else if score >= 25.0 {
        RiskLevel::Medium
    } else {
        RiskLevel::Low
    };
    (level, Some(score), factors)
}

fn add_compatibility_factors(
    factors: &mut Vec<RiskFactor>,
    impacted_ids: &BTreeSet<&NodeId>,
    context: &ImpactContext,
) {
    for input in &context.compatibility {
        if !impacted_ids.contains(&input.contract_node_id) {
            continue;
        }
        match input.status {
            ImpactCompatibilityStatus::Breaking => push_factor(
                factors,
                "breaking_compatibility",
                30.0,
                "a compatibility engine reported a breaking contract change".to_owned(),
                input.evidence.clone(),
            ),
            ImpactCompatibilityStatus::PotentiallyBreaking => push_factor(
                factors,
                "potentially_breaking_compatibility",
                18.0,
                "a compatibility engine reported a potentially breaking change".to_owned(),
                input.evidence.clone(),
            ),
            ImpactCompatibilityStatus::Compatible => {}
            ImpactCompatibilityStatus::Unknown => push_factor(
                factors,
                "compatibility_unknown",
                0.0,
                "compatibility coverage is unknown".to_owned(),
                input
                    .evidence
                    .iter()
                    .chain(input.recommended_validations.iter())
                    .cloned()
                    .collect(),
            ),
        }
    }
}

fn add_criticality_factors(
    factors: &mut Vec<RiskFactor>,
    impacted_ids: &BTreeSet<&NodeId>,
    context: &ImpactContext,
) {
    for assignment in &context.criticality {
        if !impacted_ids.contains(&assignment.node_id) {
            continue;
        }
        let (code, weight) = match assignment.tag {
            CriticalityTag::Critical => ("critical_tag", 55.0),
            CriticalityTag::Authentication => ("authentication_tag", 35.0),
            CriticalityTag::Security => ("security_tag", 50.0),
            CriticalityTag::Payment => ("payment_tag", 50.0),
            CriticalityTag::DataBoundary => ("data_boundary_tag", 45.0),
        };
        push_factor(
            factors,
            code,
            weight,
            format!(
                "explicit {:?} tag applies to impacted node `{}`",
                assignment.tag,
                assignment.node_id.as_str()
            ),
            assignment.evidence.clone(),
        );
    }
}

fn owner_count(impacted_ids: &BTreeSet<&NodeId>, context: &ImpactContext) -> usize {
    context
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::OwnedBy && impacted_ids.contains(&edge.source))
        .map(|edge| &edge.target)
        .collect::<BTreeSet<_>>()
        .len()
}

fn direct_evidence(items: &[ImpactItem]) -> Vec<String> {
    let mut evidence = items
        .iter()
        .filter(|item| item.classification == ImpactClassification::DirectlyDependent)
        .flat_map(|item| item.evidence.iter().map(|id| id.as_str().to_owned()))
        .collect::<Vec<_>>();
    evidence.sort();
    evidence.dedup();
    evidence
}

fn push_factor(
    factors: &mut Vec<RiskFactor>,
    code: &str,
    weight: f32,
    explanation: String,
    evidence: Vec<String>,
) {
    factors.push(RiskFactor {
        code: code.to_owned(),
        weight,
        explanation,
        evidence: sorted_unique(evidence),
    });
}

fn usize_to_f32(value: usize) -> f32 {
    u16::try_from(value).map_or(f32::from(u16::MAX), f32::from)
}

fn split_classifications(
    items: Vec<ImpactItem>,
) -> (
    Vec<ImpactItem>,
    Vec<ImpactItem>,
    Vec<ImpactItem>,
    Vec<ImpactItem>,
) {
    let mut direct = Vec::new();
    let mut transitive = Vec::new();
    let mut possible = Vec::new();
    let mut unknown = Vec::new();
    for item in items {
        match item.classification {
            ImpactClassification::DirectlyDependent => direct.push(item),
            ImpactClassification::TransitivelyAffected => transitive.push(item),
            ImpactClassification::PossiblyAffected => possible.push(item),
            ImpactClassification::UnknownDueToCoverage => unknown.push(item),
        }
    }
    (direct, transitive, possible, unknown)
}

fn sorted_unique(mut values: Vec<String>) -> Vec<String> {
    values.sort();
    values.dedup();
    values
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{CheckoutId, Community, CommunityConfig, CommunityMetrics};

    use super::*;

    fn node(id: &str, kind: NodeKind, repo: &str) -> Node {
        Node {
            id: NodeId::new(id),
            kind,
            repo_id: Some(RepoId::new(repo)),
            stable_key: format!("{repo}:{id}"),
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
            evidence: vec![EvidenceId::new(format!("e-{id}"))],
        }
    }

    fn freshness(repo: &str, state: RepoFreshnessState) -> RepoFreshness {
        RepoFreshness {
            repo_id: RepoId::new(repo),
            checkout_id: CheckoutId::new(format!("checkout-{repo}")),
            head_commit: Some("abc".to_owned()),
            manifest_hash: "manifest".to_owned(),
            state,
            reason: None,
        }
    }

    fn context(nodes: Vec<Node>, edges: Vec<Edge>) -> ImpactContext {
        let repos = nodes
            .iter()
            .filter_map(|item| item.repo_id.clone())
            .collect::<BTreeSet<_>>();
        ImpactContext {
            nodes,
            edges,
            communities: None,
            freshness: repos
                .iter()
                .map(|repo| freshness(repo.as_str(), RepoFreshnessState::Fresh))
                .collect(),
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

    fn request(target: &str, direction: ImpactDirection) -> ImpactRequest {
        ImpactRequest {
            target: ImpactTarget::NodeId {
                node_id: NodeId::new(target),
            },
            direction,
            options: ImpactOptions::default(),
        }
    }

    fn analyze(context: &ImpactContext, direction: ImpactDirection) -> ImpactReport {
        analyze_impact(&request("target", direction), context).expect("analysis should succeed")
    }

    #[test]
    fn breaking_direct_impact_should_raise_high_risk() {
        let mut context = context(
            vec![
                node("consumer", NodeKind::Service, "a"),
                node("target", NodeKind::HttpOperation, "b"),
            ],
            vec![edge("direct", "consumer", "target")],
        );
        context.compatibility.push(CompatibilityInput {
            contract_node_id: NodeId::new("target"),
            status: ImpactCompatibilityStatus::Breaking,
            evidence: vec!["http.required_parameter_added".to_owned()],
            recommended_validations: Vec::new(),
        });
        context.public_contracts.push(NodeId::new("target"));

        let report = analyze(&context, ImpactDirection::Upstream);

        assert_eq!(report.risk, RiskLevel::High);
    }

    #[test]
    fn breaking_transitive_path_should_remain_confirmed() {
        let context = context(
            vec![
                node("far", NodeKind::Service, "a"),
                node("near", NodeKind::Service, "b"),
                node("target", NodeKind::HttpOperation, "c"),
            ],
            vec![edge("one", "near", "target"), edge("two", "far", "near")],
        );

        let report = analyze(&context, ImpactDirection::Upstream);

        assert_eq!(report.transitive_consumers[0].node.id.as_str(), "far");
    }

    #[test]
    fn stale_freshness_should_prevent_false_safe_score() {
        let mut context = context(
            vec![
                node("consumer", NodeKind::Service, "a"),
                node("target", NodeKind::HttpOperation, "b"),
            ],
            vec![edge("direct", "consumer", "target")],
        );
        context.freshness[0].state = RepoFreshnessState::WorkingTreeChanged;

        let report = analyze(&context, ImpactDirection::Upstream);

        assert_eq!((report.risk, report.risk_score), (RiskLevel::Unknown, None));
    }

    #[test]
    fn low_confidence_should_be_unknown_risk_not_low_impact() {
        let mut candidate = edge("candidate", "consumer", "target");
        candidate.confidence = 0.2;
        let context = context(
            vec![
                node("consumer", NodeKind::Service, "a"),
                node("target", NodeKind::HttpOperation, "b"),
            ],
            vec![candidate],
        );

        let report = analyze(&context, ImpactDirection::Upstream);

        assert_eq!(
            (report.risk, report.possibly_affected[0].classification),
            (RiskLevel::Unknown, ImpactClassification::PossiblyAffected)
        );
    }

    #[test]
    fn depth_truncation_should_force_unknown() {
        let context = context(
            vec![
                node("three", NodeKind::Service, "a"),
                node("two", NodeKind::Service, "b"),
                node("one", NodeKind::Service, "c"),
                node("target", NodeKind::HttpOperation, "d"),
            ],
            vec![
                edge("one", "one", "target"),
                edge("two", "two", "one"),
                edge("three", "three", "two"),
            ],
        );
        let mut request = request("target", ImpactDirection::Upstream);
        request.options.max_depth = 2;

        let report = analyze_impact(&request, &context).expect("analysis should succeed");

        assert_eq!(report.risk, RiskLevel::Unknown);
    }

    #[test]
    fn direction_should_select_incoming_or_outgoing_edges() {
        let context = context(
            vec![
                node("upstream", NodeKind::Service, "a"),
                node("target", NodeKind::HttpOperation, "b"),
                node("downstream", NodeKind::Service, "c"),
            ],
            vec![
                edge("incoming", "upstream", "target"),
                edge("outgoing", "target", "downstream"),
            ],
        );

        let report = analyze(&context, ImpactDirection::Downstream);

        assert_eq!(report.direct_consumers[0].node.id.as_str(), "downstream");
    }

    #[test]
    fn both_direction_should_include_both_sides() {
        let context = context(
            vec![
                node("upstream", NodeKind::Service, "a"),
                node("target", NodeKind::HttpOperation, "b"),
                node("downstream", NodeKind::Service, "c"),
            ],
            vec![
                edge("incoming", "upstream", "target"),
                edge("outgoing", "target", "downstream"),
            ],
        );

        let report = analyze(&context, ImpactDirection::Both);

        assert_eq!(report.coverage.total_items, 2);
    }

    #[test]
    fn both_direction_should_not_treat_structural_siblings_as_impacted() {
        let mut contains_target = edge("contains-target", "artifact", "target");
        contains_target.kind = EdgeKind::Contains;
        let mut contains_sibling = edge("contains-sibling", "artifact", "sibling");
        contains_sibling.kind = EdgeKind::Contains;
        let mut reads_sibling = edge("reads-sibling", "reader", "sibling");
        reads_sibling.kind = EdgeKind::ReadsTable;
        let context = context(
            vec![
                node("target", NodeKind::DatabaseTable, "schema"),
                node("artifact", NodeKind::Artifact, "schema"),
                node("sibling", NodeKind::DatabaseTable, "schema"),
                node("reader", NodeKind::SymbolRef, "consumer"),
            ],
            vec![contains_target, contains_sibling, reads_sibling],
        );

        let report = analyze(&context, ImpactDirection::Both);
        let impacted = report
            .direct_consumers
            .iter()
            .chain(&report.transitive_consumers)
            .map(|item| item.node.id.as_str())
            .collect::<BTreeSet<_>>();

        assert_eq!(impacted, BTreeSet::from(["artifact"]));
    }

    #[test]
    fn cycles_should_terminate_without_repeating_target() {
        let context = context(
            vec![
                node("a", NodeKind::Service, "a"),
                node("target", NodeKind::Service, "b"),
            ],
            vec![edge("one", "target", "a"), edge("two", "a", "target")],
        );

        let report = analyze(&context, ImpactDirection::Downstream);

        assert_eq!(report.coverage.total_items, 1);
    }

    #[test]
    fn pagination_should_use_stable_combined_order() {
        let context = context(
            vec![
                node("a", NodeKind::Service, "a"),
                node("b", NodeKind::Service, "b"),
                node("target", NodeKind::Service, "t"),
            ],
            vec![edge("a", "a", "target"), edge("b", "b", "target")],
        );
        let mut request = request("target", ImpactDirection::Upstream);
        request.options.offset = 1;
        request.options.limit = 1;

        let report = analyze_impact(&request, &context).expect("analysis should succeed");

        assert_eq!(report.direct_consumers[0].node.id.as_str(), "b");
    }

    #[test]
    fn summary_only_should_omit_items_and_retain_counts() {
        let context = context(
            vec![
                node("consumer", NodeKind::Service, "a"),
                node("target", NodeKind::Service, "b"),
            ],
            vec![edge("direct", "consumer", "target")],
        );
        let mut request = request("target", ImpactDirection::Upstream);
        request.options.summary_only = true;

        let report = analyze_impact(&request, &context).expect("analysis should succeed");

        assert_eq!(
            (report.direct_consumers.len(), report.coverage.total_items),
            (0, 1)
        );
    }

    #[test]
    fn graph_tests_and_owners_should_be_ranked() {
        let mut validates = edge("validates", "test", "consumer");
        validates.kind = EdgeKind::Validates;
        let mut owned = edge("owned", "test", "owner");
        owned.kind = EdgeKind::OwnedBy;
        let context = context(
            vec![
                node("test", NodeKind::TestCase, "a"),
                node("owner", NodeKind::Owner, "a"),
                node("consumer", NodeKind::Service, "a"),
                node("target", NodeKind::HttpOperation, "b"),
            ],
            vec![edge("direct", "consumer", "target"), validates, owned],
        );

        let report = analyze(&context, ImpactDirection::Upstream);

        assert_eq!(
            report.test_recommendations[0].owners[0].id.as_str(),
            "owner"
        );
    }

    #[test]
    fn explicit_commands_should_only_be_returned_not_executed() {
        let mut validates = edge("validates", "test", "consumer");
        validates.kind = EdgeKind::Validates;
        let mut context = context(
            vec![
                node("test", NodeKind::TestCase, "a"),
                node("consumer", NodeKind::Service, "a"),
                node("target", NodeKind::Service, "b"),
            ],
            vec![edge("direct", "consumer", "target"), validates],
        );
        context.recommended_commands.push(RecommendedCommand {
            repo_id: RepoId::new("a"),
            command: "cargo test -p consumer".to_owned(),
            description: "consumer tests".to_owned(),
        });

        let report = analyze(&context, ImpactDirection::Upstream);

        assert_eq!(
            report.test_recommendations[0].recommended_commands,
            vec!["cargo test -p consumer"]
        );
    }

    #[test]
    fn affected_community_should_include_coupling() {
        let mut context = context(
            vec![
                node("consumer", NodeKind::Service, "a"),
                node("target", NodeKind::Service, "b"),
            ],
            vec![edge("direct", "consumer", "target")],
        );
        context.communities = Some(CommunitySnapshot {
            snapshot_id: "snapshot".to_owned(),
            engine_version: "1.0.0".to_owned(),
            config: CommunityConfig {
                algorithm: code_system_graph_model::CommunityAlgorithm::ConnectedComponents,
                scope: code_system_graph_model::CommunityScope::Federated,
                seed: 0,
                resolution: 1.0,
                minimum_confidence: 0.8,
                edge_weights: Vec::new(),
                max_iterations: 1,
            },
            communities: vec![Community {
                id: CommunityId::new("community"),
                label: "orders".to_owned(),
                members: vec![NodeId::new("consumer")],
                central_nodes: Vec::new(),
                repositories: vec![RepoId::new("a")],
                services: vec![NodeId::new("consumer")],
                inbound_contracts: Vec::new(),
                outbound_contracts: Vec::new(),
                metrics: CommunityMetrics {
                    size: 1,
                    density: 0.0,
                    cohesion: 0.0,
                    coupling: 2.0,
                    cross_community_edges: 1,
                },
                label_evidence: Vec::new(),
                limitations: Vec::new(),
            }],
        });

        let report = analyze(&context, ImpactDirection::Upstream);

        assert!((report.affected_communities[0].coupling - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn local_enrichment_degradation_should_be_visible_and_unknown() {
        let mut context = context(
            vec![
                node("consumer", NodeKind::Service, "a"),
                node("target", NodeKind::Service, "b"),
            ],
            vec![edge("direct", "consumer", "target")],
        );
        context.local_enrichment.push(LocalEnrichmentInput {
            repo_id: RepoId::new("a"),
            anchor: "consumer".to_owned(),
            status: LocalEnrichmentStatus::Stale,
            affected: Vec::new(),
            affected_tests: Vec::new(),
            truncated: false,
            degradations: vec!["reindex repository".to_owned()],
        });

        let report = analyze(&context, ImpactDirection::Upstream);

        assert_eq!(
            (report.risk, report.local_impact_summaries[0].status),
            (RiskLevel::Unknown, LocalEnrichmentStatus::Stale)
        );
    }

    #[test]
    fn explicit_security_tag_with_evidence_can_produce_critical() {
        let mut context = context(
            vec![
                node("consumer", NodeKind::Service, "a"),
                node("target", NodeKind::HttpOperation, "b"),
            ],
            vec![edge("direct", "consumer", "target")],
        );
        context.criticality.push(CriticalityAssignment {
            node_id: NodeId::new("target"),
            tag: CriticalityTag::Security,
            evidence: vec!["policy:security-boundary".to_owned()],
        });
        context.public_contracts.push(NodeId::new("target"));
        context.compatibility.push(CompatibilityInput {
            contract_node_id: NodeId::new("target"),
            status: ImpactCompatibilityStatus::Breaking,
            evidence: vec!["breaking".to_owned()],
            recommended_validations: Vec::new(),
        });

        let report = analyze(&context, ImpactDirection::Upstream);

        assert_eq!(report.risk, RiskLevel::Critical);
    }

    #[test]
    fn labels_should_not_infer_critical_tags() {
        let context = context(
            vec![
                node("payment-security", NodeKind::Service, "a"),
                node("target", NodeKind::Service, "b"),
            ],
            vec![edge("direct", "payment-security", "target")],
        );

        let report = analyze(&context, ImpactDirection::Upstream);

        assert!(
            !report
                .reasons
                .iter()
                .any(|factor| factor.code.ends_with("_tag"))
        );
    }

    #[test]
    fn identical_inputs_should_produce_identical_reports() {
        let context = context(
            vec![
                node("b", NodeKind::Service, "b"),
                node("target", NodeKind::Service, "t"),
                node("a", NodeKind::Service, "a"),
            ],
            vec![edge("b", "b", "target"), edge("a", "a", "target")],
        );

        let first = analyze(&context, ImpactDirection::Upstream);
        let second = analyze(&context, ImpactDirection::Upstream);

        assert_eq!(first, second);
    }

    #[test]
    fn invalid_bounds_should_be_rejected() {
        let context = context(vec![node("target", NodeKind::Service, "a")], Vec::new());
        let mut request = request("target", ImpactDirection::Both);
        request.options.max_depth = 0;

        let error = analyze_impact(&request, &context).expect_err("zero depth must be rejected");

        assert_eq!(error, ImpactError::InvalidBounds);
    }

    #[test]
    fn invalid_edge_confidence_should_be_rejected() {
        let mut invalid = edge("invalid", "consumer", "target");
        invalid.confidence = f32::NAN;
        let context = context(
            vec![
                node("consumer", NodeKind::Service, "a"),
                node("target", NodeKind::Service, "b"),
            ],
            vec![invalid],
        );

        let error = analyze_impact(&request("target", ImpactDirection::Upstream), &context)
            .expect_err("NaN confidence must be rejected");

        assert_eq!(
            error,
            ImpactError::InvalidEdgeConfidence("invalid".to_owned())
        );
    }

    #[test]
    fn fresh_complete_empty_impact_should_have_positive_low_score() {
        let context = context(vec![node("target", NodeKind::Service, "a")], Vec::new());

        let report = analyze(&context, ImpactDirection::Both);

        assert_eq!(
            (report.risk, report.risk_score),
            (RiskLevel::Low, Some(1.0))
        );
    }
}

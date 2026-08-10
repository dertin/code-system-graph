//! Public Explore request and report contracts.

use code_system_graph_core::{
    AgentNextAction, ExecutionPolicy, LocalNeighbor, LocalNeighborDirection, ProviderExecution, ResolvedSymbol
};
use code_system_graph_model::{EpistemicStatus, NodeId, RepoFreshnessState, RepoId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Ephemeral intra-repository source and flow exploration input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExploreInput {
    /// Workspace selected by the delivery adapter.
    pub workspace: String,
    /// Registered repository alias; omitted only when the workspace contains one repository.
    #[serde(default)]
    pub repository: Option<String>,
    /// Focused symbol, flow, architecture, or implementation question.
    pub query: String,
    /// Maximum source files returned by the local provider.
    #[serde(default)]
    #[schemars(range(min = 1))]
    pub max_files: Option<usize>,
}

/// Repository identity and freshness attached to an Explore response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExploreRepositoryContext {
    /// Registered alias.
    pub alias: String,
    /// Stable repository identity.
    pub repo_id: RepoId,
    /// Canonical checkout display path.
    pub root: String,
    /// Current Git revision when available.
    pub revision: Option<String>,
    /// Persisted freshness for this repository.
    pub freshness: RepoFreshnessState,
}

/// One local caller/callee relationship discovered ephemerally.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExploreLocalRelationship {
    /// Exact anchor selected for traversal.
    pub anchor: String,
    /// Direction relative to the anchor.
    pub direction: LocalNeighborDirection,
    /// Bounded neighboring symbol.
    pub neighbor: LocalNeighbor,
}

/// Verifiable persisted evidence location used by a federated handoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExploreEvidenceLocation {
    /// Repository containing the evidence.
    pub repo_id: RepoId,
    /// Repository-relative path.
    pub path: String,
    /// Optional inclusive first line.
    pub start_line: Option<u32>,
    /// Optional inclusive last line.
    pub end_line: Option<u32>,
}

/// Navigation from a local source anchor to a persisted federated entity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExploreFederatedHandoff {
    /// Local source anchor correlated with persisted evidence.
    pub anchor: String,
    /// Real persisted entity identifier.
    pub node_id: NodeId,
    /// Human-readable persisted entity label.
    pub label: String,
    /// Owning remote repository, when known.
    pub remote_repository: Option<ExploreRepositoryContext>,
    /// Confirmed, inferred, or ambiguous evidence state.
    pub status: EpistemicStatus,
    /// Persisted relationship confidence.
    pub confidence: f32,
    /// Bounded navigable evidence locations.
    pub evidence: Vec<ExploreEvidenceLocation>,
}

/// Exact execution accounting and applied bounds for Explore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExploreExecution {
    /// Effective immutable workspace policy.
    pub effective_policy: ExecutionPolicy,
    /// Public provider operations started.
    pub provider_operations: usize,
    /// Maximum concurrent provider operations observed.
    pub maximum_concurrency_observed: usize,
    /// Provider output bytes retained across successful operations.
    pub retained_bytes: usize,
    /// Per-operation provider metadata in completion order.
    pub operations: Vec<ProviderExecution>,
    /// Non-fatal budget, timeout, or provider degradations.
    pub degradations: Vec<String>,
}

/// Coverage and exact truncation accounting for Explore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExploreCoverage {
    /// Whether source context was attempted and retained.
    pub source_context: bool,
    /// Whether symbol resolution was attempted.
    pub symbol_resolution: bool,
    /// Number of anchors traversed in both directions.
    pub anchors_traversed: usize,
    /// Explicit incomplete coverage reasons.
    pub gaps: Vec<String>,
    /// Stable names of limits that truncated work or output.
    pub truncations: Vec<String>,
}

/// Complete ephemeral, bounded agent-facing Explore result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExploreReport {
    /// Selected repository identity and freshness.
    pub repository: ExploreRepositoryContext,
    /// Untrusted provider Markdown containing source; never persisted by this server.
    pub source_markdown: String,
    /// Bounded exact local symbols.
    pub resolved_symbols: Vec<ResolvedSymbol>,
    /// Bounded local caller/callee relationships.
    pub local_relationships: Vec<ExploreLocalRelationship>,
    /// Bounded correlations into persisted federated entities.
    pub federated_handoffs: Vec<ExploreFederatedHandoff>,
    /// Coverage and truncation facts.
    pub coverage: ExploreCoverage,
    /// Deterministic next navigation using real IDs only.
    pub next_actions: Vec<AgentNextAction>,
    /// Effective limits and observed consumption.
    pub execution: ExploreExecution,
}

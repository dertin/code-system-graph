//! Stable domain contracts for `Code System Graph`'s federated boundary graph.

use schemars::JsonSchema;
use semver::Version;
use serde::{Deserialize, Serialize};

macro_rules! string_id {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(
            Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
        )]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Creates an identifier from its canonical representation.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            /// Returns the canonical string representation.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

string_id!(RepoId, "Stable identifier for a registered repository.");
string_id!(
    CheckoutId,
    "Stable identifier for one repository checkout or linked worktree."
);
string_id!(
    WorkspaceId,
    "Stable identifier for a `Code System Graph` workspace."
);
string_id!(NodeId, "Stable identifier for a federated graph node.");
string_id!(EdgeId, "Stable identifier for a federated graph edge.");
string_id!(EvidenceId, "Stable identifier for an evidence record.");
string_id!(
    CommunityId,
    "Stable identifier for a detected graph community."
);

/// Lossless platform encoding used for a native filesystem path.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum NativePathEncoding {
    /// Raw Unix `OsStr` bytes.
    UnixBytes,
    /// Little-endian Windows UTF-16 code units.
    WindowsWide,
    /// UTF-8 fallback for other targets.
    Utf8,
}

/// Lossless native path plus a diagnostic-only display form.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct NativePath {
    /// Platform-specific lossless encoding.
    pub encoding: NativePathEncoding,
    /// Encoded path bytes; these are not assumed to be UTF-8.
    pub bytes: Vec<u8>,
    /// Lossy display form intended only for diagnostics.
    pub display: String,
}

/// Deterministic identity and checkout metadata for one registered repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepositoryRecord {
    /// Stable repository identity shared by linked worktrees.
    pub id: RepoId,
    /// Stable identity of this concrete checkout.
    pub checkout_id: CheckoutId,
    /// Unique alias within the workspace.
    pub alias: String,
    /// Canonical native checkout path.
    pub canonical_path: NativePath,
    /// Canonical Git common directory when the checkout is a Git repository.
    pub git_common_dir: Option<NativePath>,
    /// Credential-free normalized remote identity when available.
    pub normalized_remote: Option<String>,
    /// Current Git commit when available.
    pub head_commit: Option<String>,
    /// Whether Git reports a linked worktree rather than the common checkout.
    pub is_linked_worktree: bool,
    /// Whether tracked or untracked working-tree changes were observed.
    pub working_tree_dirty: bool,
}

/// Fully validated registry input for one workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceRecord {
    /// Stable workspace identity.
    pub id: WorkspaceId,
    /// User-facing workspace name.
    pub name: String,
    /// Fingerprint of the exact manifest content.
    pub manifest_hash: String,
    /// Canonical lossless workspace manifest path.
    pub config_path: Option<NativePath>,
    /// Repositories sorted by alias.
    pub repositories: Vec<RepositoryRecord>,
}

/// Freshness state for one repository checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RepoFreshnessState {
    /// Current inputs match the published snapshot.
    Fresh,
    /// The working tree changed after the snapshot.
    WorkingTreeChanged,
    /// Repository commits advanced after the snapshot.
    CommitsBehind,
    /// Workspace or repository configuration changed.
    ConfigChanged,
    /// An extractor version changed.
    ExtractorChanged,
    /// Local `CodeGraph` freshness is pending.
    CodegraphPending,
    /// Only part of the required inputs were scanned.
    Partial,
    /// Stored state failed an integrity check.
    Corrupt,
    /// Freshness could not be determined.
    Unknown,
    /// Repository checkout is unavailable.
    Unavailable,
}

/// Snapshot freshness recorded for one repository checkout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepoFreshness {
    /// Stable repository identity.
    pub repo_id: RepoId,
    /// Concrete checkout identity.
    pub checkout_id: CheckoutId,
    /// Commit observed by the snapshot.
    pub head_commit: Option<String>,
    /// Manifest fingerprint used by the snapshot.
    pub manifest_hash: String,
    /// Current freshness classification.
    pub state: RepoFreshnessState,
    /// Optional bounded explanation.
    pub reason: Option<String>,
}

/// Fingerprint of one extractor-relevant artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactFingerprint {
    /// Repository containing the artifact.
    pub repo_id: RepoId,
    /// Concrete checkout containing the artifact.
    pub checkout_id: CheckoutId,
    /// Repository-relative native path.
    pub path: NativePath,
    /// Extractor that consumes this artifact.
    pub extractor: String,
    /// BLAKE3 content fingerprint.
    pub content_hash: String,
    /// Exact file size in bytes.
    pub size_bytes: u64,
}

/// Incremental difference from the previous published artifact set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactChangeKind {
    /// Artifact did not exist previously.
    Added,
    /// Artifact content changed.
    Modified,
    /// Artifact is absent from the current scan.
    Deleted,
    /// Artifact fingerprint is unchanged.
    Unchanged,
}

/// Planned incremental action for one artifact identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactChange {
    /// Repository identity.
    pub repo_id: RepoId,
    /// Checkout identity.
    pub checkout_id: CheckoutId,
    /// Repository-relative path.
    pub path: NativePath,
    /// Extractor identity.
    pub extractor: String,
    /// Difference classification.
    pub kind: ArtifactChangeKind,
}

/// Outcome of one extractor execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExtractorRunStatus {
    /// Extractor completed successfully.
    Success,
    /// Inputs were unchanged and reusable.
    SkippedUnchanged,
}

/// Persisted metrics for one extractor execution or skip decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExtractorRun {
    /// Stable run identity.
    pub id: String,
    /// Published snapshot identity.
    pub snapshot_id: String,
    /// Repository identity.
    pub repo_id: RepoId,
    /// Checkout identity.
    pub checkout_id: CheckoutId,
    /// Extractor identity.
    pub extractor: String,
    /// Extractor semantic version.
    pub extractor_version: String,
    /// Run outcome.
    pub status: ExtractorRunStatus,
    /// Number of discovered files.
    pub discovered_files: u64,
    /// Number of parsed files.
    pub parsed_files: u64,
    /// Number of skipped files.
    pub skipped_files: u64,
    /// Bounded execution time in milliseconds.
    pub elapsed_ms: u64,
}

/// Opaque, versioned output owned by one extractor input.
///
/// The payload contains contract observations rather than source text. Its schema is owned by the
/// extractor identified by [`Self::source`] and [`Self::extractor_version`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StoredExtractorBatch {
    /// Fingerprint that identifies and invalidates the source-owned output.
    pub source: ArtifactFingerprint,
    /// Semantic version of the extractor payload schema.
    pub extractor_version: String,
    /// Canonical fingerprint of the effective extraction budgets.
    pub budget_fingerprint: String,
    /// Whether invalid UTF-8 input bytes were decoded lossily for text extraction.
    pub source_was_lossy: bool,
    /// Deterministic number of observations encoded in the payload.
    pub output_count: u64,
    /// UTF-8 JSON payload encoded as bytes for bounded, lossless persistence.
    pub payload: Vec<u8>,
}

/// Produces a deterministic namespaced identifier from a canonical key.
#[must_use]
pub fn stable_id(namespace: &str, canonical_key: &str) -> String {
    stable_id_bytes(namespace, canonical_key.as_bytes())
}

/// Produces a deterministic namespaced identifier from arbitrary canonical bytes.
#[must_use]
pub fn stable_id_bytes(namespace: &str, canonical_key: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(namespace.as_bytes());
    hasher.update(&[0]);
    hasher.update(canonical_key);
    format!("{namespace}:{}", hasher.finalize().to_hex())
}

/// Reports whether untrusted metadata contains control or bidirectional formatting characters.
///
/// Source files may legitimately contain such characters, but identifiers, labels, paths, and
/// diagnostic metadata must reject them before they reach durable state or terminal output.
#[must_use]
pub fn contains_unsafe_metadata_characters(value: &str) -> bool {
    value.chars().any(|character| {
        character.is_control()
            || matches!(
                character,
                '\u{061c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
                    | '\u{feff}'
            )
    })
}

/// Rejects repository-relative path display strings that are unsafe for metadata or diagnostics.
///
/// # Errors
///
/// Returns [`UnsafePathDisplayError`] when `value` contains control or bidirectional characters.
pub fn validate_safe_path_display(value: &str) -> Result<(), UnsafePathDisplayError> {
    if contains_unsafe_metadata_characters(value) {
        Err(UnsafePathDisplayError)
    } else {
        Ok(())
    }
}

/// Path display string contains unsafe metadata characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsafePathDisplayError;

impl std::fmt::Display for UnsafePathDisplayError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("path display contains unsafe control or bidirectional characters")
    }
}

impl std::error::Error for UnsafePathDisplayError {}

/// Kind of entity represented in the federated graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// Registered source repository.
    Repository,
    /// Deployable or logical service.
    Service,
    /// Package coordinate.
    Package,
    /// Boundary-defining artifact.
    Artifact,
    /// Lightweight reference to a repository-local symbol.
    SymbolRef,
    /// Executable test case in any supported language.
    TestCase,
    /// HTTP operation contract.
    HttpOperation,
    /// GraphQL operation contract.
    GraphqlOperation,
    /// Remote procedure call method.
    RpcMethod,
    /// Event topic, queue, or channel.
    EventChannel,
    /// Event payload schema.
    EventSchema,
    /// Database instance.
    Database,
    /// Database table.
    DatabaseTable,
    /// Database column.
    DatabaseColumn,
    /// Configuration key name.
    ConfigKey,
    /// Deployment unit.
    Deployment,
    /// Technical document.
    Document,
    /// Architecture decision record.
    Adr,
    /// Owning person or team.
    Owner,
    /// Local or remote change set.
    ChangeSet,
    /// Pull request.
    PullRequest,
    /// Detected graph community.
    Community,
}

/// Kind of relationship represented in the federated graph.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Parent contains child.
    Contains,
    /// Source provides target contract.
    Provides,
    /// Source consumes target contract.
    Consumes,
    /// Source performs a remote call to target.
    CallsRemote,
    /// Source publishes target event.
    Publishes,
    /// Source subscribes to target event.
    Subscribes,
    /// Event channel delivers messages to target subscriber.
    DeliversTo,
    /// Source depends on target package.
    DependsOnPackage,
    /// Source depends on target repository.
    DependsOnRepository,
    /// Source reads target table.
    ReadsTable,
    /// Source writes target table.
    WritesTable,
    /// Source deploys target.
    Deploys,
    /// Source configures target.
    Configures,
    /// Source documents target.
    Documents,
    /// Source is owned by target.
    OwnedBy,
    /// Source is implemented by target.
    ImplementedBy,
    /// Source test validates target contract or behavior.
    Validates,
    /// Source was changed in target.
    ChangedIn,
    /// Source affects target.
    Affects,
    /// Source migration is applied before target migration.
    Precedes,
    /// Source migration reverses target migration.
    Reverts,
    /// Source is compatible with target.
    CompatibleWith,
    /// Source is incompatible with target.
    IncompatibleWith,
    /// Source belongs to target community.
    MemberOf,
    /// User-declared relationship.
    ManualLink,
}

/// Origin of a graph assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    /// Explicitly declared in a source artifact.
    Declared,
    /// Deterministically extracted from a source artifact.
    Extracted,
    /// Derived from incomplete or indirect signals.
    Inferred,
    /// Explicitly supplied by a user.
    Manual,
    /// Observed at runtime.
    Runtime,
    /// Returned through a public `CodeGraph` capability.
    CodeGraph,
}

/// Confidence state attached to an assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EpistemicStatus {
    /// Supported by sufficient direct evidence.
    Confirmed,
    /// Derived from indirect evidence.
    Inferred,
    /// Multiple plausible interpretations remain.
    Ambiguous,
    /// Evidence no longer matches current inputs.
    Stale,
    /// Required inputs were unavailable.
    Incomplete,
}

/// Auditable evidence supporting a node or edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Evidence {
    /// Stable evidence identifier.
    pub id: EvidenceId,
    /// Repository in which the evidence was observed.
    pub repo_id: Option<RepoId>,
    /// Repository-relative source path.
    pub file_path: Option<String>,
    /// Inclusive first source line.
    pub start_line: Option<u32>,
    /// Inclusive last source line.
    pub end_line: Option<u32>,
    /// Extractor identifier.
    pub extractor: String,
    /// Extractor semantic version.
    pub extractor_version: String,
    /// Origin of this evidence.
    pub provenance: Provenance,
    /// Normalized confidence in the inclusive range from zero to one.
    pub confidence: f32,
    /// Commit at which this evidence was observed.
    pub observed_at_commit: Option<String>,
    /// Hash of the relevant source content.
    pub content_hash: Option<String>,
    /// Bounded explanatory note without source or secrets.
    pub note: Option<String>,
}

/// Entity in the federated graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Node {
    /// Stable node identifier.
    pub id: NodeId,
    /// Entity kind.
    pub kind: NodeKind,
    /// Owning repository when applicable.
    pub repo_id: Option<RepoId>,
    /// Versioned canonical identity key.
    pub stable_key: String,
    /// Human-readable label.
    pub label: String,
}

/// Relationship in the federated graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Edge {
    /// Stable edge identifier.
    pub id: EdgeId,
    /// Source node.
    pub source: NodeId,
    /// Target node.
    pub target: NodeId,
    /// Relationship kind.
    pub kind: EdgeKind,
    /// Normalized confidence in the inclusive range from zero to one.
    pub confidence: f32,
    /// Epistemic state.
    pub status: EpistemicStatus,
    /// Evidence records supporting this relationship.
    pub evidence: Vec<EvidenceId>,
}

/// Outcome of one auditable linker decision.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LinkStatus {
    /// The selected relationship is confirmed.
    Confirmed,
    /// An exact automatic relationship was removed by an explicit declaration.
    Suppressed,
    /// Multiple exact candidates prevented a selection.
    Ambiguous,
    /// The candidate was explicitly rejected.
    Rejected,
}

/// Exact candidate considered but not selected by a linker matcher.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RejectedAlternative {
    /// Candidate node identity.
    pub candidate: NodeId,
    /// Normalized matcher score in the inclusive range from zero to one.
    pub score: f32,
    /// Deterministically ordered reasons the candidate was rejected.
    pub reasons: Vec<String>,
}

/// Stable reference to evidence used by a linker decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRef {
    /// Stable evidence identity.
    pub id: EvidenceId,
    /// Origin of the referenced evidence.
    pub provenance: Provenance,
}

/// Versioned and explainable record of a linker match or suppression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinkDecision {
    /// Resolved source node.
    pub source: NodeId,
    /// Resolved target node.
    pub target: NodeId,
    /// Concrete relationship considered by the matcher.
    pub relation: EdgeKind,
    /// Stable matcher identifier.
    pub matcher: String,
    /// Semantic version of the matcher behavior.
    #[schemars(with = "String")]
    pub matcher_version: Version,
    /// Normalized raw match score in the inclusive range from zero to one.
    pub score: f32,
    /// Normalized confidence in the inclusive range from zero to one.
    pub confidence: f32,
    /// Deterministically ordered explanations for the decision.
    pub reasons: Vec<String>,
    /// Deterministically ordered candidates that were not selected.
    pub rejected_alternatives: Vec<RejectedAlternative>,
    /// Deterministically ordered supporting evidence references.
    pub evidence: Vec<EvidenceRef>,
    /// Final decision state.
    pub status: LinkStatus,
}

/// Overall freshness of data used by a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverallFreshness {
    /// All required inputs match their current fingerprints.
    Fresh,
    /// At least one relevant input is stale.
    Stale,
    /// Required inputs were only partially available.
    Partial,
    /// Freshness could not be established.
    Unknown,
}

/// Aggregate freshness attached to a public result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FreshnessSummary {
    /// Overall freshness classification.
    pub overall: OverallFreshness,
    /// Stable repository identifiers known to be stale.
    pub stale_repositories: Vec<RepoId>,
    /// Reasons freshness could not be fully established.
    pub reasons: Vec<String>,
}

/// A repository-scoped extraction limitation that prevents complete dependency coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepositoryCoverageGap {
    /// Repository whose extracted dependency coverage is incomplete.
    pub repo_id: RepoId,
    /// Source-free, actionable reason for the gap.
    pub reason: String,
}

/// Status of a public tool result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    /// Complete result within requested bounds.
    Ok,
    /// Useful result with explicitly missing capabilities or inputs.
    Degraded,
    /// Request could not be completed.
    Error,
}

/// One ordered segment in a federated trace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TraceSegment {
    /// Source node.
    pub source: Node,
    /// Traversed relationship.
    pub edge: Edge,
    /// Target node.
    pub target: Node,
}

/// Bounded and explainable federated trace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TraceReport {
    /// Ordered trace segments.
    pub segments: Vec<TraceSegment>,
    /// Whether a configured bound truncated traversal.
    pub truncated: bool,
    /// Human-readable coverage gaps.
    pub coverage_gaps: Vec<String>,
}

/// Community-detection algorithm selected for one reproducible analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommunityAlgorithm {
    /// Maximal regions connected by eligible edges.
    ConnectedComponents,
    /// Deterministic weighted label propagation used as a conservative fallback.
    WeightedClustering,
    /// Deterministic seeded Louvain modularity optimization.
    Louvain,
}

/// Scope over which communities are detected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum CommunityScope {
    /// Entire current federated graph.
    Federated,
    /// Nodes owned by one repository.
    Repository(RepoId),
    /// Nodes associated with one exact service stable key.
    Service(String),
    /// Every node in the selected workspace.
    Workspace,
}

/// Explicit weight assigned to one relationship kind during community detection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CommunityEdgeWeight {
    /// Relationship kind.
    pub kind: EdgeKind,
    /// Finite non-negative weight.
    pub weight: f64,
}

/// Versioned reproducibility inputs for one community analysis.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CommunityConfig {
    /// Selected algorithm.
    pub algorithm: CommunityAlgorithm,
    /// Analysis scope.
    pub scope: CommunityScope,
    /// Seed used to break otherwise equal deterministic choices.
    pub seed: u64,
    /// Positive Louvain resolution.
    pub resolution: f64,
    /// Minimum accepted edge confidence.
    pub minimum_confidence: f32,
    /// Explicit relationship weights; unspecified kinds use weight one.
    pub edge_weights: Vec<CommunityEdgeWeight>,
    /// Maximum optimization passes.
    pub max_iterations: u32,
}

/// Quantitative properties of one detected community.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CommunityMetrics {
    /// Number of member nodes.
    pub size: usize,
    /// Internal edges divided by possible directed edges.
    pub density: f64,
    /// Internal accepted edge weight.
    pub cohesion: f64,
    /// Accepted edge weight crossing the community boundary.
    pub coupling: f64,
    /// Number of accepted cross-community edges.
    pub cross_community_edges: usize,
}

/// Evidence explaining a deterministic community label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommunityLabelEvidence {
    /// Source node whose structure contributed a term.
    pub node_id: NodeId,
    /// Bounded normalized term.
    pub term: String,
}

/// One versioned and explainable graph community.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Community {
    /// Stable content-derived community identity.
    pub id: CommunityId,
    /// Deterministic structural label.
    pub label: String,
    /// Sorted member node identifiers.
    pub members: Vec<NodeId>,
    /// Central nodes sorted by descending centrality and stable identity.
    pub central_nodes: Vec<NodeId>,
    /// Repository identities represented by members.
    pub repositories: Vec<RepoId>,
    /// Service node identities represented by members.
    pub services: Vec<NodeId>,
    /// Contract nodes receiving edges from outside the community.
    pub inbound_contracts: Vec<NodeId>,
    /// Contract nodes sending edges outside the community.
    pub outbound_contracts: Vec<NodeId>,
    /// Quantitative graph metrics.
    pub metrics: CommunityMetrics,
    /// Structural evidence used to derive the label.
    pub label_evidence: Vec<CommunityLabelEvidence>,
    /// Explicit limitations or incomplete inputs.
    pub limitations: Vec<String>,
}

/// Complete community analysis tied to one immutable graph snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CommunitySnapshot {
    /// Graph snapshot analyzed.
    pub snapshot_id: String,
    /// Community engine semantic version.
    pub engine_version: String,
    /// Reproducibility configuration.
    pub config: CommunityConfig,
    /// Sorted detected communities.
    pub communities: Vec<Community>,
}

/// Material relationship between communities in two snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommunityChangeKind {
    /// Community exists only in the newer snapshot.
    Created,
    /// Community exists only in the older snapshot.
    Removed,
    /// One older community materially maps to multiple newer communities.
    Split,
    /// Multiple older communities materially map to one newer community.
    Merged,
    /// Best matching community retained identity but changed materially.
    MateriallyChanged,
}

/// Explainable community change between immutable snapshots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CommunityChange {
    /// Classified change.
    pub kind: CommunityChangeKind,
    /// Related older community identities.
    pub before: Vec<CommunityId>,
    /// Related newer community identities.
    pub after: Vec<CommunityId>,
    /// Maximum member Jaccard overlap supporting the classification.
    pub overlap: f64,
    /// Bounded deterministic explanation.
    pub explanation: String,
}

/// Community delta between two snapshots.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CommunityDelta {
    /// Older graph snapshot.
    pub before_snapshot_id: String,
    /// Newer graph snapshot.
    pub after_snapshot_id: String,
    /// Sorted material changes.
    pub changes: Vec<CommunityChange>,
}

/// Versioned result envelope shared by CLI and MCP.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolEnvelope<T> {
    /// Public schema version.
    pub schema_version: u32,
    /// Result status.
    pub status: ToolStatus,
    /// Typed result data.
    pub data: Option<T>,
    /// Freshness of inputs used to produce the result.
    pub freshness: FreshnessSummary,
    /// Non-fatal warnings.
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{contains_unsafe_metadata_characters, stable_id};

    #[test]
    fn stable_id_should_repeat_for_identical_input() {
        let first = stable_id("repo", "ssh://example.test/team/api");
        let second = stable_id("repo", "ssh://example.test/team/api");

        assert_eq!(first, second);
    }

    #[test]
    fn stable_id_should_separate_namespaces() {
        let repository = stable_id("repo", "shared-key");
        let service = stable_id("service", "shared-key");

        assert_ne!(repository, service);
    }

    #[test]
    fn unsafe_metadata_should_detect_terminal_and_bidi_controls() {
        assert!(contains_unsafe_metadata_characters(
            "trusted\u{202e}txt.exe"
        ));
        assert!(contains_unsafe_metadata_characters("line\nbreak"));
        assert!(!contains_unsafe_metadata_characters(
            "servicio-áccounts_日本"
        ));
    }
}

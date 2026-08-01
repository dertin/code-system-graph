//! Delivery-layer orchestration shared by the `Code System Graph` CLI and MCP server.

pub mod http_server;
pub mod mcp;
mod sync;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use atomic_write_file::AtomicWriteFile;
use code_system_graph_core::{
    AffectedTestsRequest, AnalyzerVersions, ArtifactKey, BatchAction, BatchPlanError, BitbucketProvider, ChangeAnalysisError, ChangeAnalysisOptions, ChangeError, ChangeImpactReport, ChangeProvider, ChangeRequest, ChangeScope, ChangeSet, CodeGraphConfig, CodeGraphProvider, CommunityError, ConfigDoctorInput, ConfigError, ConfigExtractionError, ContractReport, ContractRequest, CorroborationReport, DataDocument, DataExtractionError, DeclaredImplementation, DeclaredTestCase, DoctorReport, DoctorRequest, DocumentationDocument, DocumentationExtractionError, EffectiveRepositoryConfig, EventDocument, EventExtractionError, EventGraphFacts, ExitCode, ExportReport, ExportRequest, ExtractionGraphFacts, ExtractorBatch, ExtractorBatchPlan, FederatedGraph, FreshnessDoctorInput, GeneratedClientError, GeneratedClientMetadata, GitCliChangeProvider, GitHubProvider, GraphqlDocument, GraphqlExtractionError, GraphqlGraphFacts, HttpBoundary, HttpExtractionError, ImpactContext, ImpactError, ImpactReport, ImpactRequest, ImpactTarget, IncrementalPlan, InfrastructureDocument, InfrastructureExtractionError, IntegrityDoctorInput, InterfaceError, LinkError, LocalCodeIntelligenceProvider, LocalContextRequest, LocalContextResult, LocalEnrichmentInput, LocalEnrichmentStatus, LocalImpactItem, LocalImpactRequest, ManifestEdit, ManifestEditError, ManifestError, ManualLinkConfig, ManualLinkError, PackageGraphFacts, PackageManifest, PackageManifestError, PrAuthToken, ProtobufDocument, ProtobufExtractionError, ProtobufGraphFacts, ProviderBudget, ProviderCapability, ProviderDoctorInput, ProviderDoctorStatus, ProviderError, ProviderRequest, ProviderStatus, PullRequestCoordinates, PullRequestError, PullRequestInspectRequest, PullRequestInspection, PullRequestListPage, PullRequestListRequest, PullRequestListState, PullRequestProvider, PullRequestProviderConfig, PullRequestProviderKind, QueryError, RecommendedCommand, RegisteredWorkspace, RegistryError, ReqwestPrHttpTransport, SafeConfigDocument, SchemaDoctorInput, SearchFilters, SearchReport, SearchRequest, SourceEpistemicStatus, SourceGraphFacts, SourceLanguage, SourceObservation, SourceRole, SourceSyntaxError, SourceSyntaxLanguage, SourceWarning, SymbolAnchor, SymbolCorroboration, TraceError, TraversalReport, TraversalRequest, WorkspaceManifest, affected_link_keys, analyze_changes, analyze_communities, analyze_impact, apply_openapi_override, classify_interface_error, commit_manifest_edit, compare_community_snapshots, corroborate_repository, declared_implementation, declared_test_case, doctor, documents_to_graph, encode_native_path, event_documents_to_graph, export_graph, extract_asyncapi, extract_codeowners, extract_data_artifact, extract_docker_compose, extract_generated_client_metadata, extract_graphql_document, extract_graphql_persisted_operations, extract_helm, extract_kubernetes, extract_markdown, extract_openapi, extract_package_manifest, extract_protobuf, extract_safe_config, extract_service_catalog, extract_terraform, graphql_documents_to_graph, inspect_contracts, inspect_source_syntax, link_declared_implementations, link_declared_tests, link_http_boundaries, link_registered_package_owners, load_extractor_batch, merge_affected_link_neighborhoods, package_manifest_to_graph, parse_event_source, parse_go_source, parse_graphql_source, parse_java_source, parse_javascript_source_at_path, parse_literal_sql_source_at_root, parse_manifest, parse_protobuf_generated_source, parse_python_source, parse_rust_source, parse_typescript_source_at_path, plan_extractor_batches, plan_incremental_scan, preview_add_manual_link, preview_add_repository, preview_remove_repository, protobuf_documents_to_graph, register_workspace, resolve_manual_links, resolve_repository_config, search, source_observations_to_graph, store_extractor_batch, traverse
};
pub use code_system_graph_core::{
    ConfigSource, DEFAULT_EXCLUDES, IgnorePolicy, PROTECTED_EXCLUDES
};
use code_system_graph_model::{
    ArtifactFingerprint, CheckoutId, Community, CommunityAlgorithm, CommunityConfig, CommunityDelta, CommunityId, CommunityScope, Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, EvidenceId, ExtractorRun, ExtractorRunStatus, FreshnessSummary, LinkDecision, LinkStatus, Node, NodeId, NodeKind, OverallFreshness, Provenance, RepoFreshness, RepoFreshnessState, RepoId, RepositoryRecord, StoredExtractorBatch, ToolEnvelope, ToolStatus, TraceReport, WorkspaceRecord, stable_id, stable_id_bytes
};
use code_system_graph_store_sqlite::{
    ManualLinkDisposition, ManualLinkRecord, ProviderCapabilityRecord, QueryCacheRecord, SnapshotBatch, SqliteStore, StoreError, StoreLock, latest_schema_version
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
pub use sync::{
    CodeGraphRepositorySync, CodeGraphSyncState, CodeGraphSyncSummary, SyncSummary, SyncTarget, sync_workspace_with_overrides, workspace_sync_targets
};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

const MAX_TRACE_DEPTH: usize = 32;
const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;
const FOCUSED_EXTRACTOR_VERSION: &str = "1.0.0+extractor.4";
const LOSSY_FOCUSED_EXTRACTOR_VERSION: &str = "1.0.0+extractor.4.lossy";
const MAX_DISCOVERED_FILES_PER_REPOSITORY: usize = 100_000;
const MAX_SCAN_DEGRADATIONS: usize = 25;
const GENERATED_STATE_IGNORE_RULE: &[u8] = b".code-system-graph/";
pub(crate) const CODEGRAPH_DISABLED_CODE: &str = "codegraph_disabled";
pub(crate) const CODEGRAPH_DISABLED_MESSAGE: &str =
    "CodeGraph is disabled by the trusted process policy.";

/// Error returned while executing delivery-layer application operations.
#[derive(Debug, Error)]
pub enum ApplicationError {
    /// File input could not be read.
    #[error("failed to read `{path}`: {source}")]
    ReadFile {
        /// Path that could not be read.
        path: PathBuf,
        /// Underlying operating-system error.
        source: std::io::Error,
    },
    /// Explicit output could not be created without overwriting existing data.
    #[error("failed to create `{path}`: {source}")]
    WriteFile {
        /// Path that could not be created.
        path: PathBuf,
        /// Underlying operating-system error.
        source: std::io::Error,
    },
    /// Workspace manifest was invalid.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// HTTP boundary artifact was invalid.
    #[error(transparent)]
    HttpExtraction(#[from] HttpExtractionError),
    /// Focused package manifest was malformed.
    #[error(transparent)]
    PackageManifest(#[from] PackageManifestError),
    /// Generated-client metadata was malformed or unsafe.
    #[error(transparent)]
    GeneratedClient(#[from] GeneratedClientError),
    /// GraphQL contract or persisted-operation metadata was malformed.
    #[error(transparent)]
    Graphql(#[from] GraphqlExtractionError),
    /// `AsyncAPI` event contract was malformed or unsupported.
    #[error(transparent)]
    Event(#[from] EventExtractionError),
    /// Protobuf contract was malformed or internally inconsistent.
    #[error(transparent)]
    Protobuf(#[from] ProtobufExtractionError),
    /// Database or SQL artifact was malformed or unsupported.
    #[error(transparent)]
    Data(#[from] DataExtractionError),
    /// Infrastructure or deployment artifact was malformed or unsupported.
    #[error(transparent)]
    Infrastructure(#[from] InfrastructureExtractionError),
    /// Documentation, catalog, or ownership artifact was malformed.
    #[error(transparent)]
    Documentation(#[from] DocumentationExtractionError),
    /// Secret-safe configuration key extraction failed.
    #[error(transparent)]
    ConfigExtraction(#[from] ConfigExtractionError),
    /// Mandatory source grammar inspection failed.
    #[error(transparent)]
    SourceSyntax(#[from] SourceSyntaxError),
    /// A focused parser emitted a fact without structural syntax corroboration.
    #[error("invalid focused source observation: {0}")]
    InvalidSourceObservation(String),
    /// Incremental extractor batch state was inconsistent.
    #[error(transparent)]
    BatchPlan(#[from] BatchPlanError),
    /// Deterministic linking failed.
    #[error(transparent)]
    Link(#[from] LinkError),
    /// Exact manual relationship resolution failed.
    #[error(transparent)]
    ManualLink(#[from] ManualLinkError),
    /// Repository registry validation failed.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// Effective repository configuration could not be resolved.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Workspace manifest mutation failed.
    #[error(transparent)]
    ManifestEdit(#[from] ManifestEditError),
    /// A validated registry unexpectedly omitted a manifest alias.
    #[error("validated registry omitted repository alias `{0}`")]
    RegistryAliasMissing(String),
    /// CLI override references an unknown repository alias.
    #[error("CLI override references unknown repository alias `{0}`")]
    UnknownOverrideRepository(String),
    /// Requested workspace name differs from the manifest.
    #[error("workspace name `{requested}` does not match manifest name `{manifest}`")]
    WorkspaceNameMismatch {
        /// Name supplied by the caller.
        requested: String,
        /// Name declared by the manifest.
        manifest: String,
    },
    /// Workspace already exists in the selected registry.
    #[error("workspace `{0}` is already registered")]
    WorkspaceAlreadyExists(String),
    /// Workspace is absent from the selected registry.
    #[error("workspace `{0}` is not registered")]
    WorkspaceNotFound(String),
    /// An extractor-relevant artifact exceeds the configured byte limit.
    #[error("artifact `{path}` is {size} bytes; maximum is {maximum} bytes")]
    ArtifactTooLarge {
        /// Artifact path.
        path: PathBuf,
        /// Observed byte size.
        size: u64,
        /// Hard maximum.
        maximum: u64,
    },
    /// An extractor artifact resolves outside its repository checkout.
    #[error("artifact `{path}` resolves outside checkout `{checkout}`")]
    ArtifactOutsideCheckout {
        /// Configured artifact path.
        path: PathBuf,
        /// Canonical checkout root.
        checkout: PathBuf,
    },
    /// Repository discovery exceeded its deterministic file budget.
    #[error("repository `{repository}` contains more than {maximum} discoverable files")]
    ArtifactInventoryLimit {
        /// Repository alias.
        repository: String,
        /// Non-overridable file budget.
        maximum: usize,
    },
    /// Persistent storage failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Federated graph construction or traversal failed.
    #[error(transparent)]
    Trace(#[from] TraceError),
    /// Deterministic community analysis failed.
    #[error(transparent)]
    Community(#[from] CommunityError),
    /// Ranked search or bounded traversal inputs were invalid.
    #[error(transparent)]
    Query(#[from] QueryError),
    /// Federated impact or risk inputs were invalid.
    #[error(transparent)]
    Impact(#[from] ImpactError),
    /// Git change collection or commit-gate input was invalid.
    #[error(transparent)]
    Change(#[from] ChangeError),
    /// Semantic mapping or change-impact analysis failed.
    #[error(transparent)]
    ChangeAnalysis(#[from] ChangeAnalysisError),
    /// Public interface request or rendering failed validation.
    #[error(transparent)]
    Interface(#[from] InterfaceError),
    /// Opt-in pull-request inspection failed.
    #[error(transparent)]
    PullRequest(#[from] PullRequestError),
    /// Workspace initialization could not be completed safely.
    #[error("workspace initialization failed: {0}")]
    Initialization(String),
    /// Client requested a traversal depth outside server bounds.
    #[error("trace max_depth must be between 1 and {maximum}; received {found}")]
    InvalidTraceDepth {
        /// Requested depth.
        found: usize,
        /// Non-overridable server maximum.
        maximum: usize,
    },
}

/// Classifies application failures into stable process exit codes.
#[must_use]
pub const fn application_exit_code(error: &ApplicationError) -> ExitCode {
    match error {
        ApplicationError::Interface(source) => classify_interface_error(source),
        ApplicationError::Change(ChangeError::Cancelled)
        | ApplicationError::PullRequest(PullRequestError::Cancelled) => ExitCode::Cancelled,
        ApplicationError::Change(ChangeError::Timeout { .. })
        | ApplicationError::PullRequest(PullRequestError::Timeout) => ExitCode::Timeout,
        ApplicationError::WorkspaceNotFound(_) => ExitCode::NotFound,
        ApplicationError::WorkspaceAlreadyExists(_) => ExitCode::Conflict,
        ApplicationError::Manifest(_)
        | ApplicationError::HttpExtraction(_)
        | ApplicationError::PackageManifest(_)
        | ApplicationError::GeneratedClient(_)
        | ApplicationError::Graphql(_)
        | ApplicationError::Event(_)
        | ApplicationError::Protobuf(_)
        | ApplicationError::Data(_)
        | ApplicationError::Infrastructure(_)
        | ApplicationError::Documentation(_)
        | ApplicationError::ConfigExtraction(_)
        | ApplicationError::SourceSyntax(_)
        | ApplicationError::InvalidSourceObservation(_)
        | ApplicationError::BatchPlan(_)
        | ApplicationError::Link(_)
        | ApplicationError::ManualLink(_)
        | ApplicationError::Registry(_)
        | ApplicationError::Config(_)
        | ApplicationError::ManifestEdit(_)
        | ApplicationError::UnknownOverrideRepository(_)
        | ApplicationError::WorkspaceNameMismatch { .. }
        | ApplicationError::ArtifactTooLarge { .. }
        | ApplicationError::ArtifactOutsideCheckout { .. }
        | ApplicationError::ArtifactInventoryLimit { .. }
        | ApplicationError::Trace(_)
        | ApplicationError::Community(_)
        | ApplicationError::Query(_)
        | ApplicationError::Impact(_)
        | ApplicationError::Change(_)
        | ApplicationError::ChangeAnalysis(_)
        | ApplicationError::PullRequest(_)
        | ApplicationError::InvalidTraceDepth { .. } => ExitCode::InvalidInput,
        ApplicationError::ReadFile { .. }
        | ApplicationError::WriteFile { .. }
        | ApplicationError::RegistryAliasMissing(_)
        | ApplicationError::Store(_)
        | ApplicationError::Initialization(_) => ExitCode::Internal,
    }
}

/// Observable result of a successful scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScanSummary {
    /// Workspace name.
    pub workspace: String,
    /// Published snapshot identifier.
    pub snapshot_id: String,
    /// Number of boundary nodes.
    pub node_count: usize,
    /// Number of confirmed cross-repository links.
    pub edge_count: usize,
    /// Number of direct evidence records.
    pub evidence_count: usize,
    /// Number of deterministic communities published with the graph.
    pub community_count: usize,
    /// Number of material community changes from the previous snapshot.
    pub community_delta_count: usize,
    /// Number of extractor-relevant files discovered.
    pub discovered_input_count: usize,
    /// Number of added, modified, or deleted inputs.
    pub changed_input_count: usize,
    /// Whether the current snapshot was reused without extractor execution.
    pub reused_snapshot: bool,
    /// Number of exact source symbols corroborated by optional local intelligence.
    pub corroborated_symbol_count: usize,
    /// Number of bounded affected-test paths reported by optional local intelligence.
    pub affected_test_count: usize,
    /// Total number of unique degradations before response truncation.
    pub degradation_count: usize,
    /// Explicit optional-provider degradation messages.
    pub degradations: Vec<String>,
}

/// Highest-precedence command-line scan overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanOverrides {
    /// Optional requested workspace name, verified against the manifest before mutation.
    pub workspace: Option<String>,
    /// Per-repository `OpenAPI` artifact overrides keyed by alias.
    pub repo_openapi: BTreeMap<String, String>,
    /// Enable best-effort public `CodeGraph` corroboration.
    pub codegraph: bool,
    /// Optional explicit `CodeGraph` executable path.
    pub codegraph_binary: Option<PathBuf>,
    /// Restrict extractor work to one registered alias while reusing other repository batches.
    pub repository: Option<String>,
    /// Recompute selected extractor batches even when fingerprints are unchanged.
    pub force: bool,
}

/// Action performed by one effective repository discovery rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IgnoreRuleAction {
    /// Omit matching paths from automatic discovery.
    Exclude,
    /// Re-enable paths otherwise matched by a built-in default exclusion.
    Include,
}

/// Origin of one effective repository discovery rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IgnoreRuleSource {
    /// Non-overridable generated-state or version-control protection.
    Protected,
    /// Reactivable rule compiled into the binary.
    BuiltInDefault,
    /// Rule selected from the workspace manifest.
    WorkspaceManifest,
    /// Rule selected from repository-local configuration.
    RepositoryLocal,
}

/// One ordered effective discovery rule returned by `config show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct EffectiveIgnoreRule {
    /// Rule action.
    pub action: IgnoreRuleAction,
    /// Rule origin.
    pub source: IgnoreRuleSource,
    /// Portable repository-relative glob.
    pub pattern: String,
}

/// Configured pattern list and the precedence layer that selected it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfiguredPatterns {
    /// Selected configuration layer.
    pub source: ConfigSource,
    /// Canonical configured patterns in deterministic order.
    pub patterns: Vec<String>,
}

/// Complete observable ignore policy for one registered repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IgnorePolicyReport {
    /// Non-overridable exclusions compiled into the binary.
    pub protected_excludes: Vec<String>,
    /// Reactivable exclusions compiled into the binary.
    pub default_excludes: Vec<String>,
    /// Additional exclusions selected from configuration.
    pub configured_excludes: ConfiguredPatterns,
    /// Exceptions to built-in default exclusions selected from configuration.
    pub include_defaults: ConfiguredPatterns,
    /// Rules ordered from lowest to highest precedence.
    pub effective_rules: Vec<EffectiveIgnoreRule>,
}

/// Effective configuration report for one repository alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepositoryConfigReport {
    /// Workspace manifest alias.
    pub alias: String,
    /// Canonical checkout display path.
    pub path: String,
    /// Effective native automatic-discovery exclusions.
    pub ignore_policy: IgnorePolicyReport,
}

/// Versioned read-only result returned by `csgraph config show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigReport {
    /// Output schema version.
    pub schema_version: u8,
    /// Workspace name from the manifest.
    pub workspace: String,
    /// Effective per-repository configuration in alias order.
    pub repositories: Vec<RepositoryConfigReport>,
}

/// Current workspace registry and snapshot health.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceStatus {
    /// Workspace name.
    pub workspace: String,
    /// Applied `SQLite` schema version.
    pub schema_version: i64,
    /// Result of the latest quick integrity check.
    pub integrity_ok: bool,
    /// Aggregate conservative freshness.
    pub freshness: FreshnessSummary,
    /// Per-checkout freshness in deterministic order.
    pub repositories: Vec<RepoFreshness>,
}

/// Result of an explicit database backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BackupSummary {
    /// Source database display path.
    pub database: String,
    /// Created backup display path.
    pub backup: String,
}

/// Planned or completed database migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MigrationSummary {
    /// Schema version before migration.
    pub from_version: i64,
    /// Schema version required by this binary.
    pub to_version: i64,
    /// Automatic backup created before migration.
    pub backup: Option<String>,
    /// Whether migrations were applied.
    pub applied: bool,
    /// Whether this was a dry run.
    pub dry_run: bool,
}

/// Result of an explicit database restore.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RestoreSummary {
    /// Selected backup source display path.
    pub source: String,
    /// Safety backup of the replaced database.
    pub safety_backup: Option<String>,
    /// Schema version after restore.
    pub schema_version: i64,
}

/// Compact workspace registry item returned by CLI listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceRegistryItem {
    /// Stable workspace identifier.
    pub id: String,
    /// User-facing workspace name.
    pub name: String,
    /// Current merged manifest fingerprint.
    pub manifest_hash: String,
    /// Canonical manifest display path.
    pub config_path: Option<String>,
    /// Number of registered repository aliases.
    pub repository_count: usize,
}

/// Preview or result of a constrained repository manifest mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ManifestMutationSummary {
    /// Human-readable operation summary.
    pub summary: String,
    /// Manifest display path.
    pub manifest: String,
    /// Backup created before an applied mutation.
    pub backup: Option<String>,
    /// Whether the mutation was committed.
    pub applied: bool,
    /// Complete proposed manifest for dry-run review.
    pub rendered_manifest: Option<String>,
}

/// Result of adding or removing a persisted workspace registration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceMutationSummary {
    /// Workspace name.
    pub workspace: String,
    /// Canonical manifest display path when adding.
    pub config_path: Option<String>,
    /// Number of registered repository aliases before removal or after addition.
    pub repository_count: usize,
    /// Mutation kind.
    pub operation: String,
}

/// Input contract shared by CLI and MCP trace delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TraceInput {
    /// Stable source node identifier.
    pub from: String,
    /// Stable target node identifier.
    pub to: String,
    /// Maximum number of traversed edges.
    #[serde(default = "default_max_depth")]
    #[schemars(range(min = 1, max = 32))]
    pub max_depth: usize,
}

/// Safe ranked-search input shared by CLI and MCP delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchInput {
    /// Text matched against graph identities and the current FTS index.
    pub query: String,
    /// Optional entity-kind filter.
    #[serde(default)]
    pub node_kinds: Vec<NodeKind>,
    /// Optional repository filter.
    #[serde(default)]
    pub repo_ids: Vec<RepoId>,
    /// Optional exact service node scopes.
    #[serde(default)]
    pub service_ids: Vec<NodeId>,
    /// Optional exact community scopes.
    #[serde(default)]
    pub community_ids: Vec<CommunityId>,
    /// Zero-based result offset.
    #[serde(default)]
    pub offset: usize,
    /// Bounded page size.
    #[serde(default = "default_search_limit")]
    #[schemars(range(min = 1, max = 100))]
    pub limit: usize,
}

/// Read-only request for one deterministic community page or snapshot comparison.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CommunityInput {
    /// Optional exact community identity.
    pub community_id: Option<CommunityId>,
    /// Optional historical snapshot to compare against the current snapshot.
    pub compare_snapshot_id: Option<String>,
    /// Zero-based community offset.
    #[serde(default)]
    pub offset: usize,
    /// Bounded page size.
    #[serde(default = "default_community_limit")]
    #[schemars(range(min = 1, max = 100))]
    pub limit: usize,
}

/// Paginated and optionally comparative community result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CommunityReport {
    /// Current graph snapshot identity.
    pub snapshot_id: String,
    /// Community engine semantic version.
    pub engine_version: String,
    /// Reproducibility configuration.
    pub config: CommunityConfig,
    /// Selected communities in deterministic relevance order.
    pub communities: Vec<Community>,
    /// Total communities before pagination.
    pub total_communities: usize,
    /// Applied zero-based offset.
    pub offset: usize,
    /// Applied page size.
    pub limit: usize,
    /// Whether more communities remain.
    pub truncated: bool,
    /// Optional material delta from the requested historical snapshot.
    pub delta: Option<CommunityDelta>,
}

/// Read-only local change collection input shared by CLI and MCP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangesInput {
    /// Registered repository alias.
    pub repository: String,
    /// Local Git state or committed comparison to inspect.
    pub scope: ChangeScope,
}

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
    #[serde(default = "default_explore_max_files")]
    #[schemars(range(min = 1, max = 25))]
    pub max_files: usize,
}

const fn default_explore_max_files() -> usize {
    12
}

/// Explicit remote pull-request inspection input shared by CLI and MCP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestInput {
    /// Public provider contract.
    pub provider: PullRequestProviderKind,
    /// GitHub owner or Bitbucket workspace.
    pub owner: String,
    /// GitHub repository or Bitbucket repository slug.
    pub repository: String,
    /// Provider-native pull-request number.
    pub number: u64,
    /// Per-request confirmation that remote access is permitted.
    pub consent_to_remote_access: bool,
}

/// Explicit hosted pull-request list input shared by delivery adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestListInput {
    /// Public provider contract.
    pub provider: PullRequestProviderKind,
    /// GitHub owner or Bitbucket workspace.
    pub owner: String,
    /// GitHub repository or Bitbucket repository slug.
    pub repository: String,
    /// Provider-native lifecycle filter.
    pub state: PullRequestListState,
    /// Opaque positive page cursor returned by a prior response.
    pub cursor: Option<String>,
    /// Maximum number of source-free summaries.
    pub limit: usize,
    /// Per-request confirmation that remote access is permitted.
    pub consent_to_remote_access: bool,
}

/// Result of creating a new strict workspace manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct InitReport {
    /// Created manifest path.
    pub manifest_path: PathBuf,
    /// Workspace-local Git ignore file when the workspace belongs to a Git worktree.
    pub gitignore_path: Option<PathBuf>,
    /// Whether initialization added the generated-state rule.
    pub gitignore_updated: bool,
    /// Validated workspace name.
    pub workspace: String,
    /// Manifest contract version.
    pub manifest_version: u32,
}

/// Explicit source-free diagnostic bundle suitable for local support handoff.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiagnosticBundle {
    /// Public bundle schema version.
    pub schema_version: u32,
    /// `Code System Graph` binary version.
    pub binary_version: String,
    /// Operating-system target reported by the running binary.
    pub operating_system: String,
    /// CPU architecture reported by the running binary.
    pub architecture: String,
    /// Bundle generation time as Unix milliseconds.
    pub generated_at_unix_ms: u128,
    /// Whether extended local diagnostics were explicitly requested through the environment.
    pub debug_requested: bool,
    /// Conservative source-free doctor report.
    pub doctor: DoctorReport,
}

struct WorkspaceContext {
    manifest: WorkspaceManifest,
    registry: RegisteredWorkspace,
    repository_configs: BTreeMap<String, EffectiveRepositoryConfig>,
}

/// Resolves and reports native discovery exclusions without opening a graph database.
///
/// # Errors
///
/// Returns [`ApplicationError`] when the manifest, repository-local configuration, checkout, or
/// optional repository selection is invalid.
pub fn show_config(
    config_path: &Path,
    selected_repository: Option<&str>,
) -> Result<ConfigReport, ApplicationError> {
    let context = load_workspace_context(config_path, &ScanOverrides::default())?;
    if let Some(selected) = selected_repository
        && !context.manifest.repos.contains_key(selected)
    {
        return Err(ApplicationError::UnknownOverrideRepository(
            selected.to_owned(),
        ));
    }
    let repositories = context
        .manifest
        .repos
        .keys()
        .filter(|alias| selected_repository.is_none_or(|selected| selected == alias.as_str()))
        .map(|alias| {
            let path = context
                .registry
                .checkout_path(alias)
                .ok_or_else(|| ApplicationError::RegistryAliasMissing(alias.clone()))?;
            let effective = context
                .repository_configs
                .get(alias)
                .ok_or_else(|| ApplicationError::RegistryAliasMissing(alias.clone()))?;
            Ok(RepositoryConfigReport {
                alias: alias.clone(),
                path: path.display().to_string(),
                ignore_policy: ignore_policy_report(&effective.ignore_policy),
            })
        })
        .collect::<Result<Vec<_>, ApplicationError>>()?;
    Ok(ConfigReport {
        schema_version: 1,
        workspace: context.manifest.name,
        repositories,
    })
}

fn ignore_policy_report(policy: &IgnorePolicy) -> IgnorePolicyReport {
    let configured_source = rule_source(policy.configured_excludes_source());
    let include_source = rule_source(policy.include_defaults_source());
    let mut effective_rules = DEFAULT_EXCLUDES
        .iter()
        .map(|pattern| EffectiveIgnoreRule {
            action: IgnoreRuleAction::Exclude,
            source: IgnoreRuleSource::BuiltInDefault,
            pattern: (*pattern).to_owned(),
        })
        .chain(
            policy
                .include_defaults()
                .iter()
                .map(|pattern| EffectiveIgnoreRule {
                    action: IgnoreRuleAction::Include,
                    source: include_source,
                    pattern: pattern.clone(),
                }),
        )
        .chain(
            policy
                .configured_excludes()
                .iter()
                .map(|pattern| EffectiveIgnoreRule {
                    action: IgnoreRuleAction::Exclude,
                    source: configured_source,
                    pattern: pattern.clone(),
                }),
        )
        .collect::<Vec<_>>();
    effective_rules.extend(
        PROTECTED_EXCLUDES
            .iter()
            .map(|pattern| EffectiveIgnoreRule {
                action: IgnoreRuleAction::Exclude,
                source: IgnoreRuleSource::Protected,
                pattern: (*pattern).to_owned(),
            }),
    );
    IgnorePolicyReport {
        protected_excludes: PROTECTED_EXCLUDES
            .iter()
            .map(|pattern| (*pattern).to_owned())
            .collect(),
        default_excludes: DEFAULT_EXCLUDES
            .iter()
            .map(|pattern| (*pattern).to_owned())
            .collect(),
        configured_excludes: ConfiguredPatterns {
            source: policy.configured_excludes_source(),
            patterns: policy.configured_excludes().to_vec(),
        },
        include_defaults: ConfiguredPatterns {
            source: policy.include_defaults_source(),
            patterns: policy.include_defaults().to_vec(),
        },
        effective_rules,
    }
}

const fn rule_source(source: ConfigSource) -> IgnoreRuleSource {
    match source {
        ConfigSource::WorkspaceManifest => IgnoreRuleSource::WorkspaceManifest,
        ConfigSource::RepositoryLocal => IgnoreRuleSource::RepositoryLocal,
        ConfigSource::CliOverride | ConfigSource::AutoDetected | ConfigSource::Default => {
            IgnoreRuleSource::BuiltInDefault
        }
    }
}

struct GraphAssembly {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    evidence: Vec<Evidence>,
    link_decisions: Vec<LinkDecision>,
    link_node_keys: BTreeMap<NodeId, String>,
}

struct FocusedBatchState {
    source_batches: Vec<ExtractorBatch<SourceObservation>>,
    previous_source_batches: Vec<ExtractorBatch<SourceObservation>>,
    package_batches: Vec<ExtractorBatch<PackageManifest>>,
    generated_client_batches: Vec<ExtractorBatch<GeneratedClientMetadata>>,
    graphql_batches: Vec<ExtractorBatch<GraphqlDocument>>,
    event_batches: Vec<ExtractorBatch<EventDocument>>,
    protobuf_batches: Vec<ExtractorBatch<ProtobufDocument>>,
    data_batches: Vec<ExtractorBatch<DataDocument>>,
    infrastructure_batches: Vec<ExtractorBatch<InfrastructureDocument>>,
    documentation_batches: Vec<ExtractorBatch<DocumentationDocument>>,
    config_batches: Vec<ExtractorBatch<SafeConfigDocument>>,
    stored_batches: Vec<StoredExtractorBatch>,
    degradations: Vec<String>,
    force_relink: bool,
}

struct RepositoryCorroboration {
    repo_id: RepoId,
    report: CorroborationReport,
}

#[derive(Default)]
struct CorroborationSummary {
    reports: Vec<RepositoryCorroboration>,
    confirmed_symbols: usize,
    affected_tests: usize,
    degradations: Vec<String>,
}

fn default_max_depth() -> usize {
    8
}

fn current_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(u64::MAX)
}

fn default_search_limit() -> usize {
    20
}

fn default_community_limit() -> usize {
    20
}

fn default_community_config() -> CommunityConfig {
    CommunityConfig {
        algorithm: CommunityAlgorithm::Louvain,
        scope: CommunityScope::Federated,
        seed: 0,
        resolution: 1.0,
        minimum_confidence: 0.5,
        edge_weights: Vec::new(),
        max_iterations: 100,
    }
}

/// Creates a strict minimal workspace manifest without overwriting existing files.
///
/// # Errors
///
/// When the workspace belongs to a Git worktree, generated local state is excluded through an
/// idempotent `.gitignore` update that preserves all existing bytes and rules. No `.gitignore` is
/// created for an unversioned directory that only contains child repositories.
///
/// Returns [`ApplicationError`] when the directory cannot be created, the destination already
/// exists, the derived name is invalid, or the manifest and ignore rule cannot be durably written.
pub fn initialize_workspace(
    directory: &Path,
    requested_name: Option<&str>,
) -> Result<InitReport, ApplicationError> {
    fs::create_dir_all(directory).map_err(|error| {
        ApplicationError::Initialization(format!(
            "cannot create `{}`: {error}",
            directory.display()
        ))
    })?;
    let workspace = requested_name.map_or_else(
        || {
            directory
                .file_name()
                .and_then(OsStr::to_str)
                .filter(|name| !name.is_empty())
                .unwrap_or("workspace")
                .to_owned()
        },
        str::to_owned,
    );
    let encoded_name = serde_json::to_string(&workspace)
        .map_err(|error| ApplicationError::Initialization(error.to_string()))?;
    let source = format!("version: 1\nname: {encoded_name}\nrepos:\n  root:\n    path: .\n");
    let manifest = parse_manifest(&source)?;
    let manifest_path = directory.join("code-system-graph.yaml");
    let mut destination = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&manifest_path)
        .map_err(|error| {
            ApplicationError::Initialization(format!(
                "cannot create `{}` without overwriting: {error}",
                manifest_path.display()
            ))
        })?;
    destination.write_all(source.as_bytes()).map_err(|error| {
        ApplicationError::Initialization(format!(
            "cannot write `{}`: {error}",
            manifest_path.display()
        ))
    })?;
    destination.sync_all().map_err(|error| {
        ApplicationError::Initialization(format!(
            "cannot synchronize `{}`: {error}",
            manifest_path.display()
        ))
    })?;
    drop(destination);
    let (gitignore_path, gitignore_updated) = configure_generated_state_ignore(directory)
        .inspect_err(|_| {
            let _ = fs::remove_file(&manifest_path);
        })?;
    Ok(InitReport {
        manifest_path,
        gitignore_path,
        gitignore_updated,
        workspace: manifest.name,
        manifest_version: manifest.version,
    })
}

fn configure_generated_state_ignore(
    directory: &Path,
) -> Result<(Option<PathBuf>, bool), ApplicationError> {
    let canonical_directory = fs::canonicalize(directory).map_err(|error| {
        ApplicationError::Initialization(format!(
            "cannot resolve workspace directory `{}`: {error}",
            directory.display()
        ))
    })?;
    if !belongs_to_git_worktree(&canonical_directory) {
        return Ok((None, false));
    }
    let (path, updated) = ensure_generated_state_ignored(directory)?;
    Ok((Some(path), updated))
}

fn belongs_to_git_worktree(directory: &Path) -> bool {
    directory
        .ancestors()
        .any(|ancestor| ancestor.join(".git").exists())
}

fn ensure_generated_state_ignored(directory: &Path) -> Result<(PathBuf, bool), ApplicationError> {
    let gitignore_path = directory.join(".gitignore");
    let mut content = match fs::read(&gitignore_path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            return Err(ApplicationError::Initialization(format!(
                "cannot read `{}` before updating generated-state rules: {error}",
                gitignore_path.display()
            )));
        }
    };
    if generated_state_is_ignored(&content) {
        return Ok((gitignore_path, false));
    }
    if !content.is_empty() && !content.ends_with(b"\n") {
        content.push(b'\n');
    }
    content.extend_from_slice(GENERATED_STATE_IGNORE_RULE);
    content.push(b'\n');

    let mut destination = AtomicWriteFile::open(&gitignore_path).map_err(|error| {
        ApplicationError::Initialization(format!(
            "cannot prepare `{}` for generated-state rules: {error}",
            gitignore_path.display()
        ))
    })?;
    destination
        .write_all(&content)
        .and_then(|()| destination.sync_all())
        .map_err(|error| {
            ApplicationError::Initialization(format!(
                "cannot write `{}`: {error}",
                gitignore_path.display()
            ))
        })?;
    destination.commit().map_err(|error| {
        ApplicationError::Initialization(format!(
            "cannot commit `{}`: {error}",
            gitignore_path.display()
        ))
    })?;
    Ok((gitignore_path, true))
}

fn generated_state_is_ignored(content: &[u8]) -> bool {
    content
        .split(|byte| *byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").map_or(line, |trimmed| trimmed))
        .fold(None, |state, line| match line {
            b".code-system-graph/"
            | b"/.code-system-graph/"
            | b".code-system-graph"
            | b"/.code-system-graph" => Some(true),
            b"!.code-system-graph/"
            | b"!/.code-system-graph/"
            | b"!.code-system-graph"
            | b"!/.code-system-graph" => Some(false),
            _ => state,
        })
        .unwrap_or(false)
}

/// Scans the initial HTTP vertical slice and publishes one atomic snapshot.
///
/// # Errors
///
/// Returns [`ApplicationError`] for unreadable inputs, invalid contracts, ambiguous links, or
/// storage failure.
pub fn scan_workspace(
    config_path: &Path,
    database_path: &Path,
) -> Result<ScanSummary, ApplicationError> {
    scan_workspace_with_overrides(config_path, database_path, &ScanOverrides::default())
}

/// Scans with explicit highest-precedence command-line overrides.
///
/// # Errors
///
/// Returns [`ApplicationError`] for unknown aliases, invalid overrides, unreadable inputs,
/// invalid contracts, ambiguous links, or storage failure.
#[expect(
    clippy::too_many_lines,
    reason = "Atomic scan orchestration keeps lock, reuse, publication, and summary sequencing visible"
)]
pub fn scan_workspace_with_overrides(
    config_path: &Path,
    database_path: &Path,
    overrides: &ScanOverrides,
) -> Result<ScanSummary, ApplicationError> {
    let context = load_workspace_context(config_path, overrides)?;
    if let Some(requested) = &overrides.workspace
        && requested != &context.manifest.name
    {
        return Err(ApplicationError::WorkspaceNameMismatch {
            requested: requested.clone(),
            manifest: context.manifest.name,
        });
    }
    let mut fingerprints = discover_artifact_fingerprints(&context)?;
    let _writer_lock = StoreLock::acquire(database_path, Duration::from_mins(5))?;
    let mut store = SqliteStore::open(database_path)?;
    let mut previous_fingerprints =
        match store.load_current_artifact_fingerprints(&context.manifest.name) {
            Ok(previous) => previous,
            Err(StoreError::CurrentSnapshotMissing(_)) => Vec::new(),
            Err(error) => return Err(error.into()),
        };
    if let Some(alias) = &overrides.repository {
        let selected = context
            .registry
            .record
            .repositories
            .iter()
            .find(|repository| &repository.alias == alias)
            .ok_or_else(|| ApplicationError::UnknownOverrideRepository(alias.clone()))?
            .id
            .clone();
        fingerprints.retain(|fingerprint| fingerprint.repo_id == selected);
        fingerprints.extend(
            previous_fingerprints
                .iter()
                .filter(|fingerprint| fingerprint.repo_id != selected)
                .cloned(),
        );
        fingerprints.sort_by_key(|fingerprint| ArtifactKey::from(fingerprint));
        if overrides.force {
            previous_fingerprints.retain(|fingerprint| fingerprint.repo_id != selected);
        }
    } else if overrides.force {
        previous_fingerprints.clear();
    }
    let fingerprint_hashes = fingerprints
        .iter()
        .map(|fingerprint| fingerprint.content_hash.as_str())
        .collect::<Vec<_>>()
        .join(":");
    let snapshot_id = stable_id(
        "snapshot",
        &format!(
            "{}:{fingerprint_hashes}:community-engine-v1",
            context.registry.record.manifest_hash
        ),
    );
    let previous_extractor_batches =
        match store.load_current_extractor_batches(&context.manifest.name) {
            Ok(previous) => previous,
            Err(StoreError::CurrentSnapshotMissing(_)) => Vec::new(),
            Err(error) => return Err(error.into()),
        };
    let previous_graph = match store.load_current_graph(&context.manifest.name) {
        Ok(graph) => graph,
        Err(StoreError::CurrentSnapshotMissing(_)) => (Vec::new(), Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let previous_communities = match store.load_current_community_snapshot(&context.manifest.name) {
        Ok(snapshot) => Some(snapshot),
        Err(StoreError::CurrentSnapshotMissing(_) | StoreError::CommunitySnapshotMissing(_)) => {
            None
        }
        Err(error) => return Err(error.into()),
    };
    let previous_manifest_matches = match store.load_workspace_registry(&context.manifest.name) {
        Ok(previous) => previous.manifest_hash == context.registry.record.manifest_hash,
        Err(StoreError::RegistryIncomplete(_)) => false,
        Err(error) => return Err(error.into()),
    };
    let plan = plan_incremental_scan(&previous_fingerprints, &fingerprints);
    if previous_manifest_matches
        && !plan.has_changes()
        && focused_batch_cache_complete(&fingerprints, &previous_extractor_batches)
        && previous_communities.is_some()
        && !overrides.codegraph
    {
        let current = store.current_snapshot_summary(&context.manifest.name)?;
        let (degradation_count, degradations) =
            finalize_scan_degradations(stored_batch_degradations(&previous_extractor_batches)?);
        return Ok(ScanSummary {
            workspace: context.manifest.name,
            snapshot_id: current.snapshot_id,
            node_count: current.node_count,
            edge_count: current.edge_count,
            evidence_count: current.evidence_count,
            community_count: previous_communities
                .as_ref()
                .map_or(0, |snapshot| snapshot.communities.len()),
            community_delta_count: 0,
            discovered_input_count: fingerprints.len(),
            changed_input_count: 0,
            reused_snapshot: true,
            corroborated_symbol_count: 0,
            affected_test_count: 0,
            degradation_count,
            degradations,
        });
    }

    let batch_plan = plan_extractor_batches(&plan);
    let focused_batches = assemble_focused_batches(
        &context,
        &fingerprints,
        &previous_extractor_batches,
        &batch_plan,
    )?;
    let mut graph = assemble_graph(&context, &fingerprints, &focused_batches)?;
    relink_affected_graph(
        &mut graph,
        &plan,
        &batch_plan,
        &focused_batches,
        &previous_graph.0,
        &previous_graph.1,
    )?;
    let corroboration = if overrides.codegraph {
        run_codegraph_corroboration(
            &context,
            &focused_batches,
            &plan,
            overrides.codegraph_binary.clone(),
        )
    } else {
        CorroborationSummary::default()
    };
    apply_codegraph_corroboration(&mut graph, &corroboration.reports);
    let manual_links =
        resolve_manual_links(&context.manifest.manual_links, &graph.nodes, &graph.edges)?;
    graph.edges = manual_links.edges;
    graph.evidence.extend(manual_links.evidence);
    graph.evidence.sort_by(|left, right| left.id.cmp(&right.id));
    graph.evidence.dedup_by(|left, right| left.id == right.id);
    graph.link_decisions = manual_links.decisions;
    let GraphAssembly {
        nodes,
        edges,
        evidence,
        link_decisions,
        ..
    } = graph;
    let community_config = default_community_config();
    let community_snapshot = previous_communities
        .as_ref()
        .filter(|previous| {
            previous.snapshot_id == snapshot_id
                && previous.config == community_config
                && community_topology_unchanged(
                    &previous_graph.0,
                    &previous_graph.1,
                    &nodes,
                    &edges,
                )
        })
        .cloned()
        .map_or_else(
            || analyze_communities(&snapshot_id, &nodes, &edges, community_config.clone()),
            Ok,
        )?;
    let community_delta_count = previous_communities.as_ref().map_or(0, |previous| {
        compare_community_snapshots(previous, &community_snapshot)
            .changes
            .len()
    });
    let extractor_runs = extractor_runs(&snapshot_id, &fingerprints, &plan);
    let manual_link_records = persisted_manual_link_records(&snapshot_id, &link_decisions)?;
    store.clear_query_cache(&context.manifest.name)?;
    store.publish_snapshot(SnapshotBatch {
        workspace: &context.registry.record,
        snapshot_id: &snapshot_id,
        nodes: &nodes,
        edges: &edges,
        evidence: &evidence,
        fingerprints: &fingerprints,
        extractor_batches: &focused_batches.stored_batches,
        extractor_runs: &extractor_runs,
        manual_links: &manual_link_records,
        community_snapshot: Some(&community_snapshot),
    })?;
    let mut degradations = focused_batches.degradations.clone();
    degradations.extend(corroboration.degradations);
    for item in &corroboration.reports {
        let Some(capability) = &item.report.capability else {
            continue;
        };
        let record = provider_capability_record(&context.manifest.name, &item.repo_id, capability);
        if let Err(error) = store.upsert_provider_capabilities(&record) {
            degradations.push(format!(
                "CodeGraph capability persistence for `{}` degraded: {error}",
                item.repo_id.as_str()
            ));
        }
    }
    degradations.extend(stored_batch_degradations(&focused_batches.stored_batches)?);
    let (degradation_count, degradations) = finalize_scan_degradations(degradations);
    Ok(ScanSummary {
        workspace: context.manifest.name,
        snapshot_id,
        node_count: nodes.len(),
        edge_count: edges.len(),
        evidence_count: evidence.len(),
        community_count: community_snapshot.communities.len(),
        community_delta_count,
        discovered_input_count: fingerprints.len(),
        changed_input_count: plan.changed_count(),
        reused_snapshot: false,
        corroborated_symbol_count: corroboration.confirmed_symbols,
        affected_test_count: corroboration.affected_tests,
        degradation_count,
        degradations,
    })
}

/// Traces the current stored graph and returns a versioned conservative envelope.
///
/// # Errors
///
/// Returns [`ApplicationError`] for storage, graph validation, or invalid anchor failures.
pub fn trace_workspace(
    database_path: &Path,
    workspace: &str,
    input: &TraceInput,
) -> Result<ToolEnvelope<TraceReport>, ApplicationError> {
    if !(1..=MAX_TRACE_DEPTH).contains(&input.max_depth) {
        return Err(ApplicationError::InvalidTraceDepth {
            found: input.max_depth,
            maximum: MAX_TRACE_DEPTH,
        });
    }
    let store = SqliteStore::open_read_only(database_path)?;
    let (nodes, edges) = store.load_current_graph(workspace)?;
    let freshness = freshness_summary(&store.load_current_freshness(workspace)?);
    let graph = FederatedGraph::new(nodes, edges)?;
    let report = graph.trace(
        &NodeId::new(&input.from),
        &NodeId::new(&input.to),
        input.max_depth,
    )?;
    let status = if report.coverage_gaps.is_empty() && freshness.overall == OverallFreshness::Fresh
    {
        ToolStatus::Ok
    } else {
        ToolStatus::Degraded
    };
    Ok(ToolEnvelope {
        schema_version: 1,
        status,
        data: Some(report),
        freshness,
        warnings: Vec::new(),
    })
}

/// Searches current graph entities with structural, FTS, community, evidence, and freshness signals.
///
/// # Errors
///
/// Returns [`ApplicationError`] for invalid input, missing snapshots, malformed persistence, or
/// ranking failures.
pub fn search_workspace(
    database_path: &Path,
    workspace: &str,
    input: &SearchInput,
) -> Result<ToolEnvelope<SearchReport>, ApplicationError> {
    let store = SqliteStore::open_read_only(database_path)?;
    let snapshot = store.current_snapshot_summary(workspace)?;
    let input_json = serde_json::to_vec(input)
        .map_err(|error| ApplicationError::Initialization(error.to_string()))?;
    let input_fingerprint = stable_id_bytes("query-cache", &input_json);
    let now_unix_ms = current_unix_millis();
    if let Some(cached) = store.load_query_cache(
        workspace,
        &snapshot.snapshot_id,
        &input_fingerprint,
        now_unix_ms,
    )? && let Ok(envelope) =
        serde_json::from_slice::<ToolEnvelope<SearchReport>>(&cached.result_summary_json)
    {
        return Ok(envelope);
    }
    let (nodes, edges) = store.load_current_graph(workspace)?;
    let repository_freshness = store.load_current_freshness(workspace)?;
    let freshness = freshness_summary(&repository_freshness);
    let fts_hits = if input.query.trim().is_empty() {
        Vec::new()
    } else {
        store.search_current_nodes_ranked(workspace, &input.query, 500)?
    };
    let community_snapshot = store.load_current_community_snapshot(workspace)?;
    let evidence = store.load_current_evidence(workspace)?;
    let request = SearchRequest {
        query: input.query.clone(),
        filters: SearchFilters {
            node_kinds: input.node_kinds.clone(),
            repo_ids: input.repo_ids.clone(),
            workspace_nodes: Vec::new(),
            service_ids: input.service_ids.clone(),
            community_ids: input.community_ids.clone(),
        },
        fts_scores: fts_search_scores(&fts_hits),
        centrality_scores: community_centrality_scores(&community_snapshot.communities),
        service_memberships: service_memberships(&nodes, &edges),
        community_memberships: community_memberships(&community_snapshot.communities),
        evidence: node_evidence(&edges, &evidence),
        freshness: repository_freshness
            .iter()
            .map(|item| (item.repo_id.clone(), item.state))
            .collect(),
        offset: input.offset,
        limit: input.limit,
    };
    let report = search(&nodes, &request)?;
    let status = if report.coverage.gaps.is_empty() && freshness.overall == OverallFreshness::Fresh
    {
        ToolStatus::Ok
    } else {
        ToolStatus::Degraded
    };
    let envelope = ToolEnvelope {
        schema_version: 1,
        status,
        data: Some(report),
        freshness,
        warnings: Vec::new(),
    };
    let encoded = serde_json::to_vec(&envelope)
        .map_err(|error| ApplicationError::Initialization(error.to_string()))?;
    let envelope = serde_json::from_slice(&encoded)
        .map_err(|error| ApplicationError::Initialization(error.to_string()))?;
    drop(store);
    if let Ok(_lock) = StoreLock::acquire(database_path, Duration::from_secs(5))
        && let Ok(mut writable) = SqliteStore::open(database_path)
    {
        let _ = writable.put_query_cache(&QueryCacheRecord {
            workspace_name: workspace.to_owned(),
            snapshot_id: snapshot.snapshot_id,
            input_fingerprint,
            result_summary_json: encoded,
            stored_at_unix_ms: now_unix_ms,
            expires_at_unix_ms: None,
        });
    }
    Ok(envelope)
}

/// Executes an advanced bounded traversal against the current immutable graph.
///
/// # Errors
///
/// Returns [`ApplicationError`] for missing snapshots, invalid anchors, or bounds beyond the
/// server-enforced maximums.
pub fn traverse_workspace(
    database_path: &Path,
    workspace: &str,
    input: &TraversalRequest,
) -> Result<ToolEnvelope<TraversalReport>, ApplicationError> {
    if input.options.max_depth > 32
        || input.options.max_cross_repo_hops > 16
        || input.options.node_limit > 50_000
        || input.options.edge_limit > 250_000
        || input.options.timeout_ms > 5_000
        || input.options.k > 8
    {
        return Err(QueryError::InvalidTraversalLimits.into());
    }
    let store = SqliteStore::open_read_only(database_path)?;
    let (nodes, edges) = store.load_current_graph(workspace)?;
    let freshness = freshness_summary(&store.load_current_freshness(workspace)?);
    let report = traverse(&nodes, &edges, input)?;
    let status = if report.coverage_gaps.is_empty()
        && !report.truncated
        && freshness.overall == OverallFreshness::Fresh
    {
        ToolStatus::Ok
    } else {
        ToolStatus::Degraded
    };
    Ok(ToolEnvelope {
        schema_version: 1,
        status,
        data: Some(report),
        freshness,
        warnings: Vec::new(),
    })
}

/// Lists, inspects, or compares persisted communities for the current workspace.
///
/// # Errors
///
/// Returns [`ApplicationError`] for invalid pagination, missing snapshots, or malformed storage.
pub fn communities_workspace(
    database_path: &Path,
    workspace: &str,
    input: &CommunityInput,
) -> Result<ToolEnvelope<CommunityReport>, ApplicationError> {
    if !(1..=100).contains(&input.limit) || input.offset > 1_000_000 {
        return Err(QueryError::InvalidPagination.into());
    }
    let store = SqliteStore::open_read_only(database_path)?;
    let current = store.load_current_community_snapshot(workspace)?;
    let freshness = freshness_summary(&store.load_current_freshness(workspace)?);
    let mut selected = current
        .communities
        .iter()
        .filter(|community| {
            input
                .community_id
                .as_ref()
                .is_none_or(|id| community.id == *id)
        })
        .cloned()
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| {
        right
            .metrics
            .size
            .cmp(&left.metrics.size)
            .then_with(|| right.metrics.cohesion.total_cmp(&left.metrics.cohesion))
            .then_with(|| left.id.cmp(&right.id))
    });
    let total_communities = selected.len();
    let communities = selected
        .into_iter()
        .skip(input.offset)
        .take(input.limit)
        .collect();
    let delta = input
        .compare_snapshot_id
        .as_ref()
        .map(|snapshot_id| store.load_workspace_community_snapshot(workspace, snapshot_id))
        .transpose()?
        .map(|before| compare_community_snapshots(&before, &current));
    let exact_missing = input.community_id.is_some() && total_communities == 0;
    let truncated = input.offset.saturating_add(input.limit) < total_communities;
    let report = CommunityReport {
        snapshot_id: current.snapshot_id,
        engine_version: current.engine_version,
        config: current.config,
        communities,
        total_communities,
        offset: input.offset,
        limit: input.limit,
        truncated,
        delta,
    };
    let status = if !exact_missing && freshness.overall == OverallFreshness::Fresh {
        ToolStatus::Ok
    } else {
        ToolStatus::Degraded
    };
    Ok(ToolEnvelope {
        schema_version: 1,
        status,
        data: Some(report),
        freshness,
        warnings: exact_missing
            .then(|| "Requested community was not observed in the selected snapshot.".to_owned())
            .into_iter()
            .collect(),
    })
}

/// Explores one registered checkout through bounded, ephemeral local code intelligence.
///
/// The returned [`LocalContextResult::content`] may contain source code and must never be
/// persisted, logged, cached, or included in audit records.
pub async fn explore_repository(
    database_path: &Path,
    workspace: &str,
    input: &ExploreInput,
    binary: Option<std::ffi::OsString>,
) -> ToolEnvelope<LocalContextResult> {
    if input.workspace != workspace {
        return explore_error_envelope(format!(
            "workspace `{}` is outside this server's configured workspace `{workspace}`",
            input.workspace
        ));
    }
    if !(1..=25).contains(&input.max_files) {
        return explore_error_envelope("max_files must be between 1 and 25".to_owned());
    }

    let store = match SqliteStore::open_read_only(database_path) {
        Ok(store) => store,
        Err(error) => return explore_error_envelope(error.to_string()),
    };
    let registry = match store.load_workspace_registry(workspace) {
        Ok(registry) => registry,
        Err(error) => return explore_error_envelope(error.to_string()),
    };
    let persisted_freshness = match store.load_current_freshness(workspace) {
        Ok(freshness) => freshness,
        Err(error) => return explore_error_envelope(error.to_string()),
    };
    let freshness = freshness_summary(&persisted_freshness);
    let requested_alias = input
        .repository
        .as_deref()
        .map(str::trim)
        .filter(|alias| !alias.is_empty());
    let repository = match select_explore_repository(&registry, requested_alias, workspace) {
        Ok(repository) => repository,
        Err(message) => {
            return explore_scoped_error_envelope(freshness, ToolStatus::Error, message);
        }
    };

    let mut config = CodeGraphConfig::default();
    if let Some(binary) = binary {
        config.binary = binary;
    }
    let provider = match CodeGraphProvider::new(config) {
        Ok(provider) => provider,
        Err(error) => {
            return explore_scoped_error_envelope(
                freshness,
                ToolStatus::Degraded,
                error.to_string(),
            );
        }
    };
    let result = provider
        .build_local_context(LocalContextRequest {
            request: ProviderRequest {
                repo_id: repository.id.clone(),
                project_path: native_relative_path(&repository.canonical_path),
                budget: ProviderBudget {
                    timeout: Duration::from_secs(5),
                    max_output_bytes: 256 * 1024,
                    max_items: 25,
                },
                cancellation: CancellationToken::new(),
            },
            query: input.query.clone(),
            max_files: input.max_files,
        })
        .await;
    let shutdown = provider.shutdown().await;
    explore_result_envelope(result, shutdown, freshness)
}

fn select_explore_repository<'a>(
    registry: &'a WorkspaceRecord,
    requested_alias: Option<&str>,
    workspace: &str,
) -> Result<&'a RepositoryRecord, String> {
    if let Some(alias) = requested_alias {
        return registry
            .repositories
            .iter()
            .find(|repository| repository.alias == alias)
            .ok_or_else(|| {
                format!("repository alias `{alias}` is not registered in workspace `{workspace}`")
            });
    }
    if registry.repositories.len() == 1 {
        return registry.repositories.first().ok_or_else(|| {
            format!("workspace `{workspace}` unexpectedly has no registered repository")
        });
    }
    Err(format!(
        "repository is required because workspace `{workspace}` contains {} repositories",
        registry.repositories.len()
    ))
}

fn explore_result_envelope(
    result: Result<LocalContextResult, ProviderError>,
    shutdown: Result<(), ProviderError>,
    freshness: FreshnessSummary,
) -> ToolEnvelope<LocalContextResult> {
    match result {
        Ok(context) => {
            let mut warnings = context
                .execution
                .degradations
                .iter()
                .map(|degradation| degradation.message.clone())
                .collect::<Vec<_>>();
            if context.execution.truncated {
                warnings.push("Local context was truncated by the configured budget.".to_owned());
            }
            if let Err(error) = shutdown {
                warnings.push(format!("CodeGraph shutdown degraded: {error}"));
            }
            let status = if warnings.is_empty() && freshness.overall == OverallFreshness::Fresh {
                ToolStatus::Ok
            } else {
                ToolStatus::Degraded
            };
            ToolEnvelope {
                schema_version: 1,
                status,
                data: Some(context),
                freshness,
                warnings,
            }
        }
        Err(error) => ToolEnvelope {
            schema_version: 1,
            status: if matches!(&error, ProviderError::InvalidRequest(_)) {
                ToolStatus::Error
            } else {
                ToolStatus::Degraded
            },
            data: None,
            freshness,
            warnings: vec![error.to_string()],
        },
    }
}

fn explore_scoped_error_envelope(
    freshness: FreshnessSummary,
    status: ToolStatus,
    message: String,
) -> ToolEnvelope<LocalContextResult> {
    ToolEnvelope {
        schema_version: 1,
        status,
        data: None,
        freshness,
        warnings: vec![message],
    }
}

fn explore_error_envelope(message: String) -> ToolEnvelope<LocalContextResult> {
    ToolEnvelope {
        schema_version: 1,
        status: ToolStatus::Error,
        data: None,
        freshness: FreshnessSummary {
            overall: OverallFreshness::Unknown,
            stale_repositories: Vec::new(),
            reasons: vec!["Local repository exploration could not start.".to_owned()],
        },
        warnings: vec![message],
    }
}

/// Computes conservative upstream or downstream impact from the current immutable graph.
///
/// The default delivery path uses persisted graph, freshness, community, test, and ownership data.
/// Optional compatibility and local-provider enrichment are added by specialized callers before
/// invoking the pure core engine.
///
/// # Errors
///
/// Returns [`ApplicationError`] for missing snapshots, malformed persisted data, unknown targets,
/// dangling graph relationships, or invalid analysis bounds.
pub fn impact_workspace(
    database_path: &Path,
    workspace: &str,
    request: &ImpactRequest,
) -> Result<ToolEnvelope<ImpactReport>, ApplicationError> {
    let loaded = load_impact_context(database_path, workspace)?;
    impact_envelope(request, &loaded.context, loaded.freshness)
}

/// Computes impact with optional bounded, ephemeral `CodeGraph` local enrichment.
///
/// Provider failures, stale indexes, missing capabilities, and truncation are converted into
/// visible coverage degradation and can never lower the federated risk.
///
/// # Errors
///
/// Returns [`ApplicationError`] for persisted graph failures or invalid impact inputs. `CodeGraph`
/// operational failures degrade the report instead of failing the federated analysis.
pub async fn impact_workspace_with_codegraph(
    database_path: &Path,
    workspace: &str,
    request: &ImpactRequest,
    binary: Option<std::ffi::OsString>,
) -> Result<ToolEnvelope<ImpactReport>, ApplicationError> {
    let mut loaded = load_impact_context(database_path, workspace)?;
    let mut config = CodeGraphConfig::default();
    if let Some(binary) = binary {
        config.binary = binary;
    }
    match CodeGraphProvider::new(config) {
        Ok(provider) => {
            enrich_impact_context(
                &mut loaded.context,
                request,
                &loaded.registry,
                &loaded.node_evidence,
                &provider,
            )
            .await;
            let _ = provider.shutdown().await;
        }
        Err(error) => loaded.context.local_enrichment.push(LocalEnrichmentInput {
            repo_id: impact_target_repo(&loaded.context, request)
                .unwrap_or_else(|| RepoId::new("repo:unknown")),
            anchor: impact_target_label(&loaded.context, request)
                .unwrap_or_else(|| "unknown impact target".to_owned()),
            status: LocalEnrichmentStatus::Unavailable,
            affected: Vec::new(),
            affected_tests: Vec::new(),
            truncated: false,
            degradations: vec![error.to_string()],
        }),
    }
    impact_envelope(request, &loaded.context, loaded.freshness)
}

/// Collects a bounded fingerprinted local Git change set for one registered checkout.
///
/// This operation never stages, commits, modifies, or initializes repository state.
///
/// # Errors
///
/// Returns [`ApplicationError`] for unknown repositories, missing snapshots, invalid scopes,
/// unsafe refs/pathspecs, Git failures, cancellation, timeouts, or output-budget exhaustion.
pub async fn collect_workspace_changes(
    database_path: &Path,
    workspace: &str,
    input: &ChangesInput,
    git_binary: Option<std::ffi::OsString>,
) -> Result<ToolEnvelope<ChangeSet>, ApplicationError> {
    collect_workspace_changes_with_cancellation(
        database_path,
        workspace,
        input,
        git_binary,
        &CancellationToken::new(),
    )
    .await
}

/// Collects a local change set with caller-owned cooperative cancellation.
///
/// # Errors
///
/// Returns the same failures as [`collect_workspace_changes`], including cancellation.
pub async fn collect_workspace_changes_with_cancellation(
    database_path: &Path,
    workspace: &str,
    input: &ChangesInput,
    git_binary: Option<std::ffi::OsString>,
    cancellation: &CancellationToken,
) -> Result<ToolEnvelope<ChangeSet>, ApplicationError> {
    let store = SqliteStore::open_read_only(database_path)?;
    let registry = store.load_workspace_registry(workspace)?;
    let snapshot = store.current_snapshot_summary(workspace)?;
    let repository = registry
        .repositories
        .iter()
        .find(|repository| repository.alias == input.repository)
        .ok_or_else(|| ApplicationError::RegistryAliasMissing(input.repository.clone()))?;
    let persisted_freshness = store.load_current_freshness(workspace)?;
    let freshness = freshness_summary(&persisted_freshness);
    let provider = git_binary.map_or_else(GitCliChangeProvider::new, |binary| {
        GitCliChangeProvider::with_limits(
            binary,
            Duration::from_secs(30),
            8 * 1024 * 1024,
            64 * 1024,
        )
    });
    let mut analyzer_versions = AnalyzerVersions::new();
    analyzer_versions.insert(
        "code-system-graph.changes".to_owned(),
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    let change_set = provider
        .changes(
            &ChangeRequest {
                repo_id: repository.id.clone(),
                checkout_id: repository.checkout_id.clone(),
                worktree: native_relative_path(&repository.canonical_path),
                scope: input.scope.clone(),
                workspace_manifest_hash: registry.manifest_hash.clone(),
                contract_registry_hash: snapshot.snapshot_id,
                analyzer_versions,
            },
            cancellation,
        )
        .await?;
    let repository_is_fresh = persisted_freshness.iter().any(|item| {
        item.checkout_id == repository.checkout_id && item.state == RepoFreshnessState::Fresh
    });
    Ok(ToolEnvelope {
        schema_version: 1,
        status: if repository_is_fresh {
            ToolStatus::Ok
        } else {
            ToolStatus::Degraded
        },
        data: Some(change_set),
        freshness,
        warnings: Vec::new(),
    })
}

/// Collects local Git state and maps it to bounded semantic graph impact.
///
/// # Errors
///
/// Returns [`ApplicationError`] for change collection, storage, graph validation, or semantic
/// analysis failures.
pub async fn analyze_workspace_changes(
    database_path: &Path,
    workspace: &str,
    input: &ChangesInput,
    options: &ChangeAnalysisOptions,
    git_binary: Option<std::ffi::OsString>,
) -> Result<ToolEnvelope<ChangeImpactReport>, ApplicationError> {
    analyze_workspace_changes_with_cancellation(
        database_path,
        workspace,
        input,
        options,
        git_binary,
        &CancellationToken::new(),
    )
    .await
}

/// Analyzes local changes with caller-owned cooperative cancellation.
///
/// # Errors
///
/// Returns the same failures as [`analyze_workspace_changes`], including cancellation.
pub async fn analyze_workspace_changes_with_cancellation(
    database_path: &Path,
    workspace: &str,
    input: &ChangesInput,
    options: &ChangeAnalysisOptions,
    git_binary: Option<std::ffi::OsString>,
    cancellation: &CancellationToken,
) -> Result<ToolEnvelope<ChangeImpactReport>, ApplicationError> {
    let collected = collect_workspace_changes_with_cancellation(
        database_path,
        workspace,
        input,
        git_binary,
        cancellation,
    )
    .await?;
    let change_set = collected.data.ok_or_else(|| {
        ApplicationError::InvalidSourceObservation(
            "local change collection returned no change set".to_owned(),
        )
    })?;
    let store = SqliteStore::open_read_only(database_path)?;
    let (nodes, edges) = store.load_current_graph(workspace)?;
    let evidence = store.load_current_evidence(workspace)?;
    let community = store.load_current_community_snapshot(workspace)?;
    let freshness = store.load_current_freshness(workspace)?;
    let report = analyze_changes(
        &change_set,
        &nodes,
        &edges,
        &evidence,
        Some(&community),
        &freshness,
        &[],
        options,
    )?;
    let status = if collected.status == ToolStatus::Ok && report.coverage.complete {
        ToolStatus::Ok
    } else {
        ToolStatus::Degraded
    };
    Ok(ToolEnvelope {
        schema_version: 1,
        status,
        data: Some(report),
        freshness: collected.freshness,
        warnings: collected.warnings,
    })
}

fn pull_request_provider(
    kind: PullRequestProviderKind,
    enabled: bool,
    token: Option<String>,
    basic_auth_username: Option<String>,
) -> Result<Box<dyn PullRequestProvider>, ApplicationError> {
    if !enabled {
        return Err(PullRequestError::Disabled.into());
    }
    let mut config = match kind {
        PullRequestProviderKind::GitHub => PullRequestProviderConfig::default(),
        PullRequestProviderKind::BitbucketCloud => PullRequestProviderConfig::bitbucket_cloud(),
        PullRequestProviderKind::BitbucketDataCenter => {
            return Err(PullRequestError::InvalidConfiguration(
                "Bitbucket Data Center is not implemented in Code System Graph 1.0.0".to_owned(),
            )
            .into());
        }
    };
    config.enabled = true;
    config.auth_token = PrAuthToken::new(token.unwrap_or_default());
    if kind == PullRequestProviderKind::BitbucketCloud {
        config.basic_auth_username = basic_auth_username;
    }
    let transport = Arc::new(
        ReqwestPrHttpTransport::new()
            .map_err(|error| PullRequestError::Transport(error.to_string()))?,
    );
    match kind {
        PullRequestProviderKind::GitHub => Ok(Box::new(GitHubProvider::new(config, transport)?)),
        PullRequestProviderKind::BitbucketCloud => {
            Ok(Box::new(BitbucketProvider::new(config, transport)?))
        }
        PullRequestProviderKind::BitbucketDataCenter => unreachable!("rejected above"),
    }
}

/// Inspects one GitHub or Bitbucket Cloud pull request through the public provider API.
///
/// Both the provider-level `enabled` gate and per-request consent must be true. The token remains
/// ephemeral and is excluded from all returned data.
///
/// # Errors
///
/// Returns [`ApplicationError`] for disabled providers, missing consent, unsafe configuration,
/// cancellation, timeouts, rate limits, provider errors, malformed responses, or unsupported
/// Bitbucket Data Center requests.
pub async fn inspect_pull_request(
    input: &PullRequestInput,
    enabled: bool,
    token: Option<String>,
    basic_auth_username: Option<String>,
) -> Result<ToolEnvelope<PullRequestInspection>, ApplicationError> {
    inspect_pull_request_with_cancellation(
        input,
        enabled,
        token,
        basic_auth_username,
        CancellationToken::new(),
    )
    .await
}

/// Inspects one pull request with caller-owned cooperative cancellation.
///
/// # Errors
///
/// Returns the same failures as [`inspect_pull_request`], including cancellation.
pub async fn inspect_pull_request_with_cancellation(
    input: &PullRequestInput,
    enabled: bool,
    token: Option<String>,
    basic_auth_username: Option<String>,
    cancellation: CancellationToken,
) -> Result<ToolEnvelope<PullRequestInspection>, ApplicationError> {
    if !input.consent_to_remote_access {
        return Err(PullRequestError::ConsentRequired.into());
    }
    let provider = pull_request_provider(input.provider, enabled, token, basic_auth_username)?;
    let inspection = provider
        .inspect(PullRequestInspectRequest {
            coordinates: PullRequestCoordinates {
                provider: input.provider,
                owner: input.owner.clone(),
                repository: input.repository.clone(),
                number: input.number,
            },
            consent_to_remote_access: input.consent_to_remote_access,
            cancellation,
        })
        .await?;
    let status = if inspection.warnings.is_empty() {
        ToolStatus::Ok
    } else {
        ToolStatus::Degraded
    };
    Ok(ToolEnvelope {
        schema_version: 1,
        status,
        data: Some(inspection),
        freshness: FreshnessSummary {
            overall: OverallFreshness::Unknown,
            stale_repositories: Vec::new(),
            reasons: vec![
                "Remote pull-request inspection does not establish local graph freshness."
                    .to_owned(),
            ],
        },
        warnings: Vec::new(),
    })
}

/// Lists one bounded page of source-free hosted pull-request summaries.
///
/// # Errors
///
/// Returns [`ApplicationError`] for disabled providers, missing consent, invalid bounds or
/// cursors, cancellation, timeouts, rate limits, malformed responses, or unsupported providers.
pub async fn list_pull_requests(
    input: &PullRequestListInput,
    enabled: bool,
    token: Option<String>,
    basic_auth_username: Option<String>,
    cancellation: CancellationToken,
) -> Result<ToolEnvelope<PullRequestListPage>, ApplicationError> {
    if !input.consent_to_remote_access {
        return Err(PullRequestError::ConsentRequired.into());
    }
    let provider = pull_request_provider(input.provider, enabled, token, basic_auth_username)?;
    let page = provider
        .list(PullRequestListRequest {
            provider: input.provider,
            owner: input.owner.clone(),
            repository: input.repository.clone(),
            state: input.state,
            cursor: input.cursor.clone(),
            limit: input.limit,
            consent_to_remote_access: input.consent_to_remote_access,
            cancellation,
        })
        .await?;
    let status = if page.warnings.is_empty() && !page.truncated {
        ToolStatus::Ok
    } else {
        ToolStatus::Degraded
    };
    Ok(ToolEnvelope {
        schema_version: 1,
        status,
        data: Some(page),
        freshness: FreshnessSummary {
            overall: OverallFreshness::Unknown,
            stale_repositories: Vec::new(),
            reasons: vec![
                "Remote pull-request listing does not establish local graph freshness.".to_owned(),
            ],
        },
        warnings: Vec::new(),
    })
}

struct LoadedImpactContext {
    context: ImpactContext,
    freshness: FreshnessSummary,
    registry: WorkspaceRecord,
    node_evidence: BTreeMap<NodeId, Vec<Evidence>>,
}

fn load_impact_context(
    database_path: &Path,
    workspace: &str,
) -> Result<LoadedImpactContext, ApplicationError> {
    let store = SqliteStore::open_read_only(database_path)?;
    let (nodes, edges) = store.load_current_graph(workspace)?;
    let persisted_freshness = store.load_current_freshness(workspace)?;
    let freshness = freshness_summary(&persisted_freshness);
    let communities = store.load_current_community_snapshot(workspace)?;
    let evidence = store.load_current_evidence(workspace)?;
    let registry = store.load_workspace_registry(workspace)?;
    let node_evidence = node_evidence(&edges, &evidence);
    let context = ImpactContext {
        centrality: impact_centrality_scores(&communities.communities),
        service_memberships: service_memberships(&nodes, &edges),
        recommended_commands: recommended_test_commands(&nodes, &node_evidence),
        nodes,
        edges,
        communities: Some(communities),
        freshness: conservative_repo_freshness(&persisted_freshness),
        compatibility: Vec::new(),
        local_enrichment: Vec::new(),
        public_contracts: Vec::new(),
        criticality: Vec::new(),
        environments: Vec::new(),
        graph_complete: true,
        coverage_gaps: Vec::new(),
    };
    Ok(LoadedImpactContext {
        context,
        freshness,
        registry,
        node_evidence,
    })
}

fn impact_envelope(
    request: &ImpactRequest,
    context: &ImpactContext,
    freshness: FreshnessSummary,
) -> Result<ToolEnvelope<ImpactReport>, ApplicationError> {
    let report = analyze_impact(request, context)?;
    let status = if report.risk == code_system_graph_core::RiskLevel::Unknown
        || freshness.overall != OverallFreshness::Fresh
    {
        ToolStatus::Degraded
    } else {
        ToolStatus::Ok
    };
    Ok(ToolEnvelope {
        schema_version: 1,
        status,
        data: Some(report),
        freshness,
        warnings: Vec::new(),
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "Provider probing, impact, affected tests, and degradation form one bounded lifecycle"
)]
async fn enrich_impact_context(
    context: &mut ImpactContext,
    request: &ImpactRequest,
    registry: &WorkspaceRecord,
    evidence: &BTreeMap<NodeId, Vec<Evidence>>,
    provider: &impl LocalCodeIntelligenceProvider,
) {
    let Some(target) = impact_target_node(context, request).cloned() else {
        return;
    };
    let by_id = context
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let mut anchors = context
        .edges
        .iter()
        .filter_map(|edge| {
            let adjacent = if edge.source == target.id {
                by_id.get(&edge.target)
            } else if edge.target == target.id {
                by_id.get(&edge.source)
            } else {
                None
            };
            adjacent
                .copied()
                .filter(|node| node.kind == NodeKind::SymbolRef)
                .cloned()
        })
        .chain((target.kind == NodeKind::SymbolRef).then_some(target.clone()))
        .collect::<Vec<_>>();
    anchors.sort_by(|left, right| left.id.cmp(&right.id));
    anchors.dedup_by(|left, right| left.id == right.id);
    anchors.truncate(3);

    for anchor in anchors {
        let Some(repo_id) = anchor.repo_id.clone() else {
            continue;
        };
        let Some(repository) = registry
            .repositories
            .iter()
            .find(|repository| repository.id == repo_id)
        else {
            context.local_enrichment.push(unavailable_local_enrichment(
                repo_id,
                &anchor.label,
                "Registered checkout metadata was unavailable for local enrichment.",
            ));
            continue;
        };
        let project_path = native_relative_path(&repository.canonical_path);
        let provider_request = || ProviderRequest {
            repo_id: repo_id.clone(),
            project_path: project_path.clone(),
            budget: ProviderBudget {
                timeout: Duration::from_secs(2),
                max_output_bytes: 256 * 1024,
                max_items: 25,
            },
            cancellation: CancellationToken::new(),
        };
        let capability = match provider.probe(provider_request()).await {
            Ok(capability) => capability,
            Err(error) => {
                context.local_enrichment.push(unavailable_local_enrichment(
                    repo_id,
                    &anchor.label,
                    &error.to_string(),
                ));
                continue;
            }
        };
        if capability.status != ProviderStatus::Available {
            let status = if capability.status == ProviderStatus::Stale {
                LocalEnrichmentStatus::Stale
            } else {
                LocalEnrichmentStatus::Unavailable
            };
            let mut degradations = capability
                .degradations
                .into_iter()
                .map(|degradation| degradation.message)
                .collect::<Vec<_>>();
            degradations.extend(capability.remediation);
            context.local_enrichment.push(LocalEnrichmentInput {
                repo_id,
                anchor: anchor.label.clone(),
                status,
                affected: Vec::new(),
                affected_tests: Vec::new(),
                truncated: false,
                degradations,
            });
            continue;
        }
        let result = match provider
            .get_local_impact(LocalImpactRequest {
                request: provider_request(),
                symbol: anchor.label.clone(),
                max_depth: request.options.max_depth.min(8),
            })
            .await
        {
            Ok(result) => result,
            Err(error) => {
                context.local_enrichment.push(unavailable_local_enrichment(
                    repo_id,
                    &anchor.label,
                    &error.to_string(),
                ));
                continue;
            }
        };
        let changed_files = evidence
            .get(&anchor.id)
            .into_iter()
            .flatten()
            .filter_map(|item| item.file_path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let affected_tests = if changed_files.is_empty() {
            None
        } else {
            provider
                .get_affected_tests(AffectedTestsRequest {
                    request: provider_request(),
                    changed_files,
                    max_depth: request.options.max_depth.min(8),
                })
                .await
                .ok()
                .flatten()
        };
        let mut degradations = result
            .execution
            .degradations
            .iter()
            .map(|degradation| degradation.message.clone())
            .collect::<Vec<_>>();
        if affected_tests.is_none() {
            degradations.push(
                "CodeGraph affected-test capability was unavailable or no changed file was known."
                    .to_owned(),
            );
        }
        let tests_truncated = affected_tests
            .as_ref()
            .is_some_and(|tests| tests.execution.truncated);
        let status = if result.execution.truncated || tests_truncated || !degradations.is_empty() {
            LocalEnrichmentStatus::Partial
        } else {
            LocalEnrichmentStatus::Available
        };
        context.local_enrichment.push(LocalEnrichmentInput {
            repo_id,
            anchor: result.symbol,
            status,
            affected: result
                .affected
                .into_iter()
                .map(|item| LocalImpactItem {
                    symbol: item.name,
                    file_path: item.file_path,
                    start_line: Some(item.start_line),
                    depth: result.depth,
                })
                .collect(),
            affected_tests: affected_tests
                .map(|tests| tests.affected_tests)
                .unwrap_or_default(),
            truncated: result.execution.truncated || tests_truncated,
            degradations,
        });
    }
}

fn unavailable_local_enrichment(
    repo_id: RepoId,
    anchor: &str,
    message: &str,
) -> LocalEnrichmentInput {
    LocalEnrichmentInput {
        repo_id,
        anchor: anchor.to_owned(),
        status: LocalEnrichmentStatus::Unavailable,
        affected: Vec::new(),
        affected_tests: Vec::new(),
        truncated: false,
        degradations: vec![message.to_owned()],
    }
}

fn impact_target_node<'a>(context: &'a ImpactContext, request: &ImpactRequest) -> Option<&'a Node> {
    context.nodes.iter().find(|node| match &request.target {
        ImpactTarget::NodeId(node_id) => node.id == *node_id,
        ImpactTarget::StableKey(stable_key) => node.stable_key == *stable_key,
    })
}

fn impact_target_repo(context: &ImpactContext, request: &ImpactRequest) -> Option<RepoId> {
    impact_target_node(context, request).and_then(|node| node.repo_id.clone())
}

fn impact_target_label(context: &ImpactContext, request: &ImpactRequest) -> Option<String> {
    impact_target_node(context, request).map(|node| node.label.clone())
}

fn fts_search_scores(
    hits: &[code_system_graph_store_sqlite::StoredNodeSearchHit],
) -> BTreeMap<NodeId, f64> {
    hits.iter()
        .enumerate()
        .map(|(index, hit)| {
            (
                hit.node.id.clone(),
                1.0 / f64::from(u32::try_from(index + 1).unwrap_or(u32::MAX)),
            )
        })
        .collect()
}

fn impact_centrality_scores(communities: &[Community]) -> BTreeMap<NodeId, f32> {
    let mut scores = BTreeMap::<NodeId, f32>::new();
    for community in communities {
        for (index, node_id) in community.central_nodes.iter().enumerate() {
            let denominator = u16::try_from(index + 1).unwrap_or(u16::MAX);
            let score = 1.0 / f32::from(denominator);
            scores
                .entry(node_id.clone())
                .and_modify(|current| *current = current.max(score))
                .or_insert(score);
        }
    }
    scores
}

fn conservative_repo_freshness(items: &[RepoFreshness]) -> Vec<RepoFreshness> {
    let mut by_repository = BTreeMap::<RepoId, RepoFreshness>::new();
    for item in items {
        by_repository
            .entry(item.repo_id.clone())
            .and_modify(|current| {
                if freshness_severity(item.state) > freshness_severity(current.state) {
                    *current = item.clone();
                }
            })
            .or_insert_with(|| item.clone());
    }
    by_repository.into_values().collect()
}

fn freshness_severity(state: RepoFreshnessState) -> u8 {
    match state {
        RepoFreshnessState::Fresh => 0,
        RepoFreshnessState::WorkingTreeChanged
        | RepoFreshnessState::CommitsBehind
        | RepoFreshnessState::ConfigChanged
        | RepoFreshnessState::ExtractorChanged
        | RepoFreshnessState::CodegraphPending => 1,
        RepoFreshnessState::Partial | RepoFreshnessState::Unknown => 2,
        RepoFreshnessState::Unavailable => 3,
        RepoFreshnessState::Corrupt => 4,
    }
}

fn recommended_test_commands(
    nodes: &[Node],
    evidence: &BTreeMap<NodeId, Vec<Evidence>>,
) -> Vec<RecommendedCommand> {
    let mut commands = BTreeMap::<(RepoId, String), RecommendedCommand>::new();
    for test in nodes.iter().filter(|node| node.kind == NodeKind::TestCase) {
        let Some(repo_id) = test.repo_id.clone() else {
            continue;
        };
        for item in evidence.get(&test.id).into_iter().flatten() {
            let Some(path) = item.file_path.as_deref() else {
                continue;
            };
            let command = match Path::new(path).extension().and_then(|value| value.to_str()) {
                Some(extension) if extension.eq_ignore_ascii_case("py") => "python -m pytest",
                Some(extension) if extension.eq_ignore_ascii_case("rs") => "cargo test",
                _ => continue,
            };
            commands.entry((repo_id.clone(), command.to_owned())).or_insert_with(|| RecommendedCommand {
                repo_id: repo_id.clone(),
                command: command.to_owned(),
                description: format!("Run the repository test suite covering `{}`; Code System Graph does not execute it.", test.label),
            });
        }
    }
    commands.into_values().collect()
}

fn community_centrality_scores(communities: &[Community]) -> BTreeMap<NodeId, f64> {
    let mut scores = BTreeMap::<NodeId, f64>::new();
    for community in communities {
        for (index, node_id) in community.central_nodes.iter().enumerate() {
            let score = 1.0 / f64::from(u32::try_from(index + 1).unwrap_or(u32::MAX));
            scores
                .entry(node_id.clone())
                .and_modify(|current| *current = current.max(score))
                .or_insert(score);
        }
    }
    scores
}

fn community_memberships(communities: &[Community]) -> BTreeMap<NodeId, Vec<CommunityId>> {
    let mut memberships = BTreeMap::<NodeId, Vec<CommunityId>>::new();
    for community in communities {
        for member in &community.members {
            memberships
                .entry(member.clone())
                .or_default()
                .push(community.id.clone());
        }
    }
    memberships
}

fn service_memberships(nodes: &[Node], edges: &[Edge]) -> BTreeMap<NodeId, Vec<NodeId>> {
    let services = nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Service)
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let mut memberships = services
        .iter()
        .map(|service| (service.clone(), vec![service.clone()]))
        .collect::<BTreeMap<_, _>>();
    for edge in edges {
        if services.contains(&edge.source) {
            memberships
                .entry(edge.target.clone())
                .or_default()
                .push(edge.source.clone());
        }
        if services.contains(&edge.target) {
            memberships
                .entry(edge.source.clone())
                .or_default()
                .push(edge.target.clone());
        }
    }
    for values in memberships.values_mut() {
        values.sort();
        values.dedup();
    }
    memberships
}

fn node_evidence(edges: &[Edge], evidence: &[Evidence]) -> BTreeMap<NodeId, Vec<Evidence>> {
    let by_id = evidence
        .iter()
        .map(|item| (item.id.clone(), item))
        .collect::<BTreeMap<_, _>>();
    let mut result = BTreeMap::<NodeId, Vec<Evidence>>::new();
    for edge in edges {
        for node_id in [&edge.source, &edge.target] {
            let values = result.entry(node_id.clone()).or_default();
            values.extend(
                edge.evidence
                    .iter()
                    .filter_map(|evidence_id| by_id.get(evidence_id))
                    .map(|item| (*item).clone()),
            );
        }
    }
    for values in result.values_mut() {
        values.sort_by(|left, right| left.id.cmp(&right.id));
        values.dedup_by(|left, right| left.id == right.id);
    }
    result
}

/// Evaluates current repository state against the published snapshot.
///
/// # Errors
///
/// Returns [`ApplicationError`] for invalid manifests, path-policy failures, missing snapshots,
/// or storage failures.
pub fn status_workspace(
    config_path: &Path,
    database_path: &Path,
) -> Result<WorkspaceStatus, ApplicationError> {
    let current = load_workspace_context(config_path, &ScanOverrides::default())?;
    let store = SqliteStore::open_read_only(database_path)?;
    let previous = store.load_current_freshness(&current.manifest.name)?;
    let previous_by_checkout = previous
        .iter()
        .map(|freshness| (freshness.checkout_id.clone(), freshness))
        .collect::<BTreeMap<CheckoutId, _>>();
    let current_checkouts = current
        .registry
        .record
        .repositories
        .iter()
        .map(|repository| repository.checkout_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut repositories = current
        .registry
        .record
        .repositories
        .iter()
        .map(|repository| {
            let previous = previous_by_checkout.get(&repository.checkout_id);
            let (state, reason) = evaluate_repository_freshness(
                repository,
                previous.copied(),
                &current.registry.record.manifest_hash,
            );
            RepoFreshness {
                repo_id: repository.id.clone(),
                checkout_id: repository.checkout_id.clone(),
                head_commit: repository.head_commit.clone(),
                manifest_hash: current.registry.record.manifest_hash.clone(),
                state,
                reason,
            }
        })
        .collect::<Vec<_>>();
    repositories.extend(
        previous
            .into_iter()
            .filter(|freshness| !current_checkouts.contains(&freshness.checkout_id))
            .map(|mut freshness| {
                freshness.state = RepoFreshnessState::Unavailable;
                freshness.reason = Some(
                    "Checkout from the published snapshot is absent from the current manifest."
                        .to_owned(),
                );
                freshness
            }),
    );
    repositories.sort_by(|left, right| {
        (&left.repo_id, &left.checkout_id).cmp(&(&right.repo_id, &right.checkout_id))
    });
    Ok(WorkspaceStatus {
        workspace: current.manifest.name,
        schema_version: store.schema_version()?,
        integrity_ok: store.integrity_check()?,
        freshness: freshness_summary(&repositories),
        repositories,
    })
}

/// Inspects, validates, compares, or explains contracts in the current immutable graph.
///
/// # Errors
///
/// Returns [`ApplicationError`] for missing snapshots, invalid stored data, or invalid bounded
/// contract requests.
pub fn contracts_workspace(
    database_path: &Path,
    workspace: &str,
    request: &ContractRequest,
) -> Result<ContractReport, ApplicationError> {
    let store = SqliteStore::open_read_only(database_path)?;
    let (nodes, edges) = store.load_current_graph(workspace)?;
    let evidence = store.load_current_evidence(workspace)?;
    Ok(inspect_contracts(&nodes, &edges, &evidence, &[], request)?)
}

/// Renders one deterministic bounded source-free graph export.
///
/// # Errors
///
/// Returns [`ApplicationError`] for missing snapshots, invalid stored data, or invalid bounds.
pub fn export_workspace(
    database_path: &Path,
    workspace: &str,
    request: &ExportRequest,
) -> Result<ExportReport, ApplicationError> {
    let store = SqliteStore::open_read_only(database_path)?;
    let (nodes, edges) = store.load_current_graph(workspace)?;
    let evidence = store.load_current_evidence(workspace)?;
    Ok(export_graph(&nodes, &edges, &evidence, request)?)
}

/// Consolidates configuration, schema, integrity, and freshness diagnostics.
///
/// # Errors
///
/// Returns [`ApplicationError`] when required workspace or store observations cannot be loaded.
#[expect(
    clippy::too_many_lines,
    reason = "Doctor keeps every explicit safety observation visible in one conservative report"
)]
pub fn doctor_workspace(
    config_path: &Path,
    database_path: &Path,
) -> Result<DoctorReport, ApplicationError> {
    let status = status_workspace(config_path, database_path)?;
    let store = SqliteStore::open_read_only(database_path)?;
    let diagnostics = store.diagnostics()?;
    let context = load_workspace_context(config_path, &ScanOverrides::default())?;
    let expected_version = u32::try_from(latest_schema_version()).map_err(|error| {
        ApplicationError::Initialization(format!("unsupported schema version: {error}"))
    })?;
    let actual_version = u32::try_from(status.schema_version).ok();
    let freshness = status
        .repositories
        .iter()
        .map(|repository| FreshnessDoctorInput {
            repo_id: repository.repo_id.clone(),
            state: repository.state,
            detail: repository.reason.clone(),
        })
        .collect();
    let repository_paths_valid = context
        .registry
        .record
        .repositories
        .iter()
        .all(|repository| {
            context
                .registry
                .checkout_path(&repository.alias)
                .is_some_and(Path::is_dir)
        });
    let git_available = std::process::Command::new("git")
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    Ok(doctor(&DoctorRequest {
        schema: vec![SchemaDoctorInput {
            name: "sqlite".to_owned(),
            expected_version,
            actual_version,
            migrations_consistent: Some(actual_version == Some(expected_version)),
        }],
        integrity: vec![
            IntegrityDoctorInput {
                name: "binary-version".to_owned(),
                passed: Some(true),
                detail: Some(env!("CARGO_PKG_VERSION").to_owned()),
            },
            IntegrityDoctorInput {
                name: "sqlite".to_owned(),
                passed: Some(status.integrity_ok),
                detail: None,
            },
            IntegrityDoctorInput {
                name: "sqlite-wal".to_owned(),
                passed: Some(diagnostics.journal_mode.eq_ignore_ascii_case("wal")),
                detail: Some(diagnostics.journal_mode),
            },
            IntegrityDoctorInput {
                name: "sqlite-foreign-keys".to_owned(),
                passed: Some(diagnostics.foreign_keys_enabled),
                detail: None,
            },
            IntegrityDoctorInput {
                name: "sqlite-fts5".to_owned(),
                passed: Some(diagnostics.fts5_index_available),
                detail: None,
            },
            IntegrityDoctorInput {
                name: "repository-paths".to_owned(),
                passed: Some(repository_paths_valid),
                detail: None,
            },
            IntegrityDoctorInput {
                name: "git".to_owned(),
                passed: Some(git_available),
                detail: None,
            },
            IntegrityDoctorInput {
                name: "extractor-matrix".to_owned(),
                passed: Some(true),
                detail: Some(
                    "Built-in extractor inventory is compiled into this binary.".to_owned(),
                ),
            },
            IntegrityDoctorInput {
                name: "network-policy".to_owned(),
                passed: Some(true),
                detail: Some(
                    "Remote providers and non-loopback HTTP remain explicitly gated.".to_owned(),
                ),
            },
            IntegrityDoctorInput {
                name: "writer-lock".to_owned(),
                passed: None,
                detail: Some(
                    "Read-only doctor does not acquire or reclaim the workspace writer lock."
                        .to_owned(),
                ),
            },
            IntegrityDoctorInput {
                name: "hook-installation".to_owned(),
                passed: None,
                detail: Some(
                    "Use `csgraph hooks status` with an explicit host and repository root."
                        .to_owned(),
                ),
            },
            IntegrityDoctorInput {
                name: "release-files".to_owned(),
                passed: None,
                detail: Some(
                    "Release artifact notices and checksums require an explicit installation path."
                        .to_owned(),
                ),
            },
        ],
        freshness,
        providers: vec![ProviderDoctorInput {
            name: "codegraph".to_owned(),
            status: ProviderDoctorStatus::Unavailable,
            detail: Some(
                "CodeGraph probing is opt-in and was not requested by this doctor invocation."
                    .to_owned(),
            ),
        }],
        config: vec![ConfigDoctorInput {
            name: status.workspace,
            valid: Some(true),
            detail: None,
        }],
    }))
}

/// Creates a redacted, source-free diagnostic bundle only at an explicit output path.
///
/// Existing files are never overwritten. On Unix, the file is created with owner-only
/// permissions before any diagnostic data is serialized.
///
/// # Errors
///
/// Returns [`ApplicationError`] when diagnostics cannot be collected, the destination already
/// exists, time cannot be represented, or the bundle cannot be durably written.
pub fn create_diagnostic_bundle(
    config_path: &Path,
    database_path: &Path,
    output_path: &Path,
) -> Result<DiagnosticBundle, ApplicationError> {
    let generated_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ApplicationError::Initialization(error.to_string()))?
        .as_millis();
    let bundle = DiagnosticBundle {
        schema_version: 1,
        binary_version: env!("CARGO_PKG_VERSION").to_owned(),
        operating_system: std::env::consts::OS.to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        generated_at_unix_ms,
        debug_requested: std::env::var("CODE_SYSTEM_GRAPH_DEBUG")
            .is_ok_and(|value| value.trim() == "1"),
        doctor: doctor_workspace(config_path, database_path)?,
    };

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    let mut output = options
        .open(output_path)
        .map_err(|source| ApplicationError::WriteFile {
            path: output_path.to_path_buf(),
            source,
        })?;
    serde_json::to_writer_pretty(&mut output, &bundle)
        .map_err(|error| ApplicationError::Initialization(error.to_string()))?;
    output
        .write_all(b"\n")
        .and_then(|()| output.sync_all())
        .map_err(|source| ApplicationError::WriteFile {
            path: output_path.to_path_buf(),
            source,
        })?;
    Ok(bundle)
}

/// Creates a validated online database backup.
///
/// # Errors
///
/// Returns [`ApplicationError`] when source validation or backup creation fails.
pub fn backup_database(
    database_path: &Path,
    backup_path: &Path,
) -> Result<BackupSummary, ApplicationError> {
    SqliteStore::backup_file(database_path, backup_path)?;
    Ok(BackupSummary {
        database: database_path.to_string_lossy().into_owned(),
        backup: backup_path.to_string_lossy().into_owned(),
    })
}

/// Plans or applies database migrations.
///
/// # Errors
///
/// Returns [`ApplicationError`] when schema validation, backup, or migration fails.
pub fn migrate_database(
    database_path: &Path,
    dry_run: bool,
) -> Result<MigrationSummary, ApplicationError> {
    let report = if dry_run {
        SqliteStore::migration_plan(database_path)?
    } else {
        SqliteStore::migrate(database_path)?
    };
    Ok(MigrationSummary {
        from_version: report.from_version,
        to_version: report.to_version,
        backup: report
            .backup_path
            .map(|path| path.to_string_lossy().into_owned()),
        applied: report.applied,
        dry_run,
    })
}

/// Restores a validated backup while preserving the replaced database.
///
/// # Errors
///
/// Returns [`ApplicationError`] when locking, validation, safety backup, or restore fails.
pub fn restore_database(
    database_path: &Path,
    backup_path: &Path,
) -> Result<RestoreSummary, ApplicationError> {
    let report = SqliteStore::restore_from(database_path, backup_path)?;
    Ok(RestoreSummary {
        source: report.source_path.to_string_lossy().into_owned(),
        safety_backup: report
            .safety_backup_path
            .map(|path| path.to_string_lossy().into_owned()),
        schema_version: report.schema_version,
    })
}

/// Lists persisted workspaces without mutating or migrating the database.
///
/// # Errors
///
/// Returns [`ApplicationError`] when the database or registry cannot be read.
pub fn list_workspace_registry(
    database_path: &Path,
) -> Result<Vec<WorkspaceRegistryItem>, ApplicationError> {
    let store = SqliteStore::open_read_only(database_path)?;
    Ok(store
        .list_workspaces()?
        .into_iter()
        .map(|workspace| WorkspaceRegistryItem {
            id: workspace.id.as_str().to_owned(),
            name: workspace.name,
            manifest_hash: workspace.manifest_hash,
            config_path: workspace.config_path.map(|path| path.display),
            repository_count: workspace.repository_count,
        })
        .collect())
}

/// Validates and adds a workspace manifest to the persisted registry.
///
/// # Errors
///
/// Returns [`ApplicationError`] for name mismatch, duplicate registration, invalid configuration,
/// path policy, locking, migration, or storage failure.
pub fn add_workspace_to_registry(
    database_path: &Path,
    workspace_name: &str,
    config_path: &Path,
) -> Result<WorkspaceMutationSummary, ApplicationError> {
    let context = load_workspace_context(config_path, &ScanOverrides::default())?;
    if context.manifest.name != workspace_name {
        return Err(ApplicationError::WorkspaceNameMismatch {
            requested: workspace_name.to_owned(),
            manifest: context.manifest.name,
        });
    }
    let _lock = StoreLock::acquire(database_path, Duration::from_mins(5))?;
    let mut store = SqliteStore::open(database_path)?;
    if store.workspace_exists(workspace_name)? {
        return Err(ApplicationError::WorkspaceAlreadyExists(
            workspace_name.to_owned(),
        ));
    }
    store.save_workspace_registry(&context.registry.record)?;
    Ok(WorkspaceMutationSummary {
        workspace: workspace_name.to_owned(),
        config_path: context.registry.record.config_path.map(|path| path.display),
        repository_count: context.registry.record.repositories.len(),
        operation: "add".to_owned(),
    })
}

/// Removes one workspace and its snapshot state from the persisted registry.
///
/// # Errors
///
/// Returns [`ApplicationError`] for missing workspace, locking, migration, or storage failure.
pub fn remove_workspace_from_registry(
    database_path: &Path,
    workspace_name: &str,
) -> Result<WorkspaceMutationSummary, ApplicationError> {
    let _lock = StoreLock::acquire(database_path, Duration::from_mins(5))?;
    let mut store = SqliteStore::open(database_path)?;
    let repository_count = store
        .load_workspace_registry(workspace_name)
        .map_err(|error| match error {
            StoreError::RegistryIncomplete(_) => {
                ApplicationError::WorkspaceNotFound(workspace_name.to_owned())
            }
            other => ApplicationError::Store(other),
        })?
        .repositories
        .len();
    if !store.remove_workspace(workspace_name)? {
        return Err(ApplicationError::WorkspaceNotFound(
            workspace_name.to_owned(),
        ));
    }
    Ok(WorkspaceMutationSummary {
        workspace: workspace_name.to_owned(),
        config_path: None,
        repository_count,
        operation: "remove".to_owned(),
    })
}

/// Lists repository registrations for one persisted workspace.
///
/// # Errors
///
/// Returns [`ApplicationError`] when the database or workspace registry cannot be read.
pub fn list_repository_registry(
    database_path: &Path,
    workspace: &str,
) -> Result<Vec<RepositoryRecord>, ApplicationError> {
    let store = SqliteStore::open_read_only(database_path)?;
    Ok(store.load_workspace_registry(workspace)?.repositories)
}

/// Adds a minimal repository entry to a workspace manifest or returns a preview.
///
/// # Errors
///
/// Returns [`ApplicationError`] when parsing, path policy, backup, or atomic commit fails.
pub fn add_repository_to_manifest(
    manifest_path: &Path,
    alias: &str,
    repository_path: &str,
    dry_run: bool,
) -> Result<ManifestMutationSummary, ApplicationError> {
    let source = read_file(manifest_path)?;
    let edit = preview_add_repository(&source, alias, repository_path)?;
    validate_manifest_edit(manifest_path, &edit)?;
    finish_manifest_edit(manifest_path, edit, dry_run)
}

/// Removes one repository entry from a workspace manifest or returns a preview.
///
/// # Errors
///
/// Returns [`ApplicationError`] when parsing, path policy, backup, or atomic commit fails.
pub fn remove_repository_from_manifest(
    manifest_path: &Path,
    alias: &str,
    dry_run: bool,
) -> Result<ManifestMutationSummary, ApplicationError> {
    let source = read_file(manifest_path)?;
    let edit = preview_remove_repository(&source, alias)?;
    validate_manifest_edit(manifest_path, &edit)?;
    finish_manifest_edit(manifest_path, edit, dry_run)
}

/// Adds one exact manual relationship declaration to a workspace manifest or returns a preview.
///
/// # Errors
///
/// Returns [`ApplicationError`] when declaration validation, path policy, backup, or atomic commit
/// fails.
pub fn add_manual_link_to_manifest(
    manifest_path: &Path,
    link: &ManualLinkConfig,
    dry_run: bool,
) -> Result<ManifestMutationSummary, ApplicationError> {
    let source = read_file(manifest_path)?;
    let edit = preview_add_manual_link(&source, link)?;
    validate_manifest_edit(manifest_path, &edit)?;
    finish_manifest_edit(manifest_path, edit, dry_run)
}

fn validate_manifest_edit(
    manifest_path: &Path,
    edit: &ManifestEdit,
) -> Result<(), ApplicationError> {
    let manifest = parse_manifest(&edit.updated_source)?;
    register_workspace(manifest_path, &edit.updated_source, &manifest)?;
    Ok(())
}

fn finish_manifest_edit(
    manifest_path: &Path,
    edit: ManifestEdit,
    dry_run: bool,
) -> Result<ManifestMutationSummary, ApplicationError> {
    if dry_run {
        return Ok(ManifestMutationSummary {
            summary: edit.summary,
            manifest: manifest_path.to_string_lossy().into_owned(),
            backup: None,
            applied: false,
            rendered_manifest: Some(edit.updated_source),
        });
    }
    let report = commit_manifest_edit(manifest_path, &edit)?;
    Ok(ManifestMutationSummary {
        summary: edit.summary,
        manifest: report.manifest_path.to_string_lossy().into_owned(),
        backup: Some(report.backup_path.to_string_lossy().into_owned()),
        applied: true,
        rendered_manifest: None,
    })
}

fn evaluate_repository_freshness(
    repository: &code_system_graph_model::RepositoryRecord,
    previous: Option<&RepoFreshness>,
    current_manifest_hash: &str,
) -> (RepoFreshnessState, Option<String>) {
    if repository.working_tree_dirty {
        return (
            RepoFreshnessState::WorkingTreeChanged,
            Some("Git working tree has tracked or untracked changes.".to_owned()),
        );
    }
    let Some(previous) = previous else {
        return (
            RepoFreshnessState::Unknown,
            Some("Checkout was not observed in the published snapshot.".to_owned()),
        );
    };
    if previous.manifest_hash != current_manifest_hash {
        return (
            RepoFreshnessState::ConfigChanged,
            Some("Workspace manifest changed after the published snapshot.".to_owned()),
        );
    }
    if previous.head_commit != repository.head_commit {
        return (
            RepoFreshnessState::CommitsBehind,
            Some("Repository HEAD changed after the published snapshot.".to_owned()),
        );
    }
    (previous.state, previous.reason.clone())
}

fn freshness_summary(repositories: &[RepoFreshness]) -> FreshnessSummary {
    let overall = if repositories
        .iter()
        .all(|freshness| freshness.state == RepoFreshnessState::Fresh)
    {
        OverallFreshness::Fresh
    } else if repositories.iter().any(|freshness| {
        matches!(
            freshness.state,
            RepoFreshnessState::Unknown
                | RepoFreshnessState::Unavailable
                | RepoFreshnessState::Corrupt
        )
    }) {
        OverallFreshness::Unknown
    } else if repositories
        .iter()
        .any(|freshness| freshness.state == RepoFreshnessState::Partial)
    {
        OverallFreshness::Partial
    } else {
        OverallFreshness::Stale
    };
    FreshnessSummary {
        overall,
        stale_repositories: repositories
            .iter()
            .filter(|freshness| freshness.state != RepoFreshnessState::Fresh)
            .map(|freshness| freshness.repo_id.clone())
            .collect(),
        reasons: repositories
            .iter()
            .filter_map(|freshness| freshness.reason.clone())
            .collect(),
    }
}

fn load_workspace_context(
    config_path: &Path,
    overrides: &ScanOverrides,
) -> Result<WorkspaceContext, ApplicationError> {
    let manifest_source = read_file(config_path)?;
    let manifest = parse_manifest(&manifest_source)?;
    for alias in overrides.repo_openapi.keys() {
        if !manifest.repos.contains_key(alias) {
            return Err(ApplicationError::UnknownOverrideRepository(alias.clone()));
        }
    }
    let mut registry = register_workspace(config_path, &manifest_source, &manifest)?;
    let mut repository_configs = BTreeMap::new();
    let mut fingerprint_material = manifest_source;
    for (alias, repository) in &manifest.repos {
        let checkout_path = registry
            .checkout_path(alias)
            .ok_or_else(|| ApplicationError::RegistryAliasMissing(alias.clone()))?;
        let mut effective = resolve_repository_config(checkout_path, repository)?;
        if let Some(openapi) = overrides.repo_openapi.get(alias) {
            apply_openapi_override(&mut effective, openapi)?;
        }
        fingerprint_material.push('\n');
        fingerprint_material.push_str(alias);
        fingerprint_material.push(':');
        fingerprint_material.push_str(&effective.fingerprint);
        repository_configs.insert(alias.clone(), effective);
    }
    registry.record.manifest_hash = stable_id("manifest", &fingerprint_material);
    Ok(WorkspaceContext {
        manifest,
        registry,
        repository_configs,
    })
}

fn focused_batch_cache_complete(
    fingerprints: &[ArtifactFingerprint],
    stored: &[StoredExtractorBatch],
) -> bool {
    fingerprints
        .iter()
        .filter(|fingerprint| focused_extractor(&fingerprint.extractor))
        .all(|fingerprint| {
            stored.iter().any(|batch| {
                batch.source == *fingerprint
                    && matches!(
                        batch.extractor_version.as_str(),
                        FOCUSED_EXTRACTOR_VERSION | LOSSY_FOCUSED_EXTRACTOR_VERSION
                    )
            })
        })
}

fn stored_batch_degradations(
    stored: &[StoredExtractorBatch],
) -> Result<Vec<String>, ApplicationError> {
    let mut degradations = Vec::new();
    for batch in stored {
        if batch.extractor_version == LOSSY_FOCUSED_EXTRACTOR_VERSION {
            degradations.push(format!("{} contains invalid UTF-8 and was decoded lossily; extracted evidence is incomplete", batch.source.path.display));
        }
        if batch.source.extractor == "code-system-graph.data.artifact" {
            let decoded: ExtractorBatch<DataDocument> = load_extractor_batch(batch)?;
            for document in decoded.outputs {
                if document.incomplete {
                    degradations.push(format!(
                        "{} data extraction is incomplete: {:?}",
                        batch.source.path.display, document.warnings
                    ));
                }
            }
        }
    }
    Ok(degradations)
}

fn finalize_scan_degradations(mut degradations: Vec<String>) -> (usize, Vec<String>) {
    degradations.sort();
    degradations.dedup();
    let count = degradations.len();
    if count > MAX_SCAN_DEGRADATIONS {
        degradations.truncate(MAX_SCAN_DEGRADATIONS - 1);
        degradations.push(format!(
            "{} additional degradations omitted",
            count - (MAX_SCAN_DEGRADATIONS - 1)
        ));
    }
    (count, degradations)
}

#[expect(
    clippy::too_many_lines,
    reason = "Batch decode, reuse, extraction, and persistence share one fail-closed type boundary"
)]
fn assemble_focused_batches(
    context: &WorkspaceContext,
    fingerprints: &[ArtifactFingerprint],
    previous: &[StoredExtractorBatch],
    plan: &ExtractorBatchPlan,
) -> Result<FocusedBatchState, ApplicationError> {
    let previous_by_key = previous
        .iter()
        .map(|batch| (ArtifactKey::from(&batch.source), batch))
        .collect::<BTreeMap<_, _>>();
    let planned_actions = plan
        .batches
        .iter()
        .map(|batch| (batch.key.clone(), batch.action))
        .collect::<BTreeMap<_, _>>();
    let mut previous_source_batches = previous
        .iter()
        .filter(|batch| source_extractor(&batch.source.extractor))
        .map(load_extractor_batch)
        .collect::<Result<Vec<ExtractorBatch<SourceObservation>>, _>>()?;
    previous_source_batches.sort_by_key(ExtractorBatch::key);

    let checkouts = context
        .registry
        .record
        .repositories
        .iter()
        .filter_map(|repository| {
            context
                .registry
                .checkout_path(&repository.alias)
                .map(|path| (repository.id.clone(), path))
        })
        .collect::<BTreeMap<_, _>>();
    let mut source_batches = Vec::new();
    let mut package_batches = Vec::new();
    let mut generated_client_batches = Vec::new();
    let mut graphql_batches = Vec::new();
    let mut event_batches = Vec::new();
    let mut protobuf_batches = Vec::new();
    let mut data_batches = Vec::new();
    let mut infrastructure_batches = Vec::new();
    let mut documentation_batches = Vec::new();
    let mut config_batches = Vec::new();
    let mut stored_batches = Vec::new();
    let mut degradations = Vec::new();
    let mut force_relink = false;
    for fingerprint in fingerprints
        .iter()
        .filter(|fingerprint| focused_extractor(&fingerprint.extractor))
    {
        let key = ArtifactKey::from(fingerprint);
        let reusable = previous_by_key.get(&key).copied().filter(|batch| {
            planned_actions.get(&key) == Some(&BatchAction::Reuse)
                && batch.source.content_hash == fingerprint.content_hash
                && matches!(
                    batch.extractor_version.as_str(),
                    FOCUSED_EXTRACTOR_VERSION | LOSSY_FOCUSED_EXTRACTOR_VERSION
                )
        });
        if let Some(stored) = reusable {
            stored_batches.push(stored.clone());
            if source_extractor(&fingerprint.extractor) {
                source_batches.push(load_extractor_batch(stored)?);
            } else if fingerprint.extractor == "code-system-graph.packages" {
                package_batches.push(load_extractor_batch(stored)?);
            } else if graphql_extractor(&fingerprint.extractor) {
                graphql_batches.push(load_extractor_batch(stored)?);
            } else if event_extractor(&fingerprint.extractor) {
                event_batches.push(load_extractor_batch(stored)?);
            } else if protobuf_extractor(&fingerprint.extractor) {
                protobuf_batches.push(load_extractor_batch(stored)?);
            } else if data_extractor(&fingerprint.extractor) {
                data_batches.push(load_extractor_batch(stored)?);
            } else if infrastructure_extractor(&fingerprint.extractor) {
                infrastructure_batches.push(load_extractor_batch(stored)?);
            } else if documentation_extractor(&fingerprint.extractor) {
                documentation_batches.push(load_extractor_batch(stored)?);
            } else if fingerprint.extractor == "code-system-graph.config.safe" {
                config_batches.push(load_extractor_batch(stored)?);
            } else {
                generated_client_batches.push(load_extractor_batch(stored)?);
            }
            continue;
        }
        if previous_by_key.contains_key(&key) {
            force_relink = true;
        }
        let checkout = checkouts.get(&fingerprint.repo_id).ok_or_else(|| {
            ApplicationError::RegistryAliasMissing(fingerprint.repo_id.as_str().to_owned())
        })?;
        let relative_path = native_relative_path(&fingerprint.path);
        let artifact_path = checkout.join(&relative_path);
        let (source, source_was_lossy) = read_source_file(&artifact_path)?;
        if source_was_lossy {
            degradations.push(format!("{} contains invalid UTF-8 and was decoded lossily; extracted evidence is incomplete", fingerprint.path.display));
        }
        if source_extractor(&fingerprint.extractor) {
            let syntax = inspect_source_syntax(
                source_syntax_language(&fingerprint.extractor),
                &portable_path(&fingerprint.path.display),
                &source,
            )?;
            let mut observations = match fingerprint.extractor.as_str() {
                "code-system-graph.source.javascript" => parse_javascript_source_at_path(
                    &portable_path(&fingerprint.path.display),
                    &source,
                ),
                "code-system-graph.source.typescript" => parse_typescript_source_at_path(
                    &portable_path(&fingerprint.path.display),
                    &source,
                ),
                "code-system-graph.source.rust" => parse_rust_source(&source),
                "code-system-graph.source.python" => parse_python_source(&source),
                "code-system-graph.source.go" => parse_go_source(&source),
                "code-system-graph.source.java" => parse_java_source(&source),
                _ => Vec::new(),
            };
            if observations
                .iter()
                .any(|observation| observation.role != SourceRole::Test)
                && syntax.boundary_candidate_count == 0
            {
                return Err(ApplicationError::InvalidSourceObservation(format!(
                    "{} produced framework facts without a Tree-sitter boundary candidate",
                    fingerprint.path.display
                )));
            }
            if syntax.has_error {
                for observation in &mut observations {
                    observation.status = SourceEpistemicStatus::Incomplete;
                    if !observation
                        .warnings
                        .contains(&SourceWarning::SyntaxErrorRecovery)
                    {
                        observation
                            .warnings
                            .push(SourceWarning::SyntaxErrorRecovery);
                    }
                }
            }
            let batch = ExtractorBatch::new(fingerprint.clone(), observations);
            stored_batches.push(store_extractor_batch(&batch, FOCUSED_EXTRACTOR_VERSION)?);
            source_batches.push(batch);
        } else if fingerprint.extractor == "code-system-graph.packages" {
            let portable_path = portable_path(&fingerprint.path.display);
            let manifest = extract_package_manifest(&portable_path, &source)?;
            let batch = ExtractorBatch::new(fingerprint.clone(), vec![manifest]);
            stored_batches.push(store_extractor_batch(&batch, FOCUSED_EXTRACTOR_VERSION)?);
            package_batches.push(batch);
        } else if graphql_extractor(&fingerprint.extractor) {
            let portable_path = portable_path(&fingerprint.path.display);
            let document = match fingerprint.extractor.as_str() {
                "code-system-graph.graphql.document" => {
                    extract_graphql_document(&portable_path, &source)?
                }
                "code-system-graph.graphql.persisted" => GraphqlDocument {
                    source_path: portable_path.clone(),
                    types: Vec::new(),
                    operations: Vec::new(),
                    fragments: Vec::new(),
                    persisted_operations: extract_graphql_persisted_operations(
                        &portable_path,
                        &source,
                    )?,
                    resolvers: Vec::new(),
                    federation: Vec::new(),
                    complete: true,
                    warnings: Vec::new(),
                },
                "code-system-graph.graphql.source" => {
                    let language = source_language_for_path(&relative_path).ok_or_else(|| {
                        ApplicationError::InvalidSourceObservation(format!(
                            "unsupported GraphQL source language for `{portable_path}`"
                        ))
                    })?;
                    let mut document = parse_graphql_source(language, &source);
                    document.source_path = portable_path;
                    document
                }
                _ => unreachable!("graphql extractor classification must be exhaustive"),
            };
            let batch = ExtractorBatch::new(fingerprint.clone(), vec![document]);
            stored_batches.push(store_extractor_batch(&batch, FOCUSED_EXTRACTOR_VERSION)?);
            graphql_batches.push(batch);
        } else if event_extractor(&fingerprint.extractor) {
            let portable_path = portable_path(&fingerprint.path.display);
            let document = if fingerprint.extractor == "code-system-graph.events.asyncapi" {
                extract_asyncapi(&portable_path, &source)?
            } else {
                let language = source_language_for_path(&relative_path).ok_or_else(|| {
                    ApplicationError::InvalidSourceObservation(format!(
                        "unsupported event source language for `{portable_path}`"
                    ))
                })?;
                let mut document = parse_event_source(language, &source);
                document.source_path = Some(portable_path);
                document
            };
            let batch = ExtractorBatch::new(fingerprint.clone(), vec![document]);
            stored_batches.push(store_extractor_batch(&batch, FOCUSED_EXTRACTOR_VERSION)?);
            event_batches.push(batch);
        } else if protobuf_extractor(&fingerprint.extractor) {
            let portable_path = portable_path(&fingerprint.path.display);
            let document = if fingerprint.extractor == "code-system-graph.protobuf" {
                ProtobufDocument::File(Box::new(extract_protobuf(&portable_path, &source)?))
            } else {
                let language = source_language_for_path(&relative_path).ok_or_else(|| {
                    ApplicationError::InvalidSourceObservation(format!(
                        "unsupported generated protobuf source language for `{portable_path}`"
                    ))
                })?;
                ProtobufDocument::Generated(parse_protobuf_generated_source(
                    language,
                    &portable_path,
                    &source,
                ))
            };
            let batch = ExtractorBatch::new(fingerprint.clone(), vec![document]);
            stored_batches.push(store_extractor_batch(&batch, FOCUSED_EXTRACTOR_VERSION)?);
            protobuf_batches.push(batch);
        } else if data_extractor(&fingerprint.extractor) {
            let portable_path = portable_path(&fingerprint.path.display);
            let document = if fingerprint.extractor == "code-system-graph.data.source" {
                let language = source_language_for_path(&relative_path).ok_or_else(|| {
                    ApplicationError::InvalidSourceObservation(format!(
                        "unsupported data source language for `{portable_path}`"
                    ))
                })?;
                let crate_root = cargo_crate_root(checkout, &artifact_path);
                parse_literal_sql_source_at_root(language, &portable_path, &crate_root, &source)
            } else {
                extract_data_artifact(&portable_path, &source)?
            };
            if fingerprint.extractor == "code-system-graph.data.artifact" && document.incomplete {
                degradations.push(format!(
                    "{} data extraction is incomplete: {:?}",
                    fingerprint.path.display, document.warnings
                ));
            }
            let batch = ExtractorBatch::new(fingerprint.clone(), vec![document]);
            stored_batches.push(store_extractor_batch(&batch, FOCUSED_EXTRACTOR_VERSION)?);
            data_batches.push(batch);
        } else if infrastructure_extractor(&fingerprint.extractor) {
            let portable_path = portable_path(&fingerprint.path.display);
            let document = match fingerprint.extractor.as_str() {
                "code-system-graph.infrastructure.compose" => {
                    extract_docker_compose(&portable_path, &source)?
                }
                "code-system-graph.infrastructure.kubernetes" => {
                    extract_kubernetes(&portable_path, &source)?
                }
                "code-system-graph.infrastructure.helm" => extract_helm(&portable_path, &source)?,
                "code-system-graph.infrastructure.terraform" => {
                    extract_terraform(&portable_path, &source)?
                }
                _ => unreachable!("infrastructure extractor classification must be exhaustive"),
            };
            let batch = ExtractorBatch::new(fingerprint.clone(), vec![document]);
            stored_batches.push(store_extractor_batch(&batch, FOCUSED_EXTRACTOR_VERSION)?);
            infrastructure_batches.push(batch);
        } else if documentation_extractor(&fingerprint.extractor) {
            let portable_path = portable_path(&fingerprint.path.display);
            let document = match fingerprint.extractor.as_str() {
                "code-system-graph.documents.markdown" => {
                    extract_markdown(&portable_path, &source)?
                }
                "code-system-graph.documents.codeowners" => {
                    extract_codeowners(&portable_path, &source)?
                }
                "code-system-graph.documents.catalog" => {
                    extract_service_catalog(&portable_path, &source)?
                }
                _ => unreachable!("documentation extractor classification must be exhaustive"),
            };
            let batch = ExtractorBatch::new(fingerprint.clone(), vec![document]);
            stored_batches.push(store_extractor_batch(&batch, FOCUSED_EXTRACTOR_VERSION)?);
            documentation_batches.push(batch);
        } else if fingerprint.extractor == "code-system-graph.config.safe" {
            let portable_path = portable_path(&fingerprint.path.display);
            let document = extract_safe_config(&portable_path, &source)?;
            let batch = ExtractorBatch::new(fingerprint.clone(), vec![document]);
            stored_batches.push(store_extractor_batch(&batch, FOCUSED_EXTRACTOR_VERSION)?);
            config_batches.push(batch);
        } else {
            let portable_path = portable_path(&fingerprint.path.display);
            let metadata = extract_generated_client_metadata(&portable_path, &source)?;
            let batch = ExtractorBatch::new(fingerprint.clone(), metadata);
            stored_batches.push(store_extractor_batch(&batch, FOCUSED_EXTRACTOR_VERSION)?);
            generated_client_batches.push(batch);
        }
        if source_was_lossy {
            let stored = stored_batches
                .last_mut()
                .expect("new extraction must persist one batch");
            LOSSY_FOCUSED_EXTRACTOR_VERSION.clone_into(&mut stored.extractor_version);
        }
    }
    source_batches.sort_by_key(ExtractorBatch::key);
    package_batches.sort_by_key(ExtractorBatch::key);
    generated_client_batches.sort_by_key(ExtractorBatch::key);
    graphql_batches.sort_by_key(ExtractorBatch::key);
    event_batches.sort_by_key(ExtractorBatch::key);
    protobuf_batches.sort_by_key(ExtractorBatch::key);
    data_batches.sort_by_key(ExtractorBatch::key);
    infrastructure_batches.sort_by_key(ExtractorBatch::key);
    documentation_batches.sort_by_key(ExtractorBatch::key);
    config_batches.sort_by_key(ExtractorBatch::key);
    stored_batches.sort_by(|left, right| {
        ArtifactKey::from(&left.source).cmp(&ArtifactKey::from(&right.source))
    });
    Ok(FocusedBatchState {
        source_batches,
        previous_source_batches,
        package_batches,
        generated_client_batches,
        graphql_batches,
        event_batches,
        protobuf_batches,
        data_batches,
        infrastructure_batches,
        documentation_batches,
        config_batches,
        stored_batches,
        degradations,
        force_relink,
    })
}

fn focused_extractor(extractor: &str) -> bool {
    source_extractor(extractor)
        || matches!(
            extractor,
            "code-system-graph.packages" | "code-system-graph.http.generated-client"
        )
        || graphql_extractor(extractor)
        || event_extractor(extractor)
        || protobuf_extractor(extractor)
        || data_extractor(extractor)
        || infrastructure_extractor(extractor)
        || documentation_extractor(extractor)
        || extractor == "code-system-graph.config.safe"
}

fn graphql_extractor(extractor: &str) -> bool {
    matches!(
        extractor,
        "code-system-graph.graphql.document"
            | "code-system-graph.graphql.persisted"
            | "code-system-graph.graphql.source"
    )
}

fn event_extractor(extractor: &str) -> bool {
    matches!(
        extractor,
        "code-system-graph.events.asyncapi" | "code-system-graph.events.source"
    )
}

fn protobuf_extractor(extractor: &str) -> bool {
    matches!(
        extractor,
        "code-system-graph.protobuf" | "code-system-graph.protobuf.generated"
    )
}

fn data_extractor(extractor: &str) -> bool {
    matches!(
        extractor,
        "code-system-graph.data.artifact" | "code-system-graph.data.source"
    )
}

fn infrastructure_extractor(extractor: &str) -> bool {
    matches!(
        extractor,
        "code-system-graph.infrastructure.compose"
            | "code-system-graph.infrastructure.kubernetes"
            | "code-system-graph.infrastructure.helm"
            | "code-system-graph.infrastructure.terraform"
    )
}

fn documentation_extractor(extractor: &str) -> bool {
    matches!(
        extractor,
        "code-system-graph.documents.markdown"
            | "code-system-graph.documents.codeowners"
            | "code-system-graph.documents.catalog"
    )
}

fn source_extractor(extractor: &str) -> bool {
    matches!(
        extractor,
        "code-system-graph.source.javascript"
            | "code-system-graph.source.typescript"
            | "code-system-graph.source.rust"
            | "code-system-graph.source.python"
            | "code-system-graph.source.go"
            | "code-system-graph.source.java"
    )
}

fn source_syntax_language(extractor: &str) -> SourceSyntaxLanguage {
    match extractor {
        "code-system-graph.source.javascript" => SourceSyntaxLanguage::JavaScript,
        "code-system-graph.source.typescript" => SourceSyntaxLanguage::TypeScript,
        "code-system-graph.source.rust" => SourceSyntaxLanguage::Rust,
        "code-system-graph.source.python" => SourceSyntaxLanguage::Python,
        "code-system-graph.source.go" => SourceSyntaxLanguage::Go,
        "code-system-graph.source.java" => SourceSyntaxLanguage::Java,
        _ => unreachable!("source_syntax_language requires a focused source extractor"),
    }
}

fn source_language_for_path(path: &Path) -> Option<SourceLanguage> {
    let extension = path.extension()?.to_str()?;
    match extension {
        "js" | "jsx" => Some(SourceLanguage::JavaScript),
        "ts" | "tsx" => Some(SourceLanguage::TypeScript),
        "rs" => Some(SourceLanguage::Rust),
        "py" => Some(SourceLanguage::Python),
        "go" => Some(SourceLanguage::Go),
        "java" => Some(SourceLanguage::Java),
        _ => None,
    }
}

#[cfg(unix)]
fn native_relative_path(path: &code_system_graph_model::NativePath) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;

    PathBuf::from(std::ffi::OsString::from_vec(path.bytes.clone()))
}

#[cfg(windows)]
fn native_relative_path(path: &code_system_graph_model::NativePath) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;

    let wide = path
        .bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    PathBuf::from(std::ffi::OsString::from_wide(&wide))
}

#[cfg(not(any(unix, windows)))]
fn native_relative_path(path: &code_system_graph_model::NativePath) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(&path.bytes).into_owned())
}

fn generated_client_graph(
    batches: &[ExtractorBatch<GeneratedClientMetadata>],
    boundaries: &[HttpBoundary],
) -> (Vec<Node>, Vec<Edge>, Vec<Evidence>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut evidence = Vec::new();
    for batch in batches {
        let source_path = portable_path(&batch.source.path.display);
        for metadata in &batch.outputs {
            let name = metadata.name.as_deref().unwrap_or("metadata");
            let stable_key = format!(
                "generated-client:{}:{source_path}:{name}:{}",
                batch.source.repo_id.as_str(),
                metadata.generated_file.as_deref().unwrap_or("")
            );
            let node = Node {
                id: NodeId::new(stable_id("node", &stable_key)),
                kind: NodeKind::Artifact,
                repo_id: Some(batch.source.repo_id.clone()),
                stable_key: stable_key.clone(),
                label: metadata
                    .generator_name
                    .as_ref()
                    .map_or_else(|| format!("{} {name}", metadata.tool), Clone::clone),
            };
            let item_evidence = Evidence {
                id: EvidenceId::new(stable_id("evidence", &stable_key)),
                repo_id: Some(batch.source.repo_id.clone()),
                file_path: Some(source_path.clone()),
                start_line: Some(metadata.line),
                end_line: Some(metadata.line),
                extractor: "code-system-graph.http.generated-client".to_owned(),
                extractor_version: "1.0.0".to_owned(),
                provenance: Provenance::Extracted,
                confidence: 1.0,
                observed_at_commit: None,
                content_hash: Some(batch.source.content_hash.clone()),
                note: Some("explicit generated-client metadata".to_owned()),
            };
            if let Some(input_spec) = &metadata.input_spec {
                edges.extend(
                    boundaries
                        .iter()
                        .filter(|boundary| {
                            boundary.node.repo_id.as_ref() == Some(&batch.source.repo_id)
                                && boundary.evidence.file_path.as_ref() == Some(input_spec)
                        })
                        .map(|boundary| {
                            let key = format!(
                                "{}:{:?}:{}",
                                node.id.as_str(),
                                EdgeKind::Consumes,
                                boundary.node.id.as_str()
                            );
                            Edge {
                                id: EdgeId::new(stable_id("edge", &key)),
                                source: node.id.clone(),
                                target: boundary.node.id.clone(),
                                kind: EdgeKind::Consumes,
                                confidence: 1.0,
                                status: EpistemicStatus::Confirmed,
                                evidence: vec![item_evidence.id.clone()],
                            }
                        }),
                );
            }
            nodes.push(node);
            evidence.push(item_evidence);
        }
    }
    nodes.sort_by(|left, right| left.id.cmp(&right.id));
    nodes.dedup_by(|left, right| left.id == right.id);
    edges.sort_by(|left, right| left.id.cmp(&right.id));
    edges.dedup_by(|left, right| left.id == right.id);
    evidence.sort_by(|left, right| left.id.cmp(&right.id));
    evidence.dedup_by(|left, right| left.id == right.id);
    (nodes, edges, evidence)
}

#[expect(
    clippy::too_many_lines,
    reason = "Graph assembly keeps every source family in one deterministic merge boundary"
)]
fn assemble_graph(
    context: &WorkspaceContext,
    fingerprints: &[ArtifactFingerprint],
    focused: &FocusedBatchState,
) -> Result<GraphAssembly, ApplicationError> {
    let mut boundaries = extract_boundaries(context)?;
    let mut tests = extract_declared_tests(context, fingerprints)?;
    let mut implementations = extract_declared_implementations(context, fingerprints)?;
    let source_facts = focused
        .source_batches
        .iter()
        .map(|batch| {
            source_observations_to_graph(
                &batch.source.repo_id,
                &portable_path(&batch.source.path.display),
                &batch.source.content_hash,
                &batch.outputs,
            )
        })
        .collect::<Vec<SourceGraphFacts>>();
    for facts in &source_facts {
        boundaries.extend(facts.boundaries.clone());
        tests.extend(facts.tests.clone());
        implementations.extend(facts.implementations.clone());
    }
    let mut package_facts = focused
        .package_batches
        .iter()
        .flat_map(|batch| {
            batch.outputs.iter().map(|manifest| {
                package_manifest_to_graph(
                    &batch.source.repo_id,
                    &portable_path(&batch.source.path.display),
                    &batch.source.content_hash,
                    manifest,
                )
            })
        })
        .collect::<Vec<PackageGraphFacts>>();
    link_registered_package_owners(&mut package_facts);
    let (generated_nodes, generated_edges, generated_evidence) =
        generated_client_graph(&focused.generated_client_batches, &boundaries);
    let graphql_sources = focused
        .graphql_batches
        .iter()
        .flat_map(|batch| {
            batch.outputs.iter().map(|document| {
                (
                    batch.source.repo_id.clone(),
                    portable_path(&batch.source.path.display),
                    batch.source.content_hash.clone(),
                    document,
                )
            })
        })
        .collect::<Vec<_>>();
    let graphql_inputs = graphql_sources
        .iter()
        .map(|(repo_id, path, hash, document)| (repo_id, path.as_str(), hash.as_str(), *document))
        .collect::<Vec<_>>();
    let GraphqlGraphFacts {
        nodes: graphql_nodes,
        edges: graphql_edges,
        evidence: graphql_evidence,
    } = graphql_documents_to_graph(&graphql_inputs);
    let event_sources = focused
        .event_batches
        .iter()
        .flat_map(|batch| {
            batch.outputs.iter().map(|document| {
                (
                    batch.source.repo_id.clone(),
                    portable_path(&batch.source.path.display),
                    batch.source.content_hash.clone(),
                    document,
                )
            })
        })
        .collect::<Vec<_>>();
    let event_inputs = event_sources
        .iter()
        .map(|(repo_id, path, hash, document)| (repo_id, path.as_str(), hash.as_str(), *document))
        .collect::<Vec<_>>();
    let EventGraphFacts {
        nodes: event_nodes,
        edges: event_edges,
        evidence: event_evidence,
    } = event_documents_to_graph(&event_inputs);
    let protobuf_sources = focused
        .protobuf_batches
        .iter()
        .flat_map(|batch| {
            batch.outputs.iter().map(|document| {
                (
                    batch.source.repo_id.clone(),
                    portable_path(&batch.source.path.display),
                    batch.source.content_hash.clone(),
                    document,
                )
            })
        })
        .collect::<Vec<_>>();
    let protobuf_inputs = protobuf_sources
        .iter()
        .map(|(repo_id, path, hash, document)| (repo_id, path.as_str(), hash.as_str(), *document))
        .collect::<Vec<_>>();
    let ProtobufGraphFacts {
        nodes: protobuf_nodes,
        edges: protobuf_edges,
        evidence: protobuf_evidence,
    } = protobuf_documents_to_graph(&protobuf_inputs);
    let data_sources = focused
        .data_batches
        .iter()
        .flat_map(|batch| {
            batch.outputs.iter().map(|document| {
                (
                    batch.source.repo_id.clone(),
                    portable_path(&batch.source.path.display),
                    batch.source.content_hash.clone(),
                    document,
                )
            })
        })
        .collect::<Vec<_>>();
    let data_inputs = data_sources
        .iter()
        .map(|(repo_id, path, hash, document)| (repo_id, path.as_str(), hash.as_str(), *document))
        .collect::<Vec<_>>();
    let infrastructure_sources = focused
        .infrastructure_batches
        .iter()
        .flat_map(|batch| {
            batch.outputs.iter().map(|document| {
                (
                    batch.source.repo_id.clone(),
                    portable_path(&batch.source.path.display),
                    batch.source.content_hash.clone(),
                    document,
                )
            })
        })
        .collect::<Vec<_>>();
    let infrastructure_inputs = infrastructure_sources
        .iter()
        .map(|(repo_id, path, hash, document)| (repo_id, path.as_str(), hash.as_str(), *document))
        .collect::<Vec<_>>();
    let documentation_sources = focused
        .documentation_batches
        .iter()
        .flat_map(|batch| {
            batch.outputs.iter().map(|document| {
                (
                    batch.source.repo_id.clone(),
                    portable_path(&batch.source.path.display),
                    batch.source.content_hash.clone(),
                    document,
                )
            })
        })
        .collect::<Vec<_>>();
    let documentation_inputs = documentation_sources
        .iter()
        .map(|(repo_id, path, hash, document)| (repo_id, path.as_str(), hash.as_str(), *document))
        .collect::<Vec<_>>();
    let config_sources = focused
        .config_batches
        .iter()
        .flat_map(|batch| {
            batch.outputs.iter().map(|document| {
                (
                    batch.source.repo_id.clone(),
                    portable_path(&batch.source.path.display),
                    batch.source.content_hash.clone(),
                    document,
                )
            })
        })
        .collect::<Vec<_>>();
    let config_inputs = config_sources
        .iter()
        .map(|(repo_id, path, hash, document)| (repo_id, path.as_str(), hash.as_str(), *document))
        .collect::<Vec<_>>();
    let mut known_nodes = boundaries
        .iter()
        .map(|boundary| boundary.node.clone())
        .chain(tests.iter().map(|test| test.node.clone()))
        .chain(
            implementations
                .iter()
                .map(|implementation| implementation.node.clone()),
        )
        .chain(
            source_facts
                .iter()
                .flat_map(|facts| facts.standalone_test_nodes.iter().cloned()),
        )
        .chain(
            source_facts
                .iter()
                .flat_map(|facts| facts.relation_nodes.iter().cloned()),
        )
        .chain(
            package_facts
                .iter()
                .flat_map(|facts| facts.nodes.iter().cloned()),
        )
        .chain(generated_nodes.iter().cloned())
        .chain(graphql_nodes.iter().cloned())
        .chain(event_nodes.iter().cloned())
        .chain(protobuf_nodes.iter().cloned())
        .collect::<Vec<_>>();
    known_nodes.sort_by(|left, right| left.id.cmp(&right.id));
    known_nodes.dedup_by(|left, right| left.id == right.id);
    let repository_aliases = context
        .registry
        .record
        .repositories
        .iter()
        .map(|repository| (repository.alias.as_str(), &repository.id))
        .collect::<Vec<_>>();
    let ExtractionGraphFacts {
        nodes: extraction_nodes,
        edges: extraction_edges,
        evidence: extraction_evidence,
    } = documents_to_graph(
        &data_inputs,
        &infrastructure_inputs,
        &documentation_inputs,
        &config_inputs,
        &known_nodes,
        &repository_aliases,
    );

    let mut edges = link_http_boundaries(&boundaries)?;
    edges.extend(link_declared_tests(&tests, &boundaries)?);
    edges.extend(link_declared_implementations(
        &implementations,
        &boundaries,
    )?);
    edges.extend(
        source_facts
            .iter()
            .flat_map(|facts| facts.relation_edges.iter().cloned()),
    );
    edges.extend(
        package_facts
            .iter()
            .flat_map(|facts| facts.edges.iter().cloned()),
    );
    edges.extend(generated_edges);
    edges.extend(graphql_edges);
    edges.extend(event_edges);
    edges.extend(protobuf_edges);
    edges.extend(extraction_edges);
    edges.sort_by(|left, right| left.id.cmp(&right.id));
    edges.dedup_by(|left, right| left.id == right.id);

    let mut nodes = boundaries
        .iter()
        .map(|boundary| (boundary.node.id.clone(), boundary.node.clone()))
        .collect::<BTreeMap<_, _>>();
    nodes.extend(
        tests
            .iter()
            .map(|test| (test.node.id.clone(), test.node.clone())),
    );
    nodes.extend(
        implementations
            .iter()
            .map(|implementation| (implementation.node.id.clone(), implementation.node.clone())),
    );
    nodes.extend(source_facts.iter().flat_map(|facts| {
        facts
            .standalone_test_nodes
            .iter()
            .map(|node| (node.id.clone(), node.clone()))
    }));
    nodes.extend(source_facts.iter().flat_map(|facts| {
        facts
            .relation_nodes
            .iter()
            .map(|node| (node.id.clone(), node.clone()))
    }));
    nodes.extend(package_facts.iter().flat_map(|facts| {
        facts
            .nodes
            .iter()
            .map(|node| (node.id.clone(), node.clone()))
    }));
    nodes.extend(
        generated_nodes
            .into_iter()
            .map(|node| (node.id.clone(), node)),
    );
    nodes.extend(
        graphql_nodes
            .into_iter()
            .map(|node| (node.id.clone(), node)),
    );
    nodes.extend(event_nodes.into_iter().map(|node| (node.id.clone(), node)));
    nodes.extend(
        protobuf_nodes
            .into_iter()
            .map(|node| (node.id.clone(), node)),
    );
    nodes.extend(
        extraction_nodes
            .into_iter()
            .map(|node| (node.id.clone(), node)),
    );
    let mut evidence = boundaries
        .iter()
        .map(|boundary| (boundary.evidence.id.clone(), boundary.evidence.clone()))
        .collect::<BTreeMap<_, _>>();
    evidence.extend(
        tests
            .iter()
            .map(|test| (test.evidence.id.clone(), test.evidence.clone())),
    );
    evidence.extend(implementations.iter().map(|implementation| {
        (
            implementation.evidence.id.clone(),
            implementation.evidence.clone(),
        )
    }));
    evidence.extend(source_facts.iter().flat_map(|facts| {
        facts
            .standalone_test_evidence
            .iter()
            .map(|evidence| (evidence.id.clone(), evidence.clone()))
    }));
    evidence.extend(source_facts.iter().flat_map(|facts| {
        facts
            .relation_evidence
            .iter()
            .map(|evidence| (evidence.id.clone(), evidence.clone()))
    }));
    evidence.extend(package_facts.iter().flat_map(|facts| {
        facts
            .evidence
            .iter()
            .map(|evidence| (evidence.id.clone(), evidence.clone()))
    }));
    evidence.extend(
        generated_evidence
            .into_iter()
            .map(|item| (item.id.clone(), item)),
    );
    evidence.extend(
        graphql_evidence
            .into_iter()
            .map(|item| (item.id.clone(), item)),
    );
    evidence.extend(
        event_evidence
            .into_iter()
            .map(|item| (item.id.clone(), item)),
    );
    evidence.extend(
        protobuf_evidence
            .into_iter()
            .map(|item| (item.id.clone(), item)),
    );
    evidence.extend(
        extraction_evidence
            .into_iter()
            .map(|item| (item.id.clone(), item)),
    );
    let mut link_node_keys = BTreeMap::new();
    link_node_keys.extend(boundaries.iter().map(|boundary| {
        (
            boundary.node.id.clone(),
            format!("{}:{}", boundary.method, boundary.path),
        )
    }));
    link_node_keys.extend(tests.iter().map(|test| {
        (
            test.node.id.clone(),
            format!("{}:{}", test.method, test.path),
        )
    }));
    link_node_keys.extend(implementations.iter().map(|implementation| {
        (
            implementation.node.id.clone(),
            format!("{}:{}", implementation.method, implementation.path),
        )
    }));
    Ok(GraphAssembly {
        nodes: nodes.into_values().collect(),
        edges,
        evidence: evidence.into_values().collect(),
        link_decisions: Vec::new(),
        link_node_keys,
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "Fail-closed relinking keeps old/new neighborhood handling in one audit boundary"
)]
fn relink_affected_graph(
    graph: &mut GraphAssembly,
    plan: &IncrementalPlan,
    batch_plan: &ExtractorBatchPlan,
    focused: &FocusedBatchState,
    _previous_nodes: &[Node],
    previous_edges: &[Edge],
) -> Result<(), ApplicationError> {
    let full_relink = focused.force_relink
        || plan.changes.iter().any(|change| {
            change.kind != code_system_graph_model::ArtifactChangeKind::Unchanged
                && change.extractor != "code-system-graph.packages"
                && !source_extractor(&change.extractor)
        });
    if full_relink || previous_edges.is_empty() {
        return Ok(());
    }
    let source_plan = ExtractorBatchPlan {
        batches: batch_plan
            .batches
            .iter()
            .filter(|batch| source_extractor(&batch.key.extractor))
            .cloned()
            .collect(),
    };
    let affected = affected_link_keys(
        &source_plan,
        &focused.previous_source_batches,
        &focused.source_batches,
        |observation| {
            observation
                .method
                .as_ref()
                .zip(observation.path.as_ref())
                .map(|(method, path)| format!("{method}:{path}"))
        },
    )?
    .into_iter()
    .flatten()
    .collect::<BTreeSet<_>>();
    if affected.is_empty() {
        return Ok(());
    }

    let mut previous_node_keys = graph.link_node_keys.clone();
    for batch in &focused.previous_source_batches {
        let facts = source_observations_to_graph(
            &batch.source.repo_id,
            &portable_path(&batch.source.path.display),
            &batch.source.content_hash,
            &batch.outputs,
        );
        previous_node_keys.extend(facts.boundaries.iter().map(|boundary| {
            (
                boundary.node.id.clone(),
                format!("{}:{}", boundary.method, boundary.path),
            )
        }));
        previous_node_keys.extend(facts.tests.iter().map(|test| {
            (
                test.node.id.clone(),
                format!("{}:{}", test.method, test.path),
            )
        }));
        previous_node_keys.extend(facts.implementations.iter().map(|implementation| {
            (
                implementation.node.id.clone(),
                format!("{}:{}", implementation.method, implementation.path),
            )
        }));
    }
    let current_node_ids = graph
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let is_http_link = |edge: &Edge| {
        matches!(
            edge.kind,
            code_system_graph_model::EdgeKind::CallsRemote
                | code_system_graph_model::EdgeKind::Validates
                | code_system_graph_model::EdgeKind::ImplementedBy
        )
    };
    let recomputed_http = graph
        .edges
        .iter()
        .filter(|edge| is_http_link(edge))
        .cloned()
        .collect::<Vec<_>>();
    let previous_http = previous_edges
        .iter()
        .filter(|edge| is_http_link(edge))
        .cloned()
        .collect::<Vec<_>>();
    let mut edges = graph
        .edges
        .iter()
        .filter(|edge| !is_http_link(edge))
        .cloned()
        .collect::<Vec<_>>();
    edges.extend(merge_affected_link_neighborhoods(
        &previous_http,
        &recomputed_http,
        &affected,
        &previous_node_keys,
        &graph.link_node_keys,
        &current_node_ids,
    ));
    edges.sort_by(|left, right| left.id.cmp(&right.id));
    edges.dedup_by(|left, right| left.id == right.id);
    graph.edges = edges;
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "Optional provider orchestration keeps budgets, isolation, and degradation accounting together"
)]
fn run_codegraph_corroboration(
    context: &WorkspaceContext,
    focused: &FocusedBatchState,
    plan: &IncrementalPlan,
    codegraph_binary: Option<PathBuf>,
) -> CorroborationSummary {
    let mut jobs = Vec::new();
    let mut setup_degradations = Vec::new();
    for repository in &context.registry.record.repositories {
        let Some(project_path) = context.registry.checkout_path(&repository.alias) else {
            setup_degradations.push(format!(
                "CodeGraph skipped `{}` because its checkout is unavailable",
                repository.alias
            ));
            continue;
        };
        let mut anchors = focused
            .source_batches
            .iter()
            .filter(|batch| batch.source.repo_id == repository.id)
            .flat_map(|batch| {
                let source_path = portable_path(&batch.source.path.display);
                batch.outputs.iter().filter_map(move |observation| {
                    if observation.role != SourceRole::Provider {
                        return None;
                    }
                    Some(SymbolAnchor {
                        symbol: observation.symbol_name.clone()?,
                        source_path: source_path.clone(),
                        start_line: usize::try_from(observation.lines.start).ok()?,
                    })
                })
            })
            .collect::<Vec<_>>();
        anchors.sort_by(|left, right| {
            (&left.source_path, left.start_line, &left.symbol).cmp(&(
                &right.source_path,
                right.start_line,
                &right.symbol,
            ))
        });
        anchors.dedup();
        if anchors.len() > 50 {
            anchors.truncate(50);
            setup_degradations.push(format!(
                "CodeGraph symbol corroboration for `{}` was limited to 50 anchors",
                repository.alias
            ));
        }
        let mut changed_files = plan
            .changes
            .iter()
            .filter(|change| repository.id == change.repo_id)
            .filter(|change| change.kind != code_system_graph_model::ArtifactChangeKind::Unchanged)
            .map(|change| portable_path(&change.path.display))
            .collect::<Vec<_>>();
        changed_files.sort();
        changed_files.dedup();
        if changed_files.len() > 1_024 {
            changed_files.truncate(1_024);
            setup_degradations.push(format!(
                "CodeGraph affected-test corroboration for `{}` was limited to 1,024 changed files",
                repository.alias
            ));
        }
        if !anchors.is_empty() || !changed_files.is_empty() {
            jobs.push((
                repository.id.clone(),
                project_path.to_path_buf(),
                anchors,
                changed_files,
            ));
        }
    }
    let worker = std::thread::spawn(move || -> Result<Vec<RepositoryCorroboration>, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("cannot start CodeGraph corroboration runtime: {error}"))?;
        runtime.block_on(async move {
            let mut config = CodeGraphConfig::default();
            if let Some(binary) = codegraph_binary {
                config.binary = binary.into_os_string();
            }
            let provider = CodeGraphProvider::new(config).map_err(|error| error.to_string())?;
            let mut reports = Vec::new();
            for (repo_id, project_path, anchors, changed_files) in jobs {
                let report = corroborate_repository(
                    &provider,
                    repo_id.clone(),
                    project_path,
                    &anchors,
                    &changed_files,
                    ProviderBudget {
                        timeout: Duration::from_secs(2),
                        max_output_bytes: 256 * 1024,
                        max_items: 20,
                    },
                    CancellationToken::new(),
                )
                .await;
                reports.push(RepositoryCorroboration { repo_id, report });
            }
            provider
                .shutdown()
                .await
                .map_err(|error| error.to_string())?;
            Ok(reports)
        })
    });
    let mut summary = CorroborationSummary {
        degradations: setup_degradations,
        ..CorroborationSummary::default()
    };
    match worker.join() {
        Ok(Ok(reports)) => {
            summary.confirmed_symbols = reports
                .iter()
                .flat_map(|item| &item.report.symbols)
                .filter(|symbol| matches!(symbol, SymbolCorroboration::Confirmed { .. }))
                .count();
            summary.affected_tests = reports
                .iter()
                .map(|item| item.report.affected_tests.len())
                .sum();
            summary.degradations.extend(
                reports
                    .iter()
                    .flat_map(|item| item.report.degradations.iter().cloned()),
            );
            summary.reports = reports;
        }
        Ok(Err(error)) => summary.degradations.push(error),
        Err(_) => summary
            .degradations
            .push("CodeGraph corroboration worker terminated unexpectedly".to_owned()),
    }
    summary.degradations.sort();
    summary.degradations.dedup();
    summary
}

fn apply_codegraph_corroboration(graph: &mut GraphAssembly, reports: &[RepositoryCorroboration]) {
    for item in reports {
        for outcome in &item.report.symbols {
            let SymbolCorroboration::Confirmed {
                symbol,
                source_path,
                local_id,
            } = outcome
            else {
                continue;
            };
            let implementation_ids = ["javascript", "typescript", "rust", "python", "go", "java"]
                .map(|language| {
                    NodeId::new(stable_id(
                        "node",
                        &format!(
                            "symbol:{}:{language}:{source_path}:{symbol}",
                            item.repo_id.as_str()
                        ),
                    ))
                })
                .into_iter()
                .filter(|candidate| {
                    graph
                        .nodes
                        .iter()
                        .any(|node| node.id == *candidate && node.kind == NodeKind::SymbolRef)
                })
                .collect::<BTreeSet<_>>();
            if implementation_ids.is_empty() {
                continue;
            }
            let source_evidence = graph.evidence.iter().find(|evidence| {
                evidence.repo_id.as_ref() == Some(&item.repo_id)
                    && evidence.file_path.as_deref() == Some(source_path)
                    && evidence.extractor.starts_with("code-system-graph.source.")
            });
            let (start_line, end_line, content_hash) =
                source_evidence.map_or((None, None, None), |evidence| {
                    (
                        evidence.start_line,
                        evidence.end_line,
                        evidence.content_hash.clone(),
                    )
                });
            let evidence_key = format!(
                "codegraph:{}:{source_path}:{symbol}:{}",
                item.repo_id.as_str(),
                local_id.as_deref().unwrap_or("anonymous")
            );
            let evidence = Evidence {
                id: EvidenceId::new(stable_id("evidence", &evidence_key)),
                repo_id: Some(item.repo_id.clone()),
                file_path: Some(source_path.clone()),
                start_line,
                end_line,
                extractor: "code-system-graph.codegraph.corroboration".to_owned(),
                extractor_version: "1.0.0".to_owned(),
                provenance: Provenance::CodeGraph,
                confidence: 1.0,
                observed_at_commit: None,
                content_hash,
                note: Some("exact provider symbol name, path, and line match".to_owned()),
            };
            for edge in graph.edges.iter_mut().filter(|edge| {
                edge.kind == code_system_graph_model::EdgeKind::ImplementedBy
                    && implementation_ids.contains(&edge.target)
            }) {
                if !edge.evidence.contains(&evidence.id) {
                    edge.evidence.push(evidence.id.clone());
                    edge.evidence.sort();
                }
            }
            graph.evidence.push(evidence);
        }
    }
    graph.evidence.sort_by(|left, right| left.id.cmp(&right.id));
    graph.evidence.dedup_by(|left, right| left.id == right.id);
}

fn persisted_manual_link_records(
    snapshot_id: &str,
    decisions: &[LinkDecision],
) -> Result<Vec<ManualLinkRecord>, ApplicationError> {
    decisions
        .iter()
        .map(|decision| {
            let disposition = match decision.status {
                LinkStatus::Confirmed => ManualLinkDisposition::Active,
                LinkStatus::Suppressed => ManualLinkDisposition::Suppression,
                LinkStatus::Ambiguous | LinkStatus::Rejected => {
                    return Err(ApplicationError::Initialization(
                        "unapplied manual-link decision reached snapshot publication".to_owned(),
                    ));
                }
            };
            let kind = serde_json::to_value(decision.relation)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .ok_or_else(|| {
                    ApplicationError::Initialization(
                        "manual-link relation could not be encoded".to_owned(),
                    )
                })?;
            let reason = decision.reasons.first().cloned().ok_or_else(|| {
                ApplicationError::Initialization(
                    "manual-link decision omitted its required rationale".to_owned(),
                )
            })?;
            let identity = format!(
                "{}:{kind}:{}:{}:{reason}",
                decision.source.as_str(),
                decision.target.as_str(),
                match disposition {
                    ManualLinkDisposition::Active => "active",
                    ManualLinkDisposition::Suppression => "suppression",
                }
            );
            Ok(ManualLinkRecord {
                id: stable_id("manual-link", &identity),
                snapshot_id: snapshot_id.to_owned(),
                source_node_id: decision.source.clone(),
                target_node_id: decision.target.clone(),
                kind,
                disposition,
                reason,
                decision: decision.clone(),
                config_version: 1,
            })
        })
        .collect()
}

fn provider_capability_record(
    workspace: &str,
    repo_id: &RepoId,
    capability: &ProviderCapability,
) -> ProviderCapabilityRecord {
    let mut capabilities = Vec::new();
    if let Some(status) = serialized_enum_name(capability.status) {
        capabilities.push(format!("status:{status}"));
    }
    capabilities.extend(
        capability
            .tools
            .iter()
            .filter(|tool| tool.len() <= 240)
            .map(|tool| format!("tool:{tool}")),
    );
    for operation in &capability.operations {
        if let (Some(operation_name), Some(transport)) = (
            serialized_enum_name(operation.operation),
            serialized_enum_name(operation.transport),
        ) {
            capabilities.push(format!("operation:{operation_name}:{transport}"));
        }
    }
    capabilities.sort();
    capabilities.dedup();
    ProviderCapabilityRecord {
        workspace_name: workspace.to_owned(),
        repo_id: repo_id.clone(),
        provider: capability.provider.clone(),
        provider_version: capability
            .version
            .clone()
            .unwrap_or_else(|| "unknown".to_owned()),
        capabilities,
        observed_at_unix_ms: current_unix_millis(),
    }
}

fn community_topology_unchanged(
    previous_nodes: &[Node],
    previous_edges: &[Edge],
    current_nodes: &[Node],
    current_edges: &[Edge],
) -> bool {
    previous_nodes == current_nodes
        && previous_edges.len() == current_edges.len()
        && previous_edges
            .iter()
            .zip(current_edges)
            .all(|(previous, current)| {
                previous.id == current.id
                    && previous.source == current.source
                    && previous.target == current.target
                    && previous.kind == current.kind
                    && previous.confidence.to_bits() == current.confidence.to_bits()
                    && previous.status == current.status
            })
}

fn serialized_enum_name<T: Serialize>(value: T) -> Option<String> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
}

fn extract_boundaries(context: &WorkspaceContext) -> Result<Vec<HttpBoundary>, ApplicationError> {
    let repository_records = context
        .registry
        .record
        .repositories
        .iter()
        .map(|repository| (repository.alias.as_str(), repository))
        .collect::<BTreeMap<_, _>>();
    let mut boundaries = Vec::new();
    for alias in context.manifest.repos.keys() {
        let registered = repository_records
            .get(alias.as_str())
            .ok_or_else(|| ApplicationError::RegistryAliasMissing(alias.clone()))?;
        let effective = context
            .repository_configs
            .get(alias)
            .ok_or_else(|| ApplicationError::RegistryAliasMissing(alias.clone()))?;
        boundaries.extend(
            effective
                .http_consumers
                .iter()
                .map(|consumer| HttpBoundary::consumer(registered.id.clone(), consumer)),
        );
        let repository_path = context
            .registry
            .checkout_path(alias)
            .ok_or_else(|| ApplicationError::RegistryAliasMissing(alias.clone()))?;
        for openapi in &effective.openapi {
            let openapi_path = repository_path.join(openapi);
            let openapi_source = read_file(&openapi_path)?;
            boundaries.extend(extract_openapi(&registered.id, openapi, &openapi_source)?);
        }
    }
    Ok(boundaries)
}

fn extract_declared_tests(
    context: &WorkspaceContext,
    fingerprints: &[ArtifactFingerprint],
) -> Result<Vec<DeclaredTestCase>, ApplicationError> {
    let repository_records = context
        .registry
        .record
        .repositories
        .iter()
        .map(|repository| (repository.alias.as_str(), repository))
        .collect::<BTreeMap<_, _>>();
    let mut tests = Vec::new();
    for alias in context.manifest.repos.keys() {
        let repository = repository_records
            .get(alias.as_str())
            .ok_or_else(|| ApplicationError::RegistryAliasMissing(alias.clone()))?;
        let effective = context
            .repository_configs
            .get(alias)
            .ok_or_else(|| ApplicationError::RegistryAliasMissing(alias.clone()))?;
        tests.extend(effective.integration_tests.iter().map(|test| {
            let content_hash = artifact_content_hash(
                fingerprints,
                &repository.id,
                "code-system-graph.tests.declared",
                &test.path,
            );
            declared_test_case(repository.id.clone(), test, content_hash)
        }));
    }
    tests.sort_by(|left, right| left.node.id.cmp(&right.node.id));
    Ok(tests)
}

fn extract_declared_implementations(
    context: &WorkspaceContext,
    fingerprints: &[ArtifactFingerprint],
) -> Result<Vec<DeclaredImplementation>, ApplicationError> {
    let repository_records = context
        .registry
        .record
        .repositories
        .iter()
        .map(|repository| (repository.alias.as_str(), repository))
        .collect::<BTreeMap<_, _>>();
    let mut implementations = Vec::new();
    for alias in context.manifest.repos.keys() {
        let repository = repository_records
            .get(alias.as_str())
            .ok_or_else(|| ApplicationError::RegistryAliasMissing(alias.clone()))?;
        let effective = context
            .repository_configs
            .get(alias)
            .ok_or_else(|| ApplicationError::RegistryAliasMissing(alias.clone()))?;
        implementations.extend(effective.implementations.iter().map(|implementation| {
            let content_hash = artifact_content_hash(
                fingerprints,
                &repository.id,
                "code-system-graph.implementations.declared",
                &implementation.path,
            );
            declared_implementation(repository.id.clone(), implementation, content_hash)
        }));
    }
    implementations.sort_by(|left, right| left.node.id.cmp(&right.node.id));
    Ok(implementations)
}

fn artifact_content_hash(
    fingerprints: &[ArtifactFingerprint],
    repo_id: &code_system_graph_model::RepoId,
    extractor: &str,
    path: &str,
) -> Option<String> {
    fingerprints
        .iter()
        .find(|fingerprint| {
            &fingerprint.repo_id == repo_id
                && fingerprint.extractor == extractor
                && portable_path(&fingerprint.path.display) == portable_path(path)
        })
        .map(|fingerprint| fingerprint.content_hash.clone())
}

fn portable_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn cargo_crate_root(checkout: &Path, source_path: &Path) -> String {
    source_path
        .parent()
        .into_iter()
        .flat_map(Path::ancestors)
        .take_while(|candidate| candidate.starts_with(checkout))
        .find(|candidate| candidate.join("Cargo.toml").is_file())
        .and_then(|candidate| candidate.strip_prefix(checkout).ok())
        .map(|relative| portable_path(&relative.to_string_lossy()))
        .unwrap_or_default()
}

fn discover_artifact_fingerprints(
    context: &WorkspaceContext,
) -> Result<Vec<ArtifactFingerprint>, ApplicationError> {
    let repository_records = context
        .registry
        .record
        .repositories
        .iter()
        .map(|repository| (repository.alias.as_str(), repository))
        .collect::<BTreeMap<_, _>>();
    let mut fingerprints = BTreeMap::new();
    for alias in context.manifest.repos.keys() {
        let repository = repository_records
            .get(alias.as_str())
            .ok_or_else(|| ApplicationError::RegistryAliasMissing(alias.clone()))?;
        let checkout_path = context
            .registry
            .checkout_path(alias)
            .ok_or_else(|| ApplicationError::RegistryAliasMissing(alias.clone()))?;
        let effective = context
            .repository_configs
            .get(alias)
            .ok_or_else(|| ApplicationError::RegistryAliasMissing(alias.clone()))?;
        for openapi in &effective.openapi {
            let fingerprint = fingerprint_artifact(
                repository,
                checkout_path,
                Path::new(openapi),
                "code-system-graph.http.openapi",
            )?;
            fingerprints.insert(artifact_key(&fingerprint), fingerprint);
        }
        for consumer in &effective.http_consumers {
            let fingerprint = fingerprint_artifact(
                repository,
                checkout_path,
                Path::new(&consumer.source),
                "code-system-graph.http.declared",
            )?;
            fingerprints.insert(artifact_key(&fingerprint), fingerprint);
        }
        for test in &effective.integration_tests {
            let fingerprint = fingerprint_artifact(
                repository,
                checkout_path,
                Path::new(&test.path),
                "code-system-graph.tests.declared",
            )?;
            fingerprints.insert(artifact_key(&fingerprint), fingerprint);
        }
        for implementation in &effective.implementations {
            let fingerprint = fingerprint_artifact(
                repository,
                checkout_path,
                Path::new(&implementation.path),
                "code-system-graph.implementations.declared",
            )?;
            fingerprints.insert(artifact_key(&fingerprint), fingerprint);
        }
        for (relative_path, extractor) in
            discover_focused_artifacts(checkout_path, alias, &effective.ignore_policy)?
        {
            let fingerprint =
                fingerprint_artifact(repository, checkout_path, &relative_path, extractor)?;
            fingerprints.insert(artifact_key(&fingerprint), fingerprint);
        }
    }
    Ok(fingerprints.into_values().collect())
}

fn discover_focused_artifacts(
    checkout_path: &Path,
    repository_alias: &str,
    ignore_policy: &IgnorePolicy,
) -> Result<Vec<(PathBuf, &'static str)>, ApplicationError> {
    let mut pending = vec![checkout_path.to_path_buf()];
    let mut discovered = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|source| ApplicationError::ReadFile {
            path: directory.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| ApplicationError::ReadFile {
                path: directory.clone(),
                source,
            })?;
            let file_type = entry
                .file_type()
                .map_err(|source| ApplicationError::ReadFile {
                    path: entry.path(),
                    source,
                })?;
            if file_type.is_symlink() {
                continue;
            }
            let path = entry.path();
            let relative = path.strip_prefix(checkout_path).map_err(|_| {
                ApplicationError::ArtifactOutsideCheckout {
                    path: path.clone(),
                    checkout: checkout_path.to_path_buf(),
                }
            })?;
            if file_type.is_dir() {
                if !ignore_policy.excludes(relative, true) {
                    pending.push(path);
                }
                continue;
            }
            if !file_type.is_file() || ignore_policy.excludes(relative, false) {
                continue;
            }
            for extractor in focused_extractors_for_path(&path) {
                discovered.push((relative.to_path_buf(), extractor));
                if discovered.len() > MAX_DISCOVERED_FILES_PER_REPOSITORY {
                    return Err(ApplicationError::ArtifactInventoryLimit {
                        repository: repository_alias.to_owned(),
                        maximum: MAX_DISCOVERED_FILES_PER_REPOSITORY,
                    });
                }
            }
        }
    }
    discovered.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(right.1)));
    Ok(discovered)
}

fn focused_extractors_for_path(path: &Path) -> Vec<&'static str> {
    let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
        return Vec::new();
    };
    let mut extractors = Vec::new();
    let is_generated_client_metadata = name == "openapitools.json"
        || (matches!(name, "FILES" | "VERSION")
            && path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|parent| parent == ".openapi-generator"));
    if is_generated_client_metadata {
        extractors.push("code-system-graph.http.generated-client");
    }
    let extension = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase);
    let source_extractor = match extension.as_deref() {
        Some("js" | "jsx") => Some("code-system-graph.source.javascript"),
        Some("ts" | "tsx") => Some("code-system-graph.source.typescript"),
        Some("rs") => Some("code-system-graph.source.rust"),
        Some("py") => Some("code-system-graph.source.python"),
        Some("go") => Some("code-system-graph.source.go"),
        Some("java") => Some("code-system-graph.source.java"),
        _ => None,
    };
    if let Some(extractor) = source_extractor {
        extractors.extend([
            extractor,
            "code-system-graph.events.source",
            "code-system-graph.graphql.source",
            "code-system-graph.protobuf.generated",
            "code-system-graph.data.source",
        ]);
    }
    if matches!(extension.as_deref(), Some("graphql" | "gql")) {
        extractors.push("code-system-graph.graphql.document");
    }
    if extension.as_deref() == Some("proto") {
        extractors.push("code-system-graph.protobuf");
    }
    let lower_name = name.to_ascii_lowercase();
    if matches!(
        lower_name.as_str(),
        "asyncapi.yaml" | "asyncapi.yml" | "asyncapi.json"
    ) {
        extractors.push("code-system-graph.events.asyncapi");
    }
    if matches!(
        lower_name.as_str(),
        "persisted-queries.json"
            | "persisted_queries.json"
            | "apollo-manifest.json"
            | "operation-manifest.json"
    ) {
        extractors.push("code-system-graph.graphql.persisted");
    }
    extractors.extend(document_extractors_for_path(
        path,
        name,
        extension.as_deref(),
    ));
    let is_package_artifact = matches!(
        name,
        "package.json"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "pyproject.toml"
            | "poetry.lock"
            | "Cargo.toml"
            | "Cargo.lock"
            | "go.mod"
            | "go.work"
            | "pom.xml"
            | "build.gradle"
            | "build.gradle.kts"
            | "packages.config"
    ) || extension.as_deref() == Some("csproj")
        || (name.starts_with("requirements")
            && Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("txt")));
    if is_package_artifact {
        extractors.push("code-system-graph.packages");
    }
    extractors.sort_unstable();
    extractors.dedup();
    extractors
}

fn document_extractors_for_path(
    path: &Path,
    name: &str,
    extension: Option<&str>,
) -> Vec<&'static str> {
    let mut extractors = Vec::new();
    let lower_name = name.to_ascii_lowercase();
    let lower_components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    let in_database_migrations = lower_components
        .iter()
        .any(|component| matches!(component.as_str(), "alembic" | "migrations"));
    if matches!(extension, Some("sql" | "prisma"))
        || lower_name == "sqlx.toml"
        || lower_name == "schema.rs"
        || (extension == Some("py")
            && (in_database_migrations || matches!(lower_name.as_str(), "model.py" | "models.py")))
    {
        extractors.push("code-system-graph.data.artifact");
    }

    let is_compose = matches!(
        lower_name.as_str(),
        "compose.yaml"
            | "compose.yml"
            | "compose.json"
            | "docker-compose.yaml"
            | "docker-compose.yml"
            | "docker-compose.json"
    );
    if is_compose {
        extractors.push("code-system-graph.infrastructure.compose");
    }
    if extension == Some("tf") {
        extractors.push("code-system-graph.infrastructure.terraform");
    }
    let in_helm_templates = lower_components
        .iter()
        .any(|component| component == "templates");
    let is_helm = in_helm_templates
        || matches!(
            lower_name.as_str(),
            "chart.yaml" | "chart.yml" | "values.yaml" | "values.yml"
        );
    if is_helm {
        extractors.push("code-system-graph.infrastructure.helm");
    } else if matches!(extension, Some("yaml" | "yml" | "json"))
        && !is_compose
        && (lower_components
            .iter()
            .any(|component| matches!(component.as_str(), "k8s" | "kubernetes" | "manifests"))
            || [
                "deployment",
                "service",
                "ingress",
                "statefulset",
                "daemonset",
                "cronjob",
                "job",
                "pod",
            ]
            .iter()
            .any(|kind| lower_name.starts_with(kind)))
    {
        extractors.push("code-system-graph.infrastructure.kubernetes");
    }

    if extension == Some("md") || lower_name == "markdown" {
        extractors.push("code-system-graph.documents.markdown");
    }
    if lower_name == "codeowners" {
        extractors.push("code-system-graph.documents.codeowners");
    }
    if matches!(
        lower_name.as_str(),
        "catalog.yaml"
            | "catalog.yml"
            | "catalog.json"
            | "catalog-info.yaml"
            | "catalog-info.yml"
            | "service-catalog.yaml"
            | "service-catalog.yml"
            | "service-catalog.json"
    ) {
        extractors.push("code-system-graph.documents.catalog");
    }

    let is_named_config = [
        "config.",
        "settings.",
        "application.",
        "values.",
        "secrets.",
    ]
    .iter()
    .any(|prefix| lower_name.starts_with(prefix));
    if lower_name == ".env"
        || lower_name.starts_with(".env.")
        || (is_named_config && matches!(extension, Some("yaml" | "yml" | "json" | "toml")))
    {
        extractors.push("code-system-graph.config.safe");
    }
    extractors
}

fn fingerprint_artifact(
    repository: &RepositoryRecord,
    checkout_path: &Path,
    relative_path: &Path,
    extractor: &str,
) -> Result<ArtifactFingerprint, ApplicationError> {
    let configured_path = checkout_path.join(relative_path);
    let canonical_path =
        std::fs::canonicalize(&configured_path).map_err(|source| ApplicationError::ReadFile {
            path: configured_path.clone(),
            source,
        })?;
    if !canonical_path.starts_with(checkout_path) {
        return Err(ApplicationError::ArtifactOutsideCheckout {
            path: configured_path,
            checkout: checkout_path.to_path_buf(),
        });
    }
    let metadata =
        std::fs::metadata(&canonical_path).map_err(|source| ApplicationError::ReadFile {
            path: canonical_path.clone(),
            source,
        })?;
    if metadata.len() > MAX_ARTIFACT_BYTES {
        return Err(ApplicationError::ArtifactTooLarge {
            path: canonical_path,
            size: metadata.len(),
            maximum: MAX_ARTIFACT_BYTES,
        });
    }
    let content = std::fs::read(&canonical_path).map_err(|source| ApplicationError::ReadFile {
        path: canonical_path.clone(),
        source,
    })?;
    let relative = canonical_path.strip_prefix(checkout_path).map_err(|_| {
        ApplicationError::ArtifactOutsideCheckout {
            path: canonical_path.clone(),
            checkout: checkout_path.to_path_buf(),
        }
    })?;
    Ok(ArtifactFingerprint {
        repo_id: repository.id.clone(),
        checkout_id: repository.checkout_id.clone(),
        path: encode_native_path(relative),
        extractor: extractor.to_owned(),
        content_hash: stable_id_bytes("artifact-content", &content),
        size_bytes: metadata.len(),
    })
}

fn artifact_key(
    fingerprint: &ArtifactFingerprint,
) -> (CheckoutId, code_system_graph_model::NativePath, String) {
    (
        fingerprint.checkout_id.clone(),
        fingerprint.path.clone(),
        fingerprint.extractor.clone(),
    )
}

fn extractor_runs(
    snapshot_id: &str,
    fingerprints: &[ArtifactFingerprint],
    plan: &IncrementalPlan,
) -> Vec<ExtractorRun> {
    let actions = plan
        .changes
        .iter()
        .map(|change| {
            (
                ArtifactKey {
                    repo_id: change.repo_id.clone(),
                    checkout_id: change.checkout_id.clone(),
                    path: change.path.clone(),
                    extractor: change.extractor.clone(),
                },
                change.kind,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut groups = BTreeMap::<(String, String, String), Vec<&ArtifactFingerprint>>::new();
    for fingerprint in fingerprints {
        groups
            .entry((
                fingerprint.repo_id.as_str().to_owned(),
                fingerprint.checkout_id.as_str().to_owned(),
                fingerprint.extractor.clone(),
            ))
            .or_default()
            .push(fingerprint);
    }
    groups
        .into_iter()
        .map(|((repo_id, checkout_id, extractor), inputs)| {
            let changed = inputs
                .iter()
                .filter(|fingerprint| {
                    actions.get(&ArtifactKey::from(**fingerprint))
                        != Some(&code_system_graph_model::ArtifactChangeKind::Unchanged)
                })
                .count();
            let discovered_files = u64::try_from(inputs.len()).unwrap_or(u64::MAX);
            let parsed_files = u64::try_from(changed).unwrap_or(u64::MAX);
            let skipped_files = discovered_files.saturating_sub(parsed_files);
            let key = format!("{snapshot_id}:{repo_id}:{checkout_id}:{extractor}");
            ExtractorRun {
                id: stable_id("extractor-run", &key),
                snapshot_id: snapshot_id.to_owned(),
                repo_id: RepoId::new(repo_id),
                checkout_id: CheckoutId::new(checkout_id),
                extractor_version: if focused_extractor(&extractor) {
                    FOCUSED_EXTRACTOR_VERSION.to_owned()
                } else {
                    env!("CARGO_PKG_VERSION").to_owned()
                },
                extractor,
                status: if changed == 0 {
                    ExtractorRunStatus::SkippedUnchanged
                } else {
                    ExtractorRunStatus::Success
                },
                discovered_files,
                parsed_files,
                skipped_files,
                elapsed_ms: 0,
            }
        })
        .collect()
}

fn read_file(path: &Path) -> Result<String, ApplicationError> {
    std::fs::read_to_string(path).map_err(|source| ApplicationError::ReadFile {
        path: path.to_path_buf(),
        source,
    })
}

fn read_source_file(path: &Path) -> Result<(String, bool), ApplicationError> {
    let bytes = std::fs::read(path).map_err(|source| ApplicationError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;
    match String::from_utf8(bytes) {
        Ok(source) => Ok((source, false)),
        Err(error) => Ok((String::from_utf8_lossy(error.as_bytes()).into_owned(), true)),
    }
}

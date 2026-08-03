//! Source-free application-layer contracts and deterministic renderers.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use code_system_graph_model::{
    Edge, EdgeId, EpistemicStatus, Evidence, EvidenceId, Node, NodeId, NodeKind, Provenance, RepoFreshnessState, RepoId
};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ChangeImpactReport, ChangeRequest, CompatibilityReport, CompatibilityStatus, ImpactReport, ImpactRequest, PullRequestInspection, SearchReport, SearchRequest, TraversalReport, TraversalRequest
};

/// Schema version shared by the application-layer contracts in this module.
pub const INTERFACE_SCHEMA_VERSION: u32 = 1;
/// Result version shared by the application-layer reports in this module.
pub const INTERFACE_RESULT_VERSION: u32 = 1;
/// Version of reusable delivery metadata.
pub const DELIVERY_METADATA_VERSION: u32 = 1;
/// Hard maximum number of nodes accepted by one graph export.
pub const MAX_EXPORT_NODES: usize = 100_000;
/// Hard maximum number of edges accepted by one graph export.
pub const MAX_EXPORT_EDGES: usize = 1_000_000;
const DEFAULT_EXPORT_NODES: usize = 10_000;
const DEFAULT_EXPORT_EDGES: usize = 50_000;

/// Contract operation requested by a delivery adapter.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum ContractAction {
    /// List contract nodes in stable order.
    List,
    /// Show one contract and its directly observed relationships.
    Show,
    /// Validate graph and evidence integrity for all or one contract.
    Validate,
    /// Compare two contract nodes and surface supplied compatibility analysis.
    Diff,
    /// Explain direct links between two contract nodes.
    ExplainLink,
}

/// Bounded input for one contract application operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractRequest {
    /// Requested operation.
    pub action: ContractAction,
    /// Primary contract for `show`, targeted `validate`, `diff`, or `explain-link`.
    pub contract: Option<NodeId>,
    /// Comparison or link endpoint required by `diff` and `explain-link`.
    pub related_contract: Option<NodeId>,
    /// Maximum number of contracts or links returned.
    pub limit: usize,
}

impl Default for ContractRequest {
    fn default() -> Self {
        Self {
            action: ContractAction::List,
            contract: None,
            related_contract: None,
            limit: 100,
        }
    }
}

/// Compatibility result associated with one exact graph contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractCompatibility {
    /// Contract node receiving the compatibility result.
    pub contract_id: NodeId,
    /// Existing conservative compatibility report.
    pub report: CompatibilityReport,
}

/// Evidence location and provenance safe for public delivery.
///
/// Explanatory notes and source bodies are deliberately absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EvidenceMetadata {
    /// Stable evidence identifier.
    pub id: EvidenceId,
    /// Repository containing the observation.
    pub repo_id: Option<RepoId>,
    /// Repository-relative path, when available.
    pub file_path: Option<String>,
    /// Inclusive first observed line.
    pub start_line: Option<u32>,
    /// Inclusive last observed line.
    pub end_line: Option<u32>,
    /// Extractor identifier.
    pub extractor: String,
    /// Extractor semantic version.
    pub extractor_version: String,
    /// Observation provenance.
    pub provenance: Provenance,
    /// Normalized confidence.
    pub confidence: f32,
    /// Commit at which the location was observed.
    pub observed_at_commit: Option<String>,
    /// Hash of relevant content, never the content itself.
    pub content_hash: Option<String>,
}

impl From<&Evidence> for EvidenceMetadata {
    fn from(value: &Evidence) -> Self {
        Self {
            id: value.id.clone(),
            repo_id: value.repo_id.clone(),
            file_path: value.file_path.clone(),
            start_line: value.start_line,
            end_line: value.end_line,
            extractor: value.extractor.clone(),
            extractor_version: value.extractor_version.clone(),
            provenance: value.provenance,
            confidence: value.confidence,
            observed_at_commit: value.observed_at_commit.clone(),
            content_hash: value.content_hash.clone(),
        }
    }
}

/// Source-free relationship explanation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContractLink {
    /// Exact graph relationship.
    pub edge: Edge,
    /// Deterministically ordered supporting evidence metadata.
    pub evidence: Vec<EvidenceMetadata>,
}

/// Public projection of one contract node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContractView {
    /// Exact source-free graph node.
    pub contract: Node,
    /// Direct incoming and outgoing relationships.
    pub links: Vec<ContractLink>,
}

/// Compatibility finding with untrusted evidence text removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractFinding {
    /// Stable compatibility rule code.
    pub code: String,
    /// Contract coordinate affected by the finding.
    pub path: String,
    /// Conservative classification.
    pub status: CompatibilityStatus,
    /// Bounded factors supplied by the compatibility engine.
    pub factors: Vec<String>,
    /// Non-executing recommended validations.
    pub recommended_validations: Vec<String>,
}

/// Source-free compatibility projection for one contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractCompatibilitySummary {
    /// Exact contract identifier.
    pub contract_id: NodeId,
    /// Aggregate conservative status.
    pub status: CompatibilityStatus,
    /// Fingerprint of the previous structured contract.
    pub before_fingerprint: String,
    /// Fingerprint of the candidate structured contract.
    pub after_fingerprint: String,
    /// Deterministically ordered findings without evidence text.
    pub findings: Vec<ContractFinding>,
}

/// One structural difference between two contract nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractDifference {
    /// Stable field name.
    pub field: String,
    /// Previous source-free value.
    pub before: Option<String>,
    /// Candidate source-free value.
    pub after: Option<String>,
}

/// Severity of a contract validation issue.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ContractIssueSeverity {
    /// Coverage is insufficient for a definitive result.
    Unknown,
    /// Input is usable but degraded.
    Warning,
    /// Input violates a graph or evidence invariant.
    Error,
}

/// One stable contract validation or coverage issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractIssue {
    /// Stable machine-readable code.
    pub code: String,
    /// Issue severity.
    pub severity: ContractIssueSeverity,
    /// Affected source-free entity identifier.
    pub entity_id: Option<String>,
    /// Bounded explanation.
    pub message: String,
}

/// Result of one contract application operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContractReport {
    /// Public schema version.
    pub schema_version: u32,
    /// Public result version.
    pub result_version: u32,
    /// Completed operation.
    pub action: ContractAction,
    /// Selected contracts.
    pub contracts: Vec<ContractView>,
    /// Direct links selected by `explain-link`.
    pub links: Vec<ContractLink>,
    /// Supplied source-free compatibility results.
    pub compatibility: Vec<ContractCompatibilitySummary>,
    /// Structural differences selected by `diff`.
    pub differences: Vec<ContractDifference>,
    /// Validation and coverage issues.
    pub issues: Vec<ContractIssue>,
    /// `Some(true)` only when validation completed without unknowns or errors.
    pub valid: Option<bool>,
    /// Whether all required inputs were observed.
    pub complete: bool,
    /// Whether the requested result exceeded its bound.
    pub truncated: bool,
}

/// Supported deterministic graph rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// Versioned JSON graph payload.
    Json,
    /// `GraphML` document with escaped XML text.
    GraphMl,
    /// Markdown tables with escaped cell text.
    Markdown,
}

/// Explicit bounds for one graph export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExportRequest {
    /// Output representation.
    pub format: ExportFormat,
    /// Maximum number of nodes included.
    pub max_nodes: usize,
    /// Maximum number of edges included after node selection.
    pub max_edges: usize,
}

impl Default for ExportRequest {
    fn default() -> Self {
        Self {
            format: ExportFormat::Json,
            max_nodes: DEFAULT_EXPORT_NODES,
            max_edges: DEFAULT_EXPORT_EDGES,
        }
    }
}

/// Deterministic, bounded graph export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ExportReport {
    /// Public schema version.
    pub schema_version: u32,
    /// Public result version.
    pub result_version: u32,
    /// Rendered representation.
    pub format: ExportFormat,
    /// Complete rendered document.
    pub content: String,
    /// Number of exported nodes.
    pub exported_nodes: usize,
    /// Number of exported edges.
    pub exported_edges: usize,
    /// Number of omitted input nodes.
    pub omitted_nodes: usize,
    /// Number of omitted or ineligible input edges.
    pub omitted_edges: usize,
    /// Whether any node or edge was omitted.
    pub truncated: bool,
    /// Source-free export limitations.
    pub warnings: Vec<Warning>,
}

/// Health category consolidated by the doctor report.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DoctorCategory {
    /// Database schema and migration compatibility.
    Schema,
    /// Store and graph integrity.
    Integrity,
    /// Repository snapshot freshness.
    Freshness,
    /// Optional provider availability and compatibility.
    Provider,
    /// Manifest and effective configuration validity.
    Config,
}

/// Conservative health state for an input or derived check.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStatus {
    /// The check completed and satisfied its invariant.
    Healthy,
    /// The check completed with an actionable limitation.
    Degraded,
    /// The check completed and found an invalid state.
    Failed,
    /// Required input was absent, partial, stale, or unavailable.
    Unknown,
}

/// Exact schema state supplied to the pure doctor function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SchemaDoctorInput {
    /// Stable store or component name.
    pub name: String,
    /// Schema version expected by this binary.
    pub expected_version: u32,
    /// Observed schema version, or `None` when unavailable.
    pub actual_version: Option<u32>,
    /// Whether the exact schema metadata is internally consistent.
    pub metadata_consistent: Option<bool>,
}

/// Integrity observation supplied to the pure doctor function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct IntegrityDoctorInput {
    /// Stable store or component name.
    pub name: String,
    /// `Some(true)` for a completed successful check, `None` when not completed.
    pub passed: Option<bool>,
    /// Optional bounded source-free detail.
    pub detail: Option<String>,
}

/// Freshness observation supplied to the pure doctor function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FreshnessDoctorInput {
    /// Exact repository identifier.
    pub repo_id: RepoId,
    /// Existing repository freshness classification.
    pub state: RepoFreshnessState,
    /// Optional bounded source-free reason.
    pub detail: Option<String>,
}

/// Provider state accepted by the pure doctor function.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ProviderDoctorStatus {
    /// Provider and required index are available.
    Available,
    /// Provider exists but its repository index is missing.
    IndexMissing,
    /// Provider index does not match repository state.
    Stale,
    /// Provider cannot be reached.
    Unavailable,
    /// Provider public contract is incompatible.
    Incompatible,
    /// Provider returned invalid public data.
    InvalidResponse,
    /// Provider probe did not complete within its bound.
    TimedOut,
}

/// Provider observation supplied to the pure doctor function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderDoctorInput {
    /// Stable provider name.
    pub name: String,
    /// Provider availability state.
    pub status: ProviderDoctorStatus,
    /// Optional bounded source-free detail.
    pub detail: Option<String>,
}

/// Configuration observation supplied to the pure doctor function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigDoctorInput {
    /// Stable workspace, repository, or configuration scope.
    pub name: String,
    /// `Some(true)` for validated configuration, `None` when validation was unavailable.
    pub valid: Option<bool>,
    /// Optional bounded source-free detail.
    pub detail: Option<String>,
}

/// Immutable inputs consolidated by [`doctor`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorRequest {
    /// Schema and migration observations.
    pub schema: Vec<SchemaDoctorInput>,
    /// Store and graph integrity observations.
    pub integrity: Vec<IntegrityDoctorInput>,
    /// Repository freshness observations.
    pub freshness: Vec<FreshnessDoctorInput>,
    /// Optional provider observations.
    pub providers: Vec<ProviderDoctorInput>,
    /// Configuration observations.
    pub config: Vec<ConfigDoctorInput>,
}

/// One deterministic doctor check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorCheck {
    /// Check category.
    pub category: DoctorCategory,
    /// Stable checked subject.
    pub name: String,
    /// Conservative check result.
    pub status: DoctorStatus,
    /// Bounded source-free summary.
    pub summary: String,
    /// Concrete remediation when the status is not healthy.
    pub remediation: Option<String>,
}

/// Consolidated source-free health report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorReport {
    /// Public schema version.
    pub schema_version: u32,
    /// Public result version.
    pub result_version: u32,
    /// Most conservative aggregate status.
    pub status: DoctorStatus,
    /// Stable checks ordered by category and name.
    pub checks: Vec<DoctorCheck>,
    /// Whether every required category had conclusive input.
    pub complete: bool,
    /// Number of healthy checks.
    pub healthy_checks: usize,
    /// Number of degraded checks.
    pub degraded_checks: usize,
    /// Number of failed checks.
    pub failed_checks: usize,
    /// Number of checks with unknown state.
    pub unknown_checks: usize,
}

/// Stable pagination metadata for delivery envelopes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Pagination {
    /// Metadata schema version.
    pub version: u32,
    /// Applied zero-based offset.
    pub offset: usize,
    /// Applied page-size bound.
    pub limit: usize,
    /// Total items before pagination.
    pub total: usize,
    /// Number of items in this page.
    pub returned: usize,
    /// Whether more items follow this page.
    pub has_more: bool,
}

/// Compact source-free result summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Summary {
    /// Metadata schema version.
    pub version: u32,
    /// Stable summary code.
    pub code: String,
    /// Human-readable title.
    pub title: String,
    /// Bounded explanation.
    pub detail: String,
    /// Whether the result is complete.
    pub complete: bool,
}

/// Explicit unresolved interpretation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Ambiguity {
    /// Metadata schema version.
    pub version: u32,
    /// Stable ambiguity code.
    pub code: String,
    /// Bounded explanation.
    pub message: String,
    /// Deterministically ordered candidate identifiers.
    pub candidates: Vec<String>,
    /// Concrete way to disambiguate.
    pub remediation: String,
}

/// Non-fatal delivery warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Warning {
    /// Metadata schema version.
    pub version: u32,
    /// Stable warning code.
    pub code: String,
    /// Bounded source-free message.
    pub message: String,
}

/// Explicit follow-up offered to a caller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NextAction {
    /// Metadata schema version.
    pub version: u32,
    /// Stable action code.
    pub code: String,
    /// Bounded reason for the action.
    pub reason: String,
    /// Optional command displayed but never executed by this module.
    pub command: Option<String>,
}

/// Versioned page suitable for CLI, MCP, or HTTP delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Page<T> {
    /// Public schema version.
    pub schema_version: u32,
    /// Public result version.
    pub result_version: u32,
    /// Items in deterministic page order.
    pub items: Vec<T>,
    /// Pagination metadata.
    pub pagination: Pagination,
    /// Compact result summary.
    pub summary: Summary,
    /// Explicit unresolved interpretations.
    pub ambiguities: Vec<Ambiguity>,
    /// Non-fatal warnings.
    pub warnings: Vec<Warning>,
    /// Suggested follow-up actions.
    pub next_actions: Vec<NextAction>,
}

/// One named generated JSON Schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PublicSchema {
    /// Stable public type name.
    pub name: String,
    /// Contract version for this schema.
    pub version: u32,
    /// JSON Schema generated by `schemars`.
    pub schema: serde_json::Value,
}

/// Deterministic catalog of stable public request and report schemas.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PublicSchemaCatalog {
    /// Catalog schema version.
    pub schema_version: u32,
    /// Catalog result version.
    pub result_version: u32,
    /// Schemas ordered by stable public type name.
    pub schemas: Vec<PublicSchema>,
}

/// Stable semantic class used to map domain failures to process exit codes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DomainErrorKind {
    /// Invalid command or request input.
    InvalidInput,
    /// Requested entity does not exist.
    NotFound,
    /// Multiple candidates prevent deterministic selection.
    Ambiguous,
    /// Existing state conflicts with the operation.
    Conflict,
    /// Useful output exists but required coverage is incomplete.
    Partial,
    /// A required local or remote capability is unavailable.
    Unavailable,
    /// A bounded operation timed out.
    Timeout,
    /// Caller cancelled the operation.
    Cancelled,
    /// An invariant or unexpected internal operation failed.
    Internal,
}

/// Stable process exit classification.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[repr(u8)]
#[serde(rename_all = "snake_case")]
pub enum ExitCode {
    /// Successful complete operation.
    Success = 0,
    /// Invalid usage or request.
    InvalidInput = 2,
    /// Requested entity was not found.
    NotFound = 3,
    /// Selection was ambiguous.
    Ambiguous = 4,
    /// Existing state conflicted with the operation.
    Conflict = 5,
    /// Operation produced only partial or unknown coverage.
    Partial = 6,
    /// Required capability was unavailable.
    Unavailable = 7,
    /// Operation exceeded its explicit time bound.
    Timeout = 8,
    /// Unexpected internal failure.
    Internal = 70,
    /// Operation was cancelled by the caller.
    Cancelled = 130,
}

impl ExitCode {
    /// Returns the stable numeric process code.
    #[must_use]
    pub const fn value(self) -> u8 {
        self as u8
    }
}

/// Validation and rendering failures from the pure application interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InterfaceError {
    /// A request omitted a field required by its action.
    #[error("contract action `{action:?}` requires `{field}`")]
    MissingContractField {
        /// Requested action.
        action: ContractAction,
        /// Missing field name.
        field: String,
    },
    /// A requested graph entity was not observed.
    #[error("graph entity `{0}` was not found")]
    NotFound(String),
    /// An identifier occurred more than once.
    #[error("duplicate {entity} identifier `{id}`")]
    DuplicateIdentity {
        /// Entity family.
        entity: String,
        /// Duplicated identifier.
        id: String,
    },
    /// A request bound is zero or exceeds its hard maximum.
    #[error("requested {name} bound {value} is outside 1..={maximum}")]
    InvalidBound {
        /// Bound name.
        name: String,
        /// Supplied value.
        value: usize,
        /// Inclusive hard maximum.
        maximum: usize,
    },
    /// A public schema or deterministic payload could not be serialized.
    #[error("failed to serialize `{0}`")]
    Serialization(String),
}

/// Performs a bounded contract operation over immutable graph inputs.
///
/// Evidence notes are never copied into the returned report. Missing links and missing
/// compatibility inputs remain explicit unknown coverage rather than evidence of safety.
///
/// # Errors
///
/// Returns [`InterfaceError`] for malformed requests, duplicate identities, missing requested
/// entities, or unsupported bounds.
pub fn inspect_contracts(
    nodes: &[Node],
    edges: &[Edge],
    evidence: &[Evidence],
    compatibility: &[ContractCompatibility],
    request: &ContractRequest,
) -> Result<ContractReport, InterfaceError> {
    validate_bound("contract result", request.limit, MAX_EXPORT_NODES)?;
    let node_index = index_nodes(nodes)?;
    let evidence_index = index_evidence(evidence)?;
    let edge_index = index_edges(edges)?;
    let contract_ids = node_index
        .values()
        .filter(|node| is_contract_kind(node.kind))
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let mut issues = validate_contract_inputs(&node_index, &edge_index, &evidence_index);
    let context = ContractContext {
        nodes: &node_index,
        contract_ids: &contract_ids,
        edges: &edge_index,
        evidence: &evidence_index,
        compatibility,
    };
    let mut selected = select_contract_action(&context, request, &mut issues)?;
    if issues
        .iter()
        .any(|issue| issue.severity != ContractIssueSeverity::Warning)
    {
        selected.complete = false;
    }
    issues.sort_by(issue_order);
    issues.dedup();
    Ok(ContractReport {
        schema_version: INTERFACE_SCHEMA_VERSION,
        result_version: INTERFACE_RESULT_VERSION,
        action: request.action,
        contracts: selected.contracts,
        links: selected.links,
        compatibility: selected.compatibility,
        differences: selected.differences,
        issues,
        valid: selected.valid,
        complete: selected.complete,
        truncated: selected.truncated,
    })
}

struct ContractContext<'a> {
    nodes: &'a BTreeMap<NodeId, &'a Node>,
    contract_ids: &'a BTreeSet<NodeId>,
    edges: &'a BTreeMap<EdgeId, &'a Edge>,
    evidence: &'a BTreeMap<EvidenceId, &'a Evidence>,
    compatibility: &'a [ContractCompatibility],
}

struct ContractSelection {
    contracts: Vec<ContractView>,
    links: Vec<ContractLink>,
    compatibility: Vec<ContractCompatibilitySummary>,
    differences: Vec<ContractDifference>,
    valid: Option<bool>,
    complete: bool,
    truncated: bool,
}

impl ContractSelection {
    fn complete() -> Self {
        Self {
            contracts: Vec::new(),
            links: Vec::new(),
            compatibility: Vec::new(),
            differences: Vec::new(),
            valid: None,
            complete: true,
            truncated: false,
        }
    }
}

fn select_contract_action(
    context: &ContractContext<'_>,
    request: &ContractRequest,
    issues: &mut Vec<ContractIssue>,
) -> Result<ContractSelection, InterfaceError> {
    match request.action {
        ContractAction::List => Ok(select_contract_list(context, request.limit)),
        ContractAction::Show => select_contract_show(context, request, issues),
        ContractAction::Validate => select_contract_validation(context, request, issues),
        ContractAction::Diff => select_contract_diff(context, request, issues),
        ContractAction::ExplainLink => select_contract_link(context, request, issues),
    }
}

fn select_contract_list(context: &ContractContext<'_>, limit: usize) -> ContractSelection {
    let ordered = ordered_contracts(context.nodes);
    let mut selected = ContractSelection::complete();
    selected.truncated = ordered.len() > limit;
    selected.contracts = ordered
        .into_iter()
        .take(limit)
        .map(|node| ContractView {
            contract: node.clone(),
            links: Vec::new(),
        })
        .collect();
    selected
}

fn select_contract_show(
    context: &ContractContext<'_>,
    request: &ContractRequest,
    issues: &mut Vec<ContractIssue>,
) -> Result<ContractSelection, InterfaceError> {
    let id = required_contract(request, false)?;
    let node = require_contract(context.nodes, context.contract_ids, id)?;
    let (links, truncated) = links_for_node(id, context.edges, context.evidence, request.limit);
    let mut selected = ContractSelection::complete();
    selected.contracts.push(ContractView {
        contract: node.clone(),
        links,
    });
    selected.truncated = truncated;
    selected.compatibility =
        compatibility_for(&[id], context.compatibility, issues, &mut selected.complete);
    Ok(selected)
}

fn select_contract_validation(
    context: &ContractContext<'_>,
    request: &ContractRequest,
    issues: &mut Vec<ContractIssue>,
) -> Result<ContractSelection, InterfaceError> {
    let mut selected = ContractSelection::complete();
    if let Some(id) = request.contract.as_ref() {
        let node = require_contract(context.nodes, context.contract_ids, id)?;
        selected.contracts.push(ContractView {
            contract: node.clone(),
            links: Vec::new(),
        });
        issues.retain(|issue| issue_applies_to(issue, id, context.edges));
    }
    let conclusive = !issues
        .iter()
        .any(|issue| issue.severity == ContractIssueSeverity::Unknown);
    selected.complete = conclusive;
    selected.valid = Some(
        conclusive
            && !issues
                .iter()
                .any(|issue| issue.severity == ContractIssueSeverity::Error),
    );
    Ok(selected)
}

fn select_contract_diff(
    context: &ContractContext<'_>,
    request: &ContractRequest,
    issues: &mut Vec<ContractIssue>,
) -> Result<ContractSelection, InterfaceError> {
    let before_id = required_contract(request, false)?;
    let after_id = required_contract(request, true)?;
    let before = require_contract(context.nodes, context.contract_ids, before_id)?;
    let after = require_contract(context.nodes, context.contract_ids, after_id)?;
    let mut selected = ContractSelection::complete();
    selected.contracts = vec![
        ContractView {
            contract: before.clone(),
            links: Vec::new(),
        },
        ContractView {
            contract: after.clone(),
            links: Vec::new(),
        },
    ];
    selected.differences = contract_differences(before, after);
    selected.compatibility = compatibility_for(
        &[before_id, after_id],
        context.compatibility,
        issues,
        &mut selected.complete,
    );
    Ok(selected)
}

fn select_contract_link(
    context: &ContractContext<'_>,
    request: &ContractRequest,
    issues: &mut Vec<ContractIssue>,
) -> Result<ContractSelection, InterfaceError> {
    let source_id = required_contract(request, false)?;
    let target_id = required_contract(request, true)?;
    let source = require_contract(context.nodes, context.contract_ids, source_id)?;
    let target = require_contract(context.nodes, context.contract_ids, target_id)?;
    let mut selected = ContractSelection::complete();
    selected.contracts = vec![
        ContractView {
            contract: source.clone(),
            links: Vec::new(),
        },
        ContractView {
            contract: target.clone(),
            links: Vec::new(),
        },
    ];
    let mut matching = context
        .edges
        .values()
        .filter(|edge| {
            (&edge.source == source_id && &edge.target == target_id)
                || (&edge.source == target_id && &edge.target == source_id)
        })
        .map(|edge| link_from_edge(edge, context.evidence))
        .collect::<Vec<_>>();
    matching.sort_by(|left, right| left.edge.id.cmp(&right.edge.id));
    selected.truncated = matching.len() > request.limit;
    selected.links = matching.into_iter().take(request.limit).collect();
    if selected.links.is_empty() {
        selected.complete = false;
        issues.push(contract_issue(
            "contract.link_not_observed",
            ContractIssueSeverity::Unknown,
            None,
            "No direct link was observed; absence is not proof of independence.",
        ));
    }
    Ok(selected)
}

/// Renders a deterministic bounded graph with source-free evidence metadata.
///
/// Nodes are ordered by stable key and identifier. Edges are ordered by identifier and are
/// exported only when both endpoints are in the selected node set.
///
/// # Errors
///
/// Returns [`InterfaceError`] for invalid bounds, duplicate identities, or JSON serialization
/// failure.
pub fn export_graph(
    nodes: &[Node],
    edges: &[Edge],
    evidence: &[Evidence],
    request: &ExportRequest,
) -> Result<ExportReport, InterfaceError> {
    validate_bound("node export", request.max_nodes, MAX_EXPORT_NODES)?;
    validate_bound("edge export", request.max_edges, MAX_EXPORT_EDGES)?;
    let node_index = index_nodes(nodes)?;
    let _ = index_edges(edges)?;
    let evidence_index = index_evidence(evidence)?;
    let mut ordered_nodes = nodes.iter().collect::<Vec<_>>();
    ordered_nodes.sort_by(|left, right| {
        left.stable_key
            .cmp(&right.stable_key)
            .then_with(|| left.id.cmp(&right.id))
    });
    let selected_nodes = ordered_nodes
        .into_iter()
        .take(request.max_nodes)
        .cloned()
        .collect::<Vec<_>>();
    let selected_ids = selected_nodes
        .iter()
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    let mut warnings = Vec::new();
    let mut eligible_edges = edges
        .iter()
        .filter(|edge| selected_ids.contains(&edge.source) && selected_ids.contains(&edge.target))
        .collect::<Vec<_>>();
    for edge in edges {
        if !node_index.contains_key(&edge.source) || !node_index.contains_key(&edge.target) {
            warnings.push(warning(
                "export.dangling_edge",
                &format!(
                    "Edge `{}` was omitted because an endpoint is missing.",
                    edge.id.as_str()
                ),
            ));
        }
        for evidence_id in &edge.evidence {
            if !evidence_index.contains_key(evidence_id) {
                warnings.push(warning(
                    "export.evidence_not_observed",
                    &format!(
                        "Evidence `{}` referenced by edge `{}` was not observed.",
                        evidence_id.as_str(),
                        edge.id.as_str()
                    ),
                ));
            }
        }
    }
    eligible_edges.sort_by(|left, right| left.id.cmp(&right.id));
    let selected_edges = eligible_edges
        .into_iter()
        .take(request.max_edges)
        .map(|edge| ExportEdge {
            edge: edge.clone(),
            evidence: evidence_for_edge(edge, &evidence_index),
        })
        .collect::<Vec<_>>();
    warnings.sort_by(|left, right| {
        left.code
            .cmp(&right.code)
            .then_with(|| left.message.cmp(&right.message))
    });
    warnings.dedup();
    let content = match request.format {
        ExportFormat::Json => render_json(&selected_nodes, &selected_edges)?,
        ExportFormat::GraphMl => render_graphml(&selected_nodes, &selected_edges),
        ExportFormat::Markdown => render_markdown(&selected_nodes, &selected_edges),
    };
    let omitted_nodes = nodes.len().saturating_sub(selected_nodes.len());
    let omitted_edges = edges.len().saturating_sub(selected_edges.len());
    Ok(ExportReport {
        schema_version: INTERFACE_SCHEMA_VERSION,
        result_version: INTERFACE_RESULT_VERSION,
        format: request.format,
        content,
        exported_nodes: selected_nodes.len(),
        exported_edges: selected_edges.len(),
        omitted_nodes,
        omitted_edges,
        truncated: omitted_nodes > 0 || omitted_edges > 0,
        warnings,
    })
}

/// Consolidates schema, integrity, freshness, provider, and configuration observations.
///
/// Failed checks dominate. Otherwise any unknown check forces an unknown aggregate; degraded
/// status is returned only when every check is conclusive.
#[must_use]
pub fn doctor(request: &DoctorRequest) -> DoctorReport {
    let mut checks = Vec::new();
    append_schema_checks(&request.schema, &mut checks);
    append_integrity_checks(&request.integrity, &mut checks);
    append_freshness_checks(&request.freshness, &mut checks);
    append_provider_checks(&request.providers, &mut checks);
    append_config_checks(&request.config, &mut checks);
    checks.sort_by(|left, right| {
        left.category
            .cmp(&right.category)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.summary.cmp(&right.summary))
    });
    let healthy_checks = count_status(&checks, DoctorStatus::Healthy);
    let degraded_checks = count_status(&checks, DoctorStatus::Degraded);
    let failed_checks = count_status(&checks, DoctorStatus::Failed);
    let unknown_checks = count_status(&checks, DoctorStatus::Unknown);
    let status = if failed_checks > 0 {
        DoctorStatus::Failed
    } else if unknown_checks > 0 {
        DoctorStatus::Unknown
    } else if degraded_checks > 0 {
        DoctorStatus::Degraded
    } else {
        DoctorStatus::Healthy
    };
    DoctorReport {
        schema_version: INTERFACE_SCHEMA_VERSION,
        result_version: INTERFACE_RESULT_VERSION,
        status,
        checks,
        complete: unknown_checks == 0,
        healthy_checks,
        degraded_checks,
        failed_checks,
        unknown_checks,
    }
}

/// Builds a deterministic bounded page from an already ordered item slice.
///
/// # Errors
///
/// Returns [`InterfaceError::InvalidBound`] when `limit` is zero or exceeds the export node hard
/// bound.
pub fn paginate<T: Clone>(
    items: &[T],
    offset: usize,
    limit: usize,
    summary: Summary,
) -> Result<Page<T>, InterfaceError> {
    validate_bound("page", limit, MAX_EXPORT_NODES)?;
    let start = offset.min(items.len());
    let page_items = items
        .iter()
        .skip(start)
        .take(limit)
        .cloned()
        .collect::<Vec<_>>();
    let returned = page_items.len();
    Ok(Page {
        schema_version: INTERFACE_SCHEMA_VERSION,
        result_version: INTERFACE_RESULT_VERSION,
        items: page_items,
        pagination: Pagination {
            version: DELIVERY_METADATA_VERSION,
            offset,
            limit,
            total: items.len(),
            returned,
            has_more: start.saturating_add(returned) < items.len(),
        },
        summary,
        ambiguities: Vec::new(),
        warnings: Vec::new(),
        next_actions: Vec::new(),
    })
}

/// Generates the deterministic catalog of stable public request and report schemas.
///
/// # Errors
///
/// Returns [`InterfaceError::Serialization`] if a generated schema cannot be represented as JSON.
pub fn public_schema_catalog() -> Result<PublicSchemaCatalog, InterfaceError> {
    let mut schemas = vec![
        public_schema::<ChangeImpactReport>("ChangeImpactReport")?,
        public_schema::<ChangeRequest>("ChangeRequest")?,
        public_schema::<ContractReport>("ContractReport")?,
        public_schema::<ContractRequest>("ContractRequest")?,
        public_schema::<DoctorReport>("DoctorReport")?,
        public_schema::<DoctorRequest>("DoctorRequest")?,
        public_schema::<ExportReport>("ExportReport")?,
        public_schema::<ExportRequest>("ExportRequest")?,
        public_schema::<ImpactReport>("ImpactReport")?,
        public_schema::<ImpactRequest>("ImpactRequest")?,
        public_schema::<Page<ContractView>>("PageContractView")?,
        public_schema::<PullRequestInspection>("PullRequestInspection")?,
        public_schema::<SearchReport>("SearchReport")?,
        public_schema::<SearchRequest>("SearchRequest")?,
        public_schema::<TraversalReport>("TraversalReport")?,
        public_schema::<TraversalRequest>("TraversalRequest")?,
    ];
    schemas.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(PublicSchemaCatalog {
        schema_version: INTERFACE_SCHEMA_VERSION,
        result_version: INTERFACE_RESULT_VERSION,
        schemas,
    })
}

/// Maps a domain error class to its stable process exit code.
#[must_use]
pub const fn classify_exit_code(kind: DomainErrorKind) -> ExitCode {
    match kind {
        DomainErrorKind::InvalidInput => ExitCode::InvalidInput,
        DomainErrorKind::NotFound => ExitCode::NotFound,
        DomainErrorKind::Ambiguous => ExitCode::Ambiguous,
        DomainErrorKind::Conflict => ExitCode::Conflict,
        DomainErrorKind::Partial => ExitCode::Partial,
        DomainErrorKind::Unavailable => ExitCode::Unavailable,
        DomainErrorKind::Timeout => ExitCode::Timeout,
        DomainErrorKind::Cancelled => ExitCode::Cancelled,
        DomainErrorKind::Internal => ExitCode::Internal,
    }
}

/// Classifies an application-interface error using the stable domain mapping.
#[must_use]
pub const fn classify_interface_error(error: &InterfaceError) -> ExitCode {
    match error {
        InterfaceError::MissingContractField { .. } | InterfaceError::InvalidBound { .. } => {
            ExitCode::InvalidInput
        }
        InterfaceError::NotFound(_) => ExitCode::NotFound,
        InterfaceError::DuplicateIdentity { .. } => ExitCode::Conflict,
        InterfaceError::Serialization(_) => ExitCode::Internal,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct ExportEdge {
    edge: Edge,
    evidence: Vec<EvidenceMetadata>,
}

#[derive(Serialize)]
struct JsonExport<'a> {
    schema_version: u32,
    result_version: u32,
    nodes: &'a [Node],
    edges: &'a [ExportEdge],
}

fn validate_bound(name: &str, value: usize, maximum: usize) -> Result<(), InterfaceError> {
    if value == 0 || value > maximum {
        Err(InterfaceError::InvalidBound {
            name: name.to_owned(),
            value,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn index_nodes(nodes: &[Node]) -> Result<BTreeMap<NodeId, &Node>, InterfaceError> {
    let mut index = BTreeMap::new();
    for node in nodes {
        if index.insert(node.id.clone(), node).is_some() {
            return Err(InterfaceError::DuplicateIdentity {
                entity: "node".to_owned(),
                id: node.id.as_str().to_owned(),
            });
        }
    }
    Ok(index)
}

fn index_edges(edges: &[Edge]) -> Result<BTreeMap<EdgeId, &Edge>, InterfaceError> {
    let mut index = BTreeMap::new();
    for edge in edges {
        if index.insert(edge.id.clone(), edge).is_some() {
            return Err(InterfaceError::DuplicateIdentity {
                entity: "edge".to_owned(),
                id: edge.id.as_str().to_owned(),
            });
        }
    }
    Ok(index)
}

fn index_evidence(
    evidence: &[Evidence],
) -> Result<BTreeMap<EvidenceId, &Evidence>, InterfaceError> {
    let mut index = BTreeMap::new();
    for item in evidence {
        if index.insert(item.id.clone(), item).is_some() {
            return Err(InterfaceError::DuplicateIdentity {
                entity: "evidence".to_owned(),
                id: item.id.as_str().to_owned(),
            });
        }
    }
    Ok(index)
}

const fn is_contract_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Package
            | NodeKind::HttpOperation
            | NodeKind::GraphqlOperation
            | NodeKind::RpcMethod
            | NodeKind::EventChannel
            | NodeKind::EventSchema
            | NodeKind::Database
            | NodeKind::DatabaseTable
            | NodeKind::DatabaseColumn
    )
}

fn ordered_contracts<'a>(nodes: &'a BTreeMap<NodeId, &Node>) -> Vec<&'a Node> {
    let mut contracts = nodes
        .values()
        .copied()
        .filter(|node| is_contract_kind(node.kind))
        .collect::<Vec<_>>();
    contracts.sort_by(|left, right| {
        left.stable_key
            .cmp(&right.stable_key)
            .then_with(|| left.id.cmp(&right.id))
    });
    contracts
}

fn required_contract(request: &ContractRequest, related: bool) -> Result<&NodeId, InterfaceError> {
    let value = if related {
        request.related_contract.as_ref()
    } else {
        request.contract.as_ref()
    };
    value.ok_or_else(|| InterfaceError::MissingContractField {
        action: request.action,
        field: if related {
            "related_contract".to_owned()
        } else {
            "contract".to_owned()
        },
    })
}

fn require_contract<'a>(
    nodes: &'a BTreeMap<NodeId, &Node>,
    contracts: &BTreeSet<NodeId>,
    id: &NodeId,
) -> Result<&'a Node, InterfaceError> {
    if !contracts.contains(id) {
        return Err(InterfaceError::NotFound(id.as_str().to_owned()));
    }
    nodes
        .get(id)
        .copied()
        .ok_or_else(|| InterfaceError::NotFound(id.as_str().to_owned()))
}

fn validate_contract_inputs(
    nodes: &BTreeMap<NodeId, &Node>,
    edges: &BTreeMap<EdgeId, &Edge>,
    evidence: &BTreeMap<EvidenceId, &Evidence>,
) -> Vec<ContractIssue> {
    let mut issues = Vec::new();
    for edge in edges.values().copied() {
        for endpoint in [&edge.source, &edge.target] {
            if !nodes.contains_key(endpoint) {
                issues.push(contract_issue(
                    "contract.dangling_edge",
                    ContractIssueSeverity::Error,
                    Some(edge.id.as_str()),
                    &format!(
                        "Edge `{}` references missing node `{}`.",
                        edge.id.as_str(),
                        endpoint.as_str()
                    ),
                ));
            }
        }
        if !edge.confidence.is_finite() || !(0.0..=1.0).contains(&edge.confidence) {
            issues.push(contract_issue(
                "contract.invalid_confidence",
                ContractIssueSeverity::Error,
                Some(edge.id.as_str()),
                "Edge confidence is not finite and within the inclusive range 0..=1.",
            ));
        }
        if edge.status != EpistemicStatus::Confirmed {
            issues.push(contract_issue(
                "contract.unresolved_link",
                ContractIssueSeverity::Unknown,
                Some(edge.id.as_str()),
                "The relationship is not confirmed and cannot prove a contract link.",
            ));
        }
        for evidence_id in &edge.evidence {
            if !evidence.contains_key(evidence_id) {
                issues.push(contract_issue(
                    "contract.evidence_not_observed",
                    ContractIssueSeverity::Unknown,
                    Some(edge.id.as_str()),
                    &format!(
                        "Referenced evidence `{}` was not observed.",
                        evidence_id.as_str()
                    ),
                ));
            }
        }
    }
    for item in evidence.values().copied() {
        if !item.confidence.is_finite() || !(0.0..=1.0).contains(&item.confidence) {
            issues.push(contract_issue(
                "contract.invalid_evidence_confidence",
                ContractIssueSeverity::Error,
                Some(item.id.as_str()),
                "Evidence confidence is not finite and within the inclusive range 0..=1.",
            ));
        }
    }
    issues
}

fn contract_issue(
    code: &str,
    severity: ContractIssueSeverity,
    entity_id: Option<&str>,
    message: &str,
) -> ContractIssue {
    ContractIssue {
        code: code.to_owned(),
        severity,
        entity_id: entity_id.map(str::to_owned),
        message: message.to_owned(),
    }
}

fn issue_order(left: &ContractIssue, right: &ContractIssue) -> std::cmp::Ordering {
    left.severity
        .cmp(&right.severity)
        .then_with(|| left.code.cmp(&right.code))
        .then_with(|| left.entity_id.cmp(&right.entity_id))
        .then_with(|| left.message.cmp(&right.message))
}

fn issue_applies_to(
    issue: &ContractIssue,
    contract_id: &NodeId,
    edges: &BTreeMap<EdgeId, &Edge>,
) -> bool {
    issue.entity_id.as_deref().is_some_and(|entity_id| {
        entity_id == contract_id.as_str()
            || edges
                .get(&EdgeId::new(entity_id))
                .is_some_and(|edge| edge.source == *contract_id || edge.target == *contract_id)
    })
}

fn evidence_for_edge(
    edge: &Edge,
    evidence: &BTreeMap<EvidenceId, &Evidence>,
) -> Vec<EvidenceMetadata> {
    let mut selected = edge
        .evidence
        .iter()
        .filter_map(|id| evidence.get(id).copied())
        .map(EvidenceMetadata::from)
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| left.id.cmp(&right.id));
    selected
}

fn link_from_edge(edge: &Edge, evidence: &BTreeMap<EvidenceId, &Evidence>) -> ContractLink {
    ContractLink {
        edge: edge.clone(),
        evidence: evidence_for_edge(edge, evidence),
    }
}

fn links_for_node(
    id: &NodeId,
    edges: &BTreeMap<EdgeId, &Edge>,
    evidence: &BTreeMap<EvidenceId, &Evidence>,
    limit: usize,
) -> (Vec<ContractLink>, bool) {
    let matching = edges
        .values()
        .copied()
        .filter(|edge| edge.source == *id || edge.target == *id)
        .collect::<Vec<_>>();
    let truncated = matching.len() > limit;
    (
        matching
            .into_iter()
            .take(limit)
            .map(|edge| link_from_edge(edge, evidence))
            .collect(),
        truncated,
    )
}

fn compatibility_for(
    ids: &[&NodeId],
    compatibility: &[ContractCompatibility],
    issues: &mut Vec<ContractIssue>,
    complete: &mut bool,
) -> Vec<ContractCompatibilitySummary> {
    let wanted = ids.iter().copied().collect::<BTreeSet<_>>();
    let mut selected = compatibility
        .iter()
        .filter(|item| wanted.contains(&item.contract_id))
        .map(compatibility_summary)
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| left.contract_id.cmp(&right.contract_id));
    selected.dedup_by(|left, right| left.contract_id == right.contract_id);
    for id in ids {
        if !selected.iter().any(|item| &item.contract_id == *id) {
            *complete = false;
            issues.push(contract_issue(
                "contract.compatibility_not_observed",
                ContractIssueSeverity::Unknown,
                Some(id.as_str()),
                "Compatibility was not supplied for the selected contract.",
            ));
        }
    }
    selected
}

fn compatibility_summary(value: &ContractCompatibility) -> ContractCompatibilitySummary {
    let mut findings = value
        .report
        .findings
        .iter()
        .map(|finding| ContractFinding {
            code: finding.code.clone(),
            path: finding.path.clone(),
            status: finding.status,
            factors: finding.factors.clone(),
            recommended_validations: finding.recommended_validations.clone(),
        })
        .collect::<Vec<_>>();
    findings.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.code.cmp(&right.code))
    });
    ContractCompatibilitySummary {
        contract_id: value.contract_id.clone(),
        status: value.report.status,
        before_fingerprint: value.report.before_fingerprint.clone(),
        after_fingerprint: value.report.after_fingerprint.clone(),
        findings,
    }
}

fn contract_differences(before: &Node, after: &Node) -> Vec<ContractDifference> {
    let mut differences = Vec::new();
    if before.kind != after.kind {
        differences.push(ContractDifference {
            field: "kind".to_owned(),
            before: Some(enum_name(before.kind)),
            after: Some(enum_name(after.kind)),
        });
    }
    if before.repo_id != after.repo_id {
        differences.push(ContractDifference {
            field: "repo_id".to_owned(),
            before: before.repo_id.as_ref().map(|id| id.as_str().to_owned()),
            after: after.repo_id.as_ref().map(|id| id.as_str().to_owned()),
        });
    }
    if before.stable_key != after.stable_key {
        differences.push(ContractDifference {
            field: "stable_key".to_owned(),
            before: Some(before.stable_key.clone()),
            after: Some(after.stable_key.clone()),
        });
    }
    if before.label != after.label {
        differences.push(ContractDifference {
            field: "label".to_owned(),
            before: Some(before.label.clone()),
            after: Some(after.label.clone()),
        });
    }
    differences
}

fn render_json(nodes: &[Node], edges: &[ExportEdge]) -> Result<String, InterfaceError> {
    serde_json::to_string_pretty(&JsonExport {
        schema_version: INTERFACE_SCHEMA_VERSION,
        result_version: INTERFACE_RESULT_VERSION,
        nodes,
        edges,
    })
    .map_err(|_| InterfaceError::Serialization("graph export".to_owned()))
}

fn render_graphml(nodes: &[Node], edges: &[ExportEdge]) -> String {
    let mut output = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <graphml xmlns=\"http://graphml.graphdrawing.org/xmlns\">\n\
         <key id=\"kind\" for=\"all\" attr.name=\"kind\" attr.type=\"string\"/>\n\
         <key id=\"stable_key\" for=\"node\" attr.name=\"stable_key\" attr.type=\"string\"/>\n\
         <key id=\"label\" for=\"node\" attr.name=\"label\" attr.type=\"string\"/>\n\
         <key id=\"status\" for=\"edge\" attr.name=\"status\" attr.type=\"string\"/>\n\
         <key id=\"confidence\" for=\"edge\" attr.name=\"confidence\" attr.type=\"float\"/>\n\
         <key id=\"evidence\" for=\"edge\" attr.name=\"evidence\" attr.type=\"string\"/>\n\
         <graph id=\"code-system-graph\" edgedefault=\"directed\">\n",
    );
    for node in nodes {
        let _ = writeln!(
            output,
            "<node id=\"{}\"><data key=\"kind\">{}</data><data key=\"stable_key\">{}</data><data key=\"label\">{}</data></node>",
            escape_xml(node.id.as_str()),
            escape_xml(&enum_name(node.kind)),
            escape_xml(&node.stable_key),
            escape_xml(&node.label)
        );
    }
    for item in edges {
        let locations = item
            .evidence
            .iter()
            .map(evidence_location)
            .collect::<Vec<_>>()
            .join("; ");
        let _ = writeln!(
            output,
            "<edge id=\"{}\" source=\"{}\" target=\"{}\"><data key=\"kind\">{}</data><data key=\"status\">{}</data><data key=\"confidence\">{}</data><data key=\"evidence\">{}</data></edge>",
            escape_xml(item.edge.id.as_str()),
            escape_xml(item.edge.source.as_str()),
            escape_xml(item.edge.target.as_str()),
            escape_xml(&enum_name(item.edge.kind)),
            escape_xml(&enum_name(item.edge.status)),
            item.edge.confidence,
            escape_xml(&locations)
        );
    }
    output.push_str("</graph>\n</graphml>\n");
    output
}

fn render_markdown(nodes: &[Node], edges: &[ExportEdge]) -> String {
    let mut output = String::from(
        "# Code System Graph graph export\n\n## Nodes\n\n| ID | Kind | Stable key | Label |\n|---|---|---|---|\n",
    );
    for node in nodes {
        let _ = writeln!(
            output,
            "| {} | {} | {} | {} |",
            escape_markdown(node.id.as_str()),
            escape_markdown(&enum_name(node.kind)),
            escape_markdown(&node.stable_key),
            escape_markdown(&node.label)
        );
    }
    output.push_str("\n## Edges\n\n| ID | Source | Target | Kind | Status | Confidence | Evidence |\n|---|---|---|---|---|---:|---|\n");
    for item in edges {
        let locations = item
            .evidence
            .iter()
            .map(evidence_location)
            .collect::<Vec<_>>()
            .join("; ");
        let _ = writeln!(
            output,
            "| {} | {} | {} | {} | {} | {} | {} |",
            escape_markdown(item.edge.id.as_str()),
            escape_markdown(item.edge.source.as_str()),
            escape_markdown(item.edge.target.as_str()),
            escape_markdown(&enum_name(item.edge.kind)),
            escape_markdown(&enum_name(item.edge.status)),
            item.edge.confidence,
            escape_markdown(&locations)
        );
    }
    output
}

fn evidence_location(evidence: &EvidenceMetadata) -> String {
    let path = evidence.file_path.as_deref().unwrap_or("<unknown>");
    match (evidence.start_line, evidence.end_line) {
        (Some(start), Some(end)) => format!("{path}:{start}-{end}"),
        (Some(start), None) => format!("{path}:{start}"),
        _ => path.to_owned(),
    }
}

fn enum_name<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn escape_xml(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            '\t' | '\n' | '\r' => escaped.push(character),
            character if character.is_control() => escaped.push('\u{fffd}'),
            character => escaped.push(character),
        }
    }
    escaped
}

fn escape_markdown(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\r' | '\n' | '\t' => escaped.push(' '),
            character if character.is_control() => escaped.push('\u{fffd}'),
            '\\' | '`' | '*' | '_' | '{' | '}' | '[' | ']' | '(' | ')' | '#' | '+' | '-' | '.'
            | '!' | '|' | '>' => {
                escaped.push('\\');
                escaped.push(character);
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn append_schema_checks(inputs: &[SchemaDoctorInput], checks: &mut Vec<DoctorCheck>) {
    if inputs.is_empty() {
        checks.push(missing_doctor_category(DoctorCategory::Schema));
    }
    for input in inputs {
        let (status, summary, remediation) = match (input.actual_version, input.metadata_consistent)
        {
            (Some(actual), Some(true)) if actual == input.expected_version => (
                DoctorStatus::Healthy,
                format!("Schema version {actual} and metadata are consistent."),
                None,
            ),
            (Some(actual), _) if actual != input.expected_version => (
                DoctorStatus::Failed,
                format!(
                    "Schema version {actual} does not match expected version {}.",
                    input.expected_version
                ),
                Some(
                    "Remove the incompatible development database and run a full scan.".to_owned(),
                ),
            ),
            (Some(_), Some(false)) => (
                DoctorStatus::Failed,
                "Schema metadata is inconsistent.".to_owned(),
                Some(
                    "Restore an exact 1.0.0 backup or rebuild the development database.".to_owned(),
                ),
            ),
            _ => (
                DoctorStatus::Unknown,
                "Schema state was not fully observed.".to_owned(),
                Some("Open the store and validate its exact schema metadata.".to_owned()),
            ),
        };
        checks.push(DoctorCheck {
            category: DoctorCategory::Schema,
            name: input.name.clone(),
            status,
            summary,
            remediation,
        });
    }
}

fn append_integrity_checks(inputs: &[IntegrityDoctorInput], checks: &mut Vec<DoctorCheck>) {
    if inputs.is_empty() {
        checks.push(missing_doctor_category(DoctorCategory::Integrity));
    }
    for input in inputs {
        let (status, fallback, remediation) = match input.passed {
            Some(true) => (DoctorStatus::Healthy, "Integrity check passed.", None),
            Some(false) => (
                DoctorStatus::Failed,
                "Integrity check failed.",
                Some("Restore the last verified backup before rebuilding state.".to_owned()),
            ),
            None => (
                DoctorStatus::Unknown,
                "Integrity check did not complete.",
                Some("Run the bounded integrity check before trusting stored results.".to_owned()),
            ),
        };
        checks.push(DoctorCheck {
            category: DoctorCategory::Integrity,
            name: input.name.clone(),
            status,
            summary: input.detail.clone().unwrap_or_else(|| fallback.to_owned()),
            remediation,
        });
    }
}

fn append_freshness_checks(inputs: &[FreshnessDoctorInput], checks: &mut Vec<DoctorCheck>) {
    if inputs.is_empty() {
        checks.push(missing_doctor_category(DoctorCategory::Freshness));
    }
    for input in inputs {
        let (status, fallback, remediation) = match input.state {
            RepoFreshnessState::Fresh => (DoctorStatus::Healthy, "Snapshot is fresh.", None),
            RepoFreshnessState::WorkingTreeChanged
            | RepoFreshnessState::CommitsBehind
            | RepoFreshnessState::ConfigChanged
            | RepoFreshnessState::ExtractorChanged
            | RepoFreshnessState::CodegraphPending => (
                DoctorStatus::Degraded,
                "Snapshot is stale relative to a known input.",
                Some(
                    "Run an explicit incremental scan before relying on safety claims.".to_owned(),
                ),
            ),
            RepoFreshnessState::Corrupt => (
                DoctorStatus::Failed,
                "Snapshot is marked corrupt.",
                Some(
                    "Restore a verified snapshot or rebuild after integrity diagnosis.".to_owned(),
                ),
            ),
            RepoFreshnessState::Partial
            | RepoFreshnessState::Unknown
            | RepoFreshnessState::Unavailable => (
                DoctorStatus::Unknown,
                "Repository freshness is incomplete or unavailable.",
                Some("Restore repository access and complete a bounded scan.".to_owned()),
            ),
        };
        checks.push(DoctorCheck {
            category: DoctorCategory::Freshness,
            name: input.repo_id.as_str().to_owned(),
            status,
            summary: input.detail.clone().unwrap_or_else(|| fallback.to_owned()),
            remediation,
        });
    }
}

fn append_provider_checks(inputs: &[ProviderDoctorInput], checks: &mut Vec<DoctorCheck>) {
    if inputs.is_empty() {
        checks.push(missing_doctor_category(DoctorCategory::Provider));
    }
    for input in inputs {
        let (status, fallback, remediation) = match input.status {
            ProviderDoctorStatus::Available => {
                (DoctorStatus::Healthy, "Provider is available.", None)
            }
            ProviderDoctorStatus::IndexMissing | ProviderDoctorStatus::Stale => (
                DoctorStatus::Degraded,
                "Provider index is missing or stale.",
                Some(
                    "Initialize or refresh the provider index explicitly if enrichment is needed."
                        .to_owned(),
                ),
            ),
            ProviderDoctorStatus::Unavailable | ProviderDoctorStatus::TimedOut => (
                DoctorStatus::Unknown,
                "Provider availability could not be established.",
                Some(
                    "Verify the provider binary and rerun its bounded compatibility probe."
                        .to_owned(),
                ),
            ),
            ProviderDoctorStatus::Incompatible | ProviderDoctorStatus::InvalidResponse => (
                DoctorStatus::Failed,
                "Provider public contract is incompatible or invalid.",
                Some(
                    "Use a tested provider version or disable the incompatible adapter.".to_owned(),
                ),
            ),
        };
        checks.push(DoctorCheck {
            category: DoctorCategory::Provider,
            name: input.name.clone(),
            status,
            summary: input.detail.clone().unwrap_or_else(|| fallback.to_owned()),
            remediation,
        });
    }
}

fn append_config_checks(inputs: &[ConfigDoctorInput], checks: &mut Vec<DoctorCheck>) {
    if inputs.is_empty() {
        checks.push(missing_doctor_category(DoctorCategory::Config));
    }
    for input in inputs {
        let (status, fallback, remediation) = match input.valid {
            Some(true) => (DoctorStatus::Healthy, "Configuration is valid.", None),
            Some(false) => (
                DoctorStatus::Failed,
                "Configuration is invalid.",
                Some(
                    "Correct the reported field without discarding unrelated settings.".to_owned(),
                ),
            ),
            None => (
                DoctorStatus::Unknown,
                "Configuration validation did not complete.",
                Some("Load and validate every effective configuration layer.".to_owned()),
            ),
        };
        checks.push(DoctorCheck {
            category: DoctorCategory::Config,
            name: input.name.clone(),
            status,
            summary: input.detail.clone().unwrap_or_else(|| fallback.to_owned()),
            remediation,
        });
    }
}

fn missing_doctor_category(category: DoctorCategory) -> DoctorCheck {
    DoctorCheck {
        category,
        name: "not_observed".to_owned(),
        status: DoctorStatus::Unknown,
        summary: "No input was supplied for this required doctor category.".to_owned(),
        remediation: Some(
            "Collect the category check before claiming a healthy system.".to_owned(),
        ),
    }
}

fn count_status(checks: &[DoctorCheck], status: DoctorStatus) -> usize {
    checks.iter().filter(|check| check.status == status).count()
}

fn warning(code: &str, message: &str) -> Warning {
    Warning {
        version: DELIVERY_METADATA_VERSION,
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

fn public_schema<T: JsonSchema>(name: &str) -> Result<PublicSchema, InterfaceError> {
    let schema = serde_json::to_value(schema_for!(T))
        .map_err(|_| InterfaceError::Serialization(format!("schema {name}")))?;
    Ok(PublicSchema {
        name: name.to_owned(),
        version: INTERFACE_SCHEMA_VERSION,
        schema,
    })
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{EdgeKind, EvidenceId};

    use super::*;
    use crate::{CompatibilityFinding, CompatibilityReport};

    fn node(id: &str, kind: NodeKind, stable_key: &str) -> Node {
        Node {
            id: NodeId::new(id),
            kind,
            repo_id: Some(RepoId::new("repo:test")),
            stable_key: stable_key.to_owned(),
            label: stable_key.to_owned(),
        }
    }

    fn evidence(id: &str, path: &str) -> Evidence {
        Evidence {
            id: EvidenceId::new(id),
            repo_id: Some(RepoId::new("repo:test")),
            file_path: Some(path.to_owned()),
            start_line: Some(3),
            end_line: Some(5),
            extractor: "test".to_owned(),
            extractor_version: "1.0.0".to_owned(),
            provenance: Provenance::Extracted,
            confidence: 1.0,
            observed_at_commit: Some("abc".to_owned()),
            content_hash: Some("hash".to_owned()),
            note: Some("LEAK_ME_SOURCE_BODY".to_owned()),
        }
    }

    fn edge(id: &str, source: &str, target: &str, evidence: &[&str]) -> Edge {
        Edge {
            id: EdgeId::new(id),
            source: NodeId::new(source),
            target: NodeId::new(target),
            kind: EdgeKind::CallsRemote,
            confidence: 1.0,
            status: EpistemicStatus::Confirmed,
            evidence: evidence.iter().map(|id| EvidenceId::new(*id)).collect(),
        }
    }

    fn request(action: ContractAction) -> ContractRequest {
        ContractRequest {
            action,
            contract: None,
            related_contract: None,
            limit: 100,
        }
    }

    fn report_or_panic(result: Result<ContractReport, InterfaceError>) -> ContractReport {
        match result {
            Ok(report) => report,
            Err(error) => panic!("unexpected contract error: {error}"),
        }
    }

    fn export_or_panic(result: Result<ExportReport, InterfaceError>) -> ExportReport {
        match result {
            Ok(report) => report,
            Err(error) => panic!("unexpected export error: {error}"),
        }
    }

    fn json_or_panic<T: Serialize>(value: &T) -> String {
        match serde_json::to_string(value) {
            Ok(json) => json,
            Err(error) => panic!("unexpected JSON error: {error}"),
        }
    }

    fn healthy_doctor_request() -> DoctorRequest {
        DoctorRequest {
            schema: vec![SchemaDoctorInput {
                name: "store".to_owned(),
                expected_version: 8,
                actual_version: Some(8),
                metadata_consistent: Some(true),
            }],
            integrity: vec![IntegrityDoctorInput {
                name: "sqlite".to_owned(),
                passed: Some(true),
                detail: None,
            }],
            freshness: vec![FreshnessDoctorInput {
                repo_id: RepoId::new("repo:test"),
                state: RepoFreshnessState::Fresh,
                detail: None,
            }],
            providers: vec![ProviderDoctorInput {
                name: "codegraph".to_owned(),
                status: ProviderDoctorStatus::Available,
                detail: None,
            }],
            config: vec![ConfigDoctorInput {
                name: "workspace".to_owned(),
                valid: Some(true),
                detail: None,
            }],
        }
    }

    fn summary() -> Summary {
        Summary {
            version: DELIVERY_METADATA_VERSION,
            code: "test".to_owned(),
            title: "Test".to_owned(),
            detail: "Test page".to_owned(),
            complete: true,
        }
    }

    #[test]
    fn contract_list_is_deterministic() {
        let nodes = vec![
            node("node:b", NodeKind::HttpOperation, "z"),
            node("node:a", NodeKind::RpcMethod, "a"),
        ];
        let first = report_or_panic(inspect_contracts(
            &nodes,
            &[],
            &[],
            &[],
            &request(ContractAction::List),
        ));
        let mut reversed = nodes;
        reversed.reverse();
        let second = report_or_panic(inspect_contracts(
            &reversed,
            &[],
            &[],
            &[],
            &request(ContractAction::List),
        ));

        assert_eq!(first.contracts, second.contracts);
    }

    #[test]
    fn contract_list_excludes_non_contract_nodes() {
        let nodes = vec![
            node("node:service", NodeKind::Service, "service"),
            node("node:http", NodeKind::HttpOperation, "http"),
        ];
        let report = report_or_panic(inspect_contracts(
            &nodes,
            &[],
            &[],
            &[],
            &request(ContractAction::List),
        ));

        assert_eq!(report.contracts.len(), 1);
    }

    #[test]
    fn contract_list_applies_limit() {
        let nodes = vec![
            node("node:a", NodeKind::HttpOperation, "a"),
            node("node:b", NodeKind::HttpOperation, "b"),
        ];
        let mut input = request(ContractAction::List);
        input.limit = 1;
        let report = report_or_panic(inspect_contracts(&nodes, &[], &[], &[], &input));

        assert!(report.truncated && report.contracts.len() == 1);
    }

    #[test]
    fn contract_show_omits_evidence_note() {
        let nodes = vec![
            node("node:a", NodeKind::HttpOperation, "a"),
            node("node:b", NodeKind::HttpOperation, "b"),
        ];
        let edges = vec![edge("edge:1", "node:a", "node:b", &["evidence:1"])];
        let evidence = vec![evidence("evidence:1", "src/<api>.rs")];
        let mut input = request(ContractAction::Show);
        input.contract = Some(NodeId::new("node:a"));
        let report = report_or_panic(inspect_contracts(&nodes, &edges, &evidence, &[], &input));

        assert!(!json_or_panic(&report).contains("LEAK_ME_SOURCE_BODY"));
    }

    #[test]
    fn contract_show_rejects_unknown_contract() {
        let mut input = request(ContractAction::Show);
        input.contract = Some(NodeId::new("node:missing"));
        let result = inspect_contracts(&[], &[], &[], &[], &input);

        assert!(matches!(result, Err(InterfaceError::NotFound(_))));
    }

    #[test]
    fn contract_show_requires_contract_field() {
        let result = inspect_contracts(&[], &[], &[], &[], &request(ContractAction::Show));

        assert!(matches!(
            result,
            Err(InterfaceError::MissingContractField { .. })
        ));
    }

    #[test]
    fn contract_validate_detects_dangling_edge() {
        let nodes = vec![node("node:a", NodeKind::HttpOperation, "a")];
        let edges = vec![edge("edge:1", "node:a", "node:missing", &[])];
        let report = report_or_panic(inspect_contracts(
            &nodes,
            &edges,
            &[],
            &[],
            &request(ContractAction::Validate),
        ));

        assert_eq!(report.valid, Some(false));
    }

    #[test]
    fn contract_validate_preserves_unknown_evidence() {
        let nodes = vec![
            node("node:a", NodeKind::HttpOperation, "a"),
            node("node:b", NodeKind::HttpOperation, "b"),
        ];
        let edges = vec![edge("edge:1", "node:a", "node:b", &["evidence:missing"])];
        let report = report_or_panic(inspect_contracts(
            &nodes,
            &edges,
            &[],
            &[],
            &request(ContractAction::Validate),
        ));

        assert_eq!(report.valid, Some(false));
    }

    #[test]
    fn contract_diff_omits_compatibility_evidence_text() {
        let nodes = vec![
            node("node:a", NodeKind::HttpOperation, "a"),
            node("node:b", NodeKind::HttpOperation, "b"),
        ];
        let compatibility = vec![ContractCompatibility {
            contract_id: NodeId::new("node:a"),
            report: CompatibilityReport {
                status: CompatibilityStatus::Breaking,
                before_fingerprint: "before".to_owned(),
                after_fingerprint: "after".to_owned(),
                findings: vec![CompatibilityFinding {
                    code: "removed".to_owned(),
                    path: "GET /a".to_owned(),
                    status: CompatibilityStatus::Breaking,
                    factors: vec!["operation removed".to_owned()],
                    evidence: vec!["LEAK_ME_SOURCE_BODY".to_owned()],
                    recommended_validations: vec!["run contract tests".to_owned()],
                }],
            },
        }];
        let mut input = request(ContractAction::Diff);
        input.contract = Some(NodeId::new("node:a"));
        input.related_contract = Some(NodeId::new("node:b"));
        let report = report_or_panic(inspect_contracts(&nodes, &[], &[], &compatibility, &input));

        assert!(!json_or_panic(&report).contains("LEAK_ME_SOURCE_BODY"));
    }

    #[test]
    fn contract_diff_reports_structural_fields() {
        let nodes = vec![
            node("node:a", NodeKind::HttpOperation, "a"),
            node("node:b", NodeKind::RpcMethod, "b"),
        ];
        let mut input = request(ContractAction::Diff);
        input.contract = Some(NodeId::new("node:a"));
        input.related_contract = Some(NodeId::new("node:b"));
        let report = report_or_panic(inspect_contracts(&nodes, &[], &[], &[], &input));

        assert!(report.differences.iter().any(|item| item.field == "kind"));
    }

    #[test]
    fn explain_link_returns_direct_edge() {
        let nodes = vec![
            node("node:a", NodeKind::HttpOperation, "a"),
            node("node:b", NodeKind::RpcMethod, "b"),
        ];
        let edges = vec![edge("edge:1", "node:a", "node:b", &[])];
        let mut input = request(ContractAction::ExplainLink);
        input.contract = Some(NodeId::new("node:a"));
        input.related_contract = Some(NodeId::new("node:b"));
        let report = report_or_panic(inspect_contracts(&nodes, &edges, &[], &[], &input));

        assert_eq!(report.links.len(), 1);
    }

    #[test]
    fn explain_link_marks_absence_unknown() {
        let nodes = vec![
            node("node:a", NodeKind::HttpOperation, "a"),
            node("node:b", NodeKind::RpcMethod, "b"),
        ];
        let mut input = request(ContractAction::ExplainLink);
        input.contract = Some(NodeId::new("node:a"));
        input.related_contract = Some(NodeId::new("node:b"));
        let report = report_or_panic(inspect_contracts(&nodes, &[], &[], &[], &input));

        assert!(
            !report.complete
                && report
                    .issues
                    .iter()
                    .any(|issue| issue.severity == ContractIssueSeverity::Unknown)
        );
    }

    #[test]
    fn json_export_is_deterministic() {
        let nodes = vec![
            node("node:b", NodeKind::HttpOperation, "b"),
            node("node:a", NodeKind::HttpOperation, "a"),
        ];
        let first = export_or_panic(export_graph(&nodes, &[], &[], &ExportRequest::default()));
        let mut reversed = nodes;
        reversed.reverse();
        let second = export_or_panic(export_graph(&reversed, &[], &[], &ExportRequest::default()));

        assert_eq!(first.content, second.content);
    }

    #[test]
    fn json_export_omits_evidence_note() {
        let nodes = vec![
            node("node:a", NodeKind::HttpOperation, "a"),
            node("node:b", NodeKind::HttpOperation, "b"),
        ];
        let edges = vec![edge("edge:1", "node:a", "node:b", &["evidence:1"])];
        let report = export_or_panic(export_graph(
            &nodes,
            &edges,
            &[evidence("evidence:1", "api.rs")],
            &ExportRequest::default(),
        ));

        assert!(!report.content.contains("LEAK_ME_SOURCE_BODY"));
    }

    #[test]
    fn export_applies_node_bound() {
        let nodes = vec![
            node("node:a", NodeKind::HttpOperation, "a"),
            node("node:b", NodeKind::HttpOperation, "b"),
        ];
        let report = export_or_panic(export_graph(
            &nodes,
            &[],
            &[],
            &ExportRequest {
                max_nodes: 1,
                ..ExportRequest::default()
            },
        ));

        assert!(report.truncated && report.exported_nodes == 1);
    }

    #[test]
    fn export_applies_edge_bound() {
        let nodes = vec![
            node("node:a", NodeKind::HttpOperation, "a"),
            node("node:b", NodeKind::HttpOperation, "b"),
        ];
        let edges = vec![
            edge("edge:1", "node:a", "node:b", &[]),
            edge("edge:2", "node:b", "node:a", &[]),
        ];
        let report = export_or_panic(export_graph(
            &nodes,
            &edges,
            &[],
            &ExportRequest {
                max_edges: 1,
                ..ExportRequest::default()
            },
        ));

        assert!(report.truncated && report.exported_edges == 1);
    }

    #[test]
    fn graphml_escapes_xml_text() {
        let nodes = vec![node(
            "node:<a>&\"'",
            NodeKind::HttpOperation,
            "<script>&\"'",
        )];
        let report = export_or_panic(export_graph(
            &nodes,
            &[],
            &[],
            &ExportRequest {
                format: ExportFormat::GraphMl,
                ..ExportRequest::default()
            },
        ));

        assert!(
            report.content.contains("&lt;script&gt;&amp;&quot;&apos;")
                && !report.content.contains("<script>")
        );
    }

    #[test]
    fn markdown_escapes_table_and_markup_text() {
        let nodes = vec![node("node:a", NodeKind::HttpOperation, "a|**unsafe**\nrow")];
        let report = export_or_panic(export_graph(
            &nodes,
            &[],
            &[],
            &ExportRequest {
                format: ExportFormat::Markdown,
                ..ExportRequest::default()
            },
        ));

        assert!(report.content.contains(r"a\|\*\*unsafe\*\* row"));
    }

    #[test]
    fn export_rejects_zero_bound() {
        let result = export_graph(
            &[],
            &[],
            &[],
            &ExportRequest {
                max_nodes: 0,
                ..ExportRequest::default()
            },
        );

        assert!(matches!(result, Err(InterfaceError::InvalidBound { .. })));
    }

    #[test]
    fn export_warns_and_omits_dangling_edge() {
        let nodes = vec![node("node:a", NodeKind::HttpOperation, "a")];
        let edges = vec![edge("edge:1", "node:a", "node:missing", &[])];
        let report = export_or_panic(export_graph(&nodes, &edges, &[], &ExportRequest::default()));

        assert!(report.exported_edges == 0 && !report.warnings.is_empty());
    }

    #[test]
    fn doctor_reports_healthy_complete_input() {
        let report = doctor(&healthy_doctor_request());

        assert!(report.status == DoctorStatus::Healthy && report.complete);
    }

    #[test]
    fn doctor_failed_check_dominates_unknown() {
        let mut input = DoctorRequest::default();
        input.config.push(ConfigDoctorInput {
            name: "workspace".to_owned(),
            valid: Some(false),
            detail: None,
        });
        let report = doctor(&input);

        assert_eq!(report.status, DoctorStatus::Failed);
    }

    #[test]
    fn doctor_unknown_dominates_degraded() {
        let mut input = healthy_doctor_request();
        input.providers[0].status = ProviderDoctorStatus::Stale;
        input.integrity[0].passed = None;
        let report = doctor(&input);

        assert_eq!(report.status, DoctorStatus::Unknown);
    }

    #[test]
    fn doctor_partial_freshness_is_unknown() {
        let mut input = healthy_doctor_request();
        input.freshness[0].state = RepoFreshnessState::Partial;
        let report = doctor(&input);

        assert_eq!(report.status, DoctorStatus::Unknown);
    }

    #[test]
    fn doctor_missing_categories_are_unknown() {
        let report = doctor(&DoctorRequest::default());

        assert!(report.status == DoctorStatus::Unknown && report.unknown_checks == 5);
    }

    #[test]
    fn doctor_check_order_is_deterministic() {
        let mut first_input = healthy_doctor_request();
        first_input.config.push(ConfigDoctorInput {
            name: "alpha".to_owned(),
            valid: Some(true),
            detail: None,
        });
        let mut second_input = first_input.clone();
        second_input.config.reverse();
        let first = doctor(&first_input);
        let second = doctor(&second_input);

        assert_eq!(first, second);
    }

    #[test]
    fn schema_catalog_is_deterministic() {
        let first = public_schema_catalog();
        let second = public_schema_catalog();

        assert_eq!(first, second);
    }

    #[test]
    fn schema_catalog_contains_core_interface_types() {
        let catalog = match public_schema_catalog() {
            Ok(catalog) => catalog,
            Err(error) => panic!("unexpected catalog error: {error}"),
        };

        assert!(
            [
                "ContractRequest",
                "ContractReport",
                "ExportRequest",
                "DoctorReport"
            ]
            .iter()
            .all(|name| catalog.schemas.iter().any(|schema| schema.name == *name))
        );
    }

    #[test]
    fn generated_request_schema_has_object_shape() {
        let catalog = match public_schema_catalog() {
            Ok(catalog) => catalog,
            Err(error) => panic!("unexpected catalog error: {error}"),
        };
        let request_schema = catalog
            .schemas
            .iter()
            .find(|schema| schema.name == "ContractRequest");

        assert!(request_schema.is_some_and(|schema| schema.schema.get("properties").is_some()));
    }

    #[test]
    fn pagination_has_no_duplicates_across_pages() {
        let items = vec![1, 2, 3, 4];
        let first = paginate(&items, 0, 2, summary());
        let second = paginate(&items, 2, 2, summary());
        let combined = first
            .ok()
            .into_iter()
            .flat_map(|page| page.items)
            .chain(second.ok().into_iter().flat_map(|page| page.items))
            .collect::<BTreeSet<_>>();

        assert_eq!(combined, BTreeSet::from([1, 2, 3, 4]));
    }

    #[test]
    fn pagination_reports_unknown_offset_without_panicking() {
        let page = match paginate(&[1, 2], 100, 10, summary()) {
            Ok(page) => page,
            Err(error) => panic!("unexpected pagination error: {error}"),
        };

        assert!(page.items.is_empty() && !page.pagination.has_more);
    }

    #[test]
    fn pagination_rejects_zero_limit() {
        let result = paginate(&[1], 0, 0, summary());

        assert!(matches!(result, Err(InterfaceError::InvalidBound { .. })));
    }

    #[test]
    fn exit_code_mapping_is_stable() {
        let values = [
            classify_exit_code(DomainErrorKind::InvalidInput).value(),
            classify_exit_code(DomainErrorKind::NotFound).value(),
            classify_exit_code(DomainErrorKind::Ambiguous).value(),
            classify_exit_code(DomainErrorKind::Conflict).value(),
            classify_exit_code(DomainErrorKind::Partial).value(),
            classify_exit_code(DomainErrorKind::Unavailable).value(),
            classify_exit_code(DomainErrorKind::Timeout).value(),
            classify_exit_code(DomainErrorKind::Internal).value(),
            classify_exit_code(DomainErrorKind::Cancelled).value(),
        ];

        assert_eq!(values, [2, 3, 4, 5, 6, 7, 8, 70, 130]);
    }

    #[test]
    fn contract_action_uses_stable_kebab_case() {
        assert_eq!(
            json_or_panic(&ContractAction::ExplainLink),
            "\"explain-link\""
        );
    }
}

//! Pure semantic mapping and impact propagation for fingerprinted change sets.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use code_system_graph_model::{
    CommunityId, CommunitySnapshot, Edge, Evidence, EvidenceId, NativePath, NativePathEncoding, Node, NodeId, NodeKind, RepoFreshness, RepoFreshnessState, RepoId
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ChangeSet, ChangeSourceLayer, ChangeValidity, ChangeValidityInput, ChangedFile, ChangedFileStatus, CompatibilityFinding, CompatibilityInput, CompatibilityReport, CompatibilityStatus, ImpactCompatibilityStatus, ImpactContext, ImpactDirection, ImpactError, ImpactOptions, ImpactReport, ImpactRequest, ImpactTarget, RiskLevel, analyze_impact, validate_change_set
};

/// Semantic version of the change-analysis algorithm and report contract.
pub const CHANGE_ANALYZER_VERSION: &str = "1.0.0";

const MAX_CHANGED_FILES: usize = 100_000;
const MAX_CHANGED_NODES: usize = 100_000;
const MAX_IMPACT_TARGETS: usize = 10_000;
const MAX_DEPTH: usize = 128;
const MAX_PAGE_LIMIT: usize = 10_000;
const MAX_PAGE_OFFSET: usize = 1_000_000;

/// Deterministic bounds and presentation controls for semantic change analysis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeAnalysisOptions {
    /// Maximum changed files mapped before coverage becomes incomplete.
    pub max_changed_files: usize,
    /// Maximum distinct graph nodes retained from changed-file mappings.
    pub max_changed_nodes: usize,
    /// Maximum exact changed boundary nodes used as impact targets.
    pub max_impact_targets: usize,
    /// Direction in which impact propagates from each changed boundary.
    pub direction: ImpactDirection,
    /// Maximum graph depth for each impact traversal.
    pub max_depth: usize,
    /// Zero-based offset over the stable changed-entity order.
    pub offset: usize,
    /// Maximum changed entities returned after the offset.
    pub limit: usize,
    /// Omit detailed changed entities and impact items while retaining aggregates.
    pub summary_only: bool,
}

impl Default for ChangeAnalysisOptions {
    fn default() -> Self {
        Self {
            max_changed_files: 1_000,
            max_changed_nodes: 10_000,
            max_impact_targets: 1_000,
            direction: ImpactDirection::Upstream,
            max_depth: 8,
            offset: 0,
            limit: 100,
            summary_only: false,
        }
    }
}

/// Side of a changed path and hunk used for evidence matching.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ChangedPathSide {
    /// Original path and old-side hunk range.
    Old,
    /// Resulting path and new-side hunk range.
    New,
}

/// Strength of one path-and-line evidence match.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceMatchKind {
    /// Persisted evidence lines intersect an exact changed hunk range.
    HunkIntersection,
    /// Only the exact repository-relative file path could be matched.
    FilePathFallback,
}

/// Position-only explanation of why evidence matched a changed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HunkLineMatch {
    /// Matched persisted evidence.
    pub evidence_id: EvidenceId,
    /// Old or new path and hunk side used by the match.
    pub path_side: ChangedPathSide,
    /// Match strength.
    pub kind: EvidenceMatchKind,
    /// Zero-based hunk index for an exact line intersection.
    pub hunk_index: Option<usize>,
    /// Inclusive changed-hunk start line.
    pub hunk_start: Option<u32>,
    /// Inclusive changed-hunk end line.
    pub hunk_end: Option<u32>,
    /// Inclusive persisted-evidence start line.
    pub evidence_start: Option<u32>,
    /// Inclusive persisted-evidence end line.
    pub evidence_end: Option<u32>,
}

/// Explicit completeness of one changed-file semantic mapping.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MappingCompleteness {
    /// Exact path and hunk evidence reached at least one boundary node.
    Complete,
    /// Useful path, evidence, or graph mapping exists but is not sufficient to prove completeness.
    Partial,
    /// The native path or required persisted evidence could not be mapped.
    Unknown,
}

/// Semantic graph mapping for one changed file and source layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangedArtifactMapping {
    /// Original repository-relative native path.
    pub old_path: Option<NativePath>,
    /// Resulting repository-relative native path.
    pub new_path: Option<NativePath>,
    /// Git-level change status.
    pub status: ChangedFileStatus,
    /// Git state layer that supplied the file.
    pub layer: ChangeSourceLayer,
    /// Sorted evidence identifiers matched by exact path and position.
    pub matched_evidence_ids: Vec<EvidenceId>,
    /// Sorted artifact nodes reached through matched edge evidence.
    pub artifact_node_ids: Vec<NodeId>,
    /// Sorted `SymbolRef` nodes reached through matched edge evidence.
    pub symbol_ref_node_ids: Vec<NodeId>,
    /// Sorted contract or boundary nodes reached through matched edge evidence.
    pub boundary_node_ids: Vec<NodeId>,
    /// Position-only explanations for evidence matches.
    pub matches: Vec<HunkLineMatch>,
    /// Conservative mapping completeness.
    pub completeness: MappingCompleteness,
    /// Deterministically ordered mapping limitations.
    pub gaps: Vec<String>,
}

/// Role through which a graph node was associated with changed evidence.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ChangedEntityRole {
    /// Boundary-defining artifact.
    Artifact,
    /// Repository-local symbol reference.
    SymbolRef,
    /// Contract or integration boundary.
    Boundary,
}

/// One deduplicated graph entity associated with the analyzed change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangedEntity {
    /// Exact graph node.
    pub node: Node,
    /// Sorted semantic roles observed across changed files.
    pub roles: Vec<ChangedEntityRole>,
    /// Lossless changed paths that contributed this entity.
    pub changed_paths: Vec<NativePath>,
}

/// Precomputed compatibility result keyed by an exact contract and optional source file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ContractCompatibilityInput {
    /// Optional repository-relative path that must belong to the changed mapping.
    pub file_path: Option<String>,
    /// Exact contract node to which the comparison applies.
    pub contract_node_id: NodeId,
    /// Precomputed comparison, or `None` when either side was unavailable.
    pub report: Option<CompatibilityReport>,
}

/// Compatibility change for one exact changed contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SemanticContractDelta {
    /// Exact changed contract node.
    pub contract: Node,
    /// Contract node kind copied explicitly for schema consumers.
    pub contract_kind: NodeKind,
    /// Conservative compatibility status.
    pub status: CompatibilityStatus,
    /// Structured previous-contract fingerprint when available.
    pub before_fingerprint: Option<String>,
    /// Structured candidate-contract fingerprint when available.
    pub after_fingerprint: Option<String>,
    /// Deterministically ordered compatibility findings.
    pub findings: Vec<CompatibilityFinding>,
    /// Deduplicated factors extracted from findings.
    pub factors: Vec<String>,
    /// Deduplicated recommended validations extracted from findings.
    pub validations: Vec<String>,
}

/// Coverage accounting that prevents false-safe change conclusions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeAnalysisCoverage {
    /// Whether every changed file, mapped node, and impact target was handled within exact bounds.
    pub complete: bool,
    /// Number of changed files in the exact input.
    pub total_changed_files: usize,
    /// Number of changed files mapped within the configured file bound.
    pub analyzed_changed_files: usize,
    /// Number of file mappings with at least one matched evidence record.
    pub mapped_changed_files: usize,
    /// Number of completely mapped changed files.
    pub complete_changed_files: usize,
    /// Number of distinct changed graph nodes before node truncation.
    pub total_changed_nodes: usize,
    /// Number of changed graph nodes retained within the configured bound.
    pub retained_changed_nodes: usize,
    /// Number of exact changed boundary nodes before target truncation.
    pub total_impact_targets: usize,
    /// Number of exact changed boundary nodes analyzed for impact.
    pub analyzed_impact_targets: usize,
    /// Whether any configured bound truncated analysis or presentation inputs.
    pub truncated: bool,
    /// Deterministically ordered limitations and remediation context.
    pub gaps: Vec<String>,
}

/// Conservative summary conclusion for a semantic change report.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ChangeConclusion {
    /// Complete analysis found semantic entities or propagated impact.
    ImpactDetected,
    /// Complete analysis found no semantic impact.
    NoSemanticImpactDetected,
    /// Incomplete coverage or truncation prevents a safe conclusion.
    Unknown,
}

/// Compact aggregate suitable for summary-only callers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeAnalysisSummary {
    /// Coverage-aware conclusion.
    pub conclusion: ChangeConclusion,
    /// Highest propagated risk, forced to `Unknown` when coverage is incomplete.
    pub highest_risk: RiskLevel,
    /// Number of deduplicated changed graph entities before pagination.
    pub changed_entities: usize,
    /// Number of exact changed contracts.
    pub changed_contracts: usize,
    /// Number of exact boundary impact reports produced.
    pub impact_reports: usize,
}

/// Complete semantic mapping and propagation tied to one exact change-set input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ChangeImpactReport {
    /// Semantic version of the analyzer implementation.
    pub analyzer_version: String,
    /// BLAKE3 fingerprint of every pure analyzer input and option.
    pub analyzer_fingerprint: String,
    /// Exact position-only change set whose identities and fingerprints own this result.
    pub change_set: ChangeSet,
    /// Stable changed-file mappings before entity pagination.
    pub mappings: Vec<ChangedArtifactMapping>,
    /// Stable, deduplicated changed entities after requested pagination.
    pub changed_entities: Vec<ChangedEntity>,
    /// Semantic compatibility deltas for exact changed contract nodes.
    pub contract_deltas: Vec<SemanticContractDelta>,
    /// One bounded impact result per analyzed exact changed boundary.
    pub impacts: Vec<ImpactReport>,
    /// Repositories touched directly or through propagated impact.
    pub touched_repositories: Vec<RepoId>,
    /// Services touched directly or through propagated impact.
    pub touched_services: Vec<Node>,
    /// Communities containing directly or transitively touched nodes.
    pub touched_communities: Vec<CommunityId>,
    /// Contracts touched directly or through propagated impact.
    pub touched_contracts: Vec<Node>,
    /// Coverage accounting controlling the summary conclusion.
    pub coverage: ChangeAnalysisCoverage,
    /// Coverage-aware aggregate summary.
    pub summary: ChangeAnalysisSummary,
}

/// Invalid input or graph state encountered during pure change analysis.
#[derive(Debug, Error)]
pub enum ChangeAnalysisError {
    /// One or more configured bounds are zero or exceed supported limits.
    #[error("change-analysis bounds are outside supported limits")]
    InvalidBounds,
    /// Pure analyzer inputs could not be serialized for fingerprinting.
    #[error("change-analysis inputs could not be fingerprinted: {0}")]
    Fingerprint(#[source] serde_json::Error),
    /// Bounded impact analysis failed for one exact changed boundary.
    #[error("impact analysis failed for changed boundary `{target}`: {source}")]
    Impact {
        /// Exact boundary node that failed analysis.
        target: String,
        /// Underlying impact validation or traversal error.
        #[source]
        source: ImpactError,
    },
}

#[derive(Debug)]
struct MappingWork {
    mapping: ChangedArtifactMapping,
    representable_paths: BTreeSet<String>,
}

#[derive(Debug, Default)]
struct EntityWork {
    roles: BTreeSet<ChangedEntityRole>,
    paths: BTreeSet<NativePath>,
}

/// Maps an exact change set to semantic entities and propagates bounded graph impact.
///
/// The function is pure: it does not read files, execute Git, invoke providers, or retain source
/// and diff bodies. Compatibility inputs must have been computed before this call.
///
/// # Errors
///
/// Returns [`ChangeAnalysisError`] for invalid bounds, fingerprint serialization failures, or an
/// invalid graph context encountered by bounded impact analysis.
#[expect(
    clippy::too_many_arguments,
    reason = "the pure boundary keeps every immutable analysis input explicit"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the orchestration remains linear so coverage degradation is auditable"
)]
#[must_use = "change reports and validation errors must be handled"]
pub fn analyze_changes(
    change_set: &ChangeSet,
    nodes: &[Node],
    edges: &[Edge],
    evidence: &[Evidence],
    community: Option<&CommunitySnapshot>,
    freshness: &[RepoFreshness],
    compatibility: &[ContractCompatibilityInput],
    options: &ChangeAnalysisOptions,
) -> Result<ChangeImpactReport, ChangeAnalysisError> {
    validate_options(options)?;
    let analyzer_fingerprint = analyzer_fingerprint(
        change_set,
        nodes,
        edges,
        evidence,
        community,
        freshness,
        compatibility,
        options,
    )?;

    let node_index = nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let evidence_index = evidence
        .iter()
        .map(|item| (item.id.clone(), item))
        .collect::<BTreeMap<_, _>>();
    let mut ordered_files = change_set.files.iter().collect::<Vec<_>>();
    ordered_files.sort_by(|left, right| changed_file_order(left, right));
    let total_changed_files = ordered_files.len();
    ordered_files.truncate(options.max_changed_files);

    let mut mapping_work = ordered_files
        .into_iter()
        .map(|file| {
            map_changed_file(
                file,
                &change_set.repo_id,
                edges,
                &node_index,
                &evidence_index,
            )
        })
        .collect::<Vec<_>>();
    mapping_work.sort_by(|left, right| mapping_order(&left.mapping, &right.mapping));

    let mut coverage_gaps = mapping_work
        .iter()
        .flat_map(|work| work.mapping.gaps.iter().cloned())
        .collect::<BTreeSet<_>>();
    let mut truncated = total_changed_files > mapping_work.len();
    if truncated {
        coverage_gaps.insert(format!(
            "changed files truncated at {}; {} files were present",
            options.max_changed_files, total_changed_files
        ));
    }

    let all_changed_node_ids = mapping_work
        .iter()
        .flat_map(|work| {
            work.mapping
                .artifact_node_ids
                .iter()
                .chain(&work.mapping.symbol_ref_node_ids)
                .chain(&work.mapping.boundary_node_ids)
                .cloned()
        })
        .collect::<BTreeSet<_>>();
    let total_changed_nodes = all_changed_node_ids.len();
    let retained_node_ids = all_changed_node_ids
        .iter()
        .take(options.max_changed_nodes)
        .cloned()
        .collect::<BTreeSet<_>>();
    if total_changed_nodes > retained_node_ids.len() {
        truncated = true;
        coverage_gaps.insert(format!(
            "changed graph nodes truncated at {}; {} nodes were mapped",
            options.max_changed_nodes, total_changed_nodes
        ));
        for work in &mut mapping_work {
            retain_node_ids(&mut work.mapping, &retained_node_ids);
            degrade_mapping(
                &mut work.mapping,
                "mapped node identities were truncated by the configured node bound",
            );
        }
    }

    let mut entities = build_changed_entities(&mapping_work, &node_index);
    let changed_entity_count = entities.len();
    let changed_entities = if options.summary_only {
        Vec::new()
    } else {
        entities
            .drain(..)
            .skip(options.offset.min(changed_entity_count))
            .take(options.limit)
            .collect()
    };

    let changed_boundary_ids = mapping_work
        .iter()
        .flat_map(|work| work.mapping.boundary_node_ids.iter().cloned())
        .collect::<BTreeSet<_>>();
    let total_impact_targets = changed_boundary_ids.len();
    let impact_target_ids = changed_boundary_ids
        .iter()
        .take(options.max_impact_targets)
        .cloned()
        .collect::<Vec<_>>();
    if total_impact_targets > impact_target_ids.len() {
        truncated = true;
        coverage_gaps.insert(format!(
            "impact targets truncated at {}; {} exact changed boundaries were mapped",
            options.max_impact_targets, total_impact_targets
        ));
    }

    let contract_deltas = build_contract_deltas(
        &changed_boundary_ids,
        &mapping_work,
        &node_index,
        compatibility,
    );
    if contract_deltas.iter().any(|delta| {
        matches!(
            delta.status,
            CompatibilityStatus::Unknown | CompatibilityStatus::Incomparable
        )
    }) {
        coverage_gaps.insert(
            "one or more changed contracts lack complete before/after compatibility data"
                .to_owned(),
        );
    }

    let mapped_changed_files = mapping_work
        .iter()
        .filter(|work| !work.mapping.matched_evidence_ids.is_empty())
        .count();
    let complete_changed_files = mapping_work
        .iter()
        .filter(|work| work.mapping.completeness == MappingCompleteness::Complete)
        .count();
    let provisional_complete = !truncated
        && mapping_work.len() == total_changed_files
        && complete_changed_files == total_changed_files
        && !contract_deltas.iter().any(|delta| {
            matches!(
                delta.status,
                CompatibilityStatus::Unknown | CompatibilityStatus::Incomparable
            )
        });

    let normalized_freshness = freshness_for_change_set(change_set, freshness);
    let impact_compatibility = contract_deltas
        .iter()
        .map(impact_compatibility_input)
        .collect::<Vec<_>>();
    let service_memberships = derive_service_memberships(nodes, edges);
    let impact_context = ImpactContext {
        nodes: nodes.to_vec(),
        edges: edges.to_vec(),
        communities: community.cloned(),
        freshness: normalized_freshness,
        compatibility: impact_compatibility,
        local_enrichment: Vec::new(),
        public_contracts: changed_boundary_ids.iter().cloned().collect(),
        criticality: Vec::new(),
        centrality: BTreeMap::new(),
        service_memberships,
        environments: Vec::new(),
        recommended_commands: Vec::new(),
        graph_complete: provisional_complete,
        coverage_gaps: coverage_gaps.iter().cloned().collect(),
    };
    let mut impacts = Vec::with_capacity(impact_target_ids.len());
    for target in &impact_target_ids {
        let request = ImpactRequest {
            target: ImpactTarget::NodeId(target.clone()),
            direction: options.direction,
            options: ImpactOptions {
                max_depth: options.max_depth,
                node_limit: options.max_changed_nodes,
                edge_limit: 50_000,
                confirmed_confidence: 0.8,
                offset: 0,
                limit: options.limit,
                summary_only: options.summary_only,
                include_depth_buckets: true,
            },
        };
        impacts.push(analyze_impact(&request, &impact_context).map_err(|source| {
            ChangeAnalysisError::Impact {
                target: target.as_str().to_owned(),
                source,
            }
        })?);
    }
    impacts.sort_by(|left, right| left.target.node.id.cmp(&right.target.node.id));

    for impact in &impacts {
        coverage_gaps.extend(impact.coverage.gaps.iter().cloned());
        if impact.truncation.is_some() || !impact.coverage.sufficient_for_score {
            truncated |= impact.truncation.is_some();
        }
    }
    let complete = provisional_complete
        && impacts
            .iter()
            .all(|impact| impact.coverage.sufficient_for_score && impact.truncation.is_none());
    let coverage = ChangeAnalysisCoverage {
        complete,
        total_changed_files,
        analyzed_changed_files: mapping_work.len(),
        mapped_changed_files,
        complete_changed_files,
        total_changed_nodes,
        retained_changed_nodes: retained_node_ids.len(),
        total_impact_targets,
        analyzed_impact_targets: impacts.len(),
        truncated,
        gaps: coverage_gaps.into_iter().collect(),
    };

    let mappings = mapping_work
        .into_iter()
        .map(|work| work.mapping)
        .collect::<Vec<_>>();
    let (touched_repositories, touched_services, touched_communities, touched_contracts) =
        aggregate_touched(
            change_set,
            &retained_node_ids,
            &node_index,
            community,
            &contract_deltas,
            &impacts,
        );
    let summary = summarize(
        &coverage,
        changed_entity_count,
        contract_deltas.len(),
        &impacts,
    );

    Ok(ChangeImpactReport {
        analyzer_version: CHANGE_ANALYZER_VERSION.to_owned(),
        analyzer_fingerprint,
        change_set: change_set.clone(),
        mappings,
        changed_entities,
        contract_deltas,
        impacts,
        touched_repositories,
        touched_services,
        touched_communities,
        touched_contracts,
        coverage,
        summary,
    })
}

/// Validates whether the exact change-set inputs owning a report remain current.
#[must_use]
pub fn validate_change_analysis(
    report: &ChangeImpactReport,
    current: &ChangeValidityInput,
) -> ChangeValidity {
    validate_change_set(&report.change_set, current)
}

fn validate_options(options: &ChangeAnalysisOptions) -> Result<(), ChangeAnalysisError> {
    if options.max_changed_files == 0
        || options.max_changed_files > MAX_CHANGED_FILES
        || options.max_changed_nodes == 0
        || options.max_changed_nodes > MAX_CHANGED_NODES
        || options.max_impact_targets == 0
        || options.max_impact_targets > MAX_IMPACT_TARGETS
        || options.max_depth == 0
        || options.max_depth > MAX_DEPTH
        || options.limit == 0
        || options.limit > MAX_PAGE_LIMIT
        || options.offset > MAX_PAGE_OFFSET
    {
        return Err(ChangeAnalysisError::InvalidBounds);
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "the fingerprint intentionally covers every pure analyzer input"
)]
fn analyzer_fingerprint(
    change_set: &ChangeSet,
    nodes: &[Node],
    edges: &[Edge],
    evidence: &[Evidence],
    community: Option<&CommunitySnapshot>,
    freshness: &[RepoFreshness],
    compatibility: &[ContractCompatibilityInput],
    options: &ChangeAnalysisOptions,
) -> Result<String, ChangeAnalysisError> {
    #[derive(Serialize)]
    struct Material<'a> {
        analyzer_version: &'static str,
        change_set: &'a ChangeSet,
        nodes: Vec<&'a Node>,
        edges: Vec<&'a Edge>,
        evidence: Vec<&'a Evidence>,
        community: Option<&'a CommunitySnapshot>,
        freshness: Vec<&'a RepoFreshness>,
        compatibility: Vec<&'a ContractCompatibilityInput>,
        options: &'a ChangeAnalysisOptions,
    }

    let mut ordered_nodes = nodes.iter().collect::<Vec<_>>();
    ordered_nodes.sort_by_key(|node| &node.id);
    let mut ordered_edges = edges.iter().collect::<Vec<_>>();
    ordered_edges.sort_by_key(|edge| &edge.id);
    let mut ordered_evidence = evidence.iter().collect::<Vec<_>>();
    ordered_evidence.sort_by_key(|item| &item.id);
    let mut ordered_freshness = freshness.iter().collect::<Vec<_>>();
    ordered_freshness.sort_by_key(|item| (&item.repo_id, &item.checkout_id));
    let mut ordered_compatibility = compatibility.iter().collect::<Vec<_>>();
    ordered_compatibility.sort_by(|left, right| {
        (
            &left.contract_node_id,
            left.file_path.as_deref().unwrap_or_default(),
        )
            .cmp(&(
                &right.contract_node_id,
                right.file_path.as_deref().unwrap_or_default(),
            ))
    });
    let encoded = serde_json::to_vec(&Material {
        analyzer_version: CHANGE_ANALYZER_VERSION,
        change_set,
        nodes: ordered_nodes,
        edges: ordered_edges,
        evidence: ordered_evidence,
        community,
        freshness: ordered_freshness,
        compatibility: ordered_compatibility,
        options,
    })
    .map_err(ChangeAnalysisError::Fingerprint)?;
    Ok(blake3::hash(&encoded).to_hex().to_string())
}

fn changed_file_order(left: &ChangedFile, right: &ChangedFile) -> Ordering {
    (
        left.source,
        left.status,
        left.old_path.as_ref(),
        left.new_path.as_ref(),
    )
        .cmp(&(
            right.source,
            right.status,
            right.old_path.as_ref(),
            right.new_path.as_ref(),
        ))
}

fn mapping_order(left: &ChangedArtifactMapping, right: &ChangedArtifactMapping) -> Ordering {
    (
        left.layer,
        left.status,
        left.old_path.as_ref(),
        left.new_path.as_ref(),
    )
        .cmp(&(
            right.layer,
            right.status,
            right.old_path.as_ref(),
            right.new_path.as_ref(),
        ))
}

#[expect(
    clippy::too_many_lines,
    reason = "old/new path, line, and graph evidence decisions remain auditable together"
)]
fn map_changed_file(
    file: &ChangedFile,
    repo_id: &RepoId,
    edges: &[Edge],
    nodes: &BTreeMap<NodeId, &Node>,
    evidence: &BTreeMap<EvidenceId, &Evidence>,
) -> MappingWork {
    let mut gaps = BTreeSet::new();
    let mut matched_evidence = BTreeSet::new();
    let mut explanations = BTreeSet::new();
    let mut representable_paths = BTreeSet::new();
    let sides = relevant_sides(file);
    let mut unrepresentable = false;
    let mut matched_sides = BTreeSet::new();
    let mut line_match = false;
    let mut fallback_match = false;

    for (side, path) in sides {
        let Some(path_text) = native_path_text(path) else {
            unrepresentable = true;
            gaps.insert(format!(
                "{} path cannot be represented as Unicode for evidence matching",
                side_name(side)
            ));
            continue;
        };
        representable_paths.insert(path_text.clone());
        for item in evidence.values().copied() {
            if item.repo_id.as_ref() != Some(repo_id)
                || item
                    .file_path
                    .as_deref()
                    .is_none_or(|candidate| !same_evidence_path(candidate, &path_text))
            {
                continue;
            }
            let ranges = hunk_ranges(file, side);
            let evidence_range = evidence_range(item);
            let intersections = evidence_range.map_or_else(Vec::new, |range| {
                ranges
                    .iter()
                    .filter(|(_, hunk_range)| ranges_intersect(*hunk_range, range))
                    .copied()
                    .collect()
            });
            if !file.binary
                && file.status != ChangedFileStatus::Untracked
                && !intersections.is_empty()
            {
                line_match = true;
                matched_sides.insert(side);
                matched_evidence.insert(item.id.clone());
                for (hunk_index, range) in intersections {
                    explanations.insert(match_explanation(
                        item,
                        side,
                        EvidenceMatchKind::HunkIntersection,
                        Some(hunk_index),
                        Some(range),
                    ));
                }
            } else if file.binary
                || file.status == ChangedFileStatus::Untracked
                || evidence_range.is_none()
                || ranges.is_empty()
            {
                fallback_match = true;
                matched_sides.insert(side);
                matched_evidence.insert(item.id.clone());
                explanations.insert(match_explanation(
                    item,
                    side,
                    EvidenceMatchKind::FilePathFallback,
                    None,
                    None,
                ));
            }
        }
    }

    if file.binary {
        gaps.insert("binary change has no line-level semantic evidence".to_owned());
    }
    if file.status == ChangedFileStatus::Untracked {
        gaps.insert("untracked file body and line changes were not collected".to_owned());
    }
    if matched_evidence.is_empty() {
        gaps.insert("no persisted evidence matched the changed path and hunk ranges".to_owned());
    }
    if fallback_match {
        gaps.insert("one or more evidence records matched only at file-path level".to_owned());
    }
    if matched_sides.len() < expected_side_count(file) {
        gaps.insert(
            "not every required old/new path neighborhood matched persisted evidence".to_owned(),
        );
    }

    let (artifact_node_ids, symbol_ref_node_ids, boundary_node_ids) =
        nodes_for_evidence(&matched_evidence, edges, nodes);
    if !matched_evidence.is_empty()
        && artifact_node_ids.is_empty()
        && symbol_ref_node_ids.is_empty()
        && boundary_node_ids.is_empty()
    {
        gaps.insert(
            "matched evidence is not attached to a supported semantic graph node".to_owned(),
        );
    }
    if boundary_node_ids.is_empty() {
        gaps.insert(
            "no exact changed boundary node was established for impact propagation".to_owned(),
        );
    }

    let completeness = if unrepresentable || matched_evidence.is_empty() {
        MappingCompleteness::Unknown
    } else if !file.binary
        && file.status != ChangedFileStatus::Untracked
        && line_match
        && !fallback_match
        && matched_sides.len() == expected_side_count(file)
        && !boundary_node_ids.is_empty()
    {
        MappingCompleteness::Complete
    } else {
        MappingCompleteness::Partial
    };

    MappingWork {
        mapping: ChangedArtifactMapping {
            old_path: file.old_path.clone(),
            new_path: file.new_path.clone(),
            status: file.status,
            layer: file.source,
            matched_evidence_ids: matched_evidence.into_iter().collect(),
            artifact_node_ids,
            symbol_ref_node_ids,
            boundary_node_ids,
            matches: explanations.into_iter().collect(),
            completeness,
            gaps: gaps.into_iter().collect(),
        },
        representable_paths,
    }
}

fn relevant_sides(file: &ChangedFile) -> Vec<(ChangedPathSide, &NativePath)> {
    match file.status {
        ChangedFileStatus::Deleted => file
            .old_path
            .as_ref()
            .map(|path| vec![(ChangedPathSide::Old, path)])
            .unwrap_or_default(),
        ChangedFileStatus::Renamed => {
            let mut sides = Vec::with_capacity(2);
            if let Some(path) = &file.old_path {
                sides.push((ChangedPathSide::Old, path));
            }
            if let Some(path) = &file.new_path {
                sides.push((ChangedPathSide::New, path));
            }
            sides
        }
        _ => file
            .new_path
            .as_ref()
            .or(file.old_path.as_ref())
            .map(|path| vec![(ChangedPathSide::New, path)])
            .unwrap_or_default(),
    }
}

fn expected_side_count(file: &ChangedFile) -> usize {
    match file.status {
        ChangedFileStatus::Renamed => {
            usize::from(file.old_path.is_some()) + usize::from(file.new_path.is_some())
        }
        _ => usize::from(file.old_path.is_some() || file.new_path.is_some()),
    }
}

fn side_name(side: ChangedPathSide) -> &'static str {
    match side {
        ChangedPathSide::Old => "old",
        ChangedPathSide::New => "new",
    }
}

fn native_path_text(path: &NativePath) -> Option<String> {
    match path.encoding {
        NativePathEncoding::UnixBytes | NativePathEncoding::Utf8 => {
            std::str::from_utf8(&path.bytes).ok().map(ToOwned::to_owned)
        }
        NativePathEncoding::WindowsWide => {
            let (chunks, remainder) = path.bytes.as_chunks::<2>();
            if !remainder.is_empty() {
                return None;
            }
            let units = chunks.iter().map(|chunk| u16::from_le_bytes(*chunk));
            char::decode_utf16(units)
                .collect::<Result<String, _>>()
                .ok()
        }
    }
}

fn same_evidence_path(left: &str, right: &str) -> bool {
    left == right
}

fn hunk_ranges(file: &ChangedFile, side: ChangedPathSide) -> Vec<(usize, (u32, u32))> {
    file.hunks
        .iter()
        .enumerate()
        .filter_map(|(index, hunk)| {
            let (start, count) = match side {
                ChangedPathSide::Old => (hunk.old_start, hunk.old_count),
                ChangedPathSide::New => (hunk.new_start, hunk.new_count),
            };
            inclusive_range(start, count).map(|range| (index, range))
        })
        .collect()
}

fn inclusive_range(start: u32, count: u32) -> Option<(u32, u32)> {
    (count > 0).then(|| (start, start.saturating_add(count - 1)))
}

fn evidence_range(evidence: &Evidence) -> Option<(u32, u32)> {
    match (evidence.start_line, evidence.end_line) {
        (Some(start), Some(end)) if start <= end => Some((start, end)),
        _ => None,
    }
}

fn ranges_intersect(left: (u32, u32), right: (u32, u32)) -> bool {
    left.0 <= right.1 && right.0 <= left.1
}

fn match_explanation(
    evidence: &Evidence,
    path_side: ChangedPathSide,
    kind: EvidenceMatchKind,
    hunk_index: Option<usize>,
    hunk_range: Option<(u32, u32)>,
) -> HunkLineMatch {
    HunkLineMatch {
        evidence_id: evidence.id.clone(),
        path_side,
        kind,
        hunk_index,
        hunk_start: hunk_range.map(|range| range.0),
        hunk_end: hunk_range.map(|range| range.1),
        evidence_start: evidence.start_line,
        evidence_end: evidence.end_line,
    }
}

impl PartialOrd for HunkLineMatch {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HunkLineMatch {
    fn cmp(&self, other: &Self) -> Ordering {
        (
            &self.evidence_id,
            self.path_side,
            self.kind,
            self.hunk_index,
            self.hunk_start,
            self.hunk_end,
            self.evidence_start,
            self.evidence_end,
        )
            .cmp(&(
                &other.evidence_id,
                other.path_side,
                other.kind,
                other.hunk_index,
                other.hunk_start,
                other.hunk_end,
                other.evidence_start,
                other.evidence_end,
            ))
    }
}

fn nodes_for_evidence(
    evidence_ids: &BTreeSet<EvidenceId>,
    edges: &[Edge],
    nodes: &BTreeMap<NodeId, &Node>,
) -> (Vec<NodeId>, Vec<NodeId>, Vec<NodeId>) {
    let mut artifact = BTreeSet::new();
    let mut symbol = BTreeSet::new();
    let mut boundary = BTreeSet::new();
    for edge in edges {
        if !edge.evidence.iter().any(|id| evidence_ids.contains(id)) {
            continue;
        }
        for id in [&edge.source, &edge.target] {
            let Some(node) = nodes.get(id).copied() else {
                continue;
            };
            match node.kind {
                NodeKind::Artifact => {
                    artifact.insert(id.clone());
                }
                NodeKind::SymbolRef => {
                    symbol.insert(id.clone());
                }
                kind if is_boundary_kind(kind) => {
                    boundary.insert(id.clone());
                }
                _ => {}
            }
        }
    }
    (
        artifact.into_iter().collect(),
        symbol.into_iter().collect(),
        boundary.into_iter().collect(),
    )
}

fn is_boundary_kind(kind: NodeKind) -> bool {
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
            | NodeKind::ConfigKey
    )
}

fn retain_node_ids(mapping: &mut ChangedArtifactMapping, retained: &BTreeSet<NodeId>) {
    mapping.artifact_node_ids.retain(|id| retained.contains(id));
    mapping
        .symbol_ref_node_ids
        .retain(|id| retained.contains(id));
    mapping.boundary_node_ids.retain(|id| retained.contains(id));
}

fn degrade_mapping(mapping: &mut ChangedArtifactMapping, gap: &str) {
    mapping.completeness = MappingCompleteness::Partial;
    mapping.gaps.push(gap.to_owned());
    mapping.gaps.sort();
    mapping.gaps.dedup();
}

fn build_changed_entities(
    mappings: &[MappingWork],
    nodes: &BTreeMap<NodeId, &Node>,
) -> Vec<ChangedEntity> {
    let mut work = BTreeMap::<NodeId, EntityWork>::new();
    for mapping in mappings {
        for (role, ids) in [
            (
                ChangedEntityRole::Artifact,
                &mapping.mapping.artifact_node_ids,
            ),
            (
                ChangedEntityRole::SymbolRef,
                &mapping.mapping.symbol_ref_node_ids,
            ),
            (
                ChangedEntityRole::Boundary,
                &mapping.mapping.boundary_node_ids,
            ),
        ] {
            for id in ids {
                let entity = work.entry(id.clone()).or_default();
                entity.roles.insert(role);
                entity
                    .paths
                    .extend(mapping.mapping.old_path.iter().cloned());
                entity
                    .paths
                    .extend(mapping.mapping.new_path.iter().cloned());
            }
        }
    }
    work.into_iter()
        .filter_map(|(id, work)| {
            nodes.get(&id).map(|node| ChangedEntity {
                node: (*node).clone(),
                roles: work.roles.into_iter().collect(),
                changed_paths: work.paths.into_iter().collect(),
            })
        })
        .collect()
}

fn build_contract_deltas(
    contract_ids: &BTreeSet<NodeId>,
    mappings: &[MappingWork],
    nodes: &BTreeMap<NodeId, &Node>,
    inputs: &[ContractCompatibilityInput],
) -> Vec<SemanticContractDelta> {
    contract_ids
        .iter()
        .filter_map(|contract_id| {
            let contract = nodes.get(contract_id).copied()?;
            let paths = mappings
                .iter()
                .filter(|work| work.mapping.boundary_node_ids.contains(contract_id))
                .flat_map(|work| work.representable_paths.iter().cloned())
                .collect::<BTreeSet<_>>();
            let matching = inputs
                .iter()
                .filter(|input| {
                    input.contract_node_id == *contract_id
                        && input.file_path.as_ref().is_none_or(|path| {
                            paths
                                .iter()
                                .any(|candidate| same_evidence_path(candidate, path))
                        })
                })
                .collect::<Vec<_>>();
            Some(merge_contract_delta(contract, &matching))
        })
        .collect()
}

fn merge_contract_delta(
    contract: &Node,
    inputs: &[&ContractCompatibilityInput],
) -> SemanticContractDelta {
    let reports = inputs
        .iter()
        .filter_map(|input| input.report.as_ref())
        .collect::<Vec<_>>();
    let mut status = reports
        .iter()
        .map(|report| report.status)
        .max_by_key(|value| compatibility_severity(*value))
        .unwrap_or(CompatibilityStatus::Unknown);
    let before = unique_fingerprint(
        reports
            .iter()
            .map(|report| report.before_fingerprint.as_str()),
    );
    let after = unique_fingerprint(
        reports
            .iter()
            .map(|report| report.after_fingerprint.as_str()),
    );
    if before.is_none() || after.is_none() {
        status = CompatibilityStatus::Unknown;
    }
    let mut findings = reports
        .iter()
        .flat_map(|report| report.findings.iter().cloned())
        .collect::<Vec<_>>();
    findings.sort_by(|left, right| {
        (
            compatibility_severity(left.status),
            &left.code,
            &left.path,
            &left.factors,
            &left.evidence,
        )
            .cmp(&(
                compatibility_severity(right.status),
                &right.code,
                &right.path,
                &right.factors,
                &right.evidence,
            ))
            .reverse()
    });
    findings.dedup();
    let factors = findings
        .iter()
        .flat_map(|finding| finding.factors.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let validations = findings
        .iter()
        .flat_map(|finding| finding.recommended_validations.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    SemanticContractDelta {
        contract: contract.clone(),
        contract_kind: contract.kind,
        status,
        before_fingerprint: before,
        after_fingerprint: after,
        findings,
        factors,
        validations,
    }
}

fn compatibility_severity(status: CompatibilityStatus) -> u8 {
    match status {
        CompatibilityStatus::Compatible => 0,
        CompatibilityStatus::Incomparable => 1,
        CompatibilityStatus::Unknown => 2,
        CompatibilityStatus::PotentiallyBreaking => 3,
        CompatibilityStatus::Breaking => 4,
    }
}

fn unique_fingerprint<'a>(values: impl Iterator<Item = &'a str>) -> Option<String> {
    let values = values
        .filter(|value| !value.is_empty() && *value != "unavailable")
        .collect::<BTreeSet<_>>();
    (values.len() == 1).then(|| values.into_iter().next().unwrap_or_default().to_owned())
}

fn impact_compatibility_input(delta: &SemanticContractDelta) -> CompatibilityInput {
    CompatibilityInput {
        contract_node_id: delta.contract.id.clone(),
        status: match delta.status {
            CompatibilityStatus::Breaking => ImpactCompatibilityStatus::Breaking,
            CompatibilityStatus::PotentiallyBreaking => {
                ImpactCompatibilityStatus::PotentiallyBreaking
            }
            CompatibilityStatus::Compatible => ImpactCompatibilityStatus::Compatible,
            CompatibilityStatus::Unknown | CompatibilityStatus::Incomparable => {
                ImpactCompatibilityStatus::Unknown
            }
        },
        evidence: delta
            .findings
            .iter()
            .map(|finding| finding.code.clone())
            .chain(delta.factors.iter().cloned())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect(),
        recommended_validations: delta.validations.clone(),
    }
}

fn freshness_for_change_set(
    change_set: &ChangeSet,
    freshness: &[RepoFreshness],
) -> Vec<RepoFreshness> {
    let mut normalized = freshness.to_vec();
    for item in &mut normalized {
        if item.repo_id != change_set.repo_id {
            continue;
        }
        if item.checkout_id != change_set.checkout_id {
            item.state = RepoFreshnessState::Unknown;
            item.reason =
                Some("snapshot checkout does not match the analyzed change set".to_owned());
        } else if item.head_commit.as_deref() != Some(change_set.checkout_head_sha.as_str()) {
            item.state = RepoFreshnessState::CommitsBehind;
            item.reason = Some("snapshot HEAD does not match the analyzed change set".to_owned());
        } else if item.manifest_hash != change_set.workspace_manifest_hash {
            item.state = RepoFreshnessState::ConfigChanged;
            item.reason =
                Some("snapshot manifest does not match the analyzed change set".to_owned());
        }
    }
    normalized.sort_by(|left, right| {
        (&left.repo_id, &left.checkout_id).cmp(&(&right.repo_id, &right.checkout_id))
    });
    normalized
}

fn derive_service_memberships(nodes: &[Node], edges: &[Edge]) -> BTreeMap<NodeId, Vec<NodeId>> {
    let kinds = nodes
        .iter()
        .map(|node| (node.id.clone(), node.kind))
        .collect::<BTreeMap<_, _>>();
    let mut memberships = BTreeMap::<NodeId, BTreeSet<NodeId>>::new();
    for edge in edges {
        if edge.kind != code_system_graph_model::EdgeKind::Contains {
            continue;
        }
        match (kinds.get(&edge.source), kinds.get(&edge.target)) {
            (Some(NodeKind::Service), Some(_)) => {
                memberships
                    .entry(edge.target.clone())
                    .or_default()
                    .insert(edge.source.clone());
            }
            (Some(_), Some(NodeKind::Service)) => {
                memberships
                    .entry(edge.source.clone())
                    .or_default()
                    .insert(edge.target.clone());
            }
            _ => {}
        }
    }
    memberships
        .into_iter()
        .map(|(member, services)| (member, services.into_iter().collect()))
        .collect()
}

fn aggregate_touched(
    change_set: &ChangeSet,
    changed_node_ids: &BTreeSet<NodeId>,
    nodes: &BTreeMap<NodeId, &Node>,
    community: Option<&CommunitySnapshot>,
    deltas: &[SemanticContractDelta],
    impacts: &[ImpactReport],
) -> (Vec<RepoId>, Vec<Node>, Vec<CommunityId>, Vec<Node>) {
    let mut repositories = BTreeSet::from([change_set.repo_id.clone()]);
    let mut services = BTreeMap::<NodeId, Node>::new();
    let mut communities = BTreeSet::new();
    let mut contracts = BTreeMap::<NodeId, Node>::new();
    for id in changed_node_ids {
        if let Some(node) = nodes.get(id).copied() {
            repositories.extend(node.repo_id.iter().cloned());
            if node.kind == NodeKind::Service {
                services.insert(node.id.clone(), node.clone());
            }
            if is_boundary_kind(node.kind) {
                contracts.insert(node.id.clone(), node.clone());
            }
        }
    }
    for delta in deltas {
        contracts.insert(delta.contract.id.clone(), delta.contract.clone());
    }
    for impact in impacts {
        repositories.extend(
            impact
                .affected_repositories
                .iter()
                .map(|item| item.repo_id.clone()),
        );
        services.extend(
            impact
                .affected_services
                .iter()
                .map(|item| (item.service.id.clone(), item.service.clone())),
        );
        communities.extend(
            impact
                .affected_communities
                .iter()
                .map(|item| item.community_id.clone()),
        );
        contracts.extend(
            impact
                .affected_contracts
                .iter()
                .map(|item| (item.contract.id.clone(), item.contract.clone())),
        );
    }
    if let Some(snapshot) = community {
        for item in &snapshot.communities {
            if item
                .members
                .iter()
                .any(|member| changed_node_ids.contains(member))
            {
                communities.insert(item.id.clone());
            }
        }
    }
    (
        repositories.into_iter().collect(),
        services.into_values().collect(),
        communities.into_iter().collect(),
        contracts.into_values().collect(),
    )
}

fn summarize(
    coverage: &ChangeAnalysisCoverage,
    changed_entities: usize,
    changed_contracts: usize,
    impacts: &[ImpactReport],
) -> ChangeAnalysisSummary {
    let highest_risk = if coverage.complete {
        impacts
            .iter()
            .map(|impact| impact.risk)
            .max()
            .unwrap_or(RiskLevel::Low)
    } else {
        RiskLevel::Unknown
    };
    let conclusion = if !coverage.complete {
        ChangeConclusion::Unknown
    } else if changed_entities > 0 || changed_contracts > 0 || !impacts.is_empty() {
        ChangeConclusion::ImpactDetected
    } else {
        ChangeConclusion::NoSemanticImpactDetected
    };
    ChangeAnalysisSummary {
        conclusion,
        highest_risk,
        changed_entities,
        changed_contracts,
        impact_reports: impacts.len(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use code_system_graph_model::{
        CheckoutId, Community, CommunityAlgorithm, CommunityConfig, CommunityId, CommunityMetrics, CommunityScope, EdgeId, EdgeKind, EpistemicStatus, NativePathEncoding, Provenance
    };

    use super::*;
    use crate::{ChangeHunk, ChangeScope};

    type Fixture = (
        ChangeSet,
        Vec<Node>,
        Vec<Edge>,
        Vec<Evidence>,
        Vec<RepoFreshness>,
        Vec<ContractCompatibilityInput>,
    );

    fn path(value: &str) -> NativePath {
        NativePath {
            encoding: NativePathEncoding::Utf8,
            bytes: value.as_bytes().to_vec(),
            display: value.to_owned(),
        }
    }

    fn hunk(start: u32, count: u32) -> ChangeHunk {
        ChangeHunk {
            old_start: start,
            old_count: count,
            new_start: start,
            new_count: count,
            lines: Vec::new(),
        }
    }

    fn file(status: ChangedFileStatus, value: &str) -> ChangedFile {
        ChangedFile {
            status,
            old_path: (status != ChangedFileStatus::Added
                && status != ChangedFileStatus::Untracked)
                .then(|| path(value)),
            new_path: (status != ChangedFileStatus::Deleted).then(|| path(value)),
            binary: false,
            hunks: vec![hunk(10, 2)],
            source: if status == ChangedFileStatus::Untracked {
                ChangeSourceLayer::Untracked
            } else {
                ChangeSourceLayer::Worktree
            },
        }
    }

    fn rename(old: &str, new: &str) -> ChangedFile {
        ChangedFile {
            status: ChangedFileStatus::Renamed,
            old_path: Some(path(old)),
            new_path: Some(path(new)),
            binary: false,
            hunks: vec![hunk(10, 2)],
            source: ChangeSourceLayer::Commit,
        }
    }

    fn change_set(files: Vec<ChangedFile>) -> ChangeSet {
        ChangeSet {
            repo_id: RepoId::new("repo:api"),
            checkout_id: CheckoutId::new("checkout:repo:api"),
            worktree: path("/workspace/api"),
            git_common_dir: path("/workspace/api/.git"),
            scope: ChangeScope::All,
            checkout_head_ref: Some("refs/heads/main".to_owned()),
            checkout_head_sha: "head".to_owned(),
            base_ref: Some("main".to_owned()),
            head_ref: Some("HEAD".to_owned()),
            head_sha: "head".to_owned(),
            staged_hash: "staged".to_owned(),
            worktree_hash: "worktree".to_owned(),
            exact_diff_fingerprint: "diff".to_owned(),
            workspace_manifest_hash: "manifest".to_owned(),
            contract_registry_hash: "registry".to_owned(),
            analyzer_versions: BTreeMap::from([("extractor".to_owned(), "1.0.0".to_owned())]),
            files,
        }
    }

    fn node(id: &str, kind: NodeKind, repository: &str) -> Node {
        Node {
            id: NodeId::new(id),
            kind,
            repo_id: Some(RepoId::new(repository)),
            stable_key: id.to_owned(),
            label: id.to_owned(),
        }
    }

    fn evidence(id: &str, file_path: &str, start: Option<u32>, end: Option<u32>) -> Evidence {
        Evidence {
            id: EvidenceId::new(id),
            repo_id: Some(RepoId::new("repo:api")),
            file_path: Some(file_path.to_owned()),
            start_line: start,
            end_line: end,
            extractor: "test".to_owned(),
            extractor_version: "1.0.0".to_owned(),
            provenance: Provenance::Extracted,
            confidence: 1.0,
            observed_at_commit: Some("head".to_owned()),
            content_hash: Some("content".to_owned()),
            note: None,
        }
    }

    fn edge(id: &str, source: &str, target: &str, evidence_id: &str) -> Edge {
        Edge {
            id: EdgeId::new(id),
            source: NodeId::new(source),
            target: NodeId::new(target),
            kind: EdgeKind::Consumes,
            confidence: 1.0,
            status: EpistemicStatus::Confirmed,
            evidence: vec![EvidenceId::new(evidence_id)],
        }
    }

    fn freshness(repository: &str) -> RepoFreshness {
        RepoFreshness {
            repo_id: RepoId::new(repository),
            checkout_id: CheckoutId::new(format!("checkout:{repository}")),
            head_commit: Some("head".to_owned()),
            manifest_hash: "manifest".to_owned(),
            state: RepoFreshnessState::Fresh,
            reason: None,
        }
    }

    fn finding(status: CompatibilityStatus) -> CompatibilityFinding {
        CompatibilityFinding {
            code: "http.operation_removed".to_owned(),
            path: "GET /orders".to_owned(),
            status,
            factors: vec!["operation was removed".to_owned()],
            evidence: vec!["line:10".to_owned()],
            recommended_validations: vec!["run contract tests".to_owned()],
        }
    }

    fn compatibility(status: CompatibilityStatus) -> ContractCompatibilityInput {
        ContractCompatibilityInput {
            file_path: Some("src/api.rs".to_owned()),
            contract_node_id: NodeId::new("contract"),
            report: Some(CompatibilityReport {
                status,
                before_fingerprint: "before".to_owned(),
                after_fingerprint: "after".to_owned(),
                findings: vec![finding(status)],
            }),
        }
    }

    fn complete_fixture() -> Fixture {
        (
            change_set(vec![file(ChangedFileStatus::Modified, "src/api.rs")]),
            vec![
                node("contract", NodeKind::HttpOperation, "repo:api"),
                node("direct", NodeKind::Service, "repo:web"),
                node("transitive", NodeKind::TestCase, "repo:test"),
            ],
            vec![
                edge("edge:direct", "direct", "contract", "ev"),
                edge("edge:transitive", "transitive", "direct", "other"),
            ],
            vec![evidence("ev", "src/api.rs", Some(10), Some(11))],
            vec![
                freshness("repo:api"),
                freshness("repo:web"),
                freshness("repo:test"),
            ],
            vec![compatibility(CompatibilityStatus::Breaking)],
        )
    }

    fn analyze_fixture(
        options: &ChangeAnalysisOptions,
    ) -> Result<ChangeImpactReport, ChangeAnalysisError> {
        let (changes, nodes, edges, evidence, freshness, compatibility) = complete_fixture();
        analyze_changes(
            &changes,
            &nodes,
            &edges,
            &evidence,
            None,
            &freshness,
            &compatibility,
            options,
        )
    }

    #[test]
    fn mapping_should_intersect_exact_hunk_lines() {
        let changed = file(ChangedFileStatus::Modified, "src/api.rs");
        let nodes = BTreeMap::from([
            (
                NodeId::new("contract"),
                node("contract", NodeKind::HttpOperation, "repo:api"),
            ),
            (
                NodeId::new("consumer"),
                node("consumer", NodeKind::Service, "repo:web"),
            ),
        ]);
        let evidence = BTreeMap::from([(
            EvidenceId::new("ev"),
            evidence("ev", "src/api.rs", Some(11), Some(12)),
        )]);
        let node_refs = nodes
            .iter()
            .map(|(id, item)| (id.clone(), item))
            .collect::<BTreeMap<_, _>>();
        let evidence_refs = evidence
            .iter()
            .map(|(id, item)| (id.clone(), item))
            .collect::<BTreeMap<_, _>>();

        let result = map_changed_file(
            &changed,
            &RepoId::new("repo:api"),
            &[edge("edge", "consumer", "contract", "ev")],
            &node_refs,
            &evidence_refs,
        );

        assert_eq!(result.mapping.completeness, MappingCompleteness::Complete);
    }

    #[test]
    fn mapping_should_exclude_evidence_outside_hunks() {
        let changed = file(ChangedFileStatus::Modified, "src/api.rs");
        let stored = evidence("ev", "src/api.rs", Some(30), Some(31));
        let evidence_refs = BTreeMap::from([(stored.id.clone(), &stored)]);

        let result = map_changed_file(
            &changed,
            &RepoId::new("repo:api"),
            &[],
            &BTreeMap::new(),
            &evidence_refs,
        );

        assert_eq!(result.mapping.matched_evidence_ids, Vec::new());
    }

    #[test]
    fn mapping_should_not_normalize_distinct_native_path_bytes() {
        let changed = file(ChangedFileStatus::Modified, "src\\api.rs");
        let stored = evidence("ev", "src/api.rs", Some(10), Some(11));
        let evidence_refs = BTreeMap::from([(stored.id.clone(), &stored)]);

        let result = map_changed_file(
            &changed,
            &RepoId::new("repo:api"),
            &[],
            &BTreeMap::new(),
            &evidence_refs,
        );

        assert_eq!(result.mapping.matched_evidence_ids, Vec::new());
    }

    #[test]
    fn mapping_should_use_file_fallback_without_line_evidence() {
        let changed = file(ChangedFileStatus::Modified, "src/api.rs");
        let stored = evidence("ev", "src/api.rs", None, None);
        let evidence_refs = BTreeMap::from([(stored.id.clone(), &stored)]);

        let result = map_changed_file(
            &changed,
            &RepoId::new("repo:api"),
            &[],
            &BTreeMap::new(),
            &evidence_refs,
        );

        assert!(matches!(
            result.mapping.matches.as_slice(),
            [HunkLineMatch {
                kind: EvidenceMatchKind::FilePathFallback,
                ..
            }]
        ));
    }

    #[test]
    fn rename_should_include_old_and_new_neighborhoods() {
        let changed = rename("src/old.rs", "src/new.rs");
        let old = evidence("old", "src/old.rs", Some(10), Some(10));
        let new = evidence("new", "src/new.rs", Some(10), Some(10));
        let evidence_refs = BTreeMap::from([(old.id.clone(), &old), (new.id.clone(), &new)]);

        let result = map_changed_file(
            &changed,
            &RepoId::new("repo:api"),
            &[],
            &BTreeMap::new(),
            &evidence_refs,
        );

        assert_eq!(result.mapping.matched_evidence_ids.len(), 2);
    }

    #[test]
    fn deletion_should_map_old_path_and_old_hunk() {
        let changed = file(ChangedFileStatus::Deleted, "src/old.rs");
        let old = evidence("old", "src/old.rs", Some(10), Some(10));
        let evidence_refs = BTreeMap::from([(old.id.clone(), &old)]);

        let result = map_changed_file(
            &changed,
            &RepoId::new("repo:api"),
            &[],
            &BTreeMap::new(),
            &evidence_refs,
        );

        assert_eq!(result.mapping.matches[0].path_side, ChangedPathSide::Old);
    }

    #[test]
    fn binary_change_should_remain_incomplete() {
        let mut changed = file(ChangedFileStatus::Modified, "asset.bin");
        changed.binary = true;
        let stored = evidence("ev", "asset.bin", Some(10), Some(10));
        let evidence_refs = BTreeMap::from([(stored.id.clone(), &stored)]);

        let result = map_changed_file(
            &changed,
            &RepoId::new("repo:api"),
            &[],
            &BTreeMap::new(),
            &evidence_refs,
        );

        assert_ne!(result.mapping.completeness, MappingCompleteness::Complete);
    }

    #[test]
    fn untracked_change_should_remain_incomplete() {
        let changed = file(ChangedFileStatus::Untracked, "src/new.rs");
        let stored = evidence("ev", "src/new.rs", None, None);
        let evidence_refs = BTreeMap::from([(stored.id.clone(), &stored)]);

        let result = map_changed_file(
            &changed,
            &RepoId::new("repo:api"),
            &[],
            &BTreeMap::new(),
            &evidence_refs,
        );

        assert_eq!(result.mapping.completeness, MappingCompleteness::Partial);
    }

    #[test]
    fn non_unicode_native_path_should_be_unknown() {
        let mut changed = file(ChangedFileStatus::Modified, "ignored");
        changed.new_path = Some(NativePath {
            encoding: NativePathEncoding::UnixBytes,
            bytes: vec![0xff],
            display: "�".to_owned(),
        });

        let result = map_changed_file(
            &changed,
            &RepoId::new("repo:api"),
            &[],
            &BTreeMap::new(),
            &BTreeMap::new(),
        );

        assert_eq!(result.mapping.completeness, MappingCompleteness::Unknown);
    }

    #[test]
    fn edge_evidence_should_map_artifact_symbol_and_boundary_nodes() {
        let changed = file(ChangedFileStatus::Modified, "src/api.rs");
        let node_values = [
            node("artifact", NodeKind::Artifact, "repo:api"),
            node("symbol", NodeKind::SymbolRef, "repo:api"),
            node("contract", NodeKind::HttpOperation, "repo:api"),
        ];
        let nodes = node_values
            .iter()
            .map(|item| (item.id.clone(), item))
            .collect::<BTreeMap<_, _>>();
        let stored = evidence("ev", "src/api.rs", Some(10), Some(10));
        let evidence_refs = BTreeMap::from([(stored.id.clone(), &stored)]);
        let edges = vec![
            edge("one", "artifact", "symbol", "ev"),
            edge("two", "symbol", "contract", "ev"),
        ];

        let result = map_changed_file(
            &changed,
            &RepoId::new("repo:api"),
            &edges,
            &nodes,
            &evidence_refs,
        );

        assert!(
            result.mapping.artifact_node_ids == vec![NodeId::new("artifact")]
                && result.mapping.symbol_ref_node_ids == vec![NodeId::new("symbol")]
                && result.mapping.boundary_node_ids == vec![NodeId::new("contract")]
        );
    }

    #[test]
    fn breaking_delta_should_propagate_direct_and_transitive_impact()
    -> Result<(), ChangeAnalysisError> {
        let result = analyze_fixture(&ChangeAnalysisOptions::default())?;

        assert!(
            result.contract_deltas[0].status == CompatibilityStatus::Breaking
                && result.impacts[0]
                    .direct_consumers
                    .iter()
                    .any(|item| item.node.id == NodeId::new("direct"))
                && result.impacts[0]
                    .transitive_consumers
                    .iter()
                    .any(|item| item.node.id == NodeId::new("transitive"))
        );
        Ok(())
    }

    #[test]
    fn changed_nodes_should_aggregate_community() -> Result<(), ChangeAnalysisError> {
        let (changes, nodes, edges, evidence, freshness, compatibility) = complete_fixture();
        let community = CommunitySnapshot {
            snapshot_id: "snapshot".to_owned(),
            engine_version: "1.0.0".to_owned(),
            config: CommunityConfig {
                algorithm: CommunityAlgorithm::ConnectedComponents,
                scope: CommunityScope::Federated,
                seed: 0,
                resolution: 1.0,
                minimum_confidence: 0.8,
                edge_weights: Vec::new(),
                max_iterations: 10,
            },
            communities: vec![Community {
                id: CommunityId::new("community"),
                label: "orders".to_owned(),
                members: vec![NodeId::new("contract"), NodeId::new("direct")],
                central_nodes: vec![NodeId::new("contract")],
                repositories: vec![RepoId::new("repo:api"), RepoId::new("repo:web")],
                services: vec![NodeId::new("direct")],
                inbound_contracts: Vec::new(),
                outbound_contracts: vec![NodeId::new("contract")],
                metrics: CommunityMetrics {
                    size: 2,
                    density: 1.0,
                    cohesion: 1.0,
                    coupling: 0.0,
                    cross_community_edges: 0,
                },
                label_evidence: Vec::new(),
                limitations: Vec::new(),
            }],
        };

        let result = analyze_changes(
            &changes,
            &nodes,
            &edges,
            &evidence,
            Some(&community),
            &freshness,
            &compatibility,
            &ChangeAnalysisOptions::default(),
        )?;

        assert_eq!(
            result.touched_communities,
            vec![CommunityId::new("community")]
        );
        Ok(())
    }

    #[test]
    fn shuffled_inputs_should_produce_same_fingerprint_and_output()
    -> Result<(), ChangeAnalysisError> {
        let (changes, mut nodes, mut edges, mut evidence, mut freshness, compatibility) =
            complete_fixture();
        let first = analyze_changes(
            &changes,
            &nodes,
            &edges,
            &evidence,
            None,
            &freshness,
            &compatibility,
            &ChangeAnalysisOptions::default(),
        )?;
        nodes.reverse();
        edges.reverse();
        evidence.reverse();
        freshness.reverse();
        let second = analyze_changes(
            &changes,
            &nodes,
            &edges,
            &evidence,
            None,
            &freshness,
            &compatibility,
            &ChangeAnalysisOptions::default(),
        )?;

        assert!(
            first.analyzer_fingerprint == second.analyzer_fingerprint
                && first.mappings == second.mappings
                && first.impacts == second.impacts
        );
        Ok(())
    }

    #[test]
    fn changed_file_limit_should_force_unknown_coverage() -> Result<(), ChangeAnalysisError> {
        let (mut changes, nodes, edges, evidence, freshness, compatibility) = complete_fixture();
        changes
            .files
            .push(file(ChangedFileStatus::Modified, "src/other.rs"));
        let options = ChangeAnalysisOptions {
            max_changed_files: 1,
            ..ChangeAnalysisOptions::default()
        };

        let result = analyze_changes(
            &changes,
            &nodes,
            &edges,
            &evidence,
            None,
            &freshness,
            &compatibility,
            &options,
        )?;

        assert!(
            result.coverage.truncated
                && result.summary.conclusion == ChangeConclusion::Unknown
                && result.summary.highest_risk == RiskLevel::Unknown
        );
        Ok(())
    }

    #[test]
    fn changed_node_limit_should_bound_entities() -> Result<(), ChangeAnalysisError> {
        let (changes, mut nodes, mut edges, evidence, freshness, compatibility) =
            complete_fixture();
        nodes.push(node("artifact", NodeKind::Artifact, "repo:api"));
        edges.push(edge("edge:artifact", "artifact", "contract", "ev"));
        let options = ChangeAnalysisOptions {
            max_changed_nodes: 1,
            ..ChangeAnalysisOptions::default()
        };

        let result = analyze_changes(
            &changes,
            &nodes,
            &edges,
            &evidence,
            None,
            &freshness,
            &compatibility,
            &options,
        )?;

        assert!(
            result.coverage.retained_changed_nodes == 1
                && result.coverage.total_changed_nodes == 2
                && result.coverage.truncated
        );
        Ok(())
    }

    #[test]
    fn impact_target_limit_should_bound_reports() -> Result<(), ChangeAnalysisError> {
        let (changes, mut nodes, mut edges, evidence, freshness, mut compatibility) =
            complete_fixture();
        nodes.push(node("contract-two", NodeKind::RpcMethod, "repo:api"));
        edges.push(edge("edge:two", "direct", "contract-two", "ev"));
        compatibility.push(ContractCompatibilityInput {
            file_path: Some("src/api.rs".to_owned()),
            contract_node_id: NodeId::new("contract-two"),
            report: Some(CompatibilityReport {
                status: CompatibilityStatus::Compatible,
                before_fingerprint: "before-two".to_owned(),
                after_fingerprint: "after-two".to_owned(),
                findings: Vec::new(),
            }),
        });
        let options = ChangeAnalysisOptions {
            max_impact_targets: 1,
            ..ChangeAnalysisOptions::default()
        };

        let result = analyze_changes(
            &changes,
            &nodes,
            &edges,
            &evidence,
            None,
            &freshness,
            &compatibility,
            &options,
        )?;

        assert!(
            result.impacts.len() == 1
                && result.coverage.total_impact_targets == 2
                && result.coverage.truncated
        );
        Ok(())
    }

    #[test]
    fn entity_pagination_should_follow_stable_node_order() -> Result<(), ChangeAnalysisError> {
        let (changes, mut nodes, mut edges, evidence, freshness, compatibility) =
            complete_fixture();
        nodes.push(node("artifact", NodeKind::Artifact, "repo:api"));
        nodes.push(node("symbol", NodeKind::SymbolRef, "repo:api"));
        edges.push(edge("edge:artifact", "artifact", "symbol", "ev"));
        edges.push(edge("edge:symbol", "symbol", "contract", "ev"));
        let options = ChangeAnalysisOptions {
            offset: 1,
            limit: 1,
            ..ChangeAnalysisOptions::default()
        };

        let result = analyze_changes(
            &changes,
            &nodes,
            &edges,
            &evidence,
            None,
            &freshness,
            &compatibility,
            &options,
        )?;

        assert_eq!(result.changed_entities[0].node.id, NodeId::new("contract"));
        Ok(())
    }

    #[test]
    fn summary_only_should_omit_detailed_entities_and_impact_items()
    -> Result<(), ChangeAnalysisError> {
        let options = ChangeAnalysisOptions {
            summary_only: true,
            ..ChangeAnalysisOptions::default()
        };

        let result = analyze_fixture(&options)?;

        assert!(
            result.changed_entities.is_empty()
                && result.impacts[0].direct_consumers.is_empty()
                && result.summary.impact_reports == 1
        );
        Ok(())
    }

    #[test]
    fn missing_compatibility_input_should_be_unknown() -> Result<(), ChangeAnalysisError> {
        let (changes, nodes, edges, evidence, freshness, _) = complete_fixture();

        let result = analyze_changes(
            &changes,
            &nodes,
            &edges,
            &evidence,
            None,
            &freshness,
            &[],
            &ChangeAnalysisOptions::default(),
        )?;

        assert_eq!(
            result.contract_deltas[0].status,
            CompatibilityStatus::Unknown
        );
        Ok(())
    }

    #[test]
    fn absent_contract_fingerprint_should_be_unknown() -> Result<(), ChangeAnalysisError> {
        let (changes, nodes, edges, evidence, freshness, mut compatibility) = complete_fixture();
        compatibility[0].report = Some(CompatibilityReport {
            status: CompatibilityStatus::Compatible,
            before_fingerprint: String::new(),
            after_fingerprint: "after".to_owned(),
            findings: Vec::new(),
        });

        let result = analyze_changes(
            &changes,
            &nodes,
            &edges,
            &evidence,
            None,
            &freshness,
            &compatibility,
            &ChangeAnalysisOptions::default(),
        )?;

        assert_eq!(
            result.contract_deltas[0].status,
            CompatibilityStatus::Unknown
        );
        Ok(())
    }

    #[test]
    fn validation_should_invalidate_changed_head() -> Result<(), ChangeAnalysisError> {
        let report = analyze_fixture(&ChangeAnalysisOptions::default())?;
        let mut current = ChangeValidityInput::from(&report.change_set);
        current.checkout_head_sha = "advanced".to_owned();

        let result = validate_change_analysis(&report, &current);

        assert!(matches!(result, ChangeValidity::Stale { .. }));
        Ok(())
    }

    #[test]
    fn validation_should_invalidate_changed_manifest() -> Result<(), ChangeAnalysisError> {
        let report = analyze_fixture(&ChangeAnalysisOptions::default())?;
        let mut current = ChangeValidityInput::from(&report.change_set);
        current.workspace_manifest_hash = "new-manifest".to_owned();

        let result = validate_change_analysis(&report, &current);

        assert!(matches!(result, ChangeValidity::Stale { .. }));
        Ok(())
    }

    #[test]
    fn zero_bounds_should_be_rejected() {
        let options = ChangeAnalysisOptions {
            max_impact_targets: 0,
            ..ChangeAnalysisOptions::default()
        };

        let result = analyze_fixture(&options);

        assert!(matches!(result, Err(ChangeAnalysisError::InvalidBounds)));
    }

    #[test]
    fn serialized_report_should_contain_positions_but_no_diff_body()
    -> Result<(), Box<dyn std::error::Error>> {
        let report = analyze_fixture(&ChangeAnalysisOptions::default())?;

        let encoded = serde_json::to_string(&report)?;

        assert!(
            encoded.contains("\"old_start\":10")
                && encoded.contains("\"new_start\":10")
                && !encoded.contains("\"source_body\"")
                && !encoded.contains("\"diff_body\"")
        );
        Ok(())
    }
}

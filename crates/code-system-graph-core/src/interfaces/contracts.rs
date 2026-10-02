//! Source-free contract operations and deterministic contract projections.

use std::collections::{BTreeMap, BTreeSet};

use code_system_graph_model::{
    Edge, EdgeId, EpistemicStatus, Evidence, EvidenceId, Node, NodeId, NodeKind
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    EvidenceMetadata, INTERFACE_RESULT_VERSION, INTERFACE_SCHEMA_VERSION, InterfaceError, MAX_EXPORT_NODES, enum_name, evidence_for_edge, index_edges, index_evidence, index_nodes, validate_bound
};
use crate::{CompatibilityReport, CompatibilityStatus};

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

/// Canonical graph node kinds treated as inspectable contracts.
pub const CONTRACT_NODE_KINDS: [NodeKind; 9] = [
    NodeKind::Package,
    NodeKind::HttpOperation,
    NodeKind::GraphqlOperation,
    NodeKind::RpcMethod,
    NodeKind::EventChannel,
    NodeKind::EventSchema,
    NodeKind::Database,
    NodeKind::DatabaseTable,
    NodeKind::DatabaseColumn,
];

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
    let evidence_edges = index_evidence_edges(&edge_index);
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
        evidence_edges: &evidence_edges,
        evidence: &evidence_index,
        compatibility,
    };
    let mut selected = select_contract_action(&context, request, &mut issues)?;
    if request.action == ContractAction::List {
        issues.retain(|issue| issue.severity == ContractIssueSeverity::Error);
    }
    if issues
        .iter()
        .any(|issue| issue.severity != ContractIssueSeverity::Warning)
    {
        selected.complete = false;
    }
    issues.sort_by(issue_order);
    issues.dedup();
    if issues.len() > request.limit {
        issues.truncate(request.limit);
        selected.truncated = true;
    }
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
    evidence_edges: &'a BTreeMap<EvidenceId, BTreeSet<EdgeId>>,
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
    issues.retain(|issue| issue_applies_to(issue, id, context.edges, context.evidence_edges));
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
        issues.retain(|issue| issue_applies_to(issue, id, context.edges, context.evidence_edges));
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
    issues.retain(|issue| {
        issue_applies_to(issue, before_id, context.edges, context.evidence_edges)
            || issue_applies_to(issue, after_id, context.edges, context.evidence_edges)
    });
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
    issues.retain(|issue| {
        issue_applies_to(issue, source_id, context.edges, context.evidence_edges)
            || issue_applies_to(issue, target_id, context.edges, context.evidence_edges)
    });
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

fn is_contract_kind(kind: NodeKind) -> bool {
    CONTRACT_NODE_KINDS.contains(&kind)
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
    issue_severity_rank(left.severity)
        .cmp(&issue_severity_rank(right.severity))
        .then_with(|| left.code.cmp(&right.code))
        .then_with(|| left.entity_id.cmp(&right.entity_id))
        .then_with(|| left.message.cmp(&right.message))
}

const fn issue_severity_rank(severity: ContractIssueSeverity) -> u8 {
    match severity {
        ContractIssueSeverity::Error => 0,
        ContractIssueSeverity::Unknown => 1,
        ContractIssueSeverity::Warning => 2,
    }
}

fn issue_applies_to(
    issue: &ContractIssue,
    contract_id: &NodeId,
    edges: &BTreeMap<EdgeId, &Edge>,
    evidence_edges: &BTreeMap<EvidenceId, BTreeSet<EdgeId>>,
) -> bool {
    issue.entity_id.as_deref().is_some_and(|entity_id| {
        entity_id == contract_id.as_str()
            || edges
                .get(&EdgeId::new(entity_id))
                .is_some_and(|edge| edge.source == *contract_id || edge.target == *contract_id)
            || evidence_edges
                .get(&EvidenceId::new(entity_id))
                .is_some_and(|incident| {
                    incident.iter().any(|edge_id| {
                        edges.get(edge_id).is_some_and(|edge| {
                            edge.source == *contract_id || edge.target == *contract_id
                        })
                    })
                })
    })
}

fn index_evidence_edges(edges: &BTreeMap<EdgeId, &Edge>) -> BTreeMap<EvidenceId, BTreeSet<EdgeId>> {
    let mut index = BTreeMap::<EvidenceId, BTreeSet<EdgeId>>::new();
    for edge in edges.values() {
        for evidence_id in &edge.evidence {
            index
                .entry(evidence_id.clone())
                .or_default()
                .insert(edge.id.clone());
        }
    }
    index
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

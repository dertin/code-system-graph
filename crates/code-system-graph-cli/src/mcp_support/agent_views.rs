//! Semantic, machine-readable views paired with agent-facing MCP Markdown.

use std::collections::{BTreeMap, BTreeSet};

use code_system_graph_core::{
    AgentNextAction, ResolvedSymbol, SearchCoverage, SearchExplanation, SearchReport
};
use code_system_graph_model::{
    EdgeKind, EpistemicStatus, EvidenceId, FreshnessSummary, Node, NodeId, NodeKind, Provenance, RepoFreshnessState, RepoId, ToolStatus, TraceReport
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub(super) use super::agent_context::AgentPresentationContext;
pub(super) use super::vocabulary::{
    epistemic_status_name, freshness_name, freshness_state_name, inverse_relationship_phrase, node_kind_name, relationship_phrase
};
use super::{GraphStatusReport, SourceContextReport};
use crate::repository_ownership::RepositoryOwnership;
use crate::{
    ExploreCoverage, ExploreFederatedHandoff, ExploreLocalRelationship, ExploreReport, ExploreRepositoryContext
};

pub(super) const AGENT_DELIVERY_SCHEMA_VERSION: u32 = 5;
const QUERY_RELATION_PREVIEW_LIMIT: usize = 2;
const REPOSITORY_RELATION_PREVIEW_LIMIT: usize = 8;
const QUERY_ARCHITECTURE_RELATION_LIMIT: usize = 8;
const SOURCE_CONTEXT_PROJECTION_LIMIT: usize = 32;
pub(super) const PROJECTED_EVIDENCE_PER_RELATION_LIMIT: usize = 8;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(super) struct AgentStructuredEnvelope<T> {
    pub schema_version: u32,
    pub tool: String,
    pub status: ToolStatus,
    pub data: Option<T>,
    pub freshness: FreshnessSummary,
    pub warnings: Vec<String>,
}

impl<T> AgentStructuredEnvelope<T> {
    pub(super) fn new(
        tool: &str,
        status: ToolStatus,
        data: Option<T>,
        freshness: FreshnessSummary,
        warnings: Vec<String>,
    ) -> Self {
        Self {
            schema_version: AGENT_DELIVERY_SCHEMA_VERSION,
            tool: tool.to_owned(),
            status,
            data,
            freshness,
            warnings,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentRelationScope {
    Local,
    CrossRepository,
    WorkspaceScoped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentRelationDirection {
    Outgoing,
    Incoming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentRepositoryAttribution {
    Direct,
    StructuralOwner,
    Ambiguous,
    Unresolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentRelationDerivation {
    ObservedEdge,
    EventDeliveryPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub(super) struct AgentEntityView {
    pub node_id: String,
    pub stable_key: String,
    pub label: String,
    pub kind: NodeKind,
    pub repository_id: Option<String>,
    pub repository_alias: Option<String>,
    pub repository_attribution: AgentRepositoryAttribution,
    pub repository_candidates: Vec<String>,
    pub repository_candidate_count: usize,
    pub repository_candidates_truncated: bool,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub(super) enum AgentEvidenceRole {
    ObservedRelation,
    StructuralAttribution,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(super) struct AgentEvidenceView {
    pub evidence_id: String,
    pub role: AgentEvidenceRole,
    pub repository_alias: Option<String>,
    pub path: Option<String>,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub explanation: Option<String>,
    pub provenance: Provenance,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(super) struct AgentRelationView {
    pub edge_id: String,
    pub edge_kind: EdgeKind,
    pub source: AgentEntityView,
    pub relationship: String,
    pub inverse_relationship: String,
    pub target: AgentEntityView,
    pub scope: AgentRelationScope,
    pub direction: AgentRelationDirection,
    pub derivation: AgentRelationDerivation,
    pub status: EpistemicStatus,
    pub confidence: f32,
    pub evidence: Vec<AgentEvidenceView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(super) struct AgentQueryHitView {
    pub entity: AgentEntityView,
    pub alternate_node_ids: Vec<String>,
    pub roles: Vec<NodeKind>,
    pub matched_because: String,
    pub score: f64,
    pub relation_previews: Vec<AgentRelationView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(super) struct AgentQueryReport {
    pub results: Vec<AgentQueryHitView>,
    pub cross_repository_relations: Vec<AgentRelationView>,
    pub total_matches: usize,
    pub raw_returned_matches: usize,
    pub collapsed_duplicate_matches: usize,
    pub subordinate_matches_omitted: usize,
    pub offset: usize,
    pub limit: usize,
    pub truncated: bool,
    pub coverage: SearchCoverage,
    pub next_actions: Vec<AgentNextAction>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(super) struct AgentSourceContextReport {
    pub workspace: String,
    pub entity: AgentEntityView,
    pub outgoing_relations: Vec<AgentRelationView>,
    pub incoming_relations: Vec<AgentRelationView>,
    pub projected_relations: Vec<AgentRelationView>,
    pub total_relations: usize,
    pub direct_relations: usize,
    pub structural_relations_omitted: usize,
    pub relations_truncated: bool,
    pub total_evidence: usize,
    pub evidence_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(super) struct AgentTraceReport {
    pub segments: Vec<AgentRelationView>,
    pub cross_repository_hops: usize,
    pub truncated: bool,
    pub coverage_gaps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub(super) struct AgentExploreExecutionSummary {
    pub provider_operations: usize,
    pub maximum_concurrency_observed: usize,
    pub retained_bytes: usize,
    pub degradations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub(super) struct AgentExploreReport {
    pub repository: ExploreRepositoryContext,
    pub resolved_symbols: Vec<ResolvedSymbol>,
    pub local_relationships: Vec<ExploreLocalRelationship>,
    pub federated_handoffs: Vec<ExploreFederatedHandoff>,
    pub coverage: ExploreCoverage,
    pub next_actions: Vec<AgentNextAction>,
    pub execution: AgentExploreExecutionSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub(super) struct AgentRepositoryFreshness {
    pub repository_id: String,
    pub repository_alias: Option<String>,
    pub state: RepoFreshnessState,
    pub reason: Option<String>,
    pub head_commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub(super) struct AgentStatusReport {
    pub workspace: String,
    pub database_schema: i64,
    pub integrity_ok: bool,
    pub snapshot_id: String,
    pub node_count: usize,
    pub edge_count: usize,
    pub evidence_count: usize,
    pub structural_relationship_count: usize,
    pub semantic_relationship_count: usize,
    pub cross_repository_relationship_count: usize,
    pub retained_cross_repository_event_integration_count: usize,
    pub event_delivery_pair_count: usize,
    pub event_delivery_projection_truncated: bool,
    pub workspace_scoped_relationship_count: usize,
    pub repositories: Vec<AgentRepositoryFreshness>,
}

#[expect(
    clippy::too_many_lines,
    reason = "The query projection performs grouping, subordinate suppression, deduplication, ranking, and delivery bounds in one deterministic pipeline"
)]
pub(super) fn query_view(
    report: &SearchReport,
    context: &AgentPresentationContext,
) -> AgentQueryReport {
    let mut groups = Vec::<(String, Vec<&code_system_graph_core::SearchHit>)>::new();
    for hit in &report.hits {
        let entity = context.entity(&hit.node);
        let identity = presentation_identity(&entity);
        if let Some((_, hits)) = groups.iter_mut().find(|(key, _)| key == &identity) {
            hits.push(hit);
        } else {
            groups.push((identity, vec![hit]));
        }
    }
    let mut results = groups
        .into_iter()
        .map(|(_, hits)| {
            let representative = hits
                .iter()
                .copied()
                .min_by_key(|hit| query_entity_priority(hit.node.kind))
                .expect("query group is non-empty");
            let mut roles = hits.iter().map(|hit| hit.node.kind).collect::<Vec<_>>();
            roles.sort_by_key(|kind| query_entity_priority(*kind));
            roles.dedup();
            let focus_nodes = hits
                .iter()
                .map(|hit| hit.node.id.clone())
                .collect::<Vec<_>>();
            let relation_limit = if representative.node.kind == NodeKind::Repository {
                REPOSITORY_RELATION_PREVIEW_LIMIT
            } else {
                QUERY_RELATION_PREVIEW_LIMIT
            };
            AgentQueryHitView {
                entity: context.entity(&representative.node),
                alternate_node_ids: hits
                    .iter()
                    .filter(|hit| hit.node.id != representative.node.id)
                    .map(|hit| hit.node.id.as_str().to_owned())
                    .collect(),
                roles,
                matched_because: match_explanation(&hits[0].explanation),
                score: hits[0].score,
                relation_previews: context.semantic_relations_for(&focus_nodes, relation_limit),
            }
        })
        .collect::<Vec<_>>();
    let collapsed_duplicate_matches = report.hits.len().saturating_sub(results.len());
    let valuable_nodes = results
        .iter()
        .filter(|result| !result.relation_previews.is_empty())
        .flat_map(|result| {
            std::iter::once(NodeId::new(&result.entity.node_id))
                .chain(result.alternate_node_ids.iter().map(NodeId::new))
        })
        .collect::<BTreeSet<_>>();
    let before_subordinate_filter = results.len();
    if !valuable_nodes.is_empty() {
        results.retain(|result| {
            if !result.relation_previews.is_empty() {
                return true;
            }
            let node_ids = std::iter::once(NodeId::new(&result.entity.node_id))
                .chain(result.alternate_node_ids.iter().map(NodeId::new))
                .collect::<Vec<_>>();
            !context.structurally_adjacent_to_any(&node_ids, &valuable_nodes)
        });
    }
    let subordinate_matches_omitted = before_subordinate_filter.saturating_sub(results.len());
    let mut cross_repository_relations = results
        .iter()
        .flat_map(|result| &result.relation_previews)
        .filter(|relation| relation.scope == AgentRelationScope::CrossRepository)
        .cloned()
        .map(|relation| (relation.edge_id.clone(), relation))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect::<Vec<_>>();
    cross_repository_relations.sort_by(|left, right| {
        relation_priority(left)
            .cmp(&relation_priority(right))
            .then(left.source.label.cmp(&right.source.label))
            .then(left.target.label.cmp(&right.target.label))
            .then(left.edge_id.cmp(&right.edge_id))
    });
    cross_repository_relations.dedup_by(|left, right| left.edge_id == right.edge_id);
    remove_redundant_package_relations(&mut cross_repository_relations);
    cross_repository_relations.truncate(QUERY_ARCHITECTURE_RELATION_LIMIT);
    let include_explore = results
        .first()
        .is_none_or(|result| result.entity.kind != NodeKind::Repository);
    AgentQueryReport {
        results,
        cross_repository_relations,
        total_matches: report.total_matches,
        raw_returned_matches: report.hits.len(),
        collapsed_duplicate_matches,
        subordinate_matches_omitted,
        offset: report.offset,
        limit: report.limit,
        truncated: report.truncated,
        coverage: report.coverage.clone(),
        next_actions: report
            .next_actions
            .iter()
            .filter(|action| include_explore || action.tool != "explore")
            .take(2)
            .cloned()
            .collect(),
    }
}

pub(super) fn explore_view(report: &ExploreReport) -> AgentExploreReport {
    AgentExploreReport {
        repository: report.repository.clone(),
        resolved_symbols: report.resolved_symbols.clone(),
        local_relationships: report.local_relationships.clone(),
        federated_handoffs: report.federated_handoffs.clone(),
        coverage: report.coverage.clone(),
        next_actions: report.next_actions.iter().take(2).cloned().collect(),
        execution: AgentExploreExecutionSummary {
            provider_operations: report.execution.provider_operations,
            maximum_concurrency_observed: report.execution.maximum_concurrency_observed,
            retained_bytes: report.execution.retained_bytes,
            degradations: report.execution.degradations.clone(),
        },
    }
}

pub(super) fn source_context_view(
    report: &SourceContextReport,
    context: &AgentPresentationContext,
) -> AgentSourceContextReport {
    let selected_evidence = report
        .evidence
        .iter()
        .map(|evidence| evidence.id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let selected_evidence_ids = selected_evidence
        .iter()
        .map(|evidence| evidence.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    let mut relations = report
        .relations
        .iter()
        .map(|relation| {
            context.relation_for_parts(
                &relation.edge,
                &relation.source,
                &relation.target,
                Some(&report.entity.id),
            )
        })
        .collect::<Vec<_>>();
    for relation in &mut relations {
        relation
            .evidence
            .retain(|evidence| selected_evidence.contains(&EvidenceId::new(&evidence.evidence_id)));
    }
    let direct_edge_ids = relations
        .iter()
        .map(|relation| relation.edge_id.clone())
        .collect::<BTreeSet<_>>();
    let mut projected_relations = context.semantic_relations_for(
        std::slice::from_ref(&report.entity.id),
        SOURCE_CONTEXT_PROJECTION_LIMIT,
    );
    projected_relations.retain(|relation| !direct_edge_ids.contains(&relation.edge_id));
    for relation in &mut projected_relations {
        relation
            .evidence
            .truncate(PROJECTED_EVIDENCE_PER_RELATION_LIMIT);
    }
    let projected_evidence = projected_relations
        .iter()
        .flat_map(|relation| relation.evidence.iter())
        .map(|evidence| evidence.evidence_id.clone())
        .collect::<BTreeSet<_>>();
    let total_relations = report.total_relations + projected_relations.len();
    let total_evidence = report.total_evidence
        + projected_evidence
            .difference(&selected_evidence_ids)
            .count();
    AgentSourceContextReport {
        workspace: report.workspace.clone(),
        entity: context.entity(&report.entity),
        outgoing_relations: relations
            .iter()
            .filter(|relation| relation.direction == AgentRelationDirection::Outgoing)
            .cloned()
            .collect(),
        incoming_relations: relations
            .into_iter()
            .filter(|relation| relation.direction == AgentRelationDirection::Incoming)
            .collect(),
        projected_relations,
        total_relations,
        direct_relations: report.total_relations,
        structural_relations_omitted: report.structural_relations_omitted,
        relations_truncated: report.relations_truncated,
        total_evidence,
        evidence_truncated: report.evidence_truncated,
    }
}

pub(super) fn trace_view(
    report: &TraceReport,
    context: &AgentPresentationContext,
) -> AgentTraceReport {
    let segments = report
        .segments
        .iter()
        .filter_map(|segment| context.relation_for_edge(&segment.edge, Some(&segment.source.id)))
        .collect::<Vec<_>>();
    let cross_repository_hops = segments
        .iter()
        .filter(|segment| segment.scope == AgentRelationScope::CrossRepository)
        .count();
    AgentTraceReport {
        segments,
        cross_repository_hops,
        truncated: report.truncated,
        coverage_gaps: report.coverage_gaps.clone(),
    }
}

pub(super) fn status_view(
    report: &GraphStatusReport,
    context: &AgentPresentationContext,
) -> AgentStatusReport {
    let structural_relationship_count = context
        .edges
        .iter()
        .filter(|edge| is_structural_edge(edge.kind))
        .count();
    let cross_repository_relationship_count = context
        .edges
        .iter()
        .filter(|edge| !is_structural_edge(edge.kind))
        .filter(|edge| {
            let source = unique_owner(&context.ownership, &edge.source);
            let target = unique_owner(&context.ownership, &edge.target);
            matches!((source, target), (Some(left), Some(right)) if left != right)
        })
        .count();
    let workspace_scoped_relationship_count = context
        .edges
        .iter()
        .filter(|edge| !is_structural_edge(edge.kind))
        .filter(|edge| {
            unique_owner(&context.ownership, &edge.source).is_none()
                || unique_owner(&context.ownership, &edge.target).is_none()
        })
        .count();
    AgentStatusReport {
        workspace: report.workspace.clone(),
        database_schema: report.schema_version,
        integrity_ok: report.integrity_ok,
        snapshot_id: report.snapshot.snapshot_id.clone(),
        node_count: report.snapshot.node_count,
        edge_count: report.snapshot.edge_count,
        evidence_count: report.snapshot.evidence_count,
        structural_relationship_count,
        semantic_relationship_count: report
            .snapshot
            .edge_count
            .saturating_sub(structural_relationship_count),
        cross_repository_relationship_count,
        retained_cross_repository_event_integration_count: context
            .retained_cross_repository_event_integration_count(),
        event_delivery_pair_count: context.event_delivery_total,
        event_delivery_projection_truncated: context.event_delivery_truncated,
        workspace_scoped_relationship_count,
        repositories: report
            .repositories
            .iter()
            .map(|repository| AgentRepositoryFreshness {
                repository_id: repository.repo_id.as_str().to_owned(),
                repository_alias: context.alias_for_freshness(repository),
                state: repository.state,
                reason: repository.reason.clone(),
                head_commit: repository.head_commit.clone(),
            })
            .collect(),
    }
}

pub(super) fn unique_owner<'a>(
    ownership: &'a BTreeMap<NodeId, RepositoryOwnership>,
    node_id: &NodeId,
) -> Option<&'a RepoId> {
    ownership.get(node_id)?.unique_repository()
}

pub(super) fn agent_entity_label(node: &Node) -> String {
    match node.kind {
        NodeKind::DatabaseTable => {
            qualified_data_label(&node.stable_key, "table:").unwrap_or_else(|| node.label.clone())
        }
        NodeKind::DatabaseColumn => qualified_data_label(&node.stable_key, "column:table:")
            .unwrap_or_else(|| node.label.clone()),
        _ => node.label.clone(),
    }
}

fn qualified_data_label(stable_key: &str, prefix: &str) -> Option<String> {
    let parts = stable_key
        .strip_prefix(prefix)?
        .split(':')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("."))
}

pub(super) fn remove_redundant_package_relations(relations: &mut Vec<AgentRelationView>) {
    let repository_dependencies = relations
        .iter()
        .filter(|relation| relation.edge_kind == EdgeKind::DependsOnRepository)
        .filter_map(|relation| {
            Some((
                relation.source.repository_id.clone()?,
                relation.target.repository_id.clone()?,
            ))
        })
        .collect::<BTreeSet<_>>();
    relations.retain(|relation| {
        if relation.edge_kind != EdgeKind::DependsOnPackage {
            return true;
        }
        let Some(source) = &relation.source.repository_id else {
            return true;
        };
        let Some(target) = &relation.target.repository_id else {
            return true;
        };
        !repository_dependencies.contains(&(source.clone(), target.clone()))
    });
}

pub(super) fn relation_priority(relation: &AgentRelationView) -> (u8, u8, u8, u8) {
    (
        match relation.scope {
            AgentRelationScope::CrossRepository => 0,
            AgentRelationScope::WorkspaceScoped => 1,
            AgentRelationScope::Local => 2,
        },
        match relation.derivation {
            AgentRelationDerivation::EventDeliveryPath => 0,
            AgentRelationDerivation::ObservedEdge => 1,
        },
        u8::from(relation.direction != AgentRelationDirection::Outgoing),
        match relation.edge_kind {
            EdgeKind::CallsRemote
            | EdgeKind::DependsOnRepository
            | EdgeKind::ReadsTable
            | EdgeKind::WritesTable
            | EdgeKind::Publishes
            | EdgeKind::Subscribes
            | EdgeKind::DeliversTo => 0,
            _ => 1,
        },
    )
}

pub(super) fn is_structural_edge(kind: EdgeKind) -> bool {
    matches!(kind, EdgeKind::Contains | EdgeKind::MemberOf)
}

pub(super) fn relation_is_visually_distinct(relation: &AgentRelationView) -> bool {
    relation.source.repository_id != relation.target.repository_id
        || relation.source.label != relation.target.label
        || relation.source.path != relation.target.path
}

fn presentation_identity(entity: &AgentEntityView) -> String {
    if matches!(
        entity.kind,
        NodeKind::Artifact | NodeKind::Document | NodeKind::Adr
    ) && let Some(path) = &entity.path
    {
        return format!(
            "file:{}:{path}",
            entity.repository_id.as_deref().unwrap_or("workspace")
        );
    }
    entity.node_id.clone()
}

const fn query_entity_priority(kind: NodeKind) -> u8 {
    match kind {
        NodeKind::Document => 1,
        NodeKind::Artifact => 2,
        _ => 0,
    }
}

pub(super) fn entity_path(node: &Node) -> Option<String> {
    if matches!(
        node.kind,
        NodeKind::Artifact | NodeKind::Document | NodeKind::Adr
    ) || matches!(node.kind, NodeKind::SymbolRef | NodeKind::TestCase)
        && (node.label.contains('/') || node.label.contains('\\'))
    {
        Some(node.label.clone())
    } else {
        None
    }
}

fn match_explanation(explanation: &SearchExplanation) -> String {
    if !explanation.matched_fields.is_empty() {
        let reasons = explanation
            .matched_fields
            .iter()
            .map(|field| match field.as_str() {
                "label_exact" => "its label exactly equals the search terms".to_owned(),
                "stable_key_exact" => "its stable key exactly equals the search terms".to_owned(),
                "normalized_prefix" => "its name begins with the search terms".to_owned(),
                "normalized_suffix" => "its name ends with the search terms".to_owned(),
                "node_kind" => "its type matches the requested entity type".to_owned(),
                "label" => "its label matches the search terms".to_owned(),
                "stable key" => "its stable key matches the search terms".to_owned(),
                other => format!(
                    "its {} metadata matches the search terms",
                    other.replace(['_', '-'], " ")
                ),
            })
            .collect::<Vec<_>>();
        return format!("matched because {}", join_reasons(&reasons));
    }
    if explanation.exact_score > 0.0 {
        "matched the entity name exactly".to_owned()
    } else if explanation.prefix_score > 0.0 || explanation.suffix_score > 0.0 {
        "matched part of the entity name".to_owned()
    } else if explanation.fts_score > 0.0 {
        "matched indexed architecture metadata".to_owned()
    } else if explanation.type_score > 0.0 {
        "matched the requested entity type".to_owned()
    } else {
        "matched graph ranking signals".to_owned()
    }
}

fn join_reasons(reasons: &[String]) -> String {
    match reasons {
        [] => String::new(),
        [only] => only.clone(),
        [left, right] => format!("{left} and {right}"),
        _ => {
            let (last, leading) = reasons.split_last().expect("non-empty reasons");
            format!("{}, and {last}", leading.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use code_system_graph_core::SearchExplanation;
    use code_system_graph_model::{
        Edge, EdgeId, EdgeKind, EpistemicStatus, Node, NodeId, NodeKind, RepoId
    };

    use super::{
        AgentPresentationContext, AgentRelationScope, REPOSITORY_RELATION_PREVIEW_LIMIT, inverse_relationship_phrase, match_explanation, relationship_phrase
    };

    #[test]
    fn every_edge_kind_has_forward_and_inverse_language() {
        let kinds = [
            EdgeKind::Contains,
            EdgeKind::Provides,
            EdgeKind::Consumes,
            EdgeKind::CallsRemote,
            EdgeKind::Publishes,
            EdgeKind::Subscribes,
            EdgeKind::DeliversTo,
            EdgeKind::DependsOnPackage,
            EdgeKind::DependsOnRepository,
            EdgeKind::ReadsTable,
            EdgeKind::WritesTable,
            EdgeKind::Deploys,
            EdgeKind::Configures,
            EdgeKind::Documents,
            EdgeKind::OwnedBy,
            EdgeKind::ImplementedBy,
            EdgeKind::Validates,
            EdgeKind::ChangedIn,
            EdgeKind::Affects,
            EdgeKind::Precedes,
            EdgeKind::Reverts,
            EdgeKind::CompatibleWith,
            EdgeKind::IncompatibleWith,
            EdgeKind::MemberOf,
            EdgeKind::ManualLink,
        ];
        for kind in kinds {
            assert_ne!(relationship_phrase(kind), "");
            assert_ne!(inverse_relationship_phrase(kind), "");
        }
    }

    #[test]
    fn search_fields_are_explained_as_human_reasons() {
        let explanation = SearchExplanation {
            matched_fields: vec!["label_exact".to_owned(), "node_kind".to_owned()],
            exact_score: 4.0,
            prefix_score: 0.0,
            suffix_score: 0.0,
            fts_score: 0.0,
            type_score: 1.0,
            scope_score: 0.0,
            centrality_score: 0.0,
            community_score: 0.0,
            evidence_score: 0.0,
            freshness_penalty: 0.0,
        };

        let rendered = match_explanation(&explanation);

        assert_eq!(
            rendered,
            "matched because its label exactly equals the search terms and its type matches the requested entity type"
        );
        assert!(!rendered.contains("label_exact"));
        assert!(!rendered.contains("node_kind"));
    }

    #[test]
    fn indexed_repository_projection_stays_bounded_with_thousands_of_edges() {
        let repository = Node {
            id: NodeId::new("node:repository"),
            kind: NodeKind::Repository,
            repo_id: Some(RepoId::new("repo:source")),
            stable_key: "repository:repo:source".to_owned(),
            label: "repo:source".to_owned(),
        };
        let remote_repository = Node {
            id: NodeId::new("node:remote-repository"),
            kind: NodeKind::Repository,
            repo_id: Some(RepoId::new("repo:remote")),
            stable_key: "repository:repo:remote".to_owned(),
            label: "repo:remote".to_owned(),
        };
        let mut nodes = vec![repository.clone(), remote_repository];
        let mut edges = Vec::new();
        for index in 0..2_000 {
            let source = Node {
                id: NodeId::new(format!("node:source:{index}")),
                kind: NodeKind::HttpOperation,
                repo_id: Some(RepoId::new("repo:source")),
                stable_key: format!("http:source:consumer:GET:/items/{index}"),
                label: format!("GET /items/{index}"),
            };
            let target = Node {
                id: NodeId::new(format!("node:target:{index}")),
                kind: NodeKind::HttpOperation,
                repo_id: Some(RepoId::new("repo:remote")),
                stable_key: format!("http:remote:provider:GET:/items/{index}"),
                label: format!("GET /items/{index}"),
            };
            edges.push(Edge {
                id: EdgeId::new(format!("edge:{index}")),
                source: source.id.clone(),
                target: target.id.clone(),
                kind: EdgeKind::CallsRemote,
                confidence: 1.0,
                status: EpistemicStatus::Confirmed,
                evidence: Vec::new(),
            });
            nodes.extend([source, target]);
        }
        let context = AgentPresentationContext::fixture(
            vec![
                (RepoId::new("repo:source"), "source".to_owned()),
                (RepoId::new("repo:remote"), "remote".to_owned()),
            ],
            nodes,
            edges,
            Vec::new(),
        );

        assert_eq!(
            context
                .semantic_edges_by_repository
                .get(&RepoId::new("repo:source"))
                .map(Vec::len),
            Some(2_000)
        );
        assert_eq!(context.edge_by_id.len(), 2_000);
        assert_eq!(
            context
                .relation_for_edge_id("edge:1999")
                .map(|relation| relation.edge_id),
            Some("edge:1999".to_owned())
        );
        let projected = context.semantic_relations_for(
            std::slice::from_ref(&repository.id),
            REPOSITORY_RELATION_PREVIEW_LIMIT,
        );
        assert_eq!(projected.len(), REPOSITORY_RELATION_PREVIEW_LIMIT);
        assert!(
            projected
                .iter()
                .all(|relation| relation.scope == AgentRelationScope::CrossRepository)
        );
    }

    #[test]
    fn ambiguous_entity_reports_total_and_truncated_repository_candidates() {
        let child = Node {
            id: NodeId::new("node:shared"),
            kind: NodeKind::DatabaseTable,
            repo_id: None,
            stable_key: "table:public:shared".to_owned(),
            label: "shared".to_owned(),
        };
        let repositories = (0..6)
            .map(|index| Node {
                id: NodeId::new(format!("node:repo:{index}")),
                kind: NodeKind::Repository,
                repo_id: Some(RepoId::new(format!("repo:{index}"))),
                stable_key: format!("repository:repo:{index}"),
                label: format!("repo:{index}"),
            })
            .collect::<Vec<_>>();
        let artifacts = (0..6)
            .map(|index| Node {
                id: NodeId::new(format!("node:artifact:{index}")),
                kind: NodeKind::Artifact,
                repo_id: Some(RepoId::new(format!("repo:{index}"))),
                stable_key: format!("artifact:repo:{index}"),
                label: format!("artifact:{index}"),
            })
            .collect::<Vec<_>>();
        let edges = artifacts
            .iter()
            .enumerate()
            .map(|(index, artifact)| Edge {
                id: EdgeId::new(format!("edge:contains:{index}")),
                source: artifact.id.clone(),
                target: child.id.clone(),
                kind: EdgeKind::Contains,
                confidence: 1.0,
                status: EpistemicStatus::Confirmed,
                evidence: Vec::new(),
            })
            .collect::<Vec<_>>();
        let aliases = (0..6)
            .map(|index| {
                (
                    RepoId::new(format!("repo:{index}")),
                    format!("repo-{index}"),
                )
            })
            .collect();
        let mut nodes = repositories;
        nodes.extend(artifacts);
        nodes.push(child.clone());

        let context = AgentPresentationContext::fixture(aliases, nodes, edges, Vec::new());
        let entity = context.entity(&child);

        assert_eq!(entity.repository_candidate_count, 6);
        assert_eq!(entity.repository_candidates.len(), 4);
        assert!(entity.repository_candidates_truncated);
    }
}

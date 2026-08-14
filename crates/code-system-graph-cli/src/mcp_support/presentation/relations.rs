//! Shared entity and relationship Markdown rendering.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use code_system_graph_model::{EdgeKind, NodeKind};

use super::super::agent_views::{
    AgentEntityView, AgentEvidenceRole, AgentEvidenceView, AgentRelationDerivation, AgentRelationDirection, AgentRelationScope, AgentRelationView, AgentRepositoryAttribution, epistemic_status_name, node_kind_name
};
use super::{SemanticMarkdown, code, human_join, location, plain};

pub(super) fn add_relation_section(
    document: &mut SemanticMarkdown,
    heading: &str,
    relations: &[AgentRelationView],
) {
    if relations.is_empty() {
        document.add(format!(
            "## {heading}\n\nNone observed in the current snapshot."
        ));
        return;
    }
    let items = relations
        .iter()
        .map(|relation| format!("- {}", relation_markdown(relation)))
        .collect::<Vec<_>>();
    document.add(format!("## {heading}\n\n{}", items.join("\n\n")));
}

pub(super) fn relation_markdown(relation: &AgentRelationView) -> String {
    let scope = relation_scope_markdown(relation);
    let direction = match relation.direction {
        AgentRelationDirection::Outgoing => "outgoing from the focused entity",
        AgentRelationDirection::Incoming => "incoming to the focused entity",
    };
    let mut rendered = format!(
        "{} — {scope}, {direction}, {}, confidence {:.2}.",
        relation_chain_markdown(relation),
        epistemic_status_name(relation.status),
        relation.confidence,
    );
    if relation.derivation == AgentRelationDerivation::EventDeliveryPath {
        rendered.push_str(
            " Derived from observed publisher and subscriber edges sharing one exact event channel.",
        );
    }
    if relation.evidence.is_empty() {
        rendered.push_str(" No evidence location is included for this relation.");
    } else {
        let evidence = ordered_relation_evidence(relation);
        rendered.push_str(" Evidence: ");
        rendered.push_str(
            &evidence
                .iter()
                .map(|item| {
                    location(
                        item.repository_alias
                            .as_deref()
                            .unwrap_or("unknown repository"),
                        item.path.as_deref().unwrap_or("location unavailable"),
                        item.start_line,
                        item.end_line,
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
        );
        rendered.push('.');
    }
    rendered
}

pub(super) fn relation_preview_markdown(relation: &AgentRelationView) -> String {
    let scope = relation_scope_markdown(relation);
    let mut rendered = format!(
        "{} — {scope}, {}, confidence {:.2}.",
        relation_chain_markdown(relation),
        epistemic_status_name(relation.status),
        relation.confidence,
    );
    if relation.derivation == AgentRelationDerivation::EventDeliveryPath {
        rendered.push_str(
            " Derived from observed publisher and subscriber edges sharing one exact event channel.",
        );
    }
    if relation.evidence.is_empty() {
        rendered.push_str(" No evidence location is included.");
    } else {
        let evidence = ordered_relation_evidence(relation);
        rendered.push_str(" Evidence: ");
        rendered.push_str(
            &evidence
                .iter()
                .take(relation_preview_evidence_limit(relation))
                .map(|evidence| {
                    location(
                        evidence
                            .repository_alias
                            .as_deref()
                            .unwrap_or("unknown repository"),
                        evidence.path.as_deref().unwrap_or("location unavailable"),
                        evidence.start_line,
                        evidence.end_line,
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
        );
        rendered.push('.');
    }
    rendered
}

fn relation_preview_evidence_limit(relation: &AgentRelationView) -> usize {
    if relation.derivation == AgentRelationDerivation::EventDeliveryPath {
        return 2;
    }
    if relation.edge_kind == EdgeKind::ImplementedBy {
        return 3;
    }
    if relation.source.repository_attribution == AgentRepositoryAttribution::Ambiguous
        || relation.target.repository_attribution == AgentRepositoryAttribution::Ambiguous
        || relation.scope == AgentRelationScope::CrossRepository
            && (relation.source.repository_attribution
                == AgentRepositoryAttribution::StructuralOwner
                || relation.target.repository_attribution
                    == AgentRepositoryAttribution::StructuralOwner)
    {
        return 3;
    }
    if relation.scope == AgentRelationScope::CrossRepository {
        return 2;
    }
    1
}

const fn evidence_role_priority(role: &AgentEvidenceRole) -> u8 {
    match role {
        AgentEvidenceRole::ObservedRelation => 0,
        AgentEvidenceRole::StructuralAttribution => 1,
    }
}

fn ordered_relation_evidence(relation: &AgentRelationView) -> Vec<&AgentEvidenceView> {
    let mut evidence = relation.evidence.iter().collect::<Vec<_>>();
    evidence.sort_by_key(|item| {
        (
            evidence_role_priority(&item.role),
            item.repository_alias.as_deref().unwrap_or_default(),
            item.path.as_deref().unwrap_or_default(),
            item.start_line.unwrap_or(u32::MAX),
            item.end_line.unwrap_or(u32::MAX),
        )
    });
    let mut seen_locations = BTreeSet::new();
    evidence.retain(|item| {
        seen_locations.insert((
            item.repository_alias.as_deref(),
            item.path.as_deref(),
            item.start_line,
            item.end_line,
        ))
    });
    evidence
}

fn relation_chain_markdown(relation: &AgentRelationView) -> String {
    if relation.edge_kind == EdgeKind::DependsOnPackage
        && relation.scope == AgentRelationScope::CrossRepository
        && relation.source.kind == NodeKind::Package
        && relation.target.kind == NodeKind::Package
        && let (Some(source_repository), Some(target_repository)) = (
            relation.source.repository_alias.as_deref(),
            relation.target.repository_alias.as_deref(),
        )
    {
        return format!(
            "repository {} → **depends on** → repository {} through package {}",
            code(source_repository),
            code(target_repository),
            code(entity_display_label(&relation.target)),
        );
    }
    format!(
        "{} → **{}** → {}",
        relation_endpoint(&relation.source),
        plain(&relation.relationship),
        relation_endpoint(&relation.target),
    )
}

pub(super) fn relation_is_component_of_summary(
    relation: &AgentRelationView,
    summaries: &[AgentRelationView],
) -> bool {
    if summaries
        .iter()
        .any(|summary| summary.edge_id == relation.edge_id)
    {
        return true;
    }
    if relation.derivation != AgentRelationDerivation::ObservedEdge
        || !matches!(
            relation.edge_kind,
            EdgeKind::Publishes | EdgeKind::Subscribes | EdgeKind::DeliversTo
        )
    {
        return false;
    }
    let channel = match relation.edge_kind {
        EdgeKind::Publishes | EdgeKind::Subscribes => &relation.target,
        EdgeKind::DeliversTo => &relation.source,
        _ => unreachable!("event relationship checked above"),
    };
    summaries.iter().any(|summary| {
        summary.derivation == AgentRelationDerivation::EventDeliveryPath
            && summary
                .edge_id
                .starts_with(&format!("derived:event:{}:", channel.node_id))
            && [
                relation.source.repository_id.as_deref(),
                relation.target.repository_id.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|repository| {
                summary.source.repository_id.as_deref() == Some(repository)
                    || summary.target.repository_id.as_deref() == Some(repository)
            })
    })
}

pub(super) fn relation_endpoint(entity: &AgentEntityView) -> String {
    if entity.kind == NodeKind::Repository {
        return format!("repository {}", code(entity_display_label(entity)));
    }
    match (
        entity.repository_attribution,
        entity.repository_alias.as_deref(),
    ) {
        (AgentRepositoryAttribution::Direct, Some(repository)) => format!(
            "{} {} in repository {}",
            node_kind_name(entity.kind),
            code(entity_display_label(entity)),
            code(repository)
        ),
        (AgentRepositoryAttribution::StructuralOwner, Some(repository)) => format!(
            "{} {} {} repository {}",
            node_kind_name(entity.kind),
            code(entity_display_label(entity)),
            structural_endpoint_phrase(entity.kind),
            code(repository)
        ),
        (AgentRepositoryAttribution::Ambiguous, _) => format!(
            "workspace-scoped {} {} with ambiguous repository ownership{}",
            node_kind_name(entity.kind),
            code(entity_display_label(entity)),
            repository_candidates_suffix(entity)
        ),
        _ => format!(
            "workspace-scoped {} {}",
            node_kind_name(entity.kind),
            code(entity_display_label(entity))
        ),
    }
}

pub(super) fn entity_sentence(entity: &AgentEntityView) -> String {
    let mut sentence = match (
        entity.repository_attribution,
        entity.repository_alias.as_deref(),
    ) {
        (AgentRepositoryAttribution::Direct, Some(repository)) => format!(
            "{} is recorded as type {} in repository {}.",
            code(entity_display_label(entity)),
            node_kind_name(entity.kind),
            code(repository)
        ),
        (AgentRepositoryAttribution::StructuralOwner, Some(repository)) => format!(
            "{} is recorded as type {}; confirmed containment attributes its {} to repository {}.",
            code(entity_display_label(entity)),
            node_kind_name(entity.kind),
            structural_owner_role(entity.kind),
            code(repository)
        ),
        (AgentRepositoryAttribution::Ambiguous, _) => format!(
            "{} is recorded as a workspace-scoped entity of type {}; repository ownership is ambiguous{}.",
            code(entity_display_label(entity)),
            node_kind_name(entity.kind),
            repository_candidates_suffix(entity)
        ),
        _ => format!(
            "{} is recorded as a workspace-scoped entity of type {}; no repository alias is recorded.",
            code(entity_display_label(entity)),
            node_kind_name(entity.kind)
        ),
    };
    if let Some(path) = &entity.path {
        let _ = write!(sentence, " Its recorded path is {}.", code(path));
    }
    sentence
}

pub(super) fn query_entity_summary(entity: &AgentEntityView, roles: &[NodeKind]) -> String {
    let role_names = roles
        .iter()
        .map(|kind| node_kind_name(*kind))
        .collect::<Vec<_>>();
    let role_summary = if role_names.len() > 1 {
        format!("Roles: {}", human_join(&role_names))
    } else {
        format!("Type: {}", node_kind_name(entity.kind))
    };
    let mut summary = match (
        entity.repository_attribution,
        entity.repository_alias.as_deref(),
    ) {
        (AgentRepositoryAttribution::Direct, Some(repository)) => {
            format!("{role_summary}. Repository: {}.", code(repository))
        }
        (AgentRepositoryAttribution::StructuralOwner, Some(repository)) => format!(
            "{role_summary}. {} repository: {} (from confirmed containment).",
            structural_owner_heading(entity.kind),
            code(repository)
        ),
        (AgentRepositoryAttribution::Ambiguous, _) => format!(
            "{role_summary}. Workspace-scoped with ambiguous repository ownership{}.",
            repository_candidates_suffix(entity)
        ),
        _ => format!("{role_summary}. Workspace-scoped; no repository alias is recorded."),
    };
    if let Some(path) = &entity.path {
        let _ = write!(summary, " Path: {}.", code(path));
    }
    summary
}

fn structural_endpoint_phrase(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Database | NodeKind::DatabaseTable | NodeKind::DatabaseColumn => "defined in",
        _ => "owned by",
    }
}

fn structural_owner_role(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Database | NodeKind::DatabaseTable | NodeKind::DatabaseColumn => "definition",
        _ => "ownership",
    }
}

fn structural_owner_heading(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Database | NodeKind::DatabaseTable | NodeKind::DatabaseColumn => "Definition",
        _ => "Owning",
    }
}

fn repository_candidates_suffix(entity: &AgentEntityView) -> String {
    if entity.repository_candidates.is_empty() {
        String::new()
    } else {
        let candidates = entity
            .repository_candidates
            .iter()
            .map(|candidate| code(candidate))
            .collect::<Vec<_>>()
            .join(", ");
        let remainder = entity
            .repository_candidate_count
            .saturating_sub(entity.repository_candidates.len());
        if entity.repository_candidates_truncated && remainder > 0 {
            format!(
                " ({} candidates: {candidates}, and {remainder} more)",
                entity.repository_candidate_count
            )
        } else {
            format!(
                " ({} candidates: {candidates})",
                entity.repository_candidate_count
            )
        }
    }
}

pub(super) fn entity_display_label(entity: &AgentEntityView) -> &str {
    if entity.kind == NodeKind::Repository {
        entity
            .repository_alias
            .as_deref()
            .unwrap_or("repository with unresolved alias")
    } else {
        &entity.label
    }
}

pub(super) fn relation_scope_markdown(relation: &AgentRelationView) -> &'static str {
    match relation.scope {
        AgentRelationScope::CrossRepository => "cross-repository",
        AgentRelationScope::Local => "same repository",
        AgentRelationScope::WorkspaceScoped => {
            "workspace-scoped; no cross-repository boundary is recorded"
        }
    }
}

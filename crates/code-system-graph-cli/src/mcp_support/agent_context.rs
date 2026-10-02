//! Snapshot-bound indexes used to project graph facts for agent-facing delivery.

use std::collections::{BTreeMap, BTreeSet};

use code_system_graph_core::{EventDeliveryProjection, project_event_deliveries_bounded};
use code_system_graph_model::{
    Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, EvidenceId, Node, NodeId, NodeKind, RepoFreshness, RepoId
};
use code_system_graph_store_sqlite::SqliteStore;

use super::agent_views::{
    AgentEntityView, AgentEvidenceRole, AgentEvidenceView, AgentRelationDerivation, AgentRelationDirection, AgentRelationScope, AgentRelationView, AgentRepositoryAttribution, PROJECTED_EVIDENCE_PER_RELATION_LIMIT, agent_entity_label, entity_path, inverse_relationship_phrase, is_structural_edge, relation_is_visually_distinct, relation_priority, relationship_phrase, remove_redundant_package_relations, unique_owner
};
use crate::repository_ownership::{RepositoryOwnership, resolve_repository_ownership};

const EVENT_DELIVERY_PROJECTION_LIMIT: usize = 1_024;
const STRUCTURAL_OWNER_EVIDENCE_LIMIT: usize = 2;
const REPOSITORY_CANDIDATE_PREVIEW_LIMIT: usize = 4;

#[derive(Debug, Clone, Default)]
pub(crate) struct AgentPresentationContext {
    pub(super) aliases: BTreeMap<RepoId, String>,
    pub(super) nodes: BTreeMap<NodeId, Node>,
    pub(super) edges: Vec<Edge>,
    pub(super) edge_by_id: BTreeMap<EdgeId, usize>,
    pub(super) evidence: BTreeMap<EvidenceId, Evidence>,
    pub(super) ownership: BTreeMap<NodeId, RepositoryOwnership>,
    pub(super) incident_semantic_edges: BTreeMap<NodeId, Vec<usize>>,
    pub(super) semantic_edges_by_repository: BTreeMap<RepoId, Vec<usize>>,
    pub(super) structural_neighbors: BTreeMap<NodeId, Vec<NodeId>>,
    pub(super) structural_owner_evidence: BTreeMap<NodeId, Vec<EvidenceId>>,
    pub(super) repository_nodes: BTreeMap<RepoId, NodeId>,
    pub(super) event_deliveries: Vec<EventDeliveryProjection>,
    pub(super) event_delivery_total: usize,
    pub(super) event_delivery_truncated: bool,
    pub(super) gap: Option<String>,
    pub(super) event_gap: Option<String>,
}

impl AgentPresentationContext {
    pub(crate) fn load_direct_nodes(
        database_path: &std::path::Path,
        workspace: &str,
        nodes: Vec<Node>,
    ) -> Result<Self, String> {
        let store =
            SqliteStore::open_read_only(database_path).map_err(|error| error.to_string())?;
        let registry = store
            .load_workspace_registry(workspace)
            .map_err(|error| error.to_string())?;
        Ok(Self::from_parts(
            registry
                .repositories
                .into_iter()
                .map(|repository| (repository.id, repository.alias))
                .collect(),
            nodes,
            Vec::new(),
            Vec::new(),
            None,
        ))
    }

    pub(crate) fn load_snapshot(
        database_path: &std::path::Path,
        workspace: &str,
        snapshot_id: &str,
    ) -> Result<Self, String> {
        Self::try_load_snapshot(database_path, workspace, snapshot_id)
    }

    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self {
            gap: Some(message.into()),
            ..Self::default()
        }
    }

    fn try_load_snapshot(
        database_path: &std::path::Path,
        workspace: &str,
        snapshot_id: &str,
    ) -> Result<Self, String> {
        let store =
            SqliteStore::open_read_only(database_path).map_err(|error| error.to_string())?;
        let registry = store
            .load_workspace_registry(workspace)
            .map_err(|error| error.to_string())?;
        let (nodes, edges) = store
            .load_graph_snapshot(snapshot_id)
            .map_err(|error| error.to_string())?;
        let evidence = store
            .load_evidence_snapshot(snapshot_id)
            .map_err(|error| error.to_string())?;
        Ok(Self::from_parts(
            registry
                .repositories
                .into_iter()
                .map(|repository| (repository.id, repository.alias))
                .collect(),
            nodes,
            edges,
            evidence,
            None,
        ))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "One pass builds the mutually consistent ownership, incidence, repository, and event indexes"
    )]
    fn from_parts(
        aliases: BTreeMap<RepoId, String>,
        nodes: Vec<Node>,
        edges: Vec<Edge>,
        evidence: Vec<Evidence>,
        gap: Option<String>,
    ) -> Self {
        let nodes = nodes
            .into_iter()
            .map(|node| (node.id.clone(), node))
            .collect::<BTreeMap<_, _>>();
        let ownership = resolve_repository_ownership(nodes.values(), &edges, || false)
            .expect("non-cancellable presentation ownership resolution should complete");
        let repository_nodes = nodes
            .values()
            .filter(|node| node.kind == NodeKind::Repository)
            .filter_map(|node| {
                node.repo_id
                    .as_ref()
                    .map(|repo_id| (repo_id.clone(), node.id.clone()))
            })
            .collect::<BTreeMap<_, _>>();
        let mut incident_semantic_edges = BTreeMap::<NodeId, Vec<usize>>::new();
        let mut semantic_edges_by_repository = BTreeMap::<RepoId, Vec<usize>>::new();
        let mut structural_neighbors = BTreeMap::<NodeId, Vec<NodeId>>::new();
        let mut structural_owner_evidence = BTreeMap::<NodeId, Vec<EvidenceId>>::new();
        for (index, edge) in edges.iter().enumerate() {
            if is_structural_edge(edge.kind) {
                structural_neighbors
                    .entry(edge.source.clone())
                    .or_default()
                    .push(edge.target.clone());
                structural_neighbors
                    .entry(edge.target.clone())
                    .or_default()
                    .push(edge.source.clone());
                if edge.kind == EdgeKind::Contains
                    && edge.status == EpistemicStatus::Confirmed
                    && nodes
                        .get(&edge.target)
                        .is_some_and(|node| node.repo_id.is_none())
                {
                    structural_owner_evidence
                        .entry(edge.target.clone())
                        .or_default()
                        .extend(edge.evidence.iter().cloned());
                }
                continue;
            }
            incident_semantic_edges
                .entry(edge.source.clone())
                .or_default()
                .push(index);
            incident_semantic_edges
                .entry(edge.target.clone())
                .or_default()
                .push(index);
            let repositories = [edge.source.clone(), edge.target.clone()]
                .into_iter()
                .filter_map(|node_id| unique_owner(&ownership, &node_id).cloned())
                .collect::<BTreeSet<_>>();
            for repository in repositories {
                semantic_edges_by_repository
                    .entry(repository)
                    .or_default()
                    .push(index);
            }
        }
        for indices in semantic_edges_by_repository.values_mut() {
            indices.sort_unstable();
            indices.dedup();
        }
        for neighbors in structural_neighbors.values_mut() {
            neighbors.sort();
            neighbors.dedup();
        }
        for evidence_ids in structural_owner_evidence.values_mut() {
            evidence_ids.sort();
            evidence_ids.dedup();
            evidence_ids.truncate(STRUCTURAL_OWNER_EVIDENCE_LIMIT);
        }
        let edge_by_id = edges
            .iter()
            .enumerate()
            .map(|(index, edge)| (edge.id.clone(), index))
            .collect();
        let unique_owners = ownership
            .iter()
            .filter_map(|(node_id, ownership)| {
                ownership
                    .unique_repository()
                    .cloned()
                    .map(|repository| (node_id.clone(), repository))
            })
            .collect::<BTreeMap<_, _>>();
        let event_report = project_event_deliveries_bounded(
            nodes.values(),
            &edges,
            &unique_owners,
            EVENT_DELIVERY_PROJECTION_LIMIT,
            || false,
        );
        let event_gap = if event_report.truncated {
            let event_gap = format!(
                "Event-delivery projections were bounded at {} of {} repository/channel pairs.",
                event_report.deliveries.len(),
                event_report.total
            );
            Some(event_gap)
        } else {
            None
        };
        let event_gap = if event_report
            .deliveries
            .iter()
            .any(|delivery| delivery.evidence_truncated)
        {
            let evidence_gap =
                "Supporting evidence for at least one event delivery was bounded.".to_owned();
            Some(match event_gap {
                Some(existing) => format!("{existing} {evidence_gap}"),
                None => evidence_gap,
            })
        } else {
            event_gap
        };
        let event_gap = if event_report
            .deliveries
            .iter()
            .any(|delivery| !delivery.namespace_known)
        {
            let namespace_gap = "At least one event delivery shares only a broker/topic name; without a namespace or cluster identity it remains ambiguous.".to_owned();
            Some(match event_gap {
                Some(existing) => format!("{existing} {namespace_gap}"),
                None => namespace_gap,
            })
        } else {
            event_gap
        };
        Self {
            aliases,
            nodes,
            edges,
            edge_by_id,
            evidence: evidence
                .into_iter()
                .map(|item| (item.id.clone(), item))
                .collect(),
            ownership,
            incident_semantic_edges,
            semantic_edges_by_repository,
            structural_neighbors,
            structural_owner_evidence,
            repository_nodes,
            event_deliveries: event_report.deliveries,
            event_delivery_total: event_report.total,
            event_delivery_truncated: event_report.truncated,
            gap,
            event_gap,
        }
    }

    pub(super) fn gap(&self) -> Option<&str> {
        self.gap.as_deref()
    }

    pub(super) fn scoped_gap(&self, include_events: bool) -> Option<String> {
        match (
            self.gap(),
            include_events
                .then_some(self.event_gap.as_deref())
                .flatten(),
        ) {
            (Some(global), Some(event)) => Some(format!("{global} {event}")),
            (Some(global), None) => Some(global.to_owned()),
            (None, Some(event)) => Some(event.to_owned()),
            (None, None) => None,
        }
    }

    pub(super) fn entity(&self, node: &Node) -> AgentEntityView {
        let ownership = self.ownership.get(&node.id);
        let unique_repository = ownership.and_then(RepositoryOwnership::unique_repository);
        let repository_candidate_count = ownership
            .filter(|ownership| ownership.repositories().len() > 1)
            .map_or(0, |ownership| ownership.repositories().len());
        let repository_attribution = match ownership {
            Some(ownership) if ownership.is_direct() && unique_repository.is_some() => {
                AgentRepositoryAttribution::Direct
            }
            Some(_) if unique_repository.is_some() => AgentRepositoryAttribution::StructuralOwner,
            Some(ownership) if ownership.repositories().len() > 1 => {
                AgentRepositoryAttribution::Ambiguous
            }
            _ => AgentRepositoryAttribution::Unresolved,
        };
        AgentEntityView {
            node_id: node.id.as_str().to_owned(),
            stable_key: node.stable_key.clone(),
            label: agent_entity_label(node),
            kind: node.kind,
            repository_id: unique_repository.map(|repo_id| repo_id.as_str().to_owned()),
            repository_alias: unique_repository
                .and_then(|repo_id| self.aliases.get(repo_id).cloned()),
            repository_attribution,
            repository_candidates: ownership
                .into_iter()
                .filter(|ownership| ownership.repositories().len() > 1)
                .flat_map(RepositoryOwnership::repositories)
                .take(REPOSITORY_CANDIDATE_PREVIEW_LIMIT)
                .map(|repo_id| {
                    self.aliases
                        .get(repo_id)
                        .cloned()
                        .unwrap_or_else(|| repo_id.as_str().to_owned())
                })
                .collect(),
            repository_candidate_count,
            repository_candidates_truncated: repository_candidate_count
                > REPOSITORY_CANDIDATE_PREVIEW_LIMIT,
            path: entity_path(node),
        }
    }

    pub(super) fn alias_for_freshness(&self, item: &RepoFreshness) -> Option<String> {
        self.aliases.get(&item.repo_id).cloned()
    }

    pub(super) fn relation_for_edge(
        &self,
        edge: &Edge,
        focus: Option<&NodeId>,
    ) -> Option<AgentRelationView> {
        let source = self.nodes.get(&edge.source)?;
        let target = self.nodes.get(&edge.target)?;
        Some(self.relation_for_parts(edge, source, target, focus))
    }

    pub(super) fn relation_for_edge_id(&self, edge_id: &str) -> Option<AgentRelationView> {
        self.edge_by_id
            .get(&EdgeId::new(edge_id))
            .and_then(|index| self.edges.get(*index))
            .and_then(|edge| self.relation_for_edge(edge, None))
    }

    pub(super) fn relation_for_parts(
        &self,
        edge: &Edge,
        source: &Node,
        target: &Node,
        focus: Option<&NodeId>,
    ) -> AgentRelationView {
        let source_repository = unique_owner(&self.ownership, &source.id);
        let target_repository = unique_owner(&self.ownership, &target.id);
        let focus_repository = focus
            .and_then(|node_id| self.nodes.get(node_id))
            .filter(|node| node.kind == NodeKind::Repository)
            .and_then(|node| unique_owner(&self.ownership, &node.id));
        let direction = if focus.is_some_and(|node_id| node_id == &edge.target)
            || focus_repository.is_some_and(|repository| {
                target_repository == Some(repository) && source_repository != Some(repository)
            }) {
            AgentRelationDirection::Incoming
        } else {
            AgentRelationDirection::Outgoing
        };
        let scope = match (source_repository, target_repository) {
            (Some(left), Some(right)) if left != right => AgentRelationScope::CrossRepository,
            (Some(_), Some(_)) => AgentRelationScope::Local,
            _ => AgentRelationScope::WorkspaceScoped,
        };
        let mut seen_evidence = BTreeSet::new();
        let mut evidence = edge
            .evidence
            .iter()
            .filter(|evidence_id| seen_evidence.insert((*evidence_id).clone()))
            .filter_map(|evidence_id| self.evidence.get(evidence_id))
            .map(|item| self.evidence_view(item, AgentEvidenceRole::ObservedRelation))
            .collect::<Vec<_>>();
        for node_id in [&source.id, &target.id] {
            if let Some(owner_evidence) = self.structural_owner_evidence.get(node_id) {
                evidence.extend(
                    owner_evidence
                        .iter()
                        .filter(|evidence_id| seen_evidence.insert((*evidence_id).clone()))
                        .filter_map(|evidence_id| self.evidence.get(evidence_id))
                        .map(|item| {
                            self.evidence_view(item, AgentEvidenceRole::StructuralAttribution)
                        }),
                );
            }
        }
        AgentRelationView {
            edge_id: edge.id.as_str().to_owned(),
            edge_kind: edge.kind,
            source: self.entity(source),
            relationship: relationship_phrase(edge.kind).to_owned(),
            inverse_relationship: inverse_relationship_phrase(edge.kind).to_owned(),
            target: self.entity(target),
            scope,
            direction,
            derivation: AgentRelationDerivation::ObservedEdge,
            status: edge.status,
            confidence: edge.confidence,
            evidence,
        }
    }

    pub(super) fn semantic_relations_for(
        &self,
        focus_nodes: &[NodeId],
        maximum: usize,
    ) -> Vec<AgentRelationView> {
        let mut edge_indices = BTreeSet::<usize>::new();
        for node_id in focus_nodes {
            if let Some(indices) = self.incident_semantic_edges.get(node_id) {
                edge_indices.extend(indices);
            }
            if let Some(node) = self.nodes.get(node_id)
                && node.kind == NodeKind::Repository
                && let Some(repository) = unique_owner(&self.ownership, node_id)
                && let Some(indices) = self.semantic_edges_by_repository.get(repository)
            {
                edge_indices.extend(indices);
            }
        }
        let focus = focus_nodes.first();
        let mut relations = edge_indices
            .into_iter()
            .filter_map(|index| self.edges.get(index))
            .filter_map(|edge| self.relation_for_edge(edge, focus))
            .filter(relation_is_visually_distinct)
            .collect::<Vec<_>>();
        relations.extend(self.derived_event_relations_for(focus_nodes, maximum));
        relations.sort_by(|left, right| {
            relation_priority(left)
                .cmp(&relation_priority(right))
                .then(left.source.label.cmp(&right.source.label))
                .then(left.target.label.cmp(&right.target.label))
                .then(left.edge_id.cmp(&right.edge_id))
        });
        relations.dedup_by(|left, right| left.edge_id == right.edge_id);
        remove_redundant_package_relations(&mut relations);
        relations.truncate(maximum);
        relations
    }

    fn derived_event_relations_for(
        &self,
        focus_nodes: &[NodeId],
        maximum: usize,
    ) -> Vec<AgentRelationView> {
        let mut channels = BTreeSet::new();
        let mut focus_repositories = BTreeSet::new();
        for node_id in focus_nodes {
            if self
                .nodes
                .get(node_id)
                .is_some_and(|node| node.kind == NodeKind::EventChannel)
            {
                channels.insert(node_id.clone());
            }
            if let Some(indices) = self.incident_semantic_edges.get(node_id) {
                for index in indices {
                    let Some(edge) = self.edges.get(*index) else {
                        continue;
                    };
                    if matches!(edge.kind, EdgeKind::Publishes | EdgeKind::Subscribes) {
                        channels.insert(edge.target.clone());
                    }
                }
            }
            if self
                .nodes
                .get(node_id)
                .is_some_and(|node| node.kind == NodeKind::Repository)
                && let Some(repository) = unique_owner(&self.ownership, node_id)
            {
                focus_repositories.insert(repository.clone());
            }
        }
        let mut relations = Vec::new();
        for delivery in &self.event_deliveries {
            if !channels.contains(&delivery.channel_id)
                && !focus_repositories.contains(&delivery.publisher_repository)
                && !focus_repositories.contains(&delivery.subscriber_repository)
            {
                continue;
            }
            let Some(source) = self.repository_entity(&delivery.publisher_repository) else {
                continue;
            };
            let Some(target) = self.repository_entity(&delivery.subscriber_repository) else {
                continue;
            };
            let direction = if focus_repositories.contains(&delivery.subscriber_repository)
                && !focus_repositories.contains(&delivery.publisher_repository)
            {
                AgentRelationDirection::Incoming
            } else {
                AgentRelationDirection::Outgoing
            };
            let mut evidence = delivery
                .evidence
                .iter()
                .filter_map(|evidence_id| self.evidence.get(evidence_id))
                .map(|item| self.evidence_view(item, AgentEvidenceRole::ObservedRelation))
                .collect::<Vec<_>>();
            evidence.truncate(PROJECTED_EVIDENCE_PER_RELATION_LIMIT);
            relations.push(AgentRelationView {
                edge_id: format!(
                    "derived:event:{}:{}:{}",
                    delivery.channel_id.as_str(),
                    delivery.publisher_repository.as_str(),
                    delivery.subscriber_repository.as_str()
                ),
                edge_kind: EdgeKind::DeliversTo,
                source,
                relationship: format!("publishes event {} to", delivery.channel_label),
                inverse_relationship: format!("receives event {} from", delivery.channel_label),
                target,
                scope: if delivery.publisher_repository == delivery.subscriber_repository {
                    AgentRelationScope::Local
                } else {
                    AgentRelationScope::CrossRepository
                },
                direction,
                derivation: AgentRelationDerivation::EventDeliveryPath,
                status: delivery.status,
                confidence: delivery.confidence,
                evidence,
            });
            if relations.len() >= maximum {
                break;
            }
        }
        relations.sort_by(|left, right| left.edge_id.cmp(&right.edge_id));
        relations.dedup_by(|left, right| left.edge_id == right.edge_id);
        relations
    }

    fn repository_entity(&self, repository: &RepoId) -> Option<AgentEntityView> {
        self.repository_nodes
            .get(repository)
            .and_then(|node_id| self.nodes.get(node_id))
            .map(|node| self.entity(node))
    }

    pub(super) fn structurally_adjacent_to_any(
        &self,
        node_ids: &[NodeId],
        valuable_nodes: &BTreeSet<NodeId>,
    ) -> bool {
        node_ids.iter().any(|node_id| {
            self.structural_neighbors
                .get(node_id)
                .is_some_and(|neighbors| {
                    neighbors
                        .iter()
                        .any(|neighbor| valuable_nodes.contains(neighbor))
                })
        })
    }

    pub(super) fn retained_cross_repository_event_integration_count(&self) -> usize {
        self.event_deliveries
            .iter()
            .filter(|delivery| delivery.publisher_repository != delivery.subscriber_repository)
            .count()
    }

    fn evidence_view(&self, item: &Evidence, role: AgentEvidenceRole) -> AgentEvidenceView {
        AgentEvidenceView {
            evidence_id: item.id.as_str().to_owned(),
            role,
            repository_alias: item
                .repo_id
                .as_ref()
                .and_then(|repo_id| self.aliases.get(repo_id).cloned()),
            path: item.file_path.clone(),
            start_line: item.start_line,
            end_line: item.end_line,
            explanation: item.note.clone(),
            provenance: item.provenance,
            confidence: item.confidence,
        }
    }

    #[cfg(test)]
    pub(super) fn fixture(
        aliases: Vec<(RepoId, String)>,
        nodes: Vec<Node>,
        edges: Vec<Edge>,
        evidence: Vec<Evidence>,
    ) -> Self {
        Self::from_parts(aliases.into_iter().collect(), nodes, edges, evidence, None)
    }
}

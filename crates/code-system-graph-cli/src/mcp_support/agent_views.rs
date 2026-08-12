//! Semantic, machine-readable views paired with agent-facing MCP Markdown.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use code_system_graph_core::{
    AgentNextAction, ResolvedSymbol, SearchCoverage, SearchExplanation, SearchReport
};
use code_system_graph_model::{
    Edge, EdgeKind, EpistemicStatus, Evidence, EvidenceId, FreshnessSummary, Node, NodeId, NodeKind, OverallFreshness, Provenance, RepoFreshness, RepoFreshnessState, RepoId, ToolStatus, TraceReport
};
use code_system_graph_store_sqlite::SqliteStore;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{GraphStatusReport, SourceContextReport};
use crate::{
    ExploreCoverage, ExploreFederatedHandoff, ExploreLocalRelationship, ExploreReport, ExploreRepositoryContext
};

pub(super) const AGENT_DELIVERY_SCHEMA_VERSION: u32 = 5;
const QUERY_RELATION_PREVIEW_LIMIT: usize = 2;
const REPOSITORY_RELATION_PREVIEW_LIMIT: usize = 8;
const QUERY_ARCHITECTURE_RELATION_LIMIT: usize = 8;
const SOURCE_CONTEXT_PROJECTION_LIMIT: usize = 32;
const PROJECTED_EVIDENCE_PER_RELATION_LIMIT: usize = 8;
const STRUCTURAL_OWNER_EVIDENCE_LIMIT: usize = 2;

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
    pub derived_event_integration_count: usize,
    pub workspace_scoped_relationship_count: usize,
    pub repositories: Vec<AgentRepositoryFreshness>,
}

#[derive(Debug, Clone, Default)]
struct RepositoryOwnership {
    repositories: BTreeSet<RepoId>,
    direct: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct AgentPresentationContext {
    aliases: BTreeMap<RepoId, String>,
    nodes: BTreeMap<NodeId, Node>,
    edges: Vec<Edge>,
    evidence: BTreeMap<EvidenceId, Evidence>,
    ownership: BTreeMap<NodeId, RepositoryOwnership>,
    incident_semantic_edges: BTreeMap<NodeId, Vec<usize>>,
    semantic_edges_by_repository: BTreeMap<RepoId, Vec<usize>>,
    structural_neighbors: BTreeMap<NodeId, Vec<NodeId>>,
    structural_owner_evidence: BTreeMap<NodeId, Vec<EvidenceId>>,
    repository_nodes: BTreeMap<RepoId, NodeId>,
    event_publishers: BTreeMap<NodeId, Vec<usize>>,
    event_subscribers: BTreeMap<NodeId, Vec<usize>>,
    event_channels_by_repository: BTreeMap<RepoId, BTreeSet<NodeId>>,
    gap: Option<String>,
}

impl AgentPresentationContext {
    pub(crate) fn load(database_path: &std::path::Path, workspace: &str) -> Self {
        match Self::try_load(database_path, workspace) {
            Ok(context) => context,
            Err(error) => Self {
                gap: Some(format!(
                    "Semantic relationship context is unavailable: {error}"
                )),
                ..Self::default()
            },
        }
    }

    fn try_load(database_path: &std::path::Path, workspace: &str) -> Result<Self, String> {
        let store =
            SqliteStore::open_read_only(database_path).map_err(|error| error.to_string())?;
        let registry = store
            .load_workspace_registry(workspace)
            .map_err(|error| error.to_string())?;
        let (nodes, edges) = store
            .load_current_graph(workspace)
            .map_err(|error| error.to_string())?;
        let evidence = store
            .load_current_evidence(workspace)
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
        let ownership = resolve_structural_ownership(&nodes, &edges);
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
        let mut event_publishers = BTreeMap::<NodeId, Vec<usize>>::new();
        let mut event_subscribers = BTreeMap::<NodeId, Vec<usize>>::new();
        let mut event_channels_by_repository = BTreeMap::<RepoId, BTreeSet<NodeId>>::new();
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
            match edge.kind {
                EdgeKind::Publishes => {
                    event_publishers
                        .entry(edge.target.clone())
                        .or_default()
                        .push(index);
                    if let Some(repository) = unique_owner(&ownership, &edge.source) {
                        event_channels_by_repository
                            .entry(repository.clone())
                            .or_default()
                            .insert(edge.target.clone());
                    }
                }
                EdgeKind::Subscribes => {
                    event_subscribers
                        .entry(edge.target.clone())
                        .or_default()
                        .push(index);
                    if let Some(repository) = unique_owner(&ownership, &edge.source) {
                        event_channels_by_repository
                            .entry(repository.clone())
                            .or_default()
                            .insert(edge.target.clone());
                    }
                }
                _ => {}
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
        Self {
            aliases,
            nodes,
            edges,
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
            event_publishers,
            event_subscribers,
            event_channels_by_repository,
            gap,
        }
    }

    pub(super) fn gap(&self) -> Option<&str> {
        self.gap.as_deref()
    }

    pub(super) fn entity(&self, node: &Node) -> AgentEntityView {
        let ownership = self.ownership.get(&node.id);
        let unique_repository = ownership.and_then(|ownership| {
            (ownership.repositories.len() == 1)
                .then(|| ownership.repositories.first())
                .flatten()
        });
        let repository_attribution = match ownership {
            Some(ownership) if ownership.direct && unique_repository.is_some() => {
                AgentRepositoryAttribution::Direct
            }
            Some(_) if unique_repository.is_some() => AgentRepositoryAttribution::StructuralOwner,
            Some(ownership) if ownership.repositories.len() > 1 => {
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
                .filter(|ownership| ownership.repositories.len() > 1)
                .flat_map(|ownership| &ownership.repositories)
                .map(|repo_id| {
                    self.aliases
                        .get(repo_id)
                        .cloned()
                        .unwrap_or_else(|| repo_id.as_str().to_owned())
                })
                .collect(),
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

    fn semantic_relations_for(
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

    #[expect(
        clippy::too_many_lines,
        reason = "The bounded join keeps publisher, subscriber, repository, evidence, status, and direction checks together"
    )]
    fn derived_event_relations_for(
        &self,
        focus_nodes: &[NodeId],
        maximum: usize,
    ) -> Vec<AgentRelationView> {
        let mut channels = BTreeSet::new();
        let mut focus_repositories = BTreeSet::new();
        for node_id in focus_nodes {
            if self.event_publishers.contains_key(node_id)
                || self.event_subscribers.contains_key(node_id)
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
                if let Some(repository_channels) = self.event_channels_by_repository.get(repository)
                {
                    channels.extend(repository_channels.iter().cloned());
                }
            }
        }
        let mut relations = Vec::new();
        for channel_id in channels {
            let Some(channel) = self.nodes.get(&channel_id) else {
                continue;
            };
            let publishers = self
                .event_publishers
                .get(&channel_id)
                .into_iter()
                .flatten()
                .filter_map(|index| self.edges.get(*index))
                .filter_map(|edge| {
                    unique_owner(&self.ownership, &edge.source)
                        .cloned()
                        .map(|repository| (repository, edge))
                })
                .collect::<Vec<_>>();
            let subscribers = self
                .event_subscribers
                .get(&channel_id)
                .into_iter()
                .flatten()
                .filter_map(|index| self.edges.get(*index))
                .filter_map(|edge| {
                    unique_owner(&self.ownership, &edge.source)
                        .cloned()
                        .map(|repository| (repository, edge))
                })
                .collect::<Vec<_>>();
            for (publisher_repository, publisher) in &publishers {
                for (subscriber_repository, subscriber) in &subscribers {
                    if !focus_repositories.is_empty()
                        && !focus_repositories.contains(publisher_repository)
                        && !focus_repositories.contains(subscriber_repository)
                    {
                        continue;
                    }
                    let Some(source) = self.repository_entity(publisher_repository) else {
                        continue;
                    };
                    let Some(target) = self.repository_entity(subscriber_repository) else {
                        continue;
                    };
                    let direction = if focus_repositories.contains(subscriber_repository)
                        && !focus_repositories.contains(publisher_repository)
                    {
                        AgentRelationDirection::Incoming
                    } else {
                        AgentRelationDirection::Outgoing
                    };
                    let mut evidence = publisher
                        .evidence
                        .iter()
                        .chain(&subscriber.evidence)
                        .filter_map(|evidence_id| self.evidence.get(evidence_id))
                        .map(|item| self.evidence_view(item, AgentEvidenceRole::ObservedRelation))
                        .collect::<Vec<_>>();
                    evidence.sort_by(|left, right| left.evidence_id.cmp(&right.evidence_id));
                    evidence.dedup_by(|left, right| left.evidence_id == right.evidence_id);
                    evidence.truncate(PROJECTED_EVIDENCE_PER_RELATION_LIMIT);
                    relations.push(AgentRelationView {
                        edge_id: format!(
                            "derived:event:{}:{}:{}",
                            channel.id.as_str(),
                            publisher_repository.as_str(),
                            subscriber_repository.as_str()
                        ),
                        edge_kind: EdgeKind::DeliversTo,
                        source,
                        relationship: format!("publishes event {} to", channel.label),
                        inverse_relationship: format!("receives event {} from", channel.label),
                        target,
                        scope: if publisher_repository == subscriber_repository {
                            AgentRelationScope::Local
                        } else {
                            AgentRelationScope::CrossRepository
                        },
                        direction,
                        derivation: AgentRelationDerivation::EventDeliveryPath,
                        status: conservative_status(publisher.status, subscriber.status),
                        confidence: publisher.confidence.min(subscriber.confidence),
                        evidence,
                    });
                    if relations.len() >= maximum {
                        break;
                    }
                }
                if relations.len() >= maximum {
                    break;
                }
            }
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

    fn structurally_adjacent_to_any(
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

    fn derived_event_integration_count(&self) -> usize {
        self.event_publishers
            .keys()
            .filter_map(|channel| {
                let publishers = self.event_publishers.get(channel)?;
                let subscribers = self.event_subscribers.get(channel)?;
                let publisher_repositories = publishers
                    .iter()
                    .filter_map(|index| self.edges.get(*index))
                    .filter_map(|edge| unique_owner(&self.ownership, &edge.source))
                    .collect::<BTreeSet<_>>();
                let subscriber_repositories = subscribers
                    .iter()
                    .filter_map(|index| self.edges.get(*index))
                    .filter_map(|edge| unique_owner(&self.ownership, &edge.source))
                    .collect::<BTreeSet<_>>();
                let local_pairs = publisher_repositories
                    .intersection(&subscriber_repositories)
                    .count();
                Some(
                    publisher_repositories
                        .len()
                        .saturating_mul(subscriber_repositories.len())
                        .saturating_sub(local_pairs),
                )
            })
            .sum()
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
        derived_event_integration_count: context.derived_event_integration_count(),
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

pub(super) fn relationship_phrase(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Contains => "contains",
        EdgeKind::Provides => "provides",
        EdgeKind::Consumes => "consumes",
        EdgeKind::CallsRemote => "calls remotely",
        EdgeKind::Publishes => "publishes",
        EdgeKind::Subscribes => "subscribes to",
        EdgeKind::DeliversTo => "delivers to",
        EdgeKind::DependsOnPackage => "depends on package",
        EdgeKind::DependsOnRepository => "depends on repository",
        EdgeKind::ReadsTable => "reads table",
        EdgeKind::WritesTable => "writes table",
        EdgeKind::Deploys => "deploys",
        EdgeKind::Configures => "configures",
        EdgeKind::Documents => "documents",
        EdgeKind::OwnedBy => "is owned by",
        EdgeKind::ImplementedBy => "is implemented by",
        EdgeKind::Validates => "validates",
        EdgeKind::ChangedIn => "was changed in",
        EdgeKind::Affects => "affects",
        EdgeKind::Precedes => "precedes",
        EdgeKind::Reverts => "reverts",
        EdgeKind::CompatibleWith => "is compatible with",
        EdgeKind::IncompatibleWith => "is incompatible with",
        EdgeKind::MemberOf => "belongs to",
        EdgeKind::ManualLink => "is manually linked to",
    }
}

pub(super) fn inverse_relationship_phrase(kind: EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Contains => "is contained by",
        EdgeKind::Provides => "is provided by",
        EdgeKind::Consumes => "is consumed by",
        EdgeKind::CallsRemote => "is called remotely by",
        EdgeKind::Publishes => "is published by",
        EdgeKind::Subscribes => "has subscriber",
        EdgeKind::DeliversTo => "receives deliveries from",
        EdgeKind::DependsOnPackage => "is required by",
        EdgeKind::DependsOnRepository => "is a dependency of",
        EdgeKind::ReadsTable => "is read by",
        EdgeKind::WritesTable => "is written by",
        EdgeKind::Deploys => "is deployed by",
        EdgeKind::Configures => "is configured by",
        EdgeKind::Documents => "is documented by",
        EdgeKind::OwnedBy => "owns",
        EdgeKind::ImplementedBy => "implements",
        EdgeKind::Validates => "is validated by",
        EdgeKind::ChangedIn => "contains change to",
        EdgeKind::Affects => "is affected by",
        EdgeKind::Precedes => "is preceded by",
        EdgeKind::Reverts => "is reverted by",
        EdgeKind::CompatibleWith => "is compatible with",
        EdgeKind::IncompatibleWith => "is incompatible with",
        EdgeKind::MemberOf => "has member",
        EdgeKind::ManualLink => "is manually linked from",
    }
}

pub(super) fn node_kind_name(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Repository => "repository",
        NodeKind::Service => "service",
        NodeKind::Package => "package",
        NodeKind::Artifact => "artifact",
        NodeKind::SymbolRef => "symbol",
        NodeKind::TestCase => "test",
        NodeKind::HttpOperation => "HTTP operation",
        NodeKind::GraphqlOperation => "GraphQL operation",
        NodeKind::RpcMethod => "RPC method",
        NodeKind::EventChannel => "event channel",
        NodeKind::EventSchema => "event schema",
        NodeKind::Database => "database",
        NodeKind::DatabaseTable => "database table",
        NodeKind::DatabaseColumn => "database column",
        NodeKind::ConfigKey => "configuration key",
        NodeKind::Deployment => "deployment",
        NodeKind::Document => "document",
        NodeKind::Adr => "architecture decision",
        NodeKind::Owner => "owner",
        NodeKind::ChangeSet => "change set",
        NodeKind::PullRequest => "pull request",
        NodeKind::Community => "community",
    }
}

pub(super) fn freshness_name(freshness: OverallFreshness) -> &'static str {
    match freshness {
        OverallFreshness::Fresh => "fresh",
        OverallFreshness::Stale => "stale",
        OverallFreshness::Partial => "partial",
        OverallFreshness::Unknown => "unknown",
    }
}

pub(super) fn freshness_state_name(state: RepoFreshnessState) -> &'static str {
    match state {
        RepoFreshnessState::Fresh => "fresh",
        RepoFreshnessState::WorkingTreeChanged => "working tree changed",
        RepoFreshnessState::CommitsBehind => "commits behind",
        RepoFreshnessState::ConfigChanged => "configuration changed",
        RepoFreshnessState::ExtractorChanged => "extractor changed",
        RepoFreshnessState::CodegraphPending => "CodeGraph pending",
        RepoFreshnessState::Partial => "partial",
        RepoFreshnessState::Corrupt => "corrupt",
        RepoFreshnessState::Unknown => "unknown",
        RepoFreshnessState::Unavailable => "unavailable",
    }
}

pub(super) fn epistemic_status_name(status: EpistemicStatus) -> &'static str {
    match status {
        EpistemicStatus::Confirmed => "confirmed",
        EpistemicStatus::Inferred => "inferred",
        EpistemicStatus::Ambiguous => "ambiguous",
        EpistemicStatus::Stale => "stale",
        EpistemicStatus::Incomplete => "incomplete",
    }
}

fn resolve_structural_ownership(
    nodes: &BTreeMap<NodeId, Node>,
    edges: &[Edge],
) -> BTreeMap<NodeId, RepositoryOwnership> {
    let mut ownership = nodes
        .values()
        .map(|node| {
            let repositories = node.repo_id.iter().cloned().collect::<BTreeSet<_>>();
            (
                node.id.clone(),
                RepositoryOwnership {
                    repositories,
                    direct: node.repo_id.is_some(),
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut children = BTreeMap::<NodeId, Vec<NodeId>>::new();
    for edge in edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Contains && edge.status == EpistemicStatus::Confirmed)
    {
        children
            .entry(edge.source.clone())
            .or_default()
            .push(edge.target.clone());
    }
    let mut queue = ownership
        .iter()
        .filter(|(_, owner)| !owner.repositories.is_empty())
        .map(|(node_id, _)| node_id.clone())
        .collect::<VecDeque<_>>();
    while let Some(parent) = queue.pop_front() {
        let parent_repositories = ownership
            .get(&parent)
            .map(|owner| owner.repositories.clone())
            .unwrap_or_default();
        for child in children.get(&parent).into_iter().flatten() {
            let Some(child_owner) = ownership.get_mut(child) else {
                continue;
            };
            if child_owner.direct {
                continue;
            }
            let previous = child_owner.repositories.clone();
            for repository in &parent_repositories {
                if child_owner.repositories.len() >= 2
                    && !child_owner.repositories.contains(repository)
                {
                    continue;
                }
                child_owner.repositories.insert(repository.clone());
            }
            if child_owner.repositories != previous {
                queue.push_back(child.clone());
            }
        }
    }
    ownership
}

fn unique_owner<'a>(
    ownership: &'a BTreeMap<NodeId, RepositoryOwnership>,
    node_id: &NodeId,
) -> Option<&'a RepoId> {
    let owner = ownership.get(node_id)?;
    (owner.repositories.len() == 1)
        .then(|| owner.repositories.first())
        .flatten()
}

fn agent_entity_label(node: &Node) -> String {
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

fn conservative_status(left: EpistemicStatus, right: EpistemicStatus) -> EpistemicStatus {
    fn severity(status: EpistemicStatus) -> u8 {
        match status {
            EpistemicStatus::Confirmed => 0,
            EpistemicStatus::Inferred => 1,
            EpistemicStatus::Ambiguous => 2,
            EpistemicStatus::Stale => 3,
            EpistemicStatus::Incomplete => 4,
        }
    }
    if severity(left) >= severity(right) {
        left
    } else {
        right
    }
}

fn remove_redundant_package_relations(relations: &mut Vec<AgentRelationView>) {
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

fn relation_priority(relation: &AgentRelationView) -> (u8, u8, u8, u8) {
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

fn is_structural_edge(kind: EdgeKind) -> bool {
    matches!(kind, EdgeKind::Contains | EdgeKind::MemberOf)
}

fn relation_is_visually_distinct(relation: &AgentRelationView) -> bool {
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

fn entity_path(node: &Node) -> Option<String> {
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
}

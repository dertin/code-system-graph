//! Shared deterministic repository attribution for persisted graph nodes.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use code_system_graph_model::{Edge, EdgeKind, EpistemicStatus, Node, NodeId, NodeKind, RepoId};

#[derive(Debug, Clone, Default)]
pub(crate) struct RepositoryOwnership {
    repositories: BTreeSet<RepoId>,
    direct: bool,
}

impl RepositoryOwnership {
    pub(crate) fn repositories(&self) -> &BTreeSet<RepoId> {
        &self.repositories
    }

    pub(crate) const fn is_direct(&self) -> bool {
        self.direct
    }

    pub(crate) fn unique_repository(&self) -> Option<&RepoId> {
        (self.repositories.len() == 1)
            .then(|| self.repositories.first())
            .flatten()
    }
}

/// Resolves repository ownership through confirmed containment only.
///
/// Direct attribution always wins. All distinct inherited candidates are retained so consumers
/// can report ambiguity without silently understating its extent.
pub(crate) fn resolve_repository_ownership<'a>(
    nodes: impl IntoIterator<Item = &'a Node>,
    edges: &[Edge],
    mut should_stop: impl FnMut() -> bool,
) -> Result<BTreeMap<NodeId, RepositoryOwnership>, ()> {
    let nodes = nodes
        .into_iter()
        .map(|node| (node.id.clone(), node))
        .collect::<BTreeMap<_, _>>();
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
    for (index, edge) in edges.iter().enumerate() {
        if index.is_multiple_of(256) && should_stop() {
            return Err(());
        }
        if edge.kind == EdgeKind::Contains
            && edge.status == EpistemicStatus::Confirmed
            && ownership_containment(&nodes, edge)
        {
            children
                .entry(edge.source.clone())
                .or_default()
                .push(edge.target.clone());
        }
    }
    for values in children.values_mut() {
        values.sort();
        values.dedup();
    }
    let mut queue = ownership
        .iter()
        .filter(|(_, owner)| !owner.repositories.is_empty())
        .map(|(node_id, _)| node_id.clone())
        .collect::<VecDeque<_>>();
    let mut work = 0_usize;
    while let Some(parent) = queue.pop_front() {
        work = work.saturating_add(1);
        if work.is_multiple_of(256) && should_stop() {
            return Err(());
        }
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
                child_owner.repositories.insert(repository.clone());
            }
            if child_owner.repositories != previous {
                queue.push_back(child.clone());
            }
        }
    }
    Ok(ownership)
}

fn ownership_containment(nodes: &BTreeMap<NodeId, &Node>, edge: &Edge) -> bool {
    let Some(source) = nodes.get(&edge.source) else {
        return false;
    };
    let Some(target) = nodes.get(&edge.target) else {
        return false;
    };
    match source.kind {
        NodeKind::Repository => target.kind == NodeKind::Artifact,
        NodeKind::Artifact => !matches!(target.kind, NodeKind::Document),
        NodeKind::Database | NodeKind::DatabaseTable | NodeKind::EventChannel => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{
        Edge, EdgeId, EdgeKind, EpistemicStatus, Node, NodeId, NodeKind, RepoId
    };

    use super::resolve_repository_ownership;

    fn node(id: &str, repository: Option<&str>) -> Node {
        Node {
            id: NodeId::new(id),
            kind: NodeKind::Artifact,
            repo_id: repository.map(RepoId::new),
            stable_key: id.to_owned(),
            label: id.to_owned(),
        }
    }

    fn node_of_kind(id: &str, repository: Option<&str>, kind: NodeKind) -> Node {
        Node {
            kind,
            ..node(id, repository)
        }
    }

    fn contains(id: &str, source: &str, target: &str, status: EpistemicStatus) -> Edge {
        Edge {
            id: EdgeId::new(id),
            source: NodeId::new(source),
            target: NodeId::new(target),
            kind: EdgeKind::Contains,
            confidence: 1.0,
            status,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn inferred_containment_should_not_assign_repository_ownership() {
        let nodes = vec![node("parent", Some("repo:a")), node("child", None)];
        let edges = vec![contains(
            "contains",
            "parent",
            "child",
            EpistemicStatus::Inferred,
        )];

        let ownership = resolve_repository_ownership(&nodes, &edges, || false)
            .expect("ownership should resolve");

        assert!(
            ownership
                .get(&NodeId::new("child"))
                .and_then(super::RepositoryOwnership::unique_repository)
                .is_none()
        );
    }

    #[test]
    fn inherited_ownership_should_not_overwrite_direct_attribution() {
        let nodes = vec![
            node("parent", Some("repo:a")),
            node("child", Some("repo:b")),
        ];
        let edges = vec![contains(
            "contains",
            "parent",
            "child",
            EpistemicStatus::Confirmed,
        )];

        let ownership = resolve_repository_ownership(&nodes, &edges, || false)
            .expect("ownership should resolve");

        assert_eq!(
            ownership
                .get(&NodeId::new("child"))
                .and_then(super::RepositoryOwnership::unique_repository)
                .map(RepoId::as_str),
            Some("repo:b")
        );
    }

    #[test]
    fn document_containment_should_not_claim_semantic_entity_ownership() {
        let nodes = vec![
            node_of_kind("repo:api", Some("repo:api"), NodeKind::Repository),
            node_of_kind("api-artifact", Some("repo:api"), NodeKind::Artifact),
            node_of_kind("repo:docs", Some("repo:docs"), NodeKind::Repository),
            node_of_kind("docs-artifact", Some("repo:docs"), NodeKind::Artifact),
            node_of_kind("documentation", None, NodeKind::Document),
            node_of_kind("table", None, NodeKind::DatabaseTable),
        ];
        let edges = vec![
            contains(
                "repo-api",
                "repo:api",
                "api-artifact",
                EpistemicStatus::Confirmed,
            ),
            contains(
                "api-table",
                "api-artifact",
                "table",
                EpistemicStatus::Confirmed,
            ),
            contains(
                "repo-docs",
                "repo:docs",
                "docs-artifact",
                EpistemicStatus::Confirmed,
            ),
            contains(
                "docs-doc",
                "docs-artifact",
                "documentation",
                EpistemicStatus::Confirmed,
            ),
            contains(
                "doc-table",
                "documentation",
                "table",
                EpistemicStatus::Confirmed,
            ),
        ];

        let ownership = resolve_repository_ownership(&nodes, &edges, || false)
            .expect("ownership should resolve");

        assert_eq!(
            ownership
                .get(&NodeId::new("table"))
                .and_then(super::RepositoryOwnership::unique_repository),
            Some(&RepoId::new("repo:api"))
        );
    }

    #[test]
    fn conflicting_confirmed_parents_should_remain_ambiguous() {
        let nodes = vec![
            node("left", Some("repo:a")),
            node("right", Some("repo:b")),
            node("child", None),
        ];
        let edges = vec![
            contains("left-child", "left", "child", EpistemicStatus::Confirmed),
            contains("right-child", "right", "child", EpistemicStatus::Confirmed),
        ];

        let ownership = resolve_repository_ownership(&nodes, &edges, || false)
            .expect("ownership should resolve");
        let child = ownership.get(&NodeId::new("child")).expect("child owner");

        assert_eq!(child.repositories().len(), 2);
        assert!(child.unique_repository().is_none());
    }

    #[test]
    fn every_conflicting_repository_candidate_is_retained() {
        let nodes = vec![
            node("a", Some("repo:a")),
            node("b", Some("repo:b")),
            node("c", Some("repo:c")),
            node("child", None),
        ];
        let edges = vec![
            contains("a-child", "a", "child", EpistemicStatus::Confirmed),
            contains("b-child", "b", "child", EpistemicStatus::Confirmed),
            contains("c-child", "c", "child", EpistemicStatus::Confirmed),
        ];

        let ownership = resolve_repository_ownership(&nodes, &edges, || false)
            .expect("ownership should resolve");
        let child = ownership.get(&NodeId::new("child")).expect("child owner");

        assert_eq!(child.repositories().len(), 3);
    }
}

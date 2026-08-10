//! Pure persisted-graph correlation for Explore.

use std::collections::{BTreeMap, BTreeSet};

use code_system_graph_core::{ExecutionPolicy, ResolvedSymbol};
use code_system_graph_model::{
    Edge, Evidence, EvidenceId, Node, NodeId, RepoFreshness, RepoFreshnessState, RepoId, RepositoryRecord, WorkspaceRecord
};

use super::{ExploreEvidenceLocation, ExploreFederatedHandoff, ExploreRepositoryContext};

pub(super) struct ExploreCorrelationInput {
    pub(super) repository: RepositoryRecord,
    pub(super) anchors: Vec<ResolvedSymbol>,
    pub(super) nodes: Vec<Node>,
    pub(super) edges: Vec<Edge>,
    pub(super) evidence: Vec<Evidence>,
    pub(super) repositories: BTreeMap<RepoId, ExploreRepositoryContext>,
    pub(super) policy: ExecutionPolicy,
}

pub(super) struct ExploreCorrelationOutput {
    pub(super) handoffs: Vec<ExploreFederatedHandoff>,
    pub(super) truncations: Vec<String>,
}

pub(super) fn explore_repository_context(
    repository: &RepositoryRecord,
    freshness: &[RepoFreshness],
) -> ExploreRepositoryContext {
    ExploreRepositoryContext {
        alias: repository.alias.clone(),
        repo_id: repository.id.clone(),
        root: repository.canonical_path.display.clone(),
        revision: repository.head_commit.clone(),
        freshness: freshness
            .iter()
            .find(|item| item.repo_id == repository.id)
            .map_or(RepoFreshnessState::Unknown, |item| item.state),
    }
}

pub(super) fn explore_repository_contexts(
    registry: &WorkspaceRecord,
    freshness: &[RepoFreshness],
) -> BTreeMap<RepoId, ExploreRepositoryContext> {
    registry
        .repositories
        .iter()
        .map(|repository| {
            (
                repository.id.clone(),
                explore_repository_context(repository, freshness),
            )
        })
        .collect()
}

pub(super) fn correlate_explore_handoffs(
    input: &ExploreCorrelationInput,
    mut should_stop: impl FnMut() -> bool,
) -> Result<ExploreCorrelationOutput, ()> {
    let per_anchor = usize::try_from(input.policy.max_explore_federated_handoffs_per_anchor)
        .expect("validated policy count is usize-representable");
    let total_limit = usize::try_from(input.policy.max_explore_federated_handoffs)
        .expect("validated policy count is usize-representable");
    let evidence_limit = usize::try_from(input.policy.max_explore_evidence_locations_per_handoff)
        .expect("validated policy count is usize-representable");
    let (node_by_id, evidence_by_id) = correlation_indexes(input, &mut should_stop)?;
    let mut output = Vec::new();
    let mut truncations = Vec::new();
    for anchor in &input.anchors {
        if should_stop() {
            return Err(());
        }
        let matching_evidence = matching_anchor_evidence(input, anchor, &mut should_stop)?;
        if matching_evidence.is_empty() {
            continue;
        }
        let mut emitted_for_anchor = 0_usize;
        let mut seen = BTreeSet::new();
        for (index, edge) in input.edges.iter().enumerate() {
            if index % 256 == 0 && should_stop() {
                return Err(());
            }
            if !edge
                .evidence
                .iter()
                .any(|id| matching_evidence.contains(id))
            {
                continue;
            }
            let remote = [&edge.source, &edge.target]
                .into_iter()
                .filter_map(|id| node_by_id.get(id).copied())
                .find(|node| {
                    node.repo_id
                        .as_ref()
                        .is_some_and(|id| id != &input.repository.id)
                });
            let Some(remote) = remote else {
                continue;
            };
            if !seen.insert(remote.id.clone()) {
                continue;
            }
            if emitted_for_anchor == per_anchor || output.len() == total_limit {
                truncations.push(
                    if output.len() == total_limit {
                        "maxExploreFederatedHandoffs"
                    } else {
                        "maxExploreFederatedHandoffsPerAnchor"
                    }
                    .to_owned(),
                );
                break;
            }
            let locations =
                explore_handoff_locations(edge, &evidence_by_id, evidence_limit, &mut truncations);
            output.push(ExploreFederatedHandoff {
                anchor: anchor
                    .qualified_name
                    .clone()
                    .unwrap_or_else(|| anchor.name.clone()),
                node_id: remote.id.clone(),
                label: remote.label.clone(),
                remote_repository: remote
                    .repo_id
                    .as_ref()
                    .and_then(|id| input.repositories.get(id).cloned()),
                status: edge.status,
                confidence: edge.confidence,
                evidence: locations,
            });
            emitted_for_anchor += 1;
        }
        if output.len() == total_limit {
            break;
        }
    }
    Ok(ExploreCorrelationOutput {
        handoffs: output,
        truncations,
    })
}

type CorrelationIndexes<'a> = (
    BTreeMap<&'a NodeId, &'a Node>,
    BTreeMap<&'a EvidenceId, &'a Evidence>,
);

fn correlation_indexes<'a>(
    input: &'a ExploreCorrelationInput,
    should_stop: &mut impl FnMut() -> bool,
) -> Result<CorrelationIndexes<'a>, ()> {
    let mut node_by_id = BTreeMap::new();
    for (index, node) in input.nodes.iter().enumerate() {
        if index % 256 == 0 && should_stop() {
            return Err(());
        }
        node_by_id.insert(&node.id, node);
    }
    let mut evidence_by_id = BTreeMap::new();
    for (index, item) in input.evidence.iter().enumerate() {
        if index % 256 == 0 && should_stop() {
            return Err(());
        }
        evidence_by_id.insert(&item.id, item);
    }
    Ok((node_by_id, evidence_by_id))
}

fn matching_anchor_evidence<'a>(
    input: &'a ExploreCorrelationInput,
    anchor: &ResolvedSymbol,
    should_stop: &mut impl FnMut() -> bool,
) -> Result<BTreeSet<&'a EvidenceId>, ()> {
    let anchor_path = anchor.file_path.trim_start_matches("./");
    let mut matching = BTreeSet::new();
    for (index, item) in input.evidence.iter().enumerate() {
        if index % 256 == 0 && should_stop() {
            return Err(());
        }
        let matches_anchor = item.repo_id.as_ref() == Some(&input.repository.id)
            && item
                .file_path
                .as_deref()
                .is_some_and(|path| path.trim_start_matches("./") == anchor_path)
            && item.start_line.is_none_or(|start| {
                usize::try_from(start).is_ok_and(|start| start <= anchor.start_line)
            })
            && item
                .end_line
                .is_none_or(|end| usize::try_from(end).is_ok_and(|end| end >= anchor.start_line));
        if matches_anchor {
            matching.insert(&item.id);
        }
    }
    Ok(matching)
}

fn explore_handoff_locations(
    edge: &Edge,
    evidence_by_id: &BTreeMap<&EvidenceId, &Evidence>,
    evidence_limit: usize,
    truncations: &mut Vec<String>,
) -> Vec<ExploreEvidenceLocation> {
    let mut locations = edge
        .evidence
        .iter()
        .filter_map(|id| evidence_by_id.get(id).copied())
        .filter_map(|item| {
            Some(ExploreEvidenceLocation {
                repo_id: item.repo_id.clone()?,
                path: item.file_path.clone()?,
                start_line: item.start_line,
                end_line: item.end_line,
            })
        })
        .collect::<Vec<_>>();
    locations.sort_by(|left, right| {
        (&left.repo_id, &left.path, left.start_line, left.end_line).cmp(&(
            &right.repo_id,
            &right.path,
            right.start_line,
            right.end_line,
        ))
    });
    locations.dedup();
    if locations.len() > evidence_limit {
        locations.truncate(evidence_limit);
        truncations.push("maxExploreEvidenceLocationsPerHandoff".to_owned());
    }
    locations
}

#[cfg(test)]
mod tests {
    use code_system_graph_core::{ExecutionPolicy, ResolvedSymbol};
    use code_system_graph_model::{
        CheckoutId, NativePath, NativePathEncoding, RepoId, RepositoryRecord
    };

    use super::{ExploreCorrelationInput, correlate_explore_handoffs};

    #[test]
    fn correlation_should_stop_before_scanning_when_cancelled() {
        let input = ExploreCorrelationInput {
            repository: RepositoryRecord {
                id: RepoId::new("repo:api"),
                checkout_id: CheckoutId::new("checkout:api"),
                alias: "api".to_owned(),
                canonical_path: NativePath {
                    encoding: NativePathEncoding::Utf8,
                    bytes: b"/api".to_vec(),
                    display: "/api".to_owned(),
                },
                git_common_dir: None,
                normalized_remote: None,
                head_commit: None,
                is_linked_worktree: false,
                working_tree_dirty: false,
            },
            anchors: vec![ResolvedSymbol {
                local_id: None,
                name: "anchor".to_owned(),
                qualified_name: None,
                kind: "function".to_owned(),
                file_path: "src/lib.rs".to_owned(),
                start_line: 1,
                score: None,
            }],
            nodes: Vec::new(),
            edges: Vec::new(),
            evidence: Vec::new(),
            repositories: std::collections::BTreeMap::new(),
            policy: ExecutionPolicy::default(),
        };

        assert!(correlate_explore_handoffs(&input, || true).is_err());
    }
}

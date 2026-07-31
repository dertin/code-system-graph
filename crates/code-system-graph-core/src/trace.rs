use std::collections::{BTreeMap, BTreeSet, VecDeque};

use code_system_graph_model::{Edge, Node, NodeId, TraceReport, TraceSegment};
use thiserror::Error;

/// Validated in-memory projection used for bounded graph traversal.
#[derive(Debug, Clone)]
pub struct FederatedGraph {
    nodes: BTreeMap<NodeId, Node>,
    outgoing: BTreeMap<NodeId, Vec<Edge>>,
}

/// Error returned while constructing or traversing a federated graph.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum TraceError {
    /// An edge references a missing node.
    #[error("edge `{edge}` references missing node `{node}`")]
    DanglingEdge {
        /// Stable edge identifier.
        edge: String,
        /// Stable missing node identifier.
        node: String,
    },
    /// A requested anchor is absent.
    #[error("trace anchor `{0}` was not observed in the selected snapshot")]
    UnknownAnchor(String),
    /// Traversal bound is invalid.
    #[error("trace max_depth must be greater than zero")]
    InvalidDepth,
    /// Internal parent chain was inconsistent.
    #[error("trace parent chain is inconsistent at node `{0}`")]
    InconsistentPath(String),
}

impl FederatedGraph {
    /// Builds a deterministic graph projection and rejects dangling edges.
    ///
    /// # Errors
    ///
    /// Returns [`TraceError::DanglingEdge`] when an edge endpoint is absent.
    pub fn new(nodes: Vec<Node>, edges: Vec<Edge>) -> Result<Self, TraceError> {
        let nodes = nodes
            .into_iter()
            .map(|node| (node.id.clone(), node))
            .collect::<BTreeMap<_, _>>();
        let mut outgoing: BTreeMap<NodeId, Vec<Edge>> = BTreeMap::new();
        for edge in edges {
            for endpoint in [&edge.source, &edge.target] {
                if !nodes.contains_key(endpoint) {
                    return Err(TraceError::DanglingEdge {
                        edge: edge.id.as_str().to_owned(),
                        node: endpoint.as_str().to_owned(),
                    });
                }
            }
            outgoing.entry(edge.source.clone()).or_default().push(edge);
        }
        for adjacent in outgoing.values_mut() {
            adjacent.sort_by(|left, right| left.id.cmp(&right.id));
        }
        Ok(Self { nodes, outgoing })
    }

    /// Finds the first deterministic breadth-first path between two anchors.
    ///
    /// An empty report with a coverage gap means no confirmed path was observed within the
    /// bound; it does not mean the repositories are independent.
    ///
    /// # Errors
    ///
    /// Returns [`TraceError`] for missing anchors, zero depth, or inconsistent graph state.
    pub fn trace(
        &self,
        from: &NodeId,
        to: &NodeId,
        max_depth: usize,
    ) -> Result<TraceReport, TraceError> {
        if max_depth == 0 {
            return Err(TraceError::InvalidDepth);
        }
        if !self.nodes.contains_key(from) {
            return Err(TraceError::UnknownAnchor(from.as_str().to_owned()));
        }
        if !self.nodes.contains_key(to) {
            return Err(TraceError::UnknownAnchor(to.as_str().to_owned()));
        }
        if from == to {
            return Ok(TraceReport {
                segments: Vec::new(),
                truncated: false,
                coverage_gaps: Vec::new(),
            });
        }

        let mut queue = VecDeque::from([(from.clone(), 0_usize)]);
        let mut visited = BTreeSet::from([from.clone()]);
        let mut parents: BTreeMap<NodeId, (NodeId, Edge)> = BTreeMap::new();
        let mut truncated = false;
        let mut found = false;

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= max_depth {
                if self
                    .outgoing
                    .get(&current)
                    .is_some_and(|edges| !edges.is_empty())
                {
                    truncated = true;
                }
                continue;
            }
            for edge in self.outgoing.get(&current).into_iter().flatten() {
                if !visited.insert(edge.target.clone()) {
                    continue;
                }
                parents.insert(edge.target.clone(), (current.clone(), edge.clone()));
                if &edge.target == to {
                    found = true;
                    break;
                }
                queue.push_back((edge.target.clone(), depth + 1));
            }
            if found {
                break;
            }
        }

        if !found {
            return Ok(TraceReport {
                segments: Vec::new(),
                truncated,
                coverage_gaps: vec![
                    "No confirmed path was observed within available coverage and bounds."
                        .to_owned(),
                ],
            });
        }

        self.reconstruct(from, to, &parents, truncated)
    }

    fn reconstruct(
        &self,
        from: &NodeId,
        to: &NodeId,
        parents: &BTreeMap<NodeId, (NodeId, Edge)>,
        truncated: bool,
    ) -> Result<TraceReport, TraceError> {
        let mut current = to.clone();
        let mut segments = Vec::new();
        while &current != from {
            let (parent, edge) = parents
                .get(&current)
                .ok_or_else(|| TraceError::InconsistentPath(current.as_str().to_owned()))?;
            let source = self
                .nodes
                .get(parent)
                .ok_or_else(|| TraceError::InconsistentPath(parent.as_str().to_owned()))?;
            let target = self
                .nodes
                .get(&current)
                .ok_or_else(|| TraceError::InconsistentPath(current.as_str().to_owned()))?;
            segments.push(TraceSegment {
                source: source.clone(),
                edge: edge.clone(),
                target: target.clone(),
            });
            current = parent.clone();
        }
        segments.reverse();
        Ok(TraceReport {
            segments,
            truncated,
            coverage_gaps: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{
        Edge, EdgeId, EdgeKind, EpistemicStatus, Node, NodeId, NodeKind
    };

    use super::FederatedGraph;

    fn node(id: &str) -> Node {
        Node {
            id: NodeId::new(id),
            kind: NodeKind::HttpOperation,
            repo_id: None,
            stable_key: id.to_owned(),
            label: id.to_owned(),
        }
    }

    #[test]
    fn trace_should_return_deterministic_shortest_path() {
        let edge = Edge {
            id: EdgeId::new("edge:1"),
            source: NodeId::new("node:web"),
            target: NodeId::new("node:api"),
            kind: EdgeKind::CallsRemote,
            confidence: 1.0,
            status: EpistemicStatus::Confirmed,
            evidence: Vec::new(),
        };
        let graph = FederatedGraph::new(vec![node("node:web"), node("node:api")], vec![edge]);
        let result = graph
            .and_then(|value| value.trace(&NodeId::new("node:web"), &NodeId::new("node:api"), 4));
        let segment_count = result.map(|report| report.segments.len());

        assert_eq!(segment_count, Ok(1));
    }
}

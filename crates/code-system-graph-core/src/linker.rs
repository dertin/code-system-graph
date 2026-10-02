use std::collections::{BTreeMap, BTreeSet};

use code_system_graph_model::{
    Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, EvidenceId, EvidenceRef, HttpLinkReport, LinkDecision, LinkStatus, Node, NodeId, Provenance, stable_id
};
use semver::Version;
use thiserror::Error;

use crate::{ManifestError, ManualLinkConfig, validate_manual_links};

const MANUAL_LINK_MATCHER: &str = "manual_exact";
const MANUAL_LINK_EXTRACTOR: &str = "code-system-graph.manual-link";

/// One exact HTTP contract that could not be linked because several providers matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpLinkAmbiguity {
    /// Canonical HTTP method.
    pub method: String,
    /// Canonical path template.
    pub path: String,
    /// Stable candidate node identifiers in deterministic order.
    pub candidates: Vec<String>,
}

/// Deterministic automatic HTTP links plus unresolved exact-provider ambiguities.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpLinkResolution {
    /// Unambiguous automatic relationships.
    pub edges: Vec<Edge>,
    /// Source-free ambiguous contract identities omitted from the edge set.
    pub ambiguities: Vec<HttpLinkAmbiguity>,
    /// Consumer and test calls left without a provider edge, with outcome counts.
    pub report: HttpLinkReport,
}

/// Endpoint field being resolved for a manual relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManualLinkEndpoint {
    /// The `from` endpoint.
    From,
    /// The `to` endpoint.
    To,
}

impl std::fmt::Display for ManualLinkEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::From => formatter.write_str("from"),
            Self::To => formatter.write_str("to"),
        }
    }
}

/// Error returned while resolving exact manual relationships.
#[derive(Debug, Error)]
pub enum ManualLinkError {
    /// A manually constructed declaration violates manifest invariants.
    #[error(transparent)]
    InvalidDeclaration(#[from] ManifestError),
    /// No node has the exact requested identifier or stable key.
    #[error(
        "manualLinks[{index}].{endpoint} `{value}` did not match any exact node ID or stable key"
    )]
    MissingEndpoint {
        /// Zero-based declaration index.
        index: usize,
        /// Endpoint field.
        endpoint: ManualLinkEndpoint,
        /// Requested exact identity.
        value: String,
    },
    /// More than one node has the exact requested identifier or stable key.
    #[error(
        "manualLinks[{index}].{endpoint} `{value}` is ambiguous across exact candidates {candidates:?}"
    )]
    AmbiguousEndpoint {
        /// Zero-based declaration index.
        index: usize,
        /// Endpoint field.
        endpoint: ManualLinkEndpoint,
        /// Requested exact identity.
        value: String,
        /// Stable candidate identities in deterministic order.
        candidates: Vec<NodeId>,
    },
    /// Distinct endpoint literals resolved to the same node.
    #[error("manualLinks[{index}] resolves both endpoints to `{node}`")]
    ResolvedSelfLink {
        /// Zero-based declaration index.
        index: usize,
        /// Resolved node identity.
        node: String,
    },
    /// Distinct declarations resolved to the same graph relationship.
    #[error(
        "manualLinks[{duplicate}] resolves to the same relationship as manualLinks[{first}]: {source_node:?} -> {target_node:?} ({relation:?})"
    )]
    DuplicateResolvedLink {
        /// Index of the first declaration.
        first: usize,
        /// Index of the repeated declaration.
        duplicate: usize,
        /// Resolved source node.
        source_node: NodeId,
        /// Resolved target node.
        target_node: NodeId,
        /// Repeated concrete relationship.
        relation: EdgeKind,
    },
    /// A suppression did not identify an existing exact automatic relationship.
    #[error(
        "manualLinks[{index}] cannot suppress missing automatic relationship {source_node:?} -> {target_node:?} ({relation:?})"
    )]
    MissingSuppressionTarget {
        /// Zero-based declaration index.
        index: usize,
        /// Resolved source node.
        source_node: NodeId,
        /// Resolved target node.
        target_node: NodeId,
        /// Requested concrete relationship.
        relation: EdgeKind,
    },
}

/// Graph facts and audit records produced after applying manual declarations.
#[derive(Debug, Clone, PartialEq)]
pub struct ManualLinkResolution {
    /// Automatic edges after exact suppressions and manual replacements.
    pub edges: Vec<Edge>,
    /// Manual evidence emitted for created edges and successful suppressions.
    pub evidence: Vec<Evidence>,
    /// Versioned decisions in deterministic relationship order.
    pub decisions: Vec<LinkDecision>,
}

pub(crate) fn http_link_ambiguity<'a>(
    method: &str,
    path: &str,
    candidates: impl IntoIterator<Item = &'a NodeId>,
) -> HttpLinkAmbiguity {
    let mut candidates = candidates
        .into_iter()
        .map(|candidate| candidate.as_str().to_owned())
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    HttpLinkAmbiguity {
        method: method.to_owned(),
        path: path.to_owned(),
        candidates,
    }
}

pub(crate) fn sort_http_ambiguities(ambiguities: &mut Vec<HttpLinkAmbiguity>) {
    ambiguities.sort_by(|left, right| {
        (&left.method, &left.path, &left.candidates).cmp(&(
            &right.method,
            &right.path,
            &right.candidates,
        ))
    });
    ambiguities.dedup();
}

/// Resolves and applies exact manual relationships to an automatic edge set.
///
/// Endpoint values match only [`NodeId`] text or [`Node::stable_key`] text. A creation replaces an
/// automatic edge with the same source, target, and relation so the resulting edge is backed only
/// by explicit manual evidence. A suppression removes every automatic edge with that exact triple;
/// the optional contract is audit context and never broadens or narrows edge matching.
///
/// # Errors
///
/// Returns [`ManualLinkError`] for invalid declarations, missing or ambiguous endpoints, resolved
/// self-links, duplicate resolved relationships, or suppressions that match no automatic edge.
pub fn resolve_manual_links(
    links: &[ManualLinkConfig],
    nodes: &[Node],
    automatic_edges: &[Edge],
) -> Result<ManualLinkResolution, ManualLinkError> {
    validate_manual_links(links)?;
    let mut resolved = Vec::with_capacity(links.len());
    let mut identities = BTreeMap::new();
    for (index, link) in links.iter().enumerate() {
        let source = resolve_manual_endpoint(nodes, index, ManualLinkEndpoint::From, &link.from)?;
        let target = resolve_manual_endpoint(nodes, index, ManualLinkEndpoint::To, &link.to)?;
        if source == target {
            return Err(ManualLinkError::ResolvedSelfLink {
                index,
                node: source.as_str().to_owned(),
            });
        }
        let identity = (source.clone(), target.clone(), link.relation);
        if let Some(first) = identities.insert(identity, index) {
            return Err(ManualLinkError::DuplicateResolvedLink {
                first,
                duplicate: index,
                source_node: source,
                target_node: target,
                relation: link.relation,
            });
        }
        resolved.push(ResolvedManualLink {
            index,
            link,
            source,
            target,
        });
    }
    resolved.sort_by(|left, right| {
        (
            &left.source,
            &left.target,
            left.link.relation,
            left.link.suppress,
        )
            .cmp(&(
                &right.source,
                &right.target,
                right.link.relation,
                right.link.suppress,
            ))
    });

    let mut edges = automatic_edges.to_vec();
    edges.sort_by(|left, right| left.id.cmp(&right.id));
    let mut evidence = Vec::with_capacity(resolved.len());
    let mut decisions = Vec::with_capacity(resolved.len());
    for item in resolved {
        let manual_evidence = manual_link_evidence(&item);
        let evidence_ref = EvidenceRef {
            id: manual_evidence.id.clone(),
            provenance: Provenance::Manual,
        };
        let status = if item.link.suppress {
            let before = edges.len();
            edges.retain(|edge| !edge_matches(&item, edge));
            let removed = before - edges.len();
            if removed == 0 {
                return Err(ManualLinkError::MissingSuppressionTarget {
                    index: item.index,
                    source_node: item.source,
                    target_node: item.target,
                    relation: item.link.relation,
                });
            }
            LinkStatus::Suppressed
        } else {
            edges.retain(|edge| !edge_matches(&item, edge));
            edges.push(manual_link_edge(&item, &manual_evidence));
            LinkStatus::Confirmed
        };
        decisions.push(manual_link_decision(&item, evidence_ref, status));
        evidence.push(manual_evidence);
    }
    edges.sort_by(|left, right| left.id.cmp(&right.id));
    evidence.sort_by(|left, right| left.id.cmp(&right.id));
    decisions.sort_by(|left, right| {
        (&left.source, &left.target, left.relation, left.status).cmp(&(
            &right.source,
            &right.target,
            right.relation,
            right.status,
        ))
    });
    Ok(ManualLinkResolution {
        edges,
        evidence,
        decisions,
    })
}

#[derive(Debug)]
struct ResolvedManualLink<'a> {
    index: usize,
    link: &'a ManualLinkConfig,
    source: NodeId,
    target: NodeId,
}

fn resolve_manual_endpoint(
    nodes: &[Node],
    index: usize,
    endpoint: ManualLinkEndpoint,
    value: &str,
) -> Result<NodeId, ManualLinkError> {
    let candidates = nodes
        .iter()
        .filter(|node| node.id.as_str() == value || node.stable_key == value)
        .map(|node| node.id.clone())
        .collect::<BTreeSet<_>>();
    match candidates.len() {
        0 => Err(ManualLinkError::MissingEndpoint {
            index,
            endpoint,
            value: value.to_owned(),
        }),
        1 => candidates
            .into_iter()
            .next()
            .ok_or_else(|| ManualLinkError::MissingEndpoint {
                index,
                endpoint,
                value: value.to_owned(),
            }),
        _ => Err(ManualLinkError::AmbiguousEndpoint {
            index,
            endpoint,
            value: value.to_owned(),
            candidates: candidates.into_iter().collect(),
        }),
    }
}

fn edge_matches(link: &ResolvedManualLink<'_>, edge: &Edge) -> bool {
    edge.source == link.source && edge.target == link.target && edge.kind == link.link.relation
}

fn manual_link_edge(link: &ResolvedManualLink<'_>, evidence: &Evidence) -> Edge {
    let key = format!(
        "{}:{:?}:{}",
        link.source.as_str(),
        link.link.relation,
        link.target.as_str()
    );
    Edge {
        id: EdgeId::new(stable_id("edge", &key)),
        source: link.source.clone(),
        target: link.target.clone(),
        kind: link.link.relation,
        confidence: 1.0,
        status: EpistemicStatus::Confirmed,
        evidence: vec![evidence.id.clone()],
    }
}

fn manual_link_evidence(link: &ResolvedManualLink<'_>) -> Evidence {
    let contract = link.link.contract.as_deref().unwrap_or("");
    let key = format!(
        "{}:{}:{:?}:{contract}:{}:{}",
        link.source.as_str(),
        link.target.as_str(),
        link.link.relation,
        link.link.suppress,
        link.link.reason
    );
    Evidence {
        id: EvidenceId::new(stable_id("evidence", &key)),
        repo_id: None,
        file_path: None,
        start_line: None,
        end_line: None,
        extractor: MANUAL_LINK_EXTRACTOR.to_owned(),
        extractor_version: "1.0.0".to_owned(),
        provenance: Provenance::Manual,
        confidence: 1.0,
        observed_at_commit: None,
        content_hash: Some(stable_id("manual-link-declaration", &key)),
        note: Some(link.link.reason.clone()),
    }
}

fn manual_link_decision(
    link: &ResolvedManualLink<'_>,
    evidence: EvidenceRef,
    status: LinkStatus,
) -> LinkDecision {
    let mut reasons = vec![
        link.link.reason.clone(),
        "both endpoints matched an exact node ID or stable key".to_owned(),
    ];
    if let Some(contract) = &link.link.contract {
        reasons.push(format!("declared contract: {contract}"));
    }
    reasons.push(match status {
        LinkStatus::Confirmed => "manual declaration created the exact relationship".to_owned(),
        LinkStatus::Suppressed => {
            "manual declaration removed the exact automatic relationship".to_owned()
        }
        LinkStatus::Ambiguous | LinkStatus::Rejected => {
            "manual declaration was not applied".to_owned()
        }
    });
    LinkDecision {
        source: link.source.clone(),
        target: link.target.clone(),
        relation: link.link.relation,
        matcher: MANUAL_LINK_MATCHER.to_owned(),
        matcher_version: Version::new(1, 0, 0),
        score: 1.0,
        confidence: 1.0,
        reasons,
        rejected_alternatives: Vec::new(),
        evidence: vec![evidence],
        status,
    }
}

#[cfg(test)]
mod tests {

    use code_system_graph_model::{
        Edge, EdgeId, EdgeKind, EpistemicStatus, LinkStatus, Node, NodeId, NodeKind, Provenance, RepoId
    };

    use super::{ManualLinkEndpoint, ManualLinkError, resolve_manual_links};
    use crate::{
        AuthorityMap, HttpConsumerConfig, ManualLinkConfig, extract_openapi, link_http_routes
    };

    fn consumer() -> crate::HttpBoundary {
        crate::HttpBoundary::consumer(
            RepoId::new("repo:web"),
            &HttpConsumerConfig {
                method: "POST".to_owned(),
                path: "/api/orders".to_owned(),
                source: "src/checkout.ts".to_owned(),
            },
        )
    }

    fn provider(repo: &str) -> crate::HttpBoundary {
        let input = r"
openapi: 3.0.3
paths:
  /api/orders:
    post:
      operationId: createOrder
";
        let result = extract_openapi(&RepoId::new(repo), "openapi.yaml", input);
        match result {
            Ok(mut boundaries) => boundaries.remove(0),
            Err(error) => panic!("test fixture must be valid: {error}"),
        }
    }

    fn graph_node(id: &str, stable_key: &str) -> Node {
        Node {
            id: NodeId::new(id),
            kind: NodeKind::Service,
            repo_id: None,
            stable_key: stable_key.to_owned(),
            label: stable_key.to_owned(),
        }
    }

    fn manual_link(from: &str, to: &str, relation: EdgeKind, suppress: bool) -> ManualLinkConfig {
        ManualLinkConfig {
            from: from.to_owned(),
            to: to.to_owned(),
            relation,
            contract: Some("POST /orders".to_owned()),
            reason: "Explicit manual boundary".to_owned(),
            suppress,
        }
    }

    fn automatic_edge(id: &str, source: &str, target: &str, kind: EdgeKind) -> Edge {
        Edge {
            id: EdgeId::new(id),
            source: NodeId::new(source),
            target: NodeId::new(target),
            kind,
            confidence: 0.8,
            status: EpistemicStatus::Inferred,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn link_http_boundaries_should_require_bilateral_evidence() {
        let result = link_http_routes(
            &[consumer(), provider("repo:api")],
            &[],
            &[],
            &AuthorityMap::new(),
        );

        assert_eq!(result.edges[0].evidence.len(), 2);
    }

    #[test]
    fn link_http_boundaries_should_preserve_duplicate_providers_as_ambiguity() {
        let result = link_http_routes(
            &[consumer(), provider("repo:api-a"), provider("repo:api-b")],
            &[],
            &[],
            &AuthorityMap::new(),
        );

        assert_eq!(result.edges, Vec::new());
        assert_eq!(result.ambiguities.len(), 1);
        assert_eq!(result.ambiguities[0].method, "POST");
        assert_eq!(result.ambiguities[0].path, "/api/orders");
        assert_eq!(result.ambiguities[0].candidates.len(), 2);
    }

    #[test]
    fn resolve_manual_links_should_create_confirmed_exact_edge_with_manual_evidence() {
        let nodes = [
            graph_node("node:web", "service:web"),
            graph_node("node:api", "service:api"),
        ];
        let links = [manual_link(
            "node:web",
            "service:api",
            EdgeKind::Consumes,
            false,
        )];

        let result = resolve_manual_links(&links, &nodes, &[])
            .unwrap_or_else(|error| panic!("manual link should resolve: {error}"));

        assert!(
            result.edges.len() == 1
                && result.edges[0].source == NodeId::new("node:web")
                && result.edges[0].target == NodeId::new("node:api")
                && result.edges[0].kind == EdgeKind::Consumes
                && (result.edges[0].confidence - 1.0).abs() < f32::EPSILON
                && result.edges[0].status == EpistemicStatus::Confirmed
                && result.evidence.len() == 1
                && result.evidence[0].provenance == Provenance::Manual
                && result.decisions.len() == 1
                && result.decisions[0].status == LinkStatus::Confirmed
                && (result.decisions[0].score - 1.0).abs() < f32::EPSILON
        );
    }

    #[test]
    fn resolve_manual_links_should_suppress_only_exact_automatic_relationship() {
        let nodes = [
            graph_node("node:web", "service:web"),
            graph_node("node:api", "service:api"),
        ];
        let links = [manual_link(
            "node:web",
            "node:api",
            EdgeKind::CallsRemote,
            true,
        )];
        let automatic = [
            automatic_edge("edge:exact", "node:web", "node:api", EdgeKind::CallsRemote),
            automatic_edge(
                "edge:reverse",
                "node:api",
                "node:web",
                EdgeKind::CallsRemote,
            ),
            automatic_edge("edge:relation", "node:web", "node:api", EdgeKind::Consumes),
        ];

        let result = resolve_manual_links(&links, &nodes, &automatic)
            .unwrap_or_else(|error| panic!("suppression should resolve: {error}"));

        assert!(
            result.edges.len() == 2
                && result
                    .edges
                    .iter()
                    .all(|edge| edge.id.as_str() != "edge:exact")
                && result
                    .edges
                    .iter()
                    .any(|edge| edge.id.as_str() == "edge:reverse")
                && result
                    .edges
                    .iter()
                    .any(|edge| edge.id.as_str() == "edge:relation")
                && result.decisions[0].status == LinkStatus::Suppressed
        );
    }

    #[test]
    fn resolve_manual_links_should_fail_for_missing_endpoint() {
        let nodes = [graph_node("node:web", "service:web")];
        let links = [manual_link(
            "node:web",
            "service:missing",
            EdgeKind::Consumes,
            false,
        )];

        let result = resolve_manual_links(&links, &nodes, &[]);

        assert!(matches!(
            result,
            Err(ManualLinkError::MissingEndpoint {
                endpoint: ManualLinkEndpoint::To,
                value,
                ..
            }) if value == "service:missing"
        ));
    }

    #[test]
    fn resolve_manual_links_should_fail_for_ambiguous_exact_endpoint() {
        let nodes = [
            graph_node("node:web", "service:web"),
            graph_node("node:api-b", "service:shared"),
            graph_node("node:api-a", "service:shared"),
        ];
        let links = [manual_link(
            "node:web",
            "service:shared",
            EdgeKind::Consumes,
            false,
        )];

        let result = resolve_manual_links(&links, &nodes, &[]);

        assert!(matches!(
            result,
            Err(ManualLinkError::AmbiguousEndpoint { candidates, .. })
                if candidates
                    == vec![NodeId::new("node:api-a"), NodeId::new("node:api-b")]
        ));
    }

    #[test]
    fn resolve_manual_links_should_be_independent_of_input_order() {
        let nodes = vec![
            graph_node("node:web", "service:web"),
            graph_node("node:api", "service:api"),
            graph_node("node:worker", "service:worker"),
        ];
        let links = vec![
            manual_link("service:web", "service:api", EdgeKind::CallsRemote, false),
            manual_link("service:worker", "service:api", EdgeKind::Consumes, false),
        ];
        let mut reversed_nodes = nodes.clone();
        reversed_nodes.reverse();
        let mut reversed_links = links.clone();
        reversed_links.reverse();

        let first = resolve_manual_links(&links, &nodes, &[]);
        let second = resolve_manual_links(&reversed_links, &reversed_nodes, &[]);

        assert_eq!(
            first.unwrap_or_else(|error| panic!("first resolution should succeed: {error}")),
            second.unwrap_or_else(|error| panic!("second resolution should succeed: {error}"))
        );
    }
}

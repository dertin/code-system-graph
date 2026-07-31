use std::collections::BTreeMap;

use code_system_graph_model::{
    Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, EvidenceId, Node, NodeId, NodeKind, Provenance, RepoId, stable_id
};

use crate::{
    BoundaryRole, ContractImplementationConfig, HttpBoundary, IntegrationTestConfig, LinkError, normalize_http_path
};

/// Declared cross-language test case and its validated HTTP target.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredTestCase {
    /// Federated test-case node.
    pub node: Node,
    /// Canonical target HTTP method.
    pub method: String,
    /// Canonical target HTTP path.
    pub path: String,
    /// Evidence from the test source declaration.
    pub evidence: Evidence,
}

/// Declared source symbol and the HTTP contract it implements.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredImplementation {
    /// Lightweight source symbol node.
    pub node: Node,
    /// Canonical implemented HTTP method.
    pub method: String,
    /// Canonical implemented HTTP path.
    pub path: String,
    /// Evidence from the source declaration and file fingerprint.
    pub evidence: Evidence,
}

/// Creates a test-case node from strict declared metadata.
#[must_use]
pub fn declared_test_case(
    repo_id: RepoId,
    config: &IntegrationTestConfig,
    content_hash: Option<String>,
) -> DeclaredTestCase {
    let method = config.validates.method.trim().to_ascii_uppercase();
    let path = normalize_http_path(&config.validates.path);
    let stable_key = format!(
        "test:{}:{}:{}:{}:{}",
        repo_id.as_str(),
        config.language.trim().to_ascii_lowercase(),
        config.framework.trim().to_ascii_lowercase(),
        config.path,
        config.name
    );
    let evidence_key = format!("{stable_key}:{method}:{path}");
    DeclaredTestCase {
        node: Node {
            id: NodeId::new(stable_id("node", &stable_key)),
            kind: NodeKind::TestCase,
            repo_id: Some(repo_id.clone()),
            stable_key,
            label: format!("{}/{}::{}", config.language, config.framework, config.name),
        },
        method,
        path,
        evidence: Evidence {
            id: EvidenceId::new(stable_id("evidence", &evidence_key)),
            repo_id: Some(repo_id),
            file_path: Some(config.path.clone()),
            start_line: None,
            end_line: None,
            extractor: "code-system-graph.tests.declared".to_owned(),
            extractor_version: env!("CARGO_PKG_VERSION").to_owned(),
            provenance: Provenance::Declared,
            confidence: 1.0,
            observed_at_commit: None,
            content_hash,
            note: Some(format!(
                "{} test declared with {}",
                config.language, config.framework
            )),
        },
    }
}

/// Creates a lightweight implementation anchor from strict declared metadata.
#[must_use]
pub fn declared_implementation(
    repo_id: RepoId,
    config: &ContractImplementationConfig,
    content_hash: Option<String>,
) -> DeclaredImplementation {
    let method = config.implements.method.trim().to_ascii_uppercase();
    let path = normalize_http_path(&config.implements.path);
    let stable_key = format!(
        "symbol:{}:{}:{}:{}",
        repo_id.as_str(),
        config.language.trim().to_ascii_lowercase(),
        config.path,
        config.symbol
    );
    let evidence_key = format!("{stable_key}:{method}:{path}");
    DeclaredImplementation {
        node: Node {
            id: NodeId::new(stable_id("node", &stable_key)),
            kind: NodeKind::SymbolRef,
            repo_id: Some(repo_id.clone()),
            stable_key,
            label: format!("{}::{}", config.language, config.symbol),
        },
        method,
        path,
        evidence: Evidence {
            id: EvidenceId::new(stable_id("evidence", &evidence_key)),
            repo_id: Some(repo_id),
            file_path: Some(config.path.clone()),
            start_line: None,
            end_line: None,
            extractor: "code-system-graph.implementations.declared".to_owned(),
            extractor_version: env!("CARGO_PKG_VERSION").to_owned(),
            provenance: Provenance::Declared,
            confidence: 1.0,
            observed_at_commit: None,
            content_hash,
            note: Some(format!(
                "{} symbol declared as contract implementation",
                config.language
            )),
        },
    }
}

/// Links declared tests to exact HTTP providers with bilateral evidence.
///
/// Tests with no observed provider remain unlinked instead of inventing a target.
///
/// # Errors
///
/// Returns [`LinkError::AmbiguousProvider`] when multiple providers expose the same target.
pub fn link_declared_tests(
    tests: &[DeclaredTestCase],
    boundaries: &[HttpBoundary],
) -> Result<Vec<Edge>, LinkError> {
    let mut providers: BTreeMap<(&str, &str), Vec<&HttpBoundary>> = BTreeMap::new();
    for provider in boundaries
        .iter()
        .filter(|boundary| boundary.role == BoundaryRole::Provider)
    {
        let candidates = providers
            .entry((&provider.method, &provider.path))
            .or_default();
        if let Some(existing) = candidates
            .iter()
            .position(|candidate| candidate.node.id == provider.node.id)
        {
            if provider.evidence.confidence > candidates[existing].evidence.confidence {
                candidates[existing] = provider;
            }
        } else {
            candidates.push(provider);
        }
    }
    let mut edges = Vec::new();
    for test in tests {
        let Some(candidates) = providers.get(&(test.method.as_str(), test.path.as_str())) else {
            continue;
        };
        if candidates.len() > 1 {
            let mut candidate_ids = candidates
                .iter()
                .map(|candidate| candidate.node.id.as_str().to_owned())
                .collect::<Vec<_>>();
            candidate_ids.sort();
            return Err(LinkError::AmbiguousProvider {
                method: test.method.clone(),
                path: test.path.clone(),
                candidates: candidate_ids,
            });
        }
        let provider = candidates[0];
        let edge_key = format!(
            "{}:validates:{}",
            test.node.id.as_str(),
            provider.node.id.as_str()
        );
        edges.push(Edge {
            id: EdgeId::new(stable_id("edge", &edge_key)),
            source: test.node.id.clone(),
            target: provider.node.id.clone(),
            kind: EdgeKind::Validates,
            confidence: test.evidence.confidence.min(provider.evidence.confidence),
            status: consensus_status(test.evidence.confidence.min(provider.evidence.confidence)),
            evidence: vec![test.evidence.id.clone(), provider.evidence.id.clone()],
        });
    }
    edges.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(edges)
}

/// Links HTTP provider contracts to declared source implementations.
///
/// # Errors
///
/// Returns [`LinkError::AmbiguousProvider`] when a repository exposes duplicate exact providers.
pub fn link_declared_implementations(
    implementations: &[DeclaredImplementation],
    boundaries: &[HttpBoundary],
) -> Result<Vec<Edge>, LinkError> {
    let mut edges = Vec::new();
    for implementation in implementations {
        let candidates = boundaries
            .iter()
            .filter(|boundary| {
                boundary.role == BoundaryRole::Provider
                    && boundary.node.repo_id == implementation.node.repo_id
                    && boundary.method == implementation.method
                    && boundary.path == implementation.path
            })
            .fold(
                BTreeMap::<NodeId, &HttpBoundary>::new(),
                |mut candidates, boundary| {
                    candidates
                        .entry(boundary.node.id.clone())
                        .and_modify(|existing| {
                            if boundary.evidence.confidence > existing.evidence.confidence {
                                *existing = boundary;
                            }
                        })
                        .or_insert(boundary);
                    candidates
                },
            )
            .into_values()
            .collect::<Vec<_>>();
        if candidates.len() > 1 {
            let mut candidate_ids = candidates
                .iter()
                .map(|candidate| candidate.node.id.as_str().to_owned())
                .collect::<Vec<_>>();
            candidate_ids.sort();
            return Err(LinkError::AmbiguousProvider {
                method: implementation.method.clone(),
                path: implementation.path.clone(),
                candidates: candidate_ids,
            });
        }
        let Some(provider) = candidates.first() else {
            continue;
        };
        let edge_key = format!(
            "{}:implemented_by:{}",
            provider.node.id.as_str(),
            implementation.node.id.as_str()
        );
        let confidence = provider
            .evidence
            .confidence
            .min(implementation.evidence.confidence);
        edges.push(Edge {
            id: EdgeId::new(stable_id("edge", &edge_key)),
            source: provider.node.id.clone(),
            target: implementation.node.id.clone(),
            kind: EdgeKind::ImplementedBy,
            confidence,
            status: consensus_status(confidence),
            evidence: vec![
                provider.evidence.id.clone(),
                implementation.evidence.id.clone(),
            ],
        });
    }
    edges.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(edges)
}

fn consensus_status(confidence: f32) -> EpistemicStatus {
    if confidence >= 1.0 {
        EpistemicStatus::Confirmed
    } else {
        EpistemicStatus::Inferred
    }
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{EdgeKind, EpistemicStatus, RepoId};

    use super::{declared_test_case, link_declared_implementations, link_declared_tests};
    use crate::{
        HttpContractConfig, IntegrationTestConfig, extract_openapi, parse_rust_source, source_observations_to_graph
    };

    #[test]
    fn declared_python_test_should_validate_rust_provider_with_bilateral_evidence() {
        let test = declared_test_case(
            RepoId::new("repo:tests"),
            &IntegrationTestConfig {
                name: "test_create_order".to_owned(),
                path: "tests/test_orders.py".to_owned(),
                framework: "pytest".to_owned(),
                language: "python".to_owned(),
                validates: HttpContractConfig {
                    method: "POST".to_owned(),
                    path: "/api/orders".to_owned(),
                },
            },
            Some("content:test".to_owned()),
        );
        let providers = extract_openapi(
            &RepoId::new("repo:rust-api"),
            "openapi.yaml",
            "openapi: 3.1.0\npaths:\n  /api/orders:\n    post: {}\n",
        );
        let result = providers
            .map_err(|error| error.to_string())
            .and_then(|providers| {
                link_declared_tests(&[test], &providers).map_err(|error| error.to_string())
            });

        assert!(matches!(
            result,
            Ok(edges)
                if edges.len() == 1
                    && edges[0].kind == EdgeKind::Validates
                    && edges[0].evidence.len() == 2
        ));
    }

    #[test]
    fn executable_route_should_outweigh_divergent_utoipa_advisory() {
        let repo = RepoId::new("repo:rust-api");
        let observations = parse_rust_source(
            r#"
use actix_web::get;

#[utoipa::path(get, path = "/documented")]
#[get("/runtime")]
async fn handler() {}
"#,
        );
        let facts =
            source_observations_to_graph(&repo, "src/routes.rs", "content:routes", &observations);
        let edges = link_declared_implementations(&facts.implementations, &facts.boundaries)
            .expect("both exact declarations should remain linkable");
        let mut consensus = edges
            .iter()
            .filter_map(|edge| {
                let path = facts
                    .boundaries
                    .iter()
                    .find(|boundary| boundary.node.id == edge.source)?
                    .path
                    .clone();
                Some((path, edge.confidence, edge.status))
            })
            .collect::<Vec<_>>();
        consensus.sort_by(|left, right| left.0.cmp(&right.0));

        assert_eq!(
            consensus,
            vec![
                ("/documented".to_owned(), 0.75, EpistemicStatus::Inferred),
                ("/runtime".to_owned(), 1.0, EpistemicStatus::Confirmed),
            ]
        );
    }
}

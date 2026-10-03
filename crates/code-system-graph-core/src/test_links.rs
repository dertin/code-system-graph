use code_system_graph_model::{
    Evidence, EvidenceId, Node, NodeId, NodeKind, Provenance, RepoId, stable_id
};

use crate::{CallScope, ContractImplementationConfig, IntegrationTestConfig, normalize_http_path};

/// Declared cross-language test case and its validated HTTP target.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredTestCase {
    /// Federated test-case node.
    pub node: Node,
    /// Canonical target HTTP method.
    pub method: String,
    /// Canonical target HTTP path.
    pub path: String,
    /// Repositories allowed to provide the validated operation.
    pub scope: CallScope,
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
        scope: CallScope::Workspace,
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
    let identity = crate::SourceSymbolIdentity::new(
        repo_id.clone(),
        &config.language,
        &config.path,
        &config.symbol,
    );
    let stable_key = identity.stable_key();
    let evidence_key = format!("{stable_key}:{method}:{path}");
    DeclaredImplementation {
        node: identity.node(format!("{}::{}", config.language, config.symbol)),
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

#[cfg(test)]
mod tests {

    use code_system_graph_model::{EdgeKind, EpistemicStatus, RepoId};

    use super::declared_test_case;
    use crate::{
        AuthorityMap, HttpContractConfig, IntegrationTestConfig, extract_openapi, link_http_routes, parse_rust_source, source_observations_to_graph
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
            .map(|providers| {
                link_http_routes(&providers, &[test], &[], &AuthorityMap::new()).edges
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
        let edges = link_http_routes(
            &facts.boundaries,
            &[],
            &facts.implementations,
            &AuthorityMap::new(),
        )
        .edges
        .into_iter()
        .filter(|edge| edge.kind == EdgeKind::ImplementedBy)
        .collect::<Vec<_>>();
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

    #[test]
    fn executable_route_should_merge_matching_utoipa_advisory() {
        let repo = RepoId::new("repo:rust-api");
        let observations = parse_rust_source(
            r#"
use actix_web::get;

#[utoipa::path(get, path = "/payments")]
#[get("/payments")]
async fn handler() {}
"#,
        );
        let facts =
            source_observations_to_graph(&repo, "src/routes.rs", "content:routes", &observations);
        let edges = link_http_routes(
            &facts.boundaries,
            &[],
            &facts.implementations,
            &AuthorityMap::new(),
        )
        .edges
        .into_iter()
        .filter(|edge| edge.kind == EdgeKind::ImplementedBy)
        .collect::<Vec<_>>();

        assert!(matches!(
            edges.as_slice(),
            [edge]
                if edge.kind == EdgeKind::ImplementedBy
                    && (edge.confidence - 1.0).abs() < f32::EPSILON
                    && edge.status == EpistemicStatus::Confirmed
                    && edge.evidence.len() == 3
        ));
    }
}

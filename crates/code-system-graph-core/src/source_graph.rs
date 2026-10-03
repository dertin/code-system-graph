use std::collections::{BTreeMap, BTreeSet};

use code_system_graph_model::{
    Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, EvidenceId, Node, NodeId, NodeKind, Provenance, RepoId, stable_id
};

use crate::{
    BoundaryRole, CallScope, DeclaredImplementation, DeclaredTestCase, HttpBoundary, SourceFramework, SourceLanguage, SourceObservation, SourceRole, SourceSymbolIdentity
};

/// Graph-ready facts derived from one focused Rust or Python source file.
#[derive(Debug, Clone, Default)]
pub struct SourceGraphFacts {
    /// Exact HTTP provider and consumer boundaries.
    pub boundaries: Vec<HttpBoundary>,
    /// Tests paired with exact HTTP calls in the same test symbol.
    pub tests: Vec<DeclaredTestCase>,
    /// Exact provider implementation symbols.
    pub implementations: Vec<DeclaredImplementation>,
    /// Test nodes that have no exact HTTP target in the same symbol.
    pub standalone_test_nodes: Vec<Node>,
    /// Evidence for standalone test nodes.
    pub standalone_test_evidence: Vec<Evidence>,
    /// Factory and statically imported model symbols.
    pub relation_nodes: Vec<Node>,
    /// Direct source-level relationships such as Factory Boy `Meta.model`.
    pub relation_edges: Vec<Edge>,
    /// Evidence for direct source-level relationships.
    pub relation_evidence: Vec<Evidence>,
}

/// Converts exact focused source observations into graph-ready facts.
///
/// Ambiguous and incomplete observations remain in the persisted extractor payload but are not
/// promoted to factual graph nodes or links.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "One deterministic conversion pass keeps HTTP, test, and source relations aligned"
)]
pub fn source_observations_to_graph(
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    observations: &[SourceObservation],
) -> SourceGraphFacts {
    let mut result = SourceGraphFacts::default();
    let confirmed = observations
        .iter()
        .filter(|observation| observation.status == crate::SourceEpistemicStatus::Confirmed)
        .collect::<Vec<_>>();
    for observation in &confirmed {
        match observation.role {
            SourceRole::Provider | SourceRole::Consumer => {
                let (Some(method), Some(path)) =
                    (observation.method.as_deref(), observation.path.as_deref())
                else {
                    continue;
                };
                let boundary = source_boundary(
                    repo_id,
                    source_path,
                    content_hash,
                    observation,
                    method,
                    path,
                );
                if observation.role == SourceRole::Provider
                    && let Some(symbol) = observation.symbol_name.as_deref()
                {
                    result.implementations.push(source_implementation(
                        repo_id,
                        source_path,
                        content_hash,
                        observation,
                        symbol,
                        method,
                        path,
                    ));
                }
                if observation.role == SourceRole::Consumer
                    && let Some(symbol) = observation.symbol_name.as_deref()
                {
                    let identity = SourceSymbolIdentity::new(
                        repo_id.clone(),
                        language_name(observation.language),
                        source_path,
                        symbol,
                    );
                    let caller = identity.node(symbol);
                    let edge_key = format!(
                        "{}:consumes:{}:{}",
                        caller.id.as_str(),
                        boundary.node.id.as_str(),
                        observation.lines.start
                    );
                    result.relation_edges.push(Edge {
                        id: EdgeId::new(stable_id("edge", &edge_key)),
                        source: caller.id.clone(),
                        target: boundary.node.id.clone(),
                        kind: EdgeKind::Consumes,
                        confidence: observation.confidence,
                        status: EpistemicStatus::Confirmed,
                        evidence: vec![boundary.evidence.id.clone()],
                    });
                    result.relation_nodes.push(caller);
                    result.relation_evidence.push(boundary.evidence.clone());
                }
                result.boundaries.push(boundary);
            }
            SourceRole::Factory => {
                let (Some(symbol), Some(related_symbol)) = (
                    observation.symbol_name.as_deref(),
                    observation.related_symbol.as_deref(),
                ) else {
                    continue;
                };
                let (nodes, edge, evidence) = source_factory_relation(
                    repo_id,
                    source_path,
                    content_hash,
                    observation,
                    symbol,
                    related_symbol,
                );
                result.relation_nodes.extend(nodes);
                result.relation_edges.push(edge);
                result.relation_evidence.push(evidence);
            }
            SourceRole::Test | SourceRole::Mount | SourceRole::Call | SourceRole::Client => {}
        }
    }

    let consumers_by_symbol = confirmed
        .iter()
        .filter(|observation| observation.role == SourceRole::Consumer)
        .filter_map(|observation| {
            Some((
                observation.symbol_name.as_deref()?,
                observation.method.as_deref()?,
                observation.path.as_deref()?,
                observation.authority.as_deref(),
                observation.framework.is_in_process_client(),
            ))
        })
        .fold(
            BTreeMap::<&str, BTreeSet<(&str, &str, Option<&str>, bool)>>::new(),
            |mut grouped, (symbol, method, path, authority, in_process)| {
                grouped
                    .entry(symbol)
                    .or_default()
                    .insert((method, path, authority, in_process));
                grouped
            },
        );
    for observation in confirmed
        .iter()
        .filter(|observation| observation.role == SourceRole::Test)
    {
        let Some(symbol) = observation.symbol_name.as_deref() else {
            continue;
        };
        if let Some(targets) = consumers_by_symbol.get(symbol) {
            result
                .tests
                .extend(targets.iter().map(|(method, path, authority, in_process)| {
                    source_test_case(
                        repo_id,
                        source_path,
                        content_hash,
                        observation,
                        symbol,
                        (method, path),
                        call_scope(*authority, *in_process),
                    )
                }));
        } else {
            let (node, evidence) =
                standalone_test(repo_id, source_path, content_hash, observation, symbol);
            result.standalone_test_nodes.push(node);
            result.standalone_test_evidence.push(evidence);
        }
    }
    finish_source_graph(&mut result);
    result
}

fn finish_source_graph(result: &mut SourceGraphFacts) {
    result.boundaries.sort_by(|left, right| {
        (&left.path, &left.method, left.role as u8, &left.node.id).cmp(&(
            &right.path,
            &right.method,
            right.role as u8,
            &right.node.id,
        ))
    });
    result
        .implementations
        .sort_by(|left, right| left.node.id.cmp(&right.node.id));
    result.tests.sort_by(|left, right| {
        (&left.node.id, &left.method, &left.path).cmp(&(&right.node.id, &right.method, &right.path))
    });
    result
        .standalone_test_nodes
        .sort_by(|left, right| left.id.cmp(&right.id));
    result
        .standalone_test_evidence
        .sort_by(|left, right| left.id.cmp(&right.id));
    result
        .relation_nodes
        .sort_by(|left, right| left.id.cmp(&right.id));
    result
        .relation_nodes
        .dedup_by(|left, right| left.id == right.id);
    result
        .relation_edges
        .sort_by(|left, right| left.id.cmp(&right.id));
    result
        .relation_edges
        .dedup_by(|left, right| left.id == right.id);
    result
        .relation_evidence
        .sort_by(|left, right| left.id.cmp(&right.id));
    result
        .relation_evidence
        .dedup_by(|left, right| left.id == right.id);
}

fn source_boundary(
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    observation: &SourceObservation,
    method: &str,
    path: &str,
) -> HttpBoundary {
    let role = match observation.role {
        SourceRole::Provider => BoundaryRole::Provider,
        SourceRole::Consumer => BoundaryRole::Consumer,
        SourceRole::Test
        | SourceRole::Factory
        | SourceRole::Mount
        | SourceRole::Call
        | SourceRole::Client => {
            unreachable!("test and factory observations are not HTTP boundaries")
        }
    };
    let role_key = match role {
        BoundaryRole::Provider => "provider",
        BoundaryRole::Consumer => "consumer",
    };
    let stable_key = format!(
        "http:{}:{role_key}:{method}:{}",
        repo_id.as_str(),
        crate::canonical_route(path)
    );
    let evidence = source_evidence(
        repo_id,
        source_path,
        content_hash,
        observation,
        &format!("{stable_key}:{}", observation.lines.start),
    );
    HttpBoundary {
        node: Node {
            id: NodeId::new(stable_id("node", &stable_key)),
            kind: NodeKind::HttpOperation,
            repo_id: Some(repo_id.clone()),
            stable_key,
            label: format!("{method} {path}"),
        },
        method: method.to_owned(),
        path: path.to_owned(),
        role,
        scope: call_scope(
            observation.authority.as_deref(),
            observation.framework.is_in_process_client(),
        ),
        evidence,
    }
}

fn call_scope(authority: Option<&str>, in_process: bool) -> CallScope {
    if in_process {
        return CallScope::InProcess;
    }
    match authority {
        Some(authority) => CallScope::Authority(authority.to_owned()),
        None => CallScope::Workspace,
    }
}

fn source_implementation(
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    observation: &SourceObservation,
    symbol: &str,
    method: &str,
    path: &str,
) -> DeclaredImplementation {
    let identity = crate::SourceSymbolIdentity::new(
        repo_id.clone(),
        language_name(observation.language),
        source_path,
        symbol,
    );
    let stable_key = identity.stable_key();
    DeclaredImplementation {
        node: identity.node(format!("{}::{symbol}", language_name(observation.language))),
        method: method.to_owned(),
        path: path.to_owned(),
        evidence: source_evidence(
            repo_id,
            source_path,
            content_hash,
            observation,
            &format!(
                "{stable_key}:{}:{}:{}",
                framework_name(observation.framework),
                observation.lines.start,
                observation.lines.end
            ),
        ),
    }
}

fn source_test_case(
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    observation: &SourceObservation,
    symbol: &str,
    (method, path): (&str, &str),
    scope: CallScope,
) -> DeclaredTestCase {
    let stable_key = test_stable_key(repo_id, source_path, observation, symbol);
    DeclaredTestCase {
        node: Node {
            id: NodeId::new(stable_id("node", &stable_key)),
            kind: NodeKind::TestCase,
            repo_id: Some(repo_id.clone()),
            stable_key: stable_key.clone(),
            label: format!(
                "{}/{}::{symbol}",
                language_name(observation.language),
                framework_name(observation.framework)
            ),
        },
        method: method.to_owned(),
        path: path.to_owned(),
        scope,
        evidence: source_evidence(
            repo_id,
            source_path,
            content_hash,
            observation,
            &format!("{stable_key}:{method}:{path}"),
        ),
    }
}

fn standalone_test(
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    observation: &SourceObservation,
    symbol: &str,
) -> (Node, Evidence) {
    let stable_key = test_stable_key(repo_id, source_path, observation, symbol);
    (
        Node {
            id: NodeId::new(stable_id("node", &stable_key)),
            kind: NodeKind::TestCase,
            repo_id: Some(repo_id.clone()),
            stable_key: stable_key.clone(),
            label: format!(
                "{}/{}::{symbol}",
                language_name(observation.language),
                framework_name(observation.framework)
            ),
        },
        source_evidence(repo_id, source_path, content_hash, observation, &stable_key),
    )
}

fn source_factory_relation(
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    observation: &SourceObservation,
    factory_symbol: &str,
    model_symbol: &str,
) -> ([Node; 2], Edge, Evidence) {
    let factory_identity =
        SourceSymbolIdentity::new(repo_id.clone(), "python", source_path, factory_symbol);
    let model_path = observation.related_path.as_deref().unwrap_or(source_path);
    let model_identity =
        SourceSymbolIdentity::new(repo_id.clone(), "python", model_path, model_symbol);
    let factory_node = factory_identity.node(format!("python/factory-boy::{factory_symbol}"));
    let model_node = model_identity.node(format!("python::{model_symbol}"));
    let relation_key = format!(
        "factory-model:{}:{}",
        factory_node.id.as_str(),
        model_node.id.as_str()
    );
    let evidence = source_evidence(
        repo_id,
        source_path,
        content_hash,
        observation,
        &relation_key,
    );
    let edge = Edge {
        id: EdgeId::new(stable_id("edge", &relation_key)),
        source: factory_node.id.clone(),
        target: model_node.id.clone(),
        kind: EdgeKind::Consumes,
        confidence: observation.confidence,
        status: EpistemicStatus::Confirmed,
        evidence: vec![evidence.id.clone()],
    };
    ([factory_node, model_node], edge, evidence)
}

fn test_stable_key(
    repo_id: &RepoId,
    source_path: &str,
    observation: &SourceObservation,
    symbol: &str,
) -> String {
    format!(
        "test:{}:{}:{}:{source_path}:{symbol}",
        repo_id.as_str(),
        language_name(observation.language),
        framework_name(observation.framework)
    )
}

fn source_evidence(
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    observation: &SourceObservation,
    evidence_key: &str,
) -> Evidence {
    Evidence {
        id: EvidenceId::new(stable_id("evidence", evidence_key)),
        repo_id: Some(repo_id.clone()),
        file_path: Some(source_path.to_owned()),
        start_line: Some(observation.lines.start),
        end_line: Some(observation.lines.end),
        extractor: format!(
            "code-system-graph.source.{}",
            language_name(observation.language)
        ),
        extractor_version: "1.0.0".to_owned(),
        provenance: Provenance::Extracted,
        confidence: observation.confidence,
        observed_at_commit: None,
        content_hash: Some(content_hash.to_owned()),
        note: Some(format!(
            "{} {} syntax",
            framework_name(observation.framework),
            role_name(observation.role)
        )),
    }
}

fn language_name(language: SourceLanguage) -> &'static str {
    match language {
        SourceLanguage::JavaScript => "javascript",
        SourceLanguage::TypeScript => "typescript",
        SourceLanguage::Rust => "rust",
        SourceLanguage::Python => "python",
        SourceLanguage::Go => "go",
        SourceLanguage::Java => "java",
    }
}

fn framework_name(framework: SourceFramework) -> &'static str {
    match framework {
        SourceFramework::Fetch => "fetch",
        SourceFramework::Axios => "axios",
        SourceFramework::Express => "express",
        SourceFramework::Fastify => "fastify",
        SourceFramework::NestJs => "nestjs",
        SourceFramework::NextJs => "nextjs",
        SourceFramework::Axum => "axum",
        SourceFramework::ActixWeb => "actix-web",
        SourceFramework::Utoipa => "utoipa",
        SourceFramework::Reqwest => "reqwest",
        SourceFramework::RustTest => "rust-test",
        SourceFramework::TokioTest => "tokio-test",
        SourceFramework::Rstest => "rstest",
        SourceFramework::FastApi => "fastapi",
        SourceFramework::Flask => "flask",
        SourceFramework::Requests => "requests",
        SourceFramework::Httpx => "httpx",
        SourceFramework::AioHttp => "aiohttp",
        SourceFramework::PythonHttpRegistry => "python-http-registry",
        SourceFramework::FactoryBoy => "factory-boy",
        SourceFramework::Pytest => "pytest",
        SourceFramework::Unittest => "unittest",
        SourceFramework::PythonTest => "python-test",
        SourceFramework::GoNetHttp => "net-http",
        SourceFramework::Gin => "gin",
        SourceFramework::Chi => "chi",
        SourceFramework::SpringMvc => "spring-mvc",
        SourceFramework::WebClient => "webclient",
        SourceFramework::Feign => "feign",
        SourceFramework::RestTemplate => "rest-template",
        SourceFramework::TestClient => "test-client",
        SourceFramework::FlaskTestClient => "flask-test-client",
        SourceFramework::Jest => "jest",
        SourceFramework::Vitest => "vitest",
        SourceFramework::Mocha => "mocha",
        SourceFramework::Playwright => "playwright",
        SourceFramework::Supertest => "supertest",
        SourceFramework::GoTest => "go-test",
        SourceFramework::Httptest => "httptest",
        SourceFramework::JUnit => "junit",
        SourceFramework::MockMvc => "mockmvc",
        SourceFramework::RestAssured => "rest-assured",
        SourceFramework::WebTestClient => "webtestclient",
        SourceFramework::TestRestTemplate => "test-rest-template",
        SourceFramework::AxumOneshot => "axum-oneshot",
        SourceFramework::ActixTest => "actix-test",
    }
}

fn role_name(role: SourceRole) -> &'static str {
    match role {
        SourceRole::Provider => "provider",
        SourceRole::Consumer => "consumer",
        SourceRole::Test => "test",
        SourceRole::Factory => "factory",
        SourceRole::Mount => "mount",
        SourceRole::Call => "call",
        SourceRole::Client => "client",
    }
}

#[cfg(test)]
mod tests {

    use code_system_graph_model::{EdgeKind, RepoId};

    use super::source_observations_to_graph;
    use crate::{AuthorityMap, link_http_routes, parse_python_source, parse_rust_source};

    #[test]
    fn source_graph_should_link_python_test_to_rust_handler() {
        let rust = source_observations_to_graph(
            &RepoId::new("repo:api"),
            "src/routes.rs",
            "rust-hash",
            &parse_rust_source(
                r#"use axum::{Router, routing::post}; Router::new().route("/orders", post(create_order));"#,
            ),
        );
        let python = source_observations_to_graph(
            &RepoId::new("repo:tests"),
            "tests/test_orders.py",
            "python-hash",
            &parse_python_source(
                r#"import requests
def test_create_order():
    requests.post("/orders")
"#,
            ),
        );

        let edges = link_http_routes(
            &rust.boundaries,
            &python.tests,
            &rust.implementations,
            &AuthorityMap::new(),
        )
        .edges;
        let kinds = edges.iter().map(|edge| edge.kind).collect::<Vec<_>>();

        assert_eq!(
            (
                kinds
                    .iter()
                    .filter(|kind| **kind == EdgeKind::Validates)
                    .count(),
                kinds
                    .iter()
                    .filter(|kind| **kind == EdgeKind::ImplementedBy)
                    .count(),
            ),
            (1, 1)
        );
    }

    #[test]
    fn source_graph_should_link_factory_boy_factory_to_model() {
        let facts = source_observations_to_graph(
            &RepoId::new("repo:tests"),
            "src/factories/AccountFactory.py",
            "python-hash",
            &parse_python_source(
                r"import factory
from src.clases.Account import Account

class AccountFactory(factory.Factory):
    class Meta:
        model = Account
",
            ),
        );

        assert!(
            facts.relation_nodes.len() == 2
                && facts.relation_edges.len() == 1
                && facts.relation_edges[0].kind == EdgeKind::Consumes
                && facts
                    .relation_nodes
                    .iter()
                    .any(|node| node.label == "python/factory-boy::AccountFactory")
                && facts
                    .relation_nodes
                    .iter()
                    .any(|node| node.stable_key.ends_with("src/clases/Account.py:Account"))
        );
    }
}

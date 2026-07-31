use std::collections::BTreeMap;

use code_system_graph_model::{
    Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, EvidenceId, Node, NodeId, NodeKind, Provenance, RepoId, stable_id
};

use crate::{
    GraphqlDocument, GraphqlLineRange, GraphqlOperationKind, GraphqlResolver, GraphqlTypeDefinition
};

/// Graph facts assembled from a workspace-wide set of GraphQL documents.
#[derive(Debug, Clone, Default)]
pub struct GraphqlGraphFacts {
    /// Provider/consumer operation, resolver, and artifact nodes.
    pub nodes: Vec<Node>,
    /// Exact GraphQL consumer/provider and implementation edges.
    pub edges: Vec<Edge>,
    /// Direct SDL, operation, persisted-manifest, and resolver evidence.
    pub evidence: Vec<Evidence>,
}

struct Provider {
    repo_id: RepoId,
    kind: Option<GraphqlOperationKind>,
    field: String,
    coordinate: String,
    node: Node,
    evidence: Evidence,
}

struct Consumer {
    kind: GraphqlOperationKind,
    root_field: String,
    node: Node,
    evidence: Evidence,
}

/// Converts GraphQL documents into deterministic graph facts and links unambiguous root fields.
///
/// Each input tuple contains repository ID, source path, content hash, and extracted document.
/// Consumers remain unlinked when zero or multiple providers expose the same root coordinate.
#[must_use]
pub fn graphql_documents_to_graph(
    inputs: &[(&RepoId, &str, &str, &GraphqlDocument)],
) -> GraphqlGraphFacts {
    let mut result = GraphqlGraphFacts::default();
    let mut providers = Vec::new();
    let mut consumers = Vec::new();
    let mut resolvers = Vec::new();
    for (repo_id, source_path, content_hash, document) in inputs {
        let artifact = artifact_node(repo_id, source_path);
        let artifact_evidence = graphql_evidence(
            repo_id,
            source_path,
            content_hash,
            GraphqlLineRange { start: 1, end: 1 },
            "GraphQL artifact",
        );
        result.edges.push(edge(
            &repository_node(repo_id).id,
            &artifact.id,
            EdgeKind::Contains,
            vec![artifact_evidence.id.clone()],
        ));
        result.nodes.push(repository_node(repo_id));
        result.nodes.push(artifact);
        result.evidence.push(artifact_evidence);
        append_providers(&mut providers, repo_id, source_path, content_hash, document);
        append_consumers(&mut consumers, repo_id, source_path, content_hash, document);
        resolvers.extend(document.resolvers.iter().map(|resolver| {
            (
                (*repo_id).clone(),
                (*source_path).to_owned(),
                (*content_hash).to_owned(),
                resolver.clone(),
            )
        }));
    }
    link_consumers(&providers, &consumers, &mut result);
    link_resolvers(&providers, &resolvers, &mut result);
    for provider in providers {
        result.nodes.push(provider.node);
        result.evidence.push(provider.evidence);
    }
    for consumer in consumers {
        result.nodes.push(consumer.node);
        result.evidence.push(consumer.evidence);
    }
    finish(&mut result);
    result
}

fn append_providers(
    output: &mut Vec<Provider>,
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    document: &GraphqlDocument,
) {
    for definition in &document.types {
        append_provider_fields(
            output,
            repo_id,
            source_path,
            content_hash,
            definition,
            root_kind(&definition.name),
        );
    }
}

fn append_provider_fields(
    output: &mut Vec<Provider>,
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    definition: &GraphqlTypeDefinition,
    kind: Option<GraphqlOperationKind>,
) {
    for field in &definition.fields {
        let stable_key = format!(
            "graphql:{}:provider:{}:{}",
            repo_id.as_str(),
            definition.name,
            field.name
        );
        output.push(Provider {
            repo_id: repo_id.clone(),
            kind,
            field: field.name.clone(),
            coordinate: field.coordinate.clone(),
            node: Node {
                id: NodeId::new(stable_id("node", &stable_key)),
                kind: NodeKind::GraphqlOperation,
                repo_id: Some(repo_id.clone()),
                stable_key,
                label: field.coordinate.clone(),
            },
            evidence: graphql_evidence(
                repo_id,
                source_path,
                content_hash,
                field.lines,
                "GraphQL SDL root field",
            ),
        });
    }
}

fn append_consumers(
    output: &mut Vec<Consumer>,
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    document: &GraphqlDocument,
) {
    for (index, operation) in document.operations.iter().enumerate() {
        let roots = operation
            .consumed_field_paths
            .iter()
            .filter_map(|path| path.split('.').next())
            .collect::<std::collections::BTreeSet<_>>();
        for root in roots {
            let name = operation
                .name
                .clone()
                .unwrap_or_else(|| format!("anonymous-{index}"));
            let stable_key = format!(
                "graphql:{}:consumer:{source_path}:{}:{name}:{root}",
                repo_id.as_str(),
                operation_kind(operation.kind)
            );
            output.push(Consumer {
                kind: operation.kind,
                root_field: root.to_owned(),
                node: Node {
                    id: NodeId::new(stable_id("node", &stable_key)),
                    kind: NodeKind::GraphqlOperation,
                    repo_id: Some(repo_id.clone()),
                    stable_key,
                    label: format!("{} {name}", operation_kind(operation.kind)),
                },
                evidence: graphql_evidence(
                    repo_id,
                    source_path,
                    content_hash,
                    operation.lines,
                    "GraphQL consumer operation",
                ),
            });
        }
    }
}

fn link_consumers(providers: &[Provider], consumers: &[Consumer], result: &mut GraphqlGraphFacts) {
    let mut candidates: BTreeMap<(GraphqlOperationKind, &str), Vec<&Provider>> = BTreeMap::new();
    for provider in providers {
        if let Some(kind) = provider.kind {
            candidates
                .entry((kind, provider.field.as_str()))
                .or_default()
                .push(provider);
        }
    }
    for consumer in consumers {
        let Some(matches) = candidates.get(&(consumer.kind, consumer.root_field.as_str())) else {
            continue;
        };
        if matches.len() != 1 {
            continue;
        }
        let provider = matches[0];
        result.edges.push(edge(
            &consumer.node.id,
            &provider.node.id,
            EdgeKind::CallsRemote,
            vec![consumer.evidence.id.clone(), provider.evidence.id.clone()],
        ));
    }
}

fn link_resolvers(
    providers: &[Provider],
    resolvers: &[(RepoId, String, String, GraphqlResolver)],
    result: &mut GraphqlGraphFacts,
) {
    for (repo_id, source_path, content_hash, resolver) in resolvers {
        let Some(provider) = providers.iter().find(|provider| {
            provider.repo_id == *repo_id && provider.coordinate == resolver.coordinate
        }) else {
            continue;
        };
        let stable_key = format!(
            "graphql-resolver:{}:{source_path}:{}:{}",
            repo_id.as_str(),
            resolver.coordinate,
            resolver.symbol
        );
        let node = Node {
            id: NodeId::new(stable_id("node", &stable_key)),
            kind: NodeKind::SymbolRef,
            repo_id: Some(repo_id.clone()),
            stable_key,
            label: resolver.symbol.clone(),
        };
        let resolver_evidence = graphql_evidence(
            repo_id,
            source_path,
            content_hash,
            resolver.lines,
            "GraphQL resolver",
        );
        result.edges.push(edge(
            &provider.node.id,
            &node.id,
            EdgeKind::ImplementedBy,
            vec![provider.evidence.id.clone(), resolver_evidence.id.clone()],
        ));
        result.nodes.push(node);
        result.evidence.push(resolver_evidence);
    }
}

fn artifact_node(repo_id: &RepoId, source_path: &str) -> Node {
    let stable_key = format!("graphql-artifact:{}:{source_path}", repo_id.as_str());
    Node {
        id: NodeId::new(stable_id("node", &stable_key)),
        kind: NodeKind::Artifact,
        repo_id: Some(repo_id.clone()),
        stable_key,
        label: source_path.to_owned(),
    }
}

fn repository_node(repo_id: &RepoId) -> Node {
    let stable_key = format!("repository:{}", repo_id.as_str());
    Node {
        id: NodeId::new(stable_id("node", &stable_key)),
        kind: NodeKind::Repository,
        repo_id: Some(repo_id.clone()),
        stable_key,
        label: repo_id.as_str().to_owned(),
    }
}

fn graphql_evidence(
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    lines: GraphqlLineRange,
    note: &str,
) -> Evidence {
    let key = format!(
        "{}:{source_path}:{}:{}:{note}:{content_hash}",
        repo_id.as_str(),
        lines.start,
        lines.end
    );
    Evidence {
        id: EvidenceId::new(stable_id("evidence", &key)),
        repo_id: Some(repo_id.clone()),
        file_path: Some(source_path.to_owned()),
        start_line: Some(lines.start),
        end_line: Some(lines.end),
        extractor: "code-system-graph.graphql".to_owned(),
        extractor_version: "1.0.0".to_owned(),
        provenance: Provenance::Extracted,
        confidence: 1.0,
        observed_at_commit: None,
        content_hash: Some(content_hash.to_owned()),
        note: Some(note.to_owned()),
    }
}

fn edge(source: &NodeId, target: &NodeId, kind: EdgeKind, evidence: Vec<EvidenceId>) -> Edge {
    let key = format!("{}:{kind:?}:{}", source.as_str(), target.as_str());
    Edge {
        id: EdgeId::new(stable_id("edge", &key)),
        source: source.clone(),
        target: target.clone(),
        kind,
        confidence: 1.0,
        status: EpistemicStatus::Confirmed,
        evidence,
    }
}

fn root_kind(name: &str) -> Option<GraphqlOperationKind> {
    match name {
        "Query" => Some(GraphqlOperationKind::Query),
        "Mutation" => Some(GraphqlOperationKind::Mutation),
        "Subscription" => Some(GraphqlOperationKind::Subscription),
        _ => None,
    }
}

fn operation_kind(kind: GraphqlOperationKind) -> &'static str {
    match kind {
        GraphqlOperationKind::Query => "query",
        GraphqlOperationKind::Mutation => "mutation",
        GraphqlOperationKind::Subscription => "subscription",
    }
}

fn finish(result: &mut GraphqlGraphFacts) {
    result.nodes.sort_by(|left, right| left.id.cmp(&right.id));
    result.nodes.dedup_by(|left, right| left.id == right.id);
    result.edges.sort_by(|left, right| left.id.cmp(&right.id));
    result.edges.dedup_by(|left, right| left.id == right.id);
    result
        .evidence
        .sort_by(|left, right| left.id.cmp(&right.id));
    result.evidence.dedup_by(|left, right| left.id == right.id);
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::RepoId;

    use super::graphql_documents_to_graph;
    use crate::extract_graphql_document;

    #[test]
    fn graphql_graph_should_link_exact_consumer_to_single_provider() {
        let provider =
            extract_graphql_document("schema.graphql", "type Query { order(id: ID!): String }");
        let consumer =
            extract_graphql_document("orders.graphql", "query Order { order(id: \"1\") }");
        let provider_repo = RepoId::new("repo:api");
        let consumer_repo = RepoId::new("repo:web");
        let facts =
            provider
                .as_ref()
                .ok()
                .zip(consumer.as_ref().ok())
                .map(|(provider, consumer)| {
                    graphql_documents_to_graph(&[
                        (&provider_repo, "schema.graphql", "provider-hash", provider),
                        (&consumer_repo, "orders.graphql", "consumer-hash", consumer),
                    ])
                });

        assert!(matches!(
            facts,
            Some(facts)
                if facts
                    .edges
                    .iter()
                    .any(|edge| edge.kind == code_system_graph_model::EdgeKind::CallsRemote)
        ));
    }
}

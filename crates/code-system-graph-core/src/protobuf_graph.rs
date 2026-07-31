use std::collections::BTreeMap;

use code_system_graph_model::{
    Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, EvidenceId, Node, NodeId, NodeKind, Provenance, RepoId, stable_id
};

use crate::{ProtoGeneratedMarker, ProtoGeneratedRole, ProtoRpcMethod, ProtobufDocument};

/// Graph facts assembled from protobuf files and exact generated gRPC markers.
#[derive(Debug, Clone, Default)]
pub struct ProtobufGraphFacts {
    /// RPC methods, generated symbols, messages, artifacts, and repositories.
    pub nodes: Vec<Node>,
    /// Exact containment, remote call, and implementation relationships.
    pub edges: Vec<Edge>,
    /// Direct protobuf and generated-source evidence.
    pub evidence: Vec<Evidence>,
}

struct RpcProvider {
    service: String,
    method: String,
    node: Node,
    evidence: Evidence,
}

/// Converts protobuf contracts and generated markers into ambiguity-safe graph facts.
///
/// Generated clients and servers link only when one provider defines the exact
/// package-qualified service and method.
#[must_use]
pub fn protobuf_documents_to_graph(
    inputs: &[(&RepoId, &str, &str, &ProtobufDocument)],
) -> ProtobufGraphFacts {
    let mut result = ProtobufGraphFacts::default();
    let mut providers = Vec::new();
    let mut markers = Vec::new();
    for (repo_id, source_path, content_hash, document) in inputs {
        let repository = repository_node(repo_id);
        let artifact = artifact_node(repo_id, source_path);
        let artifact_evidence =
            protobuf_evidence(repo_id, source_path, content_hash, 1, "protobuf artifact");
        result.edges.push(edge(
            &repository.id,
            &artifact.id,
            EdgeKind::Contains,
            vec![artifact_evidence.id.clone()],
        ));
        result.nodes.extend([repository, artifact.clone()]);
        result.evidence.push(artifact_evidence);
        match document {
            ProtobufDocument::File(file) => {
                for message in &file.messages {
                    let stable_key = format!(
                        "protobuf-message:{}:{}",
                        repo_id.as_str(),
                        message.full_name
                    );
                    let node = Node {
                        id: NodeId::new(stable_id("node", &stable_key)),
                        kind: NodeKind::Artifact,
                        repo_id: Some((*repo_id).clone()),
                        stable_key,
                        label: message.full_name.clone(),
                    };
                    let evidence = protobuf_evidence(
                        repo_id,
                        source_path,
                        content_hash,
                        message.line,
                        "protobuf message",
                    );
                    result.edges.push(edge(
                        &artifact.id,
                        &node.id,
                        EdgeKind::Contains,
                        vec![evidence.id.clone()],
                    ));
                    result.nodes.push(node);
                    result.evidence.push(evidence);
                }
                for service in &file.services {
                    for method in &service.methods {
                        providers.push(rpc_provider(
                            repo_id,
                            source_path,
                            content_hash,
                            &service.full_name,
                            method,
                        ));
                    }
                }
            }
            ProtobufDocument::Generated(generated) => {
                markers.extend(generated.iter().map(|marker| {
                    (
                        (*repo_id).clone(),
                        (*source_path).to_owned(),
                        (*content_hash).to_owned(),
                        marker.clone(),
                    )
                }));
            }
        }
    }
    link_markers(&providers, &markers, &mut result);
    for provider in providers {
        result.nodes.push(provider.node);
        result.evidence.push(provider.evidence);
    }
    finish(&mut result);
    result
}

fn rpc_provider(
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    service: &str,
    method: &ProtoRpcMethod,
) -> RpcProvider {
    let stable_key = format!(
        "rpc:{}:provider:{service}/{}",
        repo_id.as_str(),
        method.name
    );
    RpcProvider {
        service: service.to_owned(),
        method: method.name.clone(),
        node: Node {
            id: NodeId::new(stable_id("node", &stable_key)),
            kind: NodeKind::RpcMethod,
            repo_id: Some(repo_id.clone()),
            stable_key,
            label: format!("{service}/{}", method.name),
        },
        evidence: protobuf_evidence(
            repo_id,
            source_path,
            content_hash,
            method.line,
            "protobuf RPC method",
        ),
    }
}

fn link_markers(
    providers: &[RpcProvider],
    markers: &[(RepoId, String, String, ProtoGeneratedMarker)],
    result: &mut ProtobufGraphFacts,
) {
    let mut candidates: BTreeMap<(&str, &str), Vec<&RpcProvider>> = BTreeMap::new();
    for provider in providers {
        candidates
            .entry((provider.service.as_str(), provider.method.as_str()))
            .or_default()
            .push(provider);
    }
    for (repo_id, source_path, content_hash, marker) in markers {
        let Some(matches) = candidates.get(&(marker.service.as_str(), marker.method.as_str()))
        else {
            continue;
        };
        if matches.len() != 1 {
            continue;
        }
        let provider = matches[0];
        let source = generated_node(repo_id, source_path, marker);
        let source_evidence = protobuf_evidence(
            repo_id,
            source_path,
            content_hash,
            marker.line,
            "generated gRPC method marker",
        );
        match marker.role {
            ProtoGeneratedRole::Client => result.edges.push(edge(
                &source.id,
                &provider.node.id,
                EdgeKind::CallsRemote,
                vec![source_evidence.id.clone(), provider.evidence.id.clone()],
            )),
            ProtoGeneratedRole::Server => result.edges.push(edge(
                &provider.node.id,
                &source.id,
                EdgeKind::ImplementedBy,
                vec![provider.evidence.id.clone(), source_evidence.id.clone()],
            )),
            ProtoGeneratedRole::Unknown => {}
        }
        result.nodes.push(source);
        result.evidence.push(source_evidence);
    }
}

fn generated_node(repo_id: &RepoId, source_path: &str, marker: &ProtoGeneratedMarker) -> Node {
    let stable_key = format!(
        "rpc:{}:generated:{source_path}:{}/{}:{:?}",
        repo_id.as_str(),
        marker.service,
        marker.method,
        marker.role
    );
    Node {
        id: NodeId::new(stable_id("node", &stable_key)),
        kind: NodeKind::SymbolRef,
        repo_id: Some(repo_id.clone()),
        stable_key,
        label: format!("{}/{}", marker.service, marker.method),
    }
}

fn artifact_node(repo_id: &RepoId, source_path: &str) -> Node {
    let stable_key = format!("protobuf-artifact:{}:{source_path}", repo_id.as_str());
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

fn protobuf_evidence(
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    line: u32,
    note: &str,
) -> Evidence {
    let key = format!(
        "{}:{source_path}:{line}:{note}:{content_hash}",
        repo_id.as_str()
    );
    Evidence {
        id: EvidenceId::new(stable_id("evidence", &key)),
        repo_id: Some(repo_id.clone()),
        file_path: Some(source_path.to_owned()),
        start_line: Some(line),
        end_line: Some(line),
        extractor: "code-system-graph.protobuf".to_owned(),
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

fn finish(result: &mut ProtobufGraphFacts) {
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
    use code_system_graph_model::{EdgeKind, RepoId};

    use super::protobuf_documents_to_graph;
    use crate::{
        ProtoGeneratedRole, ProtobufDocument, SourceLanguage, extract_protobuf, parse_protobuf_generated_source
    };

    #[test]
    fn protobuf_graph_should_link_generated_client_to_exact_method() {
        let contract = extract_protobuf(
            "orders.proto",
            r#"
syntax = "proto3";
package commerce.orders.v1;
message Request {}
message Reply {}
service Orders { rpc GetOrder(Request) returns (Reply); }
"#,
        );
        let markers = parse_protobuf_generated_source(
            SourceLanguage::Python,
            "orders_pb2_grpc.py",
            "# Generated by the gRPC Python protocol compiler plugin. DO NOT EDIT!\nchannel.unary_unary(\"/commerce.orders.v1.Orders/GetOrder\")",
        );
        assert_eq!(
            markers.first().map(|marker| marker.role),
            Some(ProtoGeneratedRole::Client)
        );
        let api = RepoId::new("repo:api");
        let worker = RepoId::new("repo:worker");
        let facts = contract.as_ref().ok().map(|contract| {
            let contract = ProtobufDocument::File(Box::new(contract.clone()));
            let generated = ProtobufDocument::Generated(markers);
            protobuf_documents_to_graph(&[
                (&api, "orders.proto", "contract", &contract),
                (&worker, "orders_pb2_grpc.py", "generated", &generated),
            ])
        });

        assert!(matches!(
            facts,
            Some(facts)
                if facts
                    .edges
                    .iter()
                    .any(|edge| edge.kind == EdgeKind::CallsRemote)
        ));
    }
}

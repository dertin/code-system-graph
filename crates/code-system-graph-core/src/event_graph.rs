use code_system_graph_model::{
    Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, EvidenceId, Node, NodeId, NodeKind, Provenance, RepoId, stable_id
};

use crate::{EventBroker, EventDocument, EventObservation, EventRole};

/// Graph facts assembled from workspace-wide event declarations and source observations.
#[derive(Debug, Clone, Default)]
pub struct EventGraphFacts {
    /// Event channels, schemas, source artifacts, and repository nodes.
    pub nodes: Vec<Node>,
    /// Publish, subscribe, delivery, schema, and dead-letter relationships.
    pub edges: Vec<Edge>,
    /// Direct contract and source-call evidence.
    pub evidence: Vec<Evidence>,
}

struct EventEndpoint {
    repo_id: RepoId,
    source_path: String,
    observation: EventObservation,
    evidence: Evidence,
}

/// Converts event documents into deterministic, ambiguity-safe graph facts.
///
/// Source observations without a namespace link to a declared broker/channel only when exactly
/// one namespace candidate exists. Generic source APIs link to a broker-specific declaration only
/// when the channel has one unambiguous declaration across the workspace.
#[must_use]
pub fn event_documents_to_graph(
    inputs: &[(&RepoId, &str, &str, &EventDocument)],
) -> EventGraphFacts {
    let mut result = EventGraphFacts::default();
    let mut declarations = Vec::new();
    let mut endpoints = Vec::new();
    for (repo_id, source_path, content_hash, document) in inputs {
        let repository = repository_node(repo_id);
        let artifact = artifact_node(repo_id, source_path);
        let artifact_evidence =
            event_evidence(repo_id, source_path, content_hash, None, "event artifact");
        result.edges.push(edge(
            &repository.id,
            &artifact.id,
            EdgeKind::Contains,
            vec![artifact_evidence.id.clone()],
        ));
        result.nodes.extend([repository, artifact]);
        result.evidence.push(artifact_evidence);
        for observation in &document.observations {
            if observation.channel.is_none() || observation.incomplete {
                continue;
            }
            let evidence = event_evidence(
                repo_id,
                source_path,
                content_hash,
                Some(observation),
                "event boundary",
            );
            let endpoint = EventEndpoint {
                repo_id: (*repo_id).clone(),
                source_path: (*source_path).to_owned(),
                observation: observation.clone(),
                evidence,
            };
            if observation.language.is_none() || observation.role == EventRole::Declaration {
                declarations.push(endpoint);
            } else {
                endpoints.push(endpoint);
            }
        }
    }
    append_declarations(&declarations, &mut result);
    append_endpoints(&declarations, &endpoints, &mut result);
    finish(&mut result);
    result
}

fn append_declarations(declarations: &[EventEndpoint], result: &mut EventGraphFacts) {
    for declaration in declarations {
        let channel = channel_node(&declaration.observation);
        let artifact = artifact_node(&declaration.repo_id, &declaration.source_path);
        result.edges.push(edge(
            &artifact.id,
            &channel.id,
            match declaration.observation.role {
                EventRole::Publisher => EdgeKind::Publishes,
                EventRole::Subscriber => EdgeKind::Subscribes,
                EventRole::Declaration => EdgeKind::Contains,
            },
            vec![declaration.evidence.id.clone()],
        ));
        append_schema(declaration, &channel, result);
        append_dead_letter(declaration, &channel, result);
        result.nodes.push(channel);
        result.evidence.push(declaration.evidence.clone());
    }
}

fn append_endpoints(
    declarations: &[EventEndpoint],
    endpoints: &[EventEndpoint],
    result: &mut EventGraphFacts,
) {
    for endpoint in endpoints {
        let channel = resolve_channel(declarations, endpoint).map_or_else(
            || channel_node(&endpoint.observation),
            |declaration| channel_node(&declaration.observation),
        );
        let source = source_node(endpoint);
        match endpoint.observation.role {
            EventRole::Publisher => result.edges.push(edge(
                &source.id,
                &channel.id,
                EdgeKind::Publishes,
                vec![endpoint.evidence.id.clone()],
            )),
            EventRole::Subscriber => {
                result.edges.push(edge(
                    &source.id,
                    &channel.id,
                    EdgeKind::Subscribes,
                    vec![endpoint.evidence.id.clone()],
                ));
                result.edges.push(edge(
                    &channel.id,
                    &source.id,
                    EdgeKind::DeliversTo,
                    vec![endpoint.evidence.id.clone()],
                ));
            }
            EventRole::Declaration => {}
        }
        result.nodes.extend([source, channel]);
        result.evidence.push(endpoint.evidence.clone());
    }
}

fn resolve_channel<'a>(
    declarations: &'a [EventEndpoint],
    endpoint: &EventEndpoint,
) -> Option<&'a EventEndpoint> {
    let channel = endpoint.observation.channel.as_deref()?;
    let candidates = declarations
        .iter()
        .filter(|declaration| {
            declaration.observation.channel.as_deref() == Some(channel)
                && (endpoint.observation.broker == EventBroker::Generic
                    || declaration.observation.broker == endpoint.observation.broker)
                && endpoint
                    .observation
                    .namespace
                    .as_ref()
                    .is_none_or(|namespace| {
                        declaration.observation.namespace.as_ref() == Some(namespace)
                    })
        })
        .collect::<Vec<_>>();
    let first = candidates.first().copied()?;
    let identity = channel_node(&first.observation).id;
    candidates
        .iter()
        .all(|candidate| channel_node(&candidate.observation).id == identity)
        .then_some(first)
}

fn append_schema(declaration: &EventEndpoint, channel: &Node, result: &mut EventGraphFacts) {
    let Some(schema) = &declaration.observation.schema else {
        return;
    };
    let Some(name) = schema
        .name
        .as_deref()
        .or(declaration.observation.event_type.as_deref())
    else {
        return;
    };
    let stable_key = format!(
        "event-schema:{:?}:{name}:{}",
        declaration.observation.broker,
        schema.version.as_deref().unwrap_or("")
    );
    let node = Node {
        id: NodeId::new(stable_id("node", &stable_key)),
        kind: NodeKind::EventSchema,
        repo_id: Some(declaration.repo_id.clone()),
        stable_key,
        label: name.to_owned(),
    };
    result.edges.push(edge(
        &channel.id,
        &node.id,
        EdgeKind::Contains,
        vec![declaration.evidence.id.clone()],
    ));
    result.nodes.push(node);
}

fn append_dead_letter(declaration: &EventEndpoint, channel: &Node, result: &mut EventGraphFacts) {
    let Some(dead_letter) = &declaration.observation.dead_letter_channel else {
        return;
    };
    let mut observation = declaration.observation.clone();
    observation.channel = Some(dead_letter.clone());
    let target = channel_node(&observation);
    result.edges.push(edge(
        &channel.id,
        &target.id,
        EdgeKind::CallsRemote,
        vec![declaration.evidence.id.clone()],
    ));
    result.nodes.push(target);
}

fn source_node(endpoint: &EventEndpoint) -> Node {
    let channel = endpoint.observation.channel.as_deref().unwrap_or("dynamic");
    let stable_key = format!(
        "event-source:{}:{}:{:?}:{channel}:{:?}",
        endpoint.repo_id.as_str(),
        endpoint.source_path,
        endpoint.observation.role,
        endpoint.observation.broker
    );
    Node {
        id: NodeId::new(stable_id("node", &stable_key)),
        kind: NodeKind::SymbolRef,
        repo_id: Some(endpoint.repo_id.clone()),
        stable_key,
        label: format!("{} {channel}", endpoint.source_path),
    }
}

fn channel_node(observation: &EventObservation) -> Node {
    let channel = observation.channel.as_deref().unwrap_or("dynamic");
    let stable_key = format!(
        "event:{:?}:{}:{channel}",
        observation.broker,
        observation.namespace.as_deref().unwrap_or("")
    );
    Node {
        id: NodeId::new(stable_id("node", &stable_key)),
        kind: NodeKind::EventChannel,
        repo_id: None,
        stable_key,
        label: channel.to_owned(),
    }
}

fn artifact_node(repo_id: &RepoId, source_path: &str) -> Node {
    let stable_key = format!("event-artifact:{}:{source_path}", repo_id.as_str());
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

fn event_evidence(
    repo_id: &RepoId,
    source_path: &str,
    content_hash: &str,
    observation: Option<&EventObservation>,
    note: &str,
) -> Evidence {
    let line = observation
        .and_then(|observation| observation.evidence.first())
        .map_or(1, |evidence| evidence.line);
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
        extractor: "code-system-graph.events".to_owned(),
        extractor_version: "1.0.0".to_owned(),
        provenance: Provenance::Extracted,
        confidence: observation.map_or(1.0, |observation| observation.confidence),
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

fn finish(result: &mut EventGraphFacts) {
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

    use super::event_documents_to_graph;
    use crate::{SourceLanguage, extract_asyncapi, parse_event_source};

    #[test]
    fn event_graph_should_trace_publisher_through_channel_to_subscriber() {
        let declaration = extract_asyncapi(
            "asyncapi.yaml",
            r"
asyncapi: 2.6.0
info: { title: events, version: 1.0.0 }
servers:
  kafka: { url: kafka:9092, protocol: kafka }
channels:
  orders.created:
    publish: { message: { name: OrderCreated } }
",
        );
        let publisher = parse_event_source(
            SourceLanguage::Rust,
            r#"kafka_producer.send("orders.created", payload);"#,
        );
        let subscriber = parse_event_source(
            SourceLanguage::Python,
            r#"kafka_consumer.subscribe("orders.created")"#,
        );
        let api = RepoId::new("repo:api");
        let worker = RepoId::new("repo:worker");
        let facts = declaration.as_ref().ok().map(|declaration| {
            event_documents_to_graph(&[
                (&api, "asyncapi.yaml", "contract", declaration),
                (&api, "src/lib.rs", "publisher", &publisher),
                (&worker, "worker.py", "subscriber", &subscriber),
            ])
        });

        assert!(matches!(
            facts,
            Some(facts)
                if [EdgeKind::Publishes, EdgeKind::Subscribes, EdgeKind::DeliversTo]
                    .into_iter()
                    .all(|kind| facts.edges.iter().any(|edge| edge.kind == kind))
        ));
    }
}

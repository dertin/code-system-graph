use std::collections::{BTreeMap, BTreeSet};

use code_system_graph_model::{
    Edge, EdgeId, EdgeKind, EpistemicStatus, Evidence, EvidenceId, Node, NodeId, NodeKind, Provenance, RepoId, stable_id
};

use crate::{EventBroker, EventDocument, EventObservation, EventRole};

const EVENT_DELIVERY_EVIDENCE_LIMIT: usize = 16;

#[derive(Clone)]
struct EventObservationAggregate {
    status: EpistemicStatus,
    confidence: f32,
    evidence: BTreeSet<EvidenceId>,
    evidence_truncated: bool,
}

type EventObservationsByChannel = BTreeMap<NodeId, BTreeMap<RepoId, EventObservationAggregate>>;

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

/// Canonical repository-to-repository delivery inferred from observed publisher and subscriber
/// edges that share one exact event-channel identity.
#[derive(Debug, Clone, PartialEq)]
pub struct EventDeliveryProjection {
    /// Shared event channel.
    pub channel_id: NodeId,
    /// Human-readable channel label.
    pub channel_label: String,
    /// Whether broker namespace/cluster identity was observed for the channel.
    pub namespace_known: bool,
    /// Unambiguous repository that publishes the event.
    pub publisher_repository: RepoId,
    /// Unambiguous repository that subscribes to the event.
    pub subscriber_repository: RepoId,
    /// Most conservative epistemic status among the contributing observations.
    pub status: EpistemicStatus,
    /// Lowest confidence among the contributing observations.
    pub confidence: f32,
    /// Union of evidence supporting the contributing observations.
    pub evidence: Vec<EvidenceId>,
    /// Whether additional supporting evidence was omitted by the projection bound.
    pub evidence_truncated: bool,
}

/// Bounded result of projecting repository-to-repository event deliveries.
#[derive(Debug, Clone, PartialEq)]
pub struct EventDeliveryProjectionReport {
    /// Canonical deliveries retained within the caller's limit.
    pub deliveries: Vec<EventDeliveryProjection>,
    /// Exact number of repository/channel pairs available before the limit.
    pub total: usize,
    /// Whether complete deliveries were omitted by the caller's limit.
    pub truncated: bool,
    /// Whether cooperative cancellation stopped projection early.
    pub cancelled: bool,
}

/// Projects canonical event deliveries from exact publish/subscribe channel joins.
///
/// Nodes without a unique repository owner are intentionally excluded. Multiple observations for
/// the same repository pair and channel are merged, retaining conservative status and all evidence.
#[must_use]
pub fn project_event_deliveries<'a>(
    nodes: impl IntoIterator<Item = &'a Node>,
    edges: &[Edge],
    owners: &BTreeMap<NodeId, RepoId>,
) -> Vec<EventDeliveryProjection> {
    project_event_deliveries_bounded(nodes, edges, owners, usize::MAX, || false).deliveries
}

/// Projects event deliveries after first collapsing duplicate observations per repository.
///
/// Aggregating before joining changes the adversarial work from observation-level `P × S` to
/// repository-level pairs. `limit` bounds retained pairs and `cancelled` permits callers with a
/// request deadline to stop long scans without returning unmarked partial data.
#[must_use]
pub fn project_event_deliveries_bounded<'a>(
    nodes: impl IntoIterator<Item = &'a Node>,
    edges: &[Edge],
    owners: &BTreeMap<NodeId, RepoId>,
    limit: usize,
    mut cancelled: impl FnMut() -> bool,
) -> EventDeliveryProjectionReport {
    let channels = nodes
        .into_iter()
        .filter(|node| node.kind == NodeKind::EventChannel)
        .map(|node| {
            (
                node.id.clone(),
                (node.label.clone(), event_channel_has_namespace(node)),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let Some((publishers, subscribers)) =
        aggregate_event_observations(edges, owners, &channels, &mut cancelled)
    else {
        return EventDeliveryProjectionReport {
            deliveries: Vec::new(),
            total: 0,
            truncated: false,
            cancelled: true,
        };
    };
    join_event_observations(publishers, &subscribers, &channels, limit, &mut cancelled)
}

fn aggregate_event_observations(
    edges: &[Edge],
    owners: &BTreeMap<NodeId, RepoId>,
    channels: &BTreeMap<NodeId, (String, bool)>,
    cancelled: &mut impl FnMut() -> bool,
) -> Option<(EventObservationsByChannel, EventObservationsByChannel)> {
    let mut publishers = EventObservationsByChannel::new();
    let mut subscribers = EventObservationsByChannel::new();
    for edge in edges {
        if cancelled() {
            return None;
        }
        let Some(repository) = owners.get(&edge.source) else {
            continue;
        };
        let observations = match edge.kind {
            EdgeKind::Publishes if channels.contains_key(&edge.target) => &mut publishers,
            EdgeKind::Subscribes if channels.contains_key(&edge.target) => &mut subscribers,
            _ => continue,
        };
        let aggregate = observations
            .entry(edge.target.clone())
            .or_default()
            .entry(repository.clone())
            .or_insert_with(|| EventObservationAggregate {
                status: edge.status,
                confidence: edge.confidence,
                evidence: BTreeSet::new(),
                evidence_truncated: false,
            });
        aggregate.status = conservative_status(aggregate.status, edge.status);
        aggregate.confidence = aggregate.confidence.min(edge.confidence);
        for evidence in &edge.evidence {
            aggregate.evidence.insert(evidence.clone());
            if aggregate.evidence.len() > EVENT_DELIVERY_EVIDENCE_LIMIT {
                aggregate.evidence_truncated = true;
                let _ = aggregate.evidence.pop_last();
            }
        }
    }
    Some((publishers, subscribers))
}

fn join_event_observations(
    publishers: EventObservationsByChannel,
    subscribers: &EventObservationsByChannel,
    channels: &BTreeMap<NodeId, (String, bool)>,
    limit: usize,
    cancelled: &mut impl FnMut() -> bool,
) -> EventDeliveryProjectionReport {
    let total = publishers
        .iter()
        .filter_map(|(channel_id, publisher_repositories)| {
            subscribers.get(channel_id).map(|subscriber_repositories| {
                publisher_repositories
                    .len()
                    .saturating_mul(subscriber_repositories.len())
            })
        })
        .fold(0_usize, usize::saturating_add);
    let mut deliveries = Vec::with_capacity(total.min(limit));
    for (channel_id, publisher_repositories) in publishers {
        let Some(subscriber_repositories) = subscribers.get(&channel_id) else {
            continue;
        };
        for (publisher_repository, publisher) in publisher_repositories {
            for (subscriber_repository, subscriber) in subscriber_repositories {
                if cancelled() {
                    return EventDeliveryProjectionReport {
                        deliveries,
                        total,
                        truncated: true,
                        cancelled: true,
                    };
                }
                if deliveries.len() == limit {
                    return EventDeliveryProjectionReport {
                        deliveries,
                        total,
                        truncated: true,
                        cancelled: false,
                    };
                }
                let (channel_label, has_namespace) =
                    channels.get(&channel_id).cloned().unwrap_or_default();
                let mut evidence = publisher
                    .evidence
                    .union(&subscriber.evidence)
                    .take(EVENT_DELIVERY_EVIDENCE_LIMIT + 1)
                    .cloned()
                    .collect::<Vec<_>>();
                let evidence_truncated = publisher.evidence_truncated
                    || subscriber.evidence_truncated
                    || evidence.len() > EVENT_DELIVERY_EVIDENCE_LIMIT;
                evidence.truncate(EVENT_DELIVERY_EVIDENCE_LIMIT);
                let observed_status = conservative_status(publisher.status, subscriber.status);
                deliveries.push(EventDeliveryProjection {
                    channel_label,
                    channel_id: channel_id.clone(),
                    namespace_known: has_namespace,
                    publisher_repository: publisher_repository.clone(),
                    subscriber_repository: subscriber_repository.clone(),
                    status: if has_namespace {
                        observed_status
                    } else {
                        conservative_status(observed_status, EpistemicStatus::Ambiguous)
                    },
                    confidence: if has_namespace {
                        publisher.confidence.min(subscriber.confidence)
                    } else {
                        publisher.confidence.min(subscriber.confidence).min(0.5)
                    },
                    evidence,
                    evidence_truncated,
                });
            }
        }
    }
    EventDeliveryProjectionReport {
        truncated: deliveries.len() < total,
        deliveries,
        total,
        cancelled: false,
    }
}

fn event_channel_has_namespace(node: &Node) -> bool {
    node.stable_key
        .strip_prefix("event:")
        .and_then(|identity| identity.split_once(':'))
        .and_then(|(_, identity)| identity.split_once(':'))
        .is_some_and(|(namespace, _)| !namespace.is_empty())
}

fn conservative_status(left: EpistemicStatus, right: EpistemicStatus) -> EpistemicStatus {
    const fn severity(status: EpistemicStatus) -> u8 {
        match status {
            EpistemicStatus::Confirmed => 0,
            EpistemicStatus::Inferred => 1,
            EpistemicStatus::Ambiguous => 2,
            EpistemicStatus::Stale => 3,
            EpistemicStatus::Incomplete => 4,
        }
    }
    if severity(left) >= severity(right) {
        left
    } else {
        right
    }
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
    use std::collections::BTreeMap;

    use code_system_graph_model::{
        Edge, EdgeId, EdgeKind, EpistemicStatus, EvidenceId, Node, NodeId, NodeKind, RepoId
    };

    use super::{
        event_documents_to_graph, project_event_deliveries, project_event_deliveries_bounded
    };
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

    #[test]
    fn event_delivery_projection_merges_duplicate_observations_conservatively() {
        let channel = Node {
            id: NodeId::new("channel"),
            kind: NodeKind::EventChannel,
            repo_id: None,
            stable_key: "event:Kafka:cluster-a:orders".to_owned(),
            label: "orders.created".to_owned(),
        };
        let owners = BTreeMap::from([
            (NodeId::new("publisher"), RepoId::new("repo:api")),
            (NodeId::new("subscriber"), RepoId::new("repo:worker")),
        ]);
        let event_edge = |id: &str,
                          source: &str,
                          kind: EdgeKind,
                          status: EpistemicStatus,
                          confidence: f32,
                          evidence: &str| Edge {
            id: EdgeId::new(id),
            source: NodeId::new(source),
            target: NodeId::new("channel"),
            kind,
            confidence,
            status,
            evidence: vec![EvidenceId::new(evidence)],
        };
        let edges = vec![
            event_edge(
                "publish-a",
                "publisher",
                EdgeKind::Publishes,
                EpistemicStatus::Confirmed,
                0.9,
                "evidence:a",
            ),
            event_edge(
                "publish-b",
                "publisher",
                EdgeKind::Publishes,
                EpistemicStatus::Inferred,
                0.7,
                "evidence:b",
            ),
            event_edge(
                "subscribe",
                "subscriber",
                EdgeKind::Subscribes,
                EpistemicStatus::Confirmed,
                0.8,
                "evidence:c",
            ),
        ];

        let projections = project_event_deliveries(&[channel], &edges, &owners);

        assert_eq!(projections.len(), 1);
        assert_eq!(projections[0].status, EpistemicStatus::Inferred);
        assert!((projections[0].confidence - 0.7).abs() < f32::EPSILON);
        assert_eq!(projections[0].evidence.len(), 3);
    }

    #[test]
    fn event_delivery_projection_aggregates_before_a_bounded_repository_join() {
        let channel = Node {
            id: NodeId::new("channel"),
            kind: NodeKind::EventChannel,
            repo_id: None,
            stable_key: "event:Kafka:cluster-a:shared".to_owned(),
            label: "shared.event".to_owned(),
        };
        let mut owners = BTreeMap::new();
        let mut edges = Vec::new();
        for index in 0..100 {
            for (role, kind) in [
                ("publisher", EdgeKind::Publishes),
                ("subscriber", EdgeKind::Subscribes),
            ] {
                let source = format!("{role}:{index}");
                owners.insert(
                    NodeId::new(&source),
                    RepoId::new(format!("repo:{role}:{index}")),
                );
                edges.push(Edge {
                    id: EdgeId::new(format!("edge:{role}:{index}")),
                    source: NodeId::new(source),
                    target: NodeId::new("channel"),
                    kind,
                    confidence: 1.0,
                    status: EpistemicStatus::Confirmed,
                    evidence: Vec::new(),
                });
            }
        }

        let mut cancellation_checks = 0_usize;
        let report = project_event_deliveries_bounded(&[channel], &edges, &owners, 32, || {
            cancellation_checks += 1;
            false
        });

        assert_eq!(report.total, 10_000);
        assert_eq!(report.deliveries.len(), 32);
        assert!(report.truncated);
        assert!(!report.cancelled);
        assert!(cancellation_checks <= edges.len() + report.deliveries.len() + 1);
    }

    #[test]
    fn event_delivery_without_namespace_is_ambiguous_and_evidence_is_bounded() {
        let channel = Node {
            id: NodeId::new("channel"),
            kind: NodeKind::EventChannel,
            repo_id: None,
            stable_key: "event:Kafka::orders".to_owned(),
            label: "orders".to_owned(),
        };
        let owners = BTreeMap::from([
            (NodeId::new("publisher"), RepoId::new("repo:api")),
            (NodeId::new("subscriber"), RepoId::new("repo:worker")),
        ]);
        let evidence = (0..100)
            .map(|index| EvidenceId::new(format!("evidence:{index:03}")))
            .collect::<Vec<_>>();
        let edges = vec![
            Edge {
                id: EdgeId::new("publish"),
                source: NodeId::new("publisher"),
                target: NodeId::new("channel"),
                kind: EdgeKind::Publishes,
                confidence: 1.0,
                status: EpistemicStatus::Confirmed,
                evidence: evidence.clone(),
            },
            Edge {
                id: EdgeId::new("subscribe"),
                source: NodeId::new("subscriber"),
                target: NodeId::new("channel"),
                kind: EdgeKind::Subscribes,
                confidence: 1.0,
                status: EpistemicStatus::Confirmed,
                evidence,
            },
        ];

        let deliveries = project_event_deliveries(&[channel], &edges, &owners);

        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].status, EpistemicStatus::Ambiguous);
        assert!((deliveries[0].confidence - 0.5).abs() < f32::EPSILON);
        assert_eq!(
            deliveries[0].evidence.len(),
            super::EVENT_DELIVERY_EVIDENCE_LIMIT
        );
        assert!(deliveries[0].evidence_truncated);
    }
}

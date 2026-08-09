//! Conservative extraction of event contracts from `AsyncAPI` and source boundaries.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

type Mapping = serde_json::Map<String, Value>;
use thiserror::Error;

use crate::SourceLanguage;

const MAX_ASYNCAPI_BYTES: usize = 4 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_OBSERVATIONS: usize = 1_024;
const MAX_SCHEMA_FIELDS: usize = 256;
const MAX_EVIDENCE_TEXT_CHARS: usize = 160;
const MAX_ERROR_CHARS: usize = 512;
const MAX_REFERENCE_DEPTH: usize = 16;

/// Event transport family identified by direct configuration or source evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventBroker {
    /// Apache Kafka or a protocol-compatible Kafka service.
    Kafka,
    /// `RabbitMQ` or another explicitly AMQP-backed `RabbitMQ` deployment.
    RabbitMq,
    /// Amazon Simple Notification Service.
    AwsSns,
    /// Amazon Simple Queue Service.
    AwsSqs,
    /// NATS or `JetStream`.
    Nats,
    /// Google Cloud Pub/Sub.
    GooglePubSub,
    /// A literal publish/subscribe API without broker-specific evidence.
    Generic,
}

/// Repository role represented by an event observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventRole {
    /// Code or a contract sends events to a channel.
    Publisher,
    /// Code or a contract receives events from a channel.
    Subscriber,
    /// A channel is declared without an associated send or receive operation.
    Declaration,
}

/// Delivery guarantee explicitly stated by a contract or source call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliverySemantics {
    /// A message may be lost but is not intentionally redelivered.
    AtMostOnce,
    /// A message may be redelivered until it is acknowledged.
    AtLeastOnce,
    /// The declaration explicitly claims exactly-once processing or delivery.
    ExactlyOnce,
    /// The declaration explicitly describes best-effort delivery.
    BestEffort,
}

/// Bounded, source-free evidence identifying where a fact was observed.
///
/// `text` is an extractor-generated label, not a copy of source code or a message payload.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventEvidenceLine {
    /// One-based line containing the direct evidence.
    pub line: u32,
    /// Bounded description of the recognized declaration.
    pub text: String,
}

/// One top-level field from a statically visible event payload schema.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EventSchemaField {
    /// Field name exactly represented by the schema.
    pub name: String,
    /// Declared scalar, object, array, or referenced type when available.
    pub field_type: Option<String>,
    /// Whether the containing schema lists this field as required.
    pub required: bool,
}

/// Bounded schema metadata attached to an event observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventSchemaDefinition {
    /// Message or schema name when explicitly declared.
    pub name: Option<String>,
    /// Schema version when explicitly declared.
    pub version: Option<String>,
    /// `AsyncAPI` schema format or media type when explicitly declared.
    pub schema_format: Option<String>,
    /// Deterministically ordered top-level payload fields.
    pub fields: Vec<EventSchemaField>,
}

/// One conservative publisher, subscriber, or channel declaration observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventObservation {
    /// Broker family supported by direct evidence.
    pub broker: EventBroker,
    /// Publisher, subscriber, or declaration role.
    pub role: EventRole,
    /// Exact channel, topic, queue, or subject; absent for dynamic expressions.
    pub channel: Option<String>,
    /// Exact server namespace when represented by a non-templated literal.
    pub namespace: Option<String>,
    /// Exact transport protocol when represented by the contract.
    pub protocol: Option<String>,
    /// Event or message name when statically visible.
    pub event_type: Option<String>,
    /// Bounded payload schema metadata.
    pub schema: Option<EventSchemaDefinition>,
    /// Exact partition key declaration when statically visible.
    pub partition_key: Option<String>,
    /// Exact routing key declaration when statically visible.
    pub routing_key: Option<String>,
    /// Explicit delivery guarantee; never inferred from broker defaults.
    pub delivery_semantics: Option<DeliverySemantics>,
    /// Exact dead-letter topic, queue, or channel when explicitly visible.
    pub dead_letter_channel: Option<String>,
    /// Source language for source-boundary observations.
    pub language: Option<SourceLanguage>,
    /// Direct, bounded evidence lines without source or payload bodies.
    pub evidence: Vec<EventEvidenceLine>,
    /// Normalized confidence from zero to one.
    pub confidence: f32,
    /// Whether missing or dynamic information prevents a complete exact fact.
    pub incomplete: bool,
    /// Deterministically ordered human-readable limitations.
    pub warnings: Vec<String>,
}

/// Deterministic event facts and extraction limitations for one input.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EventDocument {
    /// Repository-relative source path for declarative contracts.
    pub source_path: Option<String>,
    /// Contract family, currently `asyncapi`, when applicable.
    pub specification: Option<String>,
    /// Exact `AsyncAPI` version when applicable.
    pub specification_version: Option<String>,
    /// Sorted and deduplicated event observations.
    pub observations: Vec<EventObservation>,
    /// Sorted and deduplicated document-level limitations.
    pub warnings: Vec<String>,
    /// Whether truncation or ambiguity prevents complete extraction.
    pub incomplete: bool,
}

/// Failure to safely parse an `AsyncAPI` event contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[non_exhaustive]
pub enum EventExtractionError {
    /// Input exceeds the fixed extraction budget.
    #[error("event contract `{source_path}` exceeds the {limit_bytes}-byte input limit")]
    InputTooLarge {
        /// Repository-relative source path.
        source_path: String,
        /// Maximum accepted input size.
        limit_bytes: usize,
    },
    /// YAML or JSON syntax is malformed.
    #[error("invalid AsyncAPI document `{source_path}`: {message}")]
    InvalidSyntax {
        /// Repository-relative source path.
        source_path: String,
        /// Bounded parser explanation.
        message: String,
    },
    /// The top-level value is not an object.
    #[error("AsyncAPI document `{source_path}` must contain a top-level object")]
    InvalidRoot {
        /// Repository-relative source path.
        source_path: String,
    },
    /// The `asyncapi` field is missing, dynamic, or outside supported major versions.
    #[error("unsupported AsyncAPI version in `{source_path}`: {version}")]
    UnsupportedVersion {
        /// Repository-relative source path.
        source_path: String,
        /// Bounded observed value or `missing`.
        version: String,
    },
    /// A mandatory `AsyncAPI` collection has an invalid shape.
    #[error("invalid AsyncAPI structure in `{source_path}`: {message}")]
    InvalidStructure {
        /// Repository-relative source path.
        source_path: String,
        /// Bounded structural explanation.
        message: String,
    },
}

/// Extracts static event facts from an `AsyncAPI` 2.x or 3.x YAML/JSON document.
///
/// The extractor resolves only local component references. Templated channel addresses, server
/// namespaces, unresolved references, and conflicting server protocols remain explicit warnings.
/// Message payloads are reduced to bounded schema metadata and are never retained as source.
///
/// # Errors
///
/// Returns [`EventExtractionError`] for oversized or malformed input, a non-object root, an
/// unsupported `AsyncAPI` version, or object collections with invalid shapes.
pub fn extract_asyncapi(
    source_path: &str,
    input: &str,
) -> Result<EventDocument, EventExtractionError> {
    if input.len() > MAX_ASYNCAPI_BYTES {
        return Err(EventExtractionError::InputTooLarge {
            source_path: source_path.to_owned(),
            limit_bytes: MAX_ASYNCAPI_BYTES,
        });
    }
    let root = parse_asyncapi_value(source_path, input)?;
    let root_map = root
        .as_object()
        .ok_or_else(|| EventExtractionError::InvalidRoot {
            source_path: source_path.to_owned(),
        })?;
    let version = mapping_string(root_map, "asyncapi").ok_or_else(|| {
        EventExtractionError::UnsupportedVersion {
            source_path: source_path.to_owned(),
            version: "missing".to_owned(),
        }
    })?;
    let major = if version.starts_with("2.") {
        2
    } else if version.starts_with("3.") {
        3
    } else {
        return Err(EventExtractionError::UnsupportedVersion {
            source_path: source_path.to_owned(),
            version: bounded_text(version, MAX_ERROR_CHARS),
        });
    };

    let mut context = AsyncApiContext::new(source_path, input, &root, version);
    if major == 2 {
        extract_asyncapi_v2(&mut context)?;
    } else {
        extract_asyncapi_v3(&mut context)?;
    }
    Ok(context.finish())
}

fn parse_asyncapi_value(source_path: &str, input: &str) -> Result<Value, EventExtractionError> {
    let result = if input.trim_start().starts_with(['{', '[']) {
        serde_json::from_str(input).map_err(|error| error.to_string())
    } else {
        crate::yaml::from_str(input).map_err(|error| error.to_string())
    };
    result.map_err(|message| EventExtractionError::InvalidSyntax {
        source_path: source_path.to_owned(),
        message: bounded_text(&message, MAX_ERROR_CHARS),
    })
}

/// Extracts exact literal event API calls from one supported source language.
///
/// Recognition is intentionally narrow. A recognized publisher or subscriber call with a
/// computed channel is emitted with `channel = None`, zero confidence, and an explicit warning.
/// At most one mebibyte and 1,024 observations are inspected and returned.
#[must_use]
pub fn parse_event_source(language: SourceLanguage, input: &str) -> EventDocument {
    let (source, truncated) = bounded_source(input);
    let sanitized = sanitize_source(language, source);
    let mut document = EventDocument::default();
    if truncated {
        document.incomplete = true;
        document.warnings.push(format!(
            "source input truncated at {MAX_SOURCE_BYTES} bytes"
        ));
    }
    if input.contains('\0') {
        document.incomplete = true;
        document
            .warnings
            .push("source input contains a NUL byte".to_owned());
    }

    for index in 0..sanitized.len() {
        if document.observations.len() >= MAX_OBSERVATIONS {
            document.incomplete = true;
            document.warnings.push(format!(
                "observations truncated at {MAX_OBSERVATIONS} items"
            ));
            break;
        }
        let Some(candidate) = source_candidate(&sanitized, index) else {
            continue;
        };
        let Some(recognition) = recognize_source_call(&candidate) else {
            continue;
        };
        let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        append_source_observations(
            &mut document.observations,
            language,
            line_number,
            &candidate,
            recognition,
        );
    }
    finish_document(&mut document);
    document
}

struct AsyncApiContext<'a> {
    source_path: &'a str,
    input: &'a str,
    root: &'a Value,
    version: &'a str,
    observations: Vec<EventObservation>,
    warnings: Vec<String>,
    incomplete: bool,
    server: ServerFacts,
}

impl<'a> AsyncApiContext<'a> {
    fn new(source_path: &'a str, input: &'a str, root: &'a Value, version: &'a str) -> Self {
        let (server, warnings) = global_server_facts(root);
        Self {
            source_path,
            input,
            root,
            version,
            observations: Vec::new(),
            incomplete: !warnings.is_empty(),
            warnings,
            server,
        }
    }

    fn push(&mut self, mut observation: EventObservation) {
        if self.observations.len() >= MAX_OBSERVATIONS {
            self.incomplete = true;
            self.warnings.push(format!(
                "observations truncated at {MAX_OBSERVATIONS} items"
            ));
            return;
        }
        normalize_observation(&mut observation);
        self.observations.push(observation);
    }

    fn finish(self) -> EventDocument {
        let mut document = EventDocument {
            source_path: Some(self.source_path.to_owned()),
            specification: Some("asyncapi".to_owned()),
            specification_version: Some(self.version.to_owned()),
            observations: self.observations,
            warnings: self.warnings,
            incomplete: self.incomplete,
        };
        finish_document(&mut document);
        document
    }
}

#[derive(Debug, Clone, Default)]
struct ServerFacts {
    broker: Option<EventBroker>,
    protocol: Option<String>,
    namespace: Option<String>,
    ambiguous: bool,
}

fn extract_asyncapi_v2(context: &mut AsyncApiContext<'_>) -> Result<(), EventExtractionError> {
    let Some(channels) = value_get(context.root, "channels") else {
        context.incomplete = true;
        context
            .warnings
            .push("AsyncAPI document has no channels object".to_owned());
        return Ok(());
    };
    let channel_map =
        channels
            .as_object()
            .ok_or_else(|| EventExtractionError::InvalidStructure {
                source_path: context.source_path.to_owned(),
                message: "`channels` must be an object".to_owned(),
            })?;
    for (channel_key, channel_item) in channel_map {
        let raw_channel = channel_key.as_str();
        let resolved_item = resolve_local(context.root, channel_item);
        let item = resolved_item.value;
        let mut emitted = false;
        for (operation_name, role) in [
            ("publish", EventRole::Publisher),
            ("subscribe", EventRole::Subscriber),
        ] {
            let Some(operation) = value_get(item, operation_name) else {
                continue;
            };
            emitted = true;
            append_asyncapi_operation(
                context,
                raw_channel,
                role,
                operation,
                Some(item),
                &resolved_item.warnings,
            );
        }
        if !emitted {
            let mut observation = asyncapi_observation(
                context,
                raw_channel,
                EventRole::Declaration,
                None,
                Some(item),
                None,
            );
            observation.warnings.extend(resolved_item.warnings);
            context.push(observation);
        }
    }
    Ok(())
}

fn extract_asyncapi_v3(context: &mut AsyncApiContext<'_>) -> Result<(), EventExtractionError> {
    let channel_map = value_get(context.root, "channels")
        .and_then(Value::as_object)
        .ok_or_else(|| EventExtractionError::InvalidStructure {
            source_path: context.source_path.to_owned(),
            message: "`channels` must be an object".to_owned(),
        })?;
    let mut used_channels = BTreeSet::new();
    if let Some(operations) = value_get(context.root, "operations") {
        let operation_map =
            operations
                .as_object()
                .ok_or_else(|| EventExtractionError::InvalidStructure {
                    source_path: context.source_path.to_owned(),
                    message: "`operations` must be an object".to_owned(),
                })?;
        for (operation_name, operation) in operation_map {
            let operation_label = operation_name.as_str();
            let resolved_operation = resolve_local(context.root, operation);
            let Some(action) =
                value_get(resolved_operation.value, "action").and_then(Value::as_str)
            else {
                context.incomplete = true;
                context.warnings.push(format!(
                    "AsyncAPI operation `{operation_label}` has no literal action"
                ));
                continue;
            };
            let role = match action {
                "send" => EventRole::Publisher,
                "receive" => EventRole::Subscriber,
                other => {
                    context.incomplete = true;
                    context.warnings.push(format!(
                        "AsyncAPI operation `{operation_label}` uses unsupported action `{other}`"
                    ));
                    continue;
                }
            };
            let Some(channel_value) = value_get(resolved_operation.value, "channel") else {
                context.incomplete = true;
                context.warnings.push(format!(
                    "AsyncAPI operation `{operation_label}` has no channel reference"
                ));
                continue;
            };
            let channel_key = reference_component_name(channel_value, "channels");
            let resolved_channel = resolve_local(context.root, channel_value);
            let channel = value_get(resolved_channel.value, "address").and_then(literal_string);
            let raw_channel = channel.as_deref().unwrap_or("{dynamic-channel}");
            if let Some(key) = channel_key {
                used_channels.insert(key);
            }
            let mut reference_warnings = resolved_operation.warnings;
            reference_warnings.extend(resolved_channel.warnings);
            append_asyncapi_operation(
                context,
                raw_channel,
                role,
                resolved_operation.value,
                Some(resolved_channel.value),
                &reference_warnings,
            );
        }
    }

    for (channel_key, channel_item) in channel_map {
        let name = channel_key.as_str();
        if used_channels.contains(name) {
            continue;
        }
        let resolved = resolve_local(context.root, channel_item);
        append_asyncapi_declaration(context, resolved);
    }
    Ok(())
}

fn append_asyncapi_declaration(context: &mut AsyncApiContext<'_>, resolved: ResolvedValue<'_>) {
    let raw_channel = value_get(resolved.value, "address")
        .and_then(literal_string)
        .unwrap_or_else(|| "{dynamic-channel}".to_owned());
    let messages = operation_messages(context.root, resolved.value, Some(resolved.value));
    if messages.is_empty() {
        let mut observation = asyncapi_observation(
            context,
            &raw_channel,
            EventRole::Declaration,
            None,
            Some(resolved.value),
            None,
        );
        observation.warnings.extend(resolved.warnings);
        context.push(observation);
        return;
    }
    for message in messages {
        let mut observation = asyncapi_observation(
            context,
            &raw_channel,
            EventRole::Declaration,
            None,
            Some(resolved.value),
            Some(message.value),
        );
        observation
            .warnings
            .extend(resolved.warnings.iter().cloned());
        observation.warnings.extend(message.warnings);
        context.push(observation);
    }
}

fn append_asyncapi_operation(
    context: &mut AsyncApiContext<'_>,
    raw_channel: &str,
    role: EventRole,
    operation: &Value,
    channel_item: Option<&Value>,
    reference_warnings: &[String],
) {
    let resolved_operation = resolve_local(context.root, operation);
    let messages = operation_messages(context.root, resolved_operation.value, channel_item);
    if messages.is_empty() {
        let mut observation = asyncapi_observation(
            context,
            raw_channel,
            role,
            Some(resolved_operation.value),
            channel_item,
            None,
        );
        observation
            .warnings
            .extend(reference_warnings.iter().cloned());
        observation
            .warnings
            .extend(resolved_operation.warnings.iter().cloned());
        observation.incomplete = true;
        observation
            .warnings
            .push("operation has no statically resolvable message".to_owned());
        context.push(observation);
        return;
    }
    for message in messages {
        let mut observation = asyncapi_observation(
            context,
            raw_channel,
            role,
            Some(resolved_operation.value),
            channel_item,
            Some(message.value),
        );
        observation
            .warnings
            .extend(reference_warnings.iter().cloned());
        observation
            .warnings
            .extend(resolved_operation.warnings.iter().cloned());
        observation.warnings.extend(message.warnings);
        context.push(observation);
    }
}

struct ResolvedValue<'a> {
    value: &'a Value,
    warnings: Vec<String>,
}

fn operation_messages<'a>(
    root: &'a Value,
    operation: &'a Value,
    channel_item: Option<&'a Value>,
) -> Vec<ResolvedValue<'a>> {
    let message_value = value_get(operation, "message").or_else(|| {
        value_get(operation, "messages")
            .or_else(|| channel_item.and_then(|channel| value_get(channel, "messages")))
    });
    let Some(message_value) = message_value else {
        return Vec::new();
    };
    let mut output = Vec::new();
    collect_messages(root, message_value, &mut output);
    output
}

fn collect_messages<'a>(root: &'a Value, value: &'a Value, output: &mut Vec<ResolvedValue<'a>>) {
    if let Some(items) = value_get(value, "oneOf").and_then(Value::as_array) {
        for item in items {
            output.push(resolve_local(root, item));
        }
        return;
    }
    if let Some(sequence) = value.as_array() {
        for item in sequence {
            output.push(resolve_local(root, item));
        }
        return;
    }
    if let Some(mapping) = value.as_object()
        && !mapping.contains_key("$ref")
        && !mapping.contains_key("payload")
        && !mapping.contains_key("name")
    {
        for item in mapping.values() {
            output.push(resolve_local(root, item));
        }
        return;
    }
    output.push(resolve_local(root, value));
}

fn asyncapi_observation(
    context: &AsyncApiContext<'_>,
    raw_channel: &str,
    role: EventRole,
    operation: Option<&Value>,
    channel_item: Option<&Value>,
    message: Option<&Value>,
) -> EventObservation {
    let mut warnings = Vec::new();
    if context.server.ambiguous {
        warnings.push("server selection or namespace is ambiguous".to_owned());
    }
    let channel = literal_channel(raw_channel);
    if channel.is_none() {
        warnings.push("templated or empty channel is not an exact channel".to_owned());
    }
    let protocol = context.server.protocol.clone();
    let broker = context
        .server
        .broker
        .or_else(|| {
            protocol
                .as_deref()
                .map(broker_from_protocol)
                .filter(|broker| *broker != EventBroker::Generic)
        })
        .or_else(|| channel_item.and_then(broker_from_bindings))
        .or_else(|| operation.and_then(broker_from_bindings))
        .or_else(|| message.and_then(broker_from_bindings))
        .unwrap_or(EventBroker::Generic);
    let event_type = message.and_then(message_name);
    let schema = message.and_then(|value| schema_definition(context.root, value, &mut warnings));
    let partition_key = exact_metadata(
        [message, operation, channel_item],
        &["partitionKey", "partition_key", "x-partition-key"],
    );
    let routing_key = exact_metadata(
        [operation, channel_item, message],
        &["routingKey", "routing_key", "x-routing-key"],
    );
    let delivery_semantics = [operation, channel_item, message]
        .into_iter()
        .flatten()
        .find_map(explicit_delivery_semantics);
    let dead_letter_channel = exact_metadata(
        [operation, channel_item, message],
        &[
            "deadLetterChannel",
            "deadLetterQueue",
            "deadLetterTopic",
            "dead_letter_channel",
            "x-dead-letter-channel",
        ],
    );
    let evidence = evidence_for_token(
        context.input,
        channel.as_deref().unwrap_or(raw_channel),
        &format!("AsyncAPI {role:?} channel"),
    );
    EventObservation {
        broker,
        role,
        channel,
        namespace: context.server.namespace.clone(),
        protocol,
        event_type,
        schema,
        partition_key,
        routing_key,
        delivery_semantics,
        dead_letter_channel,
        language: None,
        evidence: vec![evidence],
        confidence: if warnings.is_empty() { 1.0 } else { 0.0 },
        incomplete: !warnings.is_empty(),
        warnings,
    }
}

fn schema_definition(
    root: &Value,
    message: &Value,
    warnings: &mut Vec<String>,
) -> Option<EventSchemaDefinition> {
    let payload = value_get(message, "payload")?;
    let resolved = resolve_local(root, payload);
    warnings.extend(resolved.warnings);
    let schema = resolved.value;
    let name = message_name(message).or_else(|| reference_name(payload));
    let version = exact_string_recursive(
        message,
        &["schemaVersion", "schema_version", "x-schema-version"],
        3,
    )
    .or_else(|| {
        exact_string_recursive(
            schema,
            &["schemaVersion", "schema_version", "x-schema-version"],
            2,
        )
    });
    let schema_format = value_get(message, "schemaFormat")
        .and_then(literal_string)
        .or_else(|| value_get(message, "contentType").and_then(literal_string))
        .or_else(|| value_get(schema, "$schema").and_then(literal_string));
    let required = value_get(schema, "required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    let mut fields = Vec::new();
    if let Some(properties) = value_get(schema, "properties").and_then(Value::as_object) {
        for (field_name, field_schema) in properties {
            if fields.len() >= MAX_SCHEMA_FIELDS {
                warnings.push(format!(
                    "payload fields truncated at {MAX_SCHEMA_FIELDS} items"
                ));
                break;
            }
            let field_name = field_name.as_str();
            let resolved_field = resolve_local(root, field_schema);
            warnings.extend(resolved_field.warnings);
            fields.push(EventSchemaField {
                name: field_name.to_owned(),
                field_type: schema_type(resolved_field.value)
                    .or_else(|| reference_name(field_schema)),
                required: required.contains(field_name),
            });
        }
    }
    fields.sort();
    fields.dedup();
    Some(EventSchemaDefinition {
        name,
        version,
        schema_format,
        fields,
    })
}

fn schema_type(value: &Value) -> Option<String> {
    value_get(value, "type")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            value_get(value, "format")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
}

fn message_name(message: &Value) -> Option<String> {
    value_get(message, "name")
        .and_then(literal_string)
        .or_else(|| value_get(message, "title").and_then(literal_string))
        .or_else(|| reference_name(message))
}

fn global_server_facts(root: &Value) -> (ServerFacts, Vec<String>) {
    let Some(servers) = value_get(root, "servers").and_then(Value::as_object) else {
        return (ServerFacts::default(), Vec::new());
    };
    let mut protocols = BTreeSet::new();
    let mut namespaces = BTreeSet::new();
    let mut warnings = Vec::new();
    for server in servers.values() {
        let resolved = resolve_local(root, server);
        warnings.extend(resolved.warnings);
        if let Some(protocol) = value_get(resolved.value, "protocol").and_then(literal_string) {
            protocols.insert(protocol.to_ascii_lowercase());
        }
        let namespace = server_namespace(resolved.value);
        if let Some(namespace) = namespace {
            namespaces.insert(namespace);
        } else if value_get(resolved.value, "url").is_some()
            || value_get(resolved.value, "host").is_some()
        {
            warnings
                .push("templated server namespace was not promoted to an exact value".to_owned());
        }
    }
    let protocol = unique_value(&protocols);
    let namespace = unique_value(&namespaces);
    if protocols.len() > 1 {
        warnings.push("multiple server protocols make broker selection ambiguous".to_owned());
    }
    if namespaces.len() > 1 {
        warnings.push("multiple server namespaces make namespace selection ambiguous".to_owned());
    }
    let broker = protocol
        .as_deref()
        .map(broker_from_protocol)
        .filter(|broker| *broker != EventBroker::Generic);
    let ambiguous = !warnings.is_empty();
    (
        ServerFacts {
            broker,
            protocol,
            namespace,
            ambiguous,
        },
        warnings,
    )
}

fn server_namespace(server: &Value) -> Option<String> {
    if let Some(namespace) = value_get(server, "namespace").and_then(literal_string) {
        return literal_namespace(&namespace);
    }
    if let Some(url) = value_get(server, "url").and_then(literal_string) {
        return literal_namespace(&url);
    }
    let host = value_get(server, "host").and_then(literal_string)?;
    let pathname = value_get(server, "pathname")
        .and_then(literal_string)
        .unwrap_or_default();
    literal_namespace(&format!("{host}{pathname}"))
}

fn unique_value(values: &BTreeSet<String>) -> Option<String> {
    (values.len() == 1)
        .then(|| values.iter().next().cloned())
        .flatten()
}

fn literal_namespace(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.contains(['{', '}'])
        || trimmed.contains("${")
        || trimmed.chars().any(char::is_whitespace)
    {
        return None;
    }
    let without_scheme = trimmed
        .split_once("://")
        .map_or(trimmed, |(_, remainder)| remainder);
    let without_credentials = without_scheme
        .rsplit_once('@')
        .map_or(without_scheme, |(_, remainder)| remainder);
    let namespace = without_credentials
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .trim_end_matches('/');
    (!namespace.is_empty()).then(|| namespace.to_owned())
}

fn broker_from_protocol(protocol: &str) -> EventBroker {
    match protocol.to_ascii_lowercase().as_str() {
        "kafka" | "kafka-secure" => EventBroker::Kafka,
        "amqp" | "amqps" => EventBroker::RabbitMq,
        "sns" | "aws-sns" => EventBroker::AwsSns,
        "sqs" | "aws-sqs" => EventBroker::AwsSqs,
        "nats" | "nats-secure" => EventBroker::Nats,
        "googlepubsub" | "google-pubsub" | "gcp-pubsub" | "pubsub" => EventBroker::GooglePubSub,
        _ => EventBroker::Generic,
    }
}

fn broker_from_bindings(value: &Value) -> Option<EventBroker> {
    let bindings = value_get(value, "bindings")?.as_object()?;
    for key in bindings.keys() {
        let broker = match key.to_ascii_lowercase().as_str() {
            "kafka" => Some(EventBroker::Kafka),
            "amqp" => Some(EventBroker::RabbitMq),
            "sns" => Some(EventBroker::AwsSns),
            "sqs" => Some(EventBroker::AwsSqs),
            "nats" => Some(EventBroker::Nats),
            "googlepubsub" | "google-pubsub" | "pubsub" => Some(EventBroker::GooglePubSub),
            _ => None,
        };
        if broker.is_some() {
            return broker;
        }
    }
    None
}

fn explicit_delivery_semantics(value: &Value) -> Option<DeliverySemantics> {
    let raw = exact_string_recursive(
        value,
        &[
            "deliverySemantics",
            "delivery_semantics",
            "deliveryGuarantee",
            "x-delivery-semantics",
        ],
        4,
    )?;
    parse_delivery_semantics(&raw)
}

fn parse_delivery_semantics(value: &str) -> Option<DeliverySemantics> {
    let canonical = value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect::<String>();
    match canonical.as_str() {
        "atmostonce" => Some(DeliverySemantics::AtMostOnce),
        "atleastonce" => Some(DeliverySemantics::AtLeastOnce),
        "exactlyonce" => Some(DeliverySemantics::ExactlyOnce),
        "besteffort" => Some(DeliverySemantics::BestEffort),
        _ => None,
    }
}

fn exact_metadata<const N: usize>(values: [Option<&Value>; N], keys: &[&str]) -> Option<String> {
    values
        .into_iter()
        .flatten()
        .find_map(|value| exact_string_recursive(value, keys, 4))
}

fn exact_string_recursive(value: &Value, keys: &[&str], depth: usize) -> Option<String> {
    if depth == 0 {
        return None;
    }
    let mapping = value.as_object()?;
    for key in keys {
        if let Some(found) = mapping.get(*key)
            && let Some(literal) = literal_string(found)
        {
            return Some(literal);
        }
    }
    mapping
        .values()
        .find_map(|child| exact_string_recursive(child, keys, depth - 1))
}

fn resolve_local<'a>(root: &'a Value, value: &'a Value) -> ResolvedValue<'a> {
    let mut current = value;
    let mut warnings = Vec::new();
    let mut visited = BTreeSet::new();
    for _ in 0..MAX_REFERENCE_DEPTH {
        let Some(reference) = value_get(current, "$ref").and_then(Value::as_str) else {
            return ResolvedValue {
                value: current,
                warnings,
            };
        };
        if !reference.starts_with("#/") {
            warnings.push(format!(
                "external reference `{}` was not resolved",
                bounded_text(reference, MAX_EVIDENCE_TEXT_CHARS)
            ));
            return ResolvedValue {
                value: current,
                warnings,
            };
        }
        if !visited.insert(reference.to_owned()) {
            warnings.push(format!(
                "cyclic local reference `{}` was not resolved",
                bounded_text(reference, MAX_EVIDENCE_TEXT_CHARS)
            ));
            return ResolvedValue {
                value: current,
                warnings,
            };
        }
        let Some(next) = json_pointer(root, reference) else {
            warnings.push(format!(
                "unresolved local reference `{}`",
                bounded_text(reference, MAX_EVIDENCE_TEXT_CHARS)
            ));
            return ResolvedValue {
                value: current,
                warnings,
            };
        };
        current = next;
    }
    warnings.push(format!(
        "local reference depth exceeded {MAX_REFERENCE_DEPTH}"
    ));
    ResolvedValue {
        value: current,
        warnings,
    }
}

fn json_pointer<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let mut current = root;
    for component in reference.strip_prefix("#/")?.split('/') {
        let decoded = component.replace("~1", "/").replace("~0", "~");
        current = current.as_object()?.get(&decoded)?;
    }
    Some(current)
}

fn reference_component_name(value: &Value, component: &str) -> Option<String> {
    let reference = value_get(value, "$ref")?.as_str()?;
    let prefix = format!("#/{component}/");
    reference
        .strip_prefix(&prefix)
        .map(|name| name.replace("~1", "/").replace("~0", "~"))
}

fn reference_name(value: &Value) -> Option<String> {
    value_get(value, "$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .map(|name| name.replace("~1", "/").replace("~0", "~"))
}

fn value_get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object()?.get(key)
}

fn mapping_string<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a str> {
    mapping.get(key).and_then(Value::as_str)
}

fn literal_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn literal_channel(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty() && !trimmed.contains(['{', '}']) && !trimmed.contains("${"))
        .then(|| trimmed.to_owned())
}

fn evidence_for_token(input: &str, token: &str, label: &str) -> EventEvidenceLine {
    let line = input
        .lines()
        .position(|source| source.contains(token))
        .and_then(|index| u32::try_from(index + 1).ok())
        .unwrap_or(1);
    EventEvidenceLine {
        line,
        text: bounded_text(label, MAX_EVIDENCE_TEXT_CHARS),
    }
}

#[derive(Debug, Clone, Copy)]
struct SourceRecognition {
    broker: EventBroker,
    role: EventRole,
    call: &'static str,
}

fn recognize_source_call(statement: &str) -> Option<SourceRecognition> {
    let masked = mask_string_contents(statement);
    if looks_like_function_declaration(&masked) {
        return None;
    }
    let compact = compact_lowercase(&masked);
    if let Some(role) = recognize_sns(&compact) {
        return Some(SourceRecognition {
            broker: EventBroker::AwsSns,
            role,
            call: "AWS SNS",
        });
    }
    if let Some(role) = recognize_sqs(&compact) {
        return Some(SourceRecognition {
            broker: EventBroker::AwsSqs,
            role,
            call: "AWS SQS",
        });
    }
    if let Some(role) = recognize_google_pubsub(&compact) {
        return Some(SourceRecognition {
            broker: EventBroker::GooglePubSub,
            role,
            call: "Google Pub/Sub",
        });
    }
    if let Some(role) = recognize_rabbitmq(&compact) {
        return Some(SourceRecognition {
            broker: EventBroker::RabbitMq,
            role,
            call: "RabbitMQ",
        });
    }
    if let Some(role) = recognize_kafka(&compact) {
        return Some(SourceRecognition {
            broker: EventBroker::Kafka,
            role,
            call: "Kafka",
        });
    }
    if let Some(role) = recognize_nats(&compact) {
        return Some(SourceRecognition {
            broker: EventBroker::Nats,
            role,
            call: "NATS",
        });
    }
    recognize_generic(&compact).map(|role| SourceRecognition {
        broker: EventBroker::Generic,
        role,
        call: "generic pub/sub",
    })
}

fn recognize_sns(source: &str) -> Option<EventRole> {
    let identified = source.contains("topicarn")
        || source.contains("topic_arn")
        || source.contains("sns.")
        || source.contains("snsclient");
    if !identified {
        return None;
    }
    if contains_call(source, &["createtopic", "create_topic"]) {
        Some(EventRole::Declaration)
    } else if contains_call(source, &["publish", "send"]) {
        Some(EventRole::Publisher)
    } else if contains_call(source, &["subscribe"]) {
        Some(EventRole::Subscriber)
    } else {
        None
    }
}

fn recognize_sqs(source: &str) -> Option<EventRole> {
    let identified = source.contains("queueurl")
        || source.contains("queue_url")
        || source.contains("sqs.")
        || source.contains("sqsclient")
        || source.contains("sendmessage")
        || source.contains("send_message")
        || source.contains("receivemessage")
        || source.contains("receive_message");
    if !identified {
        return None;
    }
    if contains_call(source, &["createqueue", "create_queue"]) {
        Some(EventRole::Declaration)
    } else if contains_call(source, &["sendmessage", "send_message"]) {
        Some(EventRole::Publisher)
    } else if contains_call(
        source,
        &[
            "receivemessage",
            "receive_message",
            "startmessagepoller",
            "start_message_poller",
            "sqslistener",
        ],
    ) {
        Some(EventRole::Subscriber)
    } else {
        None
    }
}

fn recognize_google_pubsub(source: &str) -> Option<EventRole> {
    let identified = source.contains("pubsub")
        || source.contains("publisherclient")
        || source.contains("subscriberclient")
        || source.contains("topicpath")
        || source.contains("topic_path")
        || source.contains("subscriptionpath")
        || source.contains("subscription_path");
    if !identified {
        return None;
    }
    if contains_call(
        source,
        &[
            "createtopic",
            "create_topic",
            "createsubscription",
            "create_subscription",
        ],
    ) {
        Some(EventRole::Declaration)
    } else if contains_call(source, &["publish", "publishmessage", "publish_message"]) {
        Some(EventRole::Publisher)
    } else if contains_call(
        source,
        &[
            "subscribe",
            "pull",
            "streamingpull",
            "streaming_pull",
            "receive",
            "pubsubsubscription",
        ],
    ) {
        Some(EventRole::Subscriber)
    } else {
        None
    }
}

fn recognize_rabbitmq(source: &str) -> Option<EventRole> {
    let identified = source.contains("rabbit")
        || source.contains("amqp")
        || source.contains("basicpublish")
        || source.contains("basic_publish")
        || source.contains("basicconsume")
        || source.contains("basic_consume")
        || source.contains("queuedeclare")
        || source.contains("queue_declare")
        || source.contains("exchangedeclare")
        || source.contains("exchange_declare");
    if !identified {
        return None;
    }
    if contains_call(
        source,
        &[
            "queuedeclare",
            "queue_declare",
            "exchangedeclare",
            "exchange_declare",
            "assertqueue",
            "assertexchange",
        ],
    ) {
        Some(EventRole::Declaration)
    } else if contains_call(
        source,
        &["basicpublish", "basic_publish", "publish", "send"],
    ) {
        Some(EventRole::Publisher)
    } else if contains_call(
        source,
        &[
            "basicconsume",
            "basic_consume",
            "consume",
            "get",
            "rabbitlistener",
        ],
    ) {
        Some(EventRole::Subscriber)
    } else {
        None
    }
}

fn recognize_kafka(source: &str) -> Option<EventRole> {
    let identified = source.contains("kafka")
        || source.contains("kafkatemplate")
        || source.contains("kafkaproducer")
        || source.contains("kafkaconsumer")
        || source.contains("baserecord::to")
        || source.contains("futurerecord::to")
        || source.contains("newtopic")
        || (source.contains("producer.") && source.contains("topic"))
        || (source.contains("consumer.") && source.contains("subscribe"));
    if !identified {
        return None;
    }
    if contains_call(source, &["newtopic", "createtopics", "create_topics"]) {
        Some(EventRole::Declaration)
    } else if contains_call(
        source,
        &[
            "send",
            "publish",
            "produce",
            "writemessages",
            "write_messages",
        ],
    ) {
        Some(EventRole::Publisher)
    } else if contains_call(
        source,
        &[
            "subscribe",
            "subscribetopics",
            "subscribe_topics",
            "assign",
            "poll",
            "kafkalistener",
        ],
    ) {
        Some(EventRole::Subscriber)
    } else {
        None
    }
}

fn recognize_nats(source: &str) -> Option<EventRole> {
    let identified = source.contains("nats")
        || source.contains("jetstream")
        || source.starts_with("nc.")
        || source.contains(" nc.")
        || source.starts_with("js.")
        || source.contains(" js.");
    if !identified {
        return None;
    }
    if contains_call(
        source,
        &["addstream", "add_stream", "createstream", "create_stream"],
    ) {
        Some(EventRole::Declaration)
    } else if contains_call(source, &["publish", "request"]) {
        Some(EventRole::Publisher)
    } else if contains_call(source, &["subscribe", "queuesubscribe", "queue_subscribe"]) {
        Some(EventRole::Subscriber)
    } else {
        None
    }
}

fn recognize_generic(source: &str) -> Option<EventRole> {
    if contains_call(
        source,
        &[
            "declarechannel",
            "declare_channel",
            "createchannel",
            "create_channel",
        ],
    ) {
        Some(EventRole::Declaration)
    } else if contains_call(source, &["publish", "publisher.publish"]) {
        Some(EventRole::Publisher)
    } else if contains_call(source, &["subscribe", "subscriber.subscribe"]) {
        Some(EventRole::Subscriber)
    } else {
        None
    }
}

fn contains_call(source: &str, names: &[&str]) -> bool {
    names.iter().any(|name| {
        let needle = format!("{name}(");
        let mut offset = 0;
        while let Some(relative) = source[offset..].find(&needle) {
            let position = offset + relative;
            if !source[..position]
                .chars()
                .next_back()
                .is_some_and(is_identifier_character)
            {
                return true;
            }
            offset = position.saturating_add(needle.len());
            if offset >= source.len() {
                break;
            }
        }
        false
    })
}

fn looks_like_function_declaration(source: &str) -> bool {
    let trimmed = source.trim_start().to_ascii_lowercase();
    ["fn ", "def ", "func ", "function "]
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
        || ["public ", "private ", "protected ", "static "]
            .iter()
            .any(|prefix| {
                trimmed.starts_with(prefix)
                    && trimmed
                        .split_once('(')
                        .is_some_and(|(head, _)| !head.contains(['.', '=']))
            })
}

fn append_source_observations(
    output: &mut Vec<EventObservation>,
    language: SourceLanguage,
    line: u32,
    statement: &str,
    recognition: SourceRecognition,
) {
    let channels = source_channels(statement, recognition);
    let partition_key = literal_after_named(statement, &["partitionKey", "partition_key", "key"]);
    let routing_key = literal_after_named(statement, &["routingKey", "routing_key"]);
    let delivery_semantics = literal_after_named(
        statement,
        &[
            "deliverySemantics",
            "delivery_semantics",
            "deliveryGuarantee",
        ],
    )
    .and_then(|value| parse_delivery_semantics(&value));
    let dead_letter_channel = literal_after_named(
        statement,
        &[
            "deadLetterChannel",
            "dead_letter_channel",
            "deadLetterQueue",
            "dead_letter_queue",
            "deadLetterTopic",
            "dead_letter_topic",
            "dlq",
        ],
    );
    let event_type = literal_after_named(statement, &["eventType", "event_type", "messageType"]);
    let channels = if channels.is_empty() {
        vec![None]
    } else {
        channels.into_iter().map(Some).collect()
    };
    for channel in channels {
        let mut warnings = Vec::new();
        if channel.is_none() {
            warnings
                .push("dynamic channel expression was not promoted to an exact value".to_owned());
        }
        let exact = channel.is_some();
        output.push(EventObservation {
            broker: recognition.broker,
            role: recognition.role,
            channel,
            namespace: None,
            protocol: None,
            event_type: event_type.clone(),
            schema: None,
            partition_key: partition_key.clone(),
            routing_key: routing_key.clone(),
            delivery_semantics,
            dead_letter_channel: dead_letter_channel.clone(),
            language: Some(language),
            evidence: vec![EventEvidenceLine {
                line,
                text: bounded_text(
                    &format!("{} {:?} literal call", recognition.call, recognition.role),
                    MAX_EVIDENCE_TEXT_CHARS,
                ),
            }],
            confidence: if exact { 1.0 } else { 0.0 },
            incomplete: !exact,
            warnings,
        });
    }
}

fn source_channels(statement: &str, recognition: SourceRecognition) -> Vec<String> {
    let named_keys: &[&str] = match recognition.broker {
        EventBroker::Kafka => &["topic", "topics", "Topic"],
        EventBroker::RabbitMq => {
            if recognition.role == EventRole::Publisher {
                &["exchange", "exchangeName", "exchange_name"]
            } else {
                &["queue", "queueName", "queue_name"]
            }
        }
        EventBroker::AwsSns => &["TopicArn", "topicArn", "topic_arn"],
        EventBroker::AwsSqs => &["QueueUrl", "queueUrl", "queue_url"],
        EventBroker::Nats => &["subject", "subjects"],
        EventBroker::GooglePubSub => {
            if recognition.role == EventRole::Subscriber {
                &["subscription", "subscriptionName", "subscription_name"]
            } else {
                &["topic", "topicName", "topic_name"]
            }
        }
        EventBroker::Generic => &["channel", "topic", "queue", "subject"],
    };
    let mut channels = literals_after_named(statement, named_keys);
    if channels.is_empty() {
        channels = call_argument_literals(statement, recognition, 0);
    }
    if recognition.broker == EventBroker::RabbitMq
        && recognition.role == EventRole::Publisher
        && channels.first().is_some_and(String::is_empty)
    {
        channels = call_argument_literals(statement, recognition, 1);
    }
    channels.retain(|channel| !channel.is_empty());
    channels.sort();
    channels.dedup();
    channels
}

fn call_argument_literals(
    statement: &str,
    recognition: SourceRecognition,
    argument_index: usize,
) -> Vec<String> {
    let markers: &[&str] = match (recognition.broker, recognition.role) {
        (EventBroker::Kafka, EventRole::Publisher) => &[
            "BaseRecord::to",
            "FutureRecord::to",
            "kafkaTemplate.send",
            ".send",
            ".produce",
            "WriteMessages",
        ],
        (EventBroker::Kafka, EventRole::Subscriber) => &[
            ".subscribe",
            "SubscribeTopics",
            "subscribe_topics",
            "KafkaListener",
        ],
        (EventBroker::RabbitMq, EventRole::Publisher) => {
            &["basic_publish", "basicPublish", ".publish", ".Publish"]
        }
        (EventBroker::RabbitMq, EventRole::Subscriber) => &[
            "basic_consume",
            "basicConsume",
            ".consume",
            ".Consume",
            "RabbitListener",
        ],
        (EventBroker::RabbitMq, EventRole::Declaration) => {
            &["queue_declare", "queueDeclare", "assertQueue"]
        }
        (EventBroker::AwsSns | EventBroker::GooglePubSub, EventRole::Publisher) => {
            &[".publish", "publish"]
        }
        (EventBroker::AwsSns, EventRole::Subscriber) => &[".subscribe", "subscribe"],
        (EventBroker::AwsSns, EventRole::Declaration) => &["create_topic", "createTopic"],
        (EventBroker::AwsSqs, EventRole::Publisher) => &["send_message", "sendMessage"],
        (EventBroker::AwsSqs, EventRole::Subscriber) => {
            &["receive_message", "receiveMessage", "SqsListener"]
        }
        (EventBroker::AwsSqs, EventRole::Declaration) => &["create_queue", "createQueue"],
        (EventBroker::Nats | EventBroker::Generic, EventRole::Publisher) => {
            &[".publish", "publish", ".Publish", "Publish"]
        }
        (EventBroker::Nats | EventBroker::Generic, EventRole::Subscriber) => {
            &[".subscribe", "subscribe", ".Subscribe", "Subscribe"]
        }
        (EventBroker::Nats, EventRole::Declaration) => &["add_stream", "addStream"],
        (EventBroker::GooglePubSub, EventRole::Subscriber) => &[
            ".subscribe",
            "subscribe",
            ".pull",
            "pull",
            "PubSubSubscription",
        ],
        (EventBroker::GooglePubSub, EventRole::Declaration) => {
            &["create_topic", "createTopic", "create_subscription"]
        }
        (EventBroker::Generic, EventRole::Declaration) => {
            &["declare_channel", "declareChannel", "create_channel"]
        }
        _ => &[],
    };
    markers
        .iter()
        .find_map(|marker| {
            argument_expression(statement, marker, argument_index)
                .map(static_literals_from_expression)
        })
        .unwrap_or_default()
}

fn argument_expression<'a>(source: &'a str, marker: &str, target: usize) -> Option<&'a str> {
    let marker_start = source.find(marker)?;
    let after_marker = &source[marker_start + marker.len()..];
    let open_offset = after_marker.find('(')?;
    let arguments = &after_marker[open_offset + 1..];
    let mut quote = None;
    let mut escaped = false;
    let mut depth = 0_u32;
    let mut start = 0;
    let mut index = 0;
    for (offset, character) in arguments.char_indices() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == delimiter {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' | '`' => quote = Some(character),
            '(' | '[' | '{' => depth = depth.saturating_add(1),
            ')' if depth == 0 => {
                return (index == target).then(|| &arguments[start..offset]);
            }
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                if index == target {
                    return Some(&arguments[start..offset]);
                }
                index += 1;
                start = offset + character.len_utf8();
            }
            _ => {}
        }
    }
    (index == target).then(|| &arguments[start..])
}

fn literals_after_named(source: &str, keys: &[&str]) -> Vec<String> {
    let mut output = Vec::new();
    for key in keys {
        let mut offset = 0;
        while let Some(relative) = source[offset..].find(key) {
            let position = offset + relative;
            if is_identifier_boundary(source, position, key.len()) {
                let after = &source[position + key.len()..];
                let expression = after
                    .trim_start()
                    .strip_prefix([':', '='])
                    .or_else(|| after.trim_start().strip_prefix('('));
                if let Some(expression) = expression {
                    output.extend(static_literals_from_expression(expression));
                }
            }
            offset = position.saturating_add(key.len());
            if offset >= source.len() {
                break;
            }
        }
    }
    output.sort();
    output.dedup();
    output
}

fn literal_after_named(source: &str, keys: &[&str]) -> Option<String> {
    literals_after_named(source, keys).into_iter().next()
}

fn static_literals_from_expression(expression: &str) -> Vec<String> {
    let trimmed = expression.trim_start_matches(|character: char| {
        character.is_whitespace() || matches!(character, '&' | '*')
    });
    if trimmed.starts_with("format!")
        || trimmed.starts_with("format(")
        || trimmed.starts_with("f\"")
        || trimmed.starts_with("f'")
        || trimmed.starts_with("F\"")
        || trimmed.starts_with("F'")
    {
        return Vec::new();
    }
    if let Some(inner) = trimmed.strip_prefix('[') {
        return split_literal_list(inner, ']');
    }
    if let Some(inner) = trimmed.strip_prefix('(') {
        return split_literal_list(inner, ')');
    }
    let Some((value, consumed)) = parse_static_literal(trimmed) else {
        return Vec::new();
    };
    let remainder = trimmed.get(consumed..).unwrap_or_default().trim_start();
    if remainder.is_empty() || remainder.starts_with([',', ')', ']', '}', ';', '.']) {
        vec![value]
    } else {
        Vec::new()
    }
}

fn split_literal_list(source: &str, closing: char) -> Vec<String> {
    let mut output = Vec::new();
    let mut remainder = source;
    loop {
        remainder = remainder.trim_start();
        if remainder.starts_with(closing) || remainder.is_empty() {
            break;
        }
        let Some((value, consumed)) = parse_static_literal(remainder) else {
            return Vec::new();
        };
        output.push(value);
        remainder = remainder.get(consumed..).unwrap_or_default().trim_start();
        if remainder.starts_with(',') {
            remainder = &remainder[1..];
        } else if !remainder.starts_with(closing) {
            return Vec::new();
        }
    }
    output
}

fn parse_static_literal(source: &str) -> Option<(String, usize)> {
    let (prefix, delimiter, raw) = if source.starts_with("r#\"") {
        ("r#\"", '"', true)
    } else if source.starts_with("r\"") {
        ("r\"", '"', true)
    } else if source.starts_with('"') {
        ("\"", '"', false)
    } else if source.starts_with('\'') {
        ("'", '\'', false)
    } else if source.starts_with('`') {
        ("`", '`', false)
    } else {
        return None;
    };
    let mut escaped = false;
    let mut value = String::new();
    let content_start = prefix.len();
    for (relative, character) in source[content_start..].char_indices() {
        if raw && character == delimiter {
            let end = content_start + relative;
            let suffix = if prefix == "r#\"" { "\"#" } else { "\"" };
            if source[end..].starts_with(suffix) {
                return Some((value, end + suffix.len()));
            }
            value.push(character);
            continue;
        }
        if !raw && escaped {
            value.push(match character {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                other => other,
            });
            escaped = false;
            continue;
        }
        if !raw && character == '\\' {
            escaped = true;
            continue;
        }
        if !raw && character == delimiter {
            if delimiter == '`' && value.contains("${") {
                return None;
            }
            return Some((value, content_start + relative + character.len_utf8()));
        }
        value.push(character);
    }
    None
}

fn is_identifier_boundary(source: &str, position: usize, length: usize) -> bool {
    let before = source[..position].chars().next_back();
    let after = source[position + length..].chars().next();
    !before.is_some_and(is_identifier_character) && !after.is_some_and(is_identifier_character)
}

fn is_identifier_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn source_candidate(lines: &[String], start: usize) -> Option<String> {
    let first = lines.get(start)?.trim();
    if first.is_empty() || !possible_event_line(first) {
        return None;
    }
    let mut candidate = String::new();
    let mut balance = 0_i32;
    for line in lines.iter().skip(start).take(12) {
        if !candidate.is_empty() {
            candidate.push(' ');
        }
        candidate.push_str(line.trim());
        balance += delimiter_balance(line);
        if balance <= 0 && candidate.contains('(') {
            break;
        }
    }
    Some(candidate)
}

fn possible_event_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "publish",
        "subscribe",
        "send",
        "receive",
        "consume",
        "produce",
        "queue",
        "topic",
        "writemessages",
        "basic_",
        "createstream",
        "create_stream",
        "pull(",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

fn delimiter_balance(line: &str) -> i32 {
    let mut balance = 0_i32;
    let mut quote = None;
    let mut escaped = false;
    for character in line.chars() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == delimiter {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' | '`' => quote = Some(character),
            '(' | '[' | '{' => balance += 1,
            ')' | ']' | '}' => balance -= 1,
            _ => {}
        }
    }
    balance
}

fn sanitize_source(language: SourceLanguage, input: &str) -> Vec<String> {
    let hash_comments = matches!(language, SourceLanguage::Python);
    let mut block_comment = false;
    input
        .lines()
        .map(|line| sanitize_line(line, hash_comments, &mut block_comment))
        .collect()
}

fn sanitize_line(line: &str, hash_comments: bool, block_comment: &mut bool) -> String {
    let characters = line.char_indices().collect::<Vec<_>>();
    let mut output = String::with_capacity(line.len());
    let mut index = 0;
    let mut quote = None;
    let mut escaped = false;
    while let Some(&(offset, character)) = characters.get(index) {
        let next = characters.get(index + 1).map(|(_, value)| *value);
        if *block_comment {
            if character == '*' && next == Some('/') {
                *block_comment = false;
                index += 2;
            } else {
                index += 1;
            }
            continue;
        }
        if let Some(delimiter) = quote {
            output.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == delimiter {
                quote = None;
            }
            index += 1;
            continue;
        }
        if matches!(character, '"' | '\'' | '`') {
            quote = Some(character);
            output.push(character);
            index += 1;
            continue;
        }
        if character == '/' && next == Some('/') {
            break;
        }
        if character == '/' && next == Some('*') {
            *block_comment = true;
            index += 2;
            continue;
        }
        if hash_comments && character == '#' {
            break;
        }
        output.push_str(&line[offset..offset + character.len_utf8()]);
        index += 1;
    }
    output
}

fn mask_string_contents(source: &str) -> String {
    let mut output = String::with_capacity(source.len());
    let mut quote = None;
    let mut escaped = false;
    for character in source.chars() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
                output.push(' ');
            } else if character == '\\' {
                escaped = true;
                output.push(' ');
            } else if character == delimiter {
                quote = None;
                output.push(character);
            } else {
                output.push(' ');
            }
        } else if matches!(character, '"' | '\'' | '`') {
            quote = Some(character);
            output.push(character);
        } else {
            output.push(character);
        }
    }
    output
}

fn compact_lowercase(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}

fn bounded_source(input: &str) -> (&str, bool) {
    if input.len() <= MAX_SOURCE_BYTES {
        return (input, false);
    }
    let boundary = input.floor_char_boundary(MAX_SOURCE_BYTES);
    (&input[..boundary], true)
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    let mut output = value.chars().take(max_chars).collect::<String>();
    if value.chars().count() > max_chars {
        output.push_str("...");
    }
    output
}

fn normalize_observation(observation: &mut EventObservation) {
    observation.warnings.sort();
    observation.warnings.dedup();
    observation.evidence.sort();
    observation.evidence.dedup();
    if let Some(schema) = &mut observation.schema {
        schema.fields.sort();
        schema.fields.dedup();
    }
    if !observation.warnings.is_empty() {
        observation.incomplete = true;
    }
}

fn finish_document(document: &mut EventDocument) {
    for observation in &mut document.observations {
        normalize_observation(observation);
    }
    document.observations.sort_by_key(observation_sort_key);
    let mut deduplicated = Vec::<EventObservation>::new();
    for observation in std::mem::take(&mut document.observations) {
        if let Some(previous) = deduplicated.last_mut()
            && observation_sort_key(previous) == observation_sort_key(&observation)
        {
            previous.evidence.extend(observation.evidence);
            normalize_observation(previous);
        } else {
            deduplicated.push(observation);
        }
    }
    document.observations = deduplicated;
    document.warnings.sort();
    document.warnings.dedup();
    document.incomplete |= document
        .observations
        .iter()
        .any(|observation| observation.incomplete);
}

fn observation_sort_key(observation: &EventObservation) -> String {
    format!(
        "{:?}\u{0}{:?}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{:?}\u{0}{:?}\u{0}{}\u{0}{}\u{0}{:?}\u{0}{}\u{0}{:?}\u{0}{}\u{0}{:?}",
        observation.broker,
        observation.role,
        observation.channel.as_deref().unwrap_or_default(),
        observation.event_type.as_deref().unwrap_or_default(),
        observation.namespace.as_deref().unwrap_or_default(),
        observation.protocol.as_deref().unwrap_or_default(),
        observation.schema,
        observation.partition_key,
        observation.routing_key.as_deref().unwrap_or_default(),
        observation
            .dead_letter_channel
            .as_deref()
            .unwrap_or_default(),
        observation.delivery_semantics,
        observation.confidence,
        observation.language,
        observation.incomplete,
        observation.warnings
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_observation(language: SourceLanguage, source: &str) -> EventObservation {
        parse_event_source(language, source)
            .observations
            .into_iter()
            .next()
            .expect("fixture should produce an observation")
    }

    #[test]
    fn asyncapi_two_extracts_kafka_message_schema_and_bindings() {
        let source = r#"
asyncapi: 2.6.0
servers:
  production:
    url: kafka.example.test:9092/orders
    protocol: kafka
channels:
  orders.created:
    bindings:
      kafka:
        topicConfiguration: {}
    publish:
      bindings:
        kafka:
          x-partition-key: tenant_id
      message:
        name: OrderCreated
        schemaFormat: application/schema+json;version=draft-07
        x-schema-version: "2"
        payload:
          type: object
          required: [id]
          properties:
            tenant_id: { type: string }
            id: { type: string }
"#;
        let document = extract_asyncapi("asyncapi.yaml", source).expect("fixture should parse");

        assert!(matches!(
            document.observations.as_slice(),
            [observation]
                if observation.broker == EventBroker::Kafka
                    && observation.role == EventRole::Publisher
                    && observation.channel.as_deref() == Some("orders.created")
                    && observation.partition_key.as_deref() == Some("tenant_id")
                    && observation.schema.as_ref().is_some_and(|schema|
                        schema.name.as_deref() == Some("OrderCreated")
                            && schema.version.as_deref() == Some("2")
                            && schema.fields.len() == 2)
        ));
    }

    #[test]
    fn asyncapi_three_extracts_send_and_receive_operations() {
        let source = r"
asyncapi: 3.0.0
servers:
  broker:
    host: nats.example.test
    protocol: nats
channels:
  jobs:
    address: jobs.ready
    messages:
      Job:
        name: Job
        payload:
          type: object
          properties:
            id: { type: string }
operations:
  sendJob:
    action: send
    channel:
      $ref: '#/channels/jobs'
  receiveJob:
    action: receive
    channel:
      $ref: '#/channels/jobs'
";
        let document = extract_asyncapi("asyncapi.json", source).expect("fixture should parse");

        assert!(
            document.observations.len() == 2
                && document.observations.iter().all(|observation| {
                    observation.channel.as_deref() == Some("jobs.ready")
                        && observation.namespace.as_deref() == Some("nats.example.test")
                        && observation.broker == EventBroker::Nats
                })
        );
    }

    #[test]
    fn asyncapi_multiple_server_protocols_remain_ambiguous() {
        let source = r"
asyncapi: 2.6.0
servers:
  kafka:
    url: kafka.example.test
    protocol: kafka
  amqp:
    url: rabbit.example.test
    protocol: amqp
channels:
  events:
    publish:
      message:
        name: Event
";
        let document = extract_asyncapi("asyncapi.yaml", source).expect("fixture should parse");

        assert!(
            document.incomplete
                && document
                    .warnings
                    .iter()
                    .any(|warning| { warning.contains("multiple server protocols") })
        );
    }

    #[test]
    fn asyncapi_templated_channel_is_not_exact() {
        let source = r"
asyncapi: 2.6.0
channels:
  orders.{tenant}:
    subscribe:
      message:
        name: Order
";
        let document = extract_asyncapi("asyncapi.yaml", source).expect("fixture should parse");

        assert!(matches!(
            document.observations.as_slice(),
            [observation]
                if observation.channel.is_none()
                    && observation.incomplete
                    && observation.confidence == 0.0
        ));
    }

    #[test]
    fn asyncapi_supports_json_and_dead_letter_metadata() {
        let source = r#"{
          "asyncapi": "2.6.0",
          "servers": {"queue": {"url": "queue.example.test", "protocol": "sqs"}},
          "channels": {
            "jobs": {
              "subscribe": {
                "x-delivery-semantics": "at-least-once",
                "deadLetterQueue": "jobs-dead",
                "message": {"name": "Job"}
              }
            }
          }
        }"#;
        let document = extract_asyncapi("asyncapi.json", source).expect("fixture should parse");

        assert!(matches!(
            document.observations.as_slice(),
            [observation]
                if observation.broker == EventBroker::AwsSqs
                    && observation.delivery_semantics
                        == Some(DeliverySemantics::AtLeastOnce)
                    && observation.dead_letter_channel.as_deref() == Some("jobs-dead")
        ));
    }

    #[test]
    fn kafka_literal_call_is_exact() {
        let observation = source_observation(
            SourceLanguage::Rust,
            r#"producer.send(FutureRecord::to("orders.created").payload(body));"#,
        );

        assert!(
            observation.broker == EventBroker::Kafka
                && observation.channel.as_deref() == Some("orders.created")
                && observation.role == EventRole::Publisher
        );
    }

    #[test]
    fn rabbitmq_literal_call_extracts_exchange() {
        let observation = source_observation(
            SourceLanguage::Python,
            r#"channel.basic_publish(exchange="orders", routing_key="created", body=data)"#,
        );

        assert!(
            observation.broker == EventBroker::RabbitMq
                && observation.channel.as_deref() == Some("orders")
                && observation.routing_key.as_deref() == Some("created")
        );
    }

    #[test]
    fn sns_literal_call_extracts_topic_arn() {
        let observation = source_observation(
            SourceLanguage::TypeScript,
            r#"sns.publish({ TopicArn: "arn:aws:sns:us-east-1:123:orders", Message: body });"#,
        );

        assert!(
            observation.broker == EventBroker::AwsSns
                && observation.channel.as_deref() == Some("arn:aws:sns:us-east-1:123:orders")
        );
    }

    #[test]
    fn sqs_literal_call_extracts_queue_url() {
        let observation = source_observation(
            SourceLanguage::JavaScript,
            r#"sqs.sendMessage({ QueueUrl: "https://sqs.example.test/orders", MessageBody: body });"#,
        );

        assert!(
            observation.broker == EventBroker::AwsSqs
                && observation.channel.as_deref() == Some("https://sqs.example.test/orders")
        );
    }

    #[test]
    fn nats_literal_call_extracts_subject() {
        let observation = source_observation(
            SourceLanguage::Go,
            r#"natsConnection.Publish("orders.created", payload)"#,
        );

        assert!(
            observation.broker == EventBroker::Nats
                && observation.channel.as_deref() == Some("orders.created")
        );
    }

    #[test]
    fn google_pubsub_literal_call_extracts_topic() {
        let observation = source_observation(
            SourceLanguage::Java,
            r#"pubsubPublisher.publish("projects/demo/topics/orders", message);"#,
        );

        assert!(
            observation.broker == EventBroker::GooglePubSub
                && observation.channel.as_deref() == Some("projects/demo/topics/orders")
        );
    }

    #[test]
    fn generic_literal_subscriber_is_exact() {
        let observation = source_observation(
            SourceLanguage::Python,
            r#"event_bus.subscribe("inventory.changed", handler)"#,
        );

        assert!(
            observation.broker == EventBroker::Generic
                && observation.role == EventRole::Subscriber
                && observation.channel.as_deref() == Some("inventory.changed")
        );
    }

    #[test]
    fn dynamic_channel_remains_incomplete_without_exact_value() {
        let observation = source_observation(
            SourceLanguage::TypeScript,
            "eventBus.publish(topicName, payload);",
        );

        assert!(
            observation.channel.is_none()
                && observation.incomplete
                && observation.confidence == 0.0
                && observation
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("dynamic channel"))
        );
    }

    #[test]
    fn interpolated_literal_remains_dynamic() {
        let observation = source_observation(
            SourceLanguage::JavaScript,
            "nats.publish(`orders.${tenant}`, payload);",
        );

        assert!(observation.channel.is_none() && observation.incomplete);
    }

    #[test]
    fn concatenated_literal_remains_dynamic() {
        let observation = source_observation(
            SourceLanguage::Java,
            r#"eventBus.publish("orders." + tenant, payload);"#,
        );

        assert!(observation.channel.is_none() && observation.incomplete);
    }

    #[test]
    fn call_text_inside_string_is_not_observed() {
        let document = parse_event_source(
            SourceLanguage::Rust,
            r#"let example = "event_bus.publish(\"orders\", payload)";"#,
        );

        assert_eq!(document.observations, Vec::new());
    }

    #[test]
    fn evidence_does_not_retain_source_or_payload() {
        let secret = "private-message-body";
        let document = parse_event_source(
            SourceLanguage::JavaScript,
            &format!(r#"eventBus.publish("orders", "{secret}");"#),
        );
        let serialized = serde_json::to_string(&document).expect("event document should serialize");

        assert!(!serialized.contains(secret));
    }

    #[test]
    fn source_results_are_sorted_and_deduplicated() {
        let document = parse_event_source(
            SourceLanguage::Python,
            "bus.publish(\"z\", body)\nbus.publish(\"a\", body)\nbus.publish(\"a\", body)",
        );

        assert!(matches!(
            document.observations.as_slice(),
            [first, second]
                if first.channel.as_deref() == Some("a")
                    && second.channel.as_deref() == Some("z")
        ));
    }
}

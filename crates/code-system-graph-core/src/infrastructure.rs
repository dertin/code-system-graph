//! Secret-safe, bounded extraction for infrastructure and deployment declarations.
//!
//! Extractors in this module retain declaration metadata only. They never retain source
//! fragments or values from environment variables, Kubernetes secrets, credentials, tokens, or
//! connection strings. Dynamic expressions are omitted and make the returned document incomplete.

use std::collections::BTreeSet;

use hcl::{Block, BlockLabel, Body, Expression, ObjectKey, Structure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

type Mapping = serde_json::Map<String, Value>;
use thiserror::Error;

/// Maximum infrastructure source size accepted by an extractor.
pub const MAX_INFRASTRUCTURE_INPUT_BYTES: usize = 1024 * 1024;
/// Maximum structural nesting accepted before or after parsing.
pub const MAX_INFRASTRUCTURE_DEPTH: usize = 64;
/// Maximum number of parsed syntax items accepted from one source.
pub const MAX_INFRASTRUCTURE_ITEMS: usize = 16_384;
/// Maximum number of deployment units and resources retained from one source.
pub const MAX_INFRASTRUCTURE_OUTPUT_ITEMS: usize = 1_024;
/// Maximum number of evidence records retained from one source.
pub const MAX_INFRASTRUCTURE_EVIDENCE: usize = 1_024;
/// Maximum byte length of a retained source-derived identifier.
pub const MAX_INFRASTRUCTURE_STRING_BYTES: usize = 512;
/// Maximum number of warnings retained from one source.
pub const MAX_INFRASTRUCTURE_WARNINGS: usize = 32;

const MAX_SOURCE_PATH_BYTES: usize = 4_096;
const MAX_YAML_DOCUMENTS: usize = 256;
const HELM_DYNAMIC_MARKER: &str = "CODE_SYSTEM_GRAPH_DYNAMIC_VALUE";

/// Supported infrastructure artifact families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InfrastructureArtifactKind {
    /// Docker Compose YAML.
    DockerCompose,
    /// Kubernetes YAML or JSON.
    Kubernetes,
    /// A Helm template containing conservatively visible YAML.
    HelmTemplate,
    /// A Helm values file.
    HelmValues,
    /// Terraform or `OpenTofu` HCL.
    Terraform,
}

/// Kinds of explicitly declared deployment units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentKind {
    /// A Docker Compose service.
    DockerComposeService,
    /// A Kubernetes Deployment.
    KubernetesDeployment,
    /// A Kubernetes `StatefulSet`.
    KubernetesStatefulSet,
    /// A Kubernetes `DaemonSet`.
    KubernetesDaemonSet,
    /// A Kubernetes Job.
    KubernetesJob,
    /// A Kubernetes `CronJob`.
    KubernetesCronJob,
    /// A standalone Kubernetes Pod.
    KubernetesPod,
    /// A statically visible workload in a Helm template whose concrete kind is unsupported.
    HelmTemplate,
    /// A Terraform or `OpenTofu` resource that directly declares a deployment unit.
    TerraformResource,
}

/// Coarse kind of an explicitly declared infrastructure resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InfrastructureResourceKind {
    /// A discoverable or deployable service.
    Service,
    /// An ingress or externally routed endpoint.
    Ingress,
    /// A messaging topic.
    Topic,
    /// A messaging queue.
    Queue,
    /// A database service or database declaration.
    Database,
    /// Another explicit resource whose provider type is retained.
    Other,
}

/// Kind of bounded, value-free evidence supporting an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InfrastructureEvidenceKind {
    /// An explicit resource or deployment declaration.
    Declaration,
    /// An explicit image attribute.
    Image,
    /// An explicit port declaration.
    Port,
    /// An explicit dependency or backend reference.
    Dependency,
    /// An explicit environment key declaration.
    EnvironmentKey,
    /// An explicit selector.
    Selector,
    /// An explicit host alias.
    HostAlias,
}

/// A bounded source locator that deliberately contains no source text or values.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfrastructureEvidence {
    /// Evidence category.
    pub kind: InfrastructureEvidenceKind,
    /// Optional one-based source line. It is absent when the parser exposes no stable span.
    pub line: Option<u32>,
}

/// One explicit network port declaration.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfrastructurePort {
    /// Optional declared port name.
    pub name: Option<String>,
    /// Service or container-facing port.
    pub port: u16,
    /// Optional target port used by a service declaration.
    pub target_port: Option<u16>,
    /// Optional host-published port.
    pub host_port: Option<u16>,
    /// Optional normalized transport protocol.
    pub protocol: Option<String>,
}

/// One explicit selector key/value pair.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfrastructureSelector {
    /// Selector key.
    pub key: String,
    /// Literal selector value.
    pub value: String,
}

/// One explicit deployment unit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeploymentUnit {
    /// Deployment technology and workload kind.
    pub kind: DeploymentKind,
    /// Literal deployment name.
    pub name: String,
    /// Optional literal namespace.
    pub namespace: Option<String>,
    /// Literal container images.
    pub images: Vec<String>,
    /// Explicit network ports.
    pub ports: Vec<InfrastructurePort>,
    /// Environment variable key names. Values are never retained.
    pub environment_keys: Vec<String>,
    /// Explicit dependency or resource-reference names.
    pub dependencies: Vec<String>,
    /// Explicit service names owned directly by this declaration.
    pub service_names: Vec<String>,
    /// Explicit service-discovery or host-alias names.
    pub host_aliases: Vec<String>,
    /// Explicit selectors. Selectors are not resolved into inferred deployment links.
    pub selectors: Vec<InfrastructureSelector>,
    /// Bounded value-free evidence.
    pub evidence: Vec<InfrastructureEvidence>,
}

/// One explicit infrastructure resource.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfrastructureResource {
    /// Coarse resource kind.
    pub kind: InfrastructureResourceKind,
    /// Literal provider or manifest type, such as `aws_sqs_queue` or `ConfigMap`.
    pub resource_type: String,
    /// Literal resource name.
    pub name: String,
    /// Optional literal namespace.
    pub namespace: Option<String>,
    /// Explicit network ports.
    pub ports: Vec<InfrastructurePort>,
    /// Environment or configuration key names. Values are never retained.
    pub key_names: Vec<String>,
    /// Explicit dependency or backend-reference names.
    pub dependencies: Vec<String>,
    /// Explicit selectors. They are retained without resolving them to workloads.
    pub selectors: Vec<InfrastructureSelector>,
    /// Bounded value-free evidence.
    pub evidence: Vec<InfrastructureEvidence>,
}

/// Secret-safe extraction result for one infrastructure source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InfrastructureDocument {
    /// Artifact family selected by the extractor.
    pub artifact_kind: InfrastructureArtifactKind,
    /// Bounded source path supplied by the caller.
    pub source_path: String,
    /// Explicit deployment units in deterministic order.
    pub deployment_units: Vec<DeploymentUnit>,
    /// Explicit infrastructure resources in deterministic order.
    pub resources: Vec<InfrastructureResource>,
    /// Environment key names visible outside a concrete deployment unit, primarily Helm values.
    pub environment_keys: Vec<String>,
    /// Bounded deterministic warnings that contain no source values.
    pub warnings: Vec<String>,
    /// Whether unsupported, dynamic, templated, or truncated constructs were observed.
    pub incomplete: bool,
}

/// Bounded, secret-safe infrastructure extraction failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InfrastructureExtractionError {
    /// Input exceeds the byte budget.
    #[error("infrastructure input is {actual} bytes; maximum is {maximum}")]
    InputTooLarge {
        /// Observed input size.
        actual: usize,
        /// Configured input limit.
        maximum: usize,
    },
    /// Source path metadata is outside the accepted bound.
    #[error("infrastructure source path is invalid or exceeds the supported length")]
    InvalidSourcePath,
    /// The source exceeded the structural nesting budget.
    #[error("infrastructure input exceeds the maximum structural depth of {maximum}")]
    StructureTooDeep {
        /// Configured depth limit.
        maximum: usize,
    },
    /// The parsed source exceeded the syntax item budget.
    #[error("infrastructure input exceeds the maximum syntax item count of {maximum}")]
    TooManyItems {
        /// Configured item limit.
        maximum: usize,
    },
    /// The source is malformed for the selected artifact family.
    #[error("infrastructure input is not a valid {artifact_kind:?} document")]
    InvalidDocument {
        /// Selected artifact family. Parser diagnostics are intentionally omitted.
        artifact_kind: InfrastructureArtifactKind,
    },
}

/// Extracts explicit Docker Compose services and their declaration metadata.
///
/// Environment values, secret definitions, and source fragments are never retained.
///
/// # Errors
///
/// Returns [`InfrastructureExtractionError`] when the source path, size, structure, or YAML
/// document exceeds a documented bound or is malformed.
pub fn extract_docker_compose(
    source_path: &str,
    input: &str,
) -> Result<InfrastructureDocument, InfrastructureExtractionError> {
    validate_common(source_path, input)?;
    let documents = parse_yaml_documents(input, InfrastructureArtifactKind::DockerCompose)?;
    let mut collector = Collector::new(InfrastructureArtifactKind::DockerCompose, source_path);

    for document in &documents {
        extract_compose_document(document, &mut collector);
    }

    Ok(collector.finish())
}

/// Extracts explicit Kubernetes workloads and resources without resolving selector-based links.
///
/// `ConfigMap` and `Secret` values are ignored; only their key names are retained.
///
/// # Errors
///
/// Returns [`InfrastructureExtractionError`] when the source path, size, structure, or YAML/JSON
/// document exceeds a documented bound or is malformed.
pub fn extract_kubernetes(
    source_path: &str,
    input: &str,
) -> Result<InfrastructureDocument, InfrastructureExtractionError> {
    validate_common(source_path, input)?;
    let documents = parse_yaml_documents(input, InfrastructureArtifactKind::Kubernetes)?;
    let mut collector = Collector::new(InfrastructureArtifactKind::Kubernetes, source_path);

    for document in &documents {
        extract_kubernetes_value(document, &mut collector, false);
    }

    Ok(collector.finish())
}

/// Extracts statically visible declarations from a Helm template or values file.
///
/// Templates are never rendered. Dynamic expressions are masked before conservative YAML parsing,
/// omitted from output, and reported through [`InfrastructureDocument::incomplete`].
///
/// # Errors
///
/// Returns [`InfrastructureExtractionError`] when the source path, size, structure, or static YAML
/// exceeds a documented bound. A syntactically valid-looking templated file that cannot be parsed
/// without rendering returns an incomplete document instead of executing template logic.
pub fn extract_helm(
    source_path: &str,
    input: &str,
) -> Result<InfrastructureDocument, InfrastructureExtractionError> {
    validate_common(source_path, input)?;
    let artifact_kind = if is_helm_values_path(source_path) {
        InfrastructureArtifactKind::HelmValues
    } else {
        InfrastructureArtifactKind::HelmTemplate
    };
    let (masked, has_template) = mask_helm_templates(input);
    let mut collector = Collector::new(artifact_kind, source_path);

    if has_template {
        collector.mark_incomplete("dynamic Helm template expressions were omitted");
    }

    let documents = match parse_yaml_documents(&masked, artifact_kind) {
        Ok(documents) => documents,
        Err(InfrastructureExtractionError::InvalidDocument { .. }) if has_template => {
            collector.mark_incomplete("Helm template requires rendering and was not evaluated");
            return Ok(collector.finish());
        }
        Err(error) => return Err(error),
    };

    if artifact_kind == InfrastructureArtifactKind::HelmValues {
        for document in &documents {
            extract_helm_values(document, &mut collector);
        }
    } else {
        for document in &documents {
            extract_kubernetes_value(document, &mut collector, true);
        }
    }

    Ok(collector.finish())
}

/// Extracts explicit Terraform or `OpenTofu` resource declarations from HCL.
///
/// Only literal names, images, ports, dependency traversals, and environment key names are
/// retained. Dynamic expressions are omitted.
///
/// # Errors
///
/// Returns [`InfrastructureExtractionError`] when the source path, size, HCL syntax, structural
/// depth, or syntax item count exceeds a documented bound.
pub fn extract_terraform(
    source_path: &str,
    input: &str,
) -> Result<InfrastructureDocument, InfrastructureExtractionError> {
    validate_common(source_path, input)?;
    let body = hcl::parse(input).map_err(|_| InfrastructureExtractionError::InvalidDocument {
        artifact_kind: InfrastructureArtifactKind::Terraform,
    })?;
    enforce_hcl_budget(&body)?;

    let mut collector = Collector::new(InfrastructureArtifactKind::Terraform, source_path);
    visit_terraform_body(&body, &mut collector);
    Ok(collector.finish())
}

struct Collector {
    document: InfrastructureDocument,
    warnings: BTreeSet<String>,
    evidence_count: usize,
    output_count: usize,
}

impl Collector {
    fn new(artifact_kind: InfrastructureArtifactKind, source_path: &str) -> Self {
        Self {
            document: InfrastructureDocument {
                artifact_kind,
                source_path: source_path.to_owned(),
                deployment_units: Vec::new(),
                resources: Vec::new(),
                environment_keys: Vec::new(),
                warnings: Vec::new(),
                incomplete: false,
            },
            warnings: BTreeSet::new(),
            evidence_count: 0,
            output_count: 0,
        }
    }

    fn mark_incomplete(&mut self, warning: &'static str) {
        self.document.incomplete = true;
        if self.warnings.len() < MAX_INFRASTRUCTURE_WARNINGS {
            self.warnings.insert(warning.to_owned());
        }
    }

    fn evidence(&mut self, kind: InfrastructureEvidenceKind) -> Vec<InfrastructureEvidence> {
        if self.evidence_count >= MAX_INFRASTRUCTURE_EVIDENCE {
            self.mark_incomplete("evidence limit reached; additional evidence was omitted");
            return Vec::new();
        }
        self.evidence_count += 1;
        vec![InfrastructureEvidence { kind, line: None }]
    }

    fn push_unit(&mut self, mut unit: DeploymentUnit) {
        normalize_unit(&mut unit);
        if self.output_count >= MAX_INFRASTRUCTURE_OUTPUT_ITEMS {
            self.mark_incomplete("output item limit reached; additional declarations were omitted");
            return;
        }
        self.output_count += 1;
        self.document.deployment_units.push(unit);
    }

    fn push_resource(&mut self, mut resource: InfrastructureResource) {
        normalize_resource(&mut resource);
        if self.output_count >= MAX_INFRASTRUCTURE_OUTPUT_ITEMS {
            self.mark_incomplete("output item limit reached; additional declarations were omitted");
            return;
        }
        self.output_count += 1;
        self.document.resources.push(resource);
    }

    fn finish(mut self) -> InfrastructureDocument {
        for unit in &mut self.document.deployment_units {
            normalize_unit(unit);
        }
        for resource in &mut self.document.resources {
            normalize_resource(resource);
        }
        sort_dedupe(&mut self.document.deployment_units);
        sort_dedupe(&mut self.document.resources);
        sort_dedupe(&mut self.document.environment_keys);
        self.document.warnings = self.warnings.into_iter().collect();
        self.document
    }
}

fn normalize_unit(unit: &mut DeploymentUnit) {
    sort_dedupe(&mut unit.images);
    sort_dedupe(&mut unit.ports);
    sort_dedupe(&mut unit.environment_keys);
    sort_dedupe(&mut unit.dependencies);
    sort_dedupe(&mut unit.service_names);
    sort_dedupe(&mut unit.host_aliases);
    sort_dedupe(&mut unit.selectors);
    sort_dedupe(&mut unit.evidence);
}

fn normalize_resource(resource: &mut InfrastructureResource) {
    sort_dedupe(&mut resource.ports);
    sort_dedupe(&mut resource.key_names);
    sort_dedupe(&mut resource.dependencies);
    sort_dedupe(&mut resource.selectors);
    sort_dedupe(&mut resource.evidence);
}

fn sort_dedupe<T: Ord>(values: &mut Vec<T>) {
    values.sort();
    values.dedup();
}

fn validate_common(source_path: &str, input: &str) -> Result<(), InfrastructureExtractionError> {
    if source_path.is_empty()
        || source_path.len() > MAX_SOURCE_PATH_BYTES
        || source_path.contains('\0')
    {
        return Err(InfrastructureExtractionError::InvalidSourcePath);
    }
    if input.len() > MAX_INFRASTRUCTURE_INPUT_BYTES {
        return Err(InfrastructureExtractionError::InputTooLarge {
            actual: input.len(),
            maximum: MAX_INFRASTRUCTURE_INPUT_BYTES,
        });
    }
    preflight_depth(input)
}

fn preflight_depth(input: &str) -> Result<(), InfrastructureExtractionError> {
    let mut flow_depth = 0_usize;
    let mut quote = None;
    let mut escaped = false;
    let mut block_comment = false;
    let mut previous = '\0';

    for line in input.lines() {
        let indentation = line
            .as_bytes()
            .iter()
            .take_while(|byte| **byte == b' ')
            .count();
        if indentation > MAX_INFRASTRUCTURE_DEPTH * 4 {
            return Err(InfrastructureExtractionError::StructureTooDeep {
                maximum: MAX_INFRASTRUCTURE_DEPTH,
            });
        }

        let mut line_comment = false;
        for current in line.chars() {
            if line_comment {
                break;
            }
            if block_comment {
                if previous == '*' && current == '/' {
                    block_comment = false;
                }
                previous = current;
                continue;
            }
            if let Some(delimiter) = quote {
                if delimiter == '"' && escaped {
                    escaped = false;
                } else if delimiter == '"' && current == '\\' {
                    escaped = true;
                } else if current == delimiter {
                    quote = None;
                }
                previous = current;
                continue;
            }
            match current {
                '"' | '\'' => quote = Some(current),
                '#' => line_comment = true,
                '/' if previous == '/' => line_comment = true,
                '*' if previous == '/' => block_comment = true,
                '{' | '[' | '(' => {
                    flow_depth += 1;
                    if flow_depth > MAX_INFRASTRUCTURE_DEPTH {
                        return Err(InfrastructureExtractionError::StructureTooDeep {
                            maximum: MAX_INFRASTRUCTURE_DEPTH,
                        });
                    }
                }
                '}' | ']' | ')' => flow_depth = flow_depth.saturating_sub(1),
                _ => {}
            }
            previous = current;
        }
        previous = '\0';
    }
    Ok(())
}

fn parse_yaml_documents(
    input: &str,
    artifact_kind: InfrastructureArtifactKind,
) -> Result<Vec<Value>, InfrastructureExtractionError> {
    let documents: Vec<Value> = crate::yaml::from_multiple(input)
        .map_err(|_| InfrastructureExtractionError::InvalidDocument { artifact_kind })?;
    if documents.len() > MAX_YAML_DOCUMENTS {
        return Err(InfrastructureExtractionError::TooManyItems {
            maximum: MAX_YAML_DOCUMENTS,
        });
    }
    for value in &documents {
        enforce_yaml_budget(value)?;
    }
    Ok(documents)
}

fn enforce_yaml_budget(value: &Value) -> Result<(), InfrastructureExtractionError> {
    let mut stack = vec![(value, 1_usize)];
    let mut items = 0_usize;
    while let Some((current, depth)) = stack.pop() {
        items += 1;
        if items > MAX_INFRASTRUCTURE_ITEMS {
            return Err(InfrastructureExtractionError::TooManyItems {
                maximum: MAX_INFRASTRUCTURE_ITEMS,
            });
        }
        if depth > MAX_INFRASTRUCTURE_DEPTH {
            return Err(InfrastructureExtractionError::StructureTooDeep {
                maximum: MAX_INFRASTRUCTURE_DEPTH,
            });
        }
        match current {
            Value::Array(sequence) => {
                stack.extend(sequence.iter().map(|item| (item, depth + 1)));
            }
            Value::Object(mapping) => {
                items = items.saturating_add(mapping.len());
                for nested in mapping.values() {
                    stack.push((nested, depth + 1));
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
    Ok(())
}

fn extract_compose_document(value: &Value, collector: &mut Collector) {
    let Some(root) = value.as_object() else {
        collector.mark_incomplete("Docker Compose root is not a mapping");
        return;
    };
    let Some(services) = yaml_get(root, "services").and_then(Value::as_object) else {
        collector.mark_incomplete("Docker Compose services mapping is absent");
        return;
    };

    let mut entries = services.iter().collect::<Vec<_>>();
    entries.sort_by_key(|(key, _)| key.as_str());
    for (name_value, service_value) in entries {
        let Some(name) = bounded_literal(name_value) else {
            collector.mark_incomplete("dynamic or invalid Compose service name was omitted");
            continue;
        };
        let Some(service) = service_value.as_object() else {
            collector.mark_incomplete("unsupported Compose service definition was omitted");
            continue;
        };

        let mut unit = empty_unit(DeploymentKind::DockerComposeService, name.clone());
        unit.service_names.push(name.clone());
        unit.evidence = collector.evidence(InfrastructureEvidenceKind::Declaration);

        if let Some(image_value) = yaml_get(service, "image") {
            match yaml_static_string(image_value) {
                Some(image) => unit.images.push(image),
                None => collector.mark_incomplete("dynamic Compose image was omitted"),
            }
        }
        if let Some(environment) = yaml_get(service, "environment") {
            extract_yaml_environment_keys(environment, &mut unit.environment_keys, collector);
        }
        if let Some(ports) = yaml_get(service, "ports") {
            extract_compose_ports(ports, &mut unit.ports, collector);
        }
        if let Some(depends_on) = yaml_get(service, "depends_on") {
            extract_name_set(depends_on, &mut unit.dependencies, collector);
        }
        if let Some(links) = yaml_get(service, "links") {
            extract_compose_links(
                links,
                &mut unit.dependencies,
                &mut unit.host_aliases,
                collector,
            );
        }
        if let Some(extra_hosts) = yaml_get(service, "extra_hosts") {
            extract_host_aliases(extra_hosts, &mut unit.host_aliases, collector);
        }
        for field in ["hostname", "container_name"] {
            if let Some(alias_value) = yaml_get(service, field) {
                if let Some(alias) = yaml_static_string(alias_value) {
                    unit.host_aliases.push(alias);
                } else {
                    collector.mark_incomplete("dynamic Compose host alias was omitted");
                }
            }
        }

        let resource = InfrastructureResource {
            kind: InfrastructureResourceKind::Service,
            resource_type: "compose_service".to_owned(),
            name,
            namespace: None,
            ports: unit.ports.clone(),
            key_names: unit.environment_keys.clone(),
            dependencies: unit.dependencies.clone(),
            selectors: Vec::new(),
            evidence: collector.evidence(InfrastructureEvidenceKind::Declaration),
        };
        collector.push_unit(unit);
        collector.push_resource(resource);
    }
}

fn extract_compose_ports(
    value: &Value,
    output: &mut Vec<InfrastructurePort>,
    collector: &mut Collector,
) {
    let Some(sequence) = value.as_array() else {
        collector.mark_incomplete("unsupported Compose ports declaration was omitted");
        return;
    };
    for entry in sequence {
        let port = match entry {
            Value::String(specification) => parse_compose_port_string(specification),
            Value::Number(number) => {
                number
                    .as_u64()
                    .and_then(to_u16)
                    .map(|port| InfrastructurePort {
                        name: None,
                        port,
                        target_port: None,
                        host_port: None,
                        protocol: None,
                    })
            }
            Value::Object(mapping) => parse_compose_port_mapping(mapping),
            _ => None,
        };
        match port {
            Some(port) => output.push(port),
            None => collector.mark_incomplete("dynamic or invalid Compose port was omitted"),
        }
    }
}

fn parse_compose_port_string(value: &str) -> Option<InfrastructurePort> {
    if contains_dynamic(value) || value.len() > MAX_INFRASTRUCTURE_STRING_BYTES {
        return None;
    }
    let (port_specification, protocol) = value
        .rsplit_once('/')
        .map_or((value, None), |(port_specification, protocol)| {
            (port_specification, normalize_protocol(protocol))
        });
    let parts = port_specification.split(':').collect::<Vec<_>>();
    let container = parts.last()?.parse::<u16>().ok()?;
    let host_port = if parts.len() >= 2 {
        parts.get(parts.len() - 2)?.parse::<u16>().ok()
    } else {
        None
    };
    Some(InfrastructurePort {
        name: None,
        port: container,
        target_port: None,
        host_port,
        protocol,
    })
}

fn parse_compose_port_mapping(mapping: &Mapping) -> Option<InfrastructurePort> {
    let port = yaml_u16(yaml_get(mapping, "target")?)?;
    Some(InfrastructurePort {
        name: yaml_get(mapping, "name").and_then(yaml_static_string),
        port,
        target_port: None,
        host_port: yaml_get(mapping, "published").and_then(yaml_u16),
        protocol: yaml_get(mapping, "protocol")
            .and_then(Value::as_str)
            .and_then(normalize_protocol),
    })
}

fn extract_compose_links(
    value: &Value,
    dependencies: &mut Vec<String>,
    aliases: &mut Vec<String>,
    collector: &mut Collector,
) {
    let Some(sequence) = value.as_array() else {
        collector.mark_incomplete("unsupported Compose links declaration was omitted");
        return;
    };
    for entry in sequence {
        let Some(link) = yaml_static_string(entry) else {
            collector.mark_incomplete("dynamic Compose link was omitted");
            continue;
        };
        let (dependency, alias) = link
            .split_once(':')
            .map_or((link.as_str(), None), |(dependency, alias)| {
                (dependency, Some(alias))
            });
        if let Some(dependency) = bounded_identifier(dependency) {
            dependencies.push(dependency);
        }
        if let Some(alias) = alias.and_then(bounded_identifier) {
            aliases.push(alias);
        }
    }
}

fn extract_host_aliases(value: &Value, aliases: &mut Vec<String>, collector: &mut Collector) {
    match value {
        Value::Array(sequence) => {
            for entry in sequence {
                let Some(specification) = yaml_static_string(entry) else {
                    collector.mark_incomplete("dynamic host alias was omitted");
                    continue;
                };
                let alias = specification
                    .split_once(':')
                    .map_or(specification.as_str(), |(alias, _)| alias);
                if let Some(alias) = bounded_identifier(alias) {
                    aliases.push(alias);
                }
            }
        }
        Value::Object(mapping) => {
            for key in mapping.keys() {
                if let Some(alias) = bounded_literal(key) {
                    aliases.push(alias);
                } else {
                    collector.mark_incomplete("dynamic host alias was omitted");
                }
            }
        }
        _ => collector.mark_incomplete("unsupported host alias declaration was omitted"),
    }
}

fn extract_name_set(value: &Value, output: &mut Vec<String>, collector: &mut Collector) {
    match value {
        Value::Array(sequence) => {
            for entry in sequence {
                if let Some(name) = yaml_static_string(entry) {
                    output.push(name);
                } else {
                    collector.mark_incomplete("dynamic dependency name was omitted");
                }
            }
        }
        Value::Object(mapping) => {
            for key in mapping.keys() {
                if let Some(name) = bounded_literal(key) {
                    output.push(name);
                } else {
                    collector.mark_incomplete("dynamic dependency name was omitted");
                }
            }
        }
        _ => collector.mark_incomplete("unsupported dependency declaration was omitted"),
    }
}

fn extract_yaml_environment_keys(
    value: &Value,
    output: &mut Vec<String>,
    collector: &mut Collector,
) {
    match value {
        Value::Object(mapping) => {
            for key in mapping.keys() {
                if let Some(key) = bounded_key_name(key) {
                    output.push(key);
                } else {
                    collector.mark_incomplete("invalid environment key name was omitted");
                }
            }
        }
        Value::Array(sequence) => {
            for entry in sequence {
                match entry {
                    Value::String(specification) => {
                        let key = specification
                            .split_once('=')
                            .map_or(specification.as_str(), |(key, _)| key);
                        if let Some(key) = bounded_key_name(key) {
                            output.push(key);
                        } else {
                            collector.mark_incomplete("invalid environment key name was omitted");
                        }
                    }
                    Value::Object(mapping) => {
                        if let Some(name) = yaml_get(mapping, "name").and_then(yaml_key_name) {
                            output.push(name);
                        } else {
                            collector.mark_incomplete("invalid environment key name was omitted");
                        }
                    }
                    _ => collector.mark_incomplete("unsupported environment entry was omitted"),
                }
            }
        }
        _ => collector.mark_incomplete("unsupported environment declaration was omitted"),
    }
}

fn extract_kubernetes_value(value: &Value, collector: &mut Collector, from_helm: bool) {
    let Some(root) = value.as_object() else {
        if !value.is_null() {
            collector.mark_incomplete("Kubernetes document root is not a mapping");
        }
        return;
    };
    let Some(kind) = yaml_get(root, "kind").and_then(yaml_static_string) else {
        if !root.is_empty() {
            collector.mark_incomplete("Kubernetes object kind was not statically visible");
        }
        return;
    };

    if kind == "List" {
        if let Some(items) = yaml_get(root, "items").and_then(Value::as_array) {
            for item in items {
                extract_kubernetes_value(item, collector, from_helm);
            }
        } else {
            collector.mark_incomplete("Kubernetes List items were not statically visible");
        }
        return;
    }

    let Some(metadata) = yaml_get(root, "metadata").and_then(Value::as_object) else {
        if !root.is_empty() {
            collector.mark_incomplete("Kubernetes object without static metadata was omitted");
        }
        return;
    };
    let Some(name) = yaml_get(metadata, "name").and_then(yaml_static_string) else {
        collector.mark_incomplete("dynamic Kubernetes resource name was omitted");
        return;
    };
    let namespace = match yaml_get(metadata, "namespace") {
        Some(namespace) => {
            if let Some(namespace) = yaml_static_string(namespace) {
                Some(namespace)
            } else {
                collector.mark_incomplete("dynamic Kubernetes namespace was omitted");
                None
            }
        }
        None => None,
    };

    let resource_kind = match kind.as_str() {
        "Service" => InfrastructureResourceKind::Service,
        "Ingress" => InfrastructureResourceKind::Ingress,
        _ => InfrastructureResourceKind::Other,
    };
    let mut resource = InfrastructureResource {
        kind: resource_kind,
        resource_type: kind.clone(),
        name: name.clone(),
        namespace: namespace.clone(),
        ports: Vec::new(),
        key_names: Vec::new(),
        dependencies: Vec::new(),
        selectors: Vec::new(),
        evidence: collector.evidence(InfrastructureEvidenceKind::Declaration),
    };

    match kind.as_str() {
        "Service" => extract_kubernetes_service(root, &mut resource, collector),
        "Ingress" => extract_kubernetes_ingress(root, &mut resource, collector),
        "ConfigMap" => {
            extract_mapping_key_names(root, &["data", "binaryData"], &mut resource.key_names);
        }
        "Secret" => {
            extract_mapping_key_names(root, &["data", "stringData"], &mut resource.key_names);
        }
        _ => {}
    }

    if let Some(deployment_kind) = kubernetes_deployment_kind(&kind, from_helm) {
        let mut unit = empty_unit(deployment_kind, name);
        unit.namespace = namespace;
        unit.evidence = collector.evidence(InfrastructureEvidenceKind::Declaration);
        extract_kubernetes_workload(root, &kind, &mut unit, collector);
        collector.push_unit(unit);
    }
    collector.push_resource(resource);
}

fn kubernetes_deployment_kind(kind: &str, from_helm: bool) -> Option<DeploymentKind> {
    match kind {
        "Deployment" => Some(DeploymentKind::KubernetesDeployment),
        "StatefulSet" => Some(DeploymentKind::KubernetesStatefulSet),
        "DaemonSet" => Some(DeploymentKind::KubernetesDaemonSet),
        "Job" => Some(DeploymentKind::KubernetesJob),
        "CronJob" => Some(DeploymentKind::KubernetesCronJob),
        "Pod" => Some(DeploymentKind::KubernetesPod),
        _ if from_helm && !kind.is_empty() => None,
        _ => None,
    }
}

fn extract_kubernetes_service(
    root: &Mapping,
    resource: &mut InfrastructureResource,
    collector: &mut Collector,
) {
    let Some(spec) = yaml_get(root, "spec").and_then(Value::as_object) else {
        return;
    };
    if let Some(ports) = yaml_get(spec, "ports").and_then(Value::as_array) {
        for entry in ports {
            let Some(port_mapping) = entry.as_object() else {
                collector.mark_incomplete("unsupported Kubernetes Service port was omitted");
                continue;
            };
            let Some(port) = yaml_get(port_mapping, "port").and_then(yaml_u16) else {
                collector.mark_incomplete("dynamic Kubernetes Service port was omitted");
                continue;
            };
            resource.ports.push(InfrastructurePort {
                name: yaml_get(port_mapping, "name").and_then(yaml_static_string),
                port,
                target_port: yaml_get(port_mapping, "targetPort").and_then(yaml_u16),
                host_port: yaml_get(port_mapping, "nodePort").and_then(yaml_u16),
                protocol: yaml_get(port_mapping, "protocol")
                    .and_then(Value::as_str)
                    .and_then(normalize_protocol),
            });
        }
    }
    if let Some(selector) = yaml_get(spec, "selector").and_then(Value::as_object) {
        extract_selectors(selector, &mut resource.selectors, collector);
    }
    if let Some(external_name) = yaml_get(spec, "externalName") {
        if let Some(external_name) = yaml_static_string(external_name) {
            resource.dependencies.push(external_name);
        } else {
            collector.mark_incomplete("dynamic Kubernetes external service name was omitted");
        }
    }
}

fn extract_kubernetes_ingress(
    root: &Mapping,
    resource: &mut InfrastructureResource,
    collector: &mut Collector,
) {
    let Some(spec) = yaml_get(root, "spec").and_then(Value::as_object) else {
        return;
    };
    if let Some(default_backend) = yaml_get(spec, "defaultBackend").and_then(Value::as_object) {
        extract_ingress_backend(default_backend, resource, collector);
    }
    if let Some(rules) = yaml_get(spec, "rules").and_then(Value::as_array) {
        for rule in rules {
            let Some(http) = rule
                .as_object()
                .and_then(|mapping| yaml_get(mapping, "http"))
                .and_then(Value::as_object)
            else {
                continue;
            };
            let Some(paths) = yaml_get(http, "paths").and_then(Value::as_array) else {
                continue;
            };
            for path in paths {
                if let Some(backend) = path
                    .as_object()
                    .and_then(|mapping| yaml_get(mapping, "backend"))
                    .and_then(Value::as_object)
                {
                    extract_ingress_backend(backend, resource, collector);
                }
            }
        }
    }
}

fn extract_ingress_backend(
    backend: &Mapping,
    resource: &mut InfrastructureResource,
    collector: &mut Collector,
) {
    if let Some(service) = yaml_get(backend, "service").and_then(Value::as_object) {
        if let Some(name) = yaml_get(service, "name").and_then(yaml_static_string) {
            resource.dependencies.push(name);
        } else {
            collector.mark_incomplete("dynamic Kubernetes Ingress backend was omitted");
        }
        if let Some(port_mapping) = yaml_get(service, "port").and_then(Value::as_object)
            && let Some(port) = yaml_get(port_mapping, "number").and_then(yaml_u16)
        {
            resource.ports.push(InfrastructurePort {
                name: yaml_get(port_mapping, "name").and_then(yaml_static_string),
                port,
                target_port: None,
                host_port: None,
                protocol: None,
            });
        }
    } else if let Some(name) = yaml_get(backend, "serviceName").and_then(yaml_static_string) {
        resource.dependencies.push(name);
        if let Some(port) = yaml_get(backend, "servicePort").and_then(yaml_u16) {
            resource.ports.push(InfrastructurePort {
                name: None,
                port,
                target_port: None,
                host_port: None,
                protocol: None,
            });
        }
    }
}

fn extract_kubernetes_workload(
    root: &Mapping,
    kind: &str,
    unit: &mut DeploymentUnit,
    collector: &mut Collector,
) {
    let Some(spec) = kubernetes_pod_spec(root, kind) else {
        collector.mark_incomplete("Kubernetes workload pod specification was not visible");
        return;
    };

    for container_field in ["initContainers", "containers"] {
        if let Some(containers) = yaml_get(spec, container_field).and_then(Value::as_array) {
            for container in containers {
                if let Some(container) = container.as_object() {
                    extract_kubernetes_container(container, unit, collector);
                } else {
                    collector.mark_incomplete("unsupported Kubernetes container was omitted");
                }
            }
        }
    }
    if let Some(host_aliases) = yaml_get(spec, "hostAliases").and_then(Value::as_array) {
        for alias in host_aliases {
            if let Some(hostnames) = alias
                .as_object()
                .and_then(|mapping| yaml_get(mapping, "hostnames"))
                .and_then(Value::as_array)
            {
                for hostname in hostnames {
                    if let Some(hostname) = yaml_static_string(hostname) {
                        unit.host_aliases.push(hostname);
                    } else {
                        collector.mark_incomplete("dynamic Kubernetes host alias was omitted");
                    }
                }
            }
        }
    }
    extract_kubernetes_volume_dependencies(spec, &mut unit.dependencies, collector);

    if kind != "Pod"
        && let Some(selector) = yaml_get(root, "spec")
            .and_then(Value::as_object)
            .and_then(|mapping| yaml_get(mapping, "selector"))
            .and_then(Value::as_object)
            .and_then(|mapping| yaml_get(mapping, "matchLabels"))
            .and_then(Value::as_object)
    {
        extract_selectors(selector, &mut unit.selectors, collector);
    }
}

fn kubernetes_pod_spec<'a>(root: &'a Mapping, kind: &str) -> Option<&'a Mapping> {
    let spec = yaml_get(root, "spec")?.as_object()?;
    match kind {
        "Pod" => Some(spec),
        "CronJob" => yaml_get(spec, "jobTemplate")
            .and_then(Value::as_object)
            .and_then(|job| yaml_get(job, "spec"))
            .and_then(Value::as_object)
            .and_then(|job_spec| yaml_get(job_spec, "template"))
            .and_then(Value::as_object)
            .and_then(|template| yaml_get(template, "spec"))
            .and_then(Value::as_object),
        _ => yaml_get(spec, "template")
            .and_then(Value::as_object)
            .and_then(|template| yaml_get(template, "spec"))
            .and_then(Value::as_object),
    }
}

fn extract_kubernetes_container(
    container: &Mapping,
    unit: &mut DeploymentUnit,
    collector: &mut Collector,
) {
    if let Some(image_value) = yaml_get(container, "image") {
        if let Some(image) = yaml_static_string(image_value) {
            unit.images.push(image);
        } else {
            collector.mark_incomplete("dynamic Kubernetes image was omitted");
        }
    }
    if let Some(ports) = yaml_get(container, "ports").and_then(Value::as_array) {
        for entry in ports {
            let Some(port_mapping) = entry.as_object() else {
                collector.mark_incomplete("unsupported Kubernetes container port was omitted");
                continue;
            };
            let Some(port) = yaml_get(port_mapping, "containerPort").and_then(yaml_u16) else {
                collector.mark_incomplete("dynamic Kubernetes container port was omitted");
                continue;
            };
            unit.ports.push(InfrastructurePort {
                name: yaml_get(port_mapping, "name").and_then(yaml_static_string),
                port,
                target_port: None,
                host_port: yaml_get(port_mapping, "hostPort").and_then(yaml_u16),
                protocol: yaml_get(port_mapping, "protocol")
                    .and_then(Value::as_str)
                    .and_then(normalize_protocol),
            });
        }
    }
    if let Some(environment) = yaml_get(container, "env") {
        extract_kubernetes_environment(environment, unit, collector);
    }
    if let Some(environment_from) = yaml_get(container, "envFrom").and_then(Value::as_array) {
        for entry in environment_from {
            let Some(mapping) = entry.as_object() else {
                continue;
            };
            for (field, kind) in [("configMapRef", "ConfigMap"), ("secretRef", "Secret")] {
                if let Some(name) = yaml_get(mapping, field)
                    .and_then(Value::as_object)
                    .and_then(|reference| yaml_get(reference, "name"))
                    .and_then(yaml_static_string)
                {
                    unit.dependencies.push(format!("{kind}/{name}"));
                }
            }
        }
    }
}

fn extract_kubernetes_environment(
    value: &Value,
    unit: &mut DeploymentUnit,
    collector: &mut Collector,
) {
    let Some(sequence) = value.as_array() else {
        collector.mark_incomplete("unsupported Kubernetes environment declaration was omitted");
        return;
    };
    for entry in sequence {
        let Some(mapping) = entry.as_object() else {
            collector.mark_incomplete("unsupported Kubernetes environment entry was omitted");
            continue;
        };
        if let Some(name) = yaml_get(mapping, "name").and_then(yaml_key_name) {
            unit.environment_keys.push(name);
        } else {
            collector.mark_incomplete("invalid Kubernetes environment key was omitted");
        }
        let Some(value_from) = yaml_get(mapping, "valueFrom").and_then(Value::as_object) else {
            continue;
        };
        for (field, kind) in [("configMapKeyRef", "ConfigMap"), ("secretKeyRef", "Secret")] {
            if let Some(reference) = yaml_get(value_from, field).and_then(Value::as_object)
                && let Some(name) = yaml_get(reference, "name").and_then(yaml_static_string)
            {
                unit.dependencies.push(format!("{kind}/{name}"));
            }
        }
    }
}

fn extract_kubernetes_volume_dependencies(
    spec: &Mapping,
    output: &mut Vec<String>,
    collector: &mut Collector,
) {
    let Some(volumes) = yaml_get(spec, "volumes").and_then(Value::as_array) else {
        return;
    };
    for volume in volumes {
        let Some(mapping) = volume.as_object() else {
            continue;
        };
        for (field, kind) in [("configMap", "ConfigMap"), ("secret", "Secret")] {
            let Some(reference) = yaml_get(mapping, field).and_then(Value::as_object) else {
                continue;
            };
            let name_field = if field == "secret" {
                "secretName"
            } else {
                "name"
            };
            if let Some(name) = yaml_get(reference, name_field).and_then(yaml_static_string) {
                output.push(format!("{kind}/{name}"));
            } else {
                collector.mark_incomplete("dynamic Kubernetes volume reference was omitted");
            }
        }
    }
}

fn extract_selectors(
    mapping: &Mapping,
    output: &mut Vec<InfrastructureSelector>,
    collector: &mut Collector,
) {
    for (key, value) in mapping {
        match (bounded_literal(key), yaml_static_string(value)) {
            (Some(key), Some(value)) => output.push(InfrastructureSelector { key, value }),
            _ => collector.mark_incomplete("dynamic selector was omitted"),
        }
    }
}

fn extract_mapping_key_names(root: &Mapping, fields: &[&str], output: &mut Vec<String>) {
    for field in fields {
        if let Some(mapping) = yaml_get(root, field).and_then(Value::as_object) {
            for key in mapping.keys() {
                if let Some(key) = bounded_key_name(key) {
                    output.push(key);
                }
            }
        }
    }
}

fn is_helm_values_path(source_path: &str) -> bool {
    let basename = source_path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(source_path)
        .to_ascii_lowercase();
    basename == "values.yaml"
        || basename == "values.yml"
        || basename.starts_with("values-")
        || basename.starts_with("values.")
}

fn mask_helm_templates(input: &str) -> (String, bool) {
    let mut output = String::with_capacity(input.len());
    let mut has_template = false;

    for segment in input.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let newline = if segment.ends_with('\n') { "\n" } else { "" };
        let trimmed = line.trim();
        if trimmed.starts_with("{{") && trimmed.ends_with("}}") {
            has_template = true;
            output.push_str(newline);
            continue;
        }

        let mut remainder = line;
        while let Some(start) = remainder.find("{{") {
            has_template = true;
            output.push_str(&remainder[..start]);
            let Some(relative_end) = remainder[start + 2..].find("}}") else {
                output.push_str(HELM_DYNAMIC_MARKER);
                remainder = "";
                break;
            };
            output.push_str(HELM_DYNAMIC_MARKER);
            remainder = &remainder[start + 2 + relative_end + 2..];
        }
        output.push_str(remainder);
        output.push_str(newline);
    }

    (output, has_template)
}

fn extract_helm_values(value: &Value, collector: &mut Collector) {
    let mut environment_keys = Vec::new();
    collect_helm_environment_keys(value, None, &mut environment_keys, collector);
    collector.document.environment_keys.extend(environment_keys);
}

fn collect_helm_environment_keys(
    value: &Value,
    parent_key: Option<&str>,
    output: &mut Vec<String>,
    collector: &mut Collector,
) {
    match value {
        Value::Object(mapping) => {
            let environment_context = parent_key.is_some_and(is_environment_container_key);
            for (key, nested) in mapping {
                let key = key.as_str();
                if environment_context {
                    if let Some(key) = bounded_key_name(key) {
                        output.push(key);
                    }
                    continue;
                }
                if contains_dynamic(key) {
                    collector.mark_incomplete("dynamic Helm values key was omitted");
                    continue;
                }
                collect_helm_environment_keys(nested, Some(key), output, collector);
            }
        }
        Value::Array(sequence) if parent_key.is_some_and(is_environment_container_key) => {
            for entry in sequence {
                if let Some(mapping) = entry.as_object()
                    && let Some(name) = yaml_get(mapping, "name").and_then(yaml_key_name)
                {
                    output.push(name);
                }
            }
        }
        Value::Array(sequence) => {
            for entry in sequence {
                collect_helm_environment_keys(entry, parent_key, output, collector);
            }
        }
        Value::String(string) if contains_dynamic(string) => {
            collector.mark_incomplete("dynamic Helm value was omitted");
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn is_environment_container_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "env" | "environment" | "environmentvariables" | "environment_variables" | "extraenv"
    )
}

fn enforce_hcl_budget(body: &Body) -> Result<(), InfrastructureExtractionError> {
    let mut stack = vec![HclNode::Body(body, 1_usize)];
    let mut items = 0_usize;
    while let Some(node) = stack.pop() {
        items += 1;
        if items > MAX_INFRASTRUCTURE_ITEMS {
            return Err(InfrastructureExtractionError::TooManyItems {
                maximum: MAX_INFRASTRUCTURE_ITEMS,
            });
        }
        let depth = match node {
            HclNode::Body(body, depth) => {
                for structure in &body.0 {
                    match structure {
                        Structure::Attribute(attribute) => {
                            stack.push(HclNode::Expression(&attribute.expr, depth + 1));
                        }
                        Structure::Block(block) => {
                            stack.push(HclNode::Body(&block.body, depth + 1));
                        }
                    }
                }
                depth
            }
            HclNode::Expression(expression, depth) => {
                match expression {
                    Expression::Array(array) => {
                        stack.extend(
                            array
                                .iter()
                                .map(|item| HclNode::Expression(item, depth + 1)),
                        );
                    }
                    Expression::Object(object) => {
                        for (key, value) in object {
                            if let ObjectKey::Expression(key) = key {
                                stack.push(HclNode::Expression(key, depth + 1));
                            }
                            stack.push(HclNode::Expression(value, depth + 1));
                        }
                    }
                    Expression::Parenthesis(expression) => {
                        stack.push(HclNode::Expression(expression, depth + 1));
                    }
                    _ => {}
                }
                depth
            }
        };
        if depth > MAX_INFRASTRUCTURE_DEPTH {
            return Err(InfrastructureExtractionError::StructureTooDeep {
                maximum: MAX_INFRASTRUCTURE_DEPTH,
            });
        }
    }
    Ok(())
}

enum HclNode<'a> {
    Body(&'a Body, usize),
    Expression(&'a Expression, usize),
}

fn visit_terraform_body(body: &Body, collector: &mut Collector) {
    for structure in &body.0 {
        let Structure::Block(block) = structure else {
            continue;
        };
        if block.identifier() == "resource" {
            extract_terraform_resource(block, collector);
        } else {
            visit_terraform_body(&block.body, collector);
        }
    }
}

fn extract_terraform_resource(block: &Block, collector: &mut Collector) {
    let Some(resource_type) = block.labels.first().and_then(hcl_label_string) else {
        collector.mark_incomplete("Terraform resource type was not statically visible");
        return;
    };
    let Some(local_name) = block.labels.get(1).and_then(hcl_label_string) else {
        collector.mark_incomplete("Terraform resource name was not statically visible");
        return;
    };

    let name_expression =
        hcl_attribute(&block.body, "name").or_else(|| hcl_attribute(&block.body, "db_name"));
    let literal_name = name_expression.and_then(hcl_literal_string);
    if name_expression.is_some() && literal_name.is_none() {
        collector.mark_incomplete("dynamic Terraform resource name was omitted");
    }
    let name = literal_name.unwrap_or(local_name);
    let kind = terraform_resource_kind(&resource_type);
    let namespace_expression = hcl_attribute(&block.body, "namespace");
    let namespace = namespace_expression.and_then(hcl_literal_string);
    if namespace_expression.is_some() && namespace.is_none() {
        collector.mark_incomplete("dynamic Terraform namespace was omitted");
    }
    let mut resource = InfrastructureResource {
        kind,
        resource_type: resource_type.clone(),
        name: name.clone(),
        namespace,
        ports: Vec::new(),
        key_names: Vec::new(),
        dependencies: Vec::new(),
        selectors: Vec::new(),
        evidence: collector.evidence(InfrastructureEvidenceKind::Declaration),
    };
    let mut images = Vec::new();
    let mut environment_keys = Vec::new();
    collect_terraform_attributes(
        &block.body,
        &mut images,
        &mut resource.ports,
        &mut environment_keys,
        &mut resource.dependencies,
        &mut resource.key_names,
        collector,
    );

    if terraform_is_deployment(&resource_type) {
        let mut unit = empty_unit(DeploymentKind::TerraformResource, name);
        unit.namespace.clone_from(&resource.namespace);
        unit.images = images;
        unit.ports.clone_from(&resource.ports);
        unit.environment_keys = environment_keys;
        unit.dependencies.clone_from(&resource.dependencies);
        if kind == InfrastructureResourceKind::Service {
            unit.service_names.push(resource.name.clone());
        }
        unit.evidence = collector.evidence(InfrastructureEvidenceKind::Declaration);
        collector.push_unit(unit);
    }
    collector.push_resource(resource);
}

fn collect_terraform_attributes(
    body: &Body,
    images: &mut Vec<String>,
    ports: &mut Vec<InfrastructurePort>,
    environment_keys: &mut Vec<String>,
    dependencies: &mut Vec<String>,
    key_names: &mut Vec<String>,
    collector: &mut Collector,
) {
    for structure in &body.0 {
        match structure {
            Structure::Attribute(attribute) => {
                let key = attribute.key();
                if is_image_attribute(key) {
                    if let Some(image) = hcl_literal_string(&attribute.expr) {
                        images.push(image);
                    } else {
                        collector.mark_incomplete("dynamic Terraform image was omitted");
                    }
                }
                if is_port_attribute(key) {
                    if let Some(port) = hcl_literal_u16(&attribute.expr) {
                        ports.push(InfrastructurePort {
                            name: None,
                            port,
                            target_port: None,
                            host_port: None,
                            protocol: None,
                        });
                    } else if !matches!(attribute.expr, Expression::Array(_)) {
                        collector.mark_incomplete("dynamic Terraform port was omitted");
                    }
                }
                if is_environment_container_key(key) {
                    collect_hcl_object_keys(
                        &attribute.expr,
                        environment_keys,
                        collector,
                        "dynamic Terraform environment key was omitted",
                    );
                }
                if key == "depends_on" {
                    collect_hcl_dependencies(&attribute.expr, dependencies, collector);
                }
                if matches!(key, "data" | "string_data" | "binary_data") {
                    collect_hcl_object_keys(
                        &attribute.expr,
                        key_names,
                        collector,
                        "dynamic Terraform configuration key was omitted",
                    );
                }
                if let Expression::Array(values) = &attribute.expr
                    && is_port_attribute(key)
                {
                    for value in values {
                        if let Some(port) = hcl_literal_u16(value) {
                            ports.push(InfrastructurePort {
                                name: None,
                                port,
                                target_port: None,
                                host_port: None,
                                protocol: None,
                            });
                        } else {
                            collector.mark_incomplete("dynamic Terraform port was omitted");
                        }
                    }
                }
            }
            Structure::Block(block) => {
                if matches!(
                    block.identifier(),
                    "environment" | "env" | "environment_variable"
                ) && let Some(name) =
                    hcl_attribute(&block.body, "name").and_then(hcl_literal_string)
                    && let Some(name) = bounded_key_name(&name)
                {
                    environment_keys.push(name);
                }
                collect_terraform_attributes(
                    &block.body,
                    images,
                    ports,
                    environment_keys,
                    dependencies,
                    key_names,
                    collector,
                );
            }
        }
    }
}

fn collect_hcl_object_keys(
    expression: &Expression,
    output: &mut Vec<String>,
    collector: &mut Collector,
    warning: &'static str,
) {
    let Expression::Object(object) = expression else {
        collector.mark_incomplete(warning);
        return;
    };
    for key in object.keys() {
        let key = match key {
            ObjectKey::Identifier(identifier) => bounded_key_name(identifier),
            ObjectKey::Expression(Expression::String(string)) => bounded_key_name(string),
            _ => None,
        };
        if let Some(key) = key {
            output.push(key);
        } else {
            collector.mark_incomplete(warning);
        }
    }
}

fn collect_hcl_dependencies(
    expression: &Expression,
    output: &mut Vec<String>,
    collector: &mut Collector,
) {
    match expression {
        Expression::Array(values) => {
            for value in values {
                collect_hcl_dependencies(value, output, collector);
            }
        }
        Expression::Traversal(_) | Expression::Variable(_) => {
            let dependency = expression.to_string();
            if is_safe_dependency(&dependency) {
                output.push(dependency);
            } else {
                collector.mark_incomplete("dynamic Terraform dependency was omitted");
            }
        }
        Expression::Parenthesis(expression) => {
            collect_hcl_dependencies(expression, output, collector);
        }
        _ => collector.mark_incomplete("dynamic Terraform dependency was omitted"),
    }
}

fn terraform_resource_kind(resource_type: &str) -> InfrastructureResourceKind {
    let resource_type = resource_type.to_ascii_lowercase();
    if resource_type.contains("ingress") {
        InfrastructureResourceKind::Ingress
    } else if resource_type.contains("topic")
        || resource_type.contains("sns_")
        || resource_type.contains("pubsub")
    {
        InfrastructureResourceKind::Topic
    } else if resource_type.contains("queue")
        || resource_type.contains("sqs_")
        || resource_type.contains("servicebus_queue")
    {
        InfrastructureResourceKind::Queue
    } else if resource_type.contains("database")
        || resource_type.contains("db_instance")
        || resource_type.contains("rds_cluster")
        || resource_type.contains("sql_database")
    {
        InfrastructureResourceKind::Database
    } else if resource_type.contains("service")
        || resource_type.contains("deployment")
        || resource_type.contains("lambda_function")
        || resource_type.contains("cloud_run")
        || resource_type.contains("web_app")
    {
        InfrastructureResourceKind::Service
    } else {
        InfrastructureResourceKind::Other
    }
}

fn terraform_is_deployment(resource_type: &str) -> bool {
    let resource_type = resource_type.to_ascii_lowercase();
    [
        "deployment",
        "ecs_service",
        "lambda_function",
        "cloud_run",
        "container_app",
        "web_app",
        "kubernetes_pod",
        "kubernetes_job",
        "nomad_job",
    ]
    .iter()
    .any(|needle| resource_type.contains(needle))
}

fn hcl_attribute<'a>(body: &'a Body, key: &str) -> Option<&'a Expression> {
    body.0.iter().find_map(|structure| match structure {
        Structure::Attribute(attribute) if attribute.key() == key => Some(&attribute.expr),
        Structure::Attribute(_) | Structure::Block(_) => None,
    })
}

fn hcl_label_string(label: &BlockLabel) -> Option<String> {
    let value = match label {
        BlockLabel::Identifier(identifier) => identifier.as_str(),
        BlockLabel::String(string) => string,
    };
    bounded_identifier(value)
}

fn hcl_literal_string(expression: &Expression) -> Option<String> {
    match expression {
        Expression::String(value) => bounded_literal(value),
        Expression::Parenthesis(expression) => hcl_literal_string(expression),
        _ => None,
    }
}

fn hcl_literal_u16(expression: &Expression) -> Option<u16> {
    match expression {
        Expression::Number(number) => number.to_string().parse().ok(),
        Expression::Parenthesis(expression) => hcl_literal_u16(expression),
        _ => None,
    }
}

fn is_image_attribute(key: &str) -> bool {
    matches!(
        key,
        "image" | "container_image" | "image_uri" | "image_name"
    )
}

fn is_port_attribute(key: &str) -> bool {
    matches!(
        key,
        "port"
            | "ports"
            | "container_port"
            | "container_ports"
            | "target_port"
            | "host_port"
            | "service_port"
    )
}

fn yaml_get<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a Value> {
    mapping.get(key)
}

fn yaml_static_string(value: &Value) -> Option<String> {
    value.as_str().and_then(bounded_literal)
}

fn yaml_key_name(value: &Value) -> Option<String> {
    value.as_str().and_then(bounded_key_name)
}

fn yaml_u16(value: &Value) -> Option<u16> {
    match value {
        Value::Number(number) => number.as_u64().and_then(to_u16),
        Value::String(string) if !contains_dynamic(string) => string.parse().ok(),
        _ => None,
    }
}

fn to_u16(value: u64) -> Option<u16> {
    u16::try_from(value).ok()
}

fn normalize_protocol(value: &str) -> Option<String> {
    match value.to_ascii_lowercase().as_str() {
        "tcp" => Some("tcp".to_owned()),
        "udp" => Some("udp".to_owned()),
        "sctp" => Some("sctp".to_owned()),
        _ => None,
    }
}

fn bounded_literal(value: &str) -> Option<String> {
    if value.is_empty()
        || value.len() > MAX_INFRASTRUCTURE_STRING_BYTES
        || value.chars().any(char::is_control)
        || contains_dynamic(value)
    {
        None
    } else {
        Some(value.to_owned())
    }
}

fn bounded_identifier(value: &str) -> Option<String> {
    let value = bounded_literal(value)?;
    if value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "._:/@+-".contains(character))
    {
        Some(value)
    } else {
        None
    }
}

fn bounded_key_name(value: &str) -> Option<String> {
    if value.is_empty()
        || value.len() > MAX_INFRASTRUCTURE_STRING_BYTES
        || contains_dynamic(value)
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-')
        })
    {
        None
    } else {
        Some(value.to_owned())
    }
}

fn contains_dynamic(value: &str) -> bool {
    value.contains("${")
        || value.contains("{{")
        || value.contains(HELM_DYNAMIC_MARKER)
        || value.contains("%{")
        || value.contains("$(")
}

fn is_safe_dependency(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_INFRASTRUCTURE_STRING_BYTES
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-[]".contains(character))
}

fn empty_unit(kind: DeploymentKind, name: String) -> DeploymentUnit {
    DeploymentUnit {
        kind,
        name,
        namespace: None,
        images: Vec::new(),
        ports: Vec::new(),
        environment_keys: Vec::new(),
        dependencies: Vec::new(),
        service_names: Vec::new(),
        host_aliases: Vec::new(),
        selectors: Vec::new(),
        evidence: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_extracts_explicit_service_metadata() {
        let input = r#"
services:
  api:
    image: ghcr.io/acme/api:1
    ports:
      - "8080:80/tcp"
    environment:
      DATABASE_URL: must-not-persist
      LOG_LEVEL: info
    depends_on:
      db:
        condition: service_healthy
    links:
      - "db:database"
    extra_hosts:
      - "gateway:127.0.0.1"
  db:
    image: postgres:17
"#;

        let document = extract_docker_compose("compose.yaml", input).expect("valid Compose");

        assert_eq!(
            document.deployment_units[0],
            DeploymentUnit {
                kind: DeploymentKind::DockerComposeService,
                name: "api".to_owned(),
                namespace: None,
                images: vec!["ghcr.io/acme/api:1".to_owned()],
                ports: vec![InfrastructurePort {
                    name: None,
                    port: 80,
                    target_port: None,
                    host_port: Some(8080),
                    protocol: Some("tcp".to_owned()),
                }],
                environment_keys: vec!["DATABASE_URL".to_owned(), "LOG_LEVEL".to_owned()],
                dependencies: vec!["db".to_owned()],
                service_names: vec!["api".to_owned()],
                host_aliases: vec!["database".to_owned(), "gateway".to_owned()],
                selectors: Vec::new(),
                evidence: vec![InfrastructureEvidence {
                    kind: InfrastructureEvidenceKind::Declaration,
                    line: None,
                }],
            }
        );
    }

    #[test]
    fn compose_never_serializes_environment_or_secret_values() {
        let secret = "postgres://admin:very-secret@db/orders";
        let input = format!(
            "services:\n  api:\n    environment:\n      DATABASE_URL: {secret}\nsecrets:\n  token:\n    data: another-secret\n"
        );

        let document = extract_docker_compose("compose.yaml", &input).expect("valid Compose");
        let serialized = serde_json::to_string(&document).expect("serializable document");

        assert!(!serialized.contains("very-secret") && !serialized.contains("another-secret"));
    }

    #[test]
    fn kubernetes_extracts_workload_service_ingress_and_secret_keys() {
        let input = r"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: orders
  namespace: production
spec:
  selector:
    matchLabels:
      app: orders
  template:
    spec:
      containers:
        - name: app
          image: ghcr.io/acme/orders:2
          ports:
            - name: http
              containerPort: 8080
          env:
            - name: DATABASE_URL
              valueFrom:
                secretKeyRef:
                  name: orders-secret
                  key: database-url
---
apiVersion: v1
kind: Service
metadata:
  name: orders
spec:
  selector:
    app: orders
  ports:
    - port: 80
      targetPort: 8080
---
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: orders
spec:
  rules:
    - http:
        paths:
          - path: /
            backend:
              service:
                name: orders
                port:
                  number: 80
---
apiVersion: v1
kind: Secret
metadata:
  name: orders-secret
data:
  database-url: cG9zdGdyZXM6Ly9zZWNyZXQ=
";

        let document = extract_kubernetes("k8s.yaml", input).expect("valid Kubernetes");

        assert_eq!(
            document.deployment_units[0].environment_keys,
            ["DATABASE_URL"]
        );
        assert_eq!(
            document
                .resources
                .iter()
                .find(|resource| resource.resource_type == "Secret")
                .expect("secret resource")
                .key_names,
            ["database-url"]
        );
    }

    #[test]
    fn kubernetes_does_not_infer_service_to_workload_link_from_selectors() {
        let input = r"
kind: Deployment
metadata: { name: api }
spec:
  selector: { matchLabels: { app: api } }
  template: { spec: { containers: [{ name: api, image: api:1 }] } }
---
kind: Service
metadata: { name: api }
spec:
  selector: { app: api }
";

        let document = extract_kubernetes("objects.yaml", input).expect("valid Kubernetes");

        assert_eq!(
            document.deployment_units[0].service_names,
            Vec::<String>::new()
        );
    }

    #[test]
    fn kubernetes_never_serializes_secret_or_environment_values() {
        let input = r"
kind: Secret
metadata: { name: credentials }
stringData:
  password: super-secret-password
  token: ghp_super_secret
---
kind: Pod
metadata: { name: api }
spec:
  containers:
    - name: api
      image: api:1
      env:
        - name: PASSWORD
          value: another-secret
";

        let document = extract_kubernetes("objects.yaml", input).expect("valid Kubernetes");
        let serialized = serde_json::to_string(&document).expect("serializable document");

        assert!(
            !serialized.contains("super-secret-password")
                && !serialized.contains("ghp_super_secret")
                && !serialized.contains("another-secret")
        );
    }

    #[test]
    fn helm_template_retains_static_names_and_marks_dynamic_values_incomplete() {
        let input = r"
apiVersion: apps/v1
kind: Deployment
metadata:
  name: orders
spec:
  template:
    spec:
      containers:
        - name: orders
          image: {{ .Values.image.repository }}
          env:
            - name: API_TOKEN
              value: {{ .Values.secretToken }}
";

        let document = extract_helm("templates/deployment.yaml", input).expect("bounded Helm");

        assert!(document.incomplete && document.deployment_units[0].images.is_empty());
    }

    #[test]
    fn helm_values_retains_only_environment_key_names() {
        let input = r"
image:
  repository: ghcr.io/acme/api
env:
  DATABASE_URL: postgres://admin:secret@db/orders
  API_TOKEN: ghp_secret
nested:
  extraEnv:
    - name: LOG_LEVEL
      value: debug
";

        let document = extract_helm("values.yaml", input).expect("valid Helm values");
        let serialized = serde_json::to_string(&document).expect("serializable document");

        assert_eq!(
            document.environment_keys,
            ["API_TOKEN", "DATABASE_URL", "LOG_LEVEL"]
        );
        assert!(!serialized.contains("postgres://") && !serialized.contains("ghp_secret"));
    }

    #[test]
    fn terraform_extracts_literal_resources_and_deployment_metadata() {
        let input = r#"
resource "aws_sqs_queue" "jobs" {
  name = "jobs"
}

resource "aws_db_instance" "orders" {
  db_name  = "orders"
  password = "never-persist"
}

resource "aws_ecs_service" "api" {
  name       = "api"
  image      = "ghcr.io/acme/api:3"
  port       = 8080
  depends_on = [aws_db_instance.orders, aws_sqs_queue.jobs]

  environment {
    name  = "DATABASE_URL"
    value = "postgres://admin:secret@db/orders"
  }
}
"#;

        let document = extract_terraform("main.tf", input).expect("valid Terraform");

        assert_eq!(
            document
                .resources
                .iter()
                .map(|resource| resource.kind)
                .collect::<Vec<_>>(),
            [
                InfrastructureResourceKind::Service,
                InfrastructureResourceKind::Queue,
                InfrastructureResourceKind::Database,
            ]
        );
        assert_eq!(
            document.deployment_units[0].dependencies,
            ["aws_db_instance.orders", "aws_sqs_queue.jobs"]
        );
    }

    #[test]
    fn terraform_never_serializes_credentials_or_connection_strings() {
        let input = r#"
resource "aws_db_instance" "orders" {
  username = "administrator"
  password = "super-secret-password"
  url      = "postgres://administrator:super-secret-password@db/orders"
}
"#;

        let document = extract_terraform("main.tf", input).expect("valid Terraform");
        let serialized = serde_json::to_string(&document).expect("serializable document");

        assert!(
            !serialized.contains("administrator")
                && !serialized.contains("super-secret-password")
                && !serialized.contains("postgres://")
        );
    }

    #[test]
    fn extraction_is_deterministically_sorted_and_deduplicated() {
        let first = r"
services:
  zeta:
    environment: [B=2, A=1, A=3]
  alpha:
    image: alpha:1
";
        let second = r"
services:
  alpha:
    image: alpha:1
  zeta:
    environment: [A=3, A=1, B=2]
";

        let left = extract_docker_compose("compose.yaml", first).expect("valid Compose");
        let right = extract_docker_compose("compose.yaml", second).expect("valid Compose");

        assert_eq!(left, right);
    }

    #[test]
    fn oversized_input_is_rejected_before_parsing() {
        let input = "x".repeat(MAX_INFRASTRUCTURE_INPUT_BYTES + 1);

        let error = extract_kubernetes("large.yaml", &input).expect_err("oversized input");

        assert!(matches!(
            error,
            InfrastructureExtractionError::InputTooLarge { .. }
        ));
    }

    #[test]
    fn deeply_nested_yaml_parser_bomb_is_rejected() {
        let mut input = String::new();
        for _ in 0..=MAX_INFRASTRUCTURE_DEPTH {
            input.push_str("a: {");
        }
        input.push_str("null");
        for _ in 0..=MAX_INFRASTRUCTURE_DEPTH {
            input.push('}');
        }

        let error = extract_kubernetes("bomb.yaml", &input).expect_err("depth bomb");

        assert!(matches!(
            error,
            InfrastructureExtractionError::StructureTooDeep { .. }
        ));
    }

    #[test]
    fn excessive_yaml_items_are_rejected() {
        let input = format!("items:\n{}", "  - null\n".repeat(MAX_INFRASTRUCTURE_ITEMS));

        let error = extract_kubernetes("bomb.yaml", &input).expect_err("item bomb");

        assert!(matches!(
            error,
            InfrastructureExtractionError::TooManyItems { .. }
        ));
    }

    #[test]
    fn malformed_errors_do_not_echo_source_or_secret_values() {
        let secret = "super-secret-token";
        let input = format!("services: [\n  {secret}");

        let error = extract_docker_compose("compose.yaml", &input).expect_err("malformed YAML");

        assert!(!error.to_string().contains(secret));
    }

    #[test]
    fn malformed_terraform_error_does_not_echo_source() {
        let secret = "super-secret-password";
        let input = format!("resource \"aws_db_instance\" \"db\" {{ password = \"{secret}\"");

        let error = extract_terraform("main.tf", &input).expect_err("malformed HCL");

        assert!(!error.to_string().contains(secret));
    }

    #[test]
    fn deeply_nested_hcl_parser_bomb_is_rejected() {
        let mut input = "value = ".to_owned();
        input.push_str(&"[".repeat(MAX_INFRASTRUCTURE_DEPTH + 1));
        input.push('0');
        input.push_str(&"]".repeat(MAX_INFRASTRUCTURE_DEPTH + 1));

        let error = extract_terraform("bomb.tf", &input).expect_err("depth bomb");

        assert!(matches!(
            error,
            InfrastructureExtractionError::StructureTooDeep { .. }
        ));
    }
}

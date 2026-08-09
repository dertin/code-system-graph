//! Owned, deterministic protobuf and generated gRPC source contracts.

use std::collections::{BTreeMap, BTreeSet};

use proto_parser::{
    Element, Enum, FieldCommon, Group, ImportKind, Literal, Message, Parser, ProtoOption, Rpc, Service
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ExtractionBudgets, ExtractionLimitExceeded, ExtractionTracker, SourceLanguage};

const MAX_FIELD_NUMBER: i64 = 536_870_911;
const FIRST_RESERVED_IMPLEMENTATION_FIELD: i64 = 19_000;
const LAST_RESERVED_IMPLEMENTATION_FIELD: i64 = 19_999;

/// Protobuf language mode declared by a contract.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtoSyntax {
    /// Protocol Buffers version 2 syntax.
    Proto2,
    /// Protocol Buffers version 3 syntax.
    Proto3,
    /// Editions syntax and its declared edition identifier.
    Edition {
        /// Edition identifier, such as `2023`.
        version: String,
    },
}

/// Cardinality attached to a protobuf field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtoFieldCardinality {
    /// An unlabeled singular field.
    Singular,
    /// An explicitly optional field.
    Optional,
    /// A required proto2 field.
    Required,
    /// A repeated field, including maps.
    Repeated,
}

/// Protobuf wire encoding used by a field value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtoWireType {
    /// Wire type 0.
    Varint,
    /// Wire type 1.
    Fixed64,
    /// Wire type 2.
    LengthDelimited,
    /// Wire type 3 used by deprecated groups.
    StartGroup,
    /// Wire type 5.
    Fixed32,
    /// A custom type whose declaration is not available in this file.
    Unknown,
}

/// Fully owned protobuf field contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtoField {
    /// Field name as declared.
    pub name: String,
    /// Positive protobuf field number.
    pub number: i64,
    /// Declared scalar, enum, message, or map value type.
    pub type_name: String,
    /// Known protobuf wire encoding.
    pub wire_type: ProtoWireType,
    /// Declared field cardinality.
    pub cardinality: ProtoFieldCardinality,
    /// Enclosing `oneof` name when this is a oneof alternative.
    pub oneof: Option<String>,
    /// Map key type when this is a map field.
    pub map_key_type: Option<String>,
    /// Map value type when this is a map field.
    pub map_value_type: Option<String>,
    /// Generator-relevant field options.
    pub options: BTreeMap<String, String>,
    /// One-based declaration line.
    pub line: u32,
}

/// Fully owned protobuf message contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtoMessage {
    /// Unqualified message name.
    pub name: String,
    /// Package-qualified and nesting-qualified message name.
    pub full_name: String,
    /// Direct fields sorted by field number and name.
    pub fields: Vec<ProtoField>,
    /// Directly nested messages sorted by fully qualified name.
    pub messages: Vec<ProtoMessage>,
    /// Directly nested enums sorted by fully qualified name.
    pub enums: Vec<ProtoEnum>,
    /// Reserved number declarations in canonical protobuf notation.
    pub reserved_numbers: Vec<String>,
    /// Reserved field names.
    pub reserved_names: Vec<String>,
    /// Generator-relevant message options.
    pub options: BTreeMap<String, String>,
    /// Whether this declaration extends an existing message.
    pub is_extension: bool,
    /// One-based declaration line.
    pub line: u32,
}

/// Fully owned protobuf enum value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtoEnumValue {
    /// Enum value name.
    pub name: String,
    /// Signed numeric enum value.
    pub number: i64,
    /// Generator-relevant value options.
    pub options: BTreeMap<String, String>,
    /// One-based declaration line.
    pub line: u32,
}

/// Fully owned protobuf enum contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtoEnum {
    /// Unqualified enum name.
    pub name: String,
    /// Package-qualified and nesting-qualified enum name.
    pub full_name: String,
    /// Numeric values sorted by number and name.
    pub values: Vec<ProtoEnumValue>,
    /// Reserved numeric declarations in canonical protobuf notation.
    pub reserved_numbers: Vec<String>,
    /// Reserved enum value names.
    pub reserved_names: Vec<String>,
    /// Generator-relevant enum options.
    pub options: BTreeMap<String, String>,
    /// One-based declaration line.
    pub line: u32,
}

/// Fully owned protobuf RPC method contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtoRpcMethod {
    /// RPC method name.
    pub name: String,
    /// Declared request message type.
    pub request_type: String,
    /// Declared response message type.
    pub response_type: String,
    /// Whether the client streams request messages.
    pub client_streaming: bool,
    /// Whether the server streams response messages.
    pub server_streaming: bool,
    /// Generator-relevant method options.
    pub options: BTreeMap<String, String>,
    /// One-based declaration line.
    pub line: u32,
}

/// Fully owned protobuf service contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtoService {
    /// Unqualified service name.
    pub name: String,
    /// Package-qualified service name.
    pub full_name: String,
    /// RPC methods sorted by name and signature.
    pub methods: Vec<ProtoRpcMethod>,
    /// Generator-relevant service options.
    pub options: BTreeMap<String, String>,
    /// One-based declaration line.
    pub line: u32,
}

/// Exact generated gRPC service/method marker found in generated source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtoGeneratedRole {
    /// Generated client or stub invocation.
    Client,
    /// Generated server or handler registration.
    Server,
    /// Exact method marker whose generated role is not explicit.
    Unknown,
}

/// Exact generated gRPC service/method marker found in generated source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ProtoGeneratedMarker {
    /// Generated source language.
    pub language: SourceLanguage,
    /// Repository-relative source path supplied by the caller.
    pub source_path: String,
    /// Generator family identified by the explicit header.
    pub generator: String,
    /// Generated client/server role when explicit on the marker line.
    pub role: ProtoGeneratedRole,
    /// Package-qualified service name from the exact RPC path.
    pub service: String,
    /// Method name from the exact RPC path.
    pub method: String,
    /// Exact canonical gRPC path, such as `/example.Greeter/SayHello`.
    pub rpc_path: String,
    /// One-based generated header line.
    pub header_line: u32,
    /// One-based line containing the exact RPC marker.
    pub line: u32,
}

/// Fully owned protobuf file contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtoFile {
    /// Repository-relative source path supplied by the caller.
    pub source_path: String,
    /// Declared syntax, with absent syntax represented by the proto2 default.
    pub syntax: ProtoSyntax,
    /// One-based syntax or edition declaration line.
    pub syntax_line: Option<u32>,
    /// Declared package name.
    pub package: Option<String>,
    /// One-based package declaration line.
    pub package_line: Option<u32>,
    /// All imported protobuf paths.
    pub imports: Vec<String>,
    /// Imports declared with the `public` qualifier.
    pub public_imports: Vec<String>,
    /// Imports declared with the `weak` qualifier.
    pub weak_imports: Vec<String>,
    /// First one-based line for each imported path.
    pub import_lines: BTreeMap<String, u32>,
    /// Generator-relevant file options.
    pub options: BTreeMap<String, String>,
    /// Top-level messages sorted by fully qualified name.
    pub messages: Vec<ProtoMessage>,
    /// Top-level enums sorted by fully qualified name.
    pub enums: Vec<ProtoEnum>,
    /// Services sorted by fully qualified name.
    pub services: Vec<ProtoService>,
}

/// Persistable protobuf contract payload produced from one source-owned artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ProtobufDocument {
    /// Declarative `.proto` file.
    File(Box<ProtoFile>),
    /// Exact method markers from an explicitly generated source file.
    Generated(Vec<ProtoGeneratedMarker>),
}

/// Error returned when a protobuf contract is malformed or internally inconsistent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProtobufExtractionError {
    /// The protobuf parser rejected the source.
    #[error("invalid protobuf `{source_path}` at {line}:{column}: {message}")]
    Parse {
        /// Source path supplied by the caller.
        source_path: String,
        /// One-based error line when available.
        line: u32,
        /// One-based error column when available.
        column: u32,
        /// Parser diagnostic without source content.
        message: String,
    },
    /// The file contains conflicting or unsupported syntax declarations.
    #[error("invalid protobuf declaration in `{source_path}` at line {line}: {message}")]
    InvalidDeclaration {
        /// Source path supplied by the caller.
        source_path: String,
        /// One-based declaration line.
        line: u32,
        /// Stable diagnostic without source content.
        message: String,
    },
    /// A field number violates protobuf's valid numeric range.
    #[error(
        "invalid protobuf field number {number} for `{field}` in `{source_path}` at line {line}"
    )]
    InvalidFieldNumber {
        /// Source path supplied by the caller.
        source_path: String,
        /// Field name.
        field: String,
        /// Rejected field number.
        number: i64,
        /// One-based declaration line.
        line: u32,
    },
    /// A message repeats a field name or number.
    #[error("duplicate protobuf field {coordinate} `{value}` in `{message}` at line {line}")]
    DuplicateField {
        /// Fully qualified message name.
        message: String,
        /// Duplicate coordinate: `name` or `number`.
        coordinate: String,
        /// Duplicate value.
        value: String,
        /// One-based declaration line of the duplicate.
        line: u32,
    },
    /// Extraction exceeded one configured invocation resource.
    #[error(transparent)]
    LimitExceeded(#[from] ExtractionLimitExceeded),
}

/// Extracts an owned protobuf contract from a `.proto` source.
///
/// Output collections are sorted and deduplicated. The result contains contract metadata and
/// line coordinates, but never retains the input source or source snippets.
///
/// # Errors
///
/// Returns [`ProtobufExtractionError`] when parsing fails, declarations conflict, field numbers
/// are invalid, or a message repeats a field name or number.
pub fn extract_protobuf(
    source_path: &str,
    input: &str,
) -> Result<ProtoFile, ProtobufExtractionError> {
    let mut tracker = ExtractionTracker::new(
        source_path,
        "code-system-graph.protobuf",
        &ExtractionBudgets::default(),
    );
    extract_protobuf_with_tracker(source_path, input, &mut tracker)
}

/// Extracts a protobuf contract using an existing per-invocation tracker.
///
/// # Errors
///
/// Returns an error for malformed input or an exhausted extraction budget.
pub fn extract_protobuf_with_tracker(
    source_path: &str,
    input: &str,
    tracker: &mut ExtractionTracker,
) -> Result<ProtoFile, ProtobufExtractionError> {
    tracker.check_input_bytes(u64::try_from(input.len()).unwrap_or(u64::MAX))?;
    tracker.charge_portable_path(source_path)?;
    precheck_protobuf_depth(input, tracker)?;
    let mut parser = Parser::with_filename(input, source_path);
    let parsed = parser.parse();
    tracker.check_structured_time()?;
    let parsed = parsed.map_err(|error| ProtobufExtractionError::Parse {
        source_path: source_path.to_owned(),
        line: source_line(error.position.line),
        column: source_line(error.position.column),
        message: "parser rejected malformed input".to_owned(),
    })?;

    let (syntax, syntax_line) = extract_syntax(source_path, &parsed.elements)?;
    let (package, package_line) = extract_package(source_path, &parsed.elements)?;
    let package_scope = package.as_deref().unwrap_or_default();
    let known_types = collect_type_names(&parsed.elements, package_scope, tracker)?;
    let mut imports = Vec::new();
    let mut public_imports = Vec::new();
    let mut weak_imports = Vec::new();
    let mut import_lines = BTreeMap::new();
    let mut messages = Vec::new();
    let mut enums = Vec::new();
    let mut services = Vec::new();

    for element in &parsed.elements {
        tracker.charge_work(1)?;
        match element {
            Element::Import(import) => {
                imports.push(import.filename.clone());
                import_lines
                    .entry(import.filename.clone())
                    .or_insert_with(|| source_line(import.position.line));
                match import.kind {
                    ImportKind::Default => {}
                    ImportKind::Public => public_imports.push(import.filename.clone()),
                    ImportKind::Weak => weak_imports.push(import.filename.clone()),
                }
            }
            Element::Message(message) => messages.push(extract_message(
                source_path,
                message,
                package_scope,
                &known_types,
                1,
                tracker,
            )?),
            Element::Enum(enumeration) => {
                enums.push(extract_enum(enumeration, package_scope, 1, tracker)?);
            }
            Element::Service(service) => {
                services.push(extract_service(service, package_scope, 1, tracker)?);
            }
            _ => {}
        }
    }

    sort_dedup(&mut imports);
    sort_dedup(&mut public_imports);
    sort_dedup(&mut weak_imports);
    messages.sort_by(|left, right| left.full_name.cmp(&right.full_name));
    messages.dedup_by(|left, right| left.full_name == right.full_name);
    enums.sort_by(|left, right| left.full_name.cmp(&right.full_name));
    enums.dedup_by(|left, right| left.full_name == right.full_name);
    services.sort_by(|left, right| left.full_name.cmp(&right.full_name));
    services.dedup_by(|left, right| left.full_name == right.full_name);

    let output = ProtoFile {
        source_path: source_path.to_owned(),
        syntax,
        syntax_line,
        package,
        package_line,
        imports,
        public_imports,
        weak_imports,
        import_lines,
        options: options_from_elements(&parsed.elements, 1, tracker)?,
        messages,
        enums,
        services,
    };
    tracker.check_structured_time()?;
    Ok(output)
}

/// Finds exact gRPC method paths in explicitly generated source.
///
/// A marker is emitted only when the file contains a recognized generated-code header and a
/// string literal whose complete value has the canonical `/qualified.Service/Method` shape.
/// Paths and filenames alone never establish that a source file is generated.
#[must_use]
pub fn parse_protobuf_generated_source(
    language: SourceLanguage,
    source_path: &str,
    input: &str,
) -> Vec<ProtoGeneratedMarker> {
    let Some((header_line, generator)) = input
        .lines()
        .enumerate()
        .find_map(|(index, line)| generated_header(line).map(|name| (index + 1, name)))
    else {
        return Vec::new();
    };
    let header_line = source_line(header_line);
    let mut markers = Vec::new();

    let lines = input.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().copied().enumerate() {
        if is_comment_only(line) {
            continue;
        }
        for literal in quoted_literals(line) {
            let Some((service, method)) = split_rpc_path(&literal) else {
                continue;
            };
            markers.push(ProtoGeneratedMarker {
                language,
                source_path: source_path.to_owned(),
                generator: generator.to_owned(),
                role: generated_role(&lines, index),
                service: service.to_owned(),
                method: method.to_owned(),
                rpc_path: literal,
                header_line,
                line: source_line(index + 1),
            });
        }
    }

    markers.sort();
    markers.dedup_by(|left, right| {
        left.language == right.language
            && left.source_path == right.source_path
            && left.service == right.service
            && left.method == right.method
            && left.line == right.line
    });
    markers
}

fn generated_role(lines: &[&str], index: usize) -> ProtoGeneratedRole {
    let start = index.saturating_sub(3);
    let end = (index + 1).min(lines.len());
    let normalized = lines[start..end].join(" ").to_ascii_lowercase();
    if ["channel", "client", "stub", "invoke"]
        .iter()
        .any(|token| normalized.contains(token))
    {
        ProtoGeneratedRole::Client
    } else if ["server", "handler", "servicer", "bind_service"]
        .iter()
        .any(|token| normalized.contains(token))
    {
        ProtoGeneratedRole::Server
    } else {
        ProtoGeneratedRole::Unknown
    }
}

fn extract_syntax(
    source_path: &str,
    elements: &[Element],
) -> Result<(ProtoSyntax, Option<u32>), ProtobufExtractionError> {
    let declarations = elements
        .iter()
        .filter_map(|element| match element {
            Element::Syntax(syntax) => Some((
                syntax.position.line,
                match syntax.value.as_str() {
                    "proto2" => Ok(ProtoSyntax::Proto2),
                    "proto3" => Ok(ProtoSyntax::Proto3),
                    other => Err(format!("unsupported syntax `{other}`")),
                },
            )),
            Element::Edition(edition) => Some((
                edition.position.line,
                Ok(ProtoSyntax::Edition {
                    version: edition.value.clone(),
                }),
            )),
            _ => None,
        })
        .collect::<Vec<_>>();

    if declarations.len() > 1 {
        return Err(ProtobufExtractionError::InvalidDeclaration {
            source_path: source_path.to_owned(),
            line: source_line(declarations[1].0),
            message: "multiple syntax or edition declarations".to_owned(),
        });
    }
    match declarations.into_iter().next() {
        Some((line, syntax)) => syntax
            .map(|syntax| (syntax, Some(source_line(line))))
            .map_err(|message| ProtobufExtractionError::InvalidDeclaration {
                source_path: source_path.to_owned(),
                line: source_line(line),
                message,
            }),
        None => Ok((ProtoSyntax::Proto2, None)),
    }
}

fn extract_package(
    source_path: &str,
    elements: &[Element],
) -> Result<(Option<String>, Option<u32>), ProtobufExtractionError> {
    let packages = elements
        .iter()
        .filter_map(|element| match element {
            Element::Package(package) => Some((&package.name, package.position.line)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if packages.len() > 1 {
        return Err(ProtobufExtractionError::InvalidDeclaration {
            source_path: source_path.to_owned(),
            line: source_line(packages[1].1),
            message: "multiple package declarations".to_owned(),
        });
    }
    Ok(packages.first().map_or((None, None), |(name, line)| {
        (Some((*name).clone()), Some(source_line(*line)))
    }))
}

#[derive(Debug, Default)]
struct KnownTypes {
    enums: BTreeSet<String>,
    messages: BTreeSet<String>,
}

fn collect_type_names(
    elements: &[Element],
    scope: &str,
    tracker: &mut ExtractionTracker,
) -> Result<KnownTypes, ExtractionLimitExceeded> {
    let mut known = KnownTypes::default();
    // The top-level file is not a recursive message scope. Its first message is depth one.
    collect_type_names_into(elements, scope, &mut known, 0, tracker)?;
    Ok(known)
}

fn collect_type_names_into(
    elements: &[Element],
    scope: &str,
    known: &mut KnownTypes,
    depth: u64,
    tracker: &mut ExtractionTracker,
) -> Result<(), ExtractionLimitExceeded> {
    tracker.check_structural_depth(depth)?;
    for element in elements {
        tracker.charge_work(1)?;
        match element {
            Element::Enum(enumeration) => {
                known.enums.insert(qualified_name(scope, &enumeration.name));
            }
            Element::Message(message) => {
                let message_scope = qualified_name(scope, &message.name);
                known.messages.insert(message_scope.clone());
                collect_type_names_into(
                    &message.elements,
                    &message_scope,
                    known,
                    depth.saturating_add(1),
                    tracker,
                )?;
            }
            Element::Group(group) => {
                let group_scope = qualified_name(scope, &group.name);
                known.messages.insert(group_scope.clone());
                collect_type_names_into(
                    &group.elements,
                    &group_scope,
                    known,
                    depth.saturating_add(1),
                    tracker,
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn extract_message(
    source_path: &str,
    message: &Message,
    parent_scope: &str,
    known_types: &KnownTypes,
    depth: u64,
    tracker: &mut ExtractionTracker,
) -> Result<ProtoMessage, ProtobufExtractionError> {
    extract_message_elements(
        source_path,
        &message.name,
        message.is_extend,
        message.position.line,
        &message.elements,
        parent_scope,
        known_types,
        depth,
        tracker,
    )
}

fn extract_group_message(
    source_path: &str,
    group: &Group,
    parent_scope: &str,
    known_types: &KnownTypes,
    depth: u64,
    tracker: &mut ExtractionTracker,
) -> Result<ProtoMessage, ProtobufExtractionError> {
    extract_message_elements(
        source_path,
        &group.name,
        false,
        group.position.line,
        &group.elements,
        parent_scope,
        known_types,
        depth,
        tracker,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "One traversal keeps message fields, nesting, options, and reservations consistent"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "The parser message context and shared budget tracker are independent inputs"
)]
fn extract_message_elements(
    source_path: &str,
    name: &str,
    is_extension: bool,
    line: usize,
    elements: &[Element],
    parent_scope: &str,
    known_types: &KnownTypes,
    depth: u64,
    tracker: &mut ExtractionTracker,
) -> Result<ProtoMessage, ProtobufExtractionError> {
    tracker.check_structural_depth(depth)?;
    let full_name = qualified_name(parent_scope, name);
    let mut fields = Vec::new();
    let mut messages = Vec::new();
    let mut enums = Vec::new();
    let mut reserved_numbers = Vec::new();
    let mut reserved_names = Vec::new();

    for element in elements {
        tracker.charge_work(1)?;
        match element {
            Element::NormalField(field) => fields.push(field_from_common(
                source_path,
                &field.field,
                cardinality(field.optional, field.required, field.repeated),
                None,
                None,
                &full_name,
                known_types,
                depth,
                tracker,
            )?),
            Element::MapField(field) => fields.push(field_from_common(
                source_path,
                &field.field,
                ProtoFieldCardinality::Repeated,
                None,
                Some(&field.key_type),
                &full_name,
                known_types,
                depth,
                tracker,
            )?),
            Element::Oneof(oneof) => {
                for child in &oneof.elements {
                    if let Element::OneofField(field) = child {
                        fields.push(field_from_common(
                            source_path,
                            &field.field,
                            ProtoFieldCardinality::Singular,
                            Some(&oneof.name),
                            None,
                            &full_name,
                            known_types,
                            depth,
                            tracker,
                        )?);
                    }
                }
            }
            Element::Message(nested) => {
                messages.push(extract_message(
                    source_path,
                    nested,
                    &full_name,
                    known_types,
                    depth.saturating_add(1),
                    tracker,
                )?);
            }
            Element::Group(group) => {
                validate_field_number(
                    source_path,
                    &group.name,
                    group.sequence,
                    group.position.line,
                )?;
                fields.push(ProtoField {
                    name: group.name.clone(),
                    number: group.sequence,
                    type_name: group.name.clone(),
                    wire_type: ProtoWireType::StartGroup,
                    cardinality: cardinality(group.optional, group.required, group.repeated),
                    oneof: None,
                    map_key_type: None,
                    map_value_type: None,
                    options: BTreeMap::new(),
                    line: source_line(group.position.line),
                });
                messages.push(extract_group_message(
                    source_path,
                    group,
                    &full_name,
                    known_types,
                    depth.saturating_add(1),
                    tracker,
                )?);
            }
            Element::Enum(enumeration) => enums.push(extract_enum(
                enumeration,
                &full_name,
                depth.saturating_add(1),
                tracker,
            )?),
            Element::Reserved(reserved) => {
                reserved_numbers.extend(
                    reserved
                        .ranges
                        .iter()
                        .map(proto_parser::Range::source_representation),
                );
                reserved_names.extend(reserved.field_names.iter().cloned());
            }
            _ => {}
        }
    }

    fields.sort_by(|left, right| {
        left.number
            .cmp(&right.number)
            .then_with(|| left.name.cmp(&right.name))
    });
    validate_unique_fields(&full_name, &fields)?;
    messages.sort_by(|left, right| left.full_name.cmp(&right.full_name));
    messages.dedup_by(|left, right| left.full_name == right.full_name);
    enums.sort_by(|left, right| left.full_name.cmp(&right.full_name));
    enums.dedup_by(|left, right| left.full_name == right.full_name);
    sort_dedup(&mut reserved_numbers);
    sort_dedup(&mut reserved_names);

    Ok(ProtoMessage {
        name: name.to_owned(),
        full_name,
        fields,
        messages,
        enums,
        reserved_numbers,
        reserved_names,
        options: options_from_elements(elements, depth, tracker)?,
        is_extension,
        line: source_line(line),
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "The parser field shape and shared budget tracker are independent inputs"
)]
fn field_from_common(
    source_path: &str,
    field: &FieldCommon,
    cardinality: ProtoFieldCardinality,
    oneof: Option<&str>,
    map_key_type: Option<&str>,
    message_scope: &str,
    known_types: &KnownTypes,
    depth: u64,
    tracker: &mut ExtractionTracker,
) -> Result<ProtoField, ProtobufExtractionError> {
    tracker.check_structural_depth(depth)?;
    validate_field_number(
        source_path,
        &field.name,
        field.sequence,
        field.position.line,
    )?;
    let map_value_type = map_key_type.map(|_| field.type_name.clone());
    Ok(ProtoField {
        name: field.name.clone(),
        number: field.sequence,
        type_name: if let Some(key_type) = map_key_type {
            format!("map<{key_type}, {}>", field.type_name)
        } else {
            field.type_name.clone()
        },
        wire_type: if map_key_type.is_some() {
            ProtoWireType::LengthDelimited
        } else {
            wire_type(&field.type_name, message_scope, known_types)
        },
        cardinality,
        oneof: oneof.map(str::to_owned),
        map_key_type: map_key_type.map(str::to_owned),
        map_value_type,
        options: relevant_options(&field.options, depth, tracker)?,
        line: source_line(field.position.line),
    })
}

fn validate_field_number(
    source_path: &str,
    field: &str,
    number: i64,
    line: usize,
) -> Result<(), ProtobufExtractionError> {
    if number <= 0
        || number > MAX_FIELD_NUMBER
        || (FIRST_RESERVED_IMPLEMENTATION_FIELD..=LAST_RESERVED_IMPLEMENTATION_FIELD)
            .contains(&number)
    {
        return Err(ProtobufExtractionError::InvalidFieldNumber {
            source_path: source_path.to_owned(),
            field: field.to_owned(),
            number,
            line: source_line(line),
        });
    }
    Ok(())
}

fn validate_unique_fields(
    message: &str,
    fields: &[ProtoField],
) -> Result<(), ProtobufExtractionError> {
    let mut names = BTreeSet::new();
    let mut numbers = BTreeSet::new();
    for field in fields {
        if !names.insert(&field.name) {
            return Err(ProtobufExtractionError::DuplicateField {
                message: message.to_owned(),
                coordinate: "name".to_owned(),
                value: field.name.clone(),
                line: field.line,
            });
        }
        if !numbers.insert(field.number) {
            return Err(ProtobufExtractionError::DuplicateField {
                message: message.to_owned(),
                coordinate: "number".to_owned(),
                value: field.number.to_string(),
                line: field.line,
            });
        }
    }
    Ok(())
}

fn extract_enum(
    enumeration: &Enum,
    parent_scope: &str,
    depth: u64,
    tracker: &mut ExtractionTracker,
) -> Result<ProtoEnum, ProtobufExtractionError> {
    tracker.check_structural_depth(depth)?;
    let mut values = Vec::new();
    let mut reserved_numbers = Vec::new();
    let mut reserved_names = Vec::new();
    for element in &enumeration.elements {
        tracker.charge_work(1)?;
        match element {
            Element::EnumField(value) => values.push(ProtoEnumValue {
                name: value.name.clone(),
                number: value.integer,
                options: options_from_elements(&value.elements, depth, tracker)?,
                line: source_line(value.position.line),
            }),
            Element::Reserved(reserved) => {
                reserved_numbers.extend(
                    reserved
                        .ranges
                        .iter()
                        .map(proto_parser::Range::source_representation),
                );
                reserved_names.extend(reserved.field_names.iter().cloned());
            }
            _ => {}
        }
    }
    values.sort_by(|left, right| {
        left.number
            .cmp(&right.number)
            .then_with(|| left.name.cmp(&right.name))
    });
    values.dedup_by(|left, right| left.number == right.number && left.name == right.name);
    sort_dedup(&mut reserved_numbers);
    sort_dedup(&mut reserved_names);
    Ok(ProtoEnum {
        name: enumeration.name.clone(),
        full_name: qualified_name(parent_scope, &enumeration.name),
        values,
        reserved_numbers,
        reserved_names,
        options: options_from_elements(&enumeration.elements, depth, tracker)?,
        line: source_line(enumeration.position.line),
    })
}

fn extract_service(
    service: &Service,
    package: &str,
    depth: u64,
    tracker: &mut ExtractionTracker,
) -> Result<ProtoService, ProtobufExtractionError> {
    tracker.check_structural_depth(depth)?;
    let mut methods = Vec::new();
    for element in &service.elements {
        tracker.charge_work(1)?;
        if let Element::Rpc(rpc) = element {
            methods.push(extract_rpc(rpc, depth.saturating_add(1), tracker)?);
        }
    }
    methods.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.request_type.cmp(&right.request_type))
            .then_with(|| left.response_type.cmp(&right.response_type))
    });
    methods.dedup();
    Ok(ProtoService {
        name: service.name.clone(),
        full_name: qualified_name(package, &service.name),
        methods,
        options: options_from_elements(&service.elements, depth, tracker)?,
        line: source_line(service.position.line),
    })
}

fn extract_rpc(
    rpc: &Rpc,
    depth: u64,
    tracker: &mut ExtractionTracker,
) -> Result<ProtoRpcMethod, ProtobufExtractionError> {
    tracker.check_structural_depth(depth)?;
    Ok(ProtoRpcMethod {
        name: rpc.name.clone(),
        request_type: rpc.request_type.clone(),
        response_type: rpc.returns_type.clone(),
        client_streaming: rpc.streams_request,
        server_streaming: rpc.streams_returns,
        options: options_from_elements(&rpc.elements, depth, tracker)?,
        line: source_line(rpc.position.line),
    })
}

fn cardinality(optional: bool, required: bool, repeated: bool) -> ProtoFieldCardinality {
    if repeated {
        ProtoFieldCardinality::Repeated
    } else if required {
        ProtoFieldCardinality::Required
    } else if optional {
        ProtoFieldCardinality::Optional
    } else {
        ProtoFieldCardinality::Singular
    }
}

fn wire_type(type_name: &str, message_scope: &str, known_types: &KnownTypes) -> ProtoWireType {
    match type_name.trim_start_matches('.') {
        "double" | "fixed64" | "sfixed64" => ProtoWireType::Fixed64,
        "float" | "fixed32" | "sfixed32" => ProtoWireType::Fixed32,
        "int32" | "int64" | "uint32" | "uint64" | "sint32" | "sint64" | "bool" => {
            ProtoWireType::Varint
        }
        "string" | "bytes" => ProtoWireType::LengthDelimited,
        custom if resolves_type(custom, message_scope, &known_types.enums) => ProtoWireType::Varint,
        custom if resolves_type(custom, message_scope, &known_types.messages) => {
            ProtoWireType::LengthDelimited
        }
        _ => ProtoWireType::Unknown,
    }
}

fn resolves_type(type_name: &str, message_scope: &str, names: &BTreeSet<String>) -> bool {
    if let Some(absolute) = type_name.strip_prefix('.') {
        return names.contains(absolute);
    }
    let mut scope = message_scope;
    loop {
        if names.contains(&qualified_name(scope, type_name)) {
            return true;
        }
        let Some((parent, _)) = scope.rsplit_once('.') else {
            break;
        };
        scope = parent;
    }
    names.contains(type_name)
}

fn options_from_elements(
    elements: &[Element],
    depth: u64,
    tracker: &mut ExtractionTracker,
) -> Result<BTreeMap<String, String>, ExtractionLimitExceeded> {
    let mut options = BTreeMap::new();
    for element in elements {
        if let Element::Option(option) = element
            && is_relevant_option(&option.name)
        {
            tracker.charge_work(1)?;
            options.insert(
                option.name.clone(),
                literal_value(&option.constant, depth.saturating_add(1), tracker)?,
            );
        }
    }
    Ok(options)
}

fn relevant_options(
    source: &[ProtoOption],
    depth: u64,
    tracker: &mut ExtractionTracker,
) -> Result<BTreeMap<String, String>, ExtractionLimitExceeded> {
    let mut options = BTreeMap::new();
    for option in source
        .iter()
        .filter(|option| is_relevant_option(&option.name))
    {
        tracker.charge_work(1)?;
        options.insert(
            option.name.clone(),
            literal_value(&option.constant, depth.saturating_add(1), tracker)?,
        );
    }
    Ok(options)
}

fn is_relevant_option(name: &str) -> bool {
    matches!(
        name,
        "cc_enable_arenas"
            | "cc_generic_services"
            | "csharp_namespace"
            | "ctype"
            | "deprecated"
            | "go_package"
            | "idempotency_level"
            | "java_generic_services"
            | "java_multiple_files"
            | "java_outer_classname"
            | "java_package"
            | "json_name"
            | "jstype"
            | "objc_class_prefix"
            | "optimize_for"
            | "packed"
            | "php_class_prefix"
            | "php_metadata_namespace"
            | "php_namespace"
            | "py_generic_services"
            | "ruby_package"
            | "swift_prefix"
    ) || option_name_matches_root(name, "google.api.http")
        || [
            "grpc.gateway.protoc_gen_openapiv2.options.openapiv2_swagger",
            "grpc.gateway.protoc_gen_openapiv2.options.openapiv2_operation",
            "grpc.gateway.protoc_gen_openapiv2.options.openapiv2_schema",
            "grpc.gateway.protoc_gen_openapiv2.options.openapiv2_field",
            "grpc.gateway.protoc_gen_openapiv2.options.openapiv2_tag",
        ]
        .iter()
        .any(|root| option_name_matches_root(name, root))
}

fn option_name_matches_root(name: &str, root: &str) -> bool {
    if name == root {
        return true;
    }
    name.strip_prefix('(')
        .and_then(|value| value.strip_prefix(root))
        .is_some_and(|suffix| suffix == ")" || suffix.starts_with(")."))
}

fn literal_value(
    literal: &Literal,
    depth: u64,
    tracker: &mut ExtractionTracker,
) -> Result<String, ExtractionLimitExceeded> {
    tracker.check_structural_depth(depth)?;
    tracker.charge_work(1)?;
    if let Some(array) = &literal.array {
        let mut values = Vec::new();
        for value in array {
            values.push(literal_value(value, depth.saturating_add(1), tracker)?);
        }
        let value = format!("[{}]", values.join(","));
        tracker.charge_string(&value)?;
        return Ok(value);
    }
    if let Some(map) = &literal.ordered_map {
        let mut values = Vec::new();
        for entry in map {
            let nested = literal_value(&entry.literal, depth.saturating_add(1), tracker)?;
            values.push(format!("{}:{nested}", entry.name));
        }
        let value = format!("{{{}}}", values.join(","));
        tracker.charge_string(&value)?;
        return Ok(value);
    }
    tracker.charge_string(&literal.source)?;
    Ok(literal.source.clone())
}

fn precheck_protobuf_depth(
    input: &str,
    tracker: &mut ExtractionTracker,
) -> Result<(), ExtractionLimitExceeded> {
    let bytes = input.as_bytes();
    let mut cursor = 0;
    let mut depth = 0_u64;
    let mut quote = None;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut string_start = None;
    let mut accumulated_string_bytes = 0_u64;
    let mut literal_context = false;
    let mut consecutive_literal_comments = 0_u64;
    while cursor < bytes.len() {
        if cursor.is_multiple_of(1_024) {
            tracker.check_structured_time()?;
        }
        let byte = bytes[cursor];
        let next = bytes.get(cursor.saturating_add(1)).copied();
        if line_comment {
            line_comment = byte != b'\n';
        } else if block_comment {
            if byte == b'*' && next == Some(b'/') {
                block_comment = false;
                cursor = cursor.saturating_add(1);
            }
        } else if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == delimiter {
                let start = string_start.take().unwrap_or(cursor);
                let observed = u64::try_from(cursor.saturating_sub(start)).unwrap_or(u64::MAX);
                tracker.check_string_bytes(observed)?;
                accumulated_string_bytes = accumulated_string_bytes.saturating_add(observed);
                tracker.check_accumulated_string_bytes(accumulated_string_bytes)?;
                quote = None;
            }
        } else if byte == b'/' && next == Some(b'/') {
            charge_protobuf_comment_recursion(
                literal_context,
                &mut consecutive_literal_comments,
                tracker,
            )?;
            line_comment = true;
            cursor = cursor.saturating_add(1);
        } else if byte == b'/' && next == Some(b'*') {
            charge_protobuf_comment_recursion(
                literal_context,
                &mut consecutive_literal_comments,
                tracker,
            )?;
            block_comment = true;
            cursor = cursor.saturating_add(1);
        } else if matches!(byte, b'"' | b'\'') {
            consecutive_literal_comments = 0;
            tracker.charge_work(1)?;
            quote = Some(byte);
            string_start = Some(cursor.saturating_add(1));
        } else if matches!(byte, b'{' | b'[' | b'(') {
            consecutive_literal_comments = 0;
            tracker.charge_work(1)?;
            depth = depth.saturating_add(1);
            tracker.check_structural_depth(depth)?;
        } else if matches!(byte, b'}' | b']' | b')') {
            consecutive_literal_comments = 0;
            tracker.charge_work(1)?;
            depth = depth.saturating_sub(1);
        } else if byte == b';' {
            literal_context = false;
            consecutive_literal_comments = 0;
            tracker.charge_work(1)?;
            tracker.charge_observation(1)?;
        } else if byte == b'=' || (literal_context && byte == b':') {
            literal_context = true;
            consecutive_literal_comments = 0;
            tracker.charge_work(1)?;
        } else if byte == b'_' || byte.is_ascii_alphabetic() {
            consecutive_literal_comments = 0;
            cursor = precheck_protobuf_identifier(
                input,
                cursor,
                &mut accumulated_string_bytes,
                tracker,
            )?;
            continue;
        } else if !byte.is_ascii_whitespace() {
            consecutive_literal_comments = 0;
            tracker.charge_work(1)?;
        }
        cursor = cursor.saturating_add(1);
    }
    Ok(())
}

fn precheck_protobuf_identifier(
    input: &str,
    start: usize,
    accumulated_string_bytes: &mut u64,
    tracker: &mut ExtractionTracker,
) -> Result<usize, ExtractionLimitExceeded> {
    let bytes = input.as_bytes();
    let mut cursor = start.saturating_add(1);
    while cursor < bytes.len()
        && (bytes[cursor] == b'_' || bytes[cursor] == b'.' || bytes[cursor].is_ascii_alphanumeric())
    {
        cursor = cursor.saturating_add(1);
    }
    let observed = u64::try_from(cursor.saturating_sub(start)).unwrap_or(u64::MAX);
    tracker.charge_work(1)?;
    tracker.check_identifier_bytes(observed)?;
    *accumulated_string_bytes = accumulated_string_bytes.saturating_add(observed);
    tracker.check_accumulated_string_bytes(*accumulated_string_bytes)?;
    if matches!(
        &input[start..cursor],
        "message" | "enum" | "service" | "rpc"
    ) {
        tracker.charge_observation(1)?;
    }
    Ok(cursor)
}

fn charge_protobuf_comment_recursion(
    literal_context: bool,
    consecutive_comments: &mut u64,
    tracker: &ExtractionTracker,
) -> Result<(), ExtractionLimitExceeded> {
    if literal_context {
        *consecutive_comments = consecutive_comments.saturating_add(1);
        tracker.check_structural_depth(*consecutive_comments)?;
    }
    Ok(())
}

fn qualified_name(scope: &str, name: &str) -> String {
    if scope.is_empty() || name.starts_with('.') {
        name.trim_start_matches('.').to_owned()
    } else {
        format!("{scope}.{name}")
    }
}

fn generated_header(line: &str) -> Option<&'static str> {
    let trimmed = line.trim().trim_start_matches('\u{feff}');
    if trimmed.starts_with("// Code generated by protoc-gen-") && trimmed.ends_with("DO NOT EDIT.")
    {
        return Some("protoc");
    }
    if (trimmed.starts_with("// Generated by the protocol buffer compiler.")
        || trimmed.starts_with("# Generated by the protocol buffer compiler."))
        && trimmed.contains("DO NOT EDIT!")
    {
        return Some("protoc");
    }
    if trimmed == "# Generated by the gRPC Python protocol compiler plugin. DO NOT EDIT!" {
        return Some("grpc-python");
    }
    if trimmed.starts_with("// This file is @generated by prost-build.") {
        return Some("prost-build");
    }
    None
}

fn is_comment_only(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with('*')
}

fn quoted_literals(line: &str) -> Vec<String> {
    let mut output = Vec::new();
    let characters = line.char_indices().collect::<Vec<_>>();
    let mut cursor = 0;
    while cursor < characters.len() {
        let quote = characters[cursor].1;
        if !matches!(quote, '"' | '\'' | '`') {
            cursor += 1;
            continue;
        }
        let start = characters[cursor].0 + quote.len_utf8();
        let mut end = None;
        let mut escaped = false;
        cursor += 1;
        while cursor < characters.len() {
            let character = characters[cursor].1;
            if character == '\\' {
                escaped = true;
                cursor = cursor.saturating_add(2);
                continue;
            }
            if character == quote {
                end = Some(characters[cursor].0);
                cursor += 1;
                break;
            }
            cursor += 1;
        }
        if !escaped
            && let Some(end) = end
            && let Some(value) = line.get(start..end)
        {
            output.push(value.to_owned());
        }
    }
    output
}

fn split_rpc_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix('/')?;
    let (service, method) = rest.split_once('/')?;
    if service.is_empty()
        || method.is_empty()
        || method.contains('/')
        || !service.split('.').all(is_proto_identifier)
        || !is_proto_identifier(method)
    {
        return None;
    }
    Some((service, method))
}

fn is_proto_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn source_line(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn sort_dedup<T: Ord>(values: &mut Vec<T>) {
    values.sort();
    values.dedup();
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::process::Command;

    use super::{
        ProtoFieldCardinality, ProtoSyntax, ProtoWireType, ProtobufExtractionError, extract_protobuf, extract_protobuf_with_tracker, parse_protobuf_generated_source
    };
    use crate::{ExtractionBudgets, ExtractionResource, ExtractionTracker, SourceLanguage};

    const COMPLETE_PROTO: &str = r#"syntax = "proto3";
package example.v1;

import "google/protobuf/empty.proto";
import public "common.proto";
import weak "optional.proto";
option java_package = "com.example.v1";

message Request {
  reserved 7, 9 to 11;
  reserved "old_name";
  string name = 1 [json_name = "displayName"];
  repeated int64 ids = 2;
  map<string, bytes> labels = 3;
  oneof target {
    string email = 4;
    int32 account_id = 5;
  }
  enum State {
    STATE_UNSPECIFIED = 0;
    ACTIVE = 1;
  }
  State state = 6;
  Nested nested = 8;
  message Nested {
    fixed32 token = 1;
  }
}

enum Result {
  RESULT_UNSPECIFIED = 0;
  OK = 1;
  FAILED = -1;
}

service Greeter {
  rpc Chat(stream Request) returns (stream Request) {
    option idempotency_level = NO_SIDE_EFFECTS;
  }
  rpc Get(google.protobuf.Empty) returns (Request);
}
"#;

    #[test]
    fn extract_should_preserve_field_numbers_wire_types_and_shapes() {
        let result = extract_protobuf("api/example.proto", COMPLETE_PROTO);

        assert!(matches!(
            result,
            Ok(file)
                if file.messages[0].fields.iter().map(|field| (
                    field.number,
                    field.wire_type,
                    field.cardinality,
                    field.oneof.as_deref(),
                    field.map_key_type.as_deref(),
                )).collect::<Vec<_>>() == vec![
                    (1, ProtoWireType::LengthDelimited, ProtoFieldCardinality::Singular, None, None),
                    (2, ProtoWireType::Varint, ProtoFieldCardinality::Repeated, None, None),
                    (3, ProtoWireType::LengthDelimited, ProtoFieldCardinality::Repeated, None, Some("string")),
                    (4, ProtoWireType::LengthDelimited, ProtoFieldCardinality::Singular, Some("target"), None),
                    (5, ProtoWireType::Varint, ProtoFieldCardinality::Singular, Some("target"), None),
                    (6, ProtoWireType::Varint, ProtoFieldCardinality::Singular, None, None),
                    (8, ProtoWireType::LengthDelimited, ProtoFieldCardinality::Singular, None, None),
                ]
        ));
    }

    #[test]
    fn extract_should_preserve_rpc_streaming_and_method_types() {
        let result = extract_protobuf("api/example.proto", COMPLETE_PROTO);

        assert!(matches!(
            result,
            Ok(file)
                if file.services[0].full_name == "example.v1.Greeter"
                    && file.services[0].methods[0].name == "Chat"
                    && file.services[0].methods[0].client_streaming
                    && file.services[0].methods[0].server_streaming
                    && file.services[0].methods[0].request_type == "Request"
                    && file.services[0].methods[0].response_type == "Request"
        ));
    }

    #[test]
    fn extract_should_preserve_sorted_import_kinds_and_lines() {
        let result = extract_protobuf("api/example.proto", COMPLETE_PROTO);

        assert!(matches!(
            result,
            Ok(file)
                if file.imports == vec![
                    "common.proto",
                    "google/protobuf/empty.proto",
                    "optional.proto",
                ]
                    && file.public_imports == vec!["common.proto"]
                    && file.weak_imports == vec!["optional.proto"]
                    && file.import_lines.get("common.proto") == Some(&5)
        ));
    }

    #[test]
    fn extract_should_preserve_enums_nested_messages_and_reservations() {
        let result = extract_protobuf("api/example.proto", COMPLETE_PROTO);

        assert!(matches!(
            result,
            Ok(file)
                if file.syntax == ProtoSyntax::Proto3
                    && file.package.as_deref() == Some("example.v1")
                    && file.enums[0].values.iter().map(|value| value.number).collect::<Vec<_>>() == vec![-1, 0, 1]
                    && file.messages[0].messages[0].full_name == "example.v1.Request.Nested"
                    && file.messages[0].enums[0].full_name == "example.v1.Request.State"
                    && file.messages[0].reserved_numbers == vec!["7", "9 to 11"]
                    && file.messages[0].reserved_names == vec!["old_name"]
        ));
    }

    #[test]
    fn extract_should_return_parse_error_for_malformed_source() {
        let result = extract_protobuf(
            "api/broken.proto",
            "syntax = \"proto3\"; message Broken { string value = ; }",
        );

        assert!(matches!(
            result,
            Err(ProtobufExtractionError::Parse { source_path, .. })
                if source_path == "api/broken.proto"
        ));
    }

    #[test]
    fn extract_should_reject_invalid_field_number() {
        let result = extract_protobuf(
            "api/broken.proto",
            "syntax = \"proto3\"; message Broken { string value = 19000; }",
        );

        assert!(matches!(
            result,
            Err(ProtobufExtractionError::InvalidFieldNumber { number: 19_000, .. })
        ));
    }

    fn nested_messages(depth: usize) -> String {
        let mut source = String::from("syntax = \"proto3\";\n");
        for index in 0..depth {
            writeln!(source, "message M{index} {{").expect("String writes are infallible");
        }
        source.push_str("string value = 1;\n");
        source.push_str(&"}\n".repeat(depth));
        source
    }

    #[test]
    fn protobuf_depth_should_accept_64_and_reject_65_before_recursive_parse() {
        let budgets = ExtractionBudgets {
            max_structural_depth_per_artifact: 64,
            ..ExtractionBudgets::default()
        };
        let mut exact = ExtractionTracker::new("exact.proto", "protobuf", &budgets);
        let mut above = ExtractionTracker::new("above.proto", "protobuf", &budgets);

        let exact_result =
            extract_protobuf_with_tracker("exact.proto", &nested_messages(64), &mut exact);
        assert!(exact_result.is_ok(), "exact depth failed: {exact_result:?}");
        assert!(matches!(
            extract_protobuf_with_tracker("above.proto", &nested_messages(65), &mut above),
            Err(ProtobufExtractionError::LimitExceeded(error))
                if error.resource == ExtractionResource::StructuralDepth
                    && error.observed == 65
                    && error.maximum == 64
        ));
    }

    #[test]
    fn protobuf_preflight_should_charge_observations_before_parsing() {
        let budgets = ExtractionBudgets {
            max_observations_per_artifact: 1,
            ..ExtractionBudgets::default()
        };
        let mut tracker = ExtractionTracker::new("facts.proto", "protobuf", &budgets);
        let result = extract_protobuf_with_tracker(
            "facts.proto",
            "syntax = \"proto3\"; message Item { string value = 1; }",
            &mut tracker,
        );

        assert!(matches!(
            result,
            Err(ProtobufExtractionError::LimitExceeded(error))
                if error.resource == ExtractionResource::Observations
                    && error.observed == 2
                    && error.maximum == 1
        ));
    }

    #[test]
    fn protobuf_should_not_persist_substring_matched_custom_option_literals() {
        let secret = "top-secret-value-51f2";
        let source =
            format!("syntax = \"proto3\"; option (evil.google.api.http_secret) = \"{secret}\";");
        let file = extract_protobuf("api/options.proto", &source).expect("valid protobuf");
        let payload = serde_json::to_string(&file).expect("protobuf contract should serialize");

        assert!(file.options.is_empty());
        assert!(!payload.contains(secret));
    }

    #[test]
    fn deeply_nested_protobuf_should_fail_cleanly_in_a_subprocess() {
        const CHILD_ENV: &str = "CSGRAPH_PROTO_DEPTH_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            let budgets = ExtractionBudgets {
                max_structural_depth_per_artifact: 64,
                ..ExtractionBudgets::default()
            };
            let mut tracker = ExtractionTracker::new("deep.proto", "protobuf", &budgets);
            assert!(matches!(
                extract_protobuf_with_tracker("deep.proto", &nested_messages(20_000), &mut tracker),
                Err(ProtobufExtractionError::LimitExceeded(_))
            ));
            return;
        }

        let output = Command::new(std::env::current_exe().expect("test executable should exist"))
            .args([
                "--exact",
                "protobuf_contracts::tests::deeply_nested_protobuf_should_fail_cleanly_in_a_subprocess",
            ])
            .env(CHILD_ENV, "1")
            .output()
            .expect("child test should launch");
        assert!(
            output.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn recursive_literal_comments_should_fail_cleanly_before_the_parser() {
        const CHILD_ENV: &str = "CSGRAPH_PROTO_COMMENT_CHILD";
        if std::env::var_os(CHILD_ENV).is_some() {
            let budgets = ExtractionBudgets {
                max_structural_depth_per_artifact: 64,
                ..ExtractionBudgets::default()
            };
            let comments = "// comment\n".repeat(20_000);
            let source = format!("syntax = \"proto3\"; option optimize_for = {comments}SPEED;");
            let mut tracker = ExtractionTracker::new("comments.proto", "protobuf", &budgets);
            assert!(matches!(
                extract_protobuf_with_tracker("comments.proto", &source, &mut tracker),
                Err(ProtobufExtractionError::LimitExceeded(error))
                    if error.resource == ExtractionResource::StructuralDepth
                        && error.observed == 65
                        && error.maximum == 64
            ));
            return;
        }

        let output = Command::new(std::env::current_exe().expect("test executable should exist"))
            .args([
                "--exact",
                "protobuf_contracts::tests::recursive_literal_comments_should_fail_cleanly_before_the_parser",
            ])
            .env(CHILD_ENV, "1")
            .output()
            .expect("child test should launch");
        assert!(
            output.status.success(),
            "child failed without a typed limit: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn generated_source_should_require_explicit_header_and_exact_rpc_path() {
        let source = r#"// Code generated by protoc-gen-go-grpc. DO NOT EDIT.
const Greeter_Chat_FullMethodName = "/example.v1.Greeter/Chat"
const invalid = "/example.v1.Greeter/Chat/extra"
"#;
        let markers = parse_protobuf_generated_source(
            SourceLanguage::Go,
            "generated/service_grpc.pb.go",
            source,
        );

        assert!(matches!(
            markers.as_slice(),
            [marker]
                if marker.generator == "protoc"
                    && marker.service == "example.v1.Greeter"
                    && marker.method == "Chat"
                    && marker.rpc_path == "/example.v1.Greeter/Chat"
                    && marker.header_line == 1
                    && marker.line == 2
        ));
    }

    #[test]
    fn generated_source_should_not_infer_from_generated_filename() {
        let markers = parse_protobuf_generated_source(
            SourceLanguage::Python,
            "generated/service_pb2_grpc.py",
            "channel.unary_unary('/example.v1.Greeter/Get')",
        );

        assert_eq!(markers, Vec::new());
    }

    #[test]
    fn generated_source_should_ignore_rpc_paths_inside_comments() {
        let markers = parse_protobuf_generated_source(
            SourceLanguage::Java,
            "GeneratedService.java",
            "// Generated by the protocol buffer compiler.  DO NOT EDIT!\n// \"/example.Greeter/Get\"\n",
        );

        assert_eq!(markers, Vec::new());
    }
}

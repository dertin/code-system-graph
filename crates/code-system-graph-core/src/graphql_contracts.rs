//! Evidence-first GraphQL contract extraction.
//!
//! Standalone GraphQL is parsed with `graphql-parser`. Focused source extraction only recognizes
//! literal documents and resolver declarations whose type, field, and implementation symbol are
//! all statically visible. Extracted values never retain source bodies.

use std::collections::{BTreeMap, BTreeSet};

use graphql_parser::{query, schema};
use serde::{Deserialize, Serialize};

use crate::{ExtractionBudgets, ExtractionLimitExceeded, ExtractionTracker, SourceLanguage};

/// Inclusive one-based source range supporting an extracted GraphQL fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GraphqlLineRange {
    /// First line containing direct evidence.
    pub start: u32,
    /// Last line containing direct evidence.
    pub end: u32,
}

/// Executable GraphQL operation category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphqlOperationKind {
    /// Read-only query operation.
    Query,
    /// Mutation operation.
    Mutation,
    /// Subscription operation.
    Subscription,
}

/// GraphQL type-system definition category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphqlTypeKind {
    /// Scalar type.
    Scalar,
    /// Object type.
    Object,
    /// Interface type.
    Interface,
    /// Input object type.
    InputObject,
    /// Enum type.
    Enum,
    /// Union type.
    Union,
}

/// Non-sensitive structural category of a GraphQL literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphqlLiteralKind {
    /// Null literal.
    Null,
    /// Boolean literal.
    Boolean,
    /// Integer literal.
    Integer,
    /// Floating-point literal.
    Float,
    /// String literal without its contents.
    String,
    /// Enum literal without its member name.
    Enum,
    /// List literal without its members.
    List,
    /// Object literal without its fields or values.
    Object,
    /// Variable reference without its name.
    Variable,
}

/// Fully owned GraphQL type reference preserving list and nullability structure.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GraphqlTypeRef {
    /// Named GraphQL type.
    Named {
        /// Type name.
        name: String,
        /// Whether this position is non-null.
        non_null: bool,
    },
    /// GraphQL list type.
    List {
        /// Type of each list element.
        element: Box<GraphqlTypeRef>,
        /// Whether the list itself is non-null.
        non_null: bool,
    },
}

impl GraphqlTypeRef {
    /// Returns canonical GraphQL type syntax.
    #[must_use]
    pub fn as_graphql(&self) -> String {
        match self {
            Self::Named { name, non_null } => {
                format!("{name}{}", if *non_null { "!" } else { "" })
            }
            Self::List { element, non_null } => format!(
                "[{}]{}",
                element.as_graphql(),
                if *non_null { "!" } else { "" }
            ),
        }
    }
}

/// Argument, variable, or input-field definition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphqlArgumentDefinition {
    /// Argument or variable name without a leading dollar sign.
    pub name: String,
    /// Declared GraphQL type.
    pub type_ref: GraphqlTypeRef,
    /// Structural category of the default value, when declared.
    pub default_value_kind: Option<GraphqlLiteralKind>,
    /// Directive names attached to the definition.
    pub directives: Vec<String>,
    /// Source evidence for the declaration.
    pub lines: GraphqlLineRange,
}

/// Field declared by an object, interface, or input object.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphqlFieldDefinition {
    /// Exact `Type.field` coordinate.
    pub coordinate: String,
    /// Field name.
    pub name: String,
    /// Field arguments. Input fields always have an empty argument list.
    pub arguments: Vec<GraphqlArgumentDefinition>,
    /// Declared return or input type.
    pub type_ref: GraphqlTypeRef,
    /// Directive names attached to the field.
    pub directives: Vec<String>,
    /// Whether this field came from an `extend` definition.
    pub extension: bool,
    /// Source evidence for the declaration.
    pub lines: GraphqlLineRange,
}

/// SDL type definition or extension.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphqlTypeDefinition {
    /// Definition category.
    pub kind: GraphqlTypeKind,
    /// Type name.
    pub name: String,
    /// Object, interface, or input-object fields.
    pub fields: Vec<GraphqlFieldDefinition>,
    /// Interfaces implemented by this type.
    pub implements: Vec<String>,
    /// Enum value names.
    pub enum_values: Vec<String>,
    /// Union member names.
    pub union_members: Vec<String>,
    /// Directive names attached to the type.
    pub directives: Vec<String>,
    /// Whether this is an `extend` definition.
    pub extension: bool,
    /// Source evidence for the declaration.
    pub lines: GraphqlLineRange,
}

/// Selection retained from an operation or fragment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GraphqlSelection {
    /// Selected field with its canonical consumed path.
    Field {
        /// Canonical field path using schema field names rather than response aliases.
        path: String,
        /// Field name.
        name: String,
        /// Optional response alias.
        alias: Option<String>,
        /// Source evidence for the field.
        lines: GraphqlLineRange,
    },
    /// Named fragment spread at a selection path.
    FragmentSpread {
        /// Fragment name.
        name: String,
        /// Parent path at which the fragment is spread.
        parent_path: Option<String>,
        /// Source evidence for the spread.
        lines: GraphqlLineRange,
    },
    /// Inline fragment type condition.
    InlineFragment {
        /// Optional type condition.
        type_condition: Option<String>,
        /// Parent path at which the fragment applies.
        parent_path: Option<String>,
        /// Source evidence for the fragment.
        lines: GraphqlLineRange,
    },
}

/// Executable GraphQL operation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphqlOperation {
    /// Operation category.
    pub kind: GraphqlOperationKind,
    /// Declared operation name, absent for shorthand or anonymous operations.
    pub name: Option<String>,
    /// Variable definitions.
    pub variables: Vec<GraphqlArgumentDefinition>,
    /// Flattened selections, including fragment-spread evidence.
    pub selections: Vec<GraphqlSelection>,
    /// Deterministically sorted consumed schema field paths.
    pub consumed_field_paths: Vec<String>,
    /// Names of directly or transitively referenced fragments.
    pub fragment_spreads: Vec<String>,
    /// Whether all referenced fragments were available and expandable.
    pub complete: bool,
    /// Machine-readable limitations for this operation.
    pub warnings: Vec<String>,
    /// Source evidence for the operation declaration.
    pub lines: GraphqlLineRange,
}

/// Named executable GraphQL fragment.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphqlFragment {
    /// Fragment name.
    pub name: String,
    /// Type condition.
    pub type_condition: String,
    /// Flattened fragment selections.
    pub selections: Vec<GraphqlSelection>,
    /// Deterministically sorted field paths relative to the fragment root.
    pub consumed_field_paths: Vec<String>,
    /// Referenced fragment names.
    pub fragment_spreads: Vec<String>,
    /// Source evidence for the fragment declaration.
    pub lines: GraphqlLineRange,
}

/// Persisted operation manifest entry without the persisted query body.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphqlPersistedOperation {
    /// Persisted operation identifier or hash.
    pub id: String,
    /// Declared or parsed operation name.
    pub operation_name: Option<String>,
    /// Parsed operation category, when a literal document is available.
    pub kind: Option<GraphqlOperationKind>,
    /// Deterministically sorted consumed field paths.
    pub consumed_field_paths: Vec<String>,
    /// Whether a literal operation document was parsed successfully.
    pub complete: bool,
    /// Machine-readable limitations for this entry.
    pub warnings: Vec<String>,
    /// Source manifest path.
    pub source_path: String,
    /// Source evidence for the manifest entry.
    pub lines: GraphqlLineRange,
}

/// Exact resolver implementation anchor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphqlResolver {
    /// GraphQL parent type.
    pub type_name: String,
    /// GraphQL field name.
    pub field_name: String,
    /// Exact `Type.field` coordinate.
    pub coordinate: String,
    /// Statically declared implementation symbol.
    pub symbol: String,
    /// Source language that supplied the declaration.
    pub language: SourceLanguage,
    /// Source evidence for the declaration.
    pub lines: GraphqlLineRange,
}

/// Declarative Apollo federation or schema-stitching directive.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GraphqlFederationMetadata {
    /// Directive name without the leading at-sign.
    pub directive: String,
    /// Schema, type, or exact field coordinate carrying the directive.
    pub target: String,
    /// Structural categories of directive arguments.
    pub arguments: BTreeMap<String, GraphqlLiteralKind>,
    /// Source evidence for the directive.
    pub lines: GraphqlLineRange,
}

/// Owned extraction result for one source artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphqlDocument {
    /// Repository-relative source path, or an empty string for embedded source parsing.
    pub source_path: String,
    /// SDL type definitions and extensions.
    pub types: Vec<GraphqlTypeDefinition>,
    /// Executable operations.
    pub operations: Vec<GraphqlOperation>,
    /// Executable fragments.
    pub fragments: Vec<GraphqlFragment>,
    /// Persisted operation entries.
    pub persisted_operations: Vec<GraphqlPersistedOperation>,
    /// Exact focused resolver declarations.
    pub resolvers: Vec<GraphqlResolver>,
    /// Declarative federation and stitching metadata.
    pub federation: Vec<GraphqlFederationMetadata>,
    /// Whether every recognized candidate was parsed exactly.
    pub complete: bool,
    /// Deterministically sorted machine-readable extraction limitations.
    pub warnings: Vec<String>,
}

impl GraphqlDocument {
    fn empty(source_path: &str) -> Self {
        Self {
            source_path: source_path.to_owned(),
            types: Vec::new(),
            operations: Vec::new(),
            fragments: Vec::new(),
            persisted_operations: Vec::new(),
            resolvers: Vec::new(),
            federation: Vec::new(),
            complete: true,
            warnings: Vec::new(),
        }
    }
}

/// Error returned for invalid standalone GraphQL or persisted-operation JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GraphqlExtractionError {
    /// The standalone GraphQL document is syntactically invalid.
    #[error("invalid GraphQL in `{source_path}`: {message}")]
    InvalidGraphql {
        /// Source artifact path.
        source_path: String,
        /// Parser diagnostic.
        message: String,
    },
    /// The persisted-operation manifest is not valid JSON.
    #[error("invalid persisted-operation JSON in `{source_path}` at line {line}: {message}")]
    InvalidJson {
        /// Source artifact path.
        source_path: String,
        /// One-based parser line.
        line: u32,
        /// JSON parser diagnostic.
        message: String,
    },
    /// JSON was valid but did not contain a supported persisted-operation shape.
    #[error("unsupported persisted-operation manifest in `{source_path}`")]
    UnsupportedPersistedManifest {
        /// Source artifact path.
        source_path: String,
    },
    /// Extraction exceeded one configured invocation resource.
    #[error(transparent)]
    LimitExceeded(#[from] ExtractionLimitExceeded),
}

/// Parses a standalone SDL or executable GraphQL document.
///
/// The function accepts one GraphQL grammar per artifact. A file mixing SDL and executable
/// definitions is rejected because `graphql-parser` exposes those grammars separately.
///
/// # Errors
///
/// Returns [`GraphqlExtractionError::InvalidGraphql`] when neither dedicated parser accepts the
/// complete input.
pub fn extract_graphql_document(
    source_path: &str,
    input: &str,
) -> Result<GraphqlDocument, GraphqlExtractionError> {
    let mut tracker = ExtractionTracker::new(
        source_path,
        "code-system-graph.graphql.document",
        &ExtractionBudgets::default(),
    );
    extract_graphql_document_with_tracker(source_path, input, &mut tracker)
}

/// Parses a standalone GraphQL document using an existing per-invocation tracker.
///
/// # Errors
///
/// Returns [`GraphqlExtractionError`] for invalid syntax or exhausted budgets.
pub fn extract_graphql_document_with_tracker(
    source_path: &str,
    input: &str,
    tracker: &mut ExtractionTracker,
) -> Result<GraphqlDocument, GraphqlExtractionError> {
    tracker.check_input_bytes(u64::try_from(input.len()).unwrap_or(u64::MAX))?;
    precheck_graphql_depth(input, tracker)?;
    let schema_result = schema::parse_schema::<String>(input);
    let query_result = query::parse_query::<String>(input);
    tracker.check_structured_time()?;

    match (schema_result, query_result) {
        (Ok(document), _) => {
            charge_schema_work(&document, tracker)?;
            let mut output = GraphqlDocument::empty(source_path);
            append_schema_document(document, &mut output);
            finish_document(&mut output);
            charge_graphql_document(&output, tracker)?;
            tracker.check_structured_time()?;
            Ok(output)
        }
        (Err(_), Ok(document)) => {
            let mut output = GraphqlDocument::empty(source_path);
            append_query_document(document, &mut output, tracker)?;
            finish_document(&mut output);
            charge_graphql_document(&output, tracker)?;
            tracker.check_structured_time()?;
            Ok(output)
        }
        (Err(schema_error), Err(query_error)) => Err(GraphqlExtractionError::InvalidGraphql {
            source_path: source_path.to_owned(),
            message: format!("{schema_error}; {query_error}"),
        }),
    }
}

/// Extracts common persisted-operation JSON maps and manifests.
///
/// Supported shapes include identifier-to-query maps, Apollo-style `operations` arrays or maps,
/// and entries using `id`, `hash`, or `sha256Hash` plus `body`, `query`, `document`, or `text`.
/// Query bodies are parsed for facts and then discarded.
///
/// # Errors
///
/// Returns [`GraphqlExtractionError::InvalidJson`] for invalid JSON,
/// [`GraphqlExtractionError::UnsupportedPersistedManifest`] when no entries are recognized, or
/// [`GraphqlExtractionError::InvalidGraphql`] when a recognized literal operation is invalid.
pub fn extract_graphql_persisted_operations(
    source_path: &str,
    input: &str,
) -> Result<Vec<GraphqlPersistedOperation>, GraphqlExtractionError> {
    let mut tracker = ExtractionTracker::new(
        source_path,
        "code-system-graph.graphql.persisted",
        &ExtractionBudgets::default(),
    );
    extract_graphql_persisted_operations_with_tracker(source_path, input, &mut tracker)
}

/// Extracts persisted operations using an existing per-invocation tracker.
///
/// # Errors
///
/// Returns [`GraphqlExtractionError`] for invalid input or exhausted budgets.
pub fn extract_graphql_persisted_operations_with_tracker(
    source_path: &str,
    input: &str,
    tracker: &mut ExtractionTracker,
) -> Result<Vec<GraphqlPersistedOperation>, GraphqlExtractionError> {
    tracker.check_input_bytes(u64::try_from(input.len()).unwrap_or(u64::MAX))?;
    let parsed = serde_json::from_str(input);
    tracker.check_structured_time()?;
    let value: serde_json::Value = parsed.map_err(|error| GraphqlExtractionError::InvalidJson {
        source_path: source_path.to_owned(),
        line: usize_to_u32(error.line()),
        message: error.to_string(),
    })?;
    let mut candidates = Vec::new();
    collect_persisted_candidates(&value, None, &mut candidates, 1, tracker)?;
    if candidates.is_empty() {
        return Err(GraphqlExtractionError::UnsupportedPersistedManifest {
            source_path: source_path.to_owned(),
        });
    }

    let mut output = Vec::new();
    for candidate in candidates {
        let line = manifest_id_line(input, &candidate.id);
        tracker.charge_work(1)?;
        output.push(persisted_operation(source_path, candidate, line, tracker)?);
    }
    output.sort();
    output.dedup();
    tracker.check_structured_time()?;
    Ok(output)
}

/// Extracts literal embedded GraphQL and focused exact resolver declarations.
///
/// Dynamic or syntactically invalid GraphQL candidates are not promoted to contracts. They make
/// the returned document partial and add a warning. Resolver recognition is intentionally limited
/// to popular declarative patterns with literal field coordinates and named implementation
/// symbols.
///
/// # Errors
///
/// Returns [`GraphqlExtractionError`] when a configured extraction budget is exhausted.
pub fn parse_graphql_source(
    language: SourceLanguage,
    input: &str,
) -> Result<GraphqlDocument, GraphqlExtractionError> {
    let mut tracker = ExtractionTracker::new(
        "<embedded>",
        "code-system-graph.graphql.source",
        &ExtractionBudgets::default(),
    );
    parse_graphql_source_with_tracker(language, input, &mut tracker)
}

/// Extracts embedded GraphQL using an existing per-invocation tracker.
///
/// # Errors
///
/// Returns an error when a configured extraction budget is exhausted.
pub fn parse_graphql_source_with_tracker(
    language: SourceLanguage,
    input: &str,
    tracker: &mut ExtractionTracker,
) -> Result<GraphqlDocument, GraphqlExtractionError> {
    tracker.check_input_bytes(u64::try_from(input.len()).unwrap_or(u64::MAX))?;
    let mut output = GraphqlDocument::empty("");
    let literals = embedded_graphql_literals(language, input);
    for literal in literals {
        if literal.dynamic {
            output.complete = false;
            output
                .warnings
                .push(format!("dynamic_graphql_literal:{}", literal.start_line));
            continue;
        }
        tracker.charge_work(1)?;
        match extract_graphql_document_with_tracker("", &literal.text, tracker) {
            Ok(mut document) => {
                shift_document_lines(&mut document, literal.start_line.saturating_sub(1));
                merge_document(&mut output, document);
            }
            Err(GraphqlExtractionError::LimitExceeded(error)) => return Err(error.into()),
            Err(
                GraphqlExtractionError::InvalidGraphql { .. }
                | GraphqlExtractionError::InvalidJson { .. }
                | GraphqlExtractionError::UnsupportedPersistedManifest { .. },
            ) => {
                output.complete = false;
                output
                    .warnings
                    .push(format!("invalid_embedded_graphql:{}", literal.start_line));
            }
        }
    }
    output.resolvers = extract_resolvers(language, input);
    tracker.charge_observation(u64::try_from(output.resolvers.len()).unwrap_or(u64::MAX))?;
    finish_document(&mut output);
    tracker.check_structured_time()?;
    Ok(output)
}

#[derive(Debug)]
struct PersistedCandidate {
    id: String,
    operation_name: Option<String>,
    document: Option<String>,
}

fn collect_persisted_candidates(
    value: &serde_json::Value,
    key_hint: Option<&str>,
    output: &mut Vec<PersistedCandidate>,
    depth: u64,
    tracker: &mut ExtractionTracker,
) -> Result<(), ExtractionLimitExceeded> {
    tracker.check_structural_depth(depth)?;
    tracker.charge_work(1)?;
    match value {
        serde_json::Value::Object(object) => {
            if let Some(candidate) = persisted_candidate_from_object(object, key_hint, tracker)? {
                tracker.charge_observation(1)?;
                output.push(candidate);
                return Ok(());
            }
            if let Some(operations) = object.get("operations") {
                collect_persisted_candidates(
                    operations,
                    None,
                    output,
                    depth.saturating_add(1),
                    tracker,
                )?;
                return Ok(());
            }
            for (key, nested) in object {
                tracker.charge_work(1)?;
                if is_manifest_metadata_key(key) {
                    continue;
                }
                match nested {
                    serde_json::Value::String(document) if looks_like_graphql(document) => {
                        tracker.charge_identifier(key)?;
                        tracker.charge_string(document)?;
                        tracker.charge_observation(1)?;
                        output.push(PersistedCandidate {
                            id: key.clone(),
                            operation_name: None,
                            document: Some(document.clone()),
                        });
                    }
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        collect_persisted_candidates(
                            nested,
                            Some(key),
                            output,
                            depth.saturating_add(1),
                            tracker,
                        )?;
                    }
                    _ => {}
                }
            }
        }
        serde_json::Value::Array(values) => {
            for nested in values {
                collect_persisted_candidates(
                    nested,
                    None,
                    output,
                    depth.saturating_add(1),
                    tracker,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn persisted_candidate_from_object(
    object: &serde_json::Map<String, serde_json::Value>,
    key_hint: Option<&str>,
    tracker: &mut ExtractionTracker,
) -> Result<Option<PersistedCandidate>, ExtractionLimitExceeded> {
    let id = string_property(object, &["id", "hash", "sha256Hash"]).or(key_hint);
    let document = string_property(object, &["body", "query", "document", "text"]);
    let operation_name = string_property(object, &["name", "operationName"]);
    let Some(id) = id else {
        return Ok(None);
    };
    if document.is_none() && operation_name.is_none() {
        return Ok(None);
    }
    tracker.charge_identifier(id)?;
    if let Some(value) = document {
        tracker.charge_string(value)?;
    }
    if let Some(value) = operation_name {
        tracker.charge_identifier(value)?;
    }
    Ok(Some(PersistedCandidate {
        id: id.to_owned(),
        operation_name: operation_name.map(str::to_owned),
        document: document.map(str::to_owned),
    }))
}

fn string_property<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(serde_json::Value::as_str))
}

fn is_manifest_metadata_key(key: &str) -> bool {
    matches!(
        key,
        "format" | "version" | "generatedAt" | "clientName" | "operations"
    )
}

fn persisted_operation(
    source_path: &str,
    candidate: PersistedCandidate,
    line: u32,
    tracker: &mut ExtractionTracker,
) -> Result<GraphqlPersistedOperation, GraphqlExtractionError> {
    let Some(document) = candidate.document else {
        return Ok(GraphqlPersistedOperation {
            id: candidate.id,
            operation_name: candidate.operation_name,
            kind: None,
            consumed_field_paths: Vec::new(),
            complete: false,
            warnings: vec!["missing_operation_document".to_owned()],
            source_path: source_path.to_owned(),
            lines: GraphqlLineRange {
                start: line,
                end: line,
            },
        });
    };
    let parsed = extract_graphql_document_with_tracker(source_path, &document, tracker)?;
    let operation =
        choose_persisted_operation(&parsed.operations, candidate.operation_name.as_deref());
    let Some(operation) = operation else {
        return Ok(GraphqlPersistedOperation {
            id: candidate.id,
            operation_name: candidate.operation_name,
            kind: None,
            consumed_field_paths: Vec::new(),
            complete: false,
            warnings: vec!["operation_name_not_found".to_owned()],
            source_path: source_path.to_owned(),
            lines: GraphqlLineRange {
                start: line,
                end: line,
            },
        });
    };
    Ok(GraphqlPersistedOperation {
        id: candidate.id,
        operation_name: operation.name.clone().or(candidate.operation_name),
        kind: Some(operation.kind),
        consumed_field_paths: operation.consumed_field_paths.clone(),
        complete: operation.complete,
        warnings: operation.warnings.clone(),
        source_path: source_path.to_owned(),
        lines: GraphqlLineRange {
            start: line,
            end: line,
        },
    })
}

fn manifest_id_line(input: &str, id: &str) -> u32 {
    let quoted = serde_json::to_string(id).unwrap_or_else(|_| format!("\"{id}\""));
    input
        .find(&quoted)
        .map_or(1, |offset| line_at_offset(input, offset))
}

fn choose_persisted_operation<'a>(
    operations: &'a [GraphqlOperation],
    requested_name: Option<&str>,
) -> Option<&'a GraphqlOperation> {
    requested_name.map_or_else(
        || (operations.len() == 1).then(|| &operations[0]),
        |name| {
            operations
                .iter()
                .find(|operation| operation.name.as_deref() == Some(name))
        },
    )
}

fn append_schema_document(document: schema::Document<'_, String>, output: &mut GraphqlDocument) {
    for definition in document.definitions {
        match definition {
            schema::Definition::SchemaDefinition(definition) => {
                append_federation_directives(
                    "schema",
                    &definition.directives,
                    definition.position.line,
                    &mut output.federation,
                );
            }
            schema::Definition::TypeDefinition(definition) => {
                append_type_definition(definition, false, output);
            }
            schema::Definition::TypeExtension(definition) => {
                append_type_extension(definition, output);
            }
            schema::Definition::DirectiveDefinition(_) => {}
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Each graphql-parser SDL variant is converted without erasing its distinct fields"
)]
fn append_type_definition(
    definition: schema::TypeDefinition<'_, String>,
    extension: bool,
    output: &mut GraphqlDocument,
) {
    match definition {
        schema::TypeDefinition::Scalar(value) => {
            append_simple_type(
                GraphqlTypeKind::Scalar,
                value.name,
                &value.directives,
                value.position.line,
                extension,
                output,
            );
        }
        schema::TypeDefinition::Object(value) => {
            let name = value.name;
            append_composite_type(
                GraphqlTypeKind::Object,
                name,
                value.fields,
                value.implements_interfaces,
                &value.directives,
                value.position.line,
                extension,
                output,
            );
        }
        schema::TypeDefinition::Interface(value) => {
            let name = value.name;
            append_composite_type(
                GraphqlTypeKind::Interface,
                name,
                value.fields,
                value.implements_interfaces,
                &value.directives,
                value.position.line,
                extension,
                output,
            );
        }
        schema::TypeDefinition::Union(value) => {
            let name = value.name;
            append_federation_directives(
                &name,
                &value.directives,
                value.position.line,
                &mut output.federation,
            );
            output.types.push(GraphqlTypeDefinition {
                kind: GraphqlTypeKind::Union,
                name,
                fields: Vec::new(),
                implements: Vec::new(),
                enum_values: Vec::new(),
                union_members: sorted_strings(value.types),
                directives: directive_names(&value.directives),
                extension,
                lines: line_range(value.position.line),
            });
        }
        schema::TypeDefinition::Enum(value) => {
            let name = value.name;
            append_federation_directives(
                &name,
                &value.directives,
                value.position.line,
                &mut output.federation,
            );
            let mut enum_values = value
                .values
                .into_iter()
                .map(|enum_value| enum_value.name)
                .collect::<Vec<_>>();
            enum_values.sort();
            enum_values.dedup();
            output.types.push(GraphqlTypeDefinition {
                kind: GraphqlTypeKind::Enum,
                name,
                fields: Vec::new(),
                implements: Vec::new(),
                enum_values,
                union_members: Vec::new(),
                directives: directive_names(&value.directives),
                extension,
                lines: line_range(value.position.line),
            });
        }
        schema::TypeDefinition::InputObject(value) => {
            let name = value.name;
            append_federation_directives(
                &name,
                &value.directives,
                value.position.line,
                &mut output.federation,
            );
            let mut fields = value
                .fields
                .into_iter()
                .map(|field| input_field_definition(&name, field, extension))
                .collect::<Vec<_>>();
            fields.sort();
            fields.dedup();
            output.types.push(GraphqlTypeDefinition {
                kind: GraphqlTypeKind::InputObject,
                name,
                fields,
                implements: Vec::new(),
                enum_values: Vec::new(),
                union_members: Vec::new(),
                directives: directive_names(&value.directives),
                extension,
                lines: line_range(value.position.line),
            });
        }
    }
}

fn append_type_extension(
    definition: schema::TypeExtension<'_, String>,
    output: &mut GraphqlDocument,
) {
    match definition {
        schema::TypeExtension::Scalar(value) => append_simple_type(
            GraphqlTypeKind::Scalar,
            value.name,
            &value.directives,
            value.position.line,
            true,
            output,
        ),
        schema::TypeExtension::Object(value) => append_composite_type(
            GraphqlTypeKind::Object,
            value.name,
            value.fields,
            value.implements_interfaces,
            &value.directives,
            value.position.line,
            true,
            output,
        ),
        schema::TypeExtension::Interface(value) => append_composite_type(
            GraphqlTypeKind::Interface,
            value.name,
            value.fields,
            value.implements_interfaces,
            &value.directives,
            value.position.line,
            true,
            output,
        ),
        schema::TypeExtension::Union(value) => {
            let name = value.name;
            append_federation_directives(
                &name,
                &value.directives,
                value.position.line,
                &mut output.federation,
            );
            output.types.push(GraphqlTypeDefinition {
                kind: GraphqlTypeKind::Union,
                name,
                fields: Vec::new(),
                implements: Vec::new(),
                enum_values: Vec::new(),
                union_members: sorted_strings(value.types),
                directives: directive_names(&value.directives),
                extension: true,
                lines: line_range(value.position.line),
            });
        }
        schema::TypeExtension::Enum(value) => {
            let name = value.name;
            append_federation_directives(
                &name,
                &value.directives,
                value.position.line,
                &mut output.federation,
            );
            output.types.push(GraphqlTypeDefinition {
                kind: GraphqlTypeKind::Enum,
                name,
                fields: Vec::new(),
                implements: Vec::new(),
                enum_values: sorted_strings(
                    value.values.into_iter().map(|enum_value| enum_value.name),
                ),
                union_members: Vec::new(),
                directives: directive_names(&value.directives),
                extension: true,
                lines: line_range(value.position.line),
            });
        }
        schema::TypeExtension::InputObject(value) => {
            let name = value.name;
            append_federation_directives(
                &name,
                &value.directives,
                value.position.line,
                &mut output.federation,
            );
            let mut fields = value
                .fields
                .into_iter()
                .map(|field| input_field_definition(&name, field, true))
                .collect::<Vec<_>>();
            fields.sort();
            fields.dedup();
            output.types.push(GraphqlTypeDefinition {
                kind: GraphqlTypeKind::InputObject,
                name,
                fields,
                implements: Vec::new(),
                enum_values: Vec::new(),
                union_members: Vec::new(),
                directives: directive_names(&value.directives),
                extension: true,
                lines: line_range(value.position.line),
            });
        }
    }
}

fn append_simple_type(
    kind: GraphqlTypeKind,
    name: String,
    directives: &[schema::Directive<'_, String>],
    line: usize,
    extension: bool,
    output: &mut GraphqlDocument,
) {
    append_federation_directives(&name, directives, line, &mut output.federation);
    output.types.push(GraphqlTypeDefinition {
        kind,
        name,
        fields: Vec::new(),
        implements: Vec::new(),
        enum_values: Vec::new(),
        union_members: Vec::new(),
        directives: directive_names(directives),
        extension,
        lines: line_range(line),
    });
}

#[expect(
    clippy::too_many_arguments,
    reason = "The parser AST exposes independent type coordinates"
)]
fn append_composite_type(
    kind: GraphqlTypeKind,
    name: String,
    source_fields: Vec<schema::Field<'_, String>>,
    implements: Vec<String>,
    directives: &[schema::Directive<'_, String>],
    line: usize,
    extension: bool,
    output: &mut GraphqlDocument,
) {
    append_federation_directives(&name, directives, line, &mut output.federation);
    let mut fields = Vec::new();
    for field in source_fields {
        append_federation_directives(
            &format!("{name}.{}", field.name),
            &field.directives,
            field.position.line,
            &mut output.federation,
        );
        fields.push(field_definition(&name, field, extension));
    }
    fields.sort();
    fields.dedup();
    output.types.push(GraphqlTypeDefinition {
        kind,
        name,
        fields,
        implements: sorted_strings(implements),
        enum_values: Vec::new(),
        union_members: Vec::new(),
        directives: directive_names(directives),
        extension,
        lines: line_range(line),
    });
}

fn field_definition(
    parent: &str,
    field: schema::Field<'_, String>,
    extension: bool,
) -> GraphqlFieldDefinition {
    let mut arguments = field
        .arguments
        .into_iter()
        .map(argument_definition)
        .collect::<Vec<_>>();
    arguments.sort();
    arguments.dedup();
    GraphqlFieldDefinition {
        coordinate: format!("{parent}.{}", field.name),
        name: field.name,
        arguments,
        type_ref: schema_type_ref(&field.field_type),
        directives: directive_names(&field.directives),
        extension,
        lines: line_range(field.position.line),
    }
}

fn input_field_definition(
    parent: &str,
    field: schema::InputValue<'_, String>,
    extension: bool,
) -> GraphqlFieldDefinition {
    GraphqlFieldDefinition {
        coordinate: format!("{parent}.{}", field.name),
        name: field.name,
        arguments: Vec::new(),
        type_ref: schema_type_ref(&field.value_type),
        directives: directive_names(&field.directives),
        extension,
        lines: line_range(field.position.line),
    }
}

fn argument_definition(value: schema::InputValue<'_, String>) -> GraphqlArgumentDefinition {
    GraphqlArgumentDefinition {
        name: value.name,
        type_ref: schema_type_ref(&value.value_type),
        default_value_kind: value.default_value.as_ref().map(graphql_value_kind),
        directives: directive_names(&value.directives),
        lines: line_range(value.position.line),
    }
}

fn schema_type_ref(value: &schema::Type<'_, String>) -> GraphqlTypeRef {
    match value {
        schema::Type::NamedType(name) => GraphqlTypeRef::Named {
            name: name.clone(),
            non_null: false,
        },
        schema::Type::ListType(element) => GraphqlTypeRef::List {
            element: Box::new(schema_type_ref(element)),
            non_null: false,
        },
        schema::Type::NonNullType(inner) => with_non_null(schema_type_ref(inner)),
    }
}

fn query_type_ref(value: &query::Type<'_, String>) -> GraphqlTypeRef {
    match value {
        query::Type::NamedType(name) => GraphqlTypeRef::Named {
            name: name.clone(),
            non_null: false,
        },
        query::Type::ListType(element) => GraphqlTypeRef::List {
            element: Box::new(query_type_ref(element)),
            non_null: false,
        },
        query::Type::NonNullType(inner) => with_non_null(query_type_ref(inner)),
    }
}

fn with_non_null(value: GraphqlTypeRef) -> GraphqlTypeRef {
    match value {
        GraphqlTypeRef::Named { name, .. } => GraphqlTypeRef::Named {
            name,
            non_null: true,
        },
        GraphqlTypeRef::List { element, .. } => GraphqlTypeRef::List {
            element,
            non_null: true,
        },
    }
}

fn directive_names(directives: &[schema::Directive<'_, String>]) -> Vec<String> {
    let mut names = directives
        .iter()
        .map(|directive| directive.name.clone())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn append_federation_directives(
    target: &str,
    directives: &[schema::Directive<'_, String>],
    fallback_line: usize,
    output: &mut Vec<GraphqlFederationMetadata>,
) {
    for directive in directives {
        if !is_federation_directive(&directive.name) {
            continue;
        }
        let arguments = directive
            .arguments
            .iter()
            .map(|(name, value)| (name.clone(), graphql_value_kind(value)))
            .collect();
        output.push(GraphqlFederationMetadata {
            directive: directive.name.clone(),
            target: target.to_owned(),
            arguments,
            lines: line_range(if directive.position.line == 0 {
                fallback_line
            } else {
                directive.position.line
            }),
        });
    }
}

fn is_federation_directive(name: &str) -> bool {
    matches!(
        name,
        "key"
            | "external"
            | "requires"
            | "provides"
            | "extends"
            | "shareable"
            | "override"
            | "inaccessible"
            | "tag"
            | "composeDirective"
            | "interfaceObject"
            | "link"
            | "merge"
            | "canonical"
            | "computed"
    )
}

fn graphql_value_kind(value: &schema::Value<'_, String>) -> GraphqlLiteralKind {
    match value {
        schema::Value::Variable(_) => GraphqlLiteralKind::Variable,
        schema::Value::Int(_) => GraphqlLiteralKind::Integer,
        schema::Value::Float(_) => GraphqlLiteralKind::Float,
        schema::Value::String(_) => GraphqlLiteralKind::String,
        schema::Value::Boolean(_) => GraphqlLiteralKind::Boolean,
        schema::Value::Null => GraphqlLiteralKind::Null,
        schema::Value::Enum(_) => GraphqlLiteralKind::Enum,
        schema::Value::List(_) => GraphqlLiteralKind::List,
        schema::Value::Object(_) => GraphqlLiteralKind::Object,
    }
}

#[derive(Debug, Clone, Default)]
struct FragmentExpansion {
    paths: BTreeSet<String>,
    spreads: BTreeSet<String>,
    missing: BTreeSet<String>,
}

#[derive(Debug, Clone)]
enum FragmentMemoState {
    Pending,
    InProgress,
    Completed(FragmentExpansion),
}

fn append_query_document(
    document: query::Document<'_, String>,
    output: &mut GraphqlDocument,
    tracker: &mut ExtractionTracker,
) -> Result<(), ExtractionLimitExceeded> {
    let mut operation_sources = Vec::new();
    let mut fragment_sources = BTreeMap::new();
    for definition in document.definitions {
        tracker.charge_work(1)?;
        match definition {
            query::Definition::Operation(operation) => operation_sources.push(operation),
            query::Definition::Fragment(fragment) => {
                fragment_sources.insert(fragment.name.clone(), fragment);
            }
        }
    }

    let mut memo = fragment_sources
        .keys()
        .map(|name| (name.clone(), FragmentMemoState::Pending))
        .collect::<BTreeMap<_, _>>();
    for fragment in fragment_sources.values() {
        output.fragments.push(fragment_definition(
            fragment,
            &fragment_sources,
            &mut memo,
            tracker,
        )?);
    }
    for operation in &operation_sources {
        output.operations.push(operation_definition(
            operation,
            &fragment_sources,
            &mut memo,
            tracker,
        )?);
    }
    Ok(())
}

fn fragment_definition(
    fragment: &query::FragmentDefinition<'_, String>,
    fragments: &BTreeMap<String, query::FragmentDefinition<'_, String>>,
    memo: &mut BTreeMap<String, FragmentMemoState>,
    tracker: &mut ExtractionTracker,
) -> Result<GraphqlFragment, ExtractionLimitExceeded> {
    let mut selections = Vec::new();
    let mut spreads = Vec::new();
    append_selections(
        &fragment.selection_set,
        None,
        &mut selections,
        &mut spreads,
        1,
        tracker,
    )?;
    let expansion = expand_fragment(&fragment.name, fragments, memo, 1, tracker)?;
    let query::TypeCondition::On(type_condition) = &fragment.type_condition;
    Ok(GraphqlFragment {
        name: fragment.name.clone(),
        type_condition: type_condition.clone(),
        consumed_field_paths: expansion.paths.into_iter().collect(),
        selections: sorted_unique(selections),
        fragment_spreads: expansion.spreads.into_iter().collect(),
        lines: line_range(fragment.position.line),
    })
}

fn operation_definition(
    operation: &query::OperationDefinition<'_, String>,
    fragments: &BTreeMap<String, query::FragmentDefinition<'_, String>>,
    memo: &mut BTreeMap<String, FragmentMemoState>,
    tracker: &mut ExtractionTracker,
) -> Result<GraphqlOperation, ExtractionLimitExceeded> {
    let (kind, name, variables, selection_set, line) = match operation {
        query::OperationDefinition::SelectionSet(selection_set) => (
            GraphqlOperationKind::Query,
            None,
            Vec::new(),
            selection_set,
            selection_set.span.0.line,
        ),
        query::OperationDefinition::Query(operation) => (
            GraphqlOperationKind::Query,
            operation.name.clone(),
            query_variables(&operation.variable_definitions),
            &operation.selection_set,
            operation.position.line,
        ),
        query::OperationDefinition::Mutation(operation) => (
            GraphqlOperationKind::Mutation,
            operation.name.clone(),
            query_variables(&operation.variable_definitions),
            &operation.selection_set,
            operation.position.line,
        ),
        query::OperationDefinition::Subscription(operation) => (
            GraphqlOperationKind::Subscription,
            operation.name.clone(),
            query_variables(&operation.variable_definitions),
            &operation.selection_set,
            operation.position.line,
        ),
    };
    let mut selections = Vec::new();
    let mut direct_spreads = Vec::new();
    append_selections(
        selection_set,
        None,
        &mut selections,
        &mut direct_spreads,
        1,
        tracker,
    )?;
    let expansion = expand_selection_set(selection_set, None, fragments, memo, 1, tracker)?;
    let warnings = expansion
        .missing
        .iter()
        .map(|name| format!("missing_fragment:{name}"))
        .collect::<Vec<_>>();
    Ok(GraphqlOperation {
        kind,
        name,
        variables,
        selections: sorted_unique(selections),
        consumed_field_paths: expansion.paths.into_iter().collect(),
        fragment_spreads: expansion.spreads.into_iter().collect(),
        complete: expansion.missing.is_empty(),
        warnings,
        lines: line_range(line),
    })
}

fn query_variables(
    variables: &[query::VariableDefinition<'_, String>],
) -> Vec<GraphqlArgumentDefinition> {
    let mut output = variables
        .iter()
        .map(|variable| GraphqlArgumentDefinition {
            name: variable.name.clone(),
            type_ref: query_type_ref(&variable.var_type),
            default_value_kind: variable.default_value.as_ref().map(graphql_value_kind),
            directives: Vec::new(),
            lines: line_range(variable.position.line),
        })
        .collect::<Vec<_>>();
    output.sort();
    output.dedup();
    output
}

fn append_selections(
    selection_set: &query::SelectionSet<'_, String>,
    parent: Option<&str>,
    output: &mut Vec<GraphqlSelection>,
    spreads: &mut Vec<String>,
    depth: u64,
    tracker: &mut ExtractionTracker,
) -> Result<(), ExtractionLimitExceeded> {
    tracker.check_structural_depth(depth)?;
    for selection in &selection_set.items {
        tracker.charge_work(1)?;
        match selection {
            query::Selection::Field(field) => {
                tracker.charge_identifier(&field.name)?;
                let path = join_field_path(parent, &field.name);
                tracker.charge_string(&path)?;
                output.push(GraphqlSelection::Field {
                    path: path.clone(),
                    name: field.name.clone(),
                    alias: field.alias.clone(),
                    lines: line_range(field.position.line),
                });
                append_selections(
                    &field.selection_set,
                    Some(&path),
                    output,
                    spreads,
                    depth.saturating_add(1),
                    tracker,
                )?;
            }
            query::Selection::FragmentSpread(spread) => {
                tracker.charge_identifier(&spread.fragment_name)?;
                spreads.push(spread.fragment_name.clone());
                output.push(GraphqlSelection::FragmentSpread {
                    name: spread.fragment_name.clone(),
                    parent_path: parent.map(str::to_owned),
                    lines: line_range(spread.position.line),
                });
            }
            query::Selection::InlineFragment(fragment) => {
                let type_condition = fragment.type_condition.as_ref().map(|condition| {
                    let query::TypeCondition::On(name) = condition;
                    name.clone()
                });
                output.push(GraphqlSelection::InlineFragment {
                    type_condition,
                    parent_path: parent.map(str::to_owned),
                    lines: line_range(fragment.position.line),
                });
                append_selections(
                    &fragment.selection_set,
                    parent,
                    output,
                    spreads,
                    depth.saturating_add(1),
                    tracker,
                )?;
            }
        }
    }
    Ok(())
}

fn expand_fragment(
    name: &str,
    fragments: &BTreeMap<String, query::FragmentDefinition<'_, String>>,
    memo: &mut BTreeMap<String, FragmentMemoState>,
    depth: u64,
    tracker: &mut ExtractionTracker,
) -> Result<FragmentExpansion, ExtractionLimitExceeded> {
    tracker.check_structural_depth(depth)?;
    tracker.charge_work(1)?;
    match memo.get(name) {
        Some(FragmentMemoState::Completed(expansion)) => {
            let materializations = expansion
                .paths
                .len()
                .saturating_add(expansion.spreads.len())
                .saturating_add(expansion.missing.len());
            tracker.charge_work(u64::try_from(materializations).unwrap_or(u64::MAX))?;
            return Ok(expansion.clone());
        }
        Some(FragmentMemoState::InProgress) => {
            return Ok(FragmentExpansion {
                missing: [format!("cycle:{name}")].into_iter().collect(),
                ..FragmentExpansion::default()
            });
        }
        Some(FragmentMemoState::Pending) | None => {}
    }
    let Some(fragment) = fragments.get(name) else {
        return Ok(FragmentExpansion {
            missing: [name.to_owned()].into_iter().collect(),
            ..FragmentExpansion::default()
        });
    };
    memo.insert(name.to_owned(), FragmentMemoState::InProgress);
    let expansion = expand_selection_set(
        &fragment.selection_set,
        None,
        fragments,
        memo,
        depth.saturating_add(1),
        tracker,
    )?;
    memo.insert(
        name.to_owned(),
        FragmentMemoState::Completed(expansion.clone()),
    );
    Ok(expansion)
}

fn expand_selection_set(
    selection_set: &query::SelectionSet<'_, String>,
    parent: Option<&str>,
    fragments: &BTreeMap<String, query::FragmentDefinition<'_, String>>,
    memo: &mut BTreeMap<String, FragmentMemoState>,
    depth: u64,
    tracker: &mut ExtractionTracker,
) -> Result<FragmentExpansion, ExtractionLimitExceeded> {
    tracker.check_structural_depth(depth)?;
    let mut expansion = FragmentExpansion::default();
    for selection in &selection_set.items {
        tracker.charge_work(1)?;
        match selection {
            query::Selection::Field(field) => {
                let path = join_field_path(parent, &field.name);
                tracker.charge_string(&path)?;
                expansion.paths.insert(path.clone());
                let nested = expand_selection_set(
                    &field.selection_set,
                    Some(&path),
                    fragments,
                    memo,
                    depth.saturating_add(1),
                    tracker,
                )?;
                merge_expansion(&mut expansion, nested);
            }
            query::Selection::InlineFragment(fragment) => {
                let nested = expand_selection_set(
                    &fragment.selection_set,
                    parent,
                    fragments,
                    memo,
                    depth.saturating_add(1),
                    tracker,
                )?;
                merge_expansion(&mut expansion, nested);
            }
            query::Selection::FragmentSpread(spread) => {
                expansion.spreads.insert(spread.fragment_name.clone());
                let cached = expand_fragment(
                    &spread.fragment_name,
                    fragments,
                    memo,
                    depth.saturating_add(1),
                    tracker,
                )?;
                expansion.spreads.extend(cached.spreads);
                expansion.missing.extend(cached.missing);
                for relative in cached.paths {
                    tracker.charge_work(1)?;
                    let materialized =
                        parent.map_or(relative.clone(), |prefix| format!("{prefix}.{relative}"));
                    tracker.charge_string(&materialized)?;
                    expansion.paths.insert(materialized);
                }
            }
        }
    }
    Ok(expansion)
}

fn merge_expansion(target: &mut FragmentExpansion, source: FragmentExpansion) {
    target.paths.extend(source.paths);
    target.spreads.extend(source.spreads);
    target.missing.extend(source.missing);
}

fn join_field_path(parent: Option<&str>, field: &str) -> String {
    parent.map_or_else(|| field.to_owned(), |prefix| format!("{prefix}.{field}"))
}

#[derive(Debug)]
struct EmbeddedLiteral {
    text: String,
    start_line: u32,
    dynamic: bool,
}

fn embedded_graphql_literals(language: SourceLanguage, input: &str) -> Vec<EmbeddedLiteral> {
    let markers: &[&str] = match language {
        SourceLanguage::JavaScript | SourceLanguage::TypeScript | SourceLanguage::Python => {
            &["gql", "graphql"]
        }
        SourceLanguage::Go | SourceLanguage::Java => &["graphql", "query"],
        SourceLanguage::Rust => &["graphql", "gql"],
    };
    let mut output = Vec::new();
    for marker in markers {
        let mut offset = 0;
        while let Some(relative) = input[offset..].find(marker) {
            let marker_start = offset.saturating_add(relative);
            if !word_boundary(input, marker_start, marker.len()) {
                offset = marker_start.saturating_add(marker.len());
                continue;
            }
            if let Some(literal) =
                literal_after_marker(input, marker_start.saturating_add(marker.len()), language)
                    .filter(|literal| looks_like_graphql(&literal.text) || literal.dynamic)
            {
                output.push(literal);
            }
            offset = marker_start.saturating_add(marker.len());
        }
    }
    output.sort_by_key(|literal| (literal.start_line, literal.text.clone()));
    output.dedup_by(|left, right| {
        left.start_line == right.start_line
            && left.text == right.text
            && left.dynamic == right.dynamic
    });
    output
}

fn literal_after_marker(
    input: &str,
    mut cursor: usize,
    language: SourceLanguage,
) -> Option<EmbeddedLiteral> {
    cursor = skip_ascii_whitespace(input, cursor);
    let mut call_like = false;
    while matches!(
        input.as_bytes().get(cursor),
        Some(b'(' | b'!' | b':' | b'=')
    ) {
        call_like |= matches!(input.as_bytes().get(cursor), Some(b'(' | b'!'));
        cursor = cursor.saturating_add(1);
        cursor = skip_ascii_whitespace(input, cursor);
    }
    if language == SourceLanguage::Rust && input.as_bytes().get(cursor) == Some(&b'r') {
        return rust_raw_literal(input, cursor);
    }
    let bytes = input.as_bytes();
    if bytes.get(cursor..cursor.saturating_add(3)) == Some(b"\"\"\"")
        || bytes.get(cursor..cursor.saturating_add(3)) == Some(b"'''")
    {
        return quoted_literal(input, cursor, 3);
    }
    match bytes.get(cursor) {
        Some(b'`' | b'"' | b'\'') => quoted_literal(input, cursor, 1),
        _ if call_like => Some(EmbeddedLiteral {
            text: String::new(),
            start_line: line_at_offset(input, cursor),
            dynamic: true,
        }),
        _ => None,
    }
}

fn quoted_literal(input: &str, start: usize, delimiter_len: usize) -> Option<EmbeddedLiteral> {
    let delimiter = input.get(start..start.saturating_add(delimiter_len))?;
    let content_start = start.saturating_add(delimiter_len);
    let mut cursor = content_start;
    let bytes = input.as_bytes();
    while cursor.saturating_add(delimiter_len) <= input.len() {
        if input.get(cursor..cursor.saturating_add(delimiter_len)) == Some(delimiter)
            && !is_escaped(bytes, cursor)
        {
            let text = input.get(content_start..cursor)?.to_owned();
            return Some(EmbeddedLiteral {
                dynamic: delimiter == "`" && text.contains("${"),
                text,
                start_line: line_at_offset(input, content_start),
            });
        }
        cursor = cursor.saturating_add(1);
    }
    None
}

fn rust_raw_literal(input: &str, start: usize) -> Option<EmbeddedLiteral> {
    let bytes = input.as_bytes();
    let mut cursor = start.saturating_add(1);
    let mut hashes = 0;
    while bytes.get(cursor) == Some(&b'#') {
        hashes += 1;
        cursor = cursor.saturating_add(1);
    }
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }
    let content_start = cursor.saturating_add(1);
    let terminator = format!("\"{}", "#".repeat(hashes));
    let relative_end = input.get(content_start..)?.find(&terminator)?;
    let end = content_start.saturating_add(relative_end);
    Some(EmbeddedLiteral {
        text: input.get(content_start..end)?.to_owned(),
        start_line: line_at_offset(input, content_start),
        dynamic: false,
    })
}

fn is_escaped(bytes: &[u8], index: usize) -> bool {
    let mut cursor = index;
    let mut slashes = 0;
    while cursor > 0 && bytes.get(cursor - 1) == Some(&b'\\') {
        slashes += 1;
        cursor -= 1;
    }
    slashes % 2 == 1
}

fn skip_ascii_whitespace(input: &str, mut cursor: usize) -> usize {
    while input
        .as_bytes()
        .get(cursor)
        .is_some_and(u8::is_ascii_whitespace)
    {
        cursor = cursor.saturating_add(1);
    }
    cursor
}

fn word_boundary(input: &str, start: usize, len: usize) -> bool {
    let bytes = input.as_bytes();
    let before = start.checked_sub(1).and_then(|index| bytes.get(index));
    let after = bytes.get(start.saturating_add(len));
    before.is_none_or(|byte| !is_identifier_byte(*byte))
        && after.is_none_or(|byte| !is_identifier_byte(*byte))
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn looks_like_graphql(value: &str) -> bool {
    let trimmed = value.trim_start_matches(|character: char| character.is_whitespace());
    [
        "query",
        "mutation",
        "subscription",
        "fragment",
        "type",
        "interface",
        "input",
        "enum",
        "union",
        "scalar",
        "schema",
        "extend",
        "{",
    ]
    .iter()
    .any(|prefix| trimmed.starts_with(prefix))
}

fn precheck_graphql_depth(
    input: &str,
    tracker: &ExtractionTracker,
) -> Result<(), ExtractionLimitExceeded> {
    let bytes = input.as_bytes();
    let mut cursor = 0;
    let mut depth = 0_u64;
    let mut quoted = false;
    let mut block_quoted = false;
    let mut escaped = false;
    let mut comment = false;
    while cursor < bytes.len() {
        if cursor % 1_024 == 0 {
            tracker.check_structured_time()?;
        }
        let byte = bytes[cursor];
        if comment {
            comment = byte != b'\n';
            cursor += 1;
            continue;
        }
        if block_quoted {
            if bytes.get(cursor..cursor.saturating_add(3)) == Some(b"\"\"\"") {
                block_quoted = false;
                cursor += 3;
            } else {
                cursor += 1;
            }
            continue;
        }
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
            cursor += 1;
            continue;
        }
        if byte == b'#' {
            comment = true;
        } else if bytes.get(cursor..cursor.saturating_add(3)) == Some(b"\"\"\"") {
            block_quoted = true;
            cursor += 3;
            continue;
        } else if byte == b'"' {
            quoted = true;
        } else if matches!(byte, b'{' | b'[' | b'(') {
            depth = depth.saturating_add(1);
            tracker.check_structural_depth(depth)?;
        } else if matches!(byte, b'}' | b']' | b')') {
            depth = depth.saturating_sub(1);
        }
        cursor += 1;
    }
    Ok(())
}

fn charge_schema_work(
    document: &schema::Document<'_, String>,
    tracker: &mut ExtractionTracker,
) -> Result<(), ExtractionLimitExceeded> {
    let units = document
        .definitions
        .iter()
        .fold(0_u64, |total, definition| {
            total.saturating_add(schema_definition_work_units(definition))
        });
    let mut remaining = units;
    while remaining > 0 {
        let chunk = remaining.min(1_024);
        tracker.charge_work(chunk)?;
        remaining -= chunk;
    }
    Ok(())
}

fn schema_definition_work_units(definition: &schema::Definition<'_, String>) -> u64 {
    let nested = match definition {
        schema::Definition::SchemaDefinition(value) => directives_work_units(&value.directives),
        schema::Definition::TypeDefinition(value) => type_definition_work_units(value),
        schema::Definition::TypeExtension(value) => type_extension_work_units(value),
        schema::Definition::DirectiveDefinition(value) => input_values_work_units(&value.arguments),
    };
    1_u64.saturating_add(nested)
}

fn type_definition_work_units(definition: &schema::TypeDefinition<'_, String>) -> u64 {
    match definition {
        schema::TypeDefinition::Scalar(value) => directives_work_units(&value.directives),
        schema::TypeDefinition::Object(value) => composite_work_units(
            &value.directives,
            value.implements_interfaces.len(),
            &value.fields,
        ),
        schema::TypeDefinition::Interface(value) => composite_work_units(
            &value.directives,
            value.implements_interfaces.len(),
            &value.fields,
        ),
        schema::TypeDefinition::Union(value) => {
            directives_work_units(&value.directives).saturating_add(usize_to_u64(value.types.len()))
        }
        schema::TypeDefinition::Enum(value) => directives_work_units(&value.directives)
            .saturating_add(value.values.iter().fold(0_u64, |total, enum_value| {
                total
                    .saturating_add(1)
                    .saturating_add(directives_work_units(&enum_value.directives))
            })),
        schema::TypeDefinition::InputObject(value) => directives_work_units(&value.directives)
            .saturating_add(input_values_work_units(&value.fields)),
    }
}

fn type_extension_work_units(extension: &schema::TypeExtension<'_, String>) -> u64 {
    match extension {
        schema::TypeExtension::Scalar(value) => directives_work_units(&value.directives),
        schema::TypeExtension::Object(value) => composite_work_units(
            &value.directives,
            value.implements_interfaces.len(),
            &value.fields,
        ),
        schema::TypeExtension::Interface(value) => composite_work_units(
            &value.directives,
            value.implements_interfaces.len(),
            &value.fields,
        ),
        schema::TypeExtension::Union(value) => {
            directives_work_units(&value.directives).saturating_add(usize_to_u64(value.types.len()))
        }
        schema::TypeExtension::Enum(value) => directives_work_units(&value.directives)
            .saturating_add(value.values.iter().fold(0_u64, |total, enum_value| {
                total
                    .saturating_add(1)
                    .saturating_add(directives_work_units(&enum_value.directives))
            })),
        schema::TypeExtension::InputObject(value) => directives_work_units(&value.directives)
            .saturating_add(input_values_work_units(&value.fields)),
    }
}

fn composite_work_units(
    directives: &[schema::Directive<'_, String>],
    implements: usize,
    fields: &[schema::Field<'_, String>],
) -> u64 {
    directives_work_units(directives)
        .saturating_add(usize_to_u64(implements))
        .saturating_add(fields.iter().fold(0_u64, |total, field| {
            total
                .saturating_add(1)
                .saturating_add(input_values_work_units(&field.arguments))
                .saturating_add(directives_work_units(&field.directives))
        }))
}

fn input_values_work_units(values: &[schema::InputValue<'_, String>]) -> u64 {
    values.iter().fold(0_u64, |total, value| {
        total
            .saturating_add(1)
            .saturating_add(directives_work_units(&value.directives))
            .saturating_add(
                value
                    .default_value
                    .as_ref()
                    .map_or(0, schema_value_work_units),
            )
    })
}

fn directives_work_units(directives: &[schema::Directive<'_, String>]) -> u64 {
    directives.iter().fold(0_u64, |total, directive| {
        total
            .saturating_add(1)
            .saturating_add(
                directive
                    .arguments
                    .iter()
                    .fold(0_u64, |arguments, (_, value)| {
                        arguments
                            .saturating_add(1)
                            .saturating_add(schema_value_work_units(value))
                    }),
            )
    })
}

fn schema_value_work_units(value: &schema::Value<'_, String>) -> u64 {
    let nested = match value {
        schema::Value::List(values) => values.iter().fold(0_u64, |total, value| {
            total.saturating_add(schema_value_work_units(value))
        }),
        schema::Value::Object(values) => values.values().fold(0_u64, |total, value| {
            total
                .saturating_add(1)
                .saturating_add(schema_value_work_units(value))
        }),
        schema::Value::Variable(_)
        | schema::Value::Int(_)
        | schema::Value::Float(_)
        | schema::Value::String(_)
        | schema::Value::Boolean(_)
        | schema::Value::Null
        | schema::Value::Enum(_) => 0,
    };
    1_u64.saturating_add(nested)
}

fn usize_to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn charge_graphql_document(
    document: &GraphqlDocument,
    tracker: &mut ExtractionTracker,
) -> Result<(), ExtractionLimitExceeded> {
    tracker.charge_portable_path(&document.source_path)?;
    let observations = document
        .types
        .len()
        .saturating_add(document.operations.len())
        .saturating_add(document.fragments.len())
        .saturating_add(document.persisted_operations.len())
        .saturating_add(document.resolvers.len())
        .saturating_add(document.federation.len());
    tracker.charge_observation(u64::try_from(observations).unwrap_or(u64::MAX))?;
    for value in document
        .types
        .iter()
        .map(|item| item.name.as_str())
        .chain(document.fragments.iter().map(|item| item.name.as_str()))
        .chain(
            document
                .resolvers
                .iter()
                .map(|item| item.coordinate.as_str()),
        )
    {
        tracker.charge_identifier(value)?;
    }
    for path in document
        .operations
        .iter()
        .flat_map(|operation| &operation.consumed_field_paths)
        .chain(
            document
                .fragments
                .iter()
                .flat_map(|fragment| &fragment.consumed_field_paths),
        )
    {
        tracker.charge_string(path)?;
    }
    Ok(())
}

fn extract_resolvers(language: SourceLanguage, input: &str) -> Vec<GraphqlResolver> {
    let mut output = match language {
        SourceLanguage::JavaScript | SourceLanguage::TypeScript => {
            extract_ecmascript_resolvers(language, input)
        }
        SourceLanguage::Python => extract_python_resolvers(input),
        SourceLanguage::Go => extract_go_resolvers(input),
        SourceLanguage::Java => extract_java_resolvers(input),
        SourceLanguage::Rust => extract_rust_resolvers(input),
    };
    output.sort();
    output.dedup();
    output
}

fn extract_ecmascript_resolvers(language: SourceLanguage, input: &str) -> Vec<GraphqlResolver> {
    let mut output = Vec::new();
    let mut in_resolvers = false;
    let mut current_type: Option<(String, i32)> = None;
    let mut depth = 0_i32;
    for (index, line) in input.lines().enumerate() {
        let trimmed = line.trim();
        if !in_resolvers && resolver_object_start(trimmed) {
            in_resolvers = true;
            depth = brace_delta(trimmed);
            continue;
        }
        if !in_resolvers {
            continue;
        }
        let previous_depth = depth;
        depth += brace_delta(trimmed);
        if let Some((name, _)) = &current_type {
            if let Some((field, symbol)) = object_symbol_mapping(trimmed) {
                output.push(resolver(
                    name,
                    &field,
                    &symbol,
                    language,
                    index.saturating_add(1),
                ));
            }
        } else if let Some(type_name) = object_type_header(trimmed) {
            current_type = Some((type_name, previous_depth));
        }
        if current_type
            .as_ref()
            .is_some_and(|(_, type_depth)| depth <= *type_depth)
        {
            current_type = None;
        }
        if depth <= 0 {
            in_resolvers = false;
            current_type = None;
        }
    }
    output
}

fn resolver_object_start(line: &str) -> bool {
    if line.contains("resolvers: {") {
        return true;
    }
    let Some((declaration, value)) = line.split_once('=') else {
        return false;
    };
    declaration
        .split(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .any(|word| word == "resolvers")
        && value.trim_start().starts_with('{')
}

fn object_type_header(line: &str) -> Option<String> {
    let (name, rest) = line.split_once(':')?;
    let name = name
        .trim()
        .trim_matches(|character| character == '\'' || character == '"');
    let rest = rest.trim_start();
    (valid_graphql_name(name) && rest.starts_with('{')).then(|| name.to_owned())
}

fn object_symbol_mapping(line: &str) -> Option<(String, String)> {
    let (field, rest) = line.split_once(':')?;
    let field = field
        .trim()
        .trim_matches(|character| character == '\'' || character == '"');
    let symbol = rest
        .trim()
        .trim_end_matches(',')
        .split_whitespace()
        .next()
        .unwrap_or_default();
    (valid_graphql_name(field)
        && valid_symbol(symbol)
        && !matches!(symbol, "function" | "async")
        && !rest.contains("=>"))
    .then(|| (field.to_owned(), symbol.to_owned()))
}

fn extract_python_resolvers(input: &str) -> Vec<GraphqlResolver> {
    let mut output = Vec::new();
    let mut pending_ariadne: Option<(String, String, usize)> = None;
    let mut pending_strawberry = false;
    let mut pending_strawberry_type = false;
    let mut class_type: Option<(String, usize)> = None;
    for (index, line) in input.lines().enumerate() {
        let trimmed = line.trim();
        if let Some((receiver, field)) = python_field_decorator(trimmed) {
            pending_ariadne = Some((receiver_to_type(&receiver), field, index.saturating_add(1)));
            continue;
        }
        if matches!(trimmed, "@strawberry.field" | "@strawberry.mutation") {
            pending_strawberry = true;
            continue;
        }
        if trimmed == "@strawberry.type" {
            pending_strawberry_type = true;
            continue;
        }
        if let Some(name) = python_graphql_class(trimmed, pending_strawberry_type) {
            class_type = Some((name, indentation(line)));
            pending_strawberry_type = false;
            continue;
        }
        if !trimmed.starts_with('@') && !trimmed.is_empty() {
            pending_strawberry_type = false;
        }
        if let Some((name, indent)) = &class_type {
            if !trimmed.is_empty() && indentation(line) <= *indent && !trimmed.starts_with('@') {
                class_type = None;
            } else if let Some(symbol) = python_function_name(trimmed) {
                if pending_strawberry {
                    output.push(resolver(
                        name,
                        &symbol,
                        &format!("{name}.{symbol}"),
                        SourceLanguage::Python,
                        index.saturating_add(1),
                    ));
                    pending_strawberry = false;
                } else if let Some(field) = symbol.strip_prefix("resolve_") {
                    output.push(resolver(
                        name,
                        field,
                        &format!("{name}.{symbol}"),
                        SourceLanguage::Python,
                        index.saturating_add(1),
                    ));
                }
            }
        }
        if let Some(symbol) = python_function_name(trimmed) {
            if let Some((type_name, field, line_number)) = pending_ariadne.take() {
                output.push(resolver(
                    &type_name,
                    &field,
                    &symbol,
                    SourceLanguage::Python,
                    line_number,
                ));
            }
        } else if !trimmed.starts_with('@') && !trimmed.is_empty() {
            pending_ariadne = None;
        }
    }
    output
}

fn python_field_decorator(line: &str) -> Option<(String, String)> {
    let value = line.strip_prefix('@')?;
    let (receiver, rest) = value.split_once(".field(")?;
    let field = first_quoted_value(rest)?;
    (valid_symbol(receiver) && valid_graphql_name(&field)).then(|| (receiver.to_owned(), field))
}

fn python_graphql_class(line: &str, strawberry_type: bool) -> Option<String> {
    let rest = line.strip_prefix("class ")?;
    let name = rest.split(['(', ':']).next()?.trim();
    let supported = strawberry_type
        || rest.contains("graphene.ObjectType")
        || rest.contains("ObjectType")
        || matches!(name, "Query" | "Mutation" | "Subscription");
    (supported && valid_graphql_name(name)).then(|| name.to_owned())
}

fn python_function_name(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("def ")
        .or_else(|| line.strip_prefix("async def "))?;
    let name = rest.split('(').next()?.trim();
    valid_graphql_name(name).then(|| name.to_owned())
}

fn extract_go_resolvers(input: &str) -> Vec<GraphqlResolver> {
    let mut output = Vec::new();
    for (index, line) in input.lines().enumerate() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("func (") else {
            continue;
        };
        let Some((receiver, method_part)) = rest.split_once(") ") else {
            continue;
        };
        let Some(receiver_type) = receiver.split_whitespace().last() else {
            continue;
        };
        let receiver_type = receiver_type.trim_start_matches('*');
        let Some(base) = receiver_type.strip_suffix("Resolver") else {
            continue;
        };
        let Some(method) = method_part.split('(').next() else {
            continue;
        };
        if !valid_graphql_name(method) || !valid_graphql_name(base) {
            continue;
        }
        let type_name = upper_first(base);
        output.push(resolver(
            &type_name,
            &lower_first(method),
            &format!("{receiver_type}.{method}"),
            SourceLanguage::Go,
            index.saturating_add(1),
        ));
    }
    output
}

fn extract_java_resolvers(input: &str) -> Vec<GraphqlResolver> {
    let mut output = Vec::new();
    let mut pending: Option<(String, Option<String>, usize)> = None;
    let mut class_name = String::new();
    for (index, line) in input.lines().enumerate() {
        let trimmed = line.trim();
        if let Some(name) = java_class_name(trimmed) {
            class_name = name;
        }
        if trimmed.starts_with("@QueryMapping") {
            pending = Some((
                "Query".to_owned(),
                annotation_value(trimmed),
                index.saturating_add(1),
            ));
            continue;
        }
        if trimmed.starts_with("@MutationMapping") {
            pending = Some((
                "Mutation".to_owned(),
                annotation_value(trimmed),
                index.saturating_add(1),
            ));
            continue;
        }
        if trimmed.starts_with("@SubscriptionMapping") {
            pending = Some((
                "Subscription".to_owned(),
                annotation_value(trimmed),
                index.saturating_add(1),
            ));
            continue;
        }
        if trimmed.starts_with("@SchemaMapping") {
            if let Some((type_name, field)) = schema_mapping_arguments(trimmed) {
                pending = Some((type_name, field, index.saturating_add(1)));
            }
            continue;
        }
        if trimmed.starts_with("@DgsData") {
            if let (Some(type_name), Some(field)) = (
                named_annotation_value(trimmed, "parentType"),
                named_annotation_value(trimmed, "field"),
            ) {
                pending = Some((type_name, Some(field), index.saturating_add(1)));
            }
            continue;
        }
        if trimmed.starts_with("@DgsQuery") {
            pending = Some((
                "Query".to_owned(),
                named_annotation_value(trimmed, "field"),
                index.saturating_add(1),
            ));
            continue;
        }
        if trimmed.starts_with("@DgsMutation") {
            pending = Some((
                "Mutation".to_owned(),
                named_annotation_value(trimmed, "field"),
                index.saturating_add(1),
            ));
            continue;
        }
        let Some((type_name, field_override, evidence_line)) = pending.take() else {
            continue;
        };
        let Some(method) = java_method_name(trimmed) else {
            if trimmed.starts_with('@') || trimmed.is_empty() {
                pending = Some((type_name, field_override, evidence_line));
            }
            continue;
        };
        let field = field_override.unwrap_or_else(|| method.clone());
        let symbol = if class_name.is_empty() {
            method.clone()
        } else {
            format!("{class_name}.{method}")
        };
        output.push(resolver(
            &type_name,
            &field,
            &symbol,
            SourceLanguage::Java,
            evidence_line,
        ));
    }
    output
}

fn java_class_name(line: &str) -> Option<String> {
    let (_, rest) = line.split_once("class ")?;
    let name = rest
        .split(|character: char| character.is_whitespace() || character == '{')
        .next()?;
    valid_symbol(name).then(|| name.to_owned())
}

fn java_method_name(line: &str) -> Option<String> {
    if !line.contains('(')
        || matches!(
            line.split_whitespace().next(),
            Some("if" | "for" | "while" | "switch")
        )
    {
        return None;
    }
    let prefix = line.split('(').next()?.trim();
    let name = prefix.split_whitespace().last()?;
    valid_symbol(name).then(|| name.to_owned())
}

fn schema_mapping_arguments(line: &str) -> Option<(String, Option<String>)> {
    let type_name = named_annotation_value(line, "typeName")?;
    let field = named_annotation_value(line, "field");
    Some((type_name, field))
}

fn annotation_value(line: &str) -> Option<String> {
    first_quoted_value(line)
}

fn named_annotation_value(line: &str, name: &str) -> Option<String> {
    let (_, rest) = line.split_once(name)?;
    let (_, value) = rest.split_once('=')?;
    first_quoted_value(value)
}

fn extract_rust_resolvers(input: &str) -> Vec<GraphqlResolver> {
    let mut output = Vec::new();
    let mut graphql_attribute = false;
    let mut active_impl: Option<(String, i32)> = None;
    let mut depth = 0_i32;
    for (index, line) in input.lines().enumerate() {
        let trimmed = line.trim();
        if matches!(
            trimmed,
            "#[Object]"
                | "#[graphql_object]"
                | "#[Subscription]"
                | "#[ComplexObject]"
                | "#[juniper::graphql_object]"
        ) {
            graphql_attribute = true;
            continue;
        }
        let previous_depth = depth;
        depth += brace_delta(trimmed);
        if graphql_attribute {
            if let Some(type_name) = rust_impl_type(trimmed) {
                active_impl = Some((type_name, previous_depth));
                graphql_attribute = false;
                continue;
            }
            if !trimmed.starts_with('#') && !trimmed.is_empty() {
                graphql_attribute = false;
            }
        }
        if let Some((type_name, impl_depth)) = &active_impl {
            if let Some(method) = rust_function_name(trimmed) {
                output.push(resolver(
                    type_name,
                    &method,
                    &format!("{type_name}::{method}"),
                    SourceLanguage::Rust,
                    index.saturating_add(1),
                ));
            }
            if depth <= *impl_depth {
                active_impl = None;
            }
        }
    }
    output
}

fn rust_impl_type(line: &str) -> Option<String> {
    let rest = line.strip_prefix("impl ")?;
    let before_brace = rest.split('{').next()?.trim();
    let type_name = before_brace
        .split(" for ")
        .last()?
        .split('<')
        .next()?
        .trim();
    valid_symbol(type_name).then(|| type_name.to_owned())
}

fn rust_function_name(line: &str) -> Option<String> {
    let position = line.find("fn ")?;
    let rest = line.get(position.saturating_add(3)..)?;
    let name = rest.split('(').next()?.trim();
    valid_graphql_name(name).then(|| name.to_owned())
}

fn resolver(
    type_name: &str,
    field_name: &str,
    symbol: &str,
    language: SourceLanguage,
    line: usize,
) -> GraphqlResolver {
    GraphqlResolver {
        type_name: type_name.to_owned(),
        field_name: field_name.to_owned(),
        coordinate: format!("{type_name}.{field_name}"),
        symbol: symbol.to_owned(),
        language,
        lines: line_range(line),
    }
}

fn receiver_to_type(receiver: &str) -> String {
    let trimmed = receiver
        .trim_end_matches("_type")
        .trim_end_matches("Type")
        .trim_end_matches("_resolver");
    upper_first(trimmed)
}

fn first_quoted_value(value: &str) -> Option<String> {
    let quote_index = value.find(['\'', '"'])?;
    let quote = *value.as_bytes().get(quote_index)?;
    let rest = value.get(quote_index.saturating_add(1)..)?;
    let end = rest.as_bytes().iter().position(|byte| *byte == quote)?;
    rest.get(..end).map(str::to_owned)
}

fn valid_graphql_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(is_identifier_byte)
}

fn valid_symbol(value: &str) -> bool {
    !value.is_empty()
        && value
            .split(['.', ':'])
            .filter(|part| !part.is_empty())
            .all(valid_graphql_name)
}

fn upper_first(value: &str) -> String {
    let mut characters = value.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>() + characters.as_str()
    })
}

fn lower_first(value: &str) -> String {
    let mut characters = value.chars();
    characters.next().map_or_else(String::new, |first| {
        first.to_lowercase().collect::<String>() + characters.as_str()
    })
}

fn indentation(line: &str) -> usize {
    line.len().saturating_sub(line.trim_start().len())
}

fn brace_delta(line: &str) -> i32 {
    line.bytes().fold(0, |delta, byte| match byte {
        b'{' => delta.saturating_add(1),
        b'}' => delta.saturating_sub(1),
        _ => delta,
    })
}

fn merge_document(target: &mut GraphqlDocument, mut source: GraphqlDocument) {
    target.types.append(&mut source.types);
    target.operations.append(&mut source.operations);
    target.fragments.append(&mut source.fragments);
    target
        .persisted_operations
        .append(&mut source.persisted_operations);
    target.resolvers.append(&mut source.resolvers);
    target.federation.append(&mut source.federation);
    target.complete &= source.complete;
    target.warnings.append(&mut source.warnings);
}

fn shift_document_lines(document: &mut GraphqlDocument, offset: u32) {
    for type_definition in &mut document.types {
        shift_lines(&mut type_definition.lines, offset);
        for field in &mut type_definition.fields {
            shift_lines(&mut field.lines, offset);
            for argument in &mut field.arguments {
                shift_lines(&mut argument.lines, offset);
            }
        }
    }
    for operation in &mut document.operations {
        shift_lines(&mut operation.lines, offset);
        for variable in &mut operation.variables {
            shift_lines(&mut variable.lines, offset);
        }
        shift_selection_lines(&mut operation.selections, offset);
    }
    for fragment in &mut document.fragments {
        shift_lines(&mut fragment.lines, offset);
        shift_selection_lines(&mut fragment.selections, offset);
    }
    for metadata in &mut document.federation {
        shift_lines(&mut metadata.lines, offset);
    }
}

fn shift_selection_lines(selections: &mut [GraphqlSelection], offset: u32) {
    for selection in selections {
        match selection {
            GraphqlSelection::Field { lines, .. }
            | GraphqlSelection::FragmentSpread { lines, .. }
            | GraphqlSelection::InlineFragment { lines, .. } => shift_lines(lines, offset),
        }
    }
}

fn shift_lines(lines: &mut GraphqlLineRange, offset: u32) {
    lines.start = lines.start.saturating_add(offset);
    lines.end = lines.end.saturating_add(offset);
}

fn finish_document(document: &mut GraphqlDocument) {
    document.types.sort();
    document.types.dedup();
    document.operations.sort();
    document.operations.dedup();
    document.fragments.sort();
    document.fragments.dedup();
    document.persisted_operations.sort();
    document.persisted_operations.dedup();
    document.resolvers.sort();
    document.resolvers.dedup();
    document.federation.sort();
    document.federation.dedup();
    document.warnings.sort();
    document.warnings.dedup();
}

fn sorted_strings<I>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    let mut output = values.into_iter().collect::<Vec<_>>();
    output.sort();
    output.dedup();
    output
}

fn sorted_unique<T>(mut values: Vec<T>) -> Vec<T>
where
    T: Ord,
{
    values.sort();
    values.dedup();
    values
}

fn line_range(line: usize) -> GraphqlLineRange {
    let line = usize_to_u32(line.max(1));
    GraphqlLineRange {
        start: line,
        end: line,
    }
}

fn line_at_offset(input: &str, offset: usize) -> u32 {
    usize_to_u32(
        input
            .as_bytes()
            .iter()
            .take(offset)
            .filter(|byte| **byte == b'\n')
            .count()
            .saturating_add(1),
    )
}

fn usize_to_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ExtractionResource;

    #[test]
    fn extracts_complete_sdl_type_shapes_and_federation() {
        let input = r#"
            scalar DateTime
            interface Node { id: ID! }
            type User implements Node @key(fields: "id") {
              id: ID!
              friends(limit: Int = 10): [User!]!
              secret: String @requires(fields: "id")
            }
            input UserFilter { active: Boolean! }
            enum Role { ADMIN USER }
            union SearchResult = User
        "#;

        let document = extract_graphql_document("schema.graphql", input)
            .expect("valid SDL should be extracted");

        assert_eq!(document.types.len(), 6);
        let user = document
            .types
            .iter()
            .find(|definition| definition.name == "User")
            .expect("User type should exist");
        let friends = user
            .fields
            .iter()
            .find(|field| field.name == "friends")
            .expect("friends field should exist");
        assert_eq!(friends.type_ref.as_graphql(), "[User!]!");
        assert_eq!(
            friends.arguments[0].default_value_kind,
            Some(GraphqlLiteralKind::Integer)
        );
        assert_eq!(document.federation.len(), 2);
    }

    #[test]
    fn extracts_operations_fragments_and_expanded_consumed_paths() {
        let input = r"
            query GetUser($id: ID!) {
              user(id: $id) {
                ...UserFields
              }
            }
            fragment UserFields on User {
              id
              profile { name }
            }
        ";

        let document = extract_graphql_document("operation.graphql", input)
            .expect("valid query should be extracted");

        assert_eq!(
            document.operations[0].consumed_field_paths,
            vec!["user", "user.id", "user.profile", "user.profile.name"]
        );
        assert_eq!(document.operations[0].fragment_spreads, vec!["UserFields"]);
    }

    #[test]
    fn repeated_fragment_dag_should_memoize_and_deduplicate_transitive_paths() {
        let input = r"
            query Viewer {
              viewer { ...Identity ...Profile ...Identity }
            }
            fragment Identity on User { id ...Shared }
            fragment Profile on User { profile { name } ...Shared }
            fragment Shared on User { tenant { id } }
        ";

        let document = extract_graphql_document("dag.graphql", input)
            .expect("acyclic repeated spreads should remain bounded and valid");

        assert_eq!(
            document.operations[0].consumed_field_paths,
            vec![
                "viewer",
                "viewer.id",
                "viewer.profile",
                "viewer.profile.name",
                "viewer.tenant",
                "viewer.tenant.id",
            ]
        );
        assert_eq!(
            document.operations[0].fragment_spreads,
            vec!["Identity", "Profile", "Shared"]
        );
    }

    #[test]
    fn fragment_cycles_and_missing_fragments_should_be_preserved_as_incomplete() {
        let input = r"
            query Viewer { viewer { ...A ...Missing } }
            fragment A on User { id ...B }
            fragment B on User { name ...A }
        ";

        let document = extract_graphql_document("cycles.graphql", input)
            .expect("cycles are represented as incomplete facts, not recursive failure");
        let operation = &document.operations[0];

        assert!(!operation.complete);
        assert!(
            operation
                .warnings
                .iter()
                .any(|warning| warning.contains("Missing"))
        );
        assert!(
            operation
                .warnings
                .iter()
                .any(|warning| warning.contains("cycle:A"))
        );
        assert_eq!(
            operation.consumed_field_paths,
            vec!["viewer", "viewer.id", "viewer.name"]
        );
    }

    #[test]
    fn literal_values_should_never_appear_in_serialized_graphql_payloads() {
        let input = r#"
            type User @key(fields: "top-secret-federation-field") {
              lookup(token: String = "top-secret-default", count: Int = 8675309): String
            }
            input Filter { enabled: Boolean = true }
        "#;

        let document = extract_graphql_document("schema.graphql", input)
            .expect("valid SDL should be extracted");
        let serialized = serde_json::to_string(&document).expect("document should serialize");

        assert!(!serialized.contains("top-secret-federation-field"));
        assert!(!serialized.contains("top-secret-default"));
        assert!(!serialized.contains("8675309"));
        assert!(serialized.contains("default_value_kind"));
        assert!(serialized.contains("string"));
        assert!(serialized.contains("integer"));
    }

    #[test]
    fn rejects_invalid_standalone_graphql() {
        let error = extract_graphql_document("broken.graphql", "query Broken { user(")
            .expect_err("invalid GraphQL should fail");

        assert!(matches!(
            error,
            GraphqlExtractionError::InvalidGraphql { .. }
        ));
    }

    #[test]
    fn extracts_identifier_to_query_persisted_map_without_body() {
        let input = r#"{
          "a1b2": "query Viewer { viewer { id } }",
          "c3d4": "mutation Rename { renameUser { id } }"
        }"#;

        let operations = extract_graphql_persisted_operations("manifest.json", input)
            .expect("valid manifest should be extracted");
        let serialized =
            serde_json::to_string(&operations).expect("persisted operations should serialize");

        assert_eq!(operations.len(), 2);
        assert!(!serialized.contains("query Viewer"));
        assert_eq!(operations[0].id, "a1b2");
    }

    #[test]
    fn extracts_apollo_operations_array() {
        let input = r#"{
          "format": "apollo-persisted-query-manifest",
          "version": 1,
          "operations": [
            {
              "id": "sha256",
              "name": "Viewer",
              "body": "query Viewer { viewer { id } }"
            }
          ]
        }"#;

        let operations = extract_graphql_persisted_operations("persisted.json", input)
            .expect("Apollo manifest should be extracted");

        assert_eq!(operations[0].operation_name.as_deref(), Some("Viewer"));
        assert_eq!(
            operations[0].consumed_field_paths,
            vec!["viewer", "viewer.id"]
        );
    }

    #[test]
    fn retains_incomplete_manifest_entry_without_document() {
        let input = r#"{"operations":[{"id":"known","name":"Viewer"}]}"#;

        let operations = extract_graphql_persisted_operations("persisted.json", input)
            .expect("metadata-only operation should be retained");

        assert!(!operations[0].complete);
        assert_eq!(operations[0].warnings, vec!["missing_operation_document"]);
    }

    #[test]
    fn preserves_persisted_identifier_line_evidence() {
        let input = "{\n  \"operations\": [\n    {\"id\":\"known\",\"name\":\"Viewer\"}\n  ]\n}";

        let operations = extract_graphql_persisted_operations("persisted.json", input)
            .expect("manifest entry should be extracted");

        assert_eq!(operations[0].lines.start, 3);
    }

    #[test]
    fn rejects_invalid_persisted_json() {
        let error = extract_graphql_persisted_operations("manifest.json", "{")
            .expect_err("invalid JSON should fail");

        assert!(matches!(error, GraphqlExtractionError::InvalidJson { .. }));
    }

    #[test]
    fn extracts_javascript_literal_and_named_resolver() {
        let input = r"
            const operation = gql`
              query Viewer { viewer { id } }
            `;
            const resolvers: Resolvers = {
              Query: {
                viewer: resolveViewer,
                dynamic: () => loadViewer(),
              },
            };
        ";

        let document = parse_graphql_source(SourceLanguage::JavaScript, input)
            .expect("bounded JavaScript extraction should succeed");

        assert_eq!(document.operations.len(), 1);
        assert_eq!(document.resolvers.len(), 1);
        assert_eq!(document.resolvers[0].coordinate, "Query.viewer");
        assert_eq!(document.resolvers[0].symbol, "resolveViewer");
    }

    #[test]
    fn marks_dynamic_javascript_graphql_literal_incomplete() {
        let input = "const operation = gql`query Viewer { viewer(id: ${id}) { id } }`;";

        let document = parse_graphql_source(SourceLanguage::TypeScript, input)
            .expect("bounded TypeScript extraction should succeed");

        assert!(!document.complete);
        assert_eq!(document.operations.len(), 0);
        assert_eq!(document.warnings, vec!["dynamic_graphql_literal:1"]);
    }

    #[test]
    fn marks_nonliteral_graphql_call_incomplete() {
        let input = "const operation = gql(buildOperation());";

        let document = parse_graphql_source(SourceLanguage::JavaScript, input)
            .expect("bounded JavaScript extraction should succeed");

        assert!(!document.complete);
        assert_eq!(document.warnings, vec!["dynamic_graphql_literal:1"]);
    }

    #[test]
    fn extracts_go_raw_string_graphql_assignment() {
        let input = "const query = `query Viewer { viewer { id } }`";

        let document = parse_graphql_source(SourceLanguage::Go, input)
            .expect("bounded Go extraction should succeed");

        assert_eq!(document.operations.len(), 1);
        assert_eq!(
            document.operations[0].consumed_field_paths,
            vec!["viewer", "viewer.id"]
        );
    }

    #[test]
    fn marks_invalid_embedded_literal_incomplete() {
        let input = "operation = gql(\"query Broken { viewer(\")";

        let document = parse_graphql_source(SourceLanguage::Python, input)
            .expect("bounded Python extraction should succeed");

        assert!(!document.complete);
        assert_eq!(document.warnings, vec!["invalid_embedded_graphql:1"]);
    }

    #[test]
    fn embedded_graphql_should_propagate_budget_failures() {
        let budgets = ExtractionBudgets {
            max_work_units_per_artifact: 1,
            ..ExtractionBudgets::default()
        };
        let mut tracker = ExtractionTracker::new("source.js", "graphql-source", &budgets);
        let result = parse_graphql_source_with_tracker(
            SourceLanguage::JavaScript,
            "const operation = gql`query Viewer { viewer { id } }`;",
            &mut tracker,
        );

        assert!(matches!(
            result,
            Err(GraphqlExtractionError::LimitExceeded(
                ExtractionLimitExceeded {
                    resource: ExtractionResource::WorkUnits,
                    ..
                }
            ))
        ));
    }

    #[test]
    fn extracts_python_ariadne_and_strawberry_resolvers() {
        let input = r#"
            @query.field("viewer")
            def resolve_viewer(_, info):
                return info.context.viewer

            @strawberry.type
            class User:
                @strawberry.field
                def display_name(self) -> str:
                    return self.name
        "#;

        let document = parse_graphql_source(SourceLanguage::Python, input)
            .expect("bounded Python extraction should succeed");

        assert_eq!(document.resolvers.len(), 2);
        assert_eq!(document.resolvers[0].coordinate, "Query.viewer");
        assert_eq!(document.resolvers[1].coordinate, "User.display_name");
    }

    #[test]
    fn extracts_go_gqlgen_resolver() {
        let input = r"
            func (r *queryResolver) User(ctx context.Context, id string) (*model.User, error) {
                return r.service.User(ctx, id)
            }
        ";

        let document = parse_graphql_source(SourceLanguage::Go, input)
            .expect("bounded Go extraction should succeed");

        assert_eq!(document.resolvers[0].coordinate, "Query.user");
        assert_eq!(document.resolvers[0].symbol, "queryResolver.User");
    }

    #[test]
    fn extracts_java_spring_graphql_resolver() {
        let input = r#"
            class UserController {
              @SchemaMapping(typeName = "User", field = "displayName")
              public String displayName(User user) { return user.name(); }
            }
        "#;

        let document = parse_graphql_source(SourceLanguage::Java, input)
            .expect("bounded Java extraction should succeed");

        assert_eq!(document.resolvers[0].coordinate, "User.displayName");
        assert_eq!(document.resolvers[0].symbol, "UserController.displayName");
    }

    #[test]
    fn extracts_java_dgs_resolver() {
        let input = r#"
            class ViewerFetcher {
              @DgsData(parentType = "Query", field = "viewer")
              public User loadViewer() { return service.viewer(); }
            }
        "#;

        let document = parse_graphql_source(SourceLanguage::Java, input)
            .expect("bounded Java extraction should succeed");

        assert_eq!(document.resolvers[0].coordinate, "Query.viewer");
        assert_eq!(document.resolvers[0].symbol, "ViewerFetcher.loadViewer");
    }

    #[test]
    fn extracts_rust_async_graphql_resolver() {
        let input = r"
            #[Object]
            impl Query {
                async fn viewer(&self) -> User {
                    self.viewer.clone()
                }
            }
        ";

        let document = parse_graphql_source(SourceLanguage::Rust, input)
            .expect("bounded Rust extraction should succeed");

        assert_eq!(document.resolvers[0].coordinate, "Query.viewer");
        assert_eq!(document.resolvers[0].symbol, "Query::viewer");
    }

    #[test]
    fn ignores_dynamic_and_inline_resolver_symbols() {
        let input = r"
            const resolvers = {
              Query: {
                viewer: makeResolver(config),
                inline: (_, args) => args.id,
              },
            };
        ";

        let document = parse_graphql_source(SourceLanguage::TypeScript, input)
            .expect("bounded TypeScript extraction should succeed");

        assert!(document.resolvers.is_empty());
    }

    #[test]
    fn serializes_owned_document_after_input_is_dropped() {
        let document = {
            let input = String::from("query Viewer { viewer { id } }");
            extract_graphql_document("viewer.graphql", &input)
                .expect("valid query should produce an owned document")
        };

        assert!(serde_json::to_string(&document).is_ok());
    }

    #[test]
    fn schema_ast_work_should_be_charged_before_materialization() {
        let budgets = ExtractionBudgets {
            max_work_units_per_artifact: 1,
            ..ExtractionBudgets::default()
        };
        let mut tracker = ExtractionTracker::new(
            "schema.graphql",
            "code-system-graph.graphql.document",
            &budgets,
        );

        let error = extract_graphql_document_with_tracker(
            "schema.graphql",
            "type Query { viewer: String }",
            &mut tracker,
        )
        .expect_err("the schema definition and field should exceed one work unit");

        assert!(matches!(
            error,
            GraphqlExtractionError::LimitExceeded(ExtractionLimitExceeded {
                resource: crate::ExtractionResource::WorkUnits,
                observed: 2,
                maximum: 1,
                ..
            })
        ));
    }
}

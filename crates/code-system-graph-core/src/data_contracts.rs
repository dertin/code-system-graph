//! Bounded, evidence-first extraction of database declarations and literal SQL access.
//!
//! Extracted documents contain names and structural metadata only. SQL bodies, default values,
//! connection strings, credentials, and other source literals are never retained.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use sqlparser::ast::{
    AlterTableOperation, ColumnDef, ColumnOption, Expr, FromTable, IndexColumn, ObjectName, Query, SetExpr, Statement, TableConstraint, TableFactor, TableObject, TableWithJoins
};
use sqlparser::dialect::{GenericDialect, MySqlDialect};
use sqlparser::parser::Parser;
use thiserror::Error;
use tree_sitter::Node as SyntaxNode;

use crate::SourceLanguage;

/// Maximum source size accepted by the database artifact extractor.
pub const MAX_DATA_INPUT_BYTES: usize = 1_048_576;
/// Maximum number of source lines accepted by the database artifact extractor.
pub const MAX_DATA_SOURCE_LINES: u32 = 100_000;
/// Maximum number of structured items retained in one document.
pub const MAX_DATA_ITEMS: usize = 4_096;
/// Maximum nesting depth followed in SQL and framework declarations.
pub const MAX_DATA_DEPTH: usize = 32;

const MAX_IDENTIFIER_CHARS: usize = 512;
const MAX_LITERAL_SQL_BYTES: usize = 65_536;

/// Kind of source artifact represented by a [`DataDocument`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataArtifactKind {
    /// Ordered SQL migration.
    SqlMigration,
    /// Declarative SQL schema such as `schema.sql`.
    DeclarativeSqlSchema,
    /// Prisma schema.
    Prisma,
    /// Alembic Python migration.
    Alembic,
    /// `SQLAlchemy` declarative model source.
    SqlAlchemy,
    /// Diesel `table!` declaration source.
    Diesel,
    /// Standalone SQL query consumed by source code, including `SQLx` `query_file*!` macros.
    SqlQueryFile,
    /// `SQLx` project configuration (`sqlx.toml`).
    SqlxConfiguration,
    /// Focused source-language file containing literal SQL calls.
    LiteralQuerySource,
}

/// Data-access library whose API supplied the direct source evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataFramework {
    /// The `transact-rs/sqlx` Rust SQL toolkit.
    Sqlx,
    /// The `blackbeam/mysql_async` asynchronous `MySQL` client.
    MysqlAsync,
    /// The `PyMySQL` implementation of Python's DB-API.
    PyMysql,
    /// The psycopg and psycopg2 `PostgreSQL` adapters.
    Psycopg,
    /// The `SQLAlchemy` Python ORM.
    SqlAlchemy,
}

/// Kind of external data artifact referenced by source code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataArtifactReferenceKind {
    /// A `SQLx` `query_file*!` query file.
    QueryFile,
    /// A `SQLx` embedded or runtime migration directory.
    MigrationDirectory,
    /// The migration directory selected by `sqlx::migrate!()` with no explicit path.
    SqlxDefaultMigrationDirectory,
}

/// Relationship between source code and a database object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataAccessRole {
    /// Reads rows from a directly named table.
    Reader,
    /// Writes rows to a directly named table.
    Writer,
    /// Binds an ORM model to a table.
    ModelBinding,
    /// Declares a table or table member.
    Declaration,
}

/// SQL operation proven by a complete literal statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataOperation {
    /// `SELECT` or a read-only query expression.
    Select,
    /// `INSERT`.
    Insert,
    /// `UPDATE`.
    Update,
    /// `DELETE`.
    Delete,
}

/// Machine-readable extraction limitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataWarning {
    /// A query was computed or concatenated and therefore remains unlinked.
    DynamicQuery,
    /// A syntactic construct was outside the focused extraction subset.
    UnsupportedConstruct,
    /// A declaration referenced an object that could not be resolved locally.
    UnresolvedReference,
    /// A bounded parser stopped at its nesting or item budget.
    LimitExceeded,
    /// A literal looked like SQL but did not parse as a supported statement.
    SqlParseRecovery,
}

/// One-based source line constrained to the extraction line budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DataEvidenceLine(u32);

impl DataEvidenceLine {
    /// Creates a bounded one-based evidence line.
    ///
    /// # Errors
    ///
    /// Returns [`DataExtractionError::InvalidEvidenceLine`] for zero or a line beyond the
    /// configured source-line limit.
    pub fn new(line: u32) -> Result<Self, DataExtractionError> {
        if line == 0 || line > MAX_DATA_SOURCE_LINES {
            return Err(DataExtractionError::InvalidEvidenceLine { line });
        }
        Ok(Self(line))
    }

    /// Returns the one-based line number.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// One database column declaration without default literal contents.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DatabaseColumn {
    /// Database column name.
    pub name: String,
    /// Declared type, when statically available.
    pub data_type: Option<String>,
    /// Explicit nullability, when available.
    pub nullable: Option<bool>,
    /// Whether the declaration marks the column as a primary key.
    pub primary_key: bool,
    /// Whether the declaration marks the column as unique.
    pub unique: bool,
    /// Whether a default exists; its value is intentionally discarded.
    pub default_present: bool,
    /// Direct declaration line.
    pub evidence: DataEvidenceLine,
}

/// One database index declaration.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DatabaseIndex {
    /// Index name, when explicitly declared.
    pub name: Option<String>,
    /// Directly named indexed columns.
    pub columns: Vec<String>,
    /// Whether this is a unique index.
    pub unique: bool,
    /// Direct declaration line.
    pub evidence: DataEvidenceLine,
}

/// One database foreign-key declaration.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DatabaseForeignKey {
    /// Constraint name, when explicitly declared.
    pub name: Option<String>,
    /// Local columns participating in the key.
    pub columns: Vec<String>,
    /// Directly named referenced table.
    pub referenced_table: String,
    /// Directly named referenced columns.
    pub referenced_columns: Vec<String>,
    /// Direct declaration line.
    pub evidence: DataEvidenceLine,
}

/// Structured table definition collected from SQL or an ORM declaration.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DatabaseTable {
    /// Explicit database or catalog qualifier.
    pub database: Option<String>,
    /// Explicit schema qualifier.
    pub schema: Option<String>,
    /// Unqualified table name.
    pub name: String,
    /// Deterministically ordered column declarations.
    pub columns: Vec<DatabaseColumn>,
    /// Deterministically ordered index declarations.
    pub indexes: Vec<DatabaseIndex>,
    /// Deterministically ordered foreign keys.
    pub foreign_keys: Vec<DatabaseForeignKey>,
    /// Direct table declaration line.
    pub evidence: DataEvidenceLine,
}

/// Ordering and ancestry metadata for one migration artifact.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MigrationMetadata {
    /// Framework revision identifier, when explicitly declared.
    pub revision: Option<String>,
    /// Direct predecessor revision, when explicitly declared.
    pub down_revision: Option<String>,
    /// Leading numeric filename hint, when present.
    pub order_hint: Option<u64>,
    /// Whether an explicit downgrade entry point was observed.
    pub reversible: bool,
    /// Direct metadata or first-source line.
    pub evidence: DataEvidenceLine,
}

/// One directly evidenced declaration, model binding, or literal SQL access.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DataAccessObservation {
    /// Relationship to the database object.
    pub role: DataAccessRole,
    /// Directly named table; absent observations are never emitted for readers or writers.
    pub table: String,
    /// ORM model name when the table is resolved through an explicit model binding.
    pub model: Option<String>,
    /// Enclosing model, function, or method when statically available.
    pub owner: Option<String>,
    /// Literal SQL operation for readers and writers.
    pub operation: Option<DataOperation>,
    /// Direct source line.
    pub evidence: DataEvidenceLine,
}

/// One statically resolved source-to-data-artifact reference.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DataArtifactReference {
    /// Framework whose API declared the reference.
    pub framework: DataFramework,
    /// Whether the target is one query file or a migration directory.
    pub kind: DataArtifactReferenceKind,
    /// Normalized repository-relative target path.
    pub path: String,
    /// Enclosing function or method when statically available.
    pub owner: Option<String>,
    /// Direct source line containing the reference.
    pub evidence: DataEvidenceLine,
}

/// Owned, serialization-ready output for one database-related source artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataDocument {
    /// Repository-relative source path.
    pub source_path: String,
    /// Artifact syntax selected from the path, filename, or source-language API.
    pub artifact_kind: DataArtifactKind,
    /// Explicit database or catalog name, when unambiguous.
    pub database_name: Option<String>,
    /// Explicit schema name, when unambiguous.
    pub schema_name: Option<String>,
    /// Deterministically sorted and merged table definitions.
    pub tables: Vec<DatabaseTable>,
    /// Migration ancestry and ordering hints.
    pub migration: Option<MigrationMetadata>,
    /// Deterministically sorted direct observations.
    pub accesses: Vec<DataAccessObservation>,
    /// Data-access libraries directly recognized in this artifact.
    pub frameworks: Vec<DataFramework>,
    /// Statically resolved references to query files or migration directories.
    pub references: Vec<DataArtifactReference>,
    /// Enclosing model or callable names.
    pub owners: Vec<String>,
    /// Deterministically sorted extraction limitations.
    pub warnings: Vec<DataWarning>,
    /// Whether dynamic, unsupported, malformed, or budget-limited input was observed.
    pub incomplete: bool,
}

/// Failure returned by bounded database artifact extraction.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DataExtractionError {
    /// Input exceeds the fixed byte budget.
    #[error("database artifact input exceeds the byte limit ({actual} > {maximum})")]
    InputTooLarge {
        /// Observed bytes.
        actual: usize,
        /// Maximum accepted bytes.
        maximum: usize,
    },
    /// Input exceeds the fixed source-line budget.
    #[error("database artifact input exceeds the line limit ({actual} > {maximum})")]
    TooManyLines {
        /// Observed source lines.
        actual: u32,
        /// Maximum accepted source lines.
        maximum: u32,
    },
    /// The path does not select a supported database artifact.
    #[error("unsupported database artifact filename or extension")]
    UnsupportedArtifact,
    /// SQL parsing failed without exposing parser input or literal values.
    #[error("SQL artifact could not be parsed")]
    SqlParseFailed,
    /// A bounded collection exceeded its item budget.
    #[error("database artifact exceeds the structured item limit")]
    ItemLimitExceeded,
    /// Evidence must fit the one-based line budget.
    #[error("evidence line {line} is outside the supported range")]
    InvalidEvidenceLine {
        /// Rejected line number.
        line: u32,
    },
}

/// Extracts a supported SQL, Prisma, Alembic, `SQLAlchemy`, or Diesel artifact.
///
/// Dispatch is path-only: `.sql`, `.prisma`, Alembic migration paths, conventional `SQLAlchemy`
/// model filenames, and Diesel's exact `schema.rs` filename. Source contents never change the
/// selected parser.
///
/// # Errors
///
/// Returns [`DataExtractionError`] for unsupported paths. Oversized inputs and structured output
/// degrade to bounded incomplete documents so one large dump cannot abort a workspace scan.
/// Over-budget inputs and unsupported SQL dialect constructs produce an incomplete source-free
/// document rather than aborting the workspace scan.
pub fn extract_data_artifact(
    source_path: &str,
    input: &str,
) -> Result<DataDocument, DataExtractionError> {
    let kind = artifact_kind(source_path, input).ok_or(DataExtractionError::UnsupportedArtifact)?;
    if validate_input(input).is_err() {
        let mut document = empty_document(source_path, kind);
        mark_incomplete(&mut document, DataWarning::LimitExceeded);
        return Ok(document);
    }
    let mut document = match kind {
        DataArtifactKind::SqlMigration
        | DataArtifactKind::DeclarativeSqlSchema
        | DataArtifactKind::SqlQueryFile => parse_sql_artifact(source_path, input, kind),
        DataArtifactKind::SqlxConfiguration => parse_sqlx_configuration(source_path, input),
        DataArtifactKind::Prisma => parse_prisma(source_path, input),
        DataArtifactKind::Alembic => parse_alembic(source_path, input),
        DataArtifactKind::SqlAlchemy => parse_sqlalchemy(source_path, input),
        DataArtifactKind::Diesel => parse_diesel(source_path, input),
        DataArtifactKind::LiteralQuerySource => {
            return Err(DataExtractionError::UnsupportedArtifact);
        }
    };
    finish_document(&mut document);
    Ok(document)
}

/// Extracts focused literal SQL calls from a supported source language.
///
/// Dynamic, interpolated, concatenated, and unparseable query expressions are marked incomplete
/// and remain unlinked. The function intentionally returns a document rather than an error so it
/// can degrade safely inside focused source extractors.
#[must_use]
pub fn parse_literal_sql_source(
    language: SourceLanguage,
    source_path: &str,
    input: &str,
) -> DataDocument {
    parse_literal_sql_source_at_root(
        language,
        source_path,
        &inferred_crate_root(source_path),
        input,
    )
}

/// Extracts literal SQL calls using an explicit repository-relative Cargo crate root.
///
/// The crate root is used to resolve `SQLx` `query_file*!` and `migrate!` paths, which `SQLx`
/// defines relative to the directory containing the crate's `Cargo.toml`.
#[must_use]
pub fn parse_literal_sql_source_at_root(
    language: SourceLanguage,
    source_path: &str,
    crate_root: &str,
    input: &str,
) -> DataDocument {
    let mut document = empty_document(source_path, DataArtifactKind::LiteralQuerySource);
    if validate_input(input).is_err() {
        mark_incomplete(&mut document, DataWarning::LimitExceeded);
        return document;
    }

    if language == SourceLanguage::Rust {
        extract_rust_database_source(input, crate_root, &mut document);
    } else if language == SourceLanguage::Python {
        if input.contains("pymysql") {
            document.frameworks.push(DataFramework::PyMysql);
        }
        if input.contains("psycopg") {
            document.frameworks.push(DataFramework::Psycopg);
        }
        if input.contains("sqlalchemy") || input.contains(".query(") {
            extract_sqlalchemy_accesses(input, &mut document);
        }
    }
    let literals = quoted_literals(input, language);
    if literals.len() >= MAX_DATA_ITEMS {
        mark_incomplete(&mut document, DataWarning::LimitExceeded);
    }
    for literal in literals {
        if literal.value.len() > MAX_LITERAL_SQL_BYTES || !has_query_context(input, &literal) {
            continue;
        }
        let query = if language == SourceLanguage::Python
            && (literal.interpolated || python_format_call_after(input, &literal))
        {
            mark_incomplete(&mut document, DataWarning::DynamicQuery);
            sanitize_python_f_string(&literal.value)
        } else if literal.dynamic {
            mark_incomplete(&mut document, DataWarning::DynamicQuery);
            None
        } else {
            Some(literal.value.clone())
        };
        let Some(query) = query else {
            continue;
        };
        let first_keyword = first_sql_keyword(&query);
        if !matches!(
            first_keyword.as_deref(),
            Some("SELECT" | "WITH" | "INSERT" | "UPDATE" | "DELETE")
        ) {
            continue;
        }
        let Some(statements) = parse_sql_with_supported_dialects(&query) else {
            mark_incomplete(&mut document, DataWarning::SqlParseRecovery);
            continue;
        };
        let owner = owner_at_line(language, input, literal.line);
        if let Some(owner) = owner.as_ref() {
            document.owners.push(owner.clone());
        }
        for statement in &statements {
            append_statement_accesses(
                statement,
                evidence(literal.line),
                owner.as_deref(),
                &mut document,
                0,
            );
        }
        document
            .accesses
            .retain(|access| !access.table.contains("__csg_dynamic_value__"));
    }

    if (language != SourceLanguage::Rust || document.frameworks.is_empty())
        && source_has_dynamic_query(input, language)
    {
        mark_incomplete(&mut document, DataWarning::DynamicQuery);
    }
    finish_document(&mut document);
    document
}

fn validate_input(input: &str) -> Result<(), DataExtractionError> {
    if input.len() > MAX_DATA_INPUT_BYTES {
        return Err(DataExtractionError::InputTooLarge {
            actual: input.len(),
            maximum: MAX_DATA_INPUT_BYTES,
        });
    }
    let lines = input.lines().count().max(1);
    let lines = u32::try_from(lines).unwrap_or(u32::MAX);
    if lines > MAX_DATA_SOURCE_LINES {
        return Err(DataExtractionError::TooManyLines {
            actual: lines,
            maximum: MAX_DATA_SOURCE_LINES,
        });
    }
    Ok(())
}

fn artifact_kind(source_path: &str, input: &str) -> Option<DataArtifactKind> {
    let path = Path::new(source_path);
    let filename = path.file_name()?.to_str()?.to_ascii_lowercase();
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let normalized = source_path.replace('\\', "/").to_ascii_lowercase();
    match (filename.as_str(), extension.as_str()) {
        ("sqlx.toml", "toml") => Some(DataArtifactKind::SqlxConfiguration),
        ("schema.sql" | "structure.sql" | "init.sql", "sql") => {
            Some(DataArtifactKind::DeclarativeSqlSchema)
        }
        (_, "sql") if sql_migration_path(&normalized, &filename) => {
            Some(DataArtifactKind::SqlMigration)
        }
        (_, "sql") if sql_contains_only_queries(input) => Some(DataArtifactKind::SqlQueryFile),
        (_, "sql") => Some(DataArtifactKind::DeclarativeSqlSchema),
        (_, "prisma") => Some(DataArtifactKind::Prisma),
        ("schema.rs", "rs") => Some(DataArtifactKind::Diesel),
        (_, "py")
            if normalized.contains("/alembic/")
                || normalized.contains("/versions/")
                || normalized.contains("/migrations/") =>
        {
            Some(DataArtifactKind::Alembic)
        }
        ("model.py" | "models.py" | "entities.py", "py") => Some(DataArtifactKind::SqlAlchemy),
        _ => None,
    }
}

fn empty_document(source_path: &str, artifact_kind: DataArtifactKind) -> DataDocument {
    DataDocument {
        source_path: source_path.to_owned(),
        artifact_kind,
        database_name: None,
        schema_name: None,
        tables: Vec::new(),
        migration: None,
        accesses: Vec::new(),
        frameworks: Vec::new(),
        references: Vec::new(),
        owners: Vec::new(),
        warnings: Vec::new(),
        incomplete: false,
    }
}

fn parse_sql_artifact(source_path: &str, input: &str, kind: DataArtifactKind) -> DataDocument {
    let mut document = empty_document(source_path, kind);
    if let Some(statements) = parse_sql_with_supported_dialects(input) {
        let starts = sql_statement_lines(input);
        for (index, statement) in statements.iter().enumerate() {
            append_sql_artifact_statement(
                statement,
                starts.get(index).copied().unwrap_or(1),
                kind,
                &mut document,
            );
        }
    } else {
        mark_incomplete(&mut document, DataWarning::SqlParseRecovery);
        for chunk in sql_statement_chunks(input) {
            if !sql_chunk_is_relevant(chunk.text, kind) {
                continue;
            }
            let Some(statements) = parse_sql_with_supported_dialects(chunk.text) else {
                continue;
            };
            for statement in &statements {
                append_sql_artifact_statement(statement, chunk.line, kind, &mut document);
            }
        }
    }

    if kind == DataArtifactKind::SqlMigration {
        document.migration = Some(sql_migration_metadata(source_path, input));
    }
    document
}

fn parse_sql_with_supported_dialects(input: &str) -> Option<Vec<Statement>> {
    Parser::parse_sql(&GenericDialect {}, input)
        .or_else(|_| Parser::parse_sql(&MySqlDialect {}, input))
        .ok()
}

fn append_sql_artifact_statement(
    statement: &Statement,
    line: u32,
    kind: DataArtifactKind,
    document: &mut DataDocument,
) {
    if kind == DataArtifactKind::SqlQueryFile {
        append_statement_accesses(statement, evidence(line), None, document, 0);
    } else if !append_schema_statement(statement, evidence(line), document) {
        mark_incomplete(document, DataWarning::UnsupportedConstruct);
    }
}

fn sql_chunk_is_relevant(input: &str, kind: DataArtifactKind) -> bool {
    let Some(keyword) = first_sql_keyword(input) else {
        return false;
    };
    if kind == DataArtifactKind::SqlQueryFile {
        matches!(
            keyword.as_str(),
            "SELECT" | "WITH" | "INSERT" | "UPDATE" | "DELETE"
        )
    } else {
        matches!(keyword.as_str(), "CREATE" | "ALTER")
    }
}

fn parse_sqlx_configuration(source_path: &str, input: &str) -> DataDocument {
    let mut document = empty_document(source_path, DataArtifactKind::SqlxConfiguration);
    document.frameworks.push(DataFramework::Sqlx);
    let crate_root = Path::new(source_path)
        .parent()
        .and_then(Path::to_str)
        .unwrap_or_default()
        .replace('\\', "/");
    let mut section = "";

    for (index, source_line) in input.lines().enumerate() {
        let line = strip_toml_comment(source_line).trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].trim();
            continue;
        }
        if section != "migrate" {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "migrations-dir" {
            continue;
        }
        let Some(value) = toml_string(value.trim()) else {
            mark_incomplete(&mut document, DataWarning::UnresolvedReference);
            continue;
        };
        push_sqlx_reference(
            DataArtifactReferenceKind::MigrationDirectory,
            value,
            evidence(u32::try_from(index + 1).unwrap_or(u32::MAX)),
            &crate_root,
            None,
            &mut document,
        );
    }
    document
}

fn strip_toml_comment(line: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if quote == Some('"') => escaped = true,
            '\'' | '"' if quote == Some(character) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(character),
            '#' if quote.is_none() => return &line[..index],
            _ => {}
        }
    }
    line
}

fn toml_string(value: &str) -> Option<&str> {
    let quote = value.chars().next()?;
    if !matches!(quote, '\'' | '"') || !value.ends_with(quote) || value.len() < 2 {
        return None;
    }
    Some(&value[1..value.len() - 1])
}

fn append_schema_statement(
    statement: &Statement,
    line: DataEvidenceLine,
    document: &mut DataDocument,
) -> bool {
    match statement {
        Statement::CreateTable(create) => {
            let mut table = table_from_name(&create.name, line);
            table.columns = create
                .columns
                .iter()
                .map(|column| sql_column(column, line))
                .collect();
            apply_table_constraints(&mut table, &create.constraints, line);
            push_declaration_access(document, &table, None);
            document.tables.push(table);
        }
        Statement::CreateIndex(index) => {
            let table = ensure_table(document, &index.table_name, line);
            table.indexes.push(DatabaseIndex {
                name: index.name.as_ref().map(object_name),
                columns: index.columns.iter().filter_map(index_column_name).collect(),
                unique: index.unique,
                evidence: line,
            });
        }
        Statement::AlterTable(alter) => {
            let table = ensure_table(document, &alter.name, line);
            for operation in &alter.operations {
                apply_alter_operation(table, operation, line);
            }
        }
        _ => return false,
    }
    true
}

fn sql_column(column: &ColumnDef, line: DataEvidenceLine) -> DatabaseColumn {
    let mut nullable = None;
    let mut primary_key = false;
    let mut unique = false;
    let mut default_present = false;
    for option in &column.options {
        match &option.option {
            ColumnOption::Null => nullable = Some(true),
            ColumnOption::NotNull => nullable = Some(false),
            ColumnOption::PrimaryKey(_) => primary_key = true,
            ColumnOption::Unique(_) => unique = true,
            ColumnOption::Default(_)
            | ColumnOption::Materialized(_)
            | ColumnOption::Generated { .. }
            | ColumnOption::Identity(_) => default_present = true,
            _ => {}
        }
    }
    DatabaseColumn {
        name: bounded_identifier(&column.name.value),
        data_type: Some(safe_data_type(&column.data_type.to_string())),
        nullable,
        primary_key,
        unique,
        default_present,
        evidence: line,
    }
}

fn apply_table_constraints(
    table: &mut DatabaseTable,
    constraints: &[TableConstraint],
    line: DataEvidenceLine,
) {
    for constraint in constraints {
        match constraint {
            TableConstraint::PrimaryKey(primary) => {
                let columns = primary
                    .columns
                    .iter()
                    .filter_map(index_column_name)
                    .collect::<Vec<_>>();
                mark_columns(table, &columns, true, false);
            }
            TableConstraint::Unique(unique) => {
                let columns = unique
                    .columns
                    .iter()
                    .filter_map(index_column_name)
                    .collect::<Vec<_>>();
                mark_columns(table, &columns, false, true);
                table.indexes.push(DatabaseIndex {
                    name: unique
                        .name
                        .as_ref()
                        .or(unique.index_name.as_ref())
                        .map(|name| bounded_identifier(&name.value)),
                    columns,
                    unique: true,
                    evidence: line,
                });
            }
            TableConstraint::ForeignKey(foreign) => {
                table.foreign_keys.push(DatabaseForeignKey {
                    name: foreign
                        .name
                        .as_ref()
                        .map(|name| bounded_identifier(&name.value)),
                    columns: foreign
                        .columns
                        .iter()
                        .map(|name| bounded_identifier(&name.value))
                        .collect(),
                    referenced_table: object_name(&foreign.foreign_table),
                    referenced_columns: foreign
                        .referred_columns
                        .iter()
                        .map(|name| bounded_identifier(&name.value))
                        .collect(),
                    evidence: line,
                });
            }
            TableConstraint::Index(index) => {
                table.indexes.push(DatabaseIndex {
                    name: index
                        .name
                        .as_ref()
                        .map(|name| bounded_identifier(&name.value)),
                    columns: index.columns.iter().filter_map(index_column_name).collect(),
                    unique: false,
                    evidence: line,
                });
            }
            _ => {}
        }
    }
}

fn apply_alter_operation(
    table: &mut DatabaseTable,
    operation: &AlterTableOperation,
    line: DataEvidenceLine,
) {
    match operation {
        AlterTableOperation::AddColumn { column_def, .. } => {
            table.columns.push(sql_column(column_def, line));
        }
        AlterTableOperation::AddConstraint { constraint, .. } => {
            apply_table_constraints(table, std::slice::from_ref(constraint), line);
        }
        _ => {}
    }
}

fn parse_prisma(source_path: &str, input: &str) -> DataDocument {
    let mut document = empty_document(source_path, DataArtifactKind::Prisma);
    for block in named_blocks(input, "model") {
        let model = bounded_identifier(block.name);
        let mapped = find_call_literal(block.body, "@@map").unwrap_or_else(|| model.clone());
        let schema = find_call_literal(block.body, "@@schema");
        let mut table = table_from_qualified_text(&mapped, evidence(block.line));
        table.schema = schema.map(|value| bounded_identifier(&value));

        for (offset, raw_line) in block.body.lines().enumerate() {
            let line = add_line(block.line, offset);
            let trimmed = raw_line.trim();
            if trimmed.is_empty()
                || trimmed.starts_with("//")
                || trimmed.starts_with("@@")
                || trimmed.starts_with('}')
            {
                continue;
            }
            let fields = trimmed.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 2 {
                continue;
            }
            let field_name =
                find_call_literal(trimmed, "@map").unwrap_or_else(|| bounded_identifier(fields[0]));
            let field_type = fields[1];
            if field_type.starts_with(char::is_uppercase) && trimmed.contains("@relation") {
                if let Some(foreign_key) = prisma_foreign_key(trimmed, field_type, evidence(line)) {
                    table.foreign_keys.push(foreign_key);
                }
                continue;
            }
            table.columns.push(DatabaseColumn {
                name: bounded_identifier(&field_name),
                data_type: Some(bounded_identifier(
                    field_type.trim_end_matches('?').trim_end_matches("[]"),
                )),
                nullable: Some(field_type.ends_with('?')),
                primary_key: trimmed.contains("@id"),
                unique: trimmed.contains("@unique"),
                default_present: trimmed.contains("@default("),
                evidence: evidence(line),
            });
        }
        document.owners.push(model.clone());
        document.accesses.push(DataAccessObservation {
            role: DataAccessRole::ModelBinding,
            table: qualified_table_name(&table),
            model: None,
            owner: Some(model),
            operation: None,
            evidence: table.evidence,
        });
        document.tables.push(table);
    }
    document
}

fn prisma_foreign_key(
    line: &str,
    referenced_model: &str,
    evidence: DataEvidenceLine,
) -> Option<DatabaseForeignKey> {
    let fields = bracket_values_after(line, "fields:")?;
    let references = bracket_values_after(line, "references:").unwrap_or_default();
    Some(DatabaseForeignKey {
        name: None,
        columns: fields,
        referenced_table: bounded_identifier(referenced_model.trim_end_matches('?')),
        referenced_columns: references,
        evidence,
    })
}

fn parse_alembic(source_path: &str, input: &str) -> DataDocument {
    let mut document = empty_document(source_path, DataArtifactKind::Alembic);
    document.migration = Some(MigrationMetadata {
        revision: assignment_literal(input, "revision"),
        down_revision: assignment_literal(input, "down_revision"),
        order_hint: filename_order_hint(source_path),
        reversible: input
            .lines()
            .any(|line| line.trim_start().starts_with("def downgrade(")),
        evidence: evidence(assignment_line(input, "revision").unwrap_or(1)),
    });

    for call in collect_calls(input, "op.create_table(") {
        let Some(name) = first_quoted(&call.body) else {
            mark_incomplete(&mut document, DataWarning::DynamicQuery);
            continue;
        };
        let mut table = table_from_qualified_text(&name, evidence(call.line));
        for column_call in collect_calls(&call.body, "sa.Column(") {
            if let Some(column) =
                python_column(&column_call.body, add_evidence(call.line, column_call.line))
            {
                table.columns.push(column);
            }
        }
        for foreign_call in collect_calls(&call.body, "sa.ForeignKeyConstraint(") {
            if let Some(foreign) = python_foreign_key(
                &foreign_call.body,
                add_evidence(call.line, foreign_call.line),
            ) {
                table.foreign_keys.push(foreign);
            }
        }
        push_declaration_access(&mut document, &table, Some("upgrade"));
        document.tables.push(table);
    }

    for call in collect_calls(input, "op.add_column(") {
        let quoted = quoted_values(&call.body);
        let Some(table_name) = quoted.first() else {
            mark_incomplete(&mut document, DataWarning::DynamicQuery);
            continue;
        };
        let table = ensure_text_table(&mut document, table_name, evidence(call.line));
        if let Some(column_call) = collect_calls(&call.body, "sa.Column(").first()
            && let Some(column) = python_column(&column_call.body, evidence(call.line))
        {
            table.columns.push(column);
        }
    }

    for call in collect_calls(input, "op.create_index(") {
        let quoted = quoted_values(&call.body);
        if quoted.len() < 2 {
            mark_incomplete(&mut document, DataWarning::DynamicQuery);
            continue;
        }
        let columns = bracket_values(&call.body).unwrap_or_default();
        let unique = call.body.split_whitespace().any(|part| {
            part.trim_matches(|character: char| character == ',' || character == ')')
                == "unique=True"
        });
        let table = ensure_text_table(&mut document, &quoted[1], evidence(call.line));
        table.indexes.push(DatabaseIndex {
            name: Some(bounded_identifier(&quoted[0])),
            columns,
            unique,
            evidence: evidence(call.line),
        });
    }
    document
}

fn parse_sqlalchemy(source_path: &str, input: &str) -> DataDocument {
    let mut document = empty_document(source_path, DataArtifactKind::SqlAlchemy);
    for class in python_classes(input) {
        let Some(table_name) = assignment_literal(class.body, "__tablename__") else {
            continue;
        };
        let mut table = table_from_qualified_text(&table_name, evidence(class.line));
        table.schema = table_argument_literal(class.body, "schema");
        for (offset, line) in class.body.lines().enumerate() {
            let Some(column_position) = line.find("Column(") else {
                continue;
            };
            let Some((attribute, _)) = line[..column_position].split_once('=') else {
                continue;
            };
            let open = column_position + "Column(".len();
            let body = balanced_slice(&line[open..]).unwrap_or(&line[open..]);
            if let Some(column) = python_column_with_fallback(
                body,
                evidence(add_line(class.line, offset)),
                Some(attribute.trim()),
            ) {
                if let Some(reference) = call_literal(body, "ForeignKey") {
                    let (foreign_table, foreign_column) = split_reference(&reference);
                    table.foreign_keys.push(DatabaseForeignKey {
                        name: None,
                        columns: vec![column.name.clone()],
                        referenced_table: foreign_table,
                        referenced_columns: foreign_column.into_iter().collect(),
                        evidence: column.evidence,
                    });
                }
                table.columns.push(column);
            }
        }
        document.owners.push(class.name.to_owned());
        document.accesses.push(DataAccessObservation {
            role: DataAccessRole::ModelBinding,
            table: qualified_table_name(&table),
            model: None,
            owner: Some(class.name.to_owned()),
            operation: None,
            evidence: table.evidence,
        });
        document.tables.push(table);
    }
    document
}

fn parse_diesel(source_path: &str, input: &str) -> DataDocument {
    let mut document = empty_document(source_path, DataArtifactKind::Diesel);
    for block in macro_blocks(input, "table!") {
        let header = block
            .body
            .lines()
            .find(|line| line.contains('('))
            .unwrap_or_default()
            .trim();
        let name = header.split('(').next().unwrap_or_default().trim();
        if name.is_empty() {
            mark_incomplete(&mut document, DataWarning::UnsupportedConstruct);
            continue;
        }
        let mut table = table_from_qualified_text(name, evidence(block.line));
        let primary_keys = header
            .split_once('(')
            .and_then(|(_, rest)| rest.split_once(')'))
            .map_or_else(Vec::new, |(keys, _)| comma_identifiers(keys));
        for (offset, raw_line) in block.body.lines().enumerate() {
            let Some((name, data_type)) = raw_line.split_once("->") else {
                continue;
            };
            let name = bounded_identifier(name.trim());
            if name.is_empty() {
                continue;
            }
            table.columns.push(DatabaseColumn {
                primary_key: primary_keys.contains(&name),
                name,
                data_type: Some(bounded_identifier(
                    data_type.trim().trim_end_matches(',').trim(),
                )),
                nullable: Some(data_type.contains("Nullable<")),
                unique: false,
                default_present: false,
                evidence: evidence(add_line(block.line, offset)),
            });
        }
        push_declaration_access(&mut document, &table, None);
        document.tables.push(table);
    }
    document
}

fn append_statement_accesses(
    statement: &Statement,
    line: DataEvidenceLine,
    owner: Option<&str>,
    document: &mut DataDocument,
    depth: usize,
) {
    if depth >= MAX_DATA_DEPTH {
        mark_incomplete(document, DataWarning::LimitExceeded);
        return;
    }
    let mut observations = BTreeSet::new();
    match statement {
        Statement::Query(query) => {
            let mut tables = BTreeSet::new();
            collect_query_tables(query, &mut tables, depth + 1);
            for table in tables {
                observations.insert((DataAccessRole::Reader, DataOperation::Select, table));
            }
        }
        Statement::Insert(insert) => {
            if let TableObject::TableName(name) = &insert.table {
                observations.insert((
                    DataAccessRole::Writer,
                    DataOperation::Insert,
                    object_name(name),
                ));
            }
            if let Some(query) = insert.source.as_deref() {
                let mut tables = BTreeSet::new();
                collect_query_tables(query, &mut tables, depth + 1);
                for table in tables {
                    observations.insert((DataAccessRole::Reader, DataOperation::Select, table));
                }
            }
        }
        Statement::Update(update) => {
            let mut tables = BTreeSet::new();
            collect_table_with_joins(&update.table, &mut tables, depth + 1);
            if let Some(table) = tables.into_iter().next() {
                observations.insert((DataAccessRole::Writer, DataOperation::Update, table));
            }
        }
        Statement::Delete(delete) => {
            let tables = match &delete.from {
                FromTable::WithFromKeyword(tables) | FromTable::WithoutKeyword(tables) => tables,
            };
            let mut names = BTreeSet::new();
            for table in tables {
                collect_table_with_joins(table, &mut names, depth + 1);
            }
            for table in names {
                observations.insert((DataAccessRole::Writer, DataOperation::Delete, table));
            }
        }
        _ => mark_incomplete(document, DataWarning::UnsupportedConstruct),
    }
    document
        .accesses
        .extend(
            observations
                .into_iter()
                .map(|(role, operation, table)| DataAccessObservation {
                    role,
                    table,
                    model: None,
                    owner: owner.map(ToOwned::to_owned),
                    operation: Some(operation),
                    evidence: line,
                }),
        );
}

fn collect_query_tables(query: &Query, output: &mut BTreeSet<String>, depth: usize) {
    if depth >= MAX_DATA_DEPTH {
        return;
    }
    if let Some(with) = &query.with {
        for cte in &with.cte_tables {
            collect_query_tables(&cte.query, output, depth + 1);
        }
    }
    collect_set_expr_tables(&query.body, output, depth + 1);
}

fn collect_set_expr_tables(expression: &SetExpr, output: &mut BTreeSet<String>, depth: usize) {
    if depth >= MAX_DATA_DEPTH {
        return;
    }
    match expression {
        SetExpr::Select(select) => {
            for table in &select.from {
                collect_table_with_joins(table, output, depth + 1);
            }
        }
        SetExpr::Query(query) => collect_query_tables(query, output, depth + 1),
        SetExpr::SetOperation { left, right, .. } => {
            collect_set_expr_tables(left, output, depth + 1);
            collect_set_expr_tables(right, output, depth + 1);
        }
        SetExpr::Insert(statement) | SetExpr::Update(statement) | SetExpr::Delete(statement) => {
            let mut ignored = empty_document("", DataArtifactKind::LiteralQuerySource);
            append_statement_accesses(statement, evidence(1), None, &mut ignored, depth + 1);
            output.extend(ignored.accesses.into_iter().map(|access| access.table));
        }
        SetExpr::Table(table) => {
            if let Some(table_name) = table.table_name.as_deref() {
                output.insert(bounded_identifier(table_name));
            }
        }
        SetExpr::Values(_) | SetExpr::Merge(_) => {}
    }
}

fn collect_table_with_joins(table: &TableWithJoins, output: &mut BTreeSet<String>, depth: usize) {
    collect_table_factor(&table.relation, output, depth + 1);
    for join in &table.joins {
        collect_table_factor(&join.relation, output, depth + 1);
    }
}

fn collect_table_factor(factor: &TableFactor, output: &mut BTreeSet<String>, depth: usize) {
    if depth >= MAX_DATA_DEPTH {
        return;
    }
    match factor {
        TableFactor::Table { name, args, .. } if args.is_none() => {
            output.insert(object_name(name));
        }
        TableFactor::Derived { subquery, .. } => collect_query_tables(subquery, output, depth + 1),
        _ => {}
    }
}

fn table_from_name(name: &ObjectName, line: DataEvidenceLine) -> DatabaseTable {
    table_from_qualified_text(&object_name(name), line)
}

fn table_from_qualified_text(name: &str, line: DataEvidenceLine) -> DatabaseTable {
    let parts = name
        .split('.')
        .map(clean_identifier)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let table_name = parts.last().cloned().unwrap_or_default();
    let schema = (parts.len() >= 2).then(|| parts[parts.len() - 2].clone());
    let database = (parts.len() >= 3).then(|| parts[parts.len() - 3].clone());
    DatabaseTable {
        database,
        schema,
        name: table_name,
        columns: Vec::new(),
        indexes: Vec::new(),
        foreign_keys: Vec::new(),
        evidence: line,
    }
}

fn ensure_table<'a>(
    document: &'a mut DataDocument,
    name: &ObjectName,
    line: DataEvidenceLine,
) -> &'a mut DatabaseTable {
    ensure_text_table(document, &object_name(name), line)
}

fn ensure_text_table<'a>(
    document: &'a mut DataDocument,
    name: &str,
    line: DataEvidenceLine,
) -> &'a mut DatabaseTable {
    let candidate = table_from_qualified_text(name, line);
    let key = table_key(&candidate);
    if let Some(index) = document
        .tables
        .iter()
        .position(|table| table_key(table) == key)
    {
        return &mut document.tables[index];
    }
    document.tables.push(candidate);
    let index = document.tables.len() - 1;
    &mut document.tables[index]
}

fn push_declaration_access(
    document: &mut DataDocument,
    table: &DatabaseTable,
    owner: Option<&str>,
) {
    document.accesses.push(DataAccessObservation {
        role: DataAccessRole::Declaration,
        table: qualified_table_name(table),
        model: None,
        owner: owner.map(ToOwned::to_owned),
        operation: None,
        evidence: table.evidence,
    });
}

fn mark_columns(table: &mut DatabaseTable, names: &[String], primary: bool, unique: bool) {
    for column in &mut table.columns {
        if names
            .iter()
            .any(|name| clean_identifier(name) == column.name)
        {
            column.primary_key |= primary;
            column.unique |= unique;
        }
    }
}

fn object_name(name: &ObjectName) -> String {
    bounded_identifier(&name.to_string())
}

fn index_column_name(column: &IndexColumn) -> Option<String> {
    match &column.column.expr {
        Expr::Identifier(identifier) => Some(bounded_identifier(&identifier.value)),
        Expr::CompoundIdentifier(identifiers) => Some(
            identifiers
                .iter()
                .map(|identifier| bounded_identifier(&identifier.value))
                .collect::<Vec<_>>()
                .join("."),
        ),
        _ => None,
    }
}

fn safe_data_type(value: &str) -> String {
    if value.contains(['\'', '"']) {
        value.split_once('(').map_or_else(
            || bounded_identifier(value),
            |(name, _)| bounded_identifier(name),
        )
    } else {
        bounded_identifier(value)
    }
}

fn qualified_table_name(table: &DatabaseTable) -> String {
    [
        table.database.as_deref(),
        table.schema.as_deref(),
        Some(&table.name),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(".")
}

fn table_key(table: &DatabaseTable) -> (Option<&str>, Option<&str>, &str) {
    (
        table.database.as_deref(),
        table.schema.as_deref(),
        &table.name,
    )
}

fn finish_document(document: &mut DataDocument) {
    let mut tables = BTreeMap::<(Option<String>, Option<String>, String), DatabaseTable>::new();
    for table in std::mem::take(&mut document.tables) {
        let key = (
            table.database.clone(),
            table.schema.clone(),
            table.name.clone(),
        );
        if let Some(existing) = tables.get_mut(&key) {
            existing.columns.extend(table.columns);
            existing.indexes.extend(table.indexes);
            existing.foreign_keys.extend(table.foreign_keys);
            existing.evidence = existing.evidence.min(table.evidence);
        } else {
            tables.insert(key, table);
        }
    }
    document.tables = tables.into_values().collect();
    for table in &mut document.tables {
        table.columns.sort();
        table.columns.dedup();
        table.indexes.sort();
        table.indexes.dedup();
        table.foreign_keys.sort();
        table.foreign_keys.dedup();
    }
    document.accesses.sort();
    document.accesses.dedup();
    document.frameworks.sort();
    document.frameworks.dedup();
    document.references.sort();
    document.references.dedup();
    document.owners.sort();
    document.owners.dedup();
    document.warnings.sort();
    document.warnings.dedup();
    document.incomplete |= !document.warnings.is_empty();

    if structured_item_count(document) > MAX_DATA_ITEMS {
        truncate_structured_items(document);
        mark_incomplete(document, DataWarning::LimitExceeded);
        document.warnings.sort();
        document.warnings.dedup();
    }

    let databases = document
        .tables
        .iter()
        .filter_map(|table| table.database.clone())
        .collect::<BTreeSet<_>>();
    let schemas = document
        .tables
        .iter()
        .filter_map(|table| table.schema.clone())
        .collect::<BTreeSet<_>>();
    document.database_name = single_value(databases);
    document.schema_name = single_value(schemas);
}

fn structured_item_count(document: &DataDocument) -> usize {
    document.tables.len()
        + document.accesses.len()
        + document.frameworks.len()
        + document.references.len()
        + document.owners.len()
        + document
            .tables
            .iter()
            .map(|table| table.columns.len() + table.indexes.len() + table.foreign_keys.len())
            .sum::<usize>()
}

fn truncate_structured_items(document: &mut DataDocument) {
    let mut remaining = MAX_DATA_ITEMS;

    document.tables.truncate(remaining);
    remaining = remaining.saturating_sub(document.tables.len());
    document.accesses.truncate(remaining);
    remaining = remaining.saturating_sub(document.accesses.len());
    document.frameworks.truncate(remaining);
    remaining = remaining.saturating_sub(document.frameworks.len());
    document.references.truncate(remaining);
    remaining = remaining.saturating_sub(document.references.len());
    document.owners.truncate(remaining);
    remaining = remaining.saturating_sub(document.owners.len());

    for table in &mut document.tables {
        table.columns.truncate(remaining);
        remaining = remaining.saturating_sub(table.columns.len());
        table.indexes.truncate(remaining);
        remaining = remaining.saturating_sub(table.indexes.len());
        table.foreign_keys.truncate(remaining);
        remaining = remaining.saturating_sub(table.foreign_keys.len());
    }
}

fn single_value(values: BTreeSet<String>) -> Option<String> {
    (values.len() == 1)
        .then(|| values.into_iter().next())
        .flatten()
}

fn mark_incomplete(document: &mut DataDocument, warning: DataWarning) {
    document.incomplete = true;
    document.warnings.push(warning);
}

fn evidence(line: u32) -> DataEvidenceLine {
    DataEvidenceLine(line.clamp(1, MAX_DATA_SOURCE_LINES))
}

fn add_evidence(base: u32, relative: u32) -> DataEvidenceLine {
    evidence(base.saturating_add(relative.saturating_sub(1)))
}

fn add_line(base: u32, offset: usize) -> u32 {
    base.saturating_add(u32::try_from(offset).unwrap_or(u32::MAX))
        .min(MAX_DATA_SOURCE_LINES)
}

fn bounded_identifier(value: &str) -> String {
    value.trim().chars().take(MAX_IDENTIFIER_CHARS).collect()
}

fn clean_identifier(value: &str) -> String {
    bounded_identifier(
        value.trim_matches(|character| matches!(character, '"' | '\'' | '`' | '[' | ']')),
    )
}

fn filename_order_hint(source_path: &str) -> Option<u64> {
    Path::new(source_path)
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| {
            let digits = name
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>();
            (!digits.is_empty()).then_some(digits)
        })
        .and_then(|digits| digits.parse().ok())
}

fn sql_migration_path(normalized_path: &str, filename: &str) -> bool {
    normalized_path
        .split('/')
        .any(|component| component == "migrations")
        || filename.ends_with(".up.sql")
        || filename.ends_with(".down.sql")
}

fn sql_contains_only_queries(input: &str) -> bool {
    Parser::parse_sql(&GenericDialect {}, input).is_ok_and(|statements| {
        !statements.is_empty()
            && statements.iter().all(|statement| {
                matches!(
                    statement,
                    Statement::Query(_)
                        | Statement::Insert(_)
                        | Statement::Update(_)
                        | Statement::Delete(_)
                )
            })
    })
}

fn sql_migration_metadata(source_path: &str, input: &str) -> MigrationMetadata {
    let filename = Path::new(source_path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let order_hint = filename_order_hint(source_path);
    let revision = order_hint.map(|value| value.to_string());
    MigrationMetadata {
        revision,
        down_revision: None,
        order_hint,
        reversible: filename.ends_with(".up.sql")
            || filename.ends_with(".down.sql")
            || contains_keyword(input, "ROLLBACK"),
        evidence: evidence(1),
    }
}

fn inferred_crate_root(source_path: &str) -> String {
    let normalized = source_path.replace('\\', "/");
    for marker in ["/src/", "/tests/", "/examples/", "/benches/"] {
        if let Some((root, _)) = normalized.split_once(marker) {
            return root.to_owned();
        }
    }
    if normalized.starts_with("src/")
        || normalized.starts_with("tests/")
        || normalized.starts_with("examples/")
        || normalized.starts_with("benches/")
    {
        return String::new();
    }
    normalized
        .rsplit_once('/')
        .map_or_else(String::new, |(parent, _)| parent.to_owned())
}

fn normalize_data_reference(crate_root: &str, reference: &str) -> Option<String> {
    let reference = reference.replace('\\', "/");
    if reference.is_empty()
        || reference.starts_with('/')
        || reference.as_bytes().get(1) == Some(&b':')
    {
        return None;
    }
    let mut components = crate_root
        .replace('\\', "/")
        .split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for component in reference.split('/') {
        match component {
            "" | "." => {}
            ".." if !components.is_empty() => {
                components.pop();
            }
            ".." => return None,
            value => components.push(value.to_owned()),
        }
    }
    (!components.is_empty()).then(|| components.join("/"))
}

fn sql_statement_lines(input: &str) -> Vec<u32> {
    let mut lines = Vec::new();
    let mut line = 1_u32;
    let mut quote = None;
    let mut escaped = false;
    let mut statement_started = false;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut characters = input.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '\n' {
            line = line.saturating_add(1);
            line_comment = false;
            continue;
        }
        if line_comment {
            continue;
        }
        if block_comment {
            if character == '*' && characters.peek() == Some(&'/') {
                characters.next();
                block_comment = false;
            }
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
            continue;
        }
        if character == '-' && characters.peek() == Some(&'-') {
            characters.next();
            line_comment = true;
            continue;
        }
        if character == '/' && characters.peek() == Some(&'*') {
            characters.next();
            block_comment = true;
            continue;
        }
        if !statement_started && !character.is_whitespace() && character != ';' {
            lines.push(line);
            statement_started = true;
        }
        if matches!(character, '\'' | '"' | '`') {
            quote = Some(character);
        } else if character == ';' {
            statement_started = false;
        }
    }
    lines
}

#[derive(Debug, Clone, Copy)]
struct SqlStatementChunk<'a> {
    text: &'a str,
    line: u32,
}

fn sql_statement_chunks(input: &str) -> Vec<SqlStatementChunk<'_>> {
    let mut output = Vec::new();
    let mut start = 0_usize;
    let mut line = 1_u32;
    let mut statement_line = None;
    let mut quote = None;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut characters = input.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        if character == '\n' {
            line = line.saturating_add(1);
            line_comment = false;
            continue;
        }
        if line_comment {
            continue;
        }
        if block_comment {
            if character == '*' && characters.peek().is_some_and(|(_, next)| *next == '/') {
                characters.next();
                block_comment = false;
            }
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
            continue;
        }
        if character == '-' && characters.peek().is_some_and(|(_, next)| *next == '-') {
            characters.next();
            line_comment = true;
            continue;
        }
        if character == '#' {
            line_comment = true;
            continue;
        }
        if character == '/' && characters.peek().is_some_and(|(_, next)| *next == '*') {
            characters.next();
            block_comment = true;
            continue;
        }
        if !character.is_whitespace() && character != ';' && statement_line.is_none() {
            statement_line = Some(line);
        }
        if matches!(character, '\'' | '"' | '`') {
            quote = Some(character);
        } else if character == ';' {
            let end = index.saturating_add(character.len_utf8());
            if let Some(statement_line) = statement_line.take() {
                output.push(SqlStatementChunk {
                    text: &input[start..end],
                    line: statement_line,
                });
            }
            start = end;
        }
        if output.len() >= MAX_DATA_ITEMS {
            break;
        }
    }
    if output.len() < MAX_DATA_ITEMS
        && let Some(statement_line) = statement_line
        && start < input.len()
    {
        output.push(SqlStatementChunk {
            text: &input[start..],
            line: statement_line,
        });
    }
    output
}

fn contains_keyword(input: &str, keyword: &str) -> bool {
    input
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .any(|word| word.eq_ignore_ascii_case(keyword))
}

#[derive(Debug)]
struct NamedBlock<'a> {
    name: &'a str,
    body: &'a str,
    line: u32,
}

fn named_blocks<'a>(input: &'a str, keyword: &str) -> Vec<NamedBlock<'a>> {
    let mut output = Vec::new();
    let mut offset = 0;
    let marker = format!("{keyword} ");
    while let Some(relative) = input[offset..].find(&marker) {
        let start = offset + relative;
        if start > 0
            && input[..start]
                .chars()
                .next_back()
                .is_some_and(|character| character.is_ascii_alphanumeric() || character == '_')
        {
            offset = start + marker.len();
            continue;
        }
        let name_start = start + marker.len();
        let name_end = input[name_start..]
            .find(|character: char| character.is_whitespace() || character == '{')
            .map_or(input.len(), |relative_end| name_start + relative_end);
        let Some(open_relative) = input[name_end..].find('{') else {
            break;
        };
        let open = name_end + open_relative;
        let Some(close) = matching_delimiter(input, open, '{', '}') else {
            break;
        };
        output.push(NamedBlock {
            name: &input[name_start..name_end],
            body: &input[open + 1..close],
            line: line_at(input, start),
        });
        if output.len() >= MAX_DATA_ITEMS {
            break;
        }
        offset = close + 1;
    }
    output
}

fn macro_blocks<'a>(input: &'a str, macro_name: &str) -> Vec<NamedBlock<'a>> {
    let mut output = Vec::new();
    let mut offset = 0;
    while let Some(relative) = input[offset..].find(macro_name) {
        let start = offset + relative;
        let Some(open_relative) = input[start + macro_name.len()..].find('{') else {
            break;
        };
        let open = start + macro_name.len() + open_relative;
        let Some(close) = matching_delimiter(input, open, '{', '}') else {
            break;
        };
        output.push(NamedBlock {
            name: &input[start..start + macro_name.len()],
            body: &input[open + 1..close],
            line: line_at(input, start),
        });
        if output.len() >= MAX_DATA_ITEMS {
            break;
        }
        offset = close + 1;
    }
    output
}

fn matching_delimiter(input: &str, open: usize, left: char, right: char) -> Option<usize> {
    let mut depth = 0_usize;
    let mut quote = None;
    let mut escaped = false;
    for (relative, character) in input[open..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
            continue;
        }
        if matches!(character, '\'' | '"' | '`') {
            quote = Some(character);
        } else if character == left {
            depth += 1;
            if depth > MAX_DATA_DEPTH {
                return None;
            }
        } else if character == right {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(open + relative);
            }
        }
    }
    None
}

fn line_at(input: &str, byte: usize) -> u32 {
    u32::try_from(
        input[..byte.min(input.len())]
            .bytes()
            .filter(|byte| *byte == b'\n')
            .count()
            + 1,
    )
    .unwrap_or(MAX_DATA_SOURCE_LINES)
    .min(MAX_DATA_SOURCE_LINES)
}

fn find_call_literal(input: &str, marker: &str) -> Option<String> {
    let position = input.find(marker)?;
    first_quoted(&input[position + marker.len()..])
}

fn bracket_values_after(input: &str, marker: &str) -> Option<Vec<String>> {
    let position = input.find(marker)?;
    bracket_values(&input[position + marker.len()..])
}

fn bracket_values(input: &str) -> Option<Vec<String>> {
    let open = input.find('[')?;
    let close = matching_delimiter(input, open, '[', ']')?;
    Some(comma_identifiers(&input[open + 1..close]))
}

fn comma_identifiers(input: &str) -> Vec<String> {
    input
        .split(',')
        .map(clean_identifier)
        .filter(|value| !value.is_empty())
        .collect()
}

#[derive(Debug)]
struct Call {
    body: String,
    line: u32,
}

fn collect_calls(input: &str, marker: &str) -> Vec<Call> {
    let mut output = Vec::new();
    let mut offset = 0;
    while let Some(relative) = input[offset..].find(marker) {
        let start = offset + relative;
        let body_start = start + marker.len();
        let Some(body) = balanced_slice(&input[body_start..]) else {
            break;
        };
        output.push(Call {
            body: body.to_owned(),
            line: line_at(input, start),
        });
        offset = body_start + body.len() + 1;
        if output.len() >= MAX_DATA_ITEMS {
            break;
        }
    }
    output
}

fn balanced_slice(input: &str) -> Option<&str> {
    let mut depth = 1_usize;
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in input.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
            continue;
        }
        if matches!(character, '\'' | '"' | '`') {
            quote = Some(character);
        } else if character == '(' {
            depth += 1;
            if depth > MAX_DATA_DEPTH {
                return None;
            }
        } else if character == ')' {
            depth -= 1;
            if depth == 0 {
                return Some(&input[..index]);
            }
        }
    }
    None
}

fn first_quoted(input: &str) -> Option<String> {
    quoted_values(input).into_iter().next()
}

fn quoted_values(input: &str) -> Vec<String> {
    let mut output = Vec::new();
    let bytes = input.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if !matches!(bytes[index], b'\'' | b'"') {
            index += 1;
            continue;
        }
        let quote = bytes[index];
        let start = index + 1;
        index = start;
        let mut escaped = false;
        while index < bytes.len() {
            if escaped {
                escaped = false;
            } else if bytes[index] == b'\\' {
                escaped = true;
            } else if bytes[index] == quote {
                output.push(bounded_identifier(&input[start..index]));
                index += 1;
                break;
            }
            index += 1;
        }
    }
    output
}

fn assignment_literal(input: &str, name: &str) -> Option<String> {
    input.lines().find_map(|line| {
        let (left, right) = line.split_once('=')?;
        (left.trim() == name).then(|| first_quoted(right)).flatten()
    })
}

fn assignment_line(input: &str, name: &str) -> Option<u32> {
    input.lines().enumerate().find_map(|(index, line)| {
        let (left, _) = line.split_once('=')?;
        (left.trim() == name).then(|| u32::try_from(index + 1).unwrap_or(MAX_DATA_SOURCE_LINES))
    })
}

fn python_column(input: &str, line: DataEvidenceLine) -> Option<DatabaseColumn> {
    python_column_with_fallback(input, line, None)
}

fn python_column_with_fallback(
    input: &str,
    line: DataEvidenceLine,
    fallback_name: Option<&str>,
) -> Option<DatabaseColumn> {
    let values = quoted_values(input);
    let explicit_name = input.trim_start().starts_with(['\'', '"']);
    let name = if explicit_name {
        values.first().cloned()
    } else {
        fallback_name.map(bounded_identifier)
    }?;
    let type_position = usize::from(explicit_name);
    let data_type = input
        .split(',')
        .nth(type_position)
        .map(str::trim)
        .filter(|value| !value.contains("ForeignKey"))
        .map(|value| {
            bounded_identifier(
                value
                    .trim_start_matches("sa.")
                    .trim_end_matches("()")
                    .trim(),
            )
        });
    Some(DatabaseColumn {
        name,
        data_type,
        nullable: if input.contains("nullable=False") {
            Some(false)
        } else if input.contains("nullable=True") {
            Some(true)
        } else {
            None
        },
        primary_key: input.contains("primary_key=True"),
        unique: input.contains("unique=True"),
        default_present: input.contains("default=") || input.contains("server_default="),
        evidence: line,
    })
}

fn python_foreign_key(input: &str, line: DataEvidenceLine) -> Option<DatabaseForeignKey> {
    let lists = all_bracket_values(input);
    let local = lists.first()?.clone();
    let references = lists.get(1)?.clone();
    let reference = references.first()?;
    let (table, column) = split_reference(reference);
    Some(DatabaseForeignKey {
        name: None,
        columns: local,
        referenced_table: table,
        referenced_columns: column.into_iter().collect(),
        evidence: line,
    })
}

fn all_bracket_values(input: &str) -> Vec<Vec<String>> {
    let mut output = Vec::new();
    let mut offset = 0;
    while let Some(relative) = input[offset..].find('[') {
        let open = offset + relative;
        let Some(close) = matching_delimiter(input, open, '[', ']') else {
            break;
        };
        output.push(
            quoted_values(&input[open + 1..close])
                .into_iter()
                .map(|value| bounded_identifier(&value))
                .collect(),
        );
        offset = close + 1;
    }
    output
}

fn split_reference(reference: &str) -> (String, Option<String>) {
    reference.rsplit_once('.').map_or_else(
        || (bounded_identifier(reference), None),
        |(table, column)| (bounded_identifier(table), Some(bounded_identifier(column))),
    )
}

fn table_argument_literal(input: &str, name: &str) -> Option<String> {
    input.lines().find_map(|line| {
        if !line.contains("__table_args__") || !line.contains(name) {
            return None;
        }
        let position = line.find(name)?;
        let remainder = &line[position + name.len()..];
        let value_start = remainder.find(':').map_or(0, |colon| colon + 1);
        first_quoted(&remainder[value_start..])
    })
}

#[derive(Debug)]
struct PythonClass<'a> {
    name: &'a str,
    body: &'a str,
    line: u32,
}

fn python_classes(input: &str) -> Vec<PythonClass<'_>> {
    let mut starts = Vec::new();
    let mut byte = 0;
    for (index, line) in input.lines().enumerate() {
        if let Some(rest) = line.strip_prefix("class ")
            && let Some(end) = rest.find(['(', ':'])
        {
            starts.push((byte, index + 1, &rest[..end]));
            if starts.len() >= MAX_DATA_ITEMS {
                break;
            }
        }
        byte += line.len() + 1;
    }
    starts
        .iter()
        .enumerate()
        .map(|(index, (start, line, name))| {
            let body_start = input[*start..]
                .find('\n')
                .map_or(input.len(), |relative| start + relative + 1);
            let body_end = starts
                .get(index + 1)
                .map_or(input.len(), |(next, _, _)| *next);
            PythonClass {
                name,
                body: &input[body_start..body_end],
                line: u32::try_from(*line).unwrap_or(MAX_DATA_SOURCE_LINES),
            }
        })
        .collect()
}

fn call_literal(input: &str, call: &str) -> Option<String> {
    let position = input.find(call)?;
    first_quoted(&input[position + call.len()..])
}

#[derive(Debug, Clone)]
struct SourceLiteral {
    value: String,
    line: u32,
    start: usize,
    end: usize,
    dynamic: bool,
    interpolated: bool,
}

fn quoted_literals(input: &str, language: SourceLanguage) -> Vec<SourceLiteral> {
    if language == SourceLanguage::Rust {
        return rust_source_string_literals(input);
    }
    let bytes = input.as_bytes();
    let mut output = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let quote = match bytes[index] {
            b'\'' | b'"' | b'`' => bytes[index],
            _ => {
                index += 1;
                continue;
            }
        };
        let triple = language == SourceLanguage::Python
            && index + 2 < bytes.len()
            && bytes[index + 1] == quote
            && bytes[index + 2] == quote;
        let delimiter = if triple { 3 } else { 1 };
        let content_start = index + delimiter;
        let mut cursor = content_start;
        let mut escaped = false;
        let mut closed = None;
        while cursor < bytes.len() {
            if triple
                && cursor + 2 < bytes.len()
                && bytes[cursor] == quote
                && bytes[cursor + 1] == quote
                && bytes[cursor + 2] == quote
            {
                closed = Some(cursor);
                break;
            }
            if !triple {
                if escaped {
                    escaped = false;
                    cursor += 1;
                    continue;
                }
                if bytes[cursor] == b'\\' {
                    escaped = true;
                    cursor += 1;
                    continue;
                }
                if bytes[cursor] == quote {
                    closed = Some(cursor);
                    break;
                }
            }
            cursor += 1;
        }
        let Some(content_end) = closed else {
            break;
        };
        let value = input[content_start..content_end].to_owned();
        let end = content_end + delimiter;
        let interpolated = is_interpolated(input, language, index, quote, &value);
        let dynamic = interpolated || adjacent_dynamic_operator(input, index, end);
        output.push(SourceLiteral {
            value,
            line: line_at(input, index),
            start: index,
            end,
            dynamic,
            interpolated,
        });
        index = end;
        if output.len() >= MAX_DATA_ITEMS {
            break;
        }
    }
    output
}

fn rust_source_string_literals(input: &str) -> Vec<SourceLiteral> {
    let mut parser = tree_sitter::Parser::new();
    let grammar = tree_sitter_rust::LANGUAGE.into();
    if parser.set_language(&grammar).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(input, None) else {
        return Vec::new();
    };
    let mut output = Vec::new();
    collect_rust_string_literals(tree.root_node(), input, &mut output);
    output.sort_by_key(|literal| literal.start);
    output.truncate(MAX_DATA_ITEMS);
    output
}

fn collect_rust_string_literals(
    node: SyntaxNode<'_>,
    input: &str,
    output: &mut Vec<SourceLiteral>,
) {
    if matches!(
        node.kind(),
        "string_literal" | "raw_string_literal" | "byte_string_literal" | "raw_byte_string_literal"
    ) {
        if let Ok(source) = node.utf8_text(input.as_bytes())
            && let Some(mut literal) = rust_string_literals(source, 1).into_iter().next()
        {
            literal.start = literal.start.saturating_add(node.start_byte());
            literal.end = literal.end.saturating_add(node.start_byte());
            literal.line = u32::try_from(node.start_position().row)
                .unwrap_or(MAX_DATA_SOURCE_LINES)
                .saturating_add(1);
            literal.dynamic = adjacent_dynamic_operator(input, literal.start, literal.end);
            output.push(literal);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_rust_string_literals(child, input, output);
    }
}

fn rust_string_literals(input: &str, base_line: u32) -> Vec<SourceLiteral> {
    let bytes = input.as_bytes();
    let mut output = Vec::new();
    let mut index = 0;
    while index < bytes.len() && output.len() < MAX_DATA_ITEMS {
        if bytes[index] == b'r' {
            let mut quote = index + 1;
            while quote < bytes.len() && bytes[quote] == b'#' {
                quote += 1;
            }
            if quote < bytes.len() && bytes[quote] == b'"' {
                let hashes = quote.saturating_sub(index + 1);
                let content_start = quote + 1;
                let mut cursor = content_start;
                while cursor < bytes.len() {
                    if bytes[cursor] == b'"'
                        && cursor + hashes < bytes.len()
                        && (hashes == 0
                            || bytes[cursor + 1..=cursor + hashes]
                                .iter()
                                .all(|byte| *byte == b'#'))
                    {
                        let end = cursor + hashes + 1;
                        output.push(SourceLiteral {
                            value: input[content_start..cursor].to_owned(),
                            line: base_line.saturating_add(line_at(input, index).saturating_sub(1)),
                            start: index,
                            end,
                            dynamic: false,
                            interpolated: false,
                        });
                        index = end;
                        break;
                    }
                    cursor += 1;
                }
                if index >= content_start {
                    continue;
                }
            }
        }
        if bytes[index] != b'"' {
            index += 1;
            continue;
        }
        let content_start = index + 1;
        let mut cursor = content_start;
        let mut escaped = false;
        while cursor < bytes.len() {
            if escaped {
                escaped = false;
                cursor += 1;
                continue;
            }
            match bytes[cursor] {
                b'\\' => escaped = true,
                b'"' => {
                    let end = cursor + 1;
                    output.push(SourceLiteral {
                        value: unescape_rust_string(&input[content_start..cursor]),
                        line: base_line.saturating_add(line_at(input, index).saturating_sub(1)),
                        start: index,
                        end,
                        dynamic: adjacent_dynamic_operator(input, index, end),
                        interpolated: false,
                    });
                    index = end;
                    break;
                }
                _ => {}
            }
            cursor += 1;
        }
        if index < content_start {
            break;
        }
    }
    output
}

fn unescape_rust_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            output.push(character);
            continue;
        }
        match characters.next() {
            Some('n') => output.push('\n'),
            Some('r') => output.push('\r'),
            Some('t') => output.push('\t'),
            Some('\\') | None => output.push('\\'),
            Some('"') => output.push('"'),
            Some('\'') => output.push('\''),
            Some(other) => {
                output.push('\\');
                output.push(other);
            }
        }
    }
    output
}

#[derive(Debug, Default)]
struct SqlxImports {
    names: BTreeMap<String, String>,
}

impl SqlxImports {
    fn discover(input: &str) -> Self {
        let mut imports = Self::default();
        for statement in input.split(';') {
            let compact = statement
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect::<String>();
            let Some(rest) = compact.strip_prefix("usesqlx::") else {
                continue;
            };
            if let Some(group) = rest
                .strip_prefix('{')
                .and_then(|value| value.strip_suffix('}'))
            {
                for item in group.split(',') {
                    imports.insert_item(item);
                }
            } else {
                imports.insert_item(rest);
            }
        }
        imports
    }

    fn insert_item(&mut self, item: &str) {
        let item = item.rsplit("::").next().unwrap_or(item);
        if sqlx_api_kind(item).is_some() || matches!(item, "Executor" | "QueryBuilder" | "Migrator")
        {
            self.names.insert(item.to_owned(), item.to_owned());
            return;
        }
        for (index, _) in item.match_indices("as") {
            let (canonical, local_with_as) = item.split_at(index);
            let local = &local_with_as[2..];
            if !local.is_empty()
                && (sqlx_api_kind(canonical).is_some()
                    || matches!(canonical, "Executor" | "QueryBuilder" | "Migrator"))
            {
                self.names.insert(local.to_owned(), canonical.to_owned());
                return;
            }
        }
    }

    fn canonical<'a>(&'a self, local: &'a str) -> Option<&'a str> {
        self.names.get(local).map(String::as_str)
    }

    fn contains(&self, canonical: &str) -> bool {
        self.names.values().any(|value| value == canonical)
    }
}

#[derive(Debug, Default)]
struct MysqlAsyncImports {
    crate_names: BTreeSet<String>,
    traits: BTreeMap<String, String>,
}

impl MysqlAsyncImports {
    fn discover(root: SyntaxNode<'_>, input: &str) -> Self {
        let mut declarations = Vec::new();
        collect_rust_use_declarations(root, input, &mut declarations);
        let mut imports = Self {
            crate_names: BTreeSet::from(["mysql_async".to_owned()]),
            traits: BTreeMap::new(),
        };
        for declaration in &declarations {
            let compact = compact_rust(declaration);
            if let Some(alias) = compact
                .strip_prefix("usemysql_asyncas")
                .and_then(|value| value.strip_suffix(';'))
                .filter(|value| is_rust_identifier(value))
            {
                imports.crate_names.insert(alias.to_owned());
            }
        }
        for declaration in declarations {
            imports.insert_declaration(&compact_rust(declaration));
        }
        imports
    }

    fn insert_declaration(&mut self, declaration: &str) {
        let Some(path) = declaration.strip_prefix("use") else {
            return;
        };
        if !self
            .crate_names
            .iter()
            .any(|name| path.starts_with(&format!("{name}::")))
        {
            return;
        }
        if path.contains("prelude::*") {
            for name in MYSQL_ASYNC_TRAITS {
                self.traits.insert((*name).to_owned(), (*name).to_owned());
            }
        }
        for canonical in MYSQL_ASYNC_TRAITS {
            let Some(start) = path.match_indices(canonical).find_map(|(start, _)| {
                let before = &path[..start];
                let rest = &path[start + canonical.len()..];
                let valid_before = before.ends_with([':', '{', ',']);
                let valid_after = rest.starts_with("as")
                    || rest
                        .chars()
                        .next()
                        .is_some_and(|character| matches!(character, ',' | '}' | ';'));
                (valid_before && valid_after).then_some(start)
            }) else {
                continue;
            };
            let rest = &path[start + canonical.len()..];
            let alias = rest
                .strip_prefix("as")
                .map(|value| {
                    value
                        .chars()
                        .take_while(|character| {
                            character.is_ascii_alphanumeric() || *character == '_'
                        })
                        .collect::<String>()
                })
                .filter(|value| is_rust_identifier(value))
                .unwrap_or_else(|| (*canonical).to_owned());
            self.traits.insert(alias, (*canonical).to_owned());
        }
    }

    fn contains(&self, canonical: &str) -> bool {
        self.traits.values().any(|value| value == canonical)
    }

    fn canonical<'a>(&'a self, local: &'a str) -> Option<&'a str> {
        self.traits.get(local).map(String::as_str)
    }

    fn is_qualified(&self, function: &str) -> bool {
        self.crate_names
            .iter()
            .any(|name| function.contains(&format!("{name}::")))
    }
}

const MYSQL_ASYNC_TRAITS: &[&str] = &["Queryable", "Query", "WithParams", "BatchQuery"];

fn collect_rust_use_declarations<'a>(
    node: SyntaxNode<'a>,
    input: &'a str,
    output: &mut Vec<&'a str>,
) {
    if node.kind() == "use_declaration" {
        if let Ok(declaration) = node.utf8_text(input.as_bytes()) {
            output.push(declaration);
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_rust_use_declarations(child, input, output);
    }
}

fn compact_rust(input: &str) -> String {
    input
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn is_rust_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SqlxApiKind {
    Inline,
    QueryFile,
    MigrationDirectory,
    QueryBuilder,
}

fn sqlx_api_kind(name: &str) -> Option<SqlxApiKind> {
    match name {
        "query"
        | "query_with"
        | "query_with_result"
        | "query_unchecked"
        | "query_as"
        | "query_as_with"
        | "query_as_with_result"
        | "query_as_unchecked"
        | "query_scalar"
        | "query_scalar_with"
        | "query_scalar_with_result"
        | "query_scalar_unchecked"
        | "raw_sql" => Some(SqlxApiKind::Inline),
        "query_file"
        | "query_file_unchecked"
        | "query_file_as"
        | "query_file_as_unchecked"
        | "query_file_scalar"
        | "query_file_scalar_unchecked" => Some(SqlxApiKind::QueryFile),
        "migrate" => Some(SqlxApiKind::MigrationDirectory),
        "QueryBuilder" => Some(SqlxApiKind::QueryBuilder),
        _ => None,
    }
}

fn extract_rust_database_source(input: &str, crate_root: &str, document: &mut DataDocument) {
    let mut parser = tree_sitter::Parser::new();
    let grammar = tree_sitter_rust::LANGUAGE.into();
    if parser.set_language(&grammar).is_err() {
        mark_incomplete(document, DataWarning::UnsupportedConstruct);
        return;
    }
    let Some(tree) = parser.parse(input, None) else {
        mark_incomplete(document, DataWarning::UnsupportedConstruct);
        return;
    };
    if tree.root_node().has_error() {
        mark_incomplete(document, DataWarning::SqlParseRecovery);
    }
    let sqlx_imports = SqlxImports::discover(input);
    let mysql_async_imports = MysqlAsyncImports::discover(tree.root_node(), input);
    visit_rust_data_nodes(
        tree.root_node(),
        input,
        crate_root,
        &sqlx_imports,
        &mysql_async_imports,
        None,
        document,
    );
}

fn visit_rust_data_nodes(
    node: SyntaxNode<'_>,
    input: &str,
    crate_root: &str,
    sqlx_imports: &SqlxImports,
    mysql_async_imports: &MysqlAsyncImports,
    owner: Option<&str>,
    document: &mut DataDocument,
) {
    let owned_name = (node.kind() == "function_item")
        .then(|| node.child_by_field_name("name"))
        .flatten()
        .and_then(|name| name.utf8_text(input.as_bytes()).ok())
        .map(bounded_identifier);
    let owner = owned_name.as_deref().or(owner);

    match node.kind() {
        "call_expression" => {
            if let (Some(function), Some(arguments)) = (
                node.child_by_field_name("function"),
                node.child_by_field_name("arguments"),
            ) && let (Ok(function), Ok(arguments)) = (
                function.utf8_text(input.as_bytes()),
                arguments.utf8_text(input.as_bytes()),
            ) {
                inspect_sqlx_call(
                    function,
                    arguments,
                    node.start_position().row,
                    crate_root,
                    sqlx_imports,
                    owner,
                    document,
                );
                inspect_mysql_async_call(
                    function,
                    arguments,
                    node.start_position().row,
                    mysql_async_imports,
                    owner,
                    document,
                );
            }
        }
        "macro_invocation" => {
            if let Ok(invocation) = node.utf8_text(input.as_bytes()) {
                inspect_sqlx_macro(
                    invocation,
                    node.start_position().row,
                    crate_root,
                    sqlx_imports,
                    owner,
                    document,
                );
            }
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        visit_rust_data_nodes(
            child,
            input,
            crate_root,
            sqlx_imports,
            mysql_async_imports,
            owner,
            document,
        );
    }
}

fn inspect_sqlx_call(
    function: &str,
    arguments: &str,
    zero_based_line: usize,
    crate_root: &str,
    imports: &SqlxImports,
    owner: Option<&str>,
    document: &mut DataDocument,
) {
    let compact = strip_rust_generics(function);
    let local = compact
        .rsplit([':', '.'])
        .find(|component| !component.is_empty())
        .unwrap_or(&compact);
    let qualified = compact.contains("sqlx::");
    let canonical = if qualified {
        local
    } else {
        imports.canonical(local).unwrap_or(local)
    };
    let executor_method = matches!(
        canonical,
        "execute"
            | "execute_many"
            | "fetch"
            | "fetch_many"
            | "fetch_all"
            | "fetch_one"
            | "fetch_optional"
    ) && (imports.contains("Executor")
        || compact.contains("sqlx::Executor::"));
    let kind = sqlx_api_kind(canonical)
        .or_else(|| executor_method.then_some(SqlxApiKind::Inline))
        .or_else(|| {
            let builder = compact.ends_with("QueryBuilder::new")
                || compact.ends_with("QueryBuilder::with_arguments")
                || imports.canonical(compact.split("::").next().unwrap_or_default())
                    == Some("QueryBuilder");
            builder.then_some(SqlxApiKind::QueryBuilder)
        });
    let migrator = compact.ends_with("Migrator::new")
        && (qualified
            || imports.canonical(compact.split("::").next().unwrap_or_default())
                == Some("Migrator"));
    let kind = if migrator {
        Some(SqlxApiKind::MigrationDirectory)
    } else if qualified
        || executor_method
        || imports.canonical(local).is_some()
        || kind == Some(SqlxApiKind::QueryBuilder)
    {
        kind
    } else {
        None
    };
    let Some(kind) = kind else {
        return;
    };
    document.frameworks.push(DataFramework::Sqlx);
    let base_line = u32::try_from(zero_based_line)
        .unwrap_or(MAX_DATA_SOURCE_LINES)
        .saturating_add(1);
    let literal = rust_string_literals(arguments, base_line)
        .into_iter()
        .next();
    apply_sqlx_api(kind, literal, base_line, crate_root, owner, document);
    if kind == SqlxApiKind::QueryBuilder {
        mark_incomplete(document, DataWarning::DynamicQuery);
    }
}

fn inspect_mysql_async_call(
    function: &str,
    arguments: &str,
    zero_based_line: usize,
    imports: &MysqlAsyncImports,
    owner: Option<&str>,
    document: &mut DataDocument,
) {
    let compact = strip_rust_generics(function);
    let method = compact
        .rsplit([':', '.'])
        .find(|component| !component.is_empty())
        .unwrap_or(&compact);
    let root = compact.split("::").next().unwrap_or_default();
    let imported_trait = imports.canonical(root);
    let qualified = imports.is_qualified(&compact);
    let method_call = compact.contains('.');
    let queryable = mysql_async_queryable_method(method)
        && (qualified
            || imported_trait == Some("Queryable")
            || (method_call && imports.contains("Queryable")));
    let required_query_trait = mysql_async_query_trait(method);
    let fluent = required_query_trait.is_some()
        && (qualified
            || imported_trait == required_query_trait
            || required_query_trait.is_some_and(|required| imports.contains(required)));
    if !queryable && !fluent {
        return;
    }

    let base_line = u32::try_from(zero_based_line)
        .unwrap_or(MAX_DATA_SOURCE_LINES)
        .saturating_add(1);
    let literal = if queryable || qualified || imported_trait.is_some() {
        rust_string_literals(arguments, base_line)
            .into_iter()
            .next()
            .or_else(|| mysql_async_fluent_literal(function, method, base_line))
    } else {
        mysql_async_fluent_literal(function, method, base_line)
    };
    document.frameworks.push(DataFramework::MysqlAsync);
    let Some(literal) = literal else {
        mark_incomplete(document, DataWarning::DynamicQuery);
        return;
    };
    if literal.dynamic {
        mark_incomplete(document, DataWarning::DynamicQuery);
        return;
    }
    append_inline_sql(&literal.value, evidence(literal.line), owner, document);
}

fn mysql_async_queryable_method(name: &str) -> bool {
    matches!(
        name,
        "query_iter"
            | "prep"
            | "exec_iter"
            | "query"
            | "query_first"
            | "query_map"
            | "query_fold"
            | "query_drop"
            | "exec_batch"
            | "exec"
            | "exec_first"
            | "exec_map"
            | "exec_fold"
            | "exec_drop"
            | "query_stream"
            | "exec_stream"
    )
}

fn mysql_async_query_trait(name: &str) -> Option<&'static str> {
    match name {
        "run" | "first" | "fetch" | "reduce" | "map" | "stream" | "ignore" => Some("Query"),
        "with" => Some("WithParams"),
        "batch" => Some("BatchQuery"),
        _ => None,
    }
}

fn mysql_async_fluent_literal(
    function: &str,
    method: &str,
    base_line: u32,
) -> Option<SourceLiteral> {
    let literal = rust_string_literals(function, base_line)
        .into_iter()
        .next()?;
    let suffix = function.get(literal.end..)?;
    let suffix = strip_rust_generics(&compact_rust(suffix));
    let suffix = suffix.trim_start_matches(')');
    let terminal = format!(".{method}");
    (suffix == terminal || (suffix.starts_with(".with(") && suffix.ends_with(&terminal)))
        .then_some(literal)
}

fn inspect_sqlx_macro(
    invocation: &str,
    zero_based_line: usize,
    crate_root: &str,
    imports: &SqlxImports,
    owner: Option<&str>,
    document: &mut DataDocument,
) {
    let Some((name, arguments)) = invocation.split_once('!') else {
        return;
    };
    let compact = name
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    let local = compact.rsplit("::").next().unwrap_or(&compact);
    let qualified = compact.contains("sqlx::");
    let canonical = if qualified {
        local
    } else {
        imports.canonical(local).unwrap_or(local)
    };
    let Some(kind) = sqlx_api_kind(canonical) else {
        return;
    };
    if !qualified && imports.canonical(local).is_none() {
        return;
    }
    document.frameworks.push(DataFramework::Sqlx);
    let base_line = u32::try_from(zero_based_line)
        .unwrap_or(MAX_DATA_SOURCE_LINES)
        .saturating_add(1);
    let literal = rust_string_literals(arguments, base_line)
        .into_iter()
        .next();
    apply_sqlx_api(kind, literal, base_line, crate_root, owner, document);
}

fn apply_sqlx_api(
    kind: SqlxApiKind,
    literal: Option<SourceLiteral>,
    call_line: u32,
    crate_root: &str,
    owner: Option<&str>,
    document: &mut DataDocument,
) {
    match (kind, literal) {
        (SqlxApiKind::Inline | SqlxApiKind::QueryBuilder, Some(literal)) if !literal.dynamic => {
            append_inline_sql(&literal.value, evidence(literal.line), owner, document);
        }
        (SqlxApiKind::QueryFile, Some(literal)) if !literal.dynamic => {
            push_sqlx_reference(
                DataArtifactReferenceKind::QueryFile,
                &literal.value,
                evidence(literal.line),
                crate_root,
                owner,
                document,
            );
        }
        (SqlxApiKind::MigrationDirectory, literal) => {
            let Some(literal) = literal else {
                if let Some(owner) = owner {
                    document.owners.push(owner.to_owned());
                }
                document.references.push(DataArtifactReference {
                    framework: DataFramework::Sqlx,
                    kind: DataArtifactReferenceKind::SqlxDefaultMigrationDirectory,
                    path: crate_root.replace('\\', "/").trim_matches('/').to_owned(),
                    owner: owner.map(ToOwned::to_owned),
                    evidence: evidence(call_line),
                });
                return;
            };
            push_sqlx_reference(
                DataArtifactReferenceKind::MigrationDirectory,
                &literal.value,
                evidence(literal.line),
                crate_root,
                owner,
                document,
            );
        }
        _ => mark_incomplete(document, DataWarning::DynamicQuery),
    }
}

fn append_inline_sql(
    sql: &str,
    line: DataEvidenceLine,
    owner: Option<&str>,
    document: &mut DataDocument,
) {
    let Ok(statements) = Parser::parse_sql(&GenericDialect {}, sql) else {
        mark_incomplete(document, DataWarning::SqlParseRecovery);
        return;
    };
    if let Some(owner) = owner {
        document.owners.push(owner.to_owned());
    }
    for statement in &statements {
        if matches!(
            statement,
            Statement::Query(_)
                | Statement::Insert(_)
                | Statement::Update(_)
                | Statement::Delete(_)
        ) {
            append_statement_accesses(statement, line, owner, document, 0);
        } else if !append_schema_statement(statement, line, document) {
            mark_incomplete(document, DataWarning::UnsupportedConstruct);
        }
    }
}

fn push_sqlx_reference(
    kind: DataArtifactReferenceKind,
    path: &str,
    line: DataEvidenceLine,
    crate_root: &str,
    owner: Option<&str>,
    document: &mut DataDocument,
) {
    let Some(path) = normalize_data_reference(crate_root, path) else {
        mark_incomplete(document, DataWarning::UnresolvedReference);
        return;
    };
    if let Some(owner) = owner {
        document.owners.push(owner.to_owned());
    }
    document.references.push(DataArtifactReference {
        framework: DataFramework::Sqlx,
        kind,
        path,
        owner: owner.map(ToOwned::to_owned),
        evidence: line,
    });
}

fn strip_rust_generics(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut depth = 0_u32;
    for character in value.chars().filter(|character| !character.is_whitespace()) {
        match character {
            '<' => depth = depth.saturating_add(1),
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => output.push(character),
            _ => {}
        }
    }
    output.replace("::::", "::")
}

fn is_interpolated(
    input: &str,
    language: SourceLanguage,
    start: usize,
    quote: u8,
    value: &str,
) -> bool {
    (quote == b'`' && value.contains("${"))
        || (language == SourceLanguage::Python
            && python_string_prefix(input, start).contains(['f', 'F'])
            && value.contains('{')
            && value.contains('}'))
}

fn python_string_prefix(input: &str, start: usize) -> &str {
    let bytes = input.as_bytes();
    let mut prefix_start = start;
    while prefix_start > 0
        && bytes[prefix_start - 1].is_ascii_alphabetic()
        && start.saturating_sub(prefix_start) < 3
    {
        prefix_start -= 1;
    }
    &input[prefix_start..start]
}

fn sanitize_python_f_string(value: &str) -> Option<String> {
    let mut output = String::with_capacity(value.len());
    let mut characters = value.char_indices().peekable();
    while let Some((_, character)) = characters.next() {
        if character == '{' {
            if characters.peek().is_some_and(|(_, next)| *next == '{') {
                characters.next();
                output.push('{');
                continue;
            }
            let mut depth = 1_usize;
            let mut quote = None;
            let mut escaped = false;
            for (_, candidate) in characters.by_ref() {
                if escaped {
                    escaped = false;
                    continue;
                }
                if candidate == '\\' && quote.is_some() {
                    escaped = true;
                    continue;
                }
                if let Some(active) = quote {
                    if candidate == active {
                        quote = None;
                    }
                    continue;
                }
                if matches!(candidate, '\'' | '"') {
                    quote = Some(candidate);
                } else if candidate == '{' {
                    depth = depth.saturating_add(1);
                } else if candidate == '}' {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        break;
                    }
                }
            }
            if depth != 0 {
                return None;
            }
            output.push_str("__csg_dynamic_value__");
        } else if character == '}' && characters.peek().is_some_and(|(_, next)| *next == '}') {
            characters.next();
            output.push('}');
        } else {
            output.push(character);
        }
    }
    Some(output)
}

fn python_format_call_after(input: &str, literal: &SourceLiteral) -> bool {
    input
        .get(literal.end..)
        .is_some_and(|after| after.trim_start().starts_with(".format("))
}

fn extract_sqlalchemy_accesses(input: &str, document: &mut DataDocument) {
    let mut known_models = BTreeSet::new();
    for line in input.lines() {
        if line.trim_start().starts_with('#') {
            continue;
        }
        if let Some(model) = sqlalchemy_query_model(line) {
            known_models.insert(model);
        }
        for model in sqlalchemy_qualified_constructors(line) {
            known_models.insert(model);
        }
    }
    if known_models.is_empty() {
        return;
    }
    document.frameworks.push(DataFramework::SqlAlchemy);
    let mut variables = BTreeMap::new();
    for (offset, line) in input.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let line_number = u32::try_from(offset)
            .unwrap_or(MAX_DATA_SOURCE_LINES)
            .saturating_add(1);
        if let Some((variable, model)) = sqlalchemy_model_assignment(trimmed, &known_models) {
            variables.insert(variable, model);
        }
        if let Some(model) = sqlalchemy_query_model(trimmed) {
            append_sqlalchemy_access(
                input,
                document,
                &model,
                if trimmed.contains(".update(") || trimmed.contains(".delete(") {
                    DataAccessRole::Writer
                } else {
                    DataAccessRole::Reader
                },
                if trimmed.contains(".update(") {
                    DataOperation::Update
                } else if trimmed.contains(".delete(") {
                    DataOperation::Delete
                } else {
                    DataOperation::Select
                },
                line_number,
            );
        }
        for (marker, operation) in [
            (".add(", DataOperation::Insert),
            (".delete(", DataOperation::Delete),
        ] {
            let Some(argument) = call_argument_identifier(trimmed, marker) else {
                continue;
            };
            let model = variables
                .get(&argument)
                .cloned()
                .or_else(|| known_models.contains(&argument).then_some(argument));
            if let Some(model) = model {
                append_sqlalchemy_access(
                    input,
                    document,
                    &model,
                    DataAccessRole::Writer,
                    operation,
                    line_number,
                );
            }
        }
    }
}

fn append_sqlalchemy_access(
    input: &str,
    document: &mut DataDocument,
    model: &str,
    role: DataAccessRole,
    operation: DataOperation,
    line: u32,
) {
    let owner = owner_at_line(SourceLanguage::Python, input, line);
    if let Some(owner) = owner.as_ref() {
        document.owners.push(owner.clone());
    }
    document.accesses.push(DataAccessObservation {
        role,
        table: String::new(),
        model: Some(model.to_owned()),
        owner,
        operation: Some(operation),
        evidence: evidence(line),
    });
}

fn sqlalchemy_query_model(line: &str) -> Option<String> {
    let (_, after) = line.split_once(".query(")?;
    python_model_name(after)
}

fn sqlalchemy_qualified_constructors(line: &str) -> Vec<String> {
    let mut output = Vec::new();
    let mut remaining = line;
    while let Some((_, after)) = remaining.split_once("models.") {
        if let Some(model) = python_model_name(after)
            && after[model.len()..].trim_start().starts_with('(')
        {
            output.push(model);
        }
        remaining = after.get(1..).unwrap_or_default();
    }
    output
}

fn sqlalchemy_model_assignment(
    line: &str,
    known_models: &BTreeSet<String>,
) -> Option<(String, String)> {
    let (left, right) = line.split_once('=')?;
    let variable = left.trim();
    if !is_python_identifier(variable) {
        return None;
    }
    let model = python_model_name(right.trim())?;
    known_models
        .contains(&model)
        .then(|| (variable.to_owned(), model))
}

fn call_argument_identifier(line: &str, marker: &str) -> Option<String> {
    let (_, after) = line.split_once(marker)?;
    let argument = after
        .trim_start()
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .next()?;
    is_python_identifier(argument).then(|| argument.to_owned())
}

fn python_model_name(input: &str) -> Option<String> {
    let qualified = input
        .trim_start()
        .split(|character: char| {
            !character.is_ascii_alphanumeric() && character != '_' && character != '.'
        })
        .next()?;
    let model = qualified.rsplit('.').next()?;
    is_python_identifier(model).then(|| model.to_owned())
}

fn is_python_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    characters
        .next()
        .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn adjacent_dynamic_operator(input: &str, start: usize, end: usize) -> bool {
    let before = input[..start].trim_end();
    let after = input[end..].trim_start();
    before.ends_with('+')
        || after.starts_with('+')
        || before.ends_with("format!(")
        || after.starts_with(".format(")
}

fn has_query_context(input: &str, literal: &SourceLiteral) -> bool {
    let context_start = floor_char_boundary(input, literal.start.saturating_sub(160));
    let before = input[context_start..literal.start].to_ascii_lowercase();
    let after_end = floor_char_boundary(input, (literal.end + 80).min(input.len()));
    let after = input[literal.end..after_end].to_ascii_lowercase();
    [
        "query", "execute", "fetch", "select", ".sql(", "sql =", "sql!", "sqlx", "prepare", "raw",
    ]
    .iter()
    .any(|marker| before.contains(marker) || after.contains(marker))
}

fn floor_char_boundary(input: &str, mut index: usize) -> usize {
    while index > 0 && !input.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn first_sql_keyword(input: &str) -> Option<String> {
    strip_leading_sql_comments(input)
        .trim_start()
        .split(|character: char| !character.is_ascii_alphabetic())
        .find(|value| !value.is_empty())
        .map(str::to_ascii_uppercase)
}

fn strip_leading_sql_comments(mut input: &str) -> &str {
    loop {
        input = input.trim_start();
        if let Some(rest) = input.strip_prefix("--").or_else(|| input.strip_prefix('#')) {
            input = rest.split_once('\n').map_or("", |(_, remaining)| remaining);
            continue;
        }
        if let Some(rest) = input.strip_prefix("/*") {
            input = rest.split_once("*/").map_or("", |(_, remaining)| remaining);
            continue;
        }
        return input;
    }
}

fn source_has_dynamic_query(input: &str, language: SourceLanguage) -> bool {
    input.lines().any(|line| {
        let lower = line.to_ascii_lowercase();
        let query_call = ["query(", "execute(", "queryrow(", "exec(", ".sql(", "raw("]
            .iter()
            .any(|marker| lower.contains(marker));
        if !query_call {
            return false;
        }
        let has_quote = line.contains('"') || line.contains('\'') || line.contains('`');
        !has_quote
            || line.contains('+')
            || line.contains("${")
            || (language == SourceLanguage::Python && (line.contains("f\"") || line.contains("f'")))
    })
}

fn owner_at_line(language: SourceLanguage, input: &str, line: u32) -> Option<String> {
    let lines = input
        .lines()
        .take(usize::try_from(line).ok()?)
        .collect::<Vec<_>>();
    lines.iter().rev().find_map(|line| {
        let trimmed = line.trim();
        match language {
            SourceLanguage::Rust => function_name_after(trimmed, "fn "),
            SourceLanguage::Python => function_name_after(trimmed, "def "),
            SourceLanguage::JavaScript | SourceLanguage::TypeScript => {
                function_name_after(trimmed, "function ").or_else(|| {
                    trimmed
                        .split_once('=')
                        .filter(|(_, right)| right.contains("=>"))
                        .map(|(left, _)| bounded_identifier(left.trim()))
                })
            }
            SourceLanguage::Go => function_name_after(trimmed, "func "),
            SourceLanguage::Java => java_method_name(trimmed),
        }
    })
}

fn function_name_after(line: &str, marker: &str) -> Option<String> {
    let rest = line.strip_prefix(marker)?;
    let name = rest
        .split(|character: char| character == '(' || character.is_whitespace())
        .next()?;
    (!name.is_empty()).then(|| bounded_identifier(name))
}

fn java_method_name(line: &str) -> Option<String> {
    if !line.ends_with('{') || !line.contains('(') {
        return None;
    }
    let before = line.split_once('(')?.0;
    before
        .split_whitespace()
        .next_back()
        .map(bounded_identifier)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_schema_extracts_columns_indexes_and_foreign_keys() {
        let input = r"
            CREATE TABLE public.users (
                id BIGINT PRIMARY KEY,
                email TEXT NOT NULL UNIQUE,
                password_hash TEXT DEFAULT 'secret-default'
            );
            CREATE TABLE public.orders (
                id BIGINT PRIMARY KEY,
                user_id BIGINT,
                CONSTRAINT fk_user FOREIGN KEY (user_id) REFERENCES public.users(id)
            );
            CREATE INDEX idx_orders_user ON public.orders(user_id);
        ";

        let document = extract_data_artifact("db/schema.sql", input).expect("schema should parse");

        assert!(
            document.tables.iter().any(|table| {
                table.name == "orders" && table.indexes.len() == 1 && table.foreign_keys.len() == 1
            }),
            "orders table should retain index and foreign key"
        );
    }

    #[test]
    fn sql_migration_preserves_numeric_order_hint() {
        let document = extract_data_artifact(
            "migrations/0042_add_users.sql",
            "CREATE TABLE users (id INTEGER);",
        )
        .expect("migration should parse");

        assert_eq!(
            document
                .migration
                .and_then(|migration| migration.order_hint),
            Some(42)
        );
    }

    #[test]
    fn prisma_extracts_mapped_model_and_column() {
        let input = r#"
            model User {
              id Int @id
              displayName String @map("display_name")
              posts Post[] @relation(fields: [id], references: [userId])
              @@map("app_users")
              @@schema("tenant")
            }
        "#;

        let document =
            extract_data_artifact("prisma/schema.prisma", input).expect("Prisma should parse");

        assert!(document.tables.iter().any(|table| {
            table.name == "app_users"
                && table.schema.as_deref() == Some("tenant")
                && table
                    .columns
                    .iter()
                    .any(|column| column.name == "display_name")
        }));
    }

    #[test]
    fn alembic_extracts_revision_table_and_added_column() {
        let input = r#"
revision = "abc123"
down_revision = "abc122"
def upgrade():
    op.create_table(
        "users",
        sa.Column("id", sa.Integer(), primary_key=True),
    )
    op.add_column("users", sa.Column("email", sa.String(), nullable=False))
def downgrade():
    op.drop_table("users")
"#;

        let document = extract_data_artifact("alembic/versions/0042_users.py", input)
            .expect("Alembic should parse");

        assert!(document.tables.iter().any(|table| {
            table.name == "users" && table.columns.iter().any(|column| column.name == "email")
        }));
    }

    #[test]
    fn sqlalchemy_extracts_model_binding_and_foreign_key() {
        let input = r#"
class Order(Base):
    __tablename__ = "orders"
    __table_args__ = {"schema": "billing"}
    id = Column(Integer, primary_key=True)
    user_id = Column(Integer, ForeignKey("public.users.id"), nullable=False)
"#;

        let document = extract_data_artifact("models.py", input).expect("SQLAlchemy should parse");

        assert!(
            document.accesses.iter().any(|access| {
                access.role == DataAccessRole::ModelBinding
                    && access.owner.as_deref() == Some("Order")
                    && access.table == "billing.orders"
            }) && document.tables.iter().any(|table| {
                table.columns.iter().any(|column| column.name == "id")
                    && table.foreign_keys.iter().any(|foreign| {
                        foreign.columns == ["user_id"] && foreign.referenced_table == "public.users"
                    })
            })
        );
    }

    #[test]
    fn sqlalchemy_nested_enum_does_not_replace_owning_model() {
        let input = r#"
class Services(Base):
    class EnumServiceType(str, enum.Enum):
        PUBLIC = "PUBLIC"

    __tablename__ = "services"
    id = Column(Integer, primary_key=True)
"#;

        let document = extract_data_artifact("models.py", input).expect("SQLAlchemy should parse");

        assert!(document.accesses.iter().any(|access| {
            access.role == DataAccessRole::ModelBinding
                && access.owner.as_deref() == Some("Services")
                && access.table == "services"
        }));
        assert!(
            !document
                .owners
                .iter()
                .any(|owner| owner == "EnumServiceType")
        );
    }

    #[test]
    fn diesel_extracts_table_macro() {
        let input = r"
diesel::table! {
    public.users (id) {
        id -> Int8,
        email -> Text,
        nickname -> Nullable<Text>,
    }
}
";

        let document = extract_data_artifact("src/schema.rs", input).expect("Diesel should parse");

        assert!(document.tables.iter().any(|table| {
            table.name == "users"
                && table.schema.as_deref() == Some("public")
                && table
                    .columns
                    .iter()
                    .any(|column| column.name == "nickname" && column.nullable == Some(true))
        }));
    }

    #[test]
    fn literal_select_records_only_direct_tables() {
        let input = r#"
fn load(pool: &Pool) {
    sqlx::query("SELECT u.id FROM users u JOIN teams t ON t.id = u.team_id");
}
"#;

        let document = parse_literal_sql_source(SourceLanguage::Rust, "src/users.rs", input);

        assert_eq!(
            document
                .accesses
                .iter()
                .map(|access| (&access.role, access.table.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (&DataAccessRole::Reader, "teams"),
                (&DataAccessRole::Reader, "users")
            ]
        );
    }

    #[test]
    fn sqlx_inline_apis_cover_functions_macros_raw_strings_and_raw_sql() {
        let input = r##"
use sqlx::Executor;

async fn load(pool: &sqlx::PgPool) {
    sqlx::query!(r#"SELECT id FROM users WHERE id = $1"#, 7_i64);
    sqlx::query_as_unchecked!(User, "UPDATE users SET active = true");
    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM teams");
    sqlx::raw_sql("DELETE FROM sessions; INSERT INTO audits(id) VALUES (1);");
    pool.fetch_one("SELECT id FROM accounts");
}
"##;

        let document = parse_literal_sql_source(SourceLanguage::Rust, "src/users.rs", input);
        let facts = document
            .accesses
            .iter()
            .map(|access| (access.role, access.operation, access.table.as_str()))
            .collect::<BTreeSet<_>>();

        assert!(document.frameworks.contains(&DataFramework::Sqlx));
        assert!(facts.contains(&(DataAccessRole::Reader, Some(DataOperation::Select), "users")));
        assert!(facts.contains(&(DataAccessRole::Writer, Some(DataOperation::Update), "users")));
        assert!(facts.contains(&(DataAccessRole::Reader, Some(DataOperation::Select), "teams")));
        assert!(facts.contains(&(
            DataAccessRole::Writer,
            Some(DataOperation::Delete),
            "sessions"
        )));
        assert!(facts.contains(&(
            DataAccessRole::Writer,
            Some(DataOperation::Insert),
            "audits"
        )));
        assert!(facts.contains(&(
            DataAccessRole::Reader,
            Some(DataOperation::Select),
            "accounts"
        )));
    }

    #[test]
    fn rust_sqlx_examples_in_comments_and_string_contents_are_not_evidence() {
        let input = r##"
fn example() {
    // sqlx::query!("SELECT id FROM leaked_comment");
    let documentation = r#"sqlx::query!("SELECT id FROM leaked_string")"#;
}
"##;

        let document = parse_literal_sql_source(SourceLanguage::Rust, "src/example.rs", input);

        assert!(
            document.accesses.is_empty()
                && document.frameworks.is_empty()
                && document.references.is_empty()
        );
    }

    #[test]
    fn mysql_async_queryable_methods_cover_text_prepared_and_streaming_apis() {
        let input = r##"
use mysql_async::prelude::Queryable as DbQueryable;

async fn manage(conn: &mut mysql_async::Conn) {
    conn.query_iter(r#"SELECT id FROM users"#).await?;
    conn.query_first(b"SELECT id FROM teams").await?;
    conn.query_map("SELECT id FROM accounts", |row| row).await?;
    conn.query_fold("SELECT id FROM audits", (), |_, _| ()).await?;
    conn.query_drop("DELETE FROM sessions").await?;
    conn.exec_iter("INSERT INTO events(id) VALUES (1)", ()).await?;
    conn.exec_batch("UPDATE jobs SET active = true", [()]).await?;
    DbQueryable::exec_drop(conn, "DELETE FROM tokens", ()).await?;
    conn.query_stream("SELECT id FROM streams").await?;
    conn.exec_stream("SELECT id FROM prepared_streams", ()).await?;
    conn.prep("UPDATE prepared_jobs SET active = true").await?;
}
"##;

        let document = parse_literal_sql_source(SourceLanguage::Rust, "src/mysql.rs", input);
        let facts = document
            .accesses
            .iter()
            .map(|access| (access.role, access.operation, access.table.as_str()))
            .collect::<BTreeSet<_>>();

        assert_eq!(document.frameworks, vec![DataFramework::MysqlAsync]);
        assert!(facts.contains(&(DataAccessRole::Reader, Some(DataOperation::Select), "users")));
        assert!(facts.contains(&(DataAccessRole::Reader, Some(DataOperation::Select), "teams")));
        assert!(facts.contains(&(
            DataAccessRole::Writer,
            Some(DataOperation::Delete),
            "sessions"
        )));
        assert!(facts.contains(&(
            DataAccessRole::Writer,
            Some(DataOperation::Insert),
            "events"
        )));
        assert!(facts.contains(&(DataAccessRole::Writer, Some(DataOperation::Update), "jobs")));
        assert!(facts.contains(&(
            DataAccessRole::Writer,
            Some(DataOperation::Update),
            "prepared_jobs"
        )));
    }

    #[test]
    fn mysql_async_fluent_query_traits_cover_aliases_params_and_batch() {
        let input = r##"
use mysql_async::prelude::{
    BatchQuery,
    Query as DbQuery,
    WithParams,
};

async fn fluent(conn: &mysql_async::Pool) {
    "SELECT id FROM users".first(conn).await?;
    br#"SELECT id FROM teams"#.fetch(conn).await?;
    "UPDATE jobs SET active = true".with(()).ignore(conn).await?;
    "INSERT INTO audits(id) VALUES (1)".with([()]).batch(conn).await?;
    DbQuery::run("DELETE FROM sessions", conn).await?;
}
"##;

        let document = parse_literal_sql_source(SourceLanguage::Rust, "src/fluent.rs", input);
        let facts = document
            .accesses
            .iter()
            .map(|access| (access.role, access.operation, access.table.as_str()))
            .collect::<BTreeSet<_>>();

        assert!(document.frameworks.contains(&DataFramework::MysqlAsync));
        assert_eq!(
            facts,
            BTreeSet::from([
                (DataAccessRole::Reader, Some(DataOperation::Select), "teams"),
                (DataAccessRole::Reader, Some(DataOperation::Select), "users"),
                (
                    DataAccessRole::Writer,
                    Some(DataOperation::Delete),
                    "sessions"
                ),
                (
                    DataAccessRole::Writer,
                    Some(DataOperation::Insert),
                    "audits"
                ),
                (DataAccessRole::Writer, Some(DataOperation::Update), "jobs"),
            ])
        );
    }

    #[test]
    fn mysql_async_examples_in_comments_and_string_contents_are_not_evidence() {
        let input = r##"
fn example() {
    // use mysql_async::prelude::*;
    // conn.query("SELECT id FROM leaked_comment");
    let documentation = r#"conn.exec_drop("DELETE FROM leaked_string", ())"#;
}
"##;

        let document = parse_literal_sql_source(SourceLanguage::Rust, "src/example.rs", input);

        assert!(document.accesses.is_empty() && document.frameworks.is_empty());
    }

    #[test]
    fn sqlx_query_file_macro_matrix_resolves_from_cargo_crate_root() {
        let input = r#"
async fn load() {
    sqlx::query_file!("queries/one.sql");
    sqlx::query_file_unchecked!("queries/two.sql");
    sqlx::query_file_as!(User, "queries/three.sql");
    sqlx::query_file_as_unchecked!(User, "queries/four.sql");
    sqlx::query_file_scalar!("queries/five.sql");
    sqlx::query_file_scalar_unchecked!("queries/six.sql");
}
"#;

        let document = parse_literal_sql_source_at_root(
            SourceLanguage::Rust,
            "crates/api/src/users.rs",
            "crates/api",
            input,
        );

        assert_eq!(
            document
                .references
                .iter()
                .map(|reference| reference.path.as_str())
                .collect::<Vec<_>>(),
            vec![
                "crates/api/queries/five.sql",
                "crates/api/queries/four.sql",
                "crates/api/queries/one.sql",
                "crates/api/queries/six.sql",
                "crates/api/queries/three.sql",
                "crates/api/queries/two.sql",
            ]
        );
        assert!(document.references.iter().all(|reference| {
            reference.framework == DataFramework::Sqlx
                && reference.kind == DataArtifactReferenceKind::QueryFile
                && reference.owner.as_deref() == Some("load")
        }));
    }

    #[test]
    fn sqlx_migration_apis_resolve_default_custom_and_runtime_directories() {
        let input = r#"
use sqlx::migrate::Migrator;

static DEFAULT: Migrator = sqlx::migrate!();
static CUSTOM: Migrator = sqlx::migrate!("db/migrations");

async fn runtime() {
    Migrator::new(std::path::Path::new("tenant/migrations")).await;
}
"#;

        let document = parse_literal_sql_source_at_root(
            SourceLanguage::Rust,
            "crates/api/src/lib.rs",
            "crates/api",
            input,
        );

        assert_eq!(
            document
                .references
                .iter()
                .filter(|reference| {
                    reference.kind == DataArtifactReferenceKind::MigrationDirectory
                })
                .map(|reference| reference.path.as_str())
                .collect::<Vec<_>>(),
            vec!["crates/api/db/migrations", "crates/api/tenant/migrations",]
        );
        assert!(document.references.iter().any(|reference| {
            reference.kind == DataArtifactReferenceKind::SqlxDefaultMigrationDirectory
                && reference.path == "crates/api"
        }));
    }

    #[test]
    fn sqlx_configuration_extracts_migration_directory_without_leaking_other_values() {
        let document = extract_data_artifact(
            "crates/api/sqlx.toml",
            r#"
[database]
url = "postgres://secret@example.invalid/private"

[migrate]
table-name = "app._sqlx_migrations"
migrations-dir = "db/migrations" # relative to the crate root

[migrate.defaults]
migration-type = "reversible"
"#,
        )
        .expect("SQLx configuration should parse");

        assert_eq!(document.artifact_kind, DataArtifactKind::SqlxConfiguration);
        assert_eq!(document.frameworks, vec![DataFramework::Sqlx]);
        assert_eq!(document.references.len(), 1);
        assert_eq!(
            document.references[0].kind,
            DataArtifactReferenceKind::MigrationDirectory
        );
        assert_eq!(document.references[0].path, "crates/api/db/migrations");
        assert!(!format!("{document:?}").contains("secret"));
    }

    #[test]
    fn sqlx_import_alias_and_query_builder_are_recognized_conservatively() {
        let input = r#"
use sqlx::{query as sql_query, QueryBuilder as SqlBuilder};

fn load() {
    sql_query("SELECT id FROM users");
    let mut builder = SqlBuilder::<sqlx::Postgres>::new("SELECT id FROM teams");
    builder.push(" WHERE id = ").push_bind(7_i64);
}
"#;

        let document = parse_literal_sql_source(SourceLanguage::Rust, "src/users.rs", input);

        assert!(
            document
                .accesses
                .iter()
                .any(|access| { access.table == "users" && access.role == DataAccessRole::Reader })
        );
        assert!(
            document
                .accesses
                .iter()
                .any(|access| { access.table == "teams" && access.role == DataAccessRole::Reader })
        );
        assert!(
            document.incomplete
                && document.warnings.contains(&DataWarning::DynamicQuery)
                && document.frameworks == vec![DataFramework::Sqlx]
        );
    }

    #[test]
    fn standalone_query_files_are_not_misclassified_as_migrations() {
        let document = extract_data_artifact(
            "queries/find_users.sql",
            "SELECT id FROM users WHERE active = true;",
        )
        .expect("query file should parse");

        assert!(
            document.artifact_kind == DataArtifactKind::SqlQueryFile
                && document.migration.is_none()
                && document.accesses.iter().any(|access| {
                    access.table == "users" && access.role == DataAccessRole::Reader
                })
        );
    }

    #[test]
    fn sqlx_reversible_migration_filename_sets_version_metadata() {
        let document = extract_data_artifact(
            "migrations/0042_add_users.up.sql",
            "CREATE TABLE users (id INTEGER);",
        )
        .expect("migration should parse");
        let migration = document.migration.expect("migration metadata");

        assert!(
            migration.order_hint == Some(42)
                && migration.revision.as_deref() == Some("42")
                && migration.reversible
        );
    }

    #[test]
    fn literal_write_records_writer_role() {
        let input = r#"
async def save(conn):
    await conn.execute("UPDATE users SET active = ? WHERE id = ?", True, 7)
"#;

        let document = parse_literal_sql_source(SourceLanguage::Python, "users.py", input);

        assert!(document.accesses.iter().any(|access| {
            access.role == DataAccessRole::Writer
                && access.operation == Some(DataOperation::Update)
                && access.table == "users"
        }));
    }

    #[test]
    fn pymysql_wrapper_f_string_preserves_static_table_access() {
        let input = r#"
from src.clases.Database import Database

def load(account_id):
    return Database.sql(f"SELECT ID FROM accounts WHERE ID = {account_id}")
"#;

        let document = parse_literal_sql_source(SourceLanguage::Python, "accounts.py", input);

        assert!(document.accesses.iter().any(|access| {
            access.role == DataAccessRole::Reader
                && access.operation == Some(DataOperation::Select)
                && access.table == "accounts"
        }));
        assert!(
            document.incomplete && document.warnings.contains(&DataWarning::DynamicQuery),
            "interpolated values must remain explicitly incomplete"
        );
    }

    #[test]
    fn pymysql_wrapper_does_not_promote_interpolated_table_names() {
        let input = r#"
from src.clases.Database import Database

def load(table):
    return Database.sql(f"SELECT ID FROM {table} WHERE active = 1")
"#;

        let document = parse_literal_sql_source(SourceLanguage::Python, "accounts.py", input);

        assert!(
            document.accesses.is_empty()
                && document.incomplete
                && document.warnings.contains(&DataWarning::DynamicQuery)
        );
    }

    #[test]
    fn psycopg_format_values_preserve_static_table_access() {
        let input = r#"
import psycopg2

def search(cursor, value):
    cursor.execute("""
        SELECT id FROM embeddings WHERE content = '{}'
    """.format(value))
"#;

        let document = parse_literal_sql_source(SourceLanguage::Python, "vectors.py", input);

        assert!(document.frameworks.contains(&DataFramework::Psycopg));
        assert!(document.accesses.iter().any(|access| {
            access.role == DataAccessRole::Reader
                && access.table == "embeddings"
                && access.owner.as_deref() == Some("search")
        }));
        assert!(document.warnings.contains(&DataWarning::DynamicQuery));
    }

    #[test]
    fn sqlalchemy_source_records_model_reads_and_writes_without_guessing_tables() {
        let input = r#"
from sqlalchemy.orm import Session
from app import models

def create(db: Session):
    service = models.Services(name="test")
    db.add(service)

def list_all(db: Session):
    return db.query(models.Services).all()
"#;

        let document = parse_literal_sql_source(SourceLanguage::Python, "controllers.py", input);

        assert!(document.frameworks.contains(&DataFramework::SqlAlchemy));
        assert!(document.accesses.iter().any(|access| {
            access.role == DataAccessRole::Writer
                && access.model.as_deref() == Some("Services")
                && access.operation == Some(DataOperation::Insert)
        }));
        assert!(document.accesses.iter().any(|access| {
            access.role == DataAccessRole::Reader
                && access.model.as_deref() == Some("Services")
                && access.operation == Some(DataOperation::Select)
        }));
        assert!(
            document
                .accesses
                .iter()
                .all(|access| access.table.is_empty())
        );
    }

    #[test]
    fn mariadb_dump_recovers_create_tables_after_unsupported_statements() {
        let input = r"
/*!40101 SET NAMES utf8mb4 */;
CREATE DATABASE IF NOT EXISTS `processor`;
USE `processor`;

CREATE TABLE IF NOT EXISTS `accounts` (
  `ID` int(10) unsigned NOT NULL AUTO_INCREMENT,
  `number` int(10) unsigned NOT NULL DEFAULT 0,
  PRIMARY KEY (`ID`),
  UNIQUE KEY `number_unique` (`number`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

LOCK TABLES `accounts` WRITE;
";

        let document =
            extract_data_artifact("sql/base_embozador.sql", input).expect("dump should recover");

        assert!(document.tables.iter().any(|table| {
            table.name == "accounts"
                && table.columns.iter().any(|column| column.name == "ID")
                && table.columns.iter().any(|column| column.name == "number")
        }));
        assert!(
            document.incomplete
                && document.warnings.iter().any(|warning| {
                    matches!(
                        warning,
                        DataWarning::SqlParseRecovery | DataWarning::UnsupportedConstruct
                    )
                }),
            "recovered dumps must retain the fact that unsupported statements were skipped"
        );
    }

    #[test]
    fn literal_queries_cover_remaining_source_languages() {
        let cases = [
            (
                SourceLanguage::JavaScript,
                r#"function load() { db.query("SELECT id FROM users"); }"#,
            ),
            (
                SourceLanguage::TypeScript,
                r#"const load = () => db.query("SELECT id FROM users");"#,
            ),
            (
                SourceLanguage::Go,
                "func load() { db.Query(`SELECT id FROM users`) }",
            ),
            (
                SourceLanguage::Java,
                r#"void load() { statement.executeQuery("SELECT id FROM users"); }"#,
            ),
        ];

        assert!(cases.into_iter().all(|(language, source)| {
            parse_literal_sql_source(language, "source.file", source)
                .accesses
                .iter()
                .any(|access| access.table == "users" && access.role == DataAccessRole::Reader)
        }));
    }

    #[test]
    fn literal_query_context_should_preserve_utf8_boundaries() {
        let input = format!(
            "{} db.query(\"SELECT id FROM users\") {}",
            "─".repeat(70),
            "─".repeat(40)
        );

        let document = parse_literal_sql_source(SourceLanguage::JavaScript, "src/users.js", &input);

        assert!(
            document
                .accesses
                .iter()
                .any(|access| access.table == "users")
        );
    }

    #[test]
    fn dynamic_query_is_incomplete_and_unlinked() {
        let input = r#"
fn load(pool: &Pool, table: &str) {
    let sql = "SELECT * FROM ".to_owned() + table;
    sqlx::query(&sql);
}
"#;

        let document = parse_literal_sql_source(SourceLanguage::Rust, "src/dynamic.rs", input);

        assert!(
            document.incomplete
                && document.accesses.is_empty()
                && document.warnings.contains(&DataWarning::DynamicQuery)
        );
    }

    #[test]
    fn serialization_does_not_persist_secrets_or_sql_bodies() {
        let input = r"
-- postgres://admin:credential@localhost/private
CREATE TABLE users (
    id INTEGER,
    token TEXT DEFAULT 'top-secret-token'
);
";
        let document =
            extract_data_artifact("schema.sql", input).expect("schema should parse safely");
        let serialized = serde_json::to_string(&document).expect("document should serialize");

        assert!(
            !serialized.contains("credential")
                && !serialized.contains("top-secret-token")
                && !serialized.contains("CREATE TABLE")
                && !serialized.contains("postgres://")
        );
    }

    #[test]
    fn literal_query_serialization_discards_values_and_body() {
        let input = r#"db.query("SELECT id FROM users WHERE token = 'literal-secret-value'")"#;
        let document = parse_literal_sql_source(SourceLanguage::JavaScript, "src/users.js", input);
        let serialized = serde_json::to_string(&document).expect("document should serialize");

        assert!(
            !serialized.contains("literal-secret-value")
                && !serialized.contains("SELECT id")
                && serialized.contains("users")
        );
    }

    #[test]
    fn parser_recovery_is_incomplete_without_echoing_secret_input() {
        let document = extract_data_artifact(
            "schema.sql",
            "CREATE TABLE secret (token DEFAULT 'do-not-echo'",
        )
        .expect("unsupported SQL syntax should degrade");
        let serialized = serde_json::to_string(&document).expect("document should serialize");

        assert!(
            document.incomplete
                && document.warnings.contains(&DataWarning::SqlParseRecovery)
                && !serialized.contains("do-not-echo")
        );
    }

    #[test]
    fn output_is_deterministic_and_deduplicated() {
        let input = "CREATE TABLE b (id INT); CREATE TABLE a (id INT);";
        let first = extract_data_artifact("schema.sql", input).expect("schema should parse");
        let second = extract_data_artifact("schema.sql", input).expect("schema should parse");

        assert_eq!(first, second);
    }

    #[test]
    fn unsupported_filename_is_rejected_without_path_echo() {
        let error = extract_data_artifact("secrets/config.txt", "password=private")
            .expect_err("unsupported file should fail");

        assert_eq!(error, DataExtractionError::UnsupportedArtifact);
    }

    #[test]
    fn oversized_input_returns_bounded_incomplete_document() {
        let input = "x".repeat(MAX_DATA_INPUT_BYTES + 1);
        let document =
            extract_data_artifact("schema.sql", &input).expect("oversized SQL should degrade");

        assert!(
            document.incomplete
                && document.warnings == vec![DataWarning::LimitExceeded]
                && document.tables.is_empty()
        );
    }

    #[test]
    fn oversized_schema_retains_tables_and_truncates_details() {
        let mut document = empty_document("schema.sql", DataArtifactKind::DeclarativeSqlSchema);
        for table_index in 0..5 {
            let mut table = table_from_qualified_text(
                &format!("table_{table_index}"),
                evidence(table_index + 1),
            );
            table.columns = (0..1_000)
                .map(|column_index| DatabaseColumn {
                    name: format!("column_{column_index}"),
                    data_type: Some("integer".to_owned()),
                    nullable: None,
                    primary_key: false,
                    unique: false,
                    default_present: false,
                    evidence: evidence(table_index + 1),
                })
                .collect();
            document.tables.push(table);
        }

        finish_document(&mut document);

        assert!(
            document.tables.len() == 5
                && document
                    .tables
                    .iter()
                    .all(|table| table.name.starts_with("table_"))
                && structured_item_count(&document) == MAX_DATA_ITEMS
                && document.incomplete
                && document.warnings.contains(&DataWarning::LimitExceeded)
        );
    }

    #[test]
    fn evidence_line_enforces_bounds() {
        assert!(DataEvidenceLine::new(0).is_err());
        assert!(DataEvidenceLine::new(MAX_DATA_SOURCE_LINES).is_ok());
        assert!(DataEvidenceLine::new(MAX_DATA_SOURCE_LINES + 1).is_err());
    }
}

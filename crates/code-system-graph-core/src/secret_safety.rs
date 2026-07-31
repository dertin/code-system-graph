//! Secret-safe extraction of configuration key names and nested key paths.

use std::collections::BTreeMap;

use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_INPUT_BYTES: usize = 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_KEYS: usize = 4096;
const MAX_KEY_BYTES: usize = 256;
const PARTIAL_YAML_WARNING: &str =
    "unsupported YAML constructs were omitted from key-path extraction";
const PARTIAL_TOML_WARNING: &str =
    "unsupported TOML inline structures were omitted from key-path extraction";
const SAFE_URI_SCHEMES: [&str; 6] = [
    "vault://",
    "secret://",
    "secretsmanager://",
    "aws-secretsmanager://",
    "gcp-secret-manager://",
    "azure-key-vault://",
];

/// Supported secret-safe configuration artifact format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigArtifactKind {
    /// A dotenv assignment file.
    Dotenv,
    /// A YAML mapping document.
    Yaml,
    /// A JSON object or array document.
    Json,
    /// A TOML table document.
    Toml,
}

/// Semantic category assigned to a configuration key whose name suggests sensitive data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensitiveKeyKind {
    /// Authentication or authorization token material.
    Token,
    /// A password, passphrase, or abbreviated password field.
    Password,
    /// Generic secret material.
    Secret,
    /// A private signing, SSH, or TLS key.
    PrivateKey,
    /// Credentials or abbreviated credential material.
    Credential,
    /// An access key or API key.
    AccessKey,
    /// A connection string.
    ConnectionString,
    /// A database URL or equivalent database locator.
    DatabaseUrl,
}

/// One configuration key observation containing no configuration value.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SafeConfigKey {
    /// Parent key path, ordered from the document root to the immediate parent.
    pub scope: Vec<String>,
    /// Unqualified key name.
    pub name: String,
    /// One-based source line where the key is declared.
    pub line: usize,
    /// Sensitive-name classification, when applicable.
    pub sensitive_kind: Option<SensitiveKeyKind>,
}

/// Owned, serializable configuration metadata that never contains configuration values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafeConfigDocument {
    /// Caller-supplied source path.
    pub source_path: String,
    /// Format selected from the source path.
    pub artifact_kind: ConfigArtifactKind,
    /// Deduplicated keys in deterministic path order.
    pub keys: Vec<SafeConfigKey>,
    /// Bounded, value-free extraction diagnostics.
    pub warnings: Vec<String>,
    /// Whether unsupported constructs prevented complete key-path extraction.
    pub incomplete: bool,
}

/// Value-free failure returned by secret-safe configuration extraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigExtractionError {
    /// The source path does not identify a supported configuration format.
    #[error("unsupported configuration artifact")]
    UnsupportedArtifact,
    /// Input exceeds the fixed extraction budget.
    #[error("configuration input is {actual} bytes; maximum is {maximum}")]
    InputTooLarge {
        /// Observed input size.
        actual: usize,
        /// Fixed maximum input size.
        maximum: usize,
    },
    /// Nesting exceeds the fixed extraction budget.
    #[error("configuration nesting at line {line} exceeds maximum depth {maximum}")]
    DepthLimitExceeded {
        /// One-based line where the limit was exceeded.
        line: usize,
        /// Fixed maximum nesting depth.
        maximum: usize,
    },
    /// The document contains more unique keys than the fixed extraction budget.
    #[error("configuration key count exceeds maximum {maximum}")]
    KeyLimitExceeded {
        /// Fixed maximum unique-key count.
        maximum: usize,
    },
    /// A key name exceeds the fixed extraction budget.
    #[error("configuration key at line {line} exceeds maximum length {maximum}")]
    KeyTooLong {
        /// One-based line containing the key.
        line: usize,
        /// Fixed maximum key length in bytes.
        maximum: usize,
    },
    /// The input is malformed; source content is intentionally omitted.
    #[error("malformed {artifact_kind:?} configuration at line {line}, column {column}")]
    Malformed {
        /// Format whose parser rejected the input.
        artifact_kind: ConfigArtifactKind,
        /// One-based line nearest the malformed construct.
        line: usize,
        /// One-based column nearest the malformed construct.
        column: usize,
    },
}

/// Extracts only configuration key names, nested scopes, and value-free diagnostics.
///
/// The source path selects dotenv, YAML, JSON, or TOML parsing. Keys are deduplicated by
/// `(scope, name)`, retain their earliest declaration line, and are returned in deterministic
/// path order.
///
/// # Errors
///
/// Returns [`ConfigExtractionError`] for unsupported paths, malformed input, or a fixed size,
/// depth, key-count, or key-length budget violation.
pub fn extract_safe_config(
    source_path: &str,
    input: &str,
) -> Result<SafeConfigDocument, ConfigExtractionError> {
    let artifact_kind = artifact_kind(source_path)?;
    if input.len() > MAX_INPUT_BYTES {
        return Err(ConfigExtractionError::InputTooLarge {
            actual: input.len(),
            maximum: MAX_INPUT_BYTES,
        });
    }

    let mut collector = KeyCollector::default();
    let mut warnings = Vec::new();
    match artifact_kind {
        ConfigArtifactKind::Dotenv => parse_dotenv(input, &mut collector)?,
        ConfigArtifactKind::Yaml => parse_yaml(input, &mut collector, &mut warnings)?,
        ConfigArtifactKind::Json => parse_json(input, &mut collector)?,
        ConfigArtifactKind::Toml => parse_toml(input, &mut collector, &mut warnings)?,
    }
    warnings.sort_unstable();
    warnings.dedup();
    let incomplete = !warnings.is_empty();

    Ok(SafeConfigDocument {
        source_path: source_path.to_owned(),
        artifact_kind,
        keys: collector.into_keys(),
        warnings,
        incomplete,
    })
}

/// Classifies a key name using separator-aware and camel-case-aware sensitive-name patterns.
#[must_use]
pub fn classify_sensitive_key(name: &str) -> Option<SensitiveKeyKind> {
    let words = normalized_words(name);
    let compact = words.concat();
    let has = |needle: &str| words.iter().any(|word| word == needle);
    let adjacent = |first: &str, second: &str| {
        words
            .windows(2)
            .any(|pair| pair[0] == first && pair[1] == second)
    };

    if compact.contains("privatekey") || adjacent("private", "key") {
        Some(SensitiveKeyKind::PrivateKey)
    } else if compact.contains("connectionstring")
        || compact.contains("connstr")
        || adjacent("connection", "string")
    {
        Some(SensitiveKeyKind::ConnectionString)
    } else if compact.contains("databaseurl")
        || compact.contains("dburl")
        || compact.contains("jdbcurl")
        || adjacent("database", "url")
    {
        Some(SensitiveKeyKind::DatabaseUrl)
    } else if compact.contains("accesskey")
        || compact.contains("apikey")
        || adjacent("access", "key")
        || adjacent("api", "key")
    {
        Some(SensitiveKeyKind::AccessKey)
    } else if has("password") || has("passwd") || has("passphrase") || has("pwd") {
        Some(SensitiveKeyKind::Password)
    } else if has("credential") || has("credentials") || has("cred") || has("creds") {
        Some(SensitiveKeyKind::Credential)
    } else if has("token") || compact.ends_with("token") || has("authorization") || has("bearer") {
        Some(SensitiveKeyKind::Token)
    } else if has("secret") || compact.ends_with("secret") {
        Some(SensitiveKeyKind::Secret)
    } else {
        None
    }
}

/// Returns whether a literal is a narrowly recognized external secret reference.
///
/// Literal credentials, token-shaped strings, generic URLs, and every URI containing userinfo
/// are rejected. Supported references are environment variables, selected secret-provider URIs,
/// and absolute secret-file paths.
#[must_use]
pub fn is_safe_literal_reference(value: &str) -> bool {
    if value.is_empty()
        || value.trim() != value
        || value.chars().any(char::is_control)
        || uri_contains_userinfo(value)
        || looks_like_secret_literal(value)
    {
        return false;
    }

    if let Some(name) = value
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
    {
        return is_reference_identifier(name);
    }
    if let Some(name) = value.strip_prefix('$') {
        return is_reference_identifier(name);
    }
    if let Some(name) = value.strip_prefix("env:") {
        return is_reference_identifier(name.trim_start_matches("//"));
    }
    if let Some(name) = value
        .strip_prefix("env(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return is_reference_identifier(name);
    }
    if let Some(name) = value
        .strip_prefix("{{")
        .and_then(|rest| rest.strip_suffix("}}"))
        .map(str::trim)
        .and_then(|inner| inner.strip_prefix("env.").or(Some(inner)))
    {
        return is_reference_identifier(name);
    }

    if SAFE_URI_SCHEMES
        .iter()
        .any(|prefix| value.starts_with(prefix))
    {
        let reference = value.split_once("://").map_or("", |(_, rest)| rest);
        return !reference.is_empty()
            && !reference.contains('?')
            && reference
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/._:-#@".contains(&byte));
    }

    value
        .strip_prefix("file:")
        .is_some_and(is_absolute_secret_path)
        || is_absolute_secret_path(value)
}

#[derive(Default)]
struct KeyCollector {
    keys: BTreeMap<(Vec<String>, String), SafeConfigKey>,
}

impl KeyCollector {
    fn insert(
        &mut self,
        scope: &[String],
        name: String,
        line: usize,
    ) -> Result<(), ConfigExtractionError> {
        if name.len() > MAX_KEY_BYTES {
            return Err(ConfigExtractionError::KeyTooLong {
                line,
                maximum: MAX_KEY_BYTES,
            });
        }
        if name.is_empty() {
            return Ok(());
        }
        let identity = (scope.to_vec(), name.clone());
        if let Some(existing) = self.keys.get_mut(&identity) {
            existing.line = existing.line.min(line);
            return Ok(());
        }
        if self.keys.len() >= MAX_KEYS {
            return Err(ConfigExtractionError::KeyLimitExceeded { maximum: MAX_KEYS });
        }
        self.keys.insert(
            identity,
            SafeConfigKey {
                scope: scope.to_vec(),
                sensitive_kind: classify_sensitive_key(&name),
                name,
                line,
            },
        );
        Ok(())
    }

    fn insert_path(
        &mut self,
        base_scope: &[String],
        path: &[String],
        line: usize,
    ) -> Result<(), ConfigExtractionError> {
        let mut scope = base_scope.to_vec();
        for name in path {
            self.insert(&scope, name.clone(), line)?;
            scope.push(name.clone());
            check_depth(scope.len(), line)?;
        }
        Ok(())
    }

    fn into_keys(self) -> Vec<SafeConfigKey> {
        self.keys.into_values().collect()
    }
}

fn artifact_kind(source_path: &str) -> Result<ConfigArtifactKind, ConfigExtractionError> {
    let file_name = source_path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(source_path)
        .to_ascii_lowercase();
    if file_name == ".env" || file_name.starts_with(".env.") {
        return Ok(ConfigArtifactKind::Dotenv);
    }
    match file_name.rsplit_once('.').map(|(_, extension)| extension) {
        Some("env") => Ok(ConfigArtifactKind::Dotenv),
        Some("yaml" | "yml") => Ok(ConfigArtifactKind::Yaml),
        Some("json") => Ok(ConfigArtifactKind::Json),
        Some("toml") => Ok(ConfigArtifactKind::Toml),
        _ => Err(ConfigExtractionError::UnsupportedArtifact),
    }
}

fn parse_dotenv(input: &str, collector: &mut KeyCollector) -> Result<(), ConfigExtractionError> {
    for (index, source_line) in input.lines().enumerate() {
        let line = index + 1;
        let text = source_line.trim();
        if text.is_empty() || text.starts_with('#') {
            continue;
        }
        let assignment = text.strip_prefix("export ").map_or(text, str::trim_start);
        let Some((key, _)) = assignment.split_once('=') else {
            return Err(malformed(ConfigArtifactKind::Dotenv, line, 1));
        };
        let key = key.trim();
        if !is_dotenv_key(key) {
            return Err(malformed(ConfigArtifactKind::Dotenv, line, 1));
        }
        collector.insert(&[], key.to_owned(), line)?;
    }
    Ok(())
}

fn parse_yaml(
    input: &str,
    collector: &mut KeyCollector,
    warnings: &mut Vec<String>,
) -> Result<(), ConfigExtractionError> {
    if let Err(error) = crate::yaml::from_multiple::<IgnoredAny>(input) {
        let location = error.location();
        return Err(malformed(
            ConfigArtifactKind::Yaml,
            location.map_or(1, |location| {
                usize::try_from(location.line()).unwrap_or(usize::MAX)
            }),
            location.map_or(1, |location| {
                usize::try_from(location.column()).unwrap_or(usize::MAX)
            }),
        ));
    }

    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut block_scalar_indent = None;
    for (index, source_line) in input.lines().enumerate() {
        let line = index + 1;
        let indentation = source_line.bytes().take_while(|byte| *byte == b' ').count();
        let trimmed = source_line.trim();
        if let Some(parent_indent) = block_scalar_indent {
            if trimmed.is_empty() || indentation > parent_indent {
                continue;
            }
            block_scalar_indent = None;
        }
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || matches!(trimmed, "---" | "...")
            || trimmed.starts_with('%')
        {
            if trimmed == "---" {
                stack.clear();
            }
            continue;
        }
        if source_line
            .bytes()
            .take_while(u8::is_ascii_whitespace)
            .any(|byte| byte == b'\t')
        {
            return Err(malformed(ConfigArtifactKind::Yaml, line, 1));
        }

        let (effective_indent, candidate) = if let Some(rest) = trimmed.strip_prefix("- ") {
            (indentation.saturating_add(2), rest.trim_start())
        } else if trimmed == "-" {
            continue;
        } else {
            (indentation, trimmed)
        };
        while stack
            .last()
            .is_some_and(|(parent_indent, _)| *parent_indent >= effective_indent)
        {
            stack.pop();
        }
        if candidate.starts_with(['?', '{', '[']) {
            push_warning(warnings, PARTIAL_YAML_WARNING);
            continue;
        }
        let Some(colon) = find_unquoted(candidate, b':') else {
            continue;
        };
        let key_token = candidate[..colon].trim();
        if key_token.is_empty() {
            return Err(malformed(ConfigArtifactKind::Yaml, line, indentation + 1));
        }
        let Some(key) = parse_yaml_key(key_token) else {
            push_warning(warnings, PARTIAL_YAML_WARNING);
            continue;
        };
        let scope = stack
            .iter()
            .map(|(_, name)| name.clone())
            .collect::<Vec<_>>();
        check_depth(scope.len().saturating_add(1), line)?;
        collector.insert(&scope, key.clone(), line)?;

        let value = strip_yaml_comment(candidate[colon + 1..].trim());
        if value.is_empty() {
            stack.push((effective_indent, key));
        } else if value.starts_with(['|', '>']) {
            block_scalar_indent = Some(effective_indent);
        } else if value.starts_with(['{', '[']) || value.starts_with(['&', '*', '!']) {
            push_warning(warnings, PARTIAL_YAML_WARNING);
        }
    }
    Ok(())
}

fn parse_json(input: &str, collector: &mut KeyCollector) -> Result<(), ConfigExtractionError> {
    let mut parser = JsonKeyParser {
        input,
        position: 0,
        line: 1,
        collector,
    };
    parser.parse_value(&[], 0)?;
    parser.skip_whitespace();
    if parser.position != input.len() {
        return Err(parser.error());
    }
    if let Err(error) = serde_json::from_str::<IgnoredAny>(input) {
        return Err(malformed(
            ConfigArtifactKind::Json,
            error.line(),
            error.column(),
        ));
    }
    Ok(())
}

struct JsonKeyParser<'a, 'b> {
    input: &'a str,
    position: usize,
    line: usize,
    collector: &'b mut KeyCollector,
}

impl JsonKeyParser<'_, '_> {
    fn parse_value(&mut self, scope: &[String], depth: usize) -> Result<(), ConfigExtractionError> {
        self.skip_whitespace();
        check_depth(depth, self.line)?;
        match self.current_byte() {
            Some(b'{') => self.parse_object(scope, depth),
            Some(b'[') => self.parse_array(scope, depth),
            Some(b'"') => self.skip_string(),
            Some(_) => self.skip_scalar(),
            None => Err(self.error()),
        }
    }

    fn parse_object(
        &mut self,
        scope: &[String],
        depth: usize,
    ) -> Result<(), ConfigExtractionError> {
        self.advance();
        self.skip_whitespace();
        if self.consume(b'}') {
            return Ok(());
        }
        loop {
            self.skip_whitespace();
            let key_line = self.line;
            let key = self.parse_key_string()?;
            self.skip_whitespace();
            if !self.consume(b':') {
                return Err(self.error());
            }
            self.collector.insert(scope, key.clone(), key_line)?;
            let mut nested_scope = scope.to_vec();
            nested_scope.push(key);
            self.parse_value(&nested_scope, depth.saturating_add(1))?;
            self.skip_whitespace();
            if self.consume(b'}') {
                return Ok(());
            }
            if !self.consume(b',') {
                return Err(self.error());
            }
        }
    }

    fn parse_array(&mut self, scope: &[String], depth: usize) -> Result<(), ConfigExtractionError> {
        self.advance();
        self.skip_whitespace();
        if self.consume(b']') {
            return Ok(());
        }
        loop {
            self.parse_value(scope, depth.saturating_add(1))?;
            self.skip_whitespace();
            if self.consume(b']') {
                return Ok(());
            }
            if !self.consume(b',') {
                return Err(self.error());
            }
        }
    }

    fn parse_key_string(&mut self) -> Result<String, ConfigExtractionError> {
        if self.current_byte() != Some(b'"') {
            return Err(self.error());
        }
        let start = self.position;
        self.skip_string()?;
        serde_json::from_str(&self.input[start..self.position]).map_err(|error| {
            malformed(
                ConfigArtifactKind::Json,
                error.line().saturating_add(self.line.saturating_sub(1)),
                error.column(),
            )
        })
    }

    fn skip_string(&mut self) -> Result<(), ConfigExtractionError> {
        if !self.consume(b'"') {
            return Err(self.error());
        }
        let mut escaped = false;
        while let Some(byte) = self.current_byte() {
            self.advance();
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                return Ok(());
            }
        }
        Err(self.error())
    }

    fn skip_scalar(&mut self) -> Result<(), ConfigExtractionError> {
        let start = self.position;
        while let Some(byte) = self.current_byte() {
            if byte.is_ascii_whitespace() || b",]}".contains(&byte) {
                break;
            }
            self.advance();
        }
        if self.position == start {
            Err(self.error())
        } else {
            Ok(())
        }
    }

    fn skip_whitespace(&mut self) {
        while self
            .current_byte()
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            self.advance();
        }
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.current_byte() == Some(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn current_byte(&self) -> Option<u8> {
        self.input.as_bytes().get(self.position).copied()
    }

    fn advance(&mut self) {
        if self.current_byte() == Some(b'\n') {
            self.line = self.line.saturating_add(1);
        }
        self.position = self.position.saturating_add(1);
    }

    fn error(&self) -> ConfigExtractionError {
        let line_start = self.input[..self.position.min(self.input.len())]
            .rfind('\n')
            .map_or(0, |position| position + 1);
        malformed(
            ConfigArtifactKind::Json,
            self.line,
            self.position.saturating_sub(line_start).saturating_add(1),
        )
    }
}

fn parse_toml(
    input: &str,
    collector: &mut KeyCollector,
    warnings: &mut Vec<String>,
) -> Result<(), ConfigExtractionError> {
    let mut current_scope = Vec::new();
    let mut value_state: Option<TomlValueState> = None;
    for (index, source_line) in input.lines().enumerate() {
        let line = index + 1;
        if let Some(state) = value_state.as_mut() {
            state.scan(source_line);
            if state.is_complete() {
                value_state = None;
            }
            continue;
        }

        let text = strip_toml_comment(source_line).trim();
        if text.is_empty() {
            continue;
        }
        if text.starts_with('[') {
            let array_table = text.starts_with("[[");
            let (open_len, close) = if array_table { (2, "]]") } else { (1, "]") };
            let Some(inner) = text
                .strip_prefix(&text[..open_len])
                .and_then(|rest| rest.strip_suffix(close))
                .map(str::trim)
            else {
                return Err(malformed(ConfigArtifactKind::Toml, line, 1));
            };
            let path = parse_toml_key_path(inner)
                .ok_or_else(|| malformed(ConfigArtifactKind::Toml, line, 1))?;
            check_depth(path.len(), line)?;
            collector.insert_path(&[], &path, line)?;
            current_scope = path;
            continue;
        }

        let Some(equals) = find_unquoted(text, b'=') else {
            return Err(malformed(ConfigArtifactKind::Toml, line, 1));
        };
        let key_text = text[..equals].trim();
        let path = parse_toml_key_path(key_text)
            .ok_or_else(|| malformed(ConfigArtifactKind::Toml, line, 1))?;
        check_depth(current_scope.len().saturating_add(path.len()), line)?;
        collector.insert_path(&current_scope, &path, line)?;

        let value = text[equals + 1..].trim_start();
        if value.is_empty() {
            return Err(malformed(
                ConfigArtifactKind::Toml,
                line,
                equals.saturating_add(2),
            ));
        }
        if value.starts_with('{') {
            push_warning(warnings, PARTIAL_TOML_WARNING);
        }
        let mut state = TomlValueState::default();
        state.scan(value);
        if state.invalid || state.unclosed_single_line_string {
            return Err(malformed(
                ConfigArtifactKind::Toml,
                line,
                equals.saturating_add(2),
            ));
        }
        if !state.is_complete() {
            value_state = Some(state);
        }
    }
    if value_state.is_some() {
        return Err(malformed(
            ConfigArtifactKind::Toml,
            input.lines().count().max(1),
            1,
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TomlQuote {
    Basic,
    Literal,
    MultiBasic,
    MultiLiteral,
}

#[derive(Default)]
struct TomlValueState {
    quote: Option<TomlQuote>,
    escaped: bool,
    square_depth: usize,
    curly_depth: usize,
    invalid: bool,
    unclosed_single_line_string: bool,
}

impl TomlValueState {
    fn scan(&mut self, text: &str) {
        self.unclosed_single_line_string = false;
        let bytes = text.as_bytes();
        let mut position = 0;
        while position < bytes.len() {
            match self.quote {
                Some(TomlQuote::MultiBasic) => {
                    if bytes[position..].starts_with(b"\"\"\"") && !self.escaped {
                        self.quote = None;
                        position += 3;
                        continue;
                    }
                    self.escaped = bytes[position] == b'\\' && !self.escaped;
                    if bytes[position] != b'\\' {
                        self.escaped = false;
                    }
                }
                Some(TomlQuote::MultiLiteral) => {
                    if bytes[position..].starts_with(b"'''") {
                        self.quote = None;
                        position += 3;
                        continue;
                    }
                }
                Some(TomlQuote::Basic) => {
                    if bytes[position] == b'"' && !self.escaped {
                        self.quote = None;
                    }
                    self.escaped = bytes[position] == b'\\' && !self.escaped;
                    if bytes[position] != b'\\' {
                        self.escaped = false;
                    }
                }
                Some(TomlQuote::Literal) => {
                    if bytes[position] == b'\'' {
                        self.quote = None;
                    }
                }
                None => {
                    if bytes[position..].starts_with(b"\"\"\"") {
                        self.quote = Some(TomlQuote::MultiBasic);
                        position += 3;
                        continue;
                    }
                    if bytes[position..].starts_with(b"'''") {
                        self.quote = Some(TomlQuote::MultiLiteral);
                        position += 3;
                        continue;
                    }
                    match bytes[position] {
                        b'"' => self.quote = Some(TomlQuote::Basic),
                        b'\'' => self.quote = Some(TomlQuote::Literal),
                        b'[' => self.square_depth = self.square_depth.saturating_add(1),
                        b']' => {
                            let Some(depth) = self.square_depth.checked_sub(1) else {
                                self.invalid = true;
                                return;
                            };
                            self.square_depth = depth;
                        }
                        b'{' => self.curly_depth = self.curly_depth.saturating_add(1),
                        b'}' => {
                            let Some(depth) = self.curly_depth.checked_sub(1) else {
                                self.invalid = true;
                                return;
                            };
                            self.curly_depth = depth;
                        }
                        b'#' => break,
                        _ => {}
                    }
                }
            }
            position += 1;
        }
        if matches!(self.quote, Some(TomlQuote::Basic | TomlQuote::Literal)) {
            self.unclosed_single_line_string = true;
        }
        self.escaped = false;
    }

    fn is_complete(&self) -> bool {
        self.quote.is_none() && self.square_depth == 0 && self.curly_depth == 0
    }
}

fn parse_toml_key_path(input: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quote = None;
    let mut escaped = false;
    for (position, byte) in input.bytes().enumerate() {
        match quote {
            Some(b'"') => {
                if byte == b'"' && !escaped {
                    quote = None;
                }
                escaped = byte == b'\\' && !escaped;
                if byte != b'\\' {
                    escaped = false;
                }
            }
            Some(b'\'') => {
                if byte == b'\'' {
                    quote = None;
                }
            }
            Some(_) => return None,
            None if matches!(byte, b'"' | b'\'') => quote = Some(byte),
            None if byte == b'.' => {
                parts.push(parse_toml_key_part(input[start..position].trim())?);
                start = position + 1;
            }
            None => {}
        }
    }
    if quote.is_some() {
        return None;
    }
    parts.push(parse_toml_key_part(input[start..].trim())?);
    (!parts.is_empty()).then_some(parts)
}

fn parse_toml_key_part(input: &str) -> Option<String> {
    if input.len() >= 2 && input.starts_with('"') && input.ends_with('"') {
        return parse_toml_basic_quoted(&input[1..input.len() - 1]);
    }
    if input.len() >= 2 && input.starts_with('\'') && input.ends_with('\'') {
        return Some(input[1..input.len() - 1].to_owned());
    }
    (!input.is_empty()
        && input
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
    .then(|| input.to_owned())
}

fn parse_toml_basic_quoted(input: &str) -> Option<String> {
    let mut decoded = String::new();
    let mut chars = input.chars();
    while let Some(character) = chars.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        let escaped = chars.next()?;
        match escaped {
            '"' | '\\' => decoded.push(escaped),
            'b' => decoded.push('\u{0008}'),
            't' => decoded.push('\t'),
            'n' => decoded.push('\n'),
            'f' => decoded.push('\u{000c}'),
            'r' => decoded.push('\r'),
            _ => return None,
        }
    }
    Some(decoded)
}

fn parse_yaml_key(input: &str) -> Option<String> {
    if input.starts_with(['"', '\'']) {
        crate::yaml::from_str::<String>(input).ok()
    } else if input.contains(['[', ']', '{', '}', ',', '&', '*', '!']) {
        None
    } else {
        Some(input.to_owned())
    }
}

fn strip_yaml_comment(input: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (position, byte) in input.bytes().enumerate() {
        match quote {
            Some(b'"') => {
                if byte == b'"' && !escaped {
                    quote = None;
                }
                escaped = byte == b'\\' && !escaped;
                if byte != b'\\' {
                    escaped = false;
                }
            }
            Some(b'\'') => {
                if byte == b'\'' {
                    quote = None;
                }
            }
            None if matches!(byte, b'"' | b'\'') => quote = Some(byte),
            None if byte == b'#'
                && (position == 0
                    || input.as_bytes()[position.saturating_sub(1)].is_ascii_whitespace()) =>
            {
                return input[..position].trim_end();
            }
            Some(_) | None => {}
        }
    }
    input.trim_end()
}

fn strip_toml_comment(input: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (position, byte) in input.bytes().enumerate() {
        match quote {
            Some(b'"') => {
                if byte == b'"' && !escaped {
                    quote = None;
                }
                escaped = byte == b'\\' && !escaped;
                if byte != b'\\' {
                    escaped = false;
                }
            }
            Some(b'\'') => {
                if byte == b'\'' {
                    quote = None;
                }
            }
            None if matches!(byte, b'"' | b'\'') => quote = Some(byte),
            None if byte == b'#' => return &input[..position],
            Some(_) | None => {}
        }
    }
    input
}

fn find_unquoted(input: &str, needle: u8) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    for (position, byte) in input.bytes().enumerate() {
        match quote {
            Some(b'"') => {
                if byte == b'"' && !escaped {
                    quote = None;
                }
                escaped = byte == b'\\' && !escaped;
                if byte != b'\\' {
                    escaped = false;
                }
            }
            Some(b'\'') => {
                if byte == b'\'' {
                    quote = None;
                }
            }
            None if matches!(byte, b'"' | b'\'') => quote = Some(byte),
            None if byte == needle => return Some(position),
            Some(_) | None => {}
        }
    }
    None
}

fn normalized_words(name: &str) -> Vec<String> {
    let mut normalized = String::with_capacity(name.len());
    let mut previous_lower_or_digit = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            if character.is_ascii_uppercase() && previous_lower_or_digit {
                normalized.push(' ');
            }
            normalized.push(character.to_ascii_lowercase());
            previous_lower_or_digit = character.is_ascii_lowercase() || character.is_ascii_digit();
        } else {
            normalized.push(' ');
            previous_lower_or_digit = false;
        }
    }
    normalized.split_whitespace().map(str::to_owned).collect()
}

fn is_dotenv_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn is_reference_identifier(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn uri_contains_userinfo(value: &str) -> bool {
    let Some((scheme, remainder)) = value.split_once("://") else {
        return false;
    };
    if scheme.is_empty()
        || !scheme
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    {
        return false;
    }
    remainder
        .split(['/', '?', '#'])
        .next()
        .is_some_and(|authority| authority.contains('@'))
}

fn looks_like_secret_literal(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if lower.contains("-----begin") && lower.contains("private key-----") {
        return true;
    }
    if [
        "ghp_",
        "github_pat_",
        "sk-",
        "xoxb-",
        "xoxp-",
        "akia",
        "asia",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
    {
        return true;
    }
    let jwt_parts = value.split('.').collect::<Vec<_>>();
    if jwt_parts.len() == 3
        && jwt_parts.iter().all(|part| {
            part.len() >= 8
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'='))
        })
    {
        return true;
    }
    value.len() >= 32
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+' | b'/' | b'=')
        })
        && value.bytes().any(|byte| byte.is_ascii_lowercase())
        && value.bytes().any(|byte| byte.is_ascii_uppercase())
        && value.bytes().any(|byte| byte.is_ascii_digit())
}

fn is_absolute_secret_path(value: &str) -> bool {
    (value.starts_with("/run/secrets/")
        || value.starts_with("/var/run/secrets/")
        || value.starts_with("/etc/secrets/"))
        && value.len()
            > value
                .find("/secrets/")
                .map_or(usize::MAX, |index| index + 9)
        && !value.contains(['\0', '\n', '\r'])
}

fn check_depth(depth: usize, line: usize) -> Result<(), ConfigExtractionError> {
    if depth > MAX_DEPTH {
        Err(ConfigExtractionError::DepthLimitExceeded {
            line,
            maximum: MAX_DEPTH,
        })
    } else {
        Ok(())
    }
}

fn malformed(
    artifact_kind: ConfigArtifactKind,
    line: usize,
    column: usize,
) -> ConfigExtractionError {
    ConfigExtractionError::Malformed {
        artifact_kind,
        line: line.max(1),
        column: column.max(1),
    }
}

fn push_warning(warnings: &mut Vec<String>, warning: &'static str) {
    if !warnings.iter().any(|existing| existing == warning) {
        warnings.push(warning.to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ConfigArtifactKind, ConfigExtractionError, SensitiveKeyKind, classify_sensitive_key, extract_safe_config, is_safe_literal_reference
    };

    const FIXTURE_SECRETS: [&str; 4] = [
        "dotenv-super-secret-9f47",
        "yaml-password-never-retain-61d2",
        "json-token-never-retain-a830",
        "toml-private-key-never-retain-77bc",
    ];

    #[test]
    fn extracts_dotenv_names_without_values() {
        let input = format!(
            "PUBLIC_NAME=mesh\nAPI_TOKEN={}\nAPI_TOKEN=duplicate\n",
            FIXTURE_SECRETS[0]
        );
        let document = extract_safe_config(".env.production", &input).expect("valid dotenv");

        assert_eq!(
            document
                .keys
                .iter()
                .map(|key| (&key.scope, key.name.as_str(), key.line, key.sensitive_kind))
                .collect::<Vec<_>>(),
            vec![
                (&Vec::new(), "API_TOKEN", 2, Some(SensitiveKeyKind::Token)),
                (&Vec::new(), "PUBLIC_NAME", 1, None),
            ]
        );
    }

    #[test]
    fn extracts_nested_yaml_paths_without_values() {
        let input = format!(
            "service:\n  replicas: 2\n  database:\n    password: {}\n",
            FIXTURE_SECRETS[1]
        );
        let document = extract_safe_config("deploy.yaml", &input).expect("valid YAML");

        assert!(document.keys.iter().any(|key| {
            key.scope == ["service", "database"]
                && key.name == "password"
                && key.sensitive_kind == Some(SensitiveKeyKind::Password)
        }));
    }

    #[test]
    fn extracts_nested_json_paths_without_values() {
        let input = format!(
            r#"{{"service":{{"token":"{}","port":8080}}}}"#,
            FIXTURE_SECRETS[2]
        );
        let document = extract_safe_config("app.json", &input).expect("valid JSON");

        assert!(document.keys.iter().any(|key| {
            key.scope == ["service"]
                && key.name == "token"
                && key.sensitive_kind == Some(SensitiveKeyKind::Token)
        }));
    }

    #[test]
    fn extracts_nested_toml_paths_without_values() {
        let input = format!(
            "[service.database]\nprivate_key = \"{}\"\nport = 5432\n",
            FIXTURE_SECRETS[3]
        );
        let document = extract_safe_config("settings.toml", &input).expect("valid TOML");

        assert!(document.keys.iter().any(|key| {
            key.scope == ["service", "database"]
                && key.name == "private_key"
                && key.sensitive_kind == Some(SensitiveKeyKind::PrivateKey)
        }));
    }

    #[test]
    fn serialized_and_debug_documents_never_contain_fixture_values() {
        let fixtures = [
            (".env", format!("TOKEN={}\n", FIXTURE_SECRETS[0])),
            ("secrets.yml", format!("password: {}\n", FIXTURE_SECRETS[1])),
            (
                "secrets.json",
                format!(r#"{{"token":"{}"}}"#, FIXTURE_SECRETS[2]),
            ),
            (
                "secrets.toml",
                format!("private_key = \"{}\"\n", FIXTURE_SECRETS[3]),
            ),
        ];
        let mut representations = Vec::new();
        for (path, input) in &fixtures {
            let document = extract_safe_config(path, input).expect("valid configuration");
            representations.push(serde_json::to_string(&document).expect("serialize JSON"));
            representations.push(serde_saphyr::to_string(&document).expect("serialize YAML"));
            representations.push(format!("{document:?}"));
        }
        let representations = representations.join("\n");

        assert!(
            FIXTURE_SECRETS
                .iter()
                .all(|secret| !representations.contains(secret))
        );
    }

    #[test]
    fn serialized_display_and_debug_errors_never_contain_fixture_values() {
        let fixtures = [
            (".env", format!("INVALID {}\n", FIXTURE_SECRETS[0])),
            (
                "secrets.yml",
                format!("password: [{}\n", FIXTURE_SECRETS[1]),
            ),
            (
                "secrets.json",
                format!("{{\"token\":\"{}\",", FIXTURE_SECRETS[2]),
            ),
            (
                "secrets.toml",
                format!("private_key = \"{}\n", FIXTURE_SECRETS[3]),
            ),
        ];
        let mut representations = Vec::new();
        for (path, input) in &fixtures {
            let error = extract_safe_config(path, input).expect_err("malformed configuration");
            representations.push(serde_json::to_string(&error).expect("serialize error"));
            representations.push(serde_saphyr::to_string(&error).expect("serialize error"));
            representations.push(format!("{error:?}"));
            representations.push(error.to_string());
        }
        let representations = representations.join("\n");

        assert!(
            FIXTURE_SECRETS
                .iter()
                .all(|secret| !representations.contains(secret))
        );
    }

    #[test]
    fn malformed_errors_are_value_free_and_located() {
        let error =
            extract_safe_config("broken.json", "{\"password\":}").expect_err("malformed JSON");

        assert!(matches!(
            error,
            ConfigExtractionError::Malformed {
                artifact_kind: ConfigArtifactKind::Json,
                line: 1,
                column: _
            }
        ));
    }

    #[test]
    fn fixed_input_and_depth_limits_fail_closed() {
        let oversized = "x".repeat(super::MAX_INPUT_BYTES + 1);
        let deeply_nested = format!(
            "{}0{}",
            "[".repeat(super::MAX_DEPTH + 1),
            "]".repeat(super::MAX_DEPTH + 1)
        );

        assert!(matches!(
            extract_safe_config(".env", &oversized),
            Err(ConfigExtractionError::InputTooLarge { .. })
        ));
        assert!(matches!(
            extract_safe_config("deep.json", &deeply_nested),
            Err(ConfigExtractionError::DepthLimitExceeded { .. })
        ));
    }

    #[test]
    fn classification_recognizes_required_sensitive_styles() {
        let cases = [
            ("refreshToken", SensitiveKeyKind::Token),
            ("db-password", SensitiveKeyKind::Password),
            ("client_secret", SensitiveKeyKind::Secret),
            ("sshPrivateKey", SensitiveKeyKind::PrivateKey),
            ("service_credentials", SensitiveKeyKind::Credential),
            ("AWS_ACCESS_KEY_ID", SensitiveKeyKind::AccessKey),
            ("connectionString", SensitiveKeyKind::ConnectionString),
            ("DATABASE_URL", SensitiveKeyKind::DatabaseUrl),
        ];

        assert!(
            cases
                .iter()
                .all(|(name, expected)| classify_sensitive_key(name) == Some(*expected))
        );
    }

    #[test]
    fn literal_reference_accepts_only_narrow_external_references() {
        assert!(is_safe_literal_reference("${DATABASE_PASSWORD}"));
        assert!(is_safe_literal_reference(
            "vault://applications/code-system-graph#database"
        ));
        assert!(is_safe_literal_reference("/run/secrets/database_password"));
    }

    #[test]
    fn literal_reference_rejects_uri_userinfo_and_secret_literals() {
        let values = [
            "postgres://admin:password@database.example/app",
            concat!("ghp_", "012345678901234567890123456789012345"),
            concat!(
                "eyJhbGciOiJIUzI1NiJ9",
                ".",
                "eyJzdWIiOiIxMjM0NTY3ODkwIn0",
                ".",
                "signature123"
            ),
            "ordinary-literal",
        ];

        assert!(values.iter().all(|value| !is_safe_literal_reference(value)));
    }
}

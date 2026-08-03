//! Dependency-free, evidence-first extraction of focused HTTP and test declarations.
//!
//! The parsers in this module recognize only framework-specific syntax and only promote literal
//! methods and paths to confirmed observations. Dynamic expressions are retained as ambiguous or
//! incomplete observations so downstream linking cannot mistake missing evidence for an exact
//! contract.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{ExtractionLimitExceeded, ExtractionTracker};

const HTTP_METHODS: [&str; 8] = [
    "DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT", "TRACE",
];

/// Source language understood by the focused parsers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceLanguage {
    /// JavaScript source.
    JavaScript,
    /// TypeScript source.
    TypeScript,
    /// Rust source.
    Rust,
    /// Python source.
    Python,
    /// Go source.
    Go,
    /// Java source.
    Java,
}

/// Framework whose syntax supplied the direct evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceFramework {
    /// Browser or runtime Fetch API calls.
    Fetch,
    /// Axios HTTP client calls.
    Axios,
    /// Express route declarations.
    Express,
    /// Fastify route declarations.
    Fastify,
    /// `NestJS` controller declarations.
    NestJs,
    /// Next.js App Router file-route declarations.
    NextJs,
    /// Axum router declarations.
    Axum,
    /// Actix Web route declarations and route attributes.
    ActixWeb,
    /// Utoipa operation declarations, including Actix routes wrapped by `cfg_attr`.
    Utoipa,
    /// Reqwest client calls.
    Reqwest,
    /// Rust built-in `#[test]` functions.
    RustTest,
    /// Tokio `#[tokio::test]` functions.
    TokioTest,
    /// Rstest `#[rstest]` functions.
    Rstest,
    /// `FastAPI` route decorators.
    FastApi,
    /// Flask route decorators.
    Flask,
    /// Python requests calls.
    Requests,
    /// Python HTTPX calls.
    Httpx,
    /// Python aiohttp client-session calls.
    AioHttp,
    /// Statically declared Python HTTP method/path registries.
    PythonHttpRegistry,
    /// Factory Boy model and sub-factory declarations.
    FactoryBoy,
    /// Pytest test functions.
    Pytest,
    /// unittest `TestCase` methods.
    Unittest,
    /// Canonical `test_` methods in a Python class whose runner inheritance is resolved elsewhere.
    PythonTest,
    /// Go standard-library `net/http`.
    GoNetHttp,
    /// Gin route declarations.
    Gin,
    /// Chi route declarations.
    Chi,
    /// Spring MVC route declarations.
    SpringMvc,
    /// Spring `WebClient` calls.
    WebClient,
    /// Feign client declarations.
    Feign,
}

/// Repository-boundary role represented by an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRole {
    /// An HTTP operation served by the repository.
    Provider,
    /// An HTTP operation invoked by the repository.
    Consumer,
    /// A declared test case.
    Test,
    /// A data factory with one statically declared model target.
    Factory,
}

/// Epistemic state of a source observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceEpistemicStatus {
    /// Method and path, when applicable, are supported by exact literal syntax.
    Confirmed,
    /// A relevant framework operation was found, but a dynamic expression prevents exact identity.
    Ambiguous,
    /// Required method, path, or symbol evidence was absent.
    Incomplete,
}

/// Inclusive one-based source range supporting an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceLineRange {
    /// First line containing direct evidence.
    pub start: u32,
    /// Last line containing direct evidence.
    pub end: u32,
}

/// Machine-readable limitation attached to an observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceWarning {
    /// The HTTP method is computed rather than represented by a supported literal or method name.
    DynamicMethod,
    /// The URL or route path is computed rather than represented by one literal.
    DynamicPath,
    /// A literal URL is not an absolute URL or root-relative path.
    UnsupportedLiteralPath,
    /// The framework declaration did not expose an implementation symbol.
    MissingSymbol,
    /// Tree-sitter recovered from at least one syntax error in the source artifact.
    SyntaxErrorRecovery,
    /// A documentation-oriented declaration may drift from executable framework registration.
    AdvisoryDeclaration,
}

/// Conversion-ready source evidence for an HTTP boundary, implementation, or test case.
///
/// Confirmed provider observations have enough method, path, and symbol evidence to construct an
/// HTTP provider plus an implementation anchor. Confirmed consumers have exact method/path
/// evidence. Test observations intentionally omit method/path; downstream correlation can join
/// them to consumer observations with the same `symbol_name`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceObservation {
    /// Language in which the evidence was found.
    pub language: SourceLanguage,
    /// Framework whose syntax was recognized.
    pub framework: SourceFramework,
    /// Provider, consumer, or test role.
    pub role: SourceRole,
    /// Canonical upper-case HTTP method when exactly known.
    pub method: Option<String>,
    /// Canonical normalized HTTP path when exactly known.
    pub path: Option<String>,
    /// Enclosing implementation symbol or test name when known.
    pub symbol_name: Option<String>,
    /// Related model symbol for declarations such as Factory Boy `Meta.model`.
    pub related_symbol: Option<String>,
    /// Repository-relative module path that declares `related_symbol`, when statically imported.
    pub related_path: Option<String>,
    /// Inclusive source range containing the direct evidence.
    pub lines: SourceLineRange,
    /// Epistemic state of this observation.
    pub status: SourceEpistemicStatus,
    /// Normalized confidence in the inclusive range from zero to one.
    pub confidence: f32,
    /// Deterministically ordered extraction limitations.
    pub warnings: Vec<SourceWarning>,
}

pub(crate) struct SourceObservationCollector<'a> {
    observations: Vec<SourceObservation>,
    tracker: Option<&'a mut ExtractionTracker>,
    error: Option<ExtractionLimitExceeded>,
}

impl<'a> SourceObservationCollector<'a> {
    pub(crate) fn unbounded() -> Self {
        Self {
            observations: Vec::new(),
            tracker: None,
            error: None,
        }
    }

    pub(crate) fn bounded(tracker: &'a mut ExtractionTracker) -> Self {
        Self {
            observations: Vec::new(),
            tracker: Some(tracker),
            error: None,
        }
    }

    pub(crate) fn push(&mut self, observation: SourceObservation) {
        if self.error.is_some() {
            return;
        }
        if let Some(tracker) = self.tracker.as_deref_mut()
            && let Err(error) = tracker
                .charge_observation(1)
                .and_then(|()| charge_source_observation_values(&observation, tracker))
        {
            self.error = Some(error);
            return;
        }
        self.observations.push(observation);
    }

    pub(crate) fn into_result(self) -> Result<Vec<SourceObservation>, ExtractionLimitExceeded> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(self.observations),
        }
    }

    pub(crate) fn into_unbounded(self) -> Vec<SourceObservation> {
        self.observations
    }
}

pub(crate) fn charge_source_observation_values(
    observation: &SourceObservation,
    tracker: &mut ExtractionTracker,
) -> Result<(), ExtractionLimitExceeded> {
    if let Some(method) = &observation.method {
        tracker.charge_identifier(method)?;
    }
    if let Some(path) = &observation.path {
        tracker.charge_portable_path(path)?;
    }
    for symbol in [
        observation.symbol_name.as_deref(),
        observation.related_symbol.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        tracker.charge_identifier(symbol)?;
    }
    if let Some(path) = &observation.related_path {
        tracker.charge_portable_path(path)?;
    }
    Ok(())
}

/// Parses the mandatory focused Rust framework matrix.
///
/// Supported syntax comprises Axum `route` declarations, Actix Web route attributes and builder
/// routes, Reqwest convenience/request calls, and built-in, Tokio, and rstest test attributes.
/// The returned vector is sorted and deduplicated deterministically.
#[must_use]
pub fn parse_rust_source(source: &str) -> Vec<SourceObservation> {
    let collector = collect_rust_source(source, SourceObservationCollector::unbounded());
    finish(collector.into_unbounded())
}

fn collect_rust_source<'a>(
    source: &str,
    mut observations: SourceObservationCollector<'a>,
) -> SourceObservationCollector<'a> {
    let tokens = lex_rust(source);
    let functions = rust_functions(&tokens);
    parse_rust_attributes(&tokens, &mut observations);
    if has_ident(&tokens, "axum") {
        parse_axum_routes(&tokens, &mut observations);
    }
    if has_ident(&tokens, "actix_web") {
        parse_actix_builder_routes(&tokens, &mut observations);
    }
    if has_ident(&tokens, "reqwest") {
        parse_reqwest_calls(&tokens, &functions, &mut observations);
    }
    observations
}

/// Parses focused Rust facts while charging each attempted observation before retention.
///
/// # Errors
///
/// Returns [`ExtractionLimitExceeded`] before an observation or one of its values exceeds the
/// effective per-artifact budget.
pub fn parse_rust_source_with_tracker(
    source: &str,
    tracker: &mut ExtractionTracker,
) -> Result<Vec<SourceObservation>, ExtractionLimitExceeded> {
    let observations = collect_rust_source(source, SourceObservationCollector::bounded(tracker));
    Ok(finish(observations.into_result()?))
}

/// Parses the mandatory focused Python framework matrix.
///
/// Supported syntax comprises `FastAPI` and Flask decorators, requests, HTTPX and aiohttp
/// convenience/client calls, static HTTP registries, Factory Boy model declarations, pytest
/// functions, unittest `TestCase` methods, and canonical class-level `test_` methods whose
/// indirect runner inheritance cannot be proven in one file. Comments and string contents are
/// lexically excluded from recognition. The returned vector is sorted and deduplicated.
#[must_use]
pub fn parse_python_source(source: &str) -> Vec<SourceObservation> {
    let collector = collect_python_source(source, SourceObservationCollector::unbounded());
    finish(collector.into_unbounded())
}

fn collect_python_source<'a>(
    source: &str,
    mut observations: SourceObservationCollector<'a>,
) -> SourceObservationCollector<'a> {
    let tokens = lex_python(source);
    let functions = python_functions(source, &tokens);
    let contexts = PythonContexts::discover(&tokens);
    parse_python_routes(&tokens, &contexts, &mut observations);
    parse_python_http_registries(&tokens, &mut observations);
    parse_python_http_calls(&tokens, &functions, &contexts, &mut observations);
    parse_python_factories(source, &tokens, &mut observations);
    parse_python_tests(source, &tokens, &mut observations);
    observations
}

/// Parses focused Python facts while charging each attempted observation before retention.
///
/// # Errors
///
/// Returns [`ExtractionLimitExceeded`] before an observation or one of its values exceeds the
/// effective per-artifact budget.
pub fn parse_python_source_with_tracker(
    source: &str,
    tracker: &mut ExtractionTracker,
) -> Result<Vec<SourceObservation>, ExtractionLimitExceeded> {
    let observations = collect_python_source(source, SourceObservationCollector::bounded(tracker));
    Ok(finish(observations.into_result()?))
}

/// Normalizes a literal path using the same slash and trailing-separator rules as HTTP contracts.
///
/// Query strings and fragments must be removed by the caller before invoking this function.
#[must_use]
pub fn normalize_source_http_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return "/".to_owned();
    }
    let mut normalized = String::with_capacity(trimmed.len() + 1);
    if !trimmed.starts_with('/') {
        normalized.push('/');
    }
    let mut previous_slash = false;
    for character in trimmed.chars() {
        if character == '/' {
            if !previous_slash {
                normalized.push('/');
            }
            previous_slash = true;
        } else {
            normalized.push(character);
            previous_slash = false;
        }
    }
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    normalized
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TokenKind {
    Ident(String),
    Literal(Option<String>),
    Punct(char),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Token {
    kind: TokenKind,
    line: u32,
    end_line: u32,
}

impl Token {
    fn is_ident(&self, expected: &str) -> bool {
        matches!(&self.kind, TokenKind::Ident(value) if value == expected)
    }

    fn is_punct(&self, expected: char) -> bool {
        self.kind == TokenKind::Punct(expected)
    }

    fn literal(&self) -> Option<&str> {
        match &self.kind {
            TokenKind::Literal(Some(value)) => Some(value),
            _ => None,
        }
    }

    fn ident(&self) -> Option<&str> {
        match &self.kind {
            TokenKind::Ident(value) => Some(value),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct FunctionSpan {
    name: String,
    start_token: usize,
    end_token: usize,
}

fn has_ident(tokens: &[Token], name: &str) -> bool {
    tokens.iter().any(|token| token.is_ident(name))
}

fn lex_rust(source: &str) -> Vec<Token> {
    let characters = source.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut line = 1_u32;
    while index < characters.len() {
        match characters[index] {
            '\n' => {
                line += 1;
                index += 1;
            }
            character if character.is_whitespace() => index += 1,
            '/' if characters.get(index + 1) == Some(&'/') => {
                index += 2;
                while index < characters.len() && characters[index] != '\n' {
                    index += 1;
                }
            }
            '/' if characters.get(index + 1) == Some(&'*') => {
                index += 2;
                let mut depth = 1_u32;
                while index < characters.len() && depth > 0 {
                    if characters[index] == '\n' {
                        line += 1;
                        index += 1;
                    } else if characters[index] == '/' && characters.get(index + 1) == Some(&'*') {
                        depth += 1;
                        index += 2;
                    } else if characters[index] == '*' && characters.get(index + 1) == Some(&'/') {
                        depth -= 1;
                        index += 2;
                    } else {
                        index += 1;
                    }
                }
            }
            'r' if rust_raw_string_start(&characters, index).is_some() => {
                let start_line = line;
                let (next, value) = consume_rust_raw_string(&characters, index, &mut line);
                tokens.push(Token {
                    kind: TokenKind::Literal(value),
                    line: start_line,
                    end_line: line,
                });
                index = next;
            }
            '"' => {
                let start_line = line;
                let (next, value) = consume_quoted(&characters, index, '"', false, &mut line);
                tokens.push(Token {
                    kind: TokenKind::Literal(value),
                    line: start_line,
                    end_line: line,
                });
                index = next;
            }
            '\'' => {
                let (next, _) = consume_quoted(&characters, index, '\'', false, &mut line);
                index = next;
            }
            character if is_ident_start(character) => {
                let start = index;
                index += 1;
                while index < characters.len() && is_ident_continue(characters[index]) {
                    index += 1;
                }
                tokens.push(Token {
                    kind: TokenKind::Ident(characters[start..index].iter().collect()),
                    line,
                    end_line: line,
                });
            }
            character => {
                tokens.push(Token {
                    kind: TokenKind::Punct(character),
                    line,
                    end_line: line,
                });
                index += 1;
            }
        }
    }
    tokens
}

fn rust_raw_string_start(characters: &[char], index: usize) -> Option<usize> {
    let mut cursor = index + 1;
    while characters.get(cursor) == Some(&'#') {
        cursor += 1;
    }
    (characters.get(cursor) == Some(&'"')).then_some(cursor - index - 1)
}

fn consume_rust_raw_string(
    characters: &[char],
    index: usize,
    line: &mut u32,
) -> (usize, Option<String>) {
    let hashes = rust_raw_string_start(characters, index).unwrap_or_default();
    let content_start = index + hashes + 2;
    let mut cursor = content_start;
    while cursor < characters.len() {
        if characters[cursor] == '\n' {
            *line += 1;
        }
        if characters[cursor] == '"'
            && (0..hashes).all(|offset| characters.get(cursor + 1 + offset) == Some(&'#'))
        {
            return (
                cursor + hashes + 1,
                Some(characters[content_start..cursor].iter().collect()),
            );
        }
        cursor += 1;
    }
    (cursor, None)
}

fn lex_python(source: &str) -> Vec<Token> {
    let characters = source.chars().collect::<Vec<_>>();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut line = 1_u32;
    while index < characters.len() {
        match characters[index] {
            '\n' => {
                line += 1;
                index += 1;
            }
            character if character.is_whitespace() => index += 1,
            '#' => {
                while index < characters.len() && characters[index] != '\n' {
                    index += 1;
                }
            }
            prefix @ ('r' | 'R' | 'u' | 'U')
                if matches!(characters.get(index + 1), Some('"' | '\'')) =>
            {
                let start_line = line;
                let quote = characters[index + 1];
                let triple = characters.get(index + 2) == Some(&quote)
                    && characters.get(index + 3) == Some(&quote);
                let (next, value) =
                    consume_quoted(&characters, index + 1, quote, triple, &mut line);
                let _ = prefix;
                tokens.push(Token {
                    kind: TokenKind::Literal(value),
                    line: start_line,
                    end_line: line,
                });
                index = next;
            }
            quote @ ('"' | '\'') => {
                let start_line = line;
                let triple = characters.get(index + 1) == Some(&quote)
                    && characters.get(index + 2) == Some(&quote);
                let (next, value) = consume_quoted(&characters, index, quote, triple, &mut line);
                tokens.push(Token {
                    kind: TokenKind::Literal(value),
                    line: start_line,
                    end_line: line,
                });
                index = next;
            }
            character if is_ident_start(character) => {
                let start = index;
                index += 1;
                while index < characters.len() && is_ident_continue(characters[index]) {
                    index += 1;
                }
                tokens.push(Token {
                    kind: TokenKind::Ident(characters[start..index].iter().collect()),
                    line,
                    end_line: line,
                });
            }
            character => {
                tokens.push(Token {
                    kind: TokenKind::Punct(character),
                    line,
                    end_line: line,
                });
                index += 1;
            }
        }
    }
    tokens
}

fn consume_quoted(
    characters: &[char],
    index: usize,
    quote: char,
    triple: bool,
    line: &mut u32,
) -> (usize, Option<String>) {
    let delimiter_width = if triple { 3 } else { 1 };
    let mut cursor = index + delimiter_width;
    let mut value = String::new();
    let mut valid = true;
    while cursor < characters.len() {
        if characters[cursor] == '\n' {
            *line += 1;
            if !triple {
                valid = false;
            }
        }
        let closes = if triple {
            characters.get(cursor) == Some(&quote)
                && characters.get(cursor + 1) == Some(&quote)
                && characters.get(cursor + 2) == Some(&quote)
        } else {
            characters.get(cursor) == Some(&quote)
        };
        if closes {
            return (
                cursor + delimiter_width,
                if valid { Some(value) } else { None },
            );
        }
        if characters[cursor] == '\\' && !triple {
            let Some(escaped) = characters.get(cursor + 1).copied() else {
                return (characters.len(), None);
            };
            match escaped {
                '\\' => value.push('\\'),
                '"' => value.push('"'),
                '\'' => value.push('\''),
                'n' => value.push('\n'),
                'r' => value.push('\r'),
                't' => value.push('\t'),
                _ => valid = false,
            }
            cursor += 2;
        } else {
            value.push(characters[cursor]);
            cursor += 1;
        }
    }
    (cursor, None)
}

fn is_ident_start(character: char) -> bool {
    character == '_' || character.is_alphabetic()
}

fn is_ident_continue(character: char) -> bool {
    character == '_' || character.is_alphanumeric()
}

fn matching(tokens: &[Token], open: usize, left: char, right: char) -> Option<usize> {
    if !tokens.get(open).is_some_and(|token| token.is_punct(left)) {
        return None;
    }
    let mut depth = 0_u32;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        if token.is_punct(left) {
            depth += 1;
        } else if token.is_punct(right) {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn rust_functions(tokens: &[Token]) -> Vec<FunctionSpan> {
    let mut functions = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        if !token.is_ident("fn") {
            continue;
        }
        let Some(name) = tokens.get(index + 1).and_then(Token::ident) else {
            continue;
        };
        let Some(open) = (index + 2..tokens.len())
            .find(|candidate| tokens[*candidate].is_punct('{') || tokens[*candidate].is_punct(';'))
        else {
            continue;
        };
        if tokens[open].is_punct(';') {
            continue;
        }
        let end = matching(tokens, open, '{', '}').unwrap_or(tokens.len().saturating_sub(1));
        functions.push(FunctionSpan {
            name: name.to_owned(),
            start_token: index,
            end_token: end,
        });
    }
    functions
}

fn enclosing_symbol(functions: &[FunctionSpan], token_index: usize) -> Option<String> {
    functions
        .iter()
        .filter(|function| function.start_token <= token_index && token_index <= function.end_token)
        .min_by_key(|function| function.end_token - function.start_token)
        .map(|function| function.name.clone())
}

fn parse_rust_attributes(tokens: &[Token], observations: &mut SourceObservationCollector<'_>) {
    let mut index = 0;
    while index + 2 < tokens.len() {
        if !tokens[index].is_punct('#') || !tokens[index + 1].is_punct('[') {
            index += 1;
            continue;
        }
        let Some(close) = matching(tokens, index + 1, '[', ']') else {
            break;
        };
        let Some((function_index, name, signature_end)) = rust_function_after(tokens, close) else {
            index = close + 1;
            continue;
        };
        let path = attribute_path(tokens, index + 2, close);
        let framework = match path.as_slice() {
            [name] if name == "test" => Some(SourceFramework::RustTest),
            [first, second] if first == "tokio" && second == "test" => {
                Some(SourceFramework::TokioTest)
            }
            [name] if name == "rstest" => Some(SourceFramework::Rstest),
            _ => None,
        };
        if let Some(framework) = framework {
            observations.push(confirmed_test(
                SourceLanguage::Rust,
                framework,
                name.clone(),
                tokens[index].line,
                tokens[signature_end].end_line,
            ));
        }
        parse_utoipa_attribute(
            tokens,
            index,
            close,
            function_index,
            signature_end,
            &name,
            observations,
        );
        if has_ident(tokens, "actix_web") {
            parse_actix_attribute(
                tokens,
                index,
                close,
                function_index,
                signature_end,
                name,
                &path,
                observations,
            );
        }
        index = close + 1;
    }
}

fn parse_utoipa_attribute(
    tokens: &[Token],
    start: usize,
    close: usize,
    function_index: usize,
    signature_end: usize,
    symbol: &str,
    observations: &mut SourceObservationCollector<'_>,
) {
    let Some(utoipa_index) = (start..close).find(|index| tokens[*index].is_ident("utoipa")) else {
        return;
    };
    if !tokens[utoipa_index + 1..close]
        .iter()
        .any(|token| token.is_ident("path"))
    {
        return;
    }
    let method = tokens[utoipa_index..close]
        .iter()
        .filter_map(Token::ident)
        .find_map(canonical_method)
        .map(str::to_owned);
    let explicit_path = parse_keyword_string_values(tokens, utoipa_index, close, "path")
        .into_iter()
        .next();
    let context_path = parse_keyword_string_values(tokens, utoipa_index, close, "context_path")
        .into_iter()
        .next();
    let actix = rust_actix_route_between(tokens, close.saturating_add(1), function_index);
    let method = method.or_else(|| actix.as_ref().and_then(|(method, _)| method.clone()));
    let path = explicit_path.or_else(|| {
        let (context, (_, route)) = (context_path?, actix?);
        Some(normalize_source_http_path(&format!("{context}{route}")))
    });
    let lines = SourceLineRange {
        start: tokens[start].line,
        end: tokens[signature_end].end_line,
    };
    let mut observation = http_from_literal(
        SourceLanguage::Rust,
        SourceFramework::Utoipa,
        SourceRole::Provider,
        method,
        path.as_deref(),
        Some(symbol.to_owned()),
        lines,
        true,
    );
    if observation.status == SourceEpistemicStatus::Confirmed {
        observation.confidence = 0.75;
        observation
            .warnings
            .push(SourceWarning::AdvisoryDeclaration);
    }
    observations.push(observation);
}

fn rust_actix_route_between(
    tokens: &[Token],
    start: usize,
    end: usize,
) -> Option<(Option<String>, String)> {
    let mut index = start;
    while index + 3 < end {
        if !tokens[index].is_punct('#') || !tokens[index + 1].is_punct('[') {
            index += 1;
            continue;
        }
        let close = matching(tokens, index + 1, '[', ']')?;
        if close > end {
            return None;
        }
        let method = tokens
            .get(index + 2)
            .and_then(Token::ident)
            .and_then(canonical_method)
            .map(str::to_owned);
        let open = (index + 2..close).find(|candidate| tokens[*candidate].is_punct('('));
        let path = open
            .and_then(|open| first_argument(open, close))
            .and_then(|argument| tokens.get(argument))
            .and_then(Token::literal)
            .and_then(route_literal_path)?;
        return Some((method, path));
    }
    None
}

fn rust_function_after(tokens: &[Token], close: usize) -> Option<(usize, String, usize)> {
    let mut index = close + 1;
    while index < tokens.len() && index <= close + 24 {
        if tokens[index].is_punct('#') {
            if tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('['))
            {
                index = matching(tokens, index + 1, '[', ']')? + 1;
                continue;
            }
            return None;
        }
        if tokens[index].is_ident("fn") {
            let name = tokens.get(index + 1)?.ident()?.to_owned();
            let end = (index + 2..tokens.len())
                .find(|candidate| {
                    tokens[*candidate].is_punct('{') || tokens[*candidate].is_punct(';')
                })
                .unwrap_or(index + 1);
            return Some((index, name, end));
        }
        index += 1;
    }
    None
}

fn attribute_path(tokens: &[Token], start: usize, end: usize) -> Vec<String> {
    let mut path = Vec::new();
    let mut index = start;
    while index < end && !tokens[index].is_punct('(') {
        if let Some(name) = tokens[index].ident() {
            path.push(name.to_owned());
        }
        index += 1;
    }
    path
}

#[expect(
    clippy::too_many_arguments,
    reason = "Attribute evidence is passed without allocation"
)]
fn parse_actix_attribute(
    tokens: &[Token],
    start: usize,
    close: usize,
    _function_index: usize,
    signature_end: usize,
    symbol: String,
    path: &[String],
    observations: &mut SourceObservationCollector<'_>,
) {
    let direct_method = match path {
        [name] => canonical_method(name),
        _ => None,
    };
    if direct_method.is_none() && path != ["route"] {
        return;
    }
    let open = (start + 2..close).find(|index| tokens[*index].is_punct('('));
    let literal = open
        .and_then(|open| first_argument(open, close))
        .and_then(|argument| tokens.get(argument))
        .and_then(Token::literal);
    let methods = direct_method
        .into_iter()
        .map(str::to_owned)
        .chain(
            parse_keyword_string_values(tokens, start, close, "method")
                .into_iter()
                .filter_map(|method| canonical_method(&method).map(str::to_owned)),
        )
        .collect::<BTreeSet<_>>();
    if methods.is_empty() {
        observations.push(inexact_http(
            SourceLanguage::Rust,
            SourceFramework::ActixWeb,
            SourceRole::Provider,
            None,
            literal.and_then(route_literal_path),
            Some(symbol),
            SourceLineRange {
                start: tokens[start].line,
                end: tokens[signature_end].end_line,
            },
            SourceWarning::DynamicMethod,
        ));
        return;
    }
    for method in methods {
        observations.push(http_from_literal(
            SourceLanguage::Rust,
            SourceFramework::ActixWeb,
            SourceRole::Provider,
            Some(method),
            literal,
            Some(symbol.clone()),
            SourceLineRange {
                start: tokens[start].line,
                end: tokens[signature_end].end_line,
            },
            true,
        ));
    }
}

fn parse_axum_routes(tokens: &[Token], observations: &mut SourceObservationCollector<'_>) {
    for (index, token) in tokens.iter().enumerate() {
        if !token.is_ident("route")
            || !tokens
                .get(index.wrapping_sub(1))
                .is_some_and(|token| token.is_punct('.'))
            || !tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('('))
        {
            continue;
        }
        let Some(close) = matching(tokens, index + 1, '(', ')') else {
            continue;
        };
        let arguments = top_level_arguments(tokens, index + 1, close);
        if arguments.len() < 2 {
            continue;
        }
        let literal = tokens.get(arguments[0]).and_then(Token::literal);
        let methods = method_calls(tokens, arguments[1], close);
        for (method, handler) in methods {
            observations.push(http_from_literal(
                SourceLanguage::Rust,
                SourceFramework::Axum,
                SourceRole::Provider,
                Some(method),
                literal,
                handler,
                SourceLineRange {
                    start: token.line,
                    end: tokens[close].end_line,
                },
                true,
            ));
        }
    }
}

fn parse_actix_builder_routes(tokens: &[Token], observations: &mut SourceObservationCollector<'_>) {
    for (index, token) in tokens.iter().enumerate() {
        if !token.is_ident("route")
            || !tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('('))
        {
            continue;
        }
        let Some(close) = matching(tokens, index + 1, '(', ')') else {
            continue;
        };
        let arguments = top_level_arguments(tokens, index + 1, close);
        if arguments.len() < 2 || !contains_ident(tokens, arguments[1], close, "web") {
            continue;
        }
        let literal = tokens.get(arguments[0]).and_then(Token::literal);
        for (method, handler) in method_calls(tokens, arguments[1], close) {
            observations.push(http_from_literal(
                SourceLanguage::Rust,
                SourceFramework::ActixWeb,
                SourceRole::Provider,
                Some(method),
                literal,
                handler,
                SourceLineRange {
                    start: token.line,
                    end: tokens[close].end_line,
                },
                true,
            ));
        }
    }
    for (index, token) in tokens.iter().enumerate() {
        if !token.is_ident("resource")
            || !tokens
                .get(index.wrapping_sub(1))
                .is_some_and(|token| token.is_punct(':'))
            || !tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('('))
        {
            continue;
        }
        let Some(resource_close) = matching(tokens, index + 1, '(', ')') else {
            continue;
        };
        let end = (resource_close + 1..tokens.len())
            .find(|candidate| tokens[*candidate].is_punct(';'))
            .unwrap_or(resource_close);
        if !contains_ident(tokens, resource_close + 1, end, "route") {
            continue;
        }
        let literal = tokens.get(index + 2).and_then(Token::literal);
        for (method, handler) in method_calls(tokens, resource_close + 1, end) {
            observations.push(http_from_literal(
                SourceLanguage::Rust,
                SourceFramework::ActixWeb,
                SourceRole::Provider,
                Some(method),
                literal,
                handler,
                SourceLineRange {
                    start: token.line,
                    end: tokens[end].end_line,
                },
                true,
            ));
        }
    }
}

fn parse_reqwest_calls(
    tokens: &[Token],
    functions: &[FunctionSpan],
    observations: &mut SourceObservationCollector<'_>,
) {
    let clients = rust_reqwest_clients(tokens);
    for index in 0..tokens.len() {
        let explicit = tokens[index].is_ident("reqwest")
            && tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct(':'))
            && tokens
                .get(index + 2)
                .is_some_and(|token| token.is_punct(':'));
        let (receiver, method_index) = if explicit {
            let direct = index + 3;
            let method_index = if tokens
                .get(direct)
                .is_some_and(|token| token.is_ident("blocking"))
                && tokens
                    .get(direct + 1)
                    .is_some_and(|token| token.is_punct(':'))
                && tokens
                    .get(direct + 2)
                    .is_some_and(|token| token.is_punct(':'))
            {
                direct + 3
            } else {
                direct
            };
            ("reqwest", method_index)
        } else if let Some(receiver) = tokens[index].ident() {
            if clients.contains(receiver)
                && tokens
                    .get(index + 1)
                    .is_some_and(|token| token.is_punct('.'))
            {
                (receiver, index + 2)
            } else {
                continue;
            }
        } else {
            continue;
        };
        let Some(method_name) = tokens.get(method_index).and_then(Token::ident) else {
            continue;
        };
        if !tokens
            .get(method_index + 1)
            .is_some_and(|token| token.is_punct('('))
        {
            continue;
        }
        let Some(close) = matching(tokens, method_index + 1, '(', ')') else {
            continue;
        };
        let arguments = top_level_arguments(tokens, method_index + 1, close);
        let (method, path_argument) = if method_name == "request" {
            (
                arguments.first().and_then(|argument| {
                    let method_end = arguments
                        .get(1)
                        .map_or(close, |second| second.saturating_sub(1));
                    rust_method_expression(tokens, *argument, method_end)
                }),
                arguments.get(1).copied(),
            )
        } else {
            (
                canonical_method(method_name).map(str::to_owned),
                arguments.first().copied(),
            )
        };
        if method.is_none() && method_name != "request" {
            continue;
        }
        let literal = path_argument
            .and_then(|argument| tokens.get(argument))
            .and_then(Token::literal);
        let mut observation = http_from_literal(
            SourceLanguage::Rust,
            SourceFramework::Reqwest,
            SourceRole::Consumer,
            method,
            literal,
            enclosing_symbol(functions, index),
            SourceLineRange {
                start: tokens[index].line,
                end: tokens[close].end_line,
            },
            false,
        );
        if method_name == "request" && observation.method.is_none() {
            observation.status = SourceEpistemicStatus::Incomplete;
            observation.confidence = 0.0;
            observation.warnings.push(SourceWarning::DynamicMethod);
            observation.warnings.sort();
            observation.warnings.dedup();
        }
        let _ = receiver;
        observations.push(observation);
    }
}

fn rust_reqwest_clients(tokens: &[Token]) -> BTreeSet<String> {
    let mut clients = BTreeSet::new();
    let client_imported = tokens.iter().enumerate().any(|(index, token)| {
        token.is_ident("reqwest")
            && (index + 1..tokens.len())
                .take_while(|candidate| !tokens[*candidate].is_punct(';'))
                .take(24)
                .any(|candidate| tokens[candidate].is_ident("Client"))
    });
    for index in 0..tokens.len() {
        if !tokens[index].is_ident("let") {
            continue;
        }
        let Some(name) = tokens.get(index + 1).and_then(Token::ident) else {
            continue;
        };
        let end = (index + 2..tokens.len())
            .find(|candidate| tokens[*candidate].is_punct(';'))
            .unwrap_or(tokens.len());
        let uses_qualified_client = contains_ident(tokens, index + 2, end, "reqwest")
            && contains_ident(tokens, index + 2, end, "Client");
        let uses_imported_client = client_imported
            && tokens
                .get(index + 3)
                .is_some_and(|token| token.is_ident("Client"));
        if uses_qualified_client || uses_imported_client {
            clients.insert(name.to_owned());
        }
    }
    clients
}

fn rust_method_expression(tokens: &[Token], start: usize, end: usize) -> Option<String> {
    if let Some(literal) = tokens.get(start).and_then(Token::literal) {
        return canonical_method(literal).map(str::to_owned);
    }
    (start..end)
        .filter_map(|index| tokens[index].ident())
        .find_map(|name| canonical_method(name).map(str::to_owned))
}

fn method_calls(tokens: &[Token], start: usize, end: usize) -> Vec<(String, Option<String>)> {
    let mut methods = Vec::new();
    for index in start..end {
        let Some(name) = tokens[index].ident() else {
            continue;
        };
        let Some(method) = canonical_method(name) else {
            continue;
        };
        if !tokens
            .get(index + 1)
            .is_some_and(|token| token.is_punct('('))
        {
            continue;
        }
        let direct_handler = tokens
            .get(index + 2)
            .and_then(Token::ident)
            .map(str::to_owned);
        let actix_handler = (index + 2..end)
            .find(|candidate| {
                tokens[*candidate].is_ident("to")
                    && tokens
                        .get(*candidate + 1)
                        .is_some_and(|token| token.is_punct('('))
            })
            .and_then(|to| tokens.get(to + 2))
            .and_then(Token::ident)
            .map(str::to_owned);
        let handler = direct_handler.or(actix_handler);
        methods.push((method.to_owned(), handler));
    }
    methods
}

fn contains_ident(tokens: &[Token], start: usize, end: usize, name: &str) -> bool {
    tokens
        .get(start..end)
        .is_some_and(|slice| slice.iter().any(|token| token.is_ident(name)))
}

fn first_argument(open: usize, close: usize) -> Option<usize> {
    (open + 1 < close).then_some(open + 1)
}

fn top_level_arguments(tokens: &[Token], open: usize, close: usize) -> Vec<usize> {
    if open + 1 >= close {
        return Vec::new();
    }
    let mut arguments = vec![open + 1];
    let mut round = 0_i32;
    let mut square = 0_i32;
    let mut curly = 0_i32;
    for (index, token) in tokens.iter().enumerate().take(close).skip(open + 1) {
        match token.kind {
            TokenKind::Punct('(') => round += 1,
            TokenKind::Punct(')') => round -= 1,
            TokenKind::Punct('[') => square += 1,
            TokenKind::Punct(']') => square -= 1,
            TokenKind::Punct('{') => curly += 1,
            TokenKind::Punct('}') => curly -= 1,
            TokenKind::Punct(',')
                if round == 0 && square == 0 && curly == 0 && index + 1 < close =>
            {
                arguments.push(index + 1);
            }
            _ => {}
        }
    }
    arguments
}

fn parse_keyword_string_values(
    tokens: &[Token],
    start: usize,
    end: usize,
    keyword: &str,
) -> Vec<String> {
    let mut values = Vec::new();
    for index in start..end {
        if !tokens[index].is_ident(keyword)
            || !tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('='))
        {
            continue;
        }
        let value_start = index + 2;
        if let Some(value) = tokens.get(value_start).and_then(Token::literal) {
            values.push(value.to_owned());
        } else if tokens
            .get(value_start)
            .is_some_and(|token| token.is_punct('['))
        {
            let close = matching(tokens, value_start, '[', ']').unwrap_or(end);
            values.extend(
                tokens[value_start + 1..close]
                    .iter()
                    .filter_map(Token::literal)
                    .map(str::to_owned),
            );
        }
    }
    values
}

fn canonical_method(name: &str) -> Option<&'static str> {
    HTTP_METHODS
        .iter()
        .copied()
        .find(|method| method.eq_ignore_ascii_case(name))
}

fn confirmed_test(
    language: SourceLanguage,
    framework: SourceFramework,
    name: String,
    start: u32,
    end: u32,
) -> SourceObservation {
    SourceObservation {
        language,
        framework,
        role: SourceRole::Test,
        method: None,
        path: None,
        symbol_name: Some(name),
        related_symbol: None,
        related_path: None,
        lines: SourceLineRange { start, end },
        status: SourceEpistemicStatus::Confirmed,
        confidence: 1.0,
        warnings: Vec::new(),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Observation fields remain explicit at evidence sites"
)]
fn http_from_literal(
    language: SourceLanguage,
    framework: SourceFramework,
    role: SourceRole,
    method: Option<String>,
    literal: Option<&str>,
    symbol_name: Option<String>,
    lines: SourceLineRange,
    route: bool,
) -> SourceObservation {
    let path = literal.and_then(|value| {
        if route {
            route_literal_path(value)
        } else {
            client_literal_path(value)
        }
    });
    let warning = if literal.is_none() {
        Some(SourceWarning::DynamicPath)
    } else if path.is_none() {
        Some(SourceWarning::UnsupportedLiteralPath)
    } else {
        None
    };
    let status = if path.is_some() && method.is_some() {
        SourceEpistemicStatus::Confirmed
    } else {
        SourceEpistemicStatus::Ambiguous
    };
    let mut observation = SourceObservation {
        language,
        framework,
        role,
        method,
        path,
        symbol_name,
        related_symbol: None,
        related_path: None,
        lines,
        status,
        confidence: if status == SourceEpistemicStatus::Confirmed {
            1.0
        } else {
            0.0
        },
        warnings: warning.into_iter().collect(),
    };
    if role == SourceRole::Provider && observation.symbol_name.is_none() {
        observation.status = SourceEpistemicStatus::Incomplete;
        observation.confidence = 0.0;
        observation.warnings.push(SourceWarning::MissingSymbol);
    }
    observation
}

#[expect(
    clippy::too_many_arguments,
    reason = "Observation fields remain explicit at evidence sites"
)]
fn inexact_http(
    language: SourceLanguage,
    framework: SourceFramework,
    role: SourceRole,
    method: Option<String>,
    path: Option<String>,
    symbol_name: Option<String>,
    lines: SourceLineRange,
    warning: SourceWarning,
) -> SourceObservation {
    SourceObservation {
        language,
        framework,
        role,
        method,
        path,
        symbol_name,
        related_symbol: None,
        related_path: None,
        lines,
        status: SourceEpistemicStatus::Incomplete,
        confidence: 0.0,
        warnings: vec![warning],
    }
}

fn route_literal_path(value: &str) -> Option<String> {
    (value.starts_with('/') && !value.contains(['?', '#']))
        .then(|| normalize_source_http_path(value))
}

fn client_literal_path(value: &str) -> Option<String> {
    if value.starts_with('/') {
        return Some(normalize_source_http_path(
            value.split(['?', '#']).next().unwrap_or(value),
        ));
    }
    let after_authority = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"))?;
    if after_authority.is_empty() || after_authority.starts_with('/') {
        return None;
    }
    let path = after_authority
        .find('/')
        .map_or("/", |separator| &after_authority[separator..]);
    Some(normalize_source_http_path(
        path.split(['?', '#']).next().unwrap_or(path),
    ))
}

#[derive(Debug, Default)]
struct PythonContexts {
    fastapi_apps: BTreeSet<String>,
    route_prefixes: BTreeMap<String, String>,
    flask_apps: BTreeSet<String>,
    request_modules: BTreeSet<String>,
    httpx_modules: BTreeSet<String>,
    aiohttp_modules: BTreeSet<String>,
    request_clients: BTreeSet<String>,
    httpx_clients: BTreeSet<String>,
    aiohttp_clients: BTreeSet<String>,
    direct_calls: BTreeMap<String, (SourceFramework, String)>,
    string_constants: BTreeMap<String, String>,
}

impl PythonContexts {
    fn discover(tokens: &[Token]) -> Self {
        let mut contexts = Self::default();
        contexts.request_modules.insert("requests".to_owned());
        contexts.httpx_modules.insert("httpx".to_owned());
        contexts.aiohttp_modules.insert("aiohttp".to_owned());
        discover_python_import_aliases(tokens, "requests", &mut contexts.request_modules);
        discover_python_import_aliases(tokens, "httpx", &mut contexts.httpx_modules);
        discover_python_import_aliases(tokens, "aiohttp", &mut contexts.aiohttp_modules);
        discover_python_direct_calls(
            tokens,
            "requests",
            SourceFramework::Requests,
            &mut contexts.direct_calls,
        );
        discover_python_direct_calls(
            tokens,
            "httpx",
            SourceFramework::Httpx,
            &mut contexts.direct_calls,
        );
        let request_session_imported = python_imports_item(tokens, "requests", "Session");
        let httpx_client_imported = python_imports_item(tokens, "httpx", "Client")
            || python_imports_item(tokens, "httpx", "AsyncClient");
        let aiohttp_client_imported = python_imports_item(tokens, "aiohttp", "ClientSession");
        for index in 0..tokens.len() {
            let Some(name) = tokens[index].ident() else {
                continue;
            };
            if !tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('='))
            {
                continue;
            }
            let end = python_expression_end(tokens, index + 2);
            if let Some(value) =
                python_static_string_expression(tokens, index + 2, end, &contexts.string_constants)
            {
                contexts.string_constants.insert(name.to_owned(), value);
            }
            if contains_ident(tokens, index + 2, end, "FastAPI")
                || contains_ident(tokens, index + 2, end, "APIRouter")
            {
                contexts.fastapi_apps.insert(name.to_owned());
                if contains_ident(tokens, index + 2, end, "APIRouter")
                    && let Some(open) =
                        (index + 2..end).find(|candidate| tokens[*candidate].is_punct('('))
                    && let Some(close) = matching(tokens, open, '(', ')')
                    && let Some(prefix) = parse_keyword_string_values(tokens, open, close, "prefix")
                        .into_iter()
                        .next()
                {
                    contexts
                        .route_prefixes
                        .insert(name.to_owned(), normalize_source_http_path(&prefix));
                }
            }
            if contains_ident(tokens, index + 2, end, "Flask")
                || contains_ident(tokens, index + 2, end, "Blueprint")
            {
                contexts.flask_apps.insert(name.to_owned());
            }
            if contains_ident(tokens, index + 2, end, "Session")
                && (contains_any(tokens, index + 2, end, &contexts.request_modules)
                    || request_session_imported)
            {
                contexts.request_clients.insert(name.to_owned());
            }
            if (contains_ident(tokens, index + 2, end, "Client")
                || contains_ident(tokens, index + 2, end, "AsyncClient"))
                && (contains_any(tokens, index + 2, end, &contexts.httpx_modules)
                    || httpx_client_imported)
            {
                contexts.httpx_clients.insert(name.to_owned());
            }
            if contains_ident(tokens, index + 2, end, "ClientSession")
                && (contains_any(tokens, index + 2, end, &contexts.aiohttp_modules)
                    || aiohttp_client_imported)
            {
                contexts.aiohttp_clients.insert(name.to_owned());
            }
        }
        if aiohttp_client_imported {
            for index in 0..tokens.len() {
                if !tokens[index].is_ident("as") {
                    continue;
                }
                let Some(name) = tokens.get(index + 1).and_then(Token::ident) else {
                    continue;
                };
                let start = index.saturating_sub(32);
                if tokens[start..index]
                    .iter()
                    .any(|token| token.is_ident("ClientSession"))
                {
                    contexts.aiohttp_clients.insert(name.to_owned());
                }
            }
        }
        contexts
    }
}

fn python_imports_item(tokens: &[Token], module: &str, item: &str) -> bool {
    tokens.iter().enumerate().any(|(index, token)| {
        token.is_ident("from")
            && tokens
                .get(index + 1)
                .is_some_and(|token| token.is_ident(module))
            && tokens
                .get(index + 2)
                .is_some_and(|token| token.is_ident("import"))
            && (index + 3..tokens.len())
                .take_while(|candidate| tokens[*candidate].line == token.line)
                .any(|candidate| tokens[candidate].is_ident(item))
    })
}

fn discover_python_direct_calls(
    tokens: &[Token],
    module: &str,
    framework: SourceFramework,
    calls: &mut BTreeMap<String, (SourceFramework, String)>,
) {
    for index in 0..tokens.len() {
        if !tokens[index].is_ident("from")
            || !tokens
                .get(index + 1)
                .is_some_and(|token| token.is_ident(module))
            || !tokens
                .get(index + 2)
                .is_some_and(|token| token.is_ident("import"))
        {
            continue;
        }
        let mut cursor = index + 3;
        while cursor < tokens.len() && tokens[cursor].line == tokens[index].line {
            let Some(imported) = tokens[cursor].ident() else {
                cursor += 1;
                continue;
            };
            if canonical_method(imported).is_none() && imported != "request" {
                cursor += 1;
                continue;
            }
            let (local, advance) = if tokens
                .get(cursor + 1)
                .is_some_and(|token| token.is_ident("as"))
            {
                (
                    tokens
                        .get(cursor + 2)
                        .and_then(Token::ident)
                        .unwrap_or(imported),
                    3,
                )
            } else {
                (imported, 1)
            };
            calls.insert(local.to_owned(), (framework, imported.to_owned()));
            cursor += advance;
        }
    }
}

fn python_expression_end(tokens: &[Token], start: usize) -> usize {
    let mut round = 0_i32;
    let mut square = 0_i32;
    let mut curly = 0_i32;
    let mut previous_line = tokens.get(start).map_or(0, |token| token.line);
    for (index, token) in tokens.iter().enumerate().skip(start) {
        if index > start && token.line > previous_line && round == 0 && square == 0 && curly == 0 {
            return index;
        }
        if token.is_punct(';') && round == 0 && square == 0 && curly == 0 {
            return index;
        }
        match token.kind {
            TokenKind::Punct('(') => round += 1,
            TokenKind::Punct(')') => round -= 1,
            TokenKind::Punct('[') => square += 1,
            TokenKind::Punct(']') => square -= 1,
            TokenKind::Punct('{') => curly += 1,
            TokenKind::Punct('}') => curly -= 1,
            _ => {}
        }
        previous_line = token.line;
    }
    tokens.len()
}

fn discover_python_import_aliases(tokens: &[Token], module: &str, aliases: &mut BTreeSet<String>) {
    for index in 0..tokens.len() {
        if !tokens[index].is_ident("import")
            || !tokens
                .get(index + 1)
                .is_some_and(|token| token.is_ident(module))
        {
            continue;
        }
        if tokens
            .get(index + 2)
            .is_some_and(|token| token.is_ident("as"))
            && let Some(alias) = tokens.get(index + 3).and_then(Token::ident)
        {
            aliases.insert(alias.to_owned());
        }
    }
}

fn contains_any(tokens: &[Token], start: usize, end: usize, names: &BTreeSet<String>) -> bool {
    tokens.get(start..end).is_some_and(|slice| {
        slice
            .iter()
            .filter_map(Token::ident)
            .any(|name| names.contains(name))
    })
}

fn python_static_string_expression(
    tokens: &[Token],
    start: usize,
    end: usize,
    constants: &BTreeMap<String, String>,
) -> Option<String> {
    let mut output = String::new();
    let mut found = false;
    for token in tokens.get(start..end)? {
        if let Some(value) = token.literal() {
            output.push_str(value);
            found = true;
        } else if let Some(name) = token.ident() {
            output.push_str(constants.get(name)?);
            found = true;
        } else if !matches!(token.kind, TokenKind::Punct('+' | '(' | ')' | '[' | ']')) {
            return None;
        }
    }
    found.then_some(output)
}

fn python_functions(source: &str, tokens: &[Token]) -> Vec<FunctionSpan> {
    let line_count = saturating_u32(source.lines().count().max(1));
    let mut functions = Vec::new();
    for index in 0..tokens.len() {
        if !tokens[index].is_ident("def") {
            continue;
        }
        let Some(name) = tokens.get(index + 1).and_then(Token::ident) else {
            continue;
        };
        let indent = source_indent(source, tokens[index].line);
        let end_line = python_block_end(source, tokens[index].line, indent, line_count);
        let end_token = (index + 1..tokens.len())
            .find(|candidate| tokens[*candidate].line > end_line)
            .unwrap_or(tokens.len())
            .saturating_sub(1);
        functions.push(FunctionSpan {
            name: name.to_owned(),
            start_token: index,
            end_token,
        });
    }
    functions
}

fn source_indent(source: &str, line: u32) -> usize {
    source
        .lines()
        .nth(line.saturating_sub(1) as usize)
        .map_or(0, |value| {
            value
                .chars()
                .take_while(|character| character.is_whitespace())
                .count()
        })
}

fn python_block_end(source: &str, start: u32, indent: usize, fallback: u32) -> u32 {
    for (offset, line) in source.lines().enumerate().skip(start as usize) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let candidate_indent = line
            .chars()
            .take_while(|character| character.is_whitespace())
            .count();
        if candidate_indent <= indent {
            return saturating_u32(offset);
        }
    }
    fallback
}

fn parse_python_routes(
    tokens: &[Token],
    contexts: &PythonContexts,
    observations: &mut SourceObservationCollector<'_>,
) {
    for index in 0..tokens.len() {
        if !tokens[index].is_punct('@') {
            continue;
        }
        let Some((receiver, decorator, open)) = python_decorator_call(tokens, index) else {
            continue;
        };
        let Some(close) = matching(tokens, open, '(', ')') else {
            continue;
        };
        let Some((name, signature_end)) = python_function_after(tokens, close) else {
            continue;
        };
        let framework = if contexts.fastapi_apps.contains(receiver) {
            SourceFramework::FastApi
        } else if contexts.flask_apps.contains(receiver) {
            SourceFramework::Flask
        } else {
            continue;
        };
        let path_start = top_level_arguments(tokens, open, close).first().copied();
        let resolved_path = path_start.and_then(|start| {
            python_static_string_expression(
                tokens,
                start,
                python_argument_end(tokens, start, close),
                &contexts.string_constants,
            )
        });
        let raw_path = resolved_path.or_else(|| {
            tokens
                .get(open + 1)
                .and_then(Token::literal)
                .map(str::to_owned)
        });
        let prefixed_path = raw_path.as_ref().map(|path| {
            contexts.route_prefixes.get(receiver).map_or_else(
                || path.clone(),
                |prefix| normalize_source_http_path(&format!("{prefix}{path}")),
            )
        });
        let literal = prefixed_path.as_deref();
        let methods = python_route_methods(tokens, open, close, decorator, framework);
        if methods.is_empty() {
            observations.push(inexact_http(
                SourceLanguage::Python,
                framework,
                SourceRole::Provider,
                None,
                literal.and_then(route_literal_path),
                Some(name),
                SourceLineRange {
                    start: tokens[index].line,
                    end: tokens[signature_end].end_line,
                },
                SourceWarning::DynamicMethod,
            ));
        } else {
            for method in methods {
                observations.push(http_from_literal(
                    SourceLanguage::Python,
                    framework,
                    SourceRole::Provider,
                    Some(method),
                    literal,
                    Some(name.clone()),
                    SourceLineRange {
                        start: tokens[index].line,
                        end: tokens[signature_end].end_line,
                    },
                    true,
                ));
            }
        }
    }
}

fn python_decorator_call(tokens: &[Token], at: usize) -> Option<(&str, &str, usize)> {
    let mut names = Vec::new();
    let mut cursor = at.saturating_add(1);
    loop {
        names.push(tokens.get(cursor)?.ident()?);
        if tokens
            .get(cursor + 1)
            .is_some_and(|token| token.is_punct('.'))
        {
            cursor = cursor.saturating_add(2);
            continue;
        }
        break;
    }
    let open = cursor.saturating_add(1);
    if names.len() < 2 || !tokens.get(open).is_some_and(|token| token.is_punct('(')) {
        return None;
    }
    Some((names[names.len() - 2], names[names.len() - 1], open))
}

fn python_argument_end(tokens: &[Token], start: usize, close: usize) -> usize {
    let mut round = 0_i32;
    let mut square = 0_i32;
    let mut curly = 0_i32;
    for (index, token) in tokens.iter().enumerate().take(close).skip(start) {
        match token.kind {
            TokenKind::Punct('(') => round += 1,
            TokenKind::Punct(')') => round -= 1,
            TokenKind::Punct('[') => square += 1,
            TokenKind::Punct(']') => square -= 1,
            TokenKind::Punct('{') => curly += 1,
            TokenKind::Punct('}') => curly -= 1,
            TokenKind::Punct(',') if round == 0 && square == 0 && curly == 0 => return index,
            _ => {}
        }
    }
    close
}

fn python_function_after(tokens: &[Token], close: usize) -> Option<(String, usize)> {
    let mut index = close + 1;
    while index < tokens.len() && tokens[index].line <= tokens[close].line.saturating_add(12) {
        if tokens[index].is_ident("async") {
            index += 1;
            continue;
        }
        if tokens[index].is_ident("def") {
            let name = tokens.get(index + 1)?.ident()?.to_owned();
            let end = (index + 2..tokens.len())
                .find(|candidate| tokens[*candidate].is_punct(':'))
                .unwrap_or(index + 1);
            return Some((name, end));
        }
        if tokens[index].is_punct('@') {
            return None;
        }
        index += 1;
    }
    None
}

fn python_route_methods(
    tokens: &[Token],
    open: usize,
    close: usize,
    decorator: &str,
    framework: SourceFramework,
) -> BTreeSet<String> {
    if let Some(method) = canonical_method(decorator) {
        return BTreeSet::from([method.to_owned()]);
    }
    let mut methods = parse_keyword_string_values(tokens, open, close, "methods")
        .into_iter()
        .filter_map(|method| canonical_method(&method).map(str::to_owned))
        .collect::<BTreeSet<_>>();
    if framework == SourceFramework::Flask && decorator == "route" && methods.is_empty() {
        methods.insert("GET".to_owned());
    }
    methods
}

fn parse_python_http_registries(
    tokens: &[Token],
    observations: &mut SourceObservationCollector<'_>,
) {
    for index in 0..tokens.len() {
        let Some(name) = tokens[index].ident() else {
            continue;
        };
        if !name
            .chars()
            .all(|character| character.is_ascii_uppercase() || character == '_')
            || !tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('='))
            || !tokens
                .get(index + 2)
                .is_some_and(|token| token.is_punct('{'))
        {
            continue;
        }
        let open = index + 2;
        let Some(close) = matching(tokens, open, '{', '}') else {
            continue;
        };
        parse_python_http_registry_dict(tokens, open, close, "", observations, 0);
    }
}

fn parse_python_http_registry_dict(
    tokens: &[Token],
    open: usize,
    close: usize,
    prefix: &str,
    observations: &mut SourceObservationCollector<'_>,
    depth: usize,
) {
    if depth >= 32 {
        return;
    }
    let mut cursor = open.saturating_add(1);
    while cursor < close {
        while cursor < close && tokens[cursor].is_punct(',') {
            cursor += 1;
        }
        let Some(key) = tokens.get(cursor).and_then(Token::literal) else {
            cursor += 1;
            continue;
        };
        if !tokens
            .get(cursor + 1)
            .is_some_and(|token| token.is_punct(':'))
        {
            cursor += 1;
            continue;
        }
        let value_start = cursor + 2;
        let value_end = python_argument_end(tokens, value_start, close);
        parse_python_http_registry_value(
            tokens,
            value_start,
            value_end,
            prefix,
            key,
            observations,
            depth,
        );
        cursor = value_end.saturating_add(1);
    }
}

fn parse_python_http_registry_value(
    tokens: &[Token],
    start: usize,
    end: usize,
    prefix: &str,
    key: &str,
    observations: &mut SourceObservationCollector<'_>,
    depth: usize,
) {
    if tokens.get(start).is_some_and(|token| token.is_punct('[')) {
        let Some(close) = matching(tokens, start, '[', ']').filter(|close| *close <= end) else {
            return;
        };
        let arguments = top_level_arguments(tokens, start, close);
        let Some(method) = arguments
            .first()
            .and_then(|argument| tokens.get(*argument))
            .and_then(Token::literal)
            .and_then(canonical_method)
        else {
            return;
        };
        let Some(path) = arguments
            .get(1)
            .and_then(|argument| tokens.get(*argument))
            .and_then(Token::literal)
        else {
            return;
        };
        let combined = canonical_python_registry_path(prefix, path);
        observations.push(http_from_literal(
            SourceLanguage::Python,
            SourceFramework::PythonHttpRegistry,
            SourceRole::Consumer,
            Some(method.to_owned()),
            Some(&combined),
            Some(key.to_owned()),
            SourceLineRange {
                start: tokens[start].line,
                end: tokens[close].end_line,
            },
            false,
        ));
        return;
    }
    if !tokens.get(start).is_some_and(|token| token.is_punct('(')) {
        return;
    }
    let Some(close) = matching(tokens, start, '(', ')').filter(|close| *close <= end) else {
        return;
    };
    let arguments = top_level_arguments(tokens, start, close);
    let Some(segment) = arguments
        .first()
        .and_then(|argument| tokens.get(*argument))
        .and_then(Token::literal)
    else {
        return;
    };
    let Some(dictionary) = arguments.get(1).copied().filter(|argument| {
        tokens
            .get(*argument)
            .is_some_and(|token| token.is_punct('{'))
    }) else {
        return;
    };
    let Some(dictionary_close) = matching(tokens, dictionary, '{', '}') else {
        return;
    };
    let nested_prefix = canonical_python_registry_path(prefix, segment);
    parse_python_http_registry_dict(
        tokens,
        dictionary,
        dictionary_close,
        &nested_prefix,
        observations,
        depth.saturating_add(1),
    );
}

fn canonical_python_registry_path(prefix: &str, segment: &str) -> String {
    let joined = format!("{prefix}{segment}")
        .replace("{{", "{")
        .replace("}}", "}");
    normalize_source_http_path(&joined)
}

fn parse_python_factories(
    source: &str,
    tokens: &[Token],
    observations: &mut SourceObservationCollector<'_>,
) {
    let imported_paths = python_imported_symbol_paths(tokens);
    let fallback = saturating_u32(source.lines().count().max(1));
    for index in 0..tokens.len() {
        if !tokens[index].is_ident("class") {
            continue;
        }
        let Some(factory_name) = tokens.get(index + 1).and_then(Token::ident) else {
            continue;
        };
        let signature_end = (index + 2..tokens.len())
            .find(|candidate| tokens[*candidate].is_punct(':'))
            .unwrap_or(index + 1);
        if !tokens
            .get(index + 2..signature_end)
            .is_some_and(|signature| {
                signature
                    .iter()
                    .filter_map(Token::ident)
                    .any(|base| base == "Factory" || base.ends_with("Factory"))
            })
        {
            continue;
        }
        let start_line = tokens[index].line;
        let end_line = python_block_end(
            source,
            start_line,
            source_indent(source, start_line),
            fallback,
        );
        let model_assignment = (signature_end + 1..tokens.len())
            .take_while(|candidate| tokens[*candidate].line <= end_line)
            .find_map(|candidate| {
                if tokens[candidate].is_ident("model")
                    && tokens
                        .get(candidate + 1)
                        .is_some_and(|token| token.is_punct('='))
                {
                    Some((
                        tokens.get(candidate + 2).and_then(Token::ident)?,
                        tokens[candidate].line,
                    ))
                } else {
                    None
                }
            });
        let Some((model_name, model_line)) = model_assignment else {
            continue;
        };
        observations.push(SourceObservation {
            language: SourceLanguage::Python,
            framework: SourceFramework::FactoryBoy,
            role: SourceRole::Factory,
            method: None,
            path: None,
            symbol_name: Some(factory_name.to_owned()),
            related_symbol: Some(model_name.to_owned()),
            related_path: imported_paths.get(model_name).cloned(),
            lines: SourceLineRange {
                start: start_line,
                end: model_line,
            },
            status: SourceEpistemicStatus::Confirmed,
            confidence: 1.0,
            warnings: Vec::new(),
        });
    }
}

fn python_imported_symbol_paths(tokens: &[Token]) -> BTreeMap<String, String> {
    let mut output = BTreeMap::new();
    for index in 0..tokens.len() {
        if !tokens[index].is_ident("from") {
            continue;
        }
        let import_index = (index + 1..tokens.len())
            .take_while(|candidate| tokens[*candidate].line == tokens[index].line)
            .find(|candidate| tokens[*candidate].is_ident("import"));
        let Some(import_index) = import_index else {
            continue;
        };
        let module = tokens[index + 1..import_index]
            .iter()
            .filter_map(Token::ident)
            .collect::<Vec<_>>()
            .join("/");
        if module.is_empty() {
            continue;
        }
        let target_path = format!("{module}.py");
        let mut cursor = import_index + 1;
        while cursor < tokens.len() && tokens[cursor].line == tokens[index].line {
            let Some(imported) = tokens[cursor].ident() else {
                cursor += 1;
                continue;
            };
            let (local, advance) = if tokens
                .get(cursor + 1)
                .is_some_and(|token| token.is_ident("as"))
            {
                (
                    tokens
                        .get(cursor + 2)
                        .and_then(Token::ident)
                        .unwrap_or(imported),
                    3,
                )
            } else {
                (imported, 1)
            };
            output.insert(local.to_owned(), target_path.clone());
            cursor += advance;
        }
    }
    output
}

fn parse_python_http_calls(
    tokens: &[Token],
    functions: &[FunctionSpan],
    contexts: &PythonContexts,
    observations: &mut SourceObservationCollector<'_>,
) {
    let receivers = contexts
        .request_modules
        .iter()
        .chain(&contexts.request_clients)
        .map(|name| (name.as_str(), SourceFramework::Requests))
        .chain(
            contexts
                .httpx_modules
                .iter()
                .chain(&contexts.httpx_clients)
                .map(|name| (name.as_str(), SourceFramework::Httpx)),
        )
        .chain(
            contexts
                .aiohttp_clients
                .iter()
                .map(|name| (name.as_str(), SourceFramework::AioHttp)),
        )
        .collect::<BTreeMap<_, _>>();
    for index in 0..tokens.len() {
        let Some(receiver) = tokens[index].ident() else {
            continue;
        };
        let (framework, call, open) = if let Some(framework) = receivers.get(receiver).copied() {
            if !tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('.'))
            {
                continue;
            }
            let Some(call) = tokens.get(index + 2).and_then(Token::ident) else {
                continue;
            };
            (framework, call, index + 3)
        } else if let Some((framework, call)) = contexts.direct_calls.get(receiver) {
            (*framework, call.as_str(), index + 1)
        } else {
            continue;
        };
        if !tokens.get(open).is_some_and(|token| token.is_punct('(')) {
            continue;
        }
        let Some(close) = matching(tokens, open, '(', ')') else {
            continue;
        };
        let arguments = top_level_arguments(tokens, open, close);
        let (method, path_argument) = if call == "request" {
            (
                arguments
                    .first()
                    .and_then(|argument| tokens.get(*argument))
                    .and_then(Token::literal)
                    .and_then(canonical_method)
                    .map(str::to_owned),
                arguments.get(1).copied(),
            )
        } else {
            (
                canonical_method(call).map(str::to_owned),
                arguments.first().copied(),
            )
        };
        if method.is_none() && call != "request" {
            continue;
        }
        let resolved_path = path_argument.and_then(|argument| {
            python_static_string_expression(
                tokens,
                argument,
                python_argument_end(tokens, argument, close),
                &contexts.string_constants,
            )
        });
        let literal = resolved_path.as_deref().or_else(|| {
            path_argument
                .and_then(|argument| tokens.get(argument))
                .and_then(Token::literal)
        });
        let mut observation = http_from_literal(
            SourceLanguage::Python,
            framework,
            SourceRole::Consumer,
            method,
            literal,
            enclosing_symbol(functions, index),
            SourceLineRange {
                start: tokens[index].line,
                end: tokens[close].end_line,
            },
            false,
        );
        if call == "request" && observation.method.is_none() {
            observation.status = SourceEpistemicStatus::Incomplete;
            observation.confidence = 0.0;
            observation.warnings.push(SourceWarning::DynamicMethod);
            observation.warnings.sort();
            observation.warnings.dedup();
        }
        observations.push(observation);
    }
}

fn parse_python_tests(
    source: &str,
    tokens: &[Token],
    observations: &mut SourceObservationCollector<'_>,
) {
    let unittest_classes = python_unittest_classes(source, tokens);
    let python_classes = python_class_ranges(source, tokens);
    for index in 0..tokens.len() {
        if !tokens[index].is_ident("def") {
            continue;
        }
        let Some(name) = tokens.get(index + 1).and_then(Token::ident) else {
            continue;
        };
        if !name.starts_with("test_") {
            continue;
        }
        let signature_end = (index + 2..tokens.len())
            .find(|candidate| tokens[*candidate].is_punct(':'))
            .unwrap_or(index + 1);
        let line = tokens[index].line;
        let is_unittest = unittest_classes
            .iter()
            .any(|range| range.start <= line && line <= range.end);
        let indent = source_indent(source, line);
        let is_class_method = python_classes.iter().any(|(range, body_indent)| {
            range.start <= line && line <= range.end && indent == *body_indent
        });
        if !is_unittest && indent != 0 && !is_class_method {
            continue;
        }
        observations.push(confirmed_test(
            SourceLanguage::Python,
            if is_unittest {
                SourceFramework::Unittest
            } else if is_class_method {
                SourceFramework::PythonTest
            } else {
                SourceFramework::Pytest
            },
            name.to_owned(),
            line,
            tokens[signature_end].end_line,
        ));
    }
}

fn python_class_ranges(source: &str, tokens: &[Token]) -> Vec<(SourceLineRange, usize)> {
    let fallback = saturating_u32(source.lines().count().max(1));
    let mut classes = Vec::new();
    for token in tokens.iter().filter(|token| token.is_ident("class")) {
        let class_indent = source_indent(source, token.line);
        let end = python_block_end(source, token.line, class_indent, fallback);
        let body_indent = source
            .lines()
            .enumerate()
            .skip(usize::try_from(token.line).unwrap_or(usize::MAX))
            .take(usize::try_from(end.saturating_sub(token.line)).unwrap_or(usize::MAX))
            .find_map(|(index, line)| {
                let trimmed = line.trim();
                let line_number = saturating_u32(index + 1);
                let indent = source_indent(source, line_number);
                (!trimmed.is_empty() && !trimmed.starts_with('#') && indent > class_indent)
                    .then_some(indent)
            });
        if let Some(body_indent) = body_indent {
            classes.push((
                SourceLineRange {
                    start: token.line,
                    end,
                },
                body_indent,
            ));
        }
    }
    classes
}

fn python_unittest_classes(source: &str, tokens: &[Token]) -> Vec<SourceLineRange> {
    if !has_ident(tokens, "unittest") {
        return Vec::new();
    }
    let fallback = saturating_u32(source.lines().count().max(1));
    let mut classes = Vec::new();
    for index in 0..tokens.len() {
        if !tokens[index].is_ident("class") {
            continue;
        }
        let signature_end = (index + 1..tokens.len())
            .find(|candidate| tokens[*candidate].is_punct(':'))
            .unwrap_or(index);
        if !contains_ident(tokens, index + 1, signature_end, "TestCase") {
            continue;
        }
        let start = tokens[index].line;
        classes.push(SourceLineRange {
            start,
            end: python_block_end(source, start, source_indent(source, start), fallback),
        });
    }
    classes
}

fn saturating_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn finish(mut observations: Vec<SourceObservation>) -> Vec<SourceObservation> {
    for observation in &mut observations {
        observation.warnings.sort();
        observation.warnings.dedup();
    }
    observations.sort_by(compare_observations);
    observations.dedup();
    observations
}

fn compare_observations(left: &SourceObservation, right: &SourceObservation) -> Ordering {
    (
        left.lines,
        left.language,
        left.framework,
        left.role,
        &left.method,
        &left.path,
        &left.symbol_name,
        &left.related_symbol,
        &left.related_path,
        left.status,
        &left.warnings,
    )
        .cmp(&(
            right.lines,
            right.language,
            right.framework,
            right.role,
            &right.method,
            &right.path,
            &right.symbol_name,
            &right.related_symbol,
            &right.related_path,
            right.status,
            &right.warnings,
        ))
}

#[cfg(test)]
mod tests {
    use super::{
        SourceEpistemicStatus, SourceFramework, SourceRole, SourceWarning, parse_python_source, parse_rust_source
    };

    #[test]
    fn axum_should_extract_multiline_route_and_handler() {
        let source = r#"
use axum::{Router, routing::post};
fn router() {
    Router::new().route(
        "/api//orders/",
        post(create_order),
    );
}
"#;
        let result = parse_rust_source(source);

        assert!(matches!(
            result.as_slice(),
            [item]
                if item.framework == SourceFramework::Axum
                    && item.role == SourceRole::Provider
                    && item.method.as_deref() == Some("POST")
                    && item.path.as_deref() == Some("/api/orders")
                    && item.symbol_name.as_deref() == Some("create_order")
                    && item.lines.start == 4
                    && item.lines.end == 7
        ));
    }

    #[test]
    fn actix_attributes_should_extract_direct_and_route_methods() {
        let source = r#"
use actix_web::{get, route};
#[get("/health")]
async fn health() {}
#[route(
    "/orders",
    method = "POST",
)]
async fn create() {}
"#;
        let result = parse_rust_source(source);

        assert_eq!(
            result
                .iter()
                .map(|item| (item.method.as_deref(), item.path.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (Some("GET"), Some("/health")),
                (Some("POST"), Some("/orders"))
            ]
        );
    }

    #[test]
    fn actix_builder_should_require_web_route_evidence() {
        let source = r#"
use actix_web::{web, App};
fn app() {
    App::new().route("/users", web::put().to(update_user));
    App::new().service(
        web::resource("/orders").route(web::post().to(create_order)),
    );
}
"#;
        let result = parse_rust_source(source);

        assert_eq!(
            result
                .iter()
                .map(|item| (
                    item.framework,
                    item.method.as_deref(),
                    item.symbol_name.as_deref()
                ))
                .collect::<Vec<_>>(),
            vec![
                (SourceFramework::ActixWeb, Some("PUT"), Some("update_user")),
                (
                    SourceFramework::ActixWeb,
                    Some("POST"),
                    Some("create_order")
                ),
            ]
        );
    }

    #[test]
    fn utoipa_should_extract_explicit_and_contextual_actix_paths() {
        let source = r#"
use actix_web::{get, post};

#[cfg_attr(feature = "openapi", utoipa::path(post, path = "/api/orders"))]
#[post("/orders")]
async fn create_order() {}

#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    context_path = "/api/users"
))]
#[get("/{id}")]
async fn get_user() {}
"#;
        let result = parse_rust_source(source);

        assert!(result.iter().any(|item| {
            item.framework == SourceFramework::Utoipa
                && item.symbol_name.as_deref() == Some("create_order")
                && item.method.as_deref() == Some("POST")
                && item.path.as_deref() == Some("/api/orders")
        }));
        assert!(result.iter().any(|item| {
            item.framework == SourceFramework::Utoipa
                && item.symbol_name.as_deref() == Some("get_user")
                && item.method.as_deref() == Some("GET")
                && item.path.as_deref() == Some("/api/users/{id}")
        }));
        assert!(result.iter().any(|item| {
            item.framework == SourceFramework::ActixWeb
                && item.symbol_name.as_deref() == Some("create_order")
                && item.path.as_deref() == Some("/orders")
                && (item.confidence - 1.0).abs() < f32::EPSILON
        }));
        assert!(result.iter().any(|item| {
            item.framework == SourceFramework::Utoipa
                && item.symbol_name.as_deref() == Some("create_order")
                && (item.confidence - 0.75).abs() < f32::EPSILON
                && item.warnings.contains(&SourceWarning::AdvisoryDeclaration)
        }));
    }

    #[test]
    fn reqwest_should_extract_convenience_client_and_request_calls() {
        let source = r#"
use reqwest::{Client, Method};
async fn send() {
    reqwest::get("https://example.test/health?full=1").await;
    reqwest::blocking::get("https://example.test/ready");
    let client = Client::new();
    client.post("/orders").send().await;
    client.request(Method::DELETE, "https://example.test/orders/7").send().await;
}
"#;
        let result = parse_rust_source(source);

        assert_eq!(
            result
                .iter()
                .map(|item| (item.method.as_deref(), item.path.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (Some("GET"), Some("/health")),
                (Some("GET"), Some("/ready")),
                (Some("POST"), Some("/orders")),
                (Some("DELETE"), Some("/orders/7")),
            ]
        );
    }

    #[test]
    fn rust_test_attributes_should_cover_builtin_tokio_and_rstest() {
        let source = r"
#[test]
fn plain() {}
#[tokio::test]
async fn asynchronous() {}
#[rstest]
#[case(1)]
fn parameterized(#[case] value: u8) {}
";
        let result = parse_rust_source(source);

        assert_eq!(
            result
                .iter()
                .map(|item| (item.framework, item.symbol_name.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (SourceFramework::RustTest, Some("plain")),
                (SourceFramework::TokioTest, Some("asynchronous")),
                (SourceFramework::Rstest, Some("parameterized")),
            ]
        );
    }

    #[test]
    fn rust_dynamic_route_should_never_claim_exact_path() {
        let source = r"
use axum::{Router, routing::get};
fn router(path: &str) {
    Router::new().route(path, get(handler));
}
";
        let result = parse_rust_source(source);

        assert!(matches!(
            result.as_slice(),
            [item]
                if item.path.is_none()
                    && item.status == SourceEpistemicStatus::Ambiguous
                    && item.warnings == [SourceWarning::DynamicPath]
        ));
    }

    #[test]
    fn rust_comments_strings_and_unrelated_route_methods_should_be_ignored() {
        let source = r##"
// use axum; Router::new().route("/fake", get(fake));
const TEXT: &str = r#"reqwest::get("https://example.test/fake")"#;
struct Router;
impl Router {
    fn route(&self, path: &str, handler: usize) {}
}
fn ordinary() {
    Router.route("/fake", handler);
}
"##;
        let result = parse_rust_source(source);

        assert!(result.is_empty());
    }

    #[test]
    fn fastapi_should_extract_method_and_api_route_decorators() {
        let source = r#"
from fastapi import FastAPI
app = FastAPI()
@app.get("/users/{user_id}")
async def get_user(user_id: str):
    pass
@app.api_route(
    "/orders",
    methods=["POST", "PUT"],
)
def orders():
    pass
"#;
        let result = parse_python_source(source);

        assert_eq!(
            result
                .iter()
                .filter(|item| item.role == SourceRole::Provider)
                .map(|item| (item.method.as_deref(), item.path.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (Some("GET"), Some("/users/{user_id}")),
                (Some("POST"), Some("/orders")),
                (Some("PUT"), Some("/orders")),
            ]
        );
    }

    #[test]
    fn fastapi_should_compose_static_router_prefixes() {
        let source = r#"
from fastapi import APIRouter
router = APIRouter(prefix="/api/v1/projects")

@router.get("/{project_id}")
async def get_project(project_id: int):
    pass
"#;
        let result = parse_python_source(source);

        assert!(result.iter().any(|item| {
            item.framework == SourceFramework::FastApi
                && item.method.as_deref() == Some("GET")
                && item.path.as_deref() == Some("/api/v1/projects/{project_id}")
                && item.symbol_name.as_deref() == Some("get_project")
        }));
    }

    #[test]
    fn flask_should_extract_default_and_explicit_methods() {
        let source = r#"
from flask import Flask
app = Flask(__name__)
@app.route("/health")
def health():
    pass
@app.route(
    "/orders",
    methods=["POST", "DELETE"],
)
def orders():
    pass
"#;
        let result = parse_python_source(source);

        assert_eq!(
            result
                .iter()
                .filter(|item| item.role == SourceRole::Provider)
                .map(|item| item.method.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("GET"), Some("DELETE"), Some("POST")]
        );
    }

    #[test]
    fn flask_should_resolve_instance_app_and_static_path_concatenation() {
        let source = r#"
from flask import Flask
URL = "/servers/remitee"

class Remitee:
    def __init__(self):
        self.app = Flask(__name__)

    def setup_routes(self):
        @self.app.route(URL + "/api/Payments/<payment_id>", methods=["GET"])
        def payment(payment_id):
            pass
"#;
        let result = parse_python_source(source);

        assert!(result.iter().any(|item| {
            item.framework == SourceFramework::Flask
                && item.role == SourceRole::Provider
                && item.method.as_deref() == Some("GET")
                && item.path.as_deref() == Some("/servers/remitee/api/Payments/<payment_id>")
        }));
    }

    #[test]
    fn python_http_registry_should_flatten_nested_static_operations() {
        let source = r#"
METHODS = {
    "public": ("/api/v1", {
        "accounts": ("/accounts", {
            "get_accounts": ["GET", ""],
            "get_account": ["GET", "/{{accounts_id}}"],
        }),
    }),
}
"#;
        let result = parse_python_source(source);
        let operations = result
            .iter()
            .filter(|item| item.framework == SourceFramework::PythonHttpRegistry)
            .map(|item| (item.method.as_deref(), item.path.as_deref()))
            .collect::<Vec<_>>();

        assert_eq!(
            operations,
            vec![
                (Some("GET"), Some("/api/v1/accounts")),
                (Some("GET"), Some("/api/v1/accounts/{accounts_id}")),
            ]
        );
    }

    #[test]
    fn factory_boy_should_link_factory_to_imported_model() {
        let source = r"
import factory
from src.clases.Account import Account

class AccountFactory(factory.Factory):
    class Meta:
        model = Account
";
        let result = parse_python_source(source);

        assert!(result.iter().any(|item| {
            item.framework == SourceFramework::FactoryBoy
                && item.role == SourceRole::Factory
                && item.symbol_name.as_deref() == Some("AccountFactory")
                && item.related_symbol.as_deref() == Some("Account")
                && item.related_path.as_deref() == Some("src/clases/Account.py")
                && item.status == SourceEpistemicStatus::Confirmed
        }));
    }

    #[test]
    fn requests_should_extract_module_alias_and_session_calls() {
        let source = r#"
import requests as rq
from requests import Session, post as create
def send():
    rq.get("https://example.test/status")
    create("/orders")
    session = Session()
    session.request("PATCH", "/orders/4")
"#;
        let result = parse_python_source(source);

        assert_eq!(
            result
                .iter()
                .map(|item| (item.framework, item.method.as_deref(), item.path.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (SourceFramework::Requests, Some("GET"), Some("/status")),
                (SourceFramework::Requests, Some("POST"), Some("/orders")),
                (SourceFramework::Requests, Some("PATCH"), Some("/orders/4")),
            ]
        );
    }

    #[test]
    fn aiohttp_should_extract_client_session_context_calls() {
        let source = r#"
from aiohttp import ClientSession

async def load():
    async with ClientSession() as session:
        await session.get("/accounts")
"#;
        let result = parse_python_source(source);

        assert!(result.iter().any(|item| {
            item.framework == SourceFramework::AioHttp
                && item.role == SourceRole::Consumer
                && item.method.as_deref() == Some("GET")
                && item.path.as_deref() == Some("/accounts")
        }));
    }

    #[test]
    fn httpx_should_extract_module_sync_and_async_clients() {
        let source = r#"
import httpx
from httpx import AsyncClient, patch as update
def direct():
    httpx.delete("https://example.test/items/1")
    update("/items/1")
async def clients():
    client = AsyncClient()
    client.put("/items/1")
"#;
        let result = parse_python_source(source);

        assert_eq!(
            result
                .iter()
                .map(|item| (item.framework, item.method.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (SourceFramework::Httpx, Some("DELETE")),
                (SourceFramework::Httpx, Some("PATCH")),
                (SourceFramework::Httpx, Some("PUT")),
            ]
        );
    }

    #[test]
    fn python_tests_should_distinguish_pytest_and_unittest() {
        let source = r"
import unittest
def test_pytest_case():
    pass
class ApiTests(unittest.TestCase):
    def test_unittest_case(self):
        pass
";
        let result = parse_python_source(source);

        assert_eq!(
            result
                .iter()
                .map(|item| (item.framework, item.symbol_name.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (SourceFramework::Pytest, Some("test_pytest_case")),
                (SourceFramework::Unittest, Some("test_unittest_case")),
            ]
        );
    }

    #[test]
    fn indirect_python_test_class_should_preserve_generic_test_evidence() {
        let source = r#"
import requests

class PaymentCases(SharedTestBase):
    def test_payment(self):
        requests.post("/payments")
"#;
        let result = parse_python_source(source);

        assert!(result.iter().any(|item| {
            item.framework == SourceFramework::PythonTest
                && item.role == SourceRole::Test
                && item.symbol_name.as_deref() == Some("test_payment")
        }));
        assert!(result.iter().any(|item| {
            item.framework == SourceFramework::Requests
                && item.role == SourceRole::Consumer
                && item.symbol_name.as_deref() == Some("test_payment")
                && item.path.as_deref() == Some("/payments")
        }));
    }

    #[test]
    fn python_dynamic_url_and_method_should_remain_explicitly_incomplete() {
        let source = r"
import httpx
def send(method, base, path):
    httpx.request(method, base + path)
";
        let result = parse_python_source(source);

        assert!(matches!(
            result.as_slice(),
            [item]
                if item.method.is_none()
                    && item.path.is_none()
                    && item.status == SourceEpistemicStatus::Incomplete
                    && item.warnings
                        == [SourceWarning::DynamicMethod, SourceWarning::DynamicPath]
        ));
    }

    #[test]
    fn python_f_strings_should_be_dynamic_not_exact() {
        let source = r#"
import requests
def fetch(user_id):
    requests.get(f"https://example.test/users/{user_id}")
"#;
        let result = parse_python_source(source);

        assert!(matches!(
            result.as_slice(),
            [item]
                if item.path.is_none()
                    && item.status == SourceEpistemicStatus::Ambiguous
                    && item.warnings == [SourceWarning::DynamicPath]
        ));
    }

    #[test]
    fn python_comments_docstrings_and_unknown_clients_should_be_ignored() {
        let source = r#"
"""@app.get("/fake")
def test_fake(): pass
requests.get("https://example.test/fake")
"""
# import requests
# requests.post("/fake")
class Client:
    def get(self, path):
        pass
client = Client()
client.get("/not-httpx")
"#;
        let result = parse_python_source(source);

        assert!(result.is_empty());
    }

    #[test]
    fn ordering_should_be_source_stable_and_independent_of_method_list_order() {
        let source = r#"
from fastapi import FastAPI
app = FastAPI()
@app.api_route("/z", methods=["PUT", "GET", "POST"])
def endpoint():
    pass
"#;
        let first = parse_python_source(source);
        let second = parse_python_source(source);

        assert_eq!(
            (
                first
                    .iter()
                    .map(|item| item.method.as_deref())
                    .collect::<Vec<_>>(),
                &first
            ),
            ((vec![Some("GET"), Some("POST"), Some("PUT")]), &second)
        );
    }
}

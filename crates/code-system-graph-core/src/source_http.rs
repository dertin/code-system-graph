//! Dependency-free, evidence-first extraction of focused HTTP and test declarations.
//!
//! The parsers in this module recognize only framework-specific syntax and only promote literal
//! methods and paths to confirmed observations. Dynamic expressions are retained as ambiguous or
//! incomplete observations so downstream linking cannot mistake missing evidence for an exact
//! contract.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::router_mounts::mount_observation;
use crate::{ExtractionLimitExceeded, ExtractionTracker};

mod brace_clients;
mod brace_flows;
mod brace_lexer;
mod brace_tests;
mod python_flows;
mod rust_bindings;
mod rust_flows;
mod rust_routers;
mod rust_test_clients;

pub(crate) use brace_clients::{BraceClients, collect_brace_clients, java_method_lines};
use python_flows::{PythonScopes, keyword_argument, record_fixture_requests};
use rust_flows::RustScopes;
use rust_routers::{RustRouters, join_prefixes};

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
    /// Spring `RestTemplate` calls.
    RestTemplate,
    /// Starlette and `FastAPI` `TestClient`, and HTTPX clients bound to an ASGI or WSGI app.
    TestClient,
    /// Flask `app.test_client()` requests.
    FlaskTestClient,
    /// Jest tests.
    Jest,
    /// Vitest tests.
    Vitest,
    /// Mocha tests.
    Mocha,
    /// Playwright tests and `APIRequestContext` requests.
    Playwright,
    /// Supertest `request(app)` requests.
    Supertest,
    /// Go `testing` test functions.
    GoTest,
    /// Go `net/http/httptest` requests and servers.
    Httptest,
    /// `JUnit` `@Test` methods.
    JUnit,
    /// Spring `MockMvc` requests.
    MockMvc,
    /// REST Assured requests.
    RestAssured,
    /// Spring `WebTestClient` requests.
    WebTestClient,
    /// Spring Boot `TestRestTemplate` requests.
    TestRestTemplate,
    /// Axum `Router` requests sent with `tower::ServiceExt::oneshot`.
    AxumOneshot,
    /// Actix Web `test::TestRequest` requests.
    ActixTest,
}

impl SourceFramework {
    /// Whether requests of this client run in-process against the application of their own
    /// repository.
    #[must_use]
    pub fn is_in_process_client(self) -> bool {
        matches!(
            self,
            Self::TestClient
                | Self::FlaskTestClient
                | Self::Supertest
                | Self::Httptest
                | Self::MockMvc
                | Self::RestAssured
                | Self::WebTestClient
                | Self::TestRestTemplate
                | Self::AxumOneshot
                | Self::ActixTest
        )
    }
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
    /// A router mounted under a path prefix on another router or on the application root.
    Mount,
    /// A call from the enclosing function to another function of the repository.
    Call,
    /// A function, such as a pytest fixture, that returns an in-process test client.
    Client,
}

/// Source-level reference to a router or function, resolved per repository.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolRef {
    /// A router bound to a name in the declaring file; `default` names a default export.
    Local(String),
    /// A router imported from another module, with the module specifier as written.
    Import {
        /// Module specifier, such as `./routes/users` or `app.api.users`.
        module: String,
        /// Imported name; `default` for default exports.
        name: String,
    },
    /// The router built, returned, or configured by a function defined in the declaring file.
    Function(String),
    /// A function referenced by its path as written, such as `users::routes` or `routes.Register`.
    Call(String),
    /// A pytest fixture injected by parameter name, declared in the same file or in the nearest
    /// enclosing `conftest.py`.
    Fixture(String),
    /// A parameter of the enclosing function, by name and zero-based position after receivers.
    Parameter {
        /// Declared parameter name.
        name: String,
        /// Position among the parameters a caller passes.
        index: usize,
    },
}

/// Client URL assembled from literal text and runtime values.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct UrlTemplate {
    /// Parts in source order; adjacent text parts are merged.
    pub parts: Vec<UrlPart>,
}

/// One part of a [`UrlTemplate`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UrlPart {
    /// Literal text.
    Text(String),
    /// A parameter of the enclosing function, by name and zero-based position after receivers.
    Parameter {
        /// Declared parameter name.
        name: String,
        /// Position among the parameters a caller passes.
        index: usize,
    },
    /// Any other runtime value, named when it is a plain identifier.
    Value(Option<String>),
}

/// Call recorded for repository-level client-wrapper and test-helper correlation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CallSite {
    /// Called function.
    pub callee: SymbolRef,
    /// Arguments in source order, up to the last string expression.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<CallArgument>,
}

/// One argument of a [`CallSite`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct CallArgument {
    /// Keyword under which the argument is passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keyword: Option<String>,
    /// String value; `None` when the argument is not a string expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<UrlTemplate>,
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
    /// Lower-case `host[:port]` named by an absolute consumer URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<String>,
    /// Router a provider route is registered on, the router mounted by a mount observation, or
    /// the parameter through which a consumer receives its client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub router: Option<SymbolRef>,
    /// Router receiving a mount observation; `None` mounts at the application root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_parent: Option<SymbolRef>,
    /// Consumer URL that depends on parameters of the enclosing function.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<UrlTemplate>,
    /// Called function and arguments of a call observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call: Option<CallSite>,
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

    pub(crate) fn has_role(&self, role: SourceRole) -> bool {
        self.observations
            .iter()
            .any(|observation| observation.role == role)
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
    let call_values = observation
        .call
        .iter()
        .flat_map(|call| &call.arguments)
        .filter_map(|argument| argument.value.as_ref());
    for template in observation.url.iter().chain(call_values) {
        for part in &template.parts {
            match part {
                UrlPart::Text(text) => tracker.charge_string(text)?,
                UrlPart::Parameter { name, .. } | UrlPart::Value(Some(name)) => {
                    tracker.charge_identifier(name)?;
                }
                UrlPart::Value(None) => {}
            }
        }
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
    let routers = RustRouters::new(&tokens, &functions);
    parse_rust_attributes(&tokens, &mut observations);
    if has_ident(&tokens, "axum") {
        parse_axum_routes(&tokens, &routers, &mut observations);
        for mount in routers.mounts(false) {
            observations.push(mount);
        }
    }
    if has_ident(&tokens, "actix_web") {
        parse_actix_builder_routes(&tokens, &routers, &mut observations);
        for mount in routers.mounts(true) {
            observations.push(mount);
        }
    }
    let scopes = RustScopes::new(&tokens, &functions);
    rust_test_clients::parse_rust_test_requests(&tokens, &functions, &scopes, &mut observations);
    let clients = if has_ident(&tokens, "reqwest") {
        let receivers = rust_bindings::reqwest_receiver_tokens(&tokens, &functions);
        parse_reqwest_calls(&tokens, &functions, &receivers, &scopes, &mut observations);
        receivers
    } else {
        BTreeSet::new()
    };
    let declares_tests = observations.has_role(SourceRole::Test) || has_ident(&tokens, "test");
    scopes.record_calls(&clients, declares_tests, &mut observations);
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
    let scopes = PythonScopes::new(&tokens, &functions);
    parse_python_routes(&tokens, &contexts, &mut observations);
    parse_python_router_mounts(&tokens, &contexts, &mut observations);
    parse_python_http_registries(&tokens, &mut observations);
    parse_python_route_calls(&tokens, &contexts, &mut observations);
    parse_python_http_calls(&tokens, &functions, &contexts, &scopes, &mut observations);
    parse_python_client_fixtures(&tokens, &functions, &contexts, &mut observations);
    parse_python_factories(source, &tokens, &mut observations);
    parse_python_tests(source, &tokens, &mut observations);
    let clients = contexts
        .request_modules
        .iter()
        .chain(&contexts.request_clients)
        .chain(&contexts.httpx_modules)
        .chain(&contexts.httpx_clients)
        .chain(&contexts.aiohttp_modules)
        .chain(&contexts.aiohttp_clients)
        .chain(contexts.direct_calls.keys())
        .chain(contexts.test_clients.keys())
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let declares_tests = has_ident(&tokens, "pytest")
        || has_ident(&tokens, "unittest")
        || functions
            .iter()
            .any(|function| function.name.starts_with("test_"));
    scopes.record_calls(
        &python_router_imports(&tokens),
        &clients,
        declares_tests,
        &mut observations,
    );
    record_fixture_requests(&scopes, &mut observations);
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
    /// Raw body of a JavaScript template literal, including `${...}` substitutions.
    Template(String),
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
    body_start_token: usize,
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
            '\'' if rust_char_literal_end(&characters, index).is_some() => {
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

fn rust_char_literal_end(characters: &[char], index: usize) -> Option<usize> {
    let value = index.checked_add(1)?;
    let closing = if characters.get(value) == Some(&'\\') {
        match characters.get(value + 1)? {
            'u' if characters.get(value + 2) == Some(&'{') => {
                (value + 3..characters.len()).find(|cursor| characters[*cursor] == '}')? + 1
            }
            'x' => value.checked_add(4)?,
            _ => value.checked_add(2)?,
        }
    } else {
        value.checked_add(1)?
    };
    (characters.get(closing) == Some(&'\'')).then_some(closing + 1)
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
            body_start_token: open,
            end_token: end,
        });
    }
    functions
}

/// Position of the smallest function span containing token `index`.
fn innermost_function(functions: &[FunctionSpan], index: usize) -> Option<usize> {
    functions
        .iter()
        .enumerate()
        .filter(|(_, function)| function.start_token <= index && index <= function.end_token)
        .min_by_key(|(_, function)| function.end_token - function.start_token)
        .map(|(position, _)| position)
}

/// Splits `start..end` at top-level `separator` punctuation.
fn split_operands(
    tokens: &[Token],
    start: usize,
    end: usize,
    separator: char,
) -> Vec<(usize, usize)> {
    let mut operands = Vec::new();
    let mut depth = 0_i32;
    let mut operand_start = start;
    for (index, token) in tokens.iter().enumerate().take(end).skip(start) {
        match token.kind {
            TokenKind::Punct('(' | '[' | '{') => depth += 1,
            TokenKind::Punct(')' | ']' | '}') => depth -= 1,
            TokenKind::Punct(character) if character == separator && depth == 0 => {
                operands.push((operand_start, index));
                operand_start = index + 1;
            }
            _ => {}
        }
    }
    operands.push((operand_start, end));
    operands
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
            [first, second]
                if (first == "actix_web" || first == "actix_rt") && second == "test" =>
            {
                Some(SourceFramework::RustTest)
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
        let mut observation = http_from_literal(
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
        );
        observation.router = Some(SymbolRef::Function(symbol.clone()));
        observations.push(observation);
    }
}

fn parse_axum_routes(
    tokens: &[Token],
    routers: &RustRouters<'_>,
    observations: &mut SourceObservationCollector<'_>,
) {
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
        let router = routers.chain_owner(index - 1).router;
        let methods = method_calls(tokens, arguments[1], close);
        for (method, handler) in methods {
            let mut observation = http_from_literal(
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
            );
            observation.router = Some(router.clone());
            observations.push(observation);
        }
    }
}

fn parse_actix_builder_routes(
    tokens: &[Token],
    routers: &RustRouters<'_>,
    observations: &mut SourceObservationCollector<'_>,
) {
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
        let owner =
            (index > 0 && tokens[index - 1].is_punct('.')).then(|| routers.chain_owner(index - 1));
        let literal = tokens.get(arguments[0]).and_then(Token::literal);
        let literal = match owner.as_ref().and_then(|owner| owner.prefix.as_deref()) {
            Some(prefix) => join_prefixes(Some(prefix), literal),
            None => literal.map(str::to_owned),
        };
        for (method, handler) in method_calls(tokens, arguments[1], close) {
            let mut observation = http_from_literal(
                SourceLanguage::Rust,
                SourceFramework::ActixWeb,
                SourceRole::Provider,
                Some(method),
                literal.as_deref(),
                handler,
                SourceLineRange {
                    start: token.line,
                    end: tokens[close].end_line,
                },
                true,
            );
            observation.router = owner.as_ref().map(|owner| owner.router.clone());
            observations.push(observation);
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
        let router = tokens
            .get(resource_close + 1)
            .is_some_and(|token| token.is_punct('.'))
            .then(|| routers.chain_owner(resource_close + 1).router);
        for (method, handler) in method_calls(tokens, resource_close + 1, end) {
            let mut observation = http_from_literal(
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
            );
            observation.router.clone_from(&router);
            observations.push(observation);
        }
    }
}

fn parse_reqwest_calls(
    tokens: &[Token],
    functions: &[FunctionSpan],
    reqwest_receivers: &BTreeSet<usize>,
    scopes: &RustScopes<'_>,
    observations: &mut SourceObservationCollector<'_>,
) {
    for index in 0..tokens.len() {
        let Some(method_index) = reqwest_method_index(tokens, index, reqwest_receivers) else {
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
        let template = path_argument.map(|argument| {
            let end = arguments
                .iter()
                .find(|start| **start > argument)
                .map_or(close, |next| next - 1);
            scopes.template(argument, end, index)
        });
        let literal = template.as_ref().and_then(UrlTemplate::client_literal);
        let mut observation = http_from_literal(
            SourceLanguage::Rust,
            SourceFramework::Reqwest,
            SourceRole::Consumer,
            method,
            literal.as_deref(),
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
        observation.url = template.filter(UrlTemplate::has_parameters);
        observations.push(observation);
    }
}

/// Index of the method name called at token `index` on the `reqwest` or `reqwest::blocking`
/// module, or on a Reqwest client receiver.
fn reqwest_method_index(
    tokens: &[Token],
    index: usize,
    reqwest_receivers: &BTreeSet<usize>,
) -> Option<usize> {
    let path_separator = |at: usize| {
        tokens.get(at).is_some_and(|token| token.is_punct(':'))
            && tokens.get(at + 1).is_some_and(|token| token.is_punct(':'))
    };
    if tokens[index].is_ident("reqwest") && path_separator(index + 1) {
        let direct = index + 3;
        let blocking = tokens
            .get(direct)
            .is_some_and(|token| token.is_ident("blocking"))
            && path_separator(direct + 1);
        return Some(if blocking { direct + 3 } else { direct });
    }
    (reqwest_receivers.contains(&index)
        && tokens
            .get(index + 1)
            .is_some_and(|token| token.is_punct('.')))
    .then_some(index + 2)
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

/// Call from function `function` recorded for repository-level flow composition.
pub(crate) fn call_observation(
    language: SourceLanguage,
    callee: SymbolRef,
    arguments: Vec<CallArgument>,
    function: String,
    lines: SourceLineRange,
) -> SourceObservation {
    let framework = match language {
        SourceLanguage::Python => SourceFramework::PythonTest,
        SourceLanguage::Rust => SourceFramework::RustTest,
        SourceLanguage::TypeScript | SourceLanguage::JavaScript => SourceFramework::Fetch,
        SourceLanguage::Go => SourceFramework::GoNetHttp,
        SourceLanguage::Java => SourceFramework::SpringMvc,
    };
    SourceObservation {
        language,
        framework,
        role: SourceRole::Call,
        method: None,
        path: None,
        symbol_name: Some(function),
        related_symbol: None,
        related_path: None,
        authority: None,
        router: None,
        mount_parent: None,
        url: None,
        call: Some(CallSite { callee, arguments }),
        lines,
        status: SourceEpistemicStatus::Confirmed,
        confidence: 1.0,
        warnings: Vec::new(),
    }
}

/// Confirmed consumer of `template` issued from `caller`, with the client coordinates of
/// `origin`; `None` when the template does not yield an exact method and path.
pub(crate) fn instantiated_consumer(
    origin: &SourceObservation,
    template: &UrlTemplate,
    caller: &str,
    lines: SourceLineRange,
) -> Option<SourceObservation> {
    let literal = template.client_literal()?;
    let observation = http_from_literal(
        origin.language,
        origin.framework,
        SourceRole::Consumer,
        origin.method.clone(),
        Some(&literal),
        Some(caller.to_owned()),
        lines,
        false,
    );
    (observation.status == SourceEpistemicStatus::Confirmed).then_some(observation)
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
        authority: None,
        router: None,
        mount_parent: None,
        url: None,
        call: None,
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
    let (path, authority) = literal.map_or((None, None), |value| {
        if route {
            (route_literal_path(value), None)
        } else {
            client_literal_identity(value)
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
        authority,
        router: None,
        mount_parent: None,
        url: None,
        call: None,
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
        authority: None,
        router: None,
        mount_parent: None,
        url: None,
        call: None,
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

fn client_literal_identity(value: &str) -> (Option<String>, Option<String>) {
    if value.starts_with('/') {
        return (
            Some(normalize_source_http_path(
                value.split(['?', '#']).next().unwrap_or(value),
            )),
            None,
        );
    }
    let after_authority = value
        .strip_prefix("http://")
        .or_else(|| value.strip_prefix("https://"));
    let Some(after_authority) = after_authority else {
        return (None, None);
    };
    if after_authority.is_empty() || after_authority.starts_with('/') {
        return (None, None);
    }
    let separator = after_authority.find('/');
    let path = separator.map_or("/", |index| &after_authority[index..]);
    (
        Some(normalize_source_http_path(
            path.split(['?', '#']).next().unwrap_or(path),
        )),
        crate::routes::url_authority(value),
    )
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
    test_clients: BTreeMap<String, SourceFramework>,
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
            if let Some(framework) = python_test_client(tokens, index + 2, end) {
                contexts.test_clients.insert(name.to_owned(), framework);
                continue;
            }
            if contains_ident(tokens, index + 2, end, "FastAPI")
                || contains_ident(tokens, index + 2, end, "APIRouter")
            {
                contexts.fastapi_apps.insert(name.to_owned());
                if contains_ident(tokens, index + 2, end, "APIRouter")
                    && let Some(prefix) =
                        python_constructor_prefix(tokens, index + 2, end, "prefix")
                {
                    contexts.route_prefixes.insert(name.to_owned(), prefix);
                }
            }
            if contains_ident(tokens, index + 2, end, "Flask")
                || contains_ident(tokens, index + 2, end, "Blueprint")
            {
                contexts.flask_apps.insert(name.to_owned());
                if contains_ident(tokens, index + 2, end, "Blueprint")
                    && let Some(prefix) =
                        python_constructor_prefix(tokens, index + 2, end, "url_prefix")
                {
                    contexts.route_prefixes.insert(name.to_owned(), prefix);
                }
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
        contexts.discover_with_targets(tokens, aiohttp_client_imported);
        contexts
    }

    /// Clients bound by `with ... as NAME`.
    fn discover_with_targets(&mut self, tokens: &[Token], aiohttp_client_imported: bool) {
        for index in 0..tokens.len() {
            if !tokens[index].is_ident("as") {
                continue;
            }
            let Some(name) = tokens.get(index + 1).and_then(Token::ident) else {
                continue;
            };
            let start = index.saturating_sub(32);
            let with = (start..index)
                .rev()
                .find(|candidate| tokens[*candidate].is_ident("with"))
                .unwrap_or(start);
            if let Some(framework) = python_test_client(tokens, with, index) {
                self.test_clients.insert(name.to_owned(), framework);
            } else if aiohttp_client_imported
                && tokens[start..index]
                    .iter()
                    .any(|token| token.is_ident("ClientSession"))
            {
                self.aiohttp_clients.insert(name.to_owned());
            }
        }
    }

    /// Client receivers and the framework of each.
    fn http_receivers(&self) -> BTreeMap<&str, SourceFramework> {
        self.request_modules
            .iter()
            .chain(&self.request_clients)
            .map(|name| (name.as_str(), SourceFramework::Requests))
            .chain(
                self.httpx_modules
                    .iter()
                    .chain(&self.httpx_clients)
                    .map(|name| (name.as_str(), SourceFramework::Httpx)),
            )
            .chain(
                self.aiohttp_clients
                    .iter()
                    .map(|name| (name.as_str(), SourceFramework::AioHttp)),
            )
            .chain(
                self.test_clients
                    .iter()
                    .map(|(name, framework)| (name.as_str(), *framework)),
            )
            .collect()
    }
}

/// In-process test client constructed by the expression in `start..end`.
fn python_test_client(tokens: &[Token], start: usize, end: usize) -> Option<SourceFramework> {
    if contains_ident(tokens, start, end, "TestClient") {
        return Some(SourceFramework::TestClient);
    }
    let flask = (start.max(1)..end).any(|index| {
        tokens[index].is_ident("test_client")
            && tokens[index - 1].is_punct('.')
            && tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('('))
    });
    if flask {
        return Some(SourceFramework::FlaskTestClient);
    }
    let httpx = contains_ident(tokens, start, end, "AsyncClient")
        || contains_ident(tokens, start, end, "Client");
    let in_process = contains_ident(tokens, start, end, "ASGITransport")
        || contains_ident(tokens, start, end, "WSGITransport")
        || (start.max(1)..end).any(|index| {
            tokens[index].is_ident("app")
                && (tokens[index - 1].is_punct('(') || tokens[index - 1].is_punct(','))
                && tokens
                    .get(index + 1)
                    .is_some_and(|token| token.is_punct('='))
        });
    (httpx && in_process).then_some(SourceFramework::TestClient)
}

/// Functions, such as pytest fixtures, that return or yield an in-process test client.
fn parse_python_client_fixtures(
    tokens: &[Token],
    functions: &[FunctionSpan],
    contexts: &PythonContexts,
    observations: &mut SourceObservationCollector<'_>,
) {
    for index in 0..tokens.len() {
        if !tokens[index].is_ident("return") && !tokens[index].is_ident("yield") {
            continue;
        }
        let Some(function) = innermost_function(functions, index) else {
            continue;
        };
        let end = python_expression_end(tokens, index + 1);
        let named = tokens
            .get(index + 1)
            .and_then(Token::ident)
            .filter(|_| end == index + 2)
            .and_then(|name| contexts.test_clients.get(name).copied());
        let Some(framework) = named.or_else(|| python_test_client(tokens, index + 1, end)) else {
            continue;
        };
        let mut observation = confirmed_test(
            SourceLanguage::Python,
            framework,
            functions[function].name.clone(),
            tokens[index].line,
            tokens[index].end_line,
        );
        observation.role = SourceRole::Client;
        observations.push(observation);
    }
}

/// Normalized `keyword=` string of the first call between `start` and `end`.
fn python_constructor_prefix(
    tokens: &[Token],
    start: usize,
    end: usize,
    keyword: &str,
) -> Option<String> {
    let open = (start..end).find(|candidate| tokens[*candidate].is_punct('('))?;
    let close = matching(tokens, open, '(', ')')?;
    let prefix = parse_keyword_string_values(tokens, open, close, keyword)
        .into_iter()
        .next()?;
    Some(normalize_source_http_path(&prefix))
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
            body_start_token: index,
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
        let router = Some(SymbolRef::Local(receiver.to_owned()));
        if methods.is_empty() {
            let mut observation = inexact_http(
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
            );
            observation.router = router;
            observations.push(observation);
        } else {
            for method in methods {
                let mut observation = http_from_literal(
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
                );
                observation.router.clone_from(&router);
                observations.push(observation);
            }
        }
    }
}

/// Records `include_router` and `register_blueprint` mounts on known applications and routers.
fn parse_python_router_mounts(
    tokens: &[Token],
    contexts: &PythonContexts,
    observations: &mut SourceObservationCollector<'_>,
) {
    let imports = python_router_imports(tokens);
    for index in 0..tokens.len().saturating_sub(3) {
        let Some(receiver) = tokens[index].ident() else {
            continue;
        };
        let (framework, keyword) = match tokens[index + 2].ident() {
            Some("include_router") if contexts.fastapi_apps.contains(receiver) => {
                (SourceFramework::FastApi, "prefix")
            }
            Some("register_blueprint") if contexts.flask_apps.contains(receiver) => {
                (SourceFramework::Flask, "url_prefix")
            }
            _ => continue,
        };
        if !tokens[index + 1].is_punct('.') || !tokens[index + 3].is_punct('(') {
            continue;
        }
        let open = index + 3;
        let Some(close) = matching(tokens, open, '(', ')') else {
            continue;
        };
        let Some(&first) = top_level_arguments(tokens, open, close).first() else {
            continue;
        };
        let mut dotted = Vec::new();
        let mut cursor = first;
        while let Some(name) = tokens.get(cursor).and_then(Token::ident) {
            dotted.push(name);
            if !tokens
                .get(cursor + 1)
                .is_some_and(|token| token.is_punct('.'))
            {
                break;
            }
            cursor += 2;
        }
        let Some(child) = python_router_reference(&dotted, contexts, &imports) else {
            continue;
        };
        let prefix = parse_keyword_string_values(tokens, open, close, keyword)
            .into_iter()
            .next();
        observations.push(mount_observation(
            SourceLanguage::Python,
            framework,
            child,
            Some(SymbolRef::Local(receiver.to_owned())),
            prefix.as_deref(),
            SourceLineRange {
                start: tokens[index].line,
                end: tokens[close].end_line,
            },
        ));
    }
}

/// Routes registered by `FastAPI` `add_api_route(path, endpoint)` and Flask
/// `add_url_rule(rule, endpoint, view_func)` calls.
fn parse_python_route_calls(
    tokens: &[Token],
    contexts: &PythonContexts,
    observations: &mut SourceObservationCollector<'_>,
) {
    for index in 0..tokens.len().saturating_sub(3) {
        let Some(receiver) = tokens[index].ident() else {
            continue;
        };
        if !tokens[index + 1].is_punct('.') || !tokens[index + 3].is_punct('(') {
            continue;
        }
        let (framework, path_keyword, handler_keyword, handler_position) =
            match tokens[index + 2].ident() {
                Some("add_api_route") if contexts.fastapi_apps.contains(receiver) => {
                    (SourceFramework::FastApi, "path", "endpoint", 1)
                }
                Some("add_url_rule") if contexts.flask_apps.contains(receiver) => {
                    (SourceFramework::Flask, "rule", "view_func", 2)
                }
                _ => continue,
            };
        let open = index + 3;
        let Some(close) = matching(tokens, open, '(', ')') else {
            continue;
        };
        let arguments = top_level_arguments(tokens, open, close)
            .into_iter()
            .map(|start| {
                let end = python_argument_end(tokens, start, close);
                match keyword_argument(tokens, start, end) {
                    Some((keyword, value)) => (Some(keyword), value, end),
                    None => (None, start, end),
                }
            })
            .collect::<Vec<_>>();
        let positional = arguments
            .iter()
            .filter(|(keyword, ..)| keyword.is_none())
            .collect::<Vec<_>>();
        let path = arguments
            .iter()
            .find(|(keyword, ..)| keyword.as_deref() == Some(path_keyword))
            .or_else(|| positional.first().copied())
            .and_then(|(_, start, end)| {
                python_static_string_expression(tokens, *start, *end, &contexts.string_constants)
            });
        let handler = arguments
            .iter()
            .find(|(keyword, ..)| keyword.as_deref() == Some(handler_keyword))
            .or_else(|| positional.get(handler_position).copied())
            .and_then(|(_, start, end)| {
                (*start..*end)
                    .map(|token| {
                        tokens[token]
                            .ident()
                            .or_else(|| tokens[token].is_punct('.').then_some("."))
                    })
                    .collect::<Option<String>>()
            });
        let mut methods =
            python_route_methods(tokens, open, close, "route", SourceFramework::Flask);
        if methods.is_empty() {
            methods.insert("GET".to_owned());
        }
        for method in methods {
            let mut observation = http_from_literal(
                SourceLanguage::Python,
                framework,
                SourceRole::Provider,
                Some(method),
                path.as_deref(),
                handler.clone(),
                SourceLineRange {
                    start: tokens[index].line,
                    end: tokens[close].end_line,
                },
                true,
            );
            observation.router = Some(SymbolRef::Local(receiver.to_owned()));
            observations.push(observation);
        }
    }
}

fn python_router_reference(
    dotted: &[&str],
    contexts: &PythonContexts,
    imports: &BTreeMap<String, (String, String)>,
) -> Option<SymbolRef> {
    let (head, rest) = dotted.split_first()?;
    if let Some((module, name)) = imports.get(*head) {
        let name = std::iter::once(name.as_str())
            .filter(|name| !name.is_empty())
            .chain(rest.iter().copied())
            .collect::<Vec<_>>()
            .join(".");
        return (!name.is_empty()).then(|| SymbolRef::Import {
            module: module.clone(),
            name,
        });
    }
    (rest.is_empty()
        && (contexts.fastapi_apps.contains(*head) || contexts.flask_apps.contains(*head)))
    .then(|| SymbolRef::Local((*head).to_owned()))
}

/// Maps local names to `(module, imported name)`; `import a.b as c` binds `c` to `(a.b, "")`.
fn python_router_imports(tokens: &[Token]) -> BTreeMap<String, (String, String)> {
    let mut imports = BTreeMap::new();
    for index in 0..tokens.len() {
        if tokens[index].is_ident("import")
            && (index == 0 || tokens[index - 1].line != tokens[index].line)
        {
            let mut cursor = index + 1;
            let mut module = String::new();
            while let Some(token) = tokens
                .get(cursor)
                .filter(|token| token.line == tokens[index].line)
            {
                match (token.ident(), token.is_punct('.')) {
                    (Some(name), _) if name != "as" => module.push_str(name),
                    (None, true) => module.push('.'),
                    _ => break,
                }
                cursor += 1;
            }
            if tokens.get(cursor).is_some_and(|token| token.is_ident("as"))
                && let Some(alias) = tokens.get(cursor + 1).and_then(Token::ident)
            {
                imports.insert(alias.to_owned(), (module, String::new()));
            }
            continue;
        }
        if !tokens[index].is_ident("from") {
            continue;
        }
        let Some(import_index) = (index + 1..tokens.len())
            .take_while(|candidate| tokens[*candidate].line == tokens[index].line)
            .find(|candidate| tokens[*candidate].is_ident("import"))
        else {
            continue;
        };
        let module = tokens[index + 1..import_index]
            .iter()
            .map(|token| token.ident().unwrap_or("."))
            .collect::<String>();
        let parenthesized = tokens
            .get(import_index + 1)
            .is_some_and(|token| token.is_punct('('));
        let end = if parenthesized {
            matching(tokens, import_index + 1, '(', ')').unwrap_or(import_index + 1)
        } else {
            (import_index + 1..tokens.len())
                .find(|candidate| tokens[*candidate].line != tokens[index].line)
                .unwrap_or(tokens.len())
        };
        let mut cursor = import_index + 1;
        while cursor < end {
            let Some(imported) = tokens[cursor].ident() else {
                cursor += 1;
                continue;
            };
            let aliased = tokens
                .get(cursor + 1)
                .is_some_and(|token| token.is_ident("as"));
            let local = if aliased {
                tokens
                    .get(cursor + 2)
                    .and_then(Token::ident)
                    .unwrap_or(imported)
            } else {
                imported
            };
            imports.insert(local.to_owned(), (module.clone(), imported.to_owned()));
            cursor += if aliased { 3 } else { 1 };
        }
    }
    imports
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
            authority: None,
            router: None,
            mount_parent: None,
            url: None,
            call: None,
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
    scopes: &PythonScopes<'_>,
    observations: &mut SourceObservationCollector<'_>,
) {
    let receivers = contexts.http_receivers();
    for index in 0..tokens.len() {
        let Some(receiver) = tokens[index].ident() else {
            continue;
        };
        let member = || {
            tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('.'))
                .then(|| tokens.get(index + 2).and_then(Token::ident))
                .flatten()
        };
        let mut parameter = None;
        let (framework, call, open) = if let Some(framework) = receivers.get(receiver).copied() {
            let Some(call) = member() else {
                continue;
            };
            (framework, call, index + 3)
        } else if let Some((framework, call)) = contexts.direct_calls.get(receiver) {
            (*framework, call.as_str(), index + 1)
        } else if let Some(call) = member().filter(|call| canonical_method(call).is_some())
            && let Some(received) = python_received_client(tokens, index, scopes)
        {
            parameter = Some(received);
            (SourceFramework::TestClient, call, index + 3)
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
        let template = path_argument.map(|argument| {
            let end = python_argument_end(tokens, argument, close);
            let start = keyword_argument(tokens, argument, end)
                .filter(|(keyword, _)| keyword == "url")
                .map_or(argument, |(_, value)| value);
            scopes.template(start, end, index)
        });
        let literal = template.as_ref().and_then(UrlTemplate::client_literal);
        let mut observation = http_from_literal(
            SourceLanguage::Python,
            framework,
            SourceRole::Consumer,
            method,
            literal.as_deref(),
            enclosing_symbol(functions, index),
            SourceLineRange {
                start: tokens[index].line,
                end: tokens[close].end_line,
            },
            false,
        );
        observation.url = template.filter(UrlTemplate::has_parameters);
        if let Some(received) = parameter {
            if !literal.as_deref().is_some_and(|path| path.starts_with('/')) {
                continue;
            }
            observation.router = Some(received);
            observation.status = SourceEpistemicStatus::Ambiguous;
            observation.confidence = 0.0;
        }
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

/// Parameter of the enclosing function through which the call at `index` receives its client.
fn python_received_client(
    tokens: &[Token],
    index: usize,
    scopes: &PythonScopes<'_>,
) -> Option<SymbolRef> {
    if index > 0 && tokens[index - 1].is_punct('.') {
        return None;
    }
    let name = tokens[index].ident()?;
    let function = scopes.innermost(index)?;
    let position = scopes
        .parameters_of(function)
        .iter()
        .position(|parameter| parameter == name)?;
    Some(SymbolRef::Parameter {
        name: name.to_owned(),
        index: position,
    })
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
        .then_with(|| {
            (
                &left.authority,
                &left.router,
                &left.mount_parent,
                &left.url,
                &left.call,
            )
                .cmp(&(
                    &right.authority,
                    &right.router,
                    &right.mount_parent,
                    &right.url,
                    &right.call,
                ))
        })
}

#[cfg(test)]
mod tests {
    use super::{
        SourceEpistemicStatus, SourceFramework, SourceObservation, SourceRole, SourceWarning, SymbolRef, UrlPart, parse_python_source
    };

    /// HTTP, route, and test facts without call observations.
    fn parse_rust_source(source: &str) -> Vec<SourceObservation> {
        super::parse_rust_source(source)
            .into_iter()
            .filter(|item| item.role != SourceRole::Call)
            .collect()
    }

    #[test]
    fn rust_reqwest_urls_should_resolve_constants_format_strings_and_wrapper_parameters() {
        let source = r#"
use reqwest::Client;
const BASE: &str = "http://orders:8080";
async fn get_order(client: &Client, id: u64) {
    client.get(format!("{BASE}/orders/{id}")).send().await;
}
async fn post_json(client: &Client, path: &str) {
    client.post(format!("{}{}", BASE, path)).send().await;
}
async fn refund(client: &Client, id: &str) {
    post_json(client, &format!("/orders/{id}/refunds")).await;
}
"#;
        let result = super::parse_rust_source(source);
        let consumers = result
            .iter()
            .filter(|item| item.role == SourceRole::Consumer)
            .map(|item| {
                (
                    item.symbol_name.as_deref(),
                    item.path.as_deref(),
                    item.url.is_some(),
                )
            })
            .collect::<Vec<_>>();
        let calls = result
            .iter()
            .filter_map(|item| item.call.as_ref())
            .map(|call| (call.callee.clone(), call.arguments.len()))
            .collect::<Vec<_>>();

        assert_eq!(
            consumers,
            [
                (Some("get_order"), Some("/orders/{id}"), true),
                (Some("post_json"), None, true),
            ]
        );
        assert_eq!(calls, [(SymbolRef::Call("post_json".to_owned()), 2)]);
    }

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
                .filter(|item| item.role == SourceRole::Provider)
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
    fn reqwest_should_recognize_injected_client_parameters() {
        let source = r#"
use reqwest::Client;
async fn synchronize(client: &Client, qualified: &reqwest::Client) {
    client.post("https://orders.test/v1/orders").send().await;
    qualified.get("https://payments.test/v1/payments").send().await;
}
"#;
        let result = parse_rust_source(source);

        assert_eq!(
            result
                .iter()
                .map(|item| (item.method.as_deref(), item.path.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (Some("POST"), Some("/v1/orders")),
                (Some("GET"), Some("/v1/payments")),
            ]
        );
    }

    #[test]
    fn reqwest_should_preserve_explicit_lifetimes_in_client_parameters() {
        let source = r#"
use reqwest::Client;
async fn synchronize<'request>(client: &'request Client) {
    client.post("https://orders.test/v1/orders").send().await;
}
"#;
        let result = parse_rust_source(source);

        assert!(result.iter().any(|item| {
            item.framework == SourceFramework::Reqwest
                && item.method.as_deref() == Some("POST")
                && item.path.as_deref() == Some("/v1/orders")
        }));
    }

    #[test]
    fn absolute_consumer_url_should_record_its_authority() {
        let source = r#"
use reqwest::Client;
async fn synchronize(client: &Client) {
    client.get("https://User@Third-Party.example:8443/health?probe=1").send().await;
    client.get("/internal-health").send().await;
}
"#;
        let result = parse_rust_source(source);
        let external = result
            .iter()
            .find(|item| item.path.as_deref() == Some("/health"))
            .expect("absolute call");
        let internal = result
            .iter()
            .find(|item| item.path.as_deref() == Some("/internal-health"))
            .expect("relative call");

        assert_eq!(external.status, SourceEpistemicStatus::Confirmed);
        assert_eq!(
            external.authority.as_deref(),
            Some("third-party.example:8443")
        );
        assert_eq!(external.warnings, []);
        assert_eq!(internal.status, SourceEpistemicStatus::Confirmed);
        assert_eq!(internal.authority, None);
    }

    #[test]
    fn reqwest_should_recognize_builder_chains_and_typed_self_fields() {
        let source = r#"
use reqwest::Client;
use std::time::Duration;

struct Api {
    client: Client,
}

impl Api {
    fn new() -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .expect("valid client configuration");
        Self { client }
    }

    async fn synchronize(&self) {
        self.client.get("https://inventory.test/v1/inventory").send().await;
    }
}
"#;
        let result = parse_rust_source(source);

        assert!(matches!(
            result.as_slice(),
            [observation]
                if observation.method.as_deref() == Some("GET")
                    && observation.path.as_deref() == Some("/v1/inventory")
        ));
    }

    #[test]
    fn unrelated_grouped_import_should_not_promote_client_to_reqwest() {
        let source = r#"
use my_http::{reqwest, Client};
async fn diagnostic() {
    let client = Client::new();
    client.get("https://inventory.test/v1/diagnostic");
}
"#;
        let result = parse_rust_source(source);

        assert_eq!(result.len(), 0);
    }

    #[test]
    fn reqwest_should_not_reuse_receiver_names_across_functions() {
        let source = r#"
use reqwest::Client;
struct FakeClient;
impl FakeClient { fn get(&self, _path: &str) {} }
fn local_only(client: &FakeClient) {
    client.get("https://inventory.test/v1/diagnostic");
}
async fn remote() {
    let client = Client::new();
    client.get("https://inventory.test/v1/inventory").send().await;
}
"#;
        let result = parse_rust_source(source);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path.as_deref(), Some("/v1/inventory"));
    }

    #[test]
    fn reqwest_import_should_not_leak_across_inline_modules() {
        let source = r#"
mod real { use reqwest::Client; }
mod fake {
    struct Client;
    impl Client { fn get(&self, _path: &str) {} }
    fn probe(client: &Client) { client.get("/health"); }
}
"#;

        assert_eq!(parse_rust_source(source), []);
    }

    #[test]
    fn top_level_reqwest_import_should_survive_an_inline_test_module() {
        let source = r#"
use reqwest::Client;
async fn probe() {
    let client = Client::new();
    client.get("/health").send().await;
}
#[cfg(test)] mod tests { fn smoke() {} }
"#;

        assert!(parse_rust_source(source).iter().any(|item| {
            item.framework == SourceFramework::Reqwest && item.path.as_deref() == Some("/health")
        }));
    }

    #[test]
    fn reqwest_import_inside_inline_module_should_remain_visible_locally() {
        let source = r#"
mod api {
    use reqwest::Client as HttpClient;
    async fn probe() {
        let client = HttpClient::new();
        client.get("/health").send().await;
    }
}
"#;

        assert!(parse_rust_source(source).iter().any(|item| {
            item.framework == SourceFramework::Reqwest && item.path.as_deref() == Some("/health")
        }));
    }

    #[test]
    fn reqwest_self_fields_should_not_leak_across_sibling_modules() {
        let source = r#"
mod real {
    use reqwest::Client;
    struct Api { client: Client }
    impl Api {
        async fn probe(&self) { self.client.get("/real").send().await; }
    }
}
mod fake {
    struct Client;
    impl Client { fn get(&self, _path: &str) {} }
    struct Api { client: Client }
    impl Api {
        fn probe(&self) { self.client.get("/fake"); }
    }
}
"#;

        let result = parse_rust_source(source);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].path.as_deref(), Some("/real"));
    }

    #[test]
    fn reqwest_should_respect_lexical_shadowing() {
        let source = r#"
use reqwest::Client;
struct FakeClient;
impl FakeClient { fn get(&self, _path: &str) {} }
async fn synchronize() {
    let client = Client::new();
    client.get("https://inventory.test/v1/inventory").send().await;
    {
        let client = FakeClient;
        client.get("https://inventory.test/v1/diagnostic");
    }
    client.get("https://payments.test/v1/payments").send().await;
}
"#;
        let result = parse_rust_source(source);

        assert_eq!(
            result
                .iter()
                .map(|item| item.path.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("/v1/inventory"), Some("/v1/payments")]
        );
    }

    #[test]
    fn reqwest_should_not_promote_wrappers_that_consume_a_client() {
        let source = r#"
use reqwest::Client;
struct FakeClient;
impl FakeClient {
    fn new(_client: Client) -> Self { Self }
    fn get(&self, _path: &str) {}
}
async fn diagnostic() {
    let fake = FakeClient::new(Client::new());
    fake.get("https://inventory.test/v1/diagnostic");
}
"#;
        let result = parse_rust_source(source);

        assert_eq!(result.len(), 0);
    }

    #[test]
    fn reqwest_should_recognize_import_aliases_and_exact_type_annotations() {
        let source = r#"
use reqwest::Client as HttpClient;
async fn synchronize(injected: &HttpClient) {
    let constructed: HttpClient = factory();
    injected.get("https://inventory.test/v1/inventory").send().await;
    constructed.post("https://orders.test/v1/orders").send().await;
}
"#;
        let result = parse_rust_source(source);

        assert_eq!(
            result
                .iter()
                .map(|item| (item.method.as_deref(), item.path.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                (Some("GET"), Some("/v1/inventory")),
                (Some("POST"), Some("/v1/orders")),
            ]
        );
    }

    #[test]
    fn reqwest_should_recognize_blocking_clients_and_generic_self_fields() {
        let source = r#"
use reqwest::blocking::Client as BlockingClient;
use reqwest::blocking::{Client as GroupedClient, Response};
struct Api<T> { client: BlockingClient, marker: T }
impl<T> Api<T> {
    fn synchronize(&self) {
        self.client.get("https://inventory.test/v1/inventory").send();
    }
}
fn qualified() {
    let client = reqwest::blocking::Client::new();
    client.post("https://orders.test/v1/orders").send();
}
fn grouped() {
    let client = GroupedClient::new();
    client.delete("https://orders.test/v1/obsolete").send();
}
"#;
        let result = parse_rust_source(source);

        assert_eq!(result.len(), 3);
        assert!(
            result
                .iter()
                .any(|item| item.path.as_deref() == Some("/v1/inventory"))
        );
        assert!(
            result
                .iter()
                .any(|item| item.path.as_deref() == Some("/v1/orders"))
        );
        assert!(
            result
                .iter()
                .any(|item| item.path.as_deref() == Some("/v1/obsolete"))
        );
    }

    #[test]
    fn reqwest_should_recognize_exact_local_type_aliases() {
        let source = r#"
use reqwest::Client as HttpClient;
type ServiceClient = HttpClient;
async fn synchronize(client: &ServiceClient) {
    client.get("https://inventory.test/v1/inventory").send().await;
}
"#;
        let result = parse_rust_source(source);

        assert!(matches!(
            result.as_slice(),
            [observation]
                if observation.method.as_deref() == Some("GET")
                    && observation.path.as_deref() == Some("/v1/inventory")
        ));
    }

    #[test]
    fn reqwest_should_track_braced_typed_closure_parameters() {
        let source = r#"
use reqwest::Client;
async fn synchronize() {
    let operation = |client: &Client| {
        client.get("https://inventory.test/v1/inventory");
    };
}
"#;
        let result = parse_rust_source(source);

        assert!(matches!(
            result.as_slice(),
            [observation] if observation.path.as_deref() == Some("/v1/inventory")
        ));
    }

    #[test]
    fn reqwest_should_treat_destructuring_as_lexical_shadowing() {
        let source = r#"
use reqwest::Client;
struct FakeClient;
impl FakeClient { fn get(&self, _path: &str) {} }
async fn synchronize(client: &Client, pair: (FakeClient, usize)) {
    let (client, _value) = pair;
    client.get("https://inventory.test/v1/diagnostic");
}
"#;
        let result = parse_rust_source(source);

        assert_eq!(result.len(), 0);
    }

    #[test]
    fn reqwest_should_conservatively_handle_binding_reassignment() {
        let source = r#"
use reqwest::Client;
async fn synchronize() {
    let mut client = Client::new();
    client = factory();
    client.get("https://inventory.test/v1/unknown");
    client = Client::new();
    client.get("https://inventory.test/v1/inventory");
}
"#;
        let result = parse_rust_source(source);

        assert_eq!(
            result
                .iter()
                .map(|item| item.path.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("/v1/inventory")]
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

        assert_eq!(result, Vec::new());
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

    fn consumer_paths(source: &str) -> Vec<(Option<String>, Option<String>, bool)> {
        parse_python_source(source)
            .into_iter()
            .filter(|item| item.role == SourceRole::Consumer)
            .map(|item| (item.symbol_name, item.path, item.url.is_some()))
            .collect()
    }

    #[test]
    fn python_url_expressions_should_resolve_whole_segment_values_and_scoped_names() {
        let source = r#"
import requests
BASE_URL = "http://orders:8080"
API = BASE_URL + "/api"
def fetch(user_id):
    requests.get(f"https://example.test/users/{user_id}")
def report(name):
    requests.get(f"/reports/{name}.csv")
def scoped(order):
    url = f"{API}/orders/{order.id}"
    requests.get(url)
def formatted(sku):
    requests.get("{}/items/{}".format(API, sku))
    requests.delete("/items/%s" % sku)
"#;

        assert_eq!(
            consumer_paths(source),
            [
                (
                    Some("fetch".to_owned()),
                    Some("/users/{user_id}".to_owned()),
                    true
                ),
                (Some("report".to_owned()), None, true),
                (
                    Some("scoped".to_owned()),
                    Some("/api/orders/{id}".to_owned()),
                    false
                ),
                (
                    Some("formatted".to_owned()),
                    Some("/api/items/{sku}".to_owned()),
                    true
                ),
                (
                    Some("formatted".to_owned()),
                    Some("/items/{sku}".to_owned()),
                    true
                ),
            ]
        );
    }

    #[test]
    fn python_test_clients_should_be_recognized_and_parameter_clients_left_for_composition() {
        let source = r#"
import httpx
import pytest
from fastapi.testclient import TestClient
from app import app, flask_app

client = TestClient(app)

def test_direct():
    client.get("/items")

class OrdersTest(unittest.TestCase):
    def setUp(self):
        self.web = flask_app.test_client()

    def test_flask(self):
        self.web.post("/orders")

async def test_async():
    async with httpx.AsyncClient(app=app, base_url="http://test") as api:
        await api.delete("/orders/1")

@pytest.fixture
def api_client():
    return TestClient(app)

def test_injected(api_client):
    api_client.put("/orders/2")
"#;
        let result = parse_python_source(source);
        let consumers = result
            .iter()
            .filter(|item| item.role == SourceRole::Consumer)
            .map(|item| {
                (
                    item.framework,
                    item.method.clone().unwrap_or_default(),
                    item.path.clone().unwrap_or_default(),
                    item.status,
                    item.router.clone(),
                )
            })
            .collect::<Vec<_>>();
        let clients = result
            .iter()
            .filter(|item| item.role == SourceRole::Client)
            .map(|item| (item.framework, item.symbol_name.clone().unwrap_or_default()))
            .collect::<Vec<_>>();

        assert_eq!(
            consumers,
            [
                (
                    SourceFramework::TestClient,
                    "GET".to_owned(),
                    "/items".to_owned(),
                    SourceEpistemicStatus::Confirmed,
                    None
                ),
                (
                    SourceFramework::FlaskTestClient,
                    "POST".to_owned(),
                    "/orders".to_owned(),
                    SourceEpistemicStatus::Confirmed,
                    None
                ),
                (
                    SourceFramework::TestClient,
                    "DELETE".to_owned(),
                    "/orders/1".to_owned(),
                    SourceEpistemicStatus::Confirmed,
                    None
                ),
                (
                    SourceFramework::TestClient,
                    "PUT".to_owned(),
                    "/orders/2".to_owned(),
                    SourceEpistemicStatus::Ambiguous,
                    Some(SymbolRef::Parameter {
                        name: "api_client".to_owned(),
                        index: 0
                    })
                ),
            ]
        );
        assert_eq!(
            clients,
            [(SourceFramework::TestClient, "api_client".to_owned())]
        );
    }

    #[test]
    fn python_imperative_routes_should_name_their_handlers() {
        let source = r#"
from fastapi import APIRouter
from flask import Flask
from orders import views

router = APIRouter()
router.add_api_route("/orders", views.list_orders, methods=["GET", "POST"])
router.add_api_route(path="/health", endpoint=health)
app = Flask(__name__)
app.add_url_rule("/legacy/<int:id>", "legacy", views.legacy)
"#;
        let providers = parse_python_source(source)
            .into_iter()
            .filter(|item| item.role == SourceRole::Provider)
            .map(|item| {
                format!(
                    "{} {} -> {}",
                    item.method.unwrap_or_default(),
                    item.path.unwrap_or_default(),
                    item.symbol_name.unwrap_or_default()
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            providers,
            [
                "GET /orders -> views.list_orders",
                "POST /orders -> views.list_orders",
                "GET /health -> health",
                "GET /legacy/<int:id> -> views.legacy",
            ]
        );
    }

    #[test]
    fn rust_test_requests_should_be_recognized_for_axum_oneshot_and_actix() {
        let source = r#"
use axum::http::{Method, Request};
use tower::ServiceExt;

#[tokio::test]
async fn creates_order() {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/orders")
        .body(Body::empty())
        .unwrap();
    app().oneshot(request).await.unwrap();
}

#[actix_web::test]
async fn reads_order() {
    let id = 42;
    let request = test::TestRequest::get().uri(&format!("/orders/{}", id)).to_request();
}
"#;
        let facts = parse_rust_source(source)
            .into_iter()
            .map(|item| {
                format!(
                    "{:?} {:?} {} {} {}",
                    item.role,
                    item.framework,
                    item.symbol_name.unwrap_or_default(),
                    item.method.unwrap_or_default(),
                    item.path.unwrap_or_default()
                )
            })
            .filter(|fact| !fact.starts_with("Call"))
            .collect::<Vec<_>>();

        assert_eq!(
            facts,
            [
                "Test TokioTest creates_order  ",
                "Consumer AxumOneshot creates_order POST /orders",
                "Test RustTest reads_order  ",
                "Consumer ActixTest reads_order GET /orders/{value}",
            ]
        );
    }

    #[test]
    fn python_wrappers_calls_and_fixture_requests_should_be_recorded() {
        let source = r#"
import pytest
import requests
from tests.helpers import create_order

def get_json(path):
    return requests.get(BASE + path)

@pytest.fixture
def order(client):
    return create_order(client, "/orders")

def test_reads_order(order, api):
    api.read_order(order)
    get_json("/orders/7")
"#;
        let result = parse_python_source(source);
        let calls = result
            .iter()
            .filter_map(|item| {
                let call = item.call.as_ref()?;
                Some((
                    item.symbol_name.clone()?,
                    call.callee.clone(),
                    call.arguments
                        .iter()
                        .filter_map(|argument| argument.value.as_ref()?.client_literal())
                        .collect::<Vec<_>>(),
                ))
            })
            .collect::<Vec<_>>();
        let wrapper = result
            .iter()
            .find(|item| item.role == SourceRole::Consumer)
            .and_then(|item| item.url.clone());

        assert_eq!(
            wrapper.map(|url| url.parts),
            Some(vec![
                UrlPart::Value(Some("BASE".to_owned())),
                UrlPart::Parameter {
                    name: "path".to_owned(),
                    index: 0
                },
            ])
        );
        assert_eq!(
            calls,
            [
                (
                    "order".to_owned(),
                    SymbolRef::Fixture("client".to_owned()),
                    vec![]
                ),
                (
                    "order".to_owned(),
                    SymbolRef::Import {
                        module: "tests.helpers".to_owned(),
                        name: "create_order".to_owned()
                    },
                    vec!["/orders".to_owned()]
                ),
                (
                    "test_reads_order".to_owned(),
                    SymbolRef::Fixture("api".to_owned()),
                    vec![]
                ),
                (
                    "test_reads_order".to_owned(),
                    SymbolRef::Fixture("order".to_owned()),
                    vec![]
                ),
                (
                    "test_reads_order".to_owned(),
                    SymbolRef::Call("api.read_order".to_owned()),
                    vec![]
                ),
                (
                    "test_reads_order".to_owned(),
                    SymbolRef::Local("get_json".to_owned()),
                    vec!["/orders/7".to_owned()]
                ),
            ]
        );
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

        assert_eq!(result, Vec::new());
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

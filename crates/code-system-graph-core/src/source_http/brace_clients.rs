//! HTTP client calls and call sites in JavaScript and TypeScript, Go, and Java.
//!
//! Client URLs are evaluated as [`UrlTemplate`]s, so constants, format strings, and wrapper
//! parameters reach repository-level composition instead of being dropped as dynamic.

use std::collections::BTreeMap;

use self::test_clients::{httptest_servers, targets_httptest};
use super::brace_flows::{BraceScopes, expression_end};
use super::brace_lexer::{BraceDialect, lex_brace};
use super::brace_tests::{
    ScriptBlock, record_go_tests, record_java_tests, record_script_tests, script_blocks
};
use super::{SourceObservationCollector, Token, http_from_literal, matching, split_operands};
use crate::{
    SourceFramework, SourceLanguage, SourceLineRange, SourceRole, SourceWarning, UrlPart, UrlTemplate
};

const METHODS: [&str; 8] = [
    "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE",
];

/// Names whose calls are runtime or assertion utilities rather than repository functions.
const SCRIPT_UTILITIES: [&str; 13] = [
    "console", "JSON", "Object", "Array", "Math", "Promise", "Number", "String", "Date", "expect",
    "fetch", "axios", "require",
];
const GO_UTILITIES: [&str; 8] = [
    "http", "fmt", "strings", "strconv", "errors", "json", "log", "t",
];
const JAVA_UTILITIES: [&str; 4] = ["System", "String", "Objects", "Arrays"];

mod test_clients;
#[cfg(test)]
mod tests;

/// Client libraries that a file imports.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct BraceClients {
    pub(crate) axios: bool,
    pub(crate) go_http: bool,
    pub(crate) web_client: bool,
}

/// Records HTTP client calls and call sites of one JavaScript, TypeScript, Go, or Java file.
pub(crate) fn collect_brace_clients(
    source: &str,
    language: SourceLanguage,
    clients: BraceClients,
    observations: &mut SourceObservationCollector<'_>,
) {
    let dialect = match language {
        SourceLanguage::JavaScript | SourceLanguage::TypeScript => BraceDialect::Script,
        SourceLanguage::Go => BraceDialect::Go,
        SourceLanguage::Java => BraceDialect::Java,
        SourceLanguage::Python | SourceLanguage::Rust => return,
    };
    let tokens = lex_brace(source, dialect);
    let blocks = if dialect == BraceDialect::Script {
        script_blocks(&tokens)
    } else {
        Vec::new()
    };
    let scopes = BraceScopes::new(
        &tokens,
        dialect,
        blocks.iter().filter_map(ScriptBlock::function).collect(),
    );
    let consumers = Consumers {
        scopes: &scopes,
        language,
    };
    let all_calls = declares_tests(&tokens, dialect);
    match dialect {
        BraceDialect::Script => {
            consumers.fetch(observations);
            let instances = if clients.axios {
                axios_instances(&scopes)
            } else {
                BTreeMap::new()
            };
            if clients.axios {
                consumers.axios(&instances, observations);
            }
            consumers.supertest(observations);
            consumers.playwright(observations);
            let is_client =
                |head: &str| SCRIPT_UTILITIES.contains(&head) || instances.contains_key(head);
            scopes.record_calls(language, &is_client, all_calls, observations);
            record_script_tests(&tokens, &blocks, language, observations);
        }
        BraceDialect::Go => {
            if clients.go_http {
                consumers.go_http(&go_client_names(&tokens), observations);
            }
            consumers.httptest(observations);
            let is_client = |head: &str| GO_UTILITIES.contains(&head);
            scopes.record_calls(language, &is_client, all_calls, observations);
            record_go_tests(&scopes, observations);
        }
        BraceDialect::Java => {
            let mentions = |name: &str| tokens.iter().any(|token| token.is_ident(name));
            if mentions("WebTestClient") {
                consumers.web_client(SourceFramework::WebTestClient, observations);
            } else if clients.web_client {
                consumers.web_client(SourceFramework::WebClient, observations);
            }
            if mentions("MockMvc") || mentions("MockMvcRequestBuilders") {
                consumers.mock_mvc(observations);
            }
            if mentions("restassured") || mentions("RestAssured") {
                consumers.rest_assured(observations);
            }
            consumers.rest_template(observations);
            let is_client = |head: &str| JAVA_UTILITIES.contains(&head);
            scopes.record_calls(language, &is_client, all_calls, observations);
            record_java_tests(&scopes, observations);
        }
    }
    observations.locate_symbols(&tokens, scopes.functions());
}

/// Start line and name of each Java method, in declaration order.
pub(crate) fn java_method_lines(source: &str) -> Vec<(u32, String)> {
    let tokens = lex_brace(source, BraceDialect::Java);
    let scopes = BraceScopes::new(&tokens, BraceDialect::Java, Vec::new());
    let mut methods = scopes
        .functions()
        .iter()
        .map(|function| (tokens[function.start_token].line, function.name.clone()))
        .collect::<Vec<_>>();
    methods.sort();
    methods
}

/// Whether the file declares tests, so that every call from its functions is recorded.
fn declares_tests(tokens: &[Token], dialect: BraceDialect) -> bool {
    tokens
        .iter()
        .enumerate()
        .any(|(index, token)| match dialect {
            BraceDialect::Script => {
                ["describe", "it", "test"]
                    .iter()
                    .any(|name| token.is_ident(name))
                    && tokens.get(index + 1).is_some_and(|next| next.is_punct('('))
                    && index
                        .checked_sub(1)
                        .is_none_or(|previous| !tokens[previous].is_punct('.'))
            }
            BraceDialect::Go => token.is_ident("testing"),
            BraceDialect::Java => {
                token.is_ident("Test") && index > 0 && tokens[index - 1].is_punct('@')
            }
        })
}

struct Consumers<'s, 'a> {
    scopes: &'s BraceScopes<'a>,
    language: SourceLanguage,
}

impl Consumers<'_, '_> {
    fn tokens(&self) -> &[Token] {
        self.scopes.tokens
    }

    fn argument_template(
        &self,
        argument: Option<&(usize, usize)>,
        at: usize,
    ) -> Option<UrlTemplate> {
        argument
            .filter(|(start, end)| start < end)
            .map(|(start, end)| self.scopes.template(*start, *end, at))
    }

    /// Records a consumer call spanning tokens `start..=close`; a missing method is dynamic.
    fn push(
        &self,
        framework: SourceFramework,
        method: Option<String>,
        template: Option<UrlTemplate>,
        (start, close): (usize, usize),
        observations: &mut SourceObservationCollector<'_>,
    ) {
        let tokens = self.tokens();
        let literal = template.as_ref().and_then(UrlTemplate::client_literal);
        let mut observation = http_from_literal(
            self.language,
            framework,
            SourceRole::Consumer,
            method,
            literal.as_deref(),
            self.scopes.function_name(start),
            SourceLineRange {
                start: tokens[start].line,
                end: tokens[close].end_line,
            },
            false,
        );
        observation.url = template.filter(UrlTemplate::has_parameters);
        if observation.method.is_none() {
            observation.warnings.push(SourceWarning::DynamicMethod);
            observation.warnings.sort();
        }
        observations.push(observation);
    }

    fn arguments(&self, open: usize) -> Option<(usize, Vec<(usize, usize)>)> {
        let tokens = self.tokens();
        let close = matching(tokens, open, '(', ')')?;
        let arguments = split_operands(tokens, open + 1, close, ',');
        Some((close, arguments))
    }

    fn fetch(&self, observations: &mut SourceObservationCollector<'_>) {
        let tokens = self.tokens();
        for index in 0..tokens.len() {
            if !tokens[index].is_ident("fetch")
                || !tokens
                    .get(index + 1)
                    .is_some_and(|token| token.is_punct('('))
            {
                continue;
            }
            let global = index < 2
                || !tokens[index - 1].is_punct('.')
                || ["window", "globalThis", "self"]
                    .iter()
                    .any(|name| tokens[index - 2].is_ident(name));
            if !global {
                continue;
            }
            let Some((close, arguments)) = self.arguments(index + 1) else {
                continue;
            };
            let method = options_method(tokens, arguments.get(1).copied());
            let template = self.argument_template(arguments.first(), index);
            self.push(
                SourceFramework::Fetch,
                method,
                template,
                (index, close),
                observations,
            );
        }
    }

    fn axios(
        &self,
        instances: &BTreeMap<String, UrlTemplate>,
        observations: &mut SourceObservationCollector<'_>,
    ) {
        let tokens = self.tokens();
        for index in 0..tokens.len() {
            let Some(name) = tokens[index].ident() else {
                continue;
            };
            let Some((receiver, call, open)) = receiver_call(tokens, index) else {
                if name == "axios"
                    && tokens
                        .get(index + 1)
                        .is_some_and(|token| token.is_punct('('))
                    && index
                        .checked_sub(1)
                        .is_none_or(|previous| !tokens[previous].is_punct('.'))
                {
                    self.axios_request(None, index, index + 1, observations);
                }
                continue;
            };
            let base = match instances.get(&receiver) {
                Some(base) => Some(base),
                None if receiver == "axios" => None,
                None => continue,
            };
            if call == "request" {
                self.axios_request(base, index, open, observations);
                continue;
            }
            let Some(method) = canonical(&call) else {
                continue;
            };
            let Some((close, arguments)) = self.arguments(open) else {
                continue;
            };
            let template = self
                .argument_template(arguments.first(), index)
                .map(|path| join_base(base, path));
            self.push(
                SourceFramework::Axios,
                Some(method),
                template,
                (index, close),
                observations,
            );
        }
    }

    /// `axios(config)`, `axios(url, config)`, or `instance.request(config)`.
    fn axios_request(
        &self,
        base: Option<&UrlTemplate>,
        start: usize,
        open: usize,
        observations: &mut SourceObservationCollector<'_>,
    ) {
        let tokens = self.tokens();
        let Some((close, arguments)) = self.arguments(open) else {
            return;
        };
        let (url, config) = match arguments.as_slice() {
            [(first, end), rest @ ..] if !tokens[*first].is_punct('{') => (
                Some(self.scopes.template(*first, *end, start)),
                rest.first().copied(),
            ),
            [config, ..] => (None, Some(*config)),
            [] => return,
        };
        let url = url.or_else(|| {
            let (config_start, config_end) = config?;
            let value = object_value(tokens, config_start, config_end, "url")?;
            Some(
                self.scopes
                    .template(value, expression_end(tokens, value).min(config_end), start),
            )
        });
        let method = options_method(tokens, config);
        let template = url.map(|path| join_base(base, path));
        self.push(
            SourceFramework::Axios,
            method,
            template,
            (start, close),
            observations,
        );
    }

    fn go_http(&self, clients: &[String], observations: &mut SourceObservationCollector<'_>) {
        let tokens = self.tokens();
        let servers = httptest_servers(tokens);
        let uses_httptest = tokens.iter().any(|token| token.is_ident("httptest"));
        for index in 0..tokens.len() {
            let Some((receiver, call, open)) = receiver_call(tokens, index) else {
                continue;
            };
            let last = receiver.rsplit('.').next().unwrap_or_default();
            let package = receiver == "http";
            let client =
                receiver == "http.DefaultClient" || clients.iter().any(|name| name == last);
            if !package && !client {
                continue;
            }
            let Some((close, arguments)) = self.arguments(open) else {
                continue;
            };
            let (method, url) = match call.as_str() {
                "Get" => (Some("GET".to_owned()), arguments.first()),
                "Head" => (Some("HEAD".to_owned()), arguments.first()),
                "Post" | "PostForm" => (Some("POST".to_owned()), arguments.first()),
                "NewRequest" if package => (go_method(tokens, arguments.first()), arguments.get(1)),
                "NewRequestWithContext" if package => {
                    (go_method(tokens, arguments.get(1)), arguments.get(2))
                }
                _ => continue,
            };
            let template = self.argument_template(url, index);
            let framework = if template
                .as_ref()
                .is_some_and(|template| targets_httptest(template, &servers, uses_httptest))
            {
                SourceFramework::Httptest
            } else {
                SourceFramework::GoNetHttp
            };
            self.push(framework, method, template, (index, close), observations);
        }
    }

    fn web_client(
        &self,
        framework: SourceFramework,
        observations: &mut SourceObservationCollector<'_>,
    ) {
        let tokens = self.tokens();
        for index in 1..tokens.len() {
            if !tokens[index].is_ident("uri")
                || !tokens[index - 1].is_punct('.')
                || !tokens
                    .get(index + 1)
                    .is_some_and(|token| token.is_punct('('))
            {
                continue;
            }
            let Some((close, arguments)) = self.arguments(index + 1) else {
                continue;
            };
            let method = chain_method(tokens, index);
            let template = self.argument_template(arguments.first(), index);
            self.push(framework, method, template, (index, close), observations);
        }
    }
}

fn canonical(name: &str) -> Option<String> {
    METHODS
        .iter()
        .find(|method| method.eq_ignore_ascii_case(name))
        .map(|method| (*method).to_owned())
}

/// `receiver.call(` at identifier `index`, where `index` starts the receiver path.
fn receiver_call(tokens: &[Token], index: usize) -> Option<(String, String, usize)> {
    if index > 0 && tokens[index - 1].is_punct('.') {
        return None;
    }
    let mut segments = vec![tokens[index].ident()?];
    let mut cursor = index + 1;
    while tokens.get(cursor)?.is_punct('.') {
        segments.push(tokens.get(cursor + 1)?.ident()?);
        cursor += 2;
    }
    if !tokens.get(cursor)?.is_punct('(') || segments.len() < 2 {
        return None;
    }
    let call = segments.pop()?.to_owned();
    Some((segments.join("."), call, cursor))
}

/// Span of the `key` entry at the top level of the object literal in `start..end`.
fn object_entry(tokens: &[Token], start: usize, end: usize, key: &str) -> Option<(usize, usize)> {
    if !tokens.get(start)?.is_punct('{') {
        return None;
    }
    let close = matching(tokens, start, '{', '}')?.min(end);
    split_operands(tokens, start + 1, close, ',')
        .into_iter()
        .find(|(entry, entry_end)| {
            entry < entry_end
                && (tokens[*entry].ident() == Some(key) || tokens[*entry].literal() == Some(key))
        })
}

/// Value start of `key:` at the top level of the object literal in `start..end`.
fn object_value(tokens: &[Token], start: usize, end: usize, key: &str) -> Option<usize> {
    let (entry, entry_end) = object_entry(tokens, start, end, key)?;
    (tokens.get(entry + 1)?.is_punct(':') && entry + 2 < entry_end).then_some(entry + 2)
}

/// Method of a request whose options object spans `options`: the literal `method` entry, `GET`
/// without options or without that entry, and `None` when the method is computed.
fn options_method(tokens: &[Token], options: Option<(usize, usize)>) -> Option<String> {
    let Some((start, end)) = options.filter(|(start, end)| start < end) else {
        return Some("GET".to_owned());
    };
    if !tokens[start].is_punct('{') {
        return None;
    }
    let Some((entry, entry_end)) = object_entry(tokens, start, end, "method") else {
        return Some("GET".to_owned());
    };
    (tokens.get(entry + 1)?.is_punct(':') && entry + 3 == entry_end)
        .then(|| tokens[entry + 2].literal())
        .flatten()
        .and_then(canonical)
}

fn join_base(base: Option<&UrlTemplate>, path: UrlTemplate) -> UrlTemplate {
    let Some(base) = base else {
        return path;
    };
    let absolute = path
        .as_literal()
        .is_some_and(|path| path.starts_with("http://") || path.starts_with("https://"));
    if absolute || base.parts.is_empty() {
        return path;
    }
    let mut joined = base.clone();
    let base_slash = base
        .client_literal()
        .is_some_and(|base| base.ends_with('/'));
    let mut path = path;
    if base_slash
        && let Some(UrlPart::Text(text)) = path.parts.first_mut()
        && let Some(stripped) = text.strip_prefix('/')
    {
        *text = stripped.to_owned();
    }
    joined.extend(path);
    joined
}

/// Instances created by `axios.create({ baseURL })`, keyed by their binding path.
fn axios_instances(scopes: &BraceScopes<'_>) -> BTreeMap<String, UrlTemplate> {
    let tokens = scopes.tokens;
    let mut instances = BTreeMap::new();
    for index in 2..tokens.len() {
        if !(tokens[index].is_ident("create")
            && tokens[index - 1].is_punct('.')
            && tokens[index - 2].is_ident("axios")
            && tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('(')))
        {
            continue;
        }
        let Some(assign) = index
            .checked_sub(3)
            .filter(|assign| tokens[*assign].is_punct('='))
        else {
            continue;
        };
        let Some(name) = assign.checked_sub(1).and_then(|name| tokens[name].ident()) else {
            continue;
        };
        let this_member =
            assign >= 3 && tokens[assign - 2].is_punct('.') && tokens[assign - 3].is_ident("this");
        let binding = if this_member {
            format!("this.{name}")
        } else {
            name.to_owned()
        };
        let base = matching(tokens, index + 1, '(', ')')
            .and_then(|close| {
                let value = object_value(tokens, index + 2, close, "baseURL")?;
                Some(scopes.template(value, expression_end(tokens, value).min(close), index))
            })
            .unwrap_or_default();
        instances.insert(binding, base);
    }
    instances
}

/// Names of variables, fields, and parameters that hold a Go `http.Client`.
fn go_client_names(tokens: &[Token]) -> Vec<String> {
    let mut names = Vec::new();
    for index in 0..tokens.len() {
        if !(tokens[index].is_ident("http")
            && tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('.'))
            && tokens
                .get(index + 2)
                .is_some_and(|token| token.is_ident("Client")))
        {
            continue;
        }
        let mut cursor = index;
        while cursor > 0 && (tokens[cursor - 1].is_punct('*') || tokens[cursor - 1].is_punct('&')) {
            cursor -= 1;
        }
        if cursor >= 2 && tokens[cursor - 1].is_punct('=') {
            let assign = if tokens[cursor - 2].is_punct(':') {
                cursor - 2
            } else {
                cursor - 1
            };
            if let Some(name) = assign.checked_sub(1).and_then(|name| tokens[name].ident()) {
                names.push(name.to_owned());
            }
        } else if let Some(name) = cursor.checked_sub(1).and_then(|name| tokens[name].ident()) {
            names.push(name.to_owned());
        }
    }
    names.sort();
    names.dedup();
    names
}

/// Method argument of `http.NewRequest`: a literal or an `http.MethodX` constant.
fn go_method(tokens: &[Token], argument: Option<&(usize, usize)>) -> Option<String> {
    let (start, end) = *argument?;
    match end - start {
        1 => tokens[start].literal().and_then(canonical),
        3 if tokens[start].is_ident("http") => tokens[start + 2]
            .ident()
            .and_then(|name| name.strip_prefix("Method"))
            .and_then(canonical),
        _ => None,
    }
}

/// HTTP method selected earlier in a `WebClient` chain ending at the `uri` identifier `index`.
fn chain_method(tokens: &[Token], index: usize) -> Option<String> {
    let mut depth = 0_i32;
    let mut cursor = index;
    while cursor > 0 {
        cursor -= 1;
        let token = &tokens[cursor];
        if token.is_punct(')') {
            depth += 1;
        } else if token.is_punct('(') {
            depth -= 1;
            if depth < 0 {
                return None;
            }
        } else if depth == 0
            && (token.is_punct(';')
                || token.is_punct('{')
                || token.is_punct('}')
                || token.is_punct('='))
        {
            return None;
        }
        if depth != 0 || cursor == 0 || !tokens[cursor - 1].is_punct('.') {
            continue;
        }
        let Some(name) = token.ident() else {
            continue;
        };
        if name == "method" {
            let argument = tokens.get(cursor + 4).and_then(Token::ident);
            return argument.and_then(canonical);
        }
        if tokens
            .get(cursor + 1)
            .is_some_and(|next| next.is_punct('('))
            && tokens
                .get(cursor + 2)
                .is_some_and(|next| next.is_punct(')'))
            && let Some(method) = canonical(name)
        {
            return Some(method);
        }
    }
    None
}

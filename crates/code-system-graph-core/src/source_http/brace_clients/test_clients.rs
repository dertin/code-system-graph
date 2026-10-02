//! Test clients: supertest and Playwright in scripts, `httptest` in Go, and `MockMvc`,
//! `RestAssured`, `WebTestClient`, and `RestTemplate` in Java.

use std::collections::BTreeSet;

use super::{Consumers, canonical, join_base, receiver_call};
use crate::source_http::brace_flows::matching_open;
use crate::source_http::{Token, matching};
use crate::{SourceFramework, UrlPart, UrlTemplate};

const SUPERTEST_METHODS: [&str; 7] = ["get", "post", "put", "patch", "delete", "head", "options"];

impl Consumers<'_, '_> {
    /// `request(app).get(url)` and agents bound to `request(app)` or `request.agent(app)`.
    pub(super) fn supertest(&self, observations: &mut super::SourceObservationCollector<'_>) {
        let tokens = self.tokens();
        let locals = module_locals(tokens, "supertest");
        if locals.is_empty() {
            return;
        }
        let agents = bound_calls(tokens, &locals);
        for index in 0..tokens.len() {
            let Some(name) = tokens[index].ident() else {
                continue;
            };
            if index > 0 && tokens[index - 1].is_punct('.') {
                continue;
            }
            let (base, verb) = if locals.contains(name)
                && tokens
                    .get(index + 1)
                    .is_some_and(|token| token.is_punct('('))
            {
                let Some(close) = matching(tokens, index + 1, '(', ')') else {
                    continue;
                };
                let base = tokens
                    .get(index + 2)
                    .filter(|token| token.literal().is_some())
                    .map(|_| self.scopes.template(index + 2, close, index));
                (base, close + 1)
            } else if agents.contains(name) {
                (None, index + 1)
            } else {
                continue;
            };
            self.chained_verb(
                SourceFramework::Supertest,
                base.as_ref(),
                index,
                verb,
                observations,
            );
        }
    }

    /// Playwright `request.get(url)` through the `request` fixture, `page.request`, or a
    /// context created by `request.newContext()`.
    pub(super) fn playwright(&self, observations: &mut super::SourceObservationCollector<'_>) {
        let tokens = self.tokens();
        if !tokens
            .iter()
            .any(|token| token.literal() == Some("@playwright/test"))
        {
            return;
        }
        let mut receivers = BTreeSet::from(["request".to_owned()]);
        for index in 2..tokens.len() {
            if tokens[index].is_ident("newContext") && tokens[index - 1].is_punct('.') {
                let assign = (index.saturating_sub(8)..index)
                    .rev()
                    .find(|candidate| tokens[*candidate].is_punct('='));
                if let Some(name) = assign
                    .and_then(|assign| assign.checked_sub(1))
                    .and_then(|name| tokens[name].ident())
                {
                    receivers.insert(name.to_owned());
                }
            }
        }
        for index in 0..tokens.len() {
            let Some(name) = tokens[index].ident() else {
                continue;
            };
            let qualified =
                index < 2 || !tokens[index - 1].is_punct('.') || tokens[index - 2].is_ident("page");
            if receivers.contains(name) && qualified {
                self.chained_verb(
                    SourceFramework::Playwright,
                    None,
                    index,
                    index + 1,
                    observations,
                );
            }
        }
    }

    /// `.verb(url, ...)` starting at token `dot`, attributed to the call starting at `start`.
    fn chained_verb(
        &self,
        framework: SourceFramework,
        base: Option<&UrlTemplate>,
        start: usize,
        dot: usize,
        observations: &mut super::SourceObservationCollector<'_>,
    ) {
        let tokens = self.tokens();
        if !tokens.get(dot).is_some_and(|token| token.is_punct('.')) {
            return;
        }
        let Some(verb) = tokens.get(dot + 1).and_then(Token::ident) else {
            return;
        };
        if !SUPERTEST_METHODS.contains(&verb) {
            return;
        }
        let Some((close, arguments)) = self.arguments(dot + 2) else {
            return;
        };
        let template = self
            .argument_template(arguments.first(), start)
            .map(|path| join_base(base, path));
        self.push(
            framework,
            canonical(verb),
            template,
            (start, close),
            observations,
        );
    }

    /// `httptest.NewRequest(method, url, body)`.
    pub(super) fn httptest(&self, observations: &mut super::SourceObservationCollector<'_>) {
        let tokens = self.tokens();
        for index in 0..tokens.len() {
            let Some((receiver, call, open)) = receiver_call(tokens, index) else {
                continue;
            };
            if receiver != "httptest" || call != "NewRequest" {
                continue;
            }
            let Some((close, arguments)) = self.arguments(open) else {
                continue;
            };
            let method = super::go_method(tokens, arguments.first());
            let template = self.argument_template(arguments.get(1), index);
            self.push(
                SourceFramework::Httptest,
                method,
                template,
                (index, close),
                observations,
            );
        }
    }

    /// `mockMvc.perform(get(url))`, also through `MockMvcRequestBuilders` and `request`.
    pub(super) fn mock_mvc(&self, observations: &mut super::SourceObservationCollector<'_>) {
        let tokens = self.tokens();
        for index in 1..tokens.len() {
            if !tokens[index].is_ident("perform")
                || !tokens[index - 1].is_punct('.')
                || !tokens
                    .get(index + 1)
                    .is_some_and(|token| token.is_punct('('))
            {
                continue;
            }
            let mut builder = index + 2;
            if tokens
                .get(builder)
                .is_some_and(|token| token.is_ident("MockMvcRequestBuilders"))
                && tokens
                    .get(builder + 1)
                    .is_some_and(|token| token.is_punct('.'))
            {
                builder += 2;
            }
            let Some(verb) = tokens.get(builder).and_then(Token::ident) else {
                continue;
            };
            let Some((close, arguments)) = self.arguments(builder + 1) else {
                continue;
            };
            let (method, url) = match verb {
                "request" => (
                    java_http_method(tokens, arguments.first()),
                    arguments.get(1),
                ),
                "multipart" => (Some("POST".to_owned()), arguments.first()),
                verb => match canonical(verb) {
                    Some(method) => (Some(method), arguments.first()),
                    None => continue,
                },
            };
            let template = self.argument_template(url, index);
            self.push(
                SourceFramework::MockMvc,
                method,
                template,
                (index, close),
                observations,
            );
        }
    }

    /// `given()...when().get(url)` and `RestAssured.get(url)` chains.
    pub(super) fn rest_assured(&self, observations: &mut super::SourceObservationCollector<'_>) {
        let tokens = self.tokens();
        for index in 1..tokens.len() {
            let Some(verb) = tokens[index].ident() else {
                continue;
            };
            if !SUPERTEST_METHODS.contains(&verb)
                || !tokens[index - 1].is_punct('.')
                || !tokens
                    .get(index + 1)
                    .is_some_and(|token| token.is_punct('('))
            {
                continue;
            }
            let heads = chain_heads(tokens, index - 1);
            if !heads
                .iter()
                .any(|head| matches!(*head, "when" | "given" | "RestAssured"))
            {
                continue;
            }
            let Some((close, arguments)) = self.arguments(index + 1) else {
                continue;
            };
            let Some(template) = self.argument_template(arguments.first(), index) else {
                continue;
            };
            self.push(
                SourceFramework::RestAssured,
                canonical(verb),
                Some(template),
                (index, close),
                observations,
            );
        }
    }

    /// Calls on fields, parameters, and variables declared as `RestTemplate` or
    /// `TestRestTemplate`.
    pub(super) fn rest_template(&self, observations: &mut super::SourceObservationCollector<'_>) {
        let tokens = self.tokens();
        let mut receivers = Vec::new();
        for index in 0..tokens.len().saturating_sub(2) {
            let framework = match tokens[index].ident() {
                Some("RestTemplate") => SourceFramework::RestTemplate,
                Some("TestRestTemplate") => SourceFramework::TestRestTemplate,
                _ => continue,
            };
            let declares = tokens[index + 2].is_punct(';')
                || tokens[index + 2].is_punct('=')
                || tokens[index + 2].is_punct(',')
                || tokens[index + 2].is_punct(')');
            if let Some(name) = tokens[index + 1].ident().filter(|_| declares) {
                receivers.push((name.to_owned(), framework));
            }
        }
        if receivers.is_empty() {
            return;
        }
        for index in 0..tokens.len() {
            let Some((receiver, call, open)) = receiver_call(tokens, index) else {
                continue;
            };
            let receiver = receiver.strip_prefix("this.").unwrap_or(&receiver);
            let Some((_, framework)) = receivers.iter().find(|(name, _)| name == receiver) else {
                continue;
            };
            let Some((close, arguments)) = self.arguments(open) else {
                continue;
            };
            let method = match call.as_str() {
                "getForObject" | "getForEntity" => Some("GET".to_owned()),
                "postForObject" | "postForEntity" | "postForLocation" => Some("POST".to_owned()),
                "put" => Some("PUT".to_owned()),
                "delete" => Some("DELETE".to_owned()),
                "patchForObject" => Some("PATCH".to_owned()),
                "headForHeaders" => Some("HEAD".to_owned()),
                "optionsForAllow" => Some("OPTIONS".to_owned()),
                "exchange" => java_http_method(tokens, arguments.get(1)),
                _ => continue,
            };
            let template = self.argument_template(arguments.first(), index);
            self.push(*framework, method, template, (index, close), observations);
        }
    }
}

/// Whether `template` is addressed to a Go `httptest` server: it starts with the `URL` field of
/// one of `servers`, or it is a relative path in a file that uses `httptest`.
pub(super) fn targets_httptest(
    template: &UrlTemplate,
    servers: &[String],
    uses_httptest: bool,
) -> bool {
    match template.parts.first() {
        Some(UrlPart::Value(Some(name))) => name
            .strip_suffix(".URL")
            .is_some_and(|server| servers.iter().any(|candidate| candidate == server)),
        Some(UrlPart::Text(text)) => uses_httptest && text.starts_with('/'),
        _ => false,
    }
}

/// Names bound to `httptest.NewServer`, `NewTLSServer`, or `NewUnstartedServer`.
pub(super) fn httptest_servers(tokens: &[Token]) -> Vec<String> {
    let mut servers = Vec::new();
    for index in 0..tokens.len() {
        let Some((receiver, call, _)) = receiver_call(tokens, index) else {
            continue;
        };
        if receiver != "httptest"
            || !matches!(
                call.as_str(),
                "NewServer" | "NewTLSServer" | "NewUnstartedServer"
            )
        {
            continue;
        }
        let Some(assign) = index
            .checked_sub(1)
            .filter(|assign| tokens[*assign].is_punct('='))
        else {
            continue;
        };
        let target = if assign > 0 && tokens[assign - 1].is_punct(':') {
            assign - 1
        } else {
            assign
        };
        if let Some(name) = target.checked_sub(1).and_then(|name| tokens[name].ident()) {
            servers.push(name.to_owned());
        }
    }
    servers
}

/// `HttpMethod.X` argument of a Spring call.
fn java_http_method(tokens: &[Token], argument: Option<&(usize, usize)>) -> Option<String> {
    let (start, end) = *argument?;
    (end == start + 3 && tokens[start].is_ident("HttpMethod") && tokens[start + 1].is_punct('.'))
        .then(|| tokens[start + 2].ident())
        .flatten()
        .and_then(canonical)
}

/// Identifiers of the calls and names chained before the `.` at token `dot`.
fn chain_heads(tokens: &[Token], dot: usize) -> Vec<&str> {
    let mut heads = Vec::new();
    let mut cursor = dot;
    while cursor > 0 && tokens[cursor].is_punct('.') {
        let mut previous = cursor - 1;
        if tokens[previous].is_punct(')') {
            let Some(open) = matching_open(tokens, previous) else {
                break;
            };
            let Some(callee) = open.checked_sub(1) else {
                break;
            };
            previous = callee;
        }
        let Some(name) = tokens[previous].ident() else {
            break;
        };
        heads.push(name);
        let Some(next) = previous.checked_sub(1) else {
            break;
        };
        cursor = next;
    }
    heads
}

/// Local names bound to the default or namespace export of the script module `module`.
fn module_locals(tokens: &[Token], module: &str) -> BTreeSet<String> {
    let mut locals = BTreeSet::new();
    for index in 0..tokens.len() {
        if tokens[index].literal() != Some(module) || index < 2 {
            continue;
        }
        if tokens[index - 1].is_ident("from") {
            let Some(import) = (index.saturating_sub(16)..index)
                .rev()
                .find(|candidate| tokens[*candidate].is_ident("import"))
            else {
                continue;
            };
            let clause = &tokens[import + 1..index - 1];
            if let Some(name) = clause
                .first()
                .and_then(Token::ident)
                .filter(|name| *name != "type")
            {
                locals.insert(name.to_owned());
            } else if let [star, alias, name, ..] = clause
                && star.is_punct('*')
                && alias.is_ident("as")
                && let Some(name) = name.ident()
            {
                locals.insert(name.to_owned());
            }
        } else if tokens[index - 1].is_punct('(')
            && tokens[index - 2].is_ident("require")
            && index >= 4
            && tokens[index - 3].is_punct('=')
            && let Some(name) = tokens[index - 4].ident()
        {
            locals.insert(name.to_owned());
        }
    }
    locals
}

/// Names bound to a call of one of `callees`, directly or through one member such as `agent`.
fn bound_calls(tokens: &[Token], callees: &BTreeSet<String>) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for index in 2..tokens.len() {
        let Some(callee) = tokens[index].ident() else {
            continue;
        };
        if !callees.contains(callee) || !tokens[index - 1].is_punct('=') {
            continue;
        }
        let direct = tokens
            .get(index + 1)
            .is_some_and(|token| token.is_punct('('));
        let member = tokens
            .get(index + 1)
            .is_some_and(|token| token.is_punct('.'))
            && tokens
                .get(index + 3)
                .is_some_and(|token| token.is_punct('('));
        if !direct && !member {
            continue;
        }
        if let Some(name) = tokens[index - 2].ident() {
            names.insert(name.to_owned());
        }
    }
    names
}

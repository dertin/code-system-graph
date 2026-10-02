use std::collections::BTreeSet;

use crate::router_mounts::mount_observation;
use crate::source_http::{
    BraceClients, SourceObservationCollector, collect_brace_clients, java_method_lines
};
use crate::source_routers::{
    GoRouterScope, receiver_before, script_imports, script_router_observations
};
use crate::{
    ExtractionLimitExceeded, ExtractionTracker, SourceEpistemicStatus, SourceFramework, SourceLanguage, SourceLineRange, SourceObservation, SourceRole, SourceWarning, SymbolRef, normalize_source_http_path
};

const METHODS: [(&str, &str); 8] = [
    ("delete", "DELETE"),
    ("get", "GET"),
    ("head", "HEAD"),
    ("options", "OPTIONS"),
    ("patch", "PATCH"),
    ("post", "POST"),
    ("put", "PUT"),
    ("trace", "TRACE"),
];

/// Extracts Fetch, Axios, Express, and Fastify facts from JavaScript.
#[must_use]
pub fn parse_javascript_source(source: &str) -> Vec<SourceObservation> {
    parse_ecmascript(source, SourceLanguage::JavaScript)
}

/// Extracts JavaScript framework facts with repository-relative file-route context.
#[must_use]
pub fn parse_javascript_source_at_path(source_path: &str, source: &str) -> Vec<SourceObservation> {
    let observations = collect_ecmascript_at_path(
        source_path,
        source,
        SourceLanguage::JavaScript,
        SourceObservationCollector::unbounded(),
    );
    finish(observations.into_unbounded())
}

/// Extracts JavaScript facts while charging each attempted observation before retention.
///
/// # Errors
///
/// Returns [`ExtractionLimitExceeded`] before an observation or one of its values exceeds the
/// effective per-artifact budget.
pub fn parse_javascript_source_at_path_with_tracker(
    source_path: &str,
    source: &str,
    tracker: &mut ExtractionTracker,
) -> Result<Vec<SourceObservation>, ExtractionLimitExceeded> {
    let observations = collect_ecmascript_at_path(
        source_path,
        source,
        SourceLanguage::JavaScript,
        SourceObservationCollector::bounded(tracker),
    );
    Ok(finish(observations.into_result()?))
}

/// Extracts Fetch, Axios, Express, Fastify, and `NestJS` facts from TypeScript.
#[must_use]
pub fn parse_typescript_source(source: &str) -> Vec<SourceObservation> {
    parse_ecmascript(source, SourceLanguage::TypeScript)
}

/// Extracts TypeScript framework facts with repository-relative file-route context.
#[must_use]
pub fn parse_typescript_source_at_path(source_path: &str, source: &str) -> Vec<SourceObservation> {
    let observations = collect_ecmascript_at_path(
        source_path,
        source,
        SourceLanguage::TypeScript,
        SourceObservationCollector::unbounded(),
    );
    finish(observations.into_unbounded())
}

/// Extracts TypeScript facts while charging each attempted observation before retention.
///
/// # Errors
///
/// Returns [`ExtractionLimitExceeded`] before an observation or one of its values exceeds the
/// effective per-artifact budget.
pub fn parse_typescript_source_at_path_with_tracker(
    source_path: &str,
    source: &str,
    tracker: &mut ExtractionTracker,
) -> Result<Vec<SourceObservation>, ExtractionLimitExceeded> {
    let observations = collect_ecmascript_at_path(
        source_path,
        source,
        SourceLanguage::TypeScript,
        SourceObservationCollector::bounded(tracker),
    );
    Ok(finish(observations.into_result()?))
}

/// Extracts `net/http`, Gin, and Chi facts from Go source.
#[must_use]
pub fn parse_go_source(source: &str) -> Vec<SourceObservation> {
    let observations = collect_go_source(source, SourceObservationCollector::unbounded());
    finish(observations.into_unbounded())
}

/// Extracts Go facts while charging each attempted observation before retention.
///
/// # Errors
///
/// Returns [`ExtractionLimitExceeded`] before an observation or one of its values exceeds the
/// effective per-artifact budget.
pub fn parse_go_source_with_tracker(
    source: &str,
    tracker: &mut ExtractionTracker,
) -> Result<Vec<SourceObservation>, ExtractionLimitExceeded> {
    let observations = collect_go_source(source, SourceObservationCollector::bounded(tracker));
    Ok(finish(observations.into_result()?))
}

fn collect_go_source<'a>(
    source: &str,
    mut observations: SourceObservationCollector<'a>,
) -> SourceObservationCollector<'a> {
    let has_http = source.contains("\"net/http\"");
    let has_gin = source.contains("github.com/gin-gonic/gin");
    let has_chi = source.contains("github.com/go-chi/chi");
    let router_framework = if has_gin {
        SourceFramework::Gin
    } else if has_chi {
        SourceFramework::Chi
    } else {
        SourceFramework::GoNetHttp
    };
    let mut scope = GoRouterScope::default();
    for statement in go_statements(source) {
        if has_http || has_gin || has_chi {
            for mount in scope.observe(&statement, router_framework) {
                observations.push(mount);
            }
        }
        if has_http {
            append_go_pattern_route(&mut observations, &statement, &scope);
        }
        if has_http
            && statement.text.contains("http.HandleFunc(")
            && !first_literal_after_call(&statement.text)
                .is_some_and(|pattern| pattern.contains(' '))
        {
            observations.push(incomplete_observation(
                SourceLanguage::Go,
                SourceFramework::GoNetHttp,
                SourceRole::Provider,
                None,
                first_literal_after_call(&statement.text)
                    .as_deref()
                    .and_then(literal_path),
                route_symbol(&statement.text, None),
                statement.lines,
                SourceWarning::DynamicMethod,
            ));
        }
        if has_gin {
            append_receiver_route(
                &mut observations,
                SourceFramework::Gin,
                &statement,
                true,
                &scope,
            );
        }
        if has_chi {
            append_receiver_route(
                &mut observations,
                SourceFramework::Chi,
                &statement,
                false,
                &scope,
            );
        }
    }
    let clients = BraceClients {
        go_http: has_http,
        ..BraceClients::default()
    };
    collect_brace_clients(source, SourceLanguage::Go, clients, &mut observations);
    observations
}

/// Extracts Spring MVC, Spring `WebClient`, and Feign facts from Java source.
#[must_use]
pub fn parse_java_source(source: &str) -> Vec<SourceObservation> {
    let observations = collect_java_source(source, SourceObservationCollector::unbounded());
    finish(observations.into_unbounded())
}

/// Extracts Java facts while charging each attempted observation before retention.
///
/// # Errors
///
/// Returns [`ExtractionLimitExceeded`] before an observation or one of its values exceeds the
/// effective per-artifact budget.
pub fn parse_java_source_with_tracker(
    source: &str,
    tracker: &mut ExtractionTracker,
) -> Result<Vec<SourceObservation>, ExtractionLimitExceeded> {
    let observations = collect_java_source(source, SourceObservationCollector::bounded(tracker));
    Ok(finish(observations.into_result()?))
}

fn collect_java_source<'a>(
    source: &str,
    mut observations: SourceObservationCollector<'a>,
) -> SourceObservationCollector<'a> {
    let spring = source.contains("org.springframework.web.bind.annotation");
    let web_client = source.contains("org.springframework.web.reactive.function.client.WebClient");
    let feign = source.contains("@FeignClient") || source.contains("openfeign.FeignClient");
    let lines = source.lines().collect::<Vec<_>>();
    let methods = if spring && !feign {
        java_method_lines(source)
    } else {
        Vec::new()
    };
    let mut class_prefix = None::<String>;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if java_annotates_type(&lines, index) {
            if trimmed.starts_with("@RequestMapping") {
                class_prefix = quoted_value(trimmed);
            } else if trimmed.starts_with("@FeignClient") {
                class_prefix = feign_path(trimmed);
            }
            continue;
        }
        if spring
            && trimmed.starts_with('@')
            && let Some((method, path)) = java_mapping(trimmed)
        {
            let path = match (&class_prefix, path) {
                (Some(prefix), path) => Some(format!("/{prefix}/{}", path.unwrap_or_default())),
                (None, path) => path,
            };
            let line = line_number(index);
            let symbol = if feign {
                lines
                    .iter()
                    .skip(index + 1)
                    .take(5)
                    .find_map(|candidate| java_method_name(candidate))
            } else {
                methods
                    .iter()
                    .find(|(start, _)| *start >= line)
                    .map(|(_, name)| name.clone())
            };
            observations.push(http_observation(
                SourceLanguage::Java,
                if feign {
                    SourceFramework::Feign
                } else {
                    SourceFramework::SpringMvc
                },
                if feign {
                    SourceRole::Consumer
                } else {
                    SourceRole::Provider
                },
                method,
                path,
                symbol,
                SourceLineRange {
                    start: line_number(index),
                    end: line_number(index),
                },
            ));
        }
    }
    let clients = BraceClients {
        web_client,
        ..BraceClients::default()
    };
    collect_brace_clients(source, SourceLanguage::Java, clients, &mut observations);
    observations
}

fn parse_ecmascript(source: &str, language: SourceLanguage) -> Vec<SourceObservation> {
    let observations = collect_ecmascript_at_path(
        "",
        source,
        language,
        SourceObservationCollector::unbounded(),
    );
    finish(observations.into_unbounded())
}

fn collect_ecmascript_at_path<'a>(
    source_path: &str,
    source: &str,
    language: SourceLanguage,
    mut observations: SourceObservationCollector<'a>,
) -> SourceObservationCollector<'a> {
    let express = source.contains("from \"express\"")
        || source.contains("from 'express'")
        || source.contains("require(\"express\")")
        || source.contains("require('express')");
    let fastify = source.contains("from \"fastify\"")
        || source.contains("from 'fastify'")
        || source.contains("require(\"fastify\")")
        || source.contains("require('fastify')");
    let axios = source.contains("from \"axios\"")
        || source.contains("from 'axios'")
        || source.contains("require(\"axios\")")
        || source.contains("require('axios')");
    let nest = source.contains("@nestjs/common");
    let mut express_receivers = assigned_receivers(source, "express");
    if express && source.contains("Router") {
        express_receivers.extend(assigned_receivers(source, "Router"));
    }
    let fastify_receivers = assigned_receivers(source, "fastify");
    let statements = statements(source);
    if express {
        let imports = script_imports(&statements);
        for mount in script_router_observations(&statements, language, &express_receivers, &imports)
        {
            observations.push(mount);
        }
    }
    for statement in statements {
        if express {
            append_js_route(
                &mut observations,
                language,
                SourceFramework::Express,
                &statement,
                &express_receivers,
            );
        }
        if fastify {
            append_js_route(
                &mut observations,
                language,
                SourceFramework::Fastify,
                &statement,
                &fastify_receivers,
            );
        }
    }
    if nest {
        append_nest_routes(&mut observations, source, language);
    }
    if let Some(prefix) = nest_global_prefix(source) {
        observations.push(mount_observation(
            language,
            SourceFramework::NestJs,
            SymbolRef::Function(NEST_APPLICATION.to_owned()),
            None,
            Some(&prefix),
            SourceLineRange { start: 1, end: 1 },
        ));
    }
    append_next_app_routes(&mut observations, source_path, source, language);
    let clients = BraceClients {
        axios,
        ..BraceClients::default()
    };
    collect_brace_clients(source, language, clients, &mut observations);
    observations
}

/// Repository-wide router of every `NestJS` controller, mounted by `setGlobalPrefix`.
const NEST_APPLICATION: &str = "@nestjs";

fn append_nest_routes(
    observations: &mut SourceObservationCollector<'_>,
    source: &str,
    language: SourceLanguage,
) {
    let lines = source.lines().collect::<Vec<_>>();
    let mut controller = None::<String>;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if let Some(arguments) = trimmed.strip_prefix("@Controller(") {
            let prefix = quoted_value(arguments.split(')').next().unwrap_or_default());
            controller = Some(prefix.unwrap_or_default());
            continue;
        }
        let Some((method, path)) = nest_mapping(trimmed) else {
            continue;
        };
        let symbol = lines
            .iter()
            .skip(index + 1)
            .take(5)
            .find_map(|candidate| ecmascript_method_name(candidate));
        let path = match (&controller, path) {
            (Some(prefix), path) => Some(format!("/{prefix}/{}", path.unwrap_or_default())),
            (None, Some(path)) if !path.starts_with('/') => Some(format!("/{path}")),
            (None, path) => path,
        };
        let mut observation = http_observation(
            language,
            SourceFramework::NestJs,
            SourceRole::Provider,
            Some(method),
            path,
            symbol,
            SourceLineRange {
                start: line_number(index),
                end: line_number(index),
            },
        );
        observation.router = Some(SymbolRef::Function(NEST_APPLICATION.to_owned()));
        observations.push(observation);
    }
}

fn nest_global_prefix(source: &str) -> Option<String> {
    let at = source.find(".setGlobalPrefix(")?;
    quoted_values(&source[at..source[at..].find(')').map_or(source.len(), |end| at + end)])
        .into_iter()
        .next()
}

fn append_next_app_routes(
    observations: &mut SourceObservationCollector<'_>,
    source_path: &str,
    source: &str,
    language: SourceLanguage,
) {
    let Some(path) = next_app_route_path(source_path) else {
        return;
    };
    for (index, line) in source.lines().enumerate() {
        let Some(method) = next_route_export_method(line) else {
            continue;
        };
        observations.push(http_observation(
            language,
            SourceFramework::NextJs,
            SourceRole::Provider,
            Some(method.clone()),
            Some(path.clone()),
            Some(method),
            SourceLineRange {
                start: line_number(index),
                end: line_number(index),
            },
        ));
    }
}

fn next_app_route_path(source_path: &str) -> Option<String> {
    let normalized = source_path.replace('\\', "/");
    let components = normalized.split('/').collect::<Vec<_>>();
    let route_file = components.last()?;
    let stem = route_file
        .rsplit_once('.')
        .map_or(*route_file, |(stem, _)| stem);
    if stem != "route" {
        return None;
    }
    let app = components
        .windows(2)
        .position(|window| window == ["src", "app"])
        .map(|index| index + 2)
        .or_else(|| {
            components
                .iter()
                .position(|component| *component == "app")
                .map(|index| index + 1)
        })?;
    let segments = components[app..components.len().saturating_sub(1)]
        .iter()
        .filter(|component| !(component.starts_with('(') && component.ends_with(')')))
        .map(|component| {
            component
                .strip_prefix("[[...")
                .and_then(|value| value.strip_suffix("]]"))
                .or_else(|| {
                    component
                        .strip_prefix("[...")
                        .and_then(|value| value.strip_suffix(']'))
                })
                .or_else(|| {
                    component
                        .strip_prefix('[')
                        .and_then(|value| value.strip_suffix(']'))
                })
                .map_or_else(|| (*component).to_owned(), |value| format!("{{{value}}}"))
        })
        .collect::<Vec<_>>();
    Some(normalize_source_http_path(&segments.join("/")))
}

fn next_route_export_method(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with("export ") {
        return None;
    }
    let tokens = trimmed
        .split(|character: char| character.is_whitespace() || matches!(character, '(' | ':' | '='))
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    let candidate = tokens
        .windows(2)
        .find_map(|window| (window[0] == "function").then_some(window[1]))
        .or_else(|| {
            tokens
                .windows(2)
                .find_map(|window| matches!(window[0], "const" | "let").then_some(window[1]))
        })?;
    METHODS
        .iter()
        .find_map(|(_, method)| (*method == candidate).then_some((*method).to_owned()))
}

#[derive(Debug)]
pub(crate) struct Statement {
    pub(crate) text: String,
    pub(crate) lines: SourceLineRange,
}

fn statements(source: &str) -> Vec<Statement> {
    split_statements(source, false)
}

/// Splits Go source into statements, also ending one at each opening or closing block brace so
/// closures and blocks are visible as separate statements.
fn go_statements(source: &str) -> Vec<Statement> {
    split_statements(source, true)
}

fn split_statements(source: &str, split_blocks: bool) -> Vec<Statement> {
    let mut output = Vec::new();
    let mut text = String::new();
    let mut start = 1_u32;
    let mut depth = 0_i32;
    for (index, line) in source.lines().enumerate() {
        let trimmed = strip_line_comment(line).trim();
        if trimmed.is_empty() {
            continue;
        }
        if text.is_empty() {
            start = line_number(index);
        } else {
            text.push(' ');
        }
        text.push_str(trimmed);
        depth += delimiter_delta(trimmed);
        let block_boundary = split_blocks && (trimmed.ends_with('{') || trimmed.starts_with('}'));
        if depth <= 0 || trimmed.ends_with(';') || block_boundary {
            output.push(Statement {
                text: std::mem::take(&mut text),
                lines: SourceLineRange {
                    start,
                    end: line_number(index),
                },
            });
            depth = 0;
        }
    }
    if !text.is_empty() {
        output.push(Statement {
            text,
            lines: SourceLineRange {
                start,
                end: u32::try_from(source.lines().count()).unwrap_or(u32::MAX),
            },
        });
    }
    output
}

fn delimiter_delta(line: &str) -> i32 {
    line.chars().fold(0, |depth, character| match character {
        '(' | '[' => depth + 1,
        ')' | ']' => depth - 1,
        _ => depth,
    })
}

fn append_js_route(
    output: &mut SourceObservationCollector<'_>,
    language: SourceLanguage,
    framework: SourceFramework,
    statement: &Statement,
    receivers: &BTreeSet<String>,
) {
    if !receivers
        .iter()
        .any(|receiver| statement.text.contains(&format!("{receiver}.")))
    {
        return;
    }
    let method = METHODS.iter().find_map(|(name, method)| {
        statement
            .text
            .contains(&format!(".{name}("))
            .then_some((*method).to_owned())
    });
    if method.is_none() {
        return;
    }
    let mut observation = http_observation(
        language,
        framework,
        SourceRole::Provider,
        method.clone(),
        first_literal_after_call(&statement.text),
        route_symbol(&statement.text, method.as_deref()),
        statement.lines,
    );
    observation.router = METHODS
        .iter()
        .find_map(|(name, _)| receiver_before(&statement.text, name))
        .filter(|receiver| receivers.contains(*receiver))
        .map(|receiver| SymbolRef::Local(receiver.to_owned()));
    output.push(observation);
}

fn append_receiver_route(
    output: &mut SourceObservationCollector<'_>,
    framework: SourceFramework,
    statement: &Statement,
    uppercase: bool,
    scope: &GoRouterScope,
) {
    let Some((call, method)) = METHODS.iter().find_map(|(name, method)| {
        let name = if uppercase {
            name.to_ascii_uppercase()
        } else {
            let mut characters = name.chars();
            characters.next().map_or_else(String::new, |first| {
                first.to_ascii_uppercase().to_string() + characters.as_str()
            })
        };
        statement
            .text
            .contains(&format!(".{name}("))
            .then(|| (name, (*method).to_owned()))
    }) else {
        return;
    };
    let mut observation = http_observation(
        SourceLanguage::Go,
        framework,
        SourceRole::Provider,
        Some(method.clone()),
        first_literal_after_call(&statement.text),
        route_symbol(&statement.text, Some(&method)),
        statement.lines,
    );
    observation.router =
        receiver_before(&statement.text, &call).map(|receiver| scope.reference(receiver));
    output.push(observation);
}

/// Records Go 1.22 `ServeMux` patterns such as `mux.HandleFunc("GET /orders/{id}", handler)`.
fn append_go_pattern_route(
    output: &mut SourceObservationCollector<'_>,
    statement: &Statement,
    scope: &GoRouterScope,
) {
    let Some(call) = ["HandleFunc", "Handle"]
        .into_iter()
        .find(|call| statement.text.contains(&format!(".{call}(")))
    else {
        return;
    };
    let Some(pattern) = first_literal_after_call(&statement.text) else {
        return;
    };
    let Some((method, path)) = pattern.split_once(' ') else {
        return;
    };
    let Some(method) = canonical_method(method) else {
        return;
    };
    let mut observation = http_observation(
        SourceLanguage::Go,
        SourceFramework::GoNetHttp,
        SourceRole::Provider,
        Some(method.clone()),
        Some(path.trim().to_owned()),
        route_symbol(&statement.text, Some(&method)),
        statement.lines,
    );
    observation.router = receiver_before(&statement.text, call)
        .filter(|receiver| *receiver != "http")
        .map(|receiver| scope.reference(receiver));
    output.push(observation);
}

fn http_observation(
    language: SourceLanguage,
    framework: SourceFramework,
    role: SourceRole,
    method: Option<String>,
    literal: Option<String>,
    symbol_name: Option<String>,
    lines: SourceLineRange,
) -> SourceObservation {
    let literal_missing = literal.is_none();
    let authority = (role == SourceRole::Consumer)
        .then(|| literal.as_deref().and_then(crate::routes::url_authority))
        .flatten();
    let path = literal.and_then(|value| literal_path(&value));
    let mut warnings = Vec::new();
    if method.is_none() {
        warnings.push(SourceWarning::DynamicMethod);
    }
    if literal_missing {
        warnings.push(SourceWarning::DynamicPath);
    } else if path.is_none() {
        warnings.push(SourceWarning::UnsupportedLiteralPath);
    }
    if role == SourceRole::Provider && symbol_name.is_none() {
        warnings.push(SourceWarning::MissingSymbol);
    }
    let confirmed = method.is_some()
        && path.is_some()
        && (role != SourceRole::Provider || symbol_name.is_some());
    SourceObservation {
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
        status: if confirmed {
            SourceEpistemicStatus::Confirmed
        } else {
            SourceEpistemicStatus::Ambiguous
        },
        confidence: if confirmed { 1.0 } else { 0.0 },
        warnings,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "Incomplete observations preserve every available direct coordinate"
)]
fn incomplete_observation(
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

/// Removes a trailing `//` comment that starts outside string literals.
fn strip_line_comment(line: &str) -> &str {
    let mut quote = None;
    let mut previous = '\0';
    for (index, character) in line.char_indices() {
        match quote {
            Some(active) if character == active && previous != '\\' => quote = None,
            None if matches!(character, '\'' | '"' | '`') => quote = Some(character),
            None if character == '/' && previous == '/' => return &line[..index - 1],
            Some(_) | None => {}
        }
        previous = if previous == '\\' && character == '\\' {
            '\0'
        } else {
            character
        };
    }
    line
}

fn first_literal_after_call(text: &str) -> Option<String> {
    let open = text.find('(')?;
    quoted_value(&text[open + 1..])
}

fn literal_after(text: &str, marker: &str) -> Option<String> {
    let start = text.find(marker)? + marker.len();
    quoted_value(&text[start..])
}

fn quoted_value(text: &str) -> Option<String> {
    quoted_values(text).into_iter().next()
}

pub(crate) fn quoted_values(text: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut quote = None;
    let mut start = 0;
    for (index, character) in text.char_indices() {
        if let Some(active) = quote {
            if character == active && !text[..index].ends_with('\\') {
                values.push(text[start..index].to_owned());
                quote = None;
            }
        } else if character == '\'' || character == '"' || character == '`' {
            quote = Some(character);
            start = index + character.len_utf8();
        }
    }
    values
}

fn assigned_receivers(source: &str, factory: &str) -> BTreeSet<String> {
    source
        .lines()
        .filter_map(|line| {
            let (left, right) = line.split_once('=')?;
            if !right.contains(&format!("{factory}("))
                && !right.contains(&format!("{factory}.Router("))
            {
                return None;
            }
            left.split_whitespace()
                .next_back()
                .map(|name| name.trim().to_owned())
        })
        .collect()
}

/// Handler symbol of a route registration: its last argument as a dotted name, unwrapped from
/// single-argument wrapper calls such as `asyncHandler(controller.list)`, or `METHOD path` for an
/// inline function, which has no name of its own.
fn route_symbol(text: &str, method: Option<&str>) -> Option<String> {
    let arguments = top_level_arguments(text)?;
    if arguments.len() < 2 {
        return None;
    }
    let mut handler = arguments.last()?.trim();
    loop {
        let unprefixed = handler
            .strip_prefix("async")
            .map_or(handler, str::trim_start);
        let inline = ["function", "(", "func(", "func "]
            .iter()
            .any(|prefix| unprefixed.starts_with(prefix))
            || unprefixed
                .split_once("=>")
                .is_some_and(|(parameter, _)| is_dotted_name(parameter.trim()));
        if inline {
            let path = first_literal_after_call(text)?;
            return Some(match method {
                Some(method) => format!("{method} {path}"),
                None => path,
            });
        }
        let name = handler.trim_start_matches('&');
        if is_dotted_name(name) {
            return Some(name.to_owned());
        }
        let (callee, rest) = handler.split_once('(')?;
        let inner = rest.strip_suffix(')')?.trim();
        if !is_dotted_name(callee.trim())
            || inner.is_empty()
            || top_level_arguments(&format!("({inner})"))?.len() != 1
        {
            return None;
        }
        handler = inner;
    }
}

fn is_dotted_name(text: &str) -> bool {
    !text.is_empty()
        && !text.starts_with('.')
        && !text.ends_with('.')
        && !text.starts_with(|character: char| character.is_ascii_digit())
        && text.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '$' | '.')
        })
}

/// Top-level arguments of the first call in `text`, ignoring nested groups and string contents.
fn top_level_arguments(text: &str) -> Option<Vec<&str>> {
    let open = text.find('(')?;
    let mut arguments = Vec::new();
    let mut depth = 0_u32;
    let mut quote = None;
    let mut escaped = false;
    let mut start = open + 1;
    for (offset, character) in text[open + 1..].char_indices() {
        let index = open + 1 + offset;
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
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => {
                arguments.push(&text[start..index]);
                return Some(arguments);
            }
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                arguments.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    None
}

fn canonical_method(value: &str) -> Option<String> {
    METHODS.iter().find_map(|(name, method)| {
        name.eq_ignore_ascii_case(value)
            .then_some((*method).to_owned())
    })
}

fn literal_path(value: &str) -> Option<String> {
    if value.contains("${") || value.contains('{') && !value.starts_with('/') {
        return None;
    }
    let path = if value.starts_with('/') {
        value
    } else {
        let after_scheme = value
            .strip_prefix("http://")
            .or_else(|| value.strip_prefix("https://"))?;
        after_scheme
            .find('/')
            .map_or("/", |index| &after_scheme[index..])
    };
    Some(normalize_source_http_path(
        path.split(['?', '#']).next().unwrap_or(path),
    ))
}

/// Whether the annotation on `index` precedes a class or interface declaration.
fn java_annotates_type(lines: &[&str], index: usize) -> bool {
    if !lines[index].trim_start().starts_with('@') {
        return false;
    }
    lines
        .iter()
        .skip(index + 1)
        .map(|line| line.trim())
        .find(|line| !line.is_empty() && !line.starts_with('@') && !line.starts_with("//"))
        .is_some_and(|declaration| {
            declaration
                .split_whitespace()
                .any(|token| matches!(token, "class" | "interface" | "record"))
        })
}

fn feign_path(line: &str) -> Option<String> {
    let at = line.find("path")?;
    let rest = line[at + "path".len()..].trim_start().strip_prefix('=')?;
    quoted_value(rest)
}

fn java_mapping(line: &str) -> Option<(Option<String>, Option<String>)> {
    for (annotation, method) in [
        ("@DeleteMapping", "DELETE"),
        ("@GetMapping", "GET"),
        ("@PatchMapping", "PATCH"),
        ("@PostMapping", "POST"),
        ("@PutMapping", "PUT"),
    ] {
        if line.starts_with(annotation) {
            return Some((Some(method.to_owned()), literal_after(line, annotation)));
        }
    }
    if line.starts_with("@RequestMapping") {
        let method = METHODS.iter().find_map(|(_, method)| {
            line.contains(&format!("RequestMethod.{method}"))
                .then_some((*method).to_owned())
        });
        return Some((method, quoted_value(line)));
    }
    None
}

fn nest_mapping(line: &str) -> Option<(String, Option<String>)> {
    for (name, method) in [
        ("Delete", "DELETE"),
        ("Get", "GET"),
        ("Head", "HEAD"),
        ("Options", "OPTIONS"),
        ("Patch", "PATCH"),
        ("Post", "POST"),
        ("Put", "PUT"),
    ] {
        let marker = format!("@{name}(");
        if let Some(at) = line.find(&marker) {
            let arguments = &line[at + marker.len()..];
            let literal = (!arguments.trim_start().starts_with(')'))
                .then(|| literal_after(line, &marker))
                .flatten();
            return Some((method.to_owned(), literal));
        }
    }
    None
}

fn java_method_name(line: &str) -> Option<String> {
    let before = line.split('(').next()?.trim();
    before
        .split_whitespace()
        .next_back()
        .filter(|name| !name.starts_with('@'))
        .map(str::to_owned)
}

fn ecmascript_method_name(line: &str) -> Option<String> {
    let before = line.split('(').next()?.trim();
    before
        .split_whitespace()
        .next_back()
        .filter(|name| {
            !name.starts_with('@')
                && name
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        })
        .map(str::to_owned)
}

fn finish(mut observations: Vec<SourceObservation>) -> Vec<SourceObservation> {
    observations.sort_by(|left, right| {
        (
            left.lines,
            left.framework,
            left.role,
            &left.method,
            &left.path,
            &left.symbol_name,
        )
            .cmp(&(
                right.lines,
                right.framework,
                right.role,
                &right.method,
                &right.path,
                &right.symbol_name,
            ))
    });
    observations.dedup();
    observations
}

fn line_number(index: usize) -> u32 {
    u32::try_from(index.saturating_add(1)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::{
        parse_go_source, parse_java_source, parse_javascript_source_at_path, parse_javascript_source_at_path_with_tracker, parse_typescript_source, parse_typescript_source_at_path
    };
    use crate::{
        ExtractionBudgets, ExtractionResource, ExtractionTracker, SourceEpistemicStatus, SourceFramework, SourceRole
    };

    #[test]
    fn polyglot_parser_should_charge_each_attempted_observation_before_retention() {
        let budgets = ExtractionBudgets {
            max_observations_per_artifact: 1,
            ..ExtractionBudgets::default()
        };
        let mut tracker = ExtractionTracker::new("client.js", "source.javascript", &budgets);
        let result = parse_javascript_source_at_path_with_tracker(
            "client.js",
            "fetch('/one');\nfetch('/two');\n",
            &mut tracker,
        );

        assert!(matches!(
            result,
            Err(error)
                if error.resource == ExtractionResource::Observations
                    && error.observed == 2
                    && error.maximum == 1
        ));
    }

    #[test]
    fn route_handlers_should_be_named_through_members_middleware_wrappers_and_inline_functions() {
        let express = r#"
import express from "express";
const app = express();
app.get("/orders", auth, orders.list);
app.post("/orders", asyncHandler(orders.create));
app.delete("/orders/:id", async (req, res) => { res.sendStatus(204); });
"#;
        let gin = r#"
package main
import "github.com/gin-gonic/gin"
func main() {
	r := gin.Default()
	r.GET("/orders/:id", handlers.GetOrder)
}
"#;
        let handlers = parse_typescript_source(express)
            .into_iter()
            .chain(parse_go_source(gin))
            .filter(|item| item.role == SourceRole::Provider)
            .map(|item| item.symbol_name.unwrap_or_default())
            .collect::<Vec<_>>();

        assert_eq!(
            handlers,
            [
                "orders.list",
                "orders.create",
                "DELETE /orders/:id",
                "handlers.GetOrder"
            ]
        );
    }

    #[test]
    fn spring_handlers_should_be_the_method_declared_after_the_mapping() {
        let source = r#"
import org.springframework.web.bind.annotation.*;

@RestController
@RequestMapping("/orders")
class OrdersController {
    @GetMapping("/{id}")
    @Operation(
        summary = "Read one order (by id)",
        description = "Returns the order")
    @PreAuthorize("hasRole('reader')")
    public Order read(@PathVariable String id) { return null; }

    @PostMapping public Order create(@RequestBody Order order) { return order; }
}
"#;
        let handlers = parse_java_source(source)
            .into_iter()
            .filter(|item| item.role == SourceRole::Provider)
            .map(|item| item.symbol_name.unwrap_or_default())
            .collect::<Vec<_>>();

        assert_eq!(handlers, ["read", "create"]);
    }

    #[test]
    fn typescript_should_extract_fetch_axios_express_fastify_and_nestjs() {
        let source = r#"
import express from "express";
import fastify from "fastify";
import axios from "axios";
import { Controller, Get } from "@nestjs/common";
const app = express();
const server = fastify();
app.post("/orders", createOrder);
server.get("/health", health);
fetch("https://api.test/items", { method: "PATCH" });
axios.delete("/items/1");
@Get("/users")
listUsers() {}
"#;
        let result = parse_typescript_source(source);

        assert!(
            [
                SourceFramework::Fetch,
                SourceFramework::Axios,
                SourceFramework::Express,
                SourceFramework::Fastify,
                SourceFramework::NestJs,
            ]
            .into_iter()
            .all(|framework| result.iter().any(|item| item.framework == framework))
        );
    }

    #[test]
    fn absolute_urls_should_survive_comment_stripping_and_record_their_authority() {
        let source = "export async function load() {\n  await fetch('http://Orders-API:8080/v1/orders/42'); // primary\n  await fetch(\"https://payments.example.com/v1/orders/42\");\n}\n";

        let result = parse_typescript_source(source);
        let calls = result
            .iter()
            .map(|item| (item.status, item.path.as_deref(), item.authority.as_deref()))
            .collect::<Vec<_>>();

        assert_eq!(
            calls,
            [
                (
                    SourceEpistemicStatus::Confirmed,
                    Some("/v1/orders/42"),
                    Some("orders-api:8080")
                ),
                (
                    SourceEpistemicStatus::Confirmed,
                    Some("/v1/orders/42"),
                    Some("payments.example.com")
                ),
            ]
        );
    }

    #[test]
    fn multiline_fetch_should_preserve_literal_method_and_path() {
        let result = parse_typescript_source(include_str!(
            "../../../fixtures/platform-demo/web/src/checkout.ts"
        ));

        assert!(
            result.iter().any(|item| {
                item.framework == SourceFramework::Fetch
                    && item.method.as_deref() == Some("POST")
                    && item.path.as_deref() == Some("/api/orders")
            }),
            "{result:?}"
        );
    }

    #[test]
    fn next_app_router_should_derive_static_and_dynamic_file_routes() {
        let javascript = "export async function POST(request) {}\n";
        let typescript = "export const GET = async () => {};\n";

        let post =
            parse_javascript_source_at_path("frontend/src/app/api/cloudflare/route.js", javascript);
        let get = parse_typescript_source_at_path(
            "src/app/(public)/users/[user_id]/route.ts",
            typescript,
        );

        assert!(post.iter().any(|item| {
            item.framework == SourceFramework::NextJs
                && item.method.as_deref() == Some("POST")
                && item.path.as_deref() == Some("/api/cloudflare")
        }));
        assert!(get.iter().any(|item| {
            item.framework == SourceFramework::NextJs
                && item.method.as_deref() == Some("GET")
                && item.path.as_deref() == Some("/users/{user_id}")
        }));
    }

    #[test]
    fn go_should_extract_net_http_gin_and_chi_without_promoting_handle_func_method() {
        let source = r#"
import "net/http"
import "github.com/gin-gonic/gin"
import "github.com/go-chi/chi/v5"
http.NewRequest("POST", "https://api.test/orders", nil)
http.HandleFunc("/health", health)
router.GET("/users", users)
r.Delete("/users/{id}", deleteUser)
"#;
        let result = parse_go_source(source);

        assert!(
            result
                .iter()
                .any(|item| item.framework == SourceFramework::GoNetHttp
                    && item.role == SourceRole::Consumer
                    && item.method.as_deref() == Some("POST"))
        );
        assert!(
            result
                .iter()
                .any(|item| item.framework == SourceFramework::Gin)
        );
        assert!(
            result
                .iter()
                .any(|item| item.framework == SourceFramework::Chi)
        );
        assert!(result.iter().any(|item| {
            item.framework == SourceFramework::GoNetHttp
                && item.status == SourceEpistemicStatus::Incomplete
        }));
    }

    #[test]
    fn java_should_extract_spring_webclient_and_feign_roles() {
        let spring = r#"
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.reactive.function.client.WebClient;
@GetMapping("/orders")
public List<Order> orders() {}
client.post().uri("/events").retrieve();
"#;
        let feign = r#"
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.cloud.openfeign.FeignClient;
@FeignClient(name = "orders")
@PostMapping("/orders")
Order create();
"#;
        let spring_result = parse_java_source(spring);
        let feign_result = parse_java_source(feign);

        assert!(spring_result.iter().any(|item| {
            item.framework == SourceFramework::SpringMvc && item.role == SourceRole::Provider
        }));
        assert!(
            spring_result
                .iter()
                .any(|item| item.framework == SourceFramework::WebClient)
        );
        assert!(
            feign_result
                .iter()
                .any(|item| item.framework == SourceFramework::Feign
                    && item.role == SourceRole::Consumer)
        );
    }
}

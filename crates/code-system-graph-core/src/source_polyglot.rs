use std::collections::BTreeSet;

use crate::source_http::SourceObservationCollector;
use crate::{
    ExtractionLimitExceeded, ExtractionTracker, SourceEpistemicStatus, SourceFramework, SourceLanguage, SourceLineRange, SourceObservation, SourceRole, SourceWarning, normalize_source_http_path
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
    for statement in statements(source) {
        if has_http {
            if let Some(method) = method_call(&statement.text, "http.")
                && matches!(method.as_str(), "GET" | "POST")
            {
                observations.push(http_observation(
                    SourceLanguage::Go,
                    SourceFramework::GoNetHttp,
                    SourceRole::Consumer,
                    Some(method),
                    first_literal_after_call(&statement.text),
                    None,
                    statement.lines,
                ));
            }
            if statement.text.contains("http.NewRequest(") {
                observations.push(http_observation(
                    SourceLanguage::Go,
                    SourceFramework::GoNetHttp,
                    SourceRole::Consumer,
                    literal_method_argument(&statement.text),
                    nth_literal(&statement.text, 2),
                    None,
                    statement.lines,
                ));
            }
            if statement.text.contains("http.HandleFunc(") {
                observations.push(incomplete_observation(
                    SourceLanguage::Go,
                    SourceFramework::GoNetHttp,
                    SourceRole::Provider,
                    None,
                    first_literal_after_call(&statement.text)
                        .as_deref()
                        .and_then(literal_path),
                    argument_identifier(&statement.text, 2),
                    statement.lines,
                    SourceWarning::DynamicMethod,
                ));
            }
        }
        if has_gin {
            append_receiver_route(
                &mut observations,
                SourceLanguage::Go,
                SourceFramework::Gin,
                &statement,
                true,
            );
        }
        if has_chi {
            append_receiver_route(
                &mut observations,
                SourceLanguage::Go,
                SourceFramework::Chi,
                &statement,
                false,
            );
        }
    }
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
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if spring
            && trimmed.starts_with('@')
            && let Some((method, path)) = java_mapping(trimmed)
        {
            let symbol = lines
                .iter()
                .skip(index + 1)
                .take(5)
                .find_map(|candidate| java_method_name(candidate));
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
    if web_client {
        for statement in statements(source) {
            if !statement.text.contains(".uri(") {
                continue;
            }
            let method = METHODS.iter().find_map(|(name, method)| {
                statement
                    .text
                    .contains(&format!(".{name}()"))
                    .then_some((*method).to_owned())
            });
            observations.push(http_observation(
                SourceLanguage::Java,
                SourceFramework::WebClient,
                SourceRole::Consumer,
                method,
                literal_after(&statement.text, ".uri("),
                None,
                statement.lines,
            ));
        }
    }
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
    let express_receivers = assigned_receivers(source, "express");
    let fastify_receivers = assigned_receivers(source, "fastify");
    for statement in statements(source) {
        if statement.text.contains("fetch(") {
            let method = object_method(&statement.text).or_else(|| Some("GET".to_owned()));
            observations.push(http_observation(
                language,
                SourceFramework::Fetch,
                SourceRole::Consumer,
                method,
                literal_after(&statement.text, "fetch("),
                None,
                statement.lines,
            ));
        }
        if axios && statement.text.contains("axios.") {
            let method = method_call(&statement.text, "axios.");
            if method.is_some() {
                observations.push(http_observation(
                    language,
                    SourceFramework::Axios,
                    SourceRole::Consumer,
                    method,
                    first_literal_after_call(&statement.text),
                    None,
                    statement.lines,
                ));
            }
        }
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
        let lines = source.lines().collect::<Vec<_>>();
        for (index, line) in lines.iter().enumerate() {
            let Some((method, path)) = nest_mapping(line.trim()) else {
                continue;
            };
            let symbol = lines
                .iter()
                .skip(index + 1)
                .take(5)
                .find_map(|candidate| ecmascript_method_name(candidate));
            observations.push(http_observation(
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
            ));
        }
    }
    append_next_app_routes(&mut observations, source_path, source, language);
    observations
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
struct Statement {
    text: String,
    lines: SourceLineRange,
}

fn statements(source: &str) -> Vec<Statement> {
    let mut output = Vec::new();
    let mut text = String::new();
    let mut start = 1_u32;
    let mut depth = 0_i32;
    for (index, line) in source.lines().enumerate() {
        let trimmed = line.split("//").next().unwrap_or_default().trim();
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
        if depth <= 0 || trimmed.ends_with(';') {
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
    output.push(http_observation(
        language,
        framework,
        SourceRole::Provider,
        method,
        first_literal_after_call(&statement.text),
        argument_identifier(&statement.text, 2),
        statement.lines,
    ));
}

fn append_receiver_route(
    output: &mut SourceObservationCollector<'_>,
    language: SourceLanguage,
    framework: SourceFramework,
    statement: &Statement,
    uppercase: bool,
) {
    let method = METHODS.iter().find_map(|(name, method)| {
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
            .then_some((*method).to_owned())
    });
    if method.is_none() {
        return;
    }
    output.push(http_observation(
        language,
        framework,
        SourceRole::Provider,
        method,
        first_literal_after_call(&statement.text),
        argument_identifier(&statement.text, 2),
        statement.lines,
    ));
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
        lines,
        status: SourceEpistemicStatus::Incomplete,
        confidence: 0.0,
        warnings: vec![warning],
    }
}

fn method_call(text: &str, prefix: &str) -> Option<String> {
    METHODS.iter().find_map(|(name, method)| {
        text.contains(&format!("{prefix}{name}("))
            .then_some((*method).to_owned())
            .or_else(|| {
                text.contains(&format!("{prefix}{}(", name.to_ascii_uppercase()))
                    .then_some((*method).to_owned())
            })
    })
}

fn first_literal_after_call(text: &str) -> Option<String> {
    let open = text.find('(')?;
    quoted_value(&text[open + 1..])
}

fn literal_after(text: &str, marker: &str) -> Option<String> {
    let start = text.find(marker)? + marker.len();
    quoted_value(&text[start..])
}

fn nth_literal(text: &str, target: usize) -> Option<String> {
    quoted_values(text)
        .into_iter()
        .nth(target.saturating_sub(1))
}

fn quoted_value(text: &str) -> Option<String> {
    quoted_values(text).into_iter().next()
}

fn quoted_values(text: &str) -> Vec<String> {
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

fn literal_method_argument(text: &str) -> Option<String> {
    nth_literal(text, 1).and_then(|value| canonical_method(&value))
}

fn object_method(text: &str) -> Option<String> {
    let start = text.find("method")? + "method".len();
    quoted_value(&text[start..]).and_then(|method| canonical_method(&method))
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

fn argument_identifier(text: &str, target: usize) -> Option<String> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let argument = text[open + 1..close].split(',').nth(target - 1)?.trim();
    let identifier = argument
        .trim_start_matches('&')
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .next()
        .unwrap_or_default();
    (!identifier.is_empty() && !identifier.starts_with(['"', '\'', '`']))
        .then(|| identifier.to_owned())
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
        if line.contains(&marker) {
            return Some((method.to_owned(), literal_after(line, &marker)));
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

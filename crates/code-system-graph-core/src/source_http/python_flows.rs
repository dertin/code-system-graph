//! Python URL expressions, scoped string constants, client wrappers, and call sites.
//!
//! URL arguments are evaluated through literals, implicit and `+` concatenation, f-strings,
//! `str.format`, `%` formatting, and names assigned earlier in the same function or at module
//! level. Calls are recorded so repository composition can instantiate client wrappers at call
//! sites and attribute helper and fixture requests to the tests that use them.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    FunctionSpan, SourceObservationCollector, Token, TokenKind, call_observation, innermost_function, matching, split_operands, top_level_arguments
};
use crate::url_template::{brace_format, printf_format};
use crate::{CallArgument, SourceLanguage, SourceLineRange, SymbolRef, UrlPart, UrlTemplate};

const KEYWORDS: [&str; 30] = [
    "and", "as", "assert", "async", "await", "class", "def", "del", "elif", "else", "except",
    "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal", "not", "or", "pass",
    "raise", "return", "while", "with", "yield", "print", "super",
];

const BUILTINS: [&str; 34] = [
    "abs",
    "all",
    "any",
    "bool",
    "bytes",
    "dict",
    "enumerate",
    "filter",
    "float",
    "format",
    "getattr",
    "hasattr",
    "hash",
    "id",
    "int",
    "isinstance",
    "issubclass",
    "iter",
    "len",
    "list",
    "map",
    "max",
    "min",
    "next",
    "open",
    "range",
    "repr",
    "round",
    "set",
    "setattr",
    "sorted",
    "str",
    "sum",
    "tuple",
];

const STRING_PREFIXES: [&str; 12] = [
    "f", "F", "rf", "fr", "Rf", "fR", "RF", "FR", "rF", "Fr", "b", "u",
];

struct Assignment {
    function: Option<usize>,
    index: usize,
    name: String,
    value: UrlTemplate,
}

/// Function parameters and string assignments of one Python file.
pub(super) struct PythonScopes<'a> {
    tokens: &'a [Token],
    functions: &'a [FunctionSpan],
    parameters: Vec<Vec<String>>,
    assignments: Vec<Assignment>,
}

impl<'a> PythonScopes<'a> {
    pub(super) fn new(tokens: &'a [Token], functions: &'a [FunctionSpan]) -> Self {
        let parameters = functions
            .iter()
            .map(|function| python_parameters(tokens, function))
            .collect();
        let mut scopes = Self {
            tokens,
            functions,
            parameters,
            assignments: Vec::new(),
        };
        for index in 0..tokens.len() {
            if index > 0 && tokens[index - 1].line == tokens[index].line {
                continue;
            }
            let Some((name, value_start)) = assignment_target(tokens, index) else {
                continue;
            };
            let end = statement_end(tokens, value_start);
            let value = scopes.template(value_start, end, index);
            scopes.assignments.push(Assignment {
                function: scopes.innermost(index),
                index,
                name,
                value,
            });
        }
        scopes
    }

    /// Caller-visible parameters of the function named `name`, receivers excluded.
    pub(super) fn parameters_of(&self, function: usize) -> &[String] {
        &self.parameters[function]
    }

    pub(super) fn innermost(&self, index: usize) -> Option<usize> {
        innermost_function(self.functions, index)
    }

    /// Evaluates the expression in `start..end`, resolving names as seen at token `at`.
    pub(super) fn template(&self, start: usize, end: usize, at: usize) -> UrlTemplate {
        let mut template = UrlTemplate::default();
        for (operand_start, operand_end) in split_operands(self.tokens, start, end, '+') {
            template.extend(self.operand(operand_start, operand_end, at));
        }
        template
    }

    fn operand(&self, start: usize, end: usize, at: usize) -> UrlTemplate {
        let tokens = self.tokens;
        let anonymous = || UrlTemplate::part(UrlPart::Value(None));
        if start >= end {
            return anonymous();
        }
        if let [(left_start, left_end), (right_start, right_end)] =
            split_operands(tokens, start, end, '%').as_slice()
            && let Some(format) = self.literal_run(*left_start, *left_end, at)
            && let Some(format) = format.as_literal()
        {
            let arguments = if tokens[*right_start].is_punct('(')
                && matching(tokens, *right_start, '(', ')') == Some(right_end - 1)
            {
                split_operands(tokens, right_start + 1, right_end - 1, ',')
                    .into_iter()
                    .map(|(start, end)| self.template(start, end, at))
                    .collect()
            } else {
                vec![self.template(*right_start, *right_end, at)]
            };
            return printf_format(format, &arguments);
        }
        if let Some(template) = self.literal_run(start, end, at) {
            return template;
        }
        if let Some(format_end) = self.format_call(start, end, at) {
            return format_end;
        }
        if tokens[start].is_punct('(') && matching(tokens, start, '(', ')') == Some(end - 1) {
            return self.template(start + 1, end - 1, at);
        }
        match dotted_name(tokens, start, end) {
            Some(name) => self.resolve(&name, at),
            None => anonymous(),
        }
    }

    /// Adjacent string literals, including f-strings, that span all of `start..end`.
    fn literal_run(&self, start: usize, end: usize, at: usize) -> Option<UrlTemplate> {
        let tokens = self.tokens;
        let mut template = UrlTemplate::default();
        let mut cursor = start;
        while cursor < end {
            let formatted = tokens[cursor]
                .ident()
                .filter(|prefix| STRING_PREFIXES.contains(prefix))
                .filter(|_| {
                    tokens
                        .get(cursor + 1)
                        .is_some_and(|next| next.literal().is_some())
                });
            if let Some(prefix) = formatted {
                let literal = tokens[cursor + 1].literal().unwrap_or_default();
                if prefix.contains(['f', 'F']) {
                    template.extend(brace_format(literal, &[], &BTreeMap::new(), &|name| {
                        self.inline(name, at)
                    }));
                } else {
                    template.push_text(literal);
                }
                cursor += 2;
            } else {
                template.push_text(tokens[cursor].literal()?);
                cursor += 1;
            }
        }
        Some(template)
    }

    /// `"...".format(...)` spanning all of `start..end`.
    fn format_call(&self, start: usize, end: usize, at: usize) -> Option<UrlTemplate> {
        let tokens = self.tokens;
        let dot = (start..end).find(|index| tokens[*index].is_punct('.'))?;
        let format = self.literal_run(start, dot, at)?;
        let format = format.as_literal()?;
        if !tokens
            .get(dot + 1)
            .is_some_and(|token| token.is_ident("format"))
        {
            return None;
        }
        let open = dot + 2;
        let close = matching(tokens, open, '(', ')')?;
        if close + 1 != end {
            return None;
        }
        let mut positional = Vec::new();
        let mut named = BTreeMap::new();
        for (argument_start, argument_end) in split_operands(tokens, open + 1, close, ',') {
            match keyword_argument(tokens, argument_start, argument_end) {
                Some((keyword, value_start)) => {
                    named.insert(keyword, self.template(value_start, argument_end, at));
                }
                None => positional.push(self.template(argument_start, argument_end, at)),
            }
        }
        Some(brace_format(format, &positional, &named, &|name| {
            self.inline(name, at)
        }))
    }

    fn inline(&self, expression: &str, at: usize) -> UrlTemplate {
        let expression = expression.trim();
        let is_dotted = expression.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '_')
        });
        if is_dotted {
            self.resolve(expression, at)
        } else {
            UrlTemplate::part(UrlPart::Value(None))
        }
    }

    /// Resolves a plain or dotted name at token `at`.
    pub(super) fn resolve(&self, name: &str, at: usize) -> UrlTemplate {
        let function = self.innermost(at);
        if let Some(function) = function
            && let Some(index) = self.parameters[function]
                .iter()
                .position(|parameter| parameter == name)
        {
            return UrlTemplate::part(UrlPart::Parameter {
                name: name.to_owned(),
                index,
            });
        }
        let local = self
            .assignments
            .iter()
            .rev()
            .filter(|assignment| assignment.name == name && assignment.index < at)
            .find(|assignment| function.is_some() && assignment.function == function);
        let module = || {
            self.assignments
                .iter()
                .rev()
                .find(|assignment| assignment.name == name && assignment.function.is_none())
        };
        let attribute = || {
            name.starts_with("self.")
                .then(|| {
                    let mut values = self
                        .assignments
                        .iter()
                        .filter(|assignment| assignment.name == name)
                        .map(|assignment| &assignment.value);
                    let first = values.next()?;
                    values.all(|value| value == first).then_some(first)
                })
                .flatten()
        };
        local
            .or_else(module)
            .map(|assignment| &assignment.value)
            .or_else(attribute)
            .filter(|value| !value.has_parameters() || local.is_some())
            .cloned()
            .unwrap_or_else(|| UrlTemplate::part(UrlPart::Value(Some(name.to_owned()))))
    }

    /// Records calls from functions, with their URL-like arguments.
    ///
    /// With `all_calls`, every call is recorded; otherwise only calls that pass a path literal or
    /// forward a parameter of the caller.
    pub(super) fn record_calls(
        &self,
        imports: &BTreeMap<String, (String, String)>,
        clients: &BTreeSet<&str>,
        all_calls: bool,
        observations: &mut SourceObservationCollector<'_>,
    ) {
        let tokens = self.tokens;
        for index in 0..tokens.len() {
            if !tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('('))
            {
                continue;
            }
            let Some(name) = tokens[index].ident() else {
                continue;
            };
            if KEYWORDS.contains(&name) || BUILTINS.contains(&name) {
                continue;
            }
            let Some((start, dotted)) = dotted_before(tokens, index) else {
                continue;
            };
            if start > 0
                && (tokens[start - 1].is_ident("def")
                    || tokens[start - 1].is_ident("class")
                    || tokens[start - 1].is_punct('@'))
            {
                continue;
            }
            if clients.contains(dotted[0].as_str()) {
                continue;
            }
            let Some(function) = self.innermost(index) else {
                continue;
            };
            let Some(close) = matching(tokens, index + 1, '(', ')') else {
                continue;
            };
            let arguments = self.call_arguments(index + 1, close, index);
            let forwards_url = arguments.iter().any(|argument| {
                argument.value.as_ref().is_some_and(|value| {
                    value.has_parameters()
                        || value
                            .parts
                            .iter()
                            .any(|part| matches!(part, UrlPart::Text(text) if text.contains('/')))
                })
            });
            if !all_calls && !forwards_url {
                continue;
            }
            observations.push(call_observation(
                SourceLanguage::Python,
                python_callee(&dotted, imports),
                arguments,
                self.functions[function].name.clone(),
                SourceLineRange {
                    start: tokens[start].line,
                    end: tokens[close].end_line,
                },
            ));
        }
    }

    fn call_arguments(&self, open: usize, close: usize, at: usize) -> Vec<CallArgument> {
        let tokens = self.tokens;
        let mut arguments = split_operands(tokens, open + 1, close, ',')
            .into_iter()
            .filter(|(start, end)| start < end)
            .map(|(start, end)| {
                let (keyword, value_start) = keyword_argument(tokens, start, end)
                    .map_or((None, start), |(keyword, value)| (Some(keyword), value));
                let value = self.template(value_start, end, at);
                let is_string = value
                    .parts
                    .iter()
                    .any(|part| matches!(part, UrlPart::Text(_) | UrlPart::Parameter { .. }));
                CallArgument {
                    keyword,
                    value: is_string.then_some(value),
                }
            })
            .collect::<Vec<_>>();
        while arguments
            .last()
            .is_some_and(|argument| argument.value.is_none())
        {
            arguments.pop();
        }
        arguments
    }
}

/// Records pytest fixture requests of tests and fixtures as calls to the requested fixtures.
pub(super) fn record_fixture_requests(
    scopes: &PythonScopes<'_>,
    observations: &mut SourceObservationCollector<'_>,
) {
    let tokens = scopes.tokens;
    for (position, function) in scopes.functions.iter().enumerate() {
        let is_test = function.name.starts_with("test_");
        let is_fixture =
            (function.start_token.saturating_sub(12)..function.start_token).any(|index| {
                tokens[index].is_ident("fixture")
                    && tokens[..index]
                        .iter()
                        .rev()
                        .take(3)
                        .any(|token| token.is_punct('@'))
            });
        if !is_test && !is_fixture {
            continue;
        }
        for parameter in scopes.parameters_of(position) {
            observations.push(call_observation(
                SourceLanguage::Python,
                SymbolRef::Fixture(parameter.clone()),
                Vec::new(),
                function.name.clone(),
                SourceLineRange {
                    start: tokens[function.start_token].line,
                    end: tokens[function.start_token].end_line,
                },
            ));
        }
    }
}

fn python_callee(dotted: &[String], imports: &BTreeMap<String, (String, String)>) -> SymbolRef {
    let head = &dotted[0];
    let rest = &dotted[1..];
    if let Some((module, name)) = imports.get(head) {
        let name = std::iter::once(name.as_str())
            .filter(|name| !name.is_empty())
            .chain(rest.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(".");
        if !name.is_empty() {
            return SymbolRef::Import {
                module: module.clone(),
                name,
            };
        }
    }
    match (head.as_str(), rest) {
        (_, []) => SymbolRef::Local(head.clone()),
        ("self" | "cls", [method]) => SymbolRef::Local(method.clone()),
        _ => SymbolRef::Call(dotted.join(".")),
    }
}

/// Parameter names a caller binds, without `self`/`cls` receivers or `*`/`**` markers.
fn python_parameters(tokens: &[Token], function: &FunctionSpan) -> Vec<String> {
    let open = function.start_token + 2;
    let Some(close) = matching(tokens, open, '(', ')') else {
        return Vec::new();
    };
    let mut parameters = Vec::new();
    for (position, start) in top_level_arguments(tokens, open, close)
        .into_iter()
        .enumerate()
    {
        let Some(name) = tokens[start..close]
            .iter()
            .take_while(|token| !token.is_punct(':') && !token.is_punct('='))
            .find_map(Token::ident)
        else {
            continue;
        };
        if position == 0 && matches!(name, "self" | "cls") {
            continue;
        }
        parameters.push(name.to_owned());
    }
    parameters
}

/// Target and value start of a statement such as `NAME = ...` or `self.NAME = ...`.
fn assignment_target(tokens: &[Token], index: usize) -> Option<(String, usize)> {
    let head = tokens[index].ident()?;
    let (name, equals) = if head == "self"
        && tokens.get(index + 1)?.is_punct('.')
        && let Some(attribute) = tokens.get(index + 2)?.ident()
    {
        (format!("self.{attribute}"), index + 3)
    } else {
        (head.to_owned(), index + 1)
    };
    let is_assignment = tokens.get(equals)?.is_punct('=')
        && !tokens
            .get(equals + 1)
            .is_some_and(|token| token.is_punct('='));
    (is_assignment && !KEYWORDS.contains(&head)).then_some((name, equals + 1))
}

fn statement_end(tokens: &[Token], start: usize) -> usize {
    let mut depth = 0_i32;
    let mut previous_line = tokens.get(start).map_or(0, |token| token.end_line);
    for (index, token) in tokens.iter().enumerate().skip(start) {
        if index > start && depth == 0 && token.line > previous_line {
            return index;
        }
        match token.kind {
            TokenKind::Punct('(' | '[' | '{') => depth += 1,
            TokenKind::Punct(')' | ']' | '}') => depth -= 1,
            TokenKind::Punct(';') if depth == 0 => return index,
            _ => {}
        }
        previous_line = token.end_line;
    }
    tokens.len()
}

pub(super) fn keyword_argument(
    tokens: &[Token],
    start: usize,
    end: usize,
) -> Option<(String, usize)> {
    let keyword = tokens.get(start)?.ident()?;
    (start + 2 <= end
        && tokens.get(start + 1)?.is_punct('=')
        && !tokens
            .get(start + 2)
            .is_some_and(|token| token.is_punct('=')))
    .then(|| (keyword.to_owned(), start + 2))
}

fn dotted_name(tokens: &[Token], start: usize, end: usize) -> Option<String> {
    let mut name = String::new();
    for (offset, token) in tokens.get(start..end)?.iter().enumerate() {
        if offset % 2 == 0 {
            name.push_str(token.ident()?);
        } else if token.is_punct('.') {
            name.push('.');
        } else {
            return None;
        }
    }
    (!name.is_empty() && !name.ends_with('.')).then_some(name)
}

/// Dotted callee ending at identifier `index`, when it does not continue another expression.
fn dotted_before(tokens: &[Token], index: usize) -> Option<(usize, Vec<String>)> {
    let mut start = index;
    let mut segments = vec![tokens[index].ident()?.to_owned()];
    while start >= 2 && tokens[start - 1].is_punct('.') {
        let previous = tokens[start - 2].ident()?;
        segments.insert(0, previous.to_owned());
        start -= 2;
    }
    if start > 0 && tokens[start - 1].is_punct('.') {
        return None;
    }
    Some((start, segments))
}

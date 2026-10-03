//! Rust URL expressions, string constants, client wrappers, and call sites.
//!
//! URL arguments are evaluated through literals, `format!`, `concat!`, `+` concatenation,
//! `const`/`static` items, and `let` bindings earlier in the same function. Calls are recorded so
//! repository composition can instantiate client wrappers and attribute helper requests to tests.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    FunctionSpan, SourceObservationCollector, Token, TokenKind, call_observation, innermost_function, matching, split_operands
};
use crate::url_template::brace_format;
use crate::{CallArgument, SourceLanguage, SourceLineRange, SymbolRef, UrlPart, UrlTemplate};

const KEYWORDS: [&str; 24] = [
    "if", "while", "match", "for", "loop", "return", "fn", "let", "in", "as", "move", "async",
    "await", "Some", "Ok", "Err", "Box", "Vec", "String", "Self", "self", "unsafe", "where",
    "impl",
];

/// Conversions that leave a string value unchanged.
const IDENTITY_METHODS: [&str; 6] = ["to_string", "to_owned", "as_str", "into", "as_ref", "clone"];

struct Binding {
    function: usize,
    index: usize,
    name: String,
    value: UrlTemplate,
}

/// Function parameters, string constants, and `let` bindings of one Rust file.
pub(super) struct RustScopes<'a> {
    tokens: &'a [Token],
    functions: &'a [FunctionSpan],
    parameters: Vec<Vec<String>>,
    constants: BTreeMap<String, UrlTemplate>,
    bindings: Vec<Binding>,
}

impl<'a> RustScopes<'a> {
    pub(super) fn new(tokens: &'a [Token], functions: &'a [FunctionSpan]) -> Self {
        let mut scopes = Self {
            tokens,
            functions,
            parameters: functions
                .iter()
                .map(|function| rust_parameters(tokens, function))
                .collect(),
            constants: BTreeMap::new(),
            bindings: Vec::new(),
        };
        for index in 0..tokens.len() {
            let is_item = tokens[index].is_ident("const") || tokens[index].is_ident("static");
            let is_let = tokens[index].is_ident("let");
            if !is_item && !is_let {
                continue;
            }
            let mut name_index = index + 1;
            if tokens
                .get(name_index)
                .is_some_and(|token| token.is_ident("mut"))
            {
                name_index += 1;
            }
            let Some(name) = tokens.get(name_index).and_then(Token::ident) else {
                continue;
            };
            let Some(equals) = (name_index + 1..tokens.len())
                .take_while(|candidate| !tokens[*candidate].is_punct(';'))
                .find(|candidate| {
                    tokens[*candidate].is_punct('=')
                        && !tokens
                            .get(candidate + 1)
                            .is_some_and(|token| token.is_punct('=') || token.is_punct('>'))
                })
            else {
                continue;
            };
            let end = statement_end(tokens, equals + 1);
            let value = scopes.template(equals + 1, end, index);
            match (is_item, scopes.innermost(index)) {
                (true, _) => {
                    scopes.constants.insert(name.to_owned(), value);
                }
                (false, Some(function)) => scopes.bindings.push(Binding {
                    function,
                    index,
                    name: name.to_owned(),
                    value,
                }),
                (false, None) => {}
            }
        }
        scopes
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

    fn operand(&self, mut start: usize, mut end: usize, at: usize) -> UrlTemplate {
        let tokens = self.tokens;
        while start < end && tokens[start].is_punct('&') {
            start += 1;
        }
        while end >= start + 4
            && tokens[end - 1].is_punct(')')
            && tokens[end - 2].is_punct('(')
            && tokens[end - 3]
                .ident()
                .is_some_and(|method| IDENTITY_METHODS.contains(&method))
            && tokens[end - 4].is_punct('.')
        {
            end -= 4;
        }
        if start >= end {
            return UrlTemplate::part(UrlPart::Value(None));
        }
        if end == start + 1
            && let Some(literal) = tokens[start].literal()
        {
            return UrlTemplate::text(literal);
        }
        if let Some(macro_name) = tokens[start].ident()
            && tokens
                .get(start + 1)
                .is_some_and(|token| token.is_punct('!'))
            && matching(tokens, start + 2, '(', ')') == Some(end - 1)
        {
            return self.macro_call(macro_name, start + 2, end - 1, at);
        }
        if tokens[start].is_ident("String")
            && end > start + 5
            && tokens[start + 3].is_ident("from")
            && matching(tokens, start + 4, '(', ')') == Some(end - 1)
        {
            return self.template(start + 5, end - 1, at);
        }
        if tokens[start].is_punct('(') && matching(tokens, start, '(', ')') == Some(end - 1) {
            return self.template(start + 1, end - 1, at);
        }
        match path_name(tokens, start, end) {
            Some(name) => self.resolve(&name, at),
            None => UrlTemplate::part(UrlPart::Value(None)),
        }
    }

    fn macro_call(&self, name: &str, open: usize, close: usize, at: usize) -> UrlTemplate {
        let arguments = split_operands(self.tokens, open + 1, close, ',');
        match name {
            "format" => {
                let Some(format) = arguments
                    .first()
                    .and_then(|(start, _)| self.tokens[*start].literal())
                else {
                    return UrlTemplate::part(UrlPart::Value(None));
                };
                let mut positional = Vec::new();
                let mut named = BTreeMap::new();
                for &(start, end) in &arguments[1..] {
                    if let Some(keyword) = self.tokens.get(start).and_then(Token::ident)
                        && self
                            .tokens
                            .get(start + 1)
                            .is_some_and(|token| token.is_punct('='))
                    {
                        named.insert(keyword.to_owned(), self.template(start + 2, end, at));
                    } else {
                        positional.push(self.template(start, end, at));
                    }
                }
                brace_format(format, &positional, &named, &|name| self.resolve(name, at))
            }
            "concat" => {
                let mut joined = UrlTemplate::default();
                for (start, end) in arguments {
                    joined.extend(self.template(start, end, at));
                }
                joined
            }
            _ => UrlTemplate::part(UrlPart::Value(None)),
        }
    }

    fn resolve(&self, name: &str, at: usize) -> UrlTemplate {
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
        let binding = self.bindings.iter().rev().find(|binding| {
            Some(binding.function) == function && binding.index < at && binding.name == name
        });
        let constant = || {
            let last = name.rsplit(':').next().unwrap_or(name);
            self.constants
                .get(name)
                .or_else(|| self.constants.get(last))
        };
        binding
            .map(|binding| &binding.value)
            .or_else(constant)
            .cloned()
            .unwrap_or_else(|| UrlTemplate::part(UrlPart::Value(Some(name.to_owned()))))
    }

    /// Records calls from functions; see the Python counterpart for the selection rule.
    pub(super) fn record_calls(
        &self,
        clients: &BTreeSet<usize>,
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
            if KEYWORDS.contains(&name) {
                continue;
            }
            let Some((start, path)) = path_before(tokens, index) else {
                continue;
            };
            if start > 0 && tokens[start - 1].is_ident("fn")
                || clients.contains(&start)
                || path.starts_with("reqwest::")
            {
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
                SourceLanguage::Rust,
                SymbolRef::Call(path),
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
        let mut arguments = split_operands(self.tokens, open + 1, close, ',')
            .into_iter()
            .filter(|(start, end)| start < end)
            .map(|(start, end)| {
                let value = self.template(start, end, at);
                let is_string = value
                    .parts
                    .iter()
                    .any(|part| matches!(part, UrlPart::Text(_) | UrlPart::Parameter { .. }));
                CallArgument {
                    keyword: None,
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

/// Parameter names a caller binds, without `self` receivers.
fn rust_parameters(tokens: &[Token], function: &FunctionSpan) -> Vec<String> {
    let Some(open) = (function.start_token..function.body_start_token)
        .find(|index| tokens[*index].is_punct('('))
    else {
        return Vec::new();
    };
    let Some(close) = matching(tokens, open, '(', ')') else {
        return Vec::new();
    };
    split_operands(tokens, open + 1, close, ',')
        .into_iter()
        .filter_map(|(start, end)| {
            let pattern = tokens[start..end]
                .iter()
                .take_while(|token| !token.is_punct(':'))
                .filter_map(Token::ident)
                .filter(|name| *name != "mut")
                .collect::<Vec<_>>();
            match pattern.as_slice() {
                [name] if *name != "self" => Some((*name).to_owned()),
                _ => None,
            }
        })
        .collect()
}

fn statement_end(tokens: &[Token], start: usize) -> usize {
    let mut depth = 0_i32;
    for (index, token) in tokens.iter().enumerate().skip(start) {
        match token.kind {
            TokenKind::Punct('(' | '[' | '{') => depth += 1,
            TokenKind::Punct(')' | ']' | '}') => {
                depth -= 1;
                if depth < 0 {
                    return index;
                }
            }
            TokenKind::Punct(';') if depth == 0 => return index,
            _ => {}
        }
    }
    tokens.len()
}

/// Path such as `BASE`, `config::BASE`, or `self.base_url` spanning all of `start..end`.
fn path_name(tokens: &[Token], start: usize, end: usize) -> Option<String> {
    let mut name = String::new();
    let mut cursor = start;
    while cursor < end {
        if let Some(ident) = tokens[cursor].ident() {
            name.push_str(ident);
            cursor += 1;
        } else if tokens[cursor].is_punct('.') {
            name.push('.');
            cursor += 1;
        } else if tokens[cursor].is_punct(':')
            && tokens
                .get(cursor + 1)
                .is_some_and(|token| token.is_punct(':'))
        {
            name.push_str("::");
            cursor += 2;
        } else {
            return None;
        }
    }
    (!name.is_empty() && !name.ends_with(['.', ':'])).then_some(name)
}

/// Callee path ending at identifier `index`, such as `crate::client::get` or `self.fetch`.
fn path_before(tokens: &[Token], index: usize) -> Option<(usize, String)> {
    let mut start = index;
    let mut segments = vec![tokens[index].ident()?];
    loop {
        if start >= 2 && tokens[start - 1].is_punct('.') {
            segments.insert(0, tokens[start - 2].ident()?);
            start -= 2;
        } else if start >= 3 && tokens[start - 1].is_punct(':') && tokens[start - 2].is_punct(':') {
            segments.insert(0, tokens[start - 3].ident()?);
            start -= 3;
        } else {
            break;
        }
    }
    if start > 0 && (tokens[start - 1].is_punct('.') || tokens[start - 1].is_punct('!')) {
        return None;
    }
    Some((start, segments.join("::")))
}

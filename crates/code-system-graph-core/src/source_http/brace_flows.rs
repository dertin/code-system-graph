//! Function scopes, URL expressions, and call sites for JavaScript and TypeScript, Go, and Java.
//!
//! Named functions are discovered per dialect: declarations, functions and arrow functions bound
//! to a name, class and object methods, Go functions and methods, and Java methods. URL
//! expressions are evaluated through literals, template literals, `+` concatenation,
//! `fmt.Sprintf`, `String.format`, conversions, and names bound earlier in the same function or
//! at file level.

use std::cmp::Reverse;
use std::collections::BTreeMap;

use super::brace_lexer::BraceDialect;
use super::{
    FunctionSpan, SourceObservationCollector, Token, TokenKind, call_observation, matching, split_operands
};
use crate::url_template::{CONVERSIONS, printf_format, template_literal};
use crate::{CallArgument, SourceLanguage, SourceLineRange, SymbolRef, UrlPart, UrlTemplate};

const SCRIPT_KEYWORDS: [&str; 22] = [
    "if", "for", "while", "switch", "catch", "function", "return", "with", "typeof", "new",
    "await", "yield", "delete", "void", "super", "import", "require", "else", "do", "try", "in",
    "of",
];

const GO_KEYWORDS: [&str; 14] = [
    "if", "for", "switch", "func", "return", "go", "defer", "select", "range", "make", "new",
    "append", "len", "panic",
];

const JAVA_KEYWORDS: [&str; 16] = [
    "if",
    "for",
    "while",
    "switch",
    "catch",
    "synchronized",
    "return",
    "new",
    "try",
    "else",
    "do",
    "throw",
    "super",
    "this",
    "assert",
    "case",
];

const DECLARATION_MODIFIERS: [&str; 10] = [
    "async",
    "static",
    "get",
    "set",
    "public",
    "private",
    "protected",
    "readonly",
    "override",
    "abstract",
];

struct Binding {
    function: Option<usize>,
    index: usize,
    name: String,
    value: UrlTemplate,
}

/// Named functions, parameters, bindings, and relative imports of one brace-language file.
pub(super) struct BraceScopes<'a> {
    pub(super) tokens: &'a [Token],
    pub(super) dialect: BraceDialect,
    functions: Vec<FunctionSpan>,
    /// Innermost named function of each token.
    owners: Vec<Option<usize>>,
    /// Whether each token belongs to a function signature.
    signatures: Vec<bool>,
    parameters: Vec<Vec<String>>,
    bindings: Vec<Binding>,
    imports: BTreeMap<String, (String, String)>,
}

impl<'a> BraceScopes<'a> {
    /// Scopes of `tokens`, with `extra` functions found by the caller, such as test callbacks.
    pub(super) fn new(
        tokens: &'a [Token],
        dialect: BraceDialect,
        extra: Vec<(FunctionSpan, Vec<String>)>,
    ) -> Self {
        let mut declared = match dialect {
            BraceDialect::Script => script_functions(tokens),
            BraceDialect::Go => go_functions(tokens),
            BraceDialect::Java => java_functions(tokens),
        };
        declared.extend(extra);
        let (functions, parameters): (Vec<FunctionSpan>, _) = declared.into_iter().unzip();
        let mut owners = vec![None; tokens.len()];
        let mut signatures = vec![false; tokens.len()];
        let mut by_size = (0..functions.len()).collect::<Vec<_>>();
        by_size.sort_by_key(|&function| {
            Reverse(functions[function].end_token - functions[function].start_token)
        });
        for function in by_size {
            let span = &functions[function];
            let end = span.end_token.min(tokens.len().saturating_sub(1));
            for owner in owners.iter_mut().take(end + 1).skip(span.start_token) {
                *owner = Some(function);
            }
            for signature in signatures
                .iter_mut()
                .take(span.body_start_token)
                .skip(span.start_token)
            {
                *signature = true;
            }
        }
        let mut scopes = Self {
            tokens,
            dialect,
            functions,
            owners,
            signatures,
            parameters,
            bindings: Vec::new(),
            imports: if dialect == BraceDialect::Script {
                script_imports(tokens)
            } else {
                BTreeMap::new()
            },
        };
        let mut index = 0;
        while index < tokens.len() {
            if let Some((name, value_start)) = scopes.binding_target(index) {
                let end = expression_end(tokens, value_start);
                let value = scopes.template(value_start, end, index);
                scopes.bindings.push(Binding {
                    function: scopes.innermost(index),
                    index,
                    name,
                    value,
                });
            }
            index += 1;
        }
        scopes
    }

    pub(super) fn functions(&self) -> &[FunctionSpan] {
        &self.functions
    }

    pub(super) fn innermost(&self, index: usize) -> Option<usize> {
        self.owners.get(index).copied().flatten()
    }

    pub(super) fn function_name(&self, index: usize) -> Option<String> {
        self.innermost(index)
            .map(|function| self.functions[function].name.clone())
    }

    /// Bound name and value start of a binding at token `index`.
    fn binding_target(&self, index: usize) -> Option<(String, usize)> {
        let tokens = self.tokens;
        let name = tokens[index].ident()?;
        let previous = index.checked_sub(1).map(|previous| &tokens[previous]);
        if tokens.get(index + 1)?.is_punct(':') && tokens.get(index + 2)?.is_punct('=') {
            return Some((name.to_owned(), index + 3));
        }
        let this_member = previous.is_some_and(|token| token.is_punct('.'))
            && index >= 2
            && tokens[index - 2].is_ident("this");
        if previous.is_some_and(|token| token.is_punct('.')) && !this_member {
            return None;
        }
        let mut equals = index + 1;
        if self.dialect == BraceDialect::Script
            && tokens.get(equals)?.is_punct(':')
            && previous.is_some_and(|token| {
                token.is_ident("const") || token.is_ident("let") || token.is_ident("var")
            })
        {
            equals = (equals..tokens.len())
                .take_while(|candidate| !tokens[*candidate].is_punct(';'))
                .find(|candidate| is_assignment(tokens, *candidate))?;
        }
        if !is_assignment(tokens, equals) {
            return None;
        }
        let name = if this_member {
            format!("this.{name}")
        } else {
            name.to_owned()
        };
        Some((name, equals + 1))
    }

    /// Evaluates the expression in `start..end`, resolving names as seen at token `at`.
    pub(super) fn template(&self, start: usize, end: usize, at: usize) -> UrlTemplate {
        let mut template = UrlTemplate::default();
        for (operand_start, operand_end) in split_operands(self.tokens, start, end, '+') {
            template.extend(self.operand(operand_start, operand_end, at));
        }
        template
    }

    fn operand(&self, start: usize, mut end: usize, at: usize) -> UrlTemplate {
        let tokens = self.tokens;
        let anonymous = || UrlTemplate::part(UrlPart::Value(None));
        while end > start + 3
            && tokens[end - 1].is_punct(')')
            && tokens[end - 2].is_punct('(')
            && tokens[end - 3].is_ident("toString")
            && tokens[end - 4].is_punct('.')
        {
            end -= 4;
        }
        if start >= end {
            return anonymous();
        }
        if end == start + 1 {
            return match &tokens[start].kind {
                TokenKind::Literal(Some(value)) => UrlTemplate::text(value),
                TokenKind::Template(body) => {
                    template_literal(body, &|name| self.resolve(name, at)).unwrap_or_else(anonymous)
                }
                TokenKind::Ident(_) => self.resolve(tokens[start].ident().unwrap_or_default(), at),
                TokenKind::Literal(None) | TokenKind::Punct(_) => anonymous(),
            };
        }
        if tokens[start].is_punct('(') && matching(tokens, start, '(', ')') == Some(end - 1) {
            return self.template(start + 1, end - 1, at);
        }
        if let Some(open) = (start..end).find(|index| tokens[*index].is_punct('('))
            && matching(tokens, open, '(', ')') == Some(end - 1)
        {
            return self.call_value(start, open, end - 1, at);
        }
        match dotted_name(tokens, start, end) {
            Some(name) => self.resolve(&name, at),
            None => anonymous(),
        }
    }

    /// Value of a call expression whose callee spans `start..open`.
    fn call_value(&self, start: usize, open: usize, close: usize, at: usize) -> UrlTemplate {
        let tokens = self.tokens;
        let callee = dotted_name(tokens, start, open).unwrap_or_default();
        let callee = callee.strip_prefix("new ").unwrap_or(&callee);
        let arguments = split_operands(tokens, open + 1, close, ',')
            .into_iter()
            .filter(|(argument_start, argument_end)| argument_start < argument_end)
            .map(|(argument_start, argument_end)| self.template(argument_start, argument_end, at))
            .collect::<Vec<_>>();
        let last = callee.rsplit('.').next().unwrap_or_default();
        match (callee, last) {
            ("fmt.Sprintf" | "String.format", _) => {
                let Some(format) = arguments.first().and_then(UrlTemplate::as_literal) else {
                    return UrlTemplate::part(UrlPart::Value(None));
                };
                printf_format(format, &arguments[1..])
            }
            ("URL", _) if start > 0 && tokens[start - 1].is_ident("new") => arguments
                .first()
                .filter(|path| path.as_literal().is_some_and(|path| path.starts_with('/')))
                .cloned()
                .unwrap_or_else(|| UrlTemplate::part(UrlPart::Value(None))),
            (_, conversion) if CONVERSIONS.contains(&conversion) => match arguments.as_slice() {
                [value] if value.as_literal().is_none() => value.clone(),
                _ => UrlTemplate::part(UrlPart::Value(None)),
            },
            _ => UrlTemplate::part(UrlPart::Value(None)),
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
        let visible = |binding: &&Binding| {
            binding.name == name
                && (binding.function.is_none()
                    || (binding.function == function && binding.index < at))
        };
        let local = self
            .bindings
            .iter()
            .rev()
            .filter(visible)
            .find(|binding| binding.function.is_some());
        let global = || {
            self.bindings
                .iter()
                .rev()
                .filter(visible)
                .find(|binding| binding.function.is_none())
        };
        let member = || {
            name.strip_prefix("this.").and_then(|field| {
                let mut values = self
                    .bindings
                    .iter()
                    .filter(|binding| {
                        binding.name == name || binding.function.is_none() && binding.name == field
                    })
                    .map(|binding| &binding.value);
                let first = values.next()?;
                values.all(|value| value == first).then_some(first)
            })
        };
        local
            .or_else(global)
            .map(|binding| &binding.value)
            .or_else(member)
            .filter(|value| !value.has_parameters() || local.is_some())
            .cloned()
            .unwrap_or_else(|| UrlTemplate::part(UrlPart::Value(Some(name.to_owned()))))
    }

    /// Records calls from named functions; with `all_calls` every call, otherwise only calls
    /// that pass a path literal or forward a parameter of the caller.
    pub(super) fn record_calls(
        &self,
        language: SourceLanguage,
        is_client: &dyn Fn(&str) -> bool,
        all_calls: bool,
        observations: &mut SourceObservationCollector<'_>,
    ) {
        let tokens = self.tokens;
        let keywords: &[&str] = match self.dialect {
            BraceDialect::Script => &SCRIPT_KEYWORDS,
            BraceDialect::Go => &GO_KEYWORDS,
            BraceDialect::Java => &JAVA_KEYWORDS,
        };
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
            if keywords.contains(&name) {
                continue;
            }
            let Some((start, path)) = dotted_before(tokens, index) else {
                continue;
            };
            let head = path.split('.').next().unwrap_or_default();
            if self.signatures[index]
                || is_client(head)
                || start > 0
                    && (tokens[start - 1].is_ident("new") || tokens[start - 1].is_ident("func"))
            {
                continue;
            }
            let Some(caller) = self.function_name(index) else {
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
                language,
                self.callee(&path),
                arguments,
                caller,
                SourceLineRange {
                    start: tokens[start].line,
                    end: tokens[close].end_line,
                },
            ));
        }
    }

    fn callee(&self, path: &str) -> SymbolRef {
        let (head, rest) = path
            .split_once('.')
            .map_or((path, None), |(head, rest)| (head, Some(rest)));
        if let Some((module, name)) = self.imports.get(head) {
            let name = match (name.as_str(), rest) {
                ("*", Some(rest)) => rest.to_owned(),
                (name, None) => name.to_owned(),
                (_, Some(_)) => return SymbolRef::Call(path.to_owned()),
            };
            return SymbolRef::Import {
                module: module.clone(),
                name,
            };
        }
        match (self.dialect, rest) {
            (BraceDialect::Script, None) => SymbolRef::Local(head.to_owned()),
            (BraceDialect::Script, Some(method)) if head == "this" && !method.contains('.') => {
                SymbolRef::Local(method.to_owned())
            }
            _ => SymbolRef::Call(path.to_owned()),
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

fn is_assignment(tokens: &[Token], index: usize) -> bool {
    tokens.get(index).is_some_and(|token| token.is_punct('='))
        && !tokens
            .get(index + 1)
            .is_some_and(|token| token.is_punct('=') || token.is_punct('>'))
        && !index.checked_sub(1).is_some_and(|previous| {
            matches!(
                tokens[previous].kind,
                TokenKind::Punct(
                    '=' | '!' | '<' | '>' | '+' | '-' | '*' | '/' | '%' | '&' | '|' | '^' | '?'
                )
            )
        })
}

/// End of the expression starting at `start`: a top-level `;` or `,`, a closing bracket of an
/// enclosing group, or a new line once all groups are closed.
pub(super) fn expression_end(tokens: &[Token], start: usize) -> usize {
    let mut depth = 0_i32;
    let mut previous_line = tokens.get(start).map_or(0, |token| token.end_line);
    for (index, token) in tokens.iter().enumerate().skip(start) {
        if index > start && depth == 0 && token.line > previous_line {
            let continues = matches!(token.kind, TokenKind::Punct('+' | '.' | '?' | ':'))
                || matches!(
                    tokens[index - 1].kind,
                    TokenKind::Punct('+' | '=' | '(' | ',')
                );
            if !continues {
                return index;
            }
        }
        match token.kind {
            TokenKind::Punct('(' | '[' | '{') => depth += 1,
            TokenKind::Punct(')' | ']' | '}') => {
                depth -= 1;
                if depth < 0 {
                    return index;
                }
            }
            TokenKind::Punct(';' | ',') if depth == 0 => return index,
            _ => {}
        }
        previous_line = token.end_line;
    }
    tokens.len()
}

fn dotted_name(tokens: &[Token], start: usize, end: usize) -> Option<String> {
    let mut name = String::new();
    let mut cursor = start;
    if tokens.get(cursor)?.is_ident("new") {
        name.push_str("new ");
        cursor += 1;
    }
    let mut expect_word = true;
    for token in tokens.get(cursor..end)? {
        match (expect_word, token.ident()) {
            (true, Some(word)) => name.push_str(word),
            (false, None) if token.is_punct('.') => name.push('.'),
            (false, None) if token.is_punct('?') || token.is_punct('!') => {
                expect_word = true;
                continue;
            }
            _ => return None,
        }
        expect_word = !expect_word;
    }
    (!expect_word).then_some(name)
}

/// Dotted callee ending at identifier `index`, when it does not continue another expression.
fn dotted_before(tokens: &[Token], index: usize) -> Option<(usize, String)> {
    let mut start = index;
    let mut segments = vec![tokens[index].ident()?];
    while start >= 2 && tokens[start - 1].is_punct('.') {
        segments.insert(0, tokens[start - 2].ident()?);
        start -= 2;
    }
    if start > 0 && tokens[start - 1].is_punct('.') {
        return None;
    }
    Some((start, segments.join(".")))
}

/// Body span of a function whose signature ends at `close`, skipping a return type.
fn body_after(tokens: &[Token], close: usize, end: usize) -> Option<(usize, usize)> {
    let mut depth = 0_i32;
    let mut cursor = close + 1;
    while cursor < end.min(tokens.len()) {
        let token = &tokens[cursor];
        match token.kind {
            TokenKind::Punct('(' | '<' | '[') => depth += 1,
            TokenKind::Punct(')' | '>' | ']') => depth -= 1,
            TokenKind::Punct('{') if depth == 0 => {
                let is_empty_type = tokens
                    .get(cursor + 1)
                    .is_some_and(|next| next.is_punct('}'))
                    && cursor > 0
                    && (tokens[cursor - 1].is_ident("interface")
                        || tokens[cursor - 1].is_ident("struct"));
                if !is_empty_type {
                    return Some((cursor, matching(tokens, cursor, '{', '}')?));
                }
                cursor += 1;
            }
            TokenKind::Punct(';' | '=') if depth == 0 => return None,
            _ => {}
        }
        cursor += 1;
    }
    None
}

fn parameter_list(
    tokens: &[Token],
    open: usize,
    close: usize,
    dialect: BraceDialect,
) -> Vec<String> {
    let parts = split_operands(tokens, open + 1, close, ',')
        .into_iter()
        .filter(|(start, end)| start < end)
        .map(|(start, end)| strip_annotations(&tokens[start..end]))
        .collect::<Vec<_>>();
    match dialect {
        BraceDialect::Script => parts
            .iter()
            .filter_map(|part| {
                part.iter()
                    .map(|token| token.ident())
                    .take_while(Option::is_some)
                    .flatten()
                    .find(|word| !DECLARATION_MODIFIERS.contains(word) && *word != "this")
                    .map(str::to_owned)
            })
            .collect(),
        BraceDialect::Java => parts
            .iter()
            .filter_map(|part| {
                part.iter()
                    .rev()
                    .find_map(|token| token.ident())
                    .map(str::to_owned)
            })
            .collect(),
        BraceDialect::Go => {
            let mut names = Vec::new();
            let mut pending = Vec::new();
            for part in &parts {
                let Some(name) = part.first().and_then(|token| token.ident()) else {
                    pending.clear();
                    continue;
                };
                pending.push(name.to_owned());
                if part.len() > 1 {
                    names.append(&mut pending);
                }
            }
            names
        }
    }
}

/// Tokens of one parameter without `@Annotation(...)` prefixes.
fn strip_annotations(tokens: &[Token]) -> Vec<&Token> {
    let mut output = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        if tokens[index].is_punct('@') {
            index += 2;
            if tokens.get(index).is_some_and(|token| token.is_punct('(')) {
                let mut depth = 0_i32;
                while index < tokens.len() {
                    if tokens[index].is_punct('(') {
                        depth += 1;
                    } else if tokens[index].is_punct(')') {
                        depth -= 1;
                        if depth == 0 {
                            index += 1;
                            break;
                        }
                    }
                    index += 1;
                }
            }
            continue;
        }
        output.push(&tokens[index]);
        index += 1;
    }
    output
}

fn span(name: &str, start: usize, body: (usize, usize)) -> FunctionSpan {
    FunctionSpan {
        name: name.to_owned(),
        start_token: start,
        body_start_token: body.0,
        end_token: body.1,
    }
}

fn script_functions(tokens: &[Token]) -> Vec<(FunctionSpan, Vec<String>)> {
    let mut functions = Vec::new();
    for index in 0..tokens.len() {
        if tokens[index].is_ident("function") {
            let named = tokens.get(index + 1).and_then(Token::ident);
            let open = if named.is_some() {
                index + 2
            } else {
                index + 1
            };
            let open = if tokens.get(open).is_some_and(|token| token.is_punct('*')) {
                open + 1
            } else {
                open
            };
            let Some(close) = matching(tokens, open, '(', ')') else {
                continue;
            };
            let Some(name) = named
                .map(str::to_owned)
                .or_else(|| bound_name(tokens, index))
            else {
                continue;
            };
            if let Some(body) = body_after(tokens, close, tokens.len()) {
                functions.push((
                    span(&name, index, body),
                    parameter_list(tokens, open, close, BraceDialect::Script),
                ));
            }
        } else if tokens[index].is_punct('=')
            && tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('>'))
        {
            if let Some(function) = script_arrow(tokens, index) {
                functions.push(function);
            }
        } else if let Some(function) = script_method(tokens, index) {
            functions.push(function);
        }
    }
    functions
}

/// Name bound to the function or arrow expression starting at `start`.
fn bound_name(tokens: &[Token], start: usize) -> Option<String> {
    let mut cursor = start;
    if cursor > 0 && tokens[cursor - 1].is_ident("async") {
        cursor -= 1;
    }
    let previous = tokens.get(cursor.checked_sub(1)?)?;
    if previous.is_punct(':') {
        return tokens
            .get(cursor.checked_sub(2)?)?
            .ident()
            .map(str::to_owned);
    }
    if !previous.is_punct('=') {
        return None;
    }
    let declaration = (cursor.saturating_sub(16)..cursor - 1)
        .rev()
        .take_while(|index| !matches!(tokens[*index].kind, TokenKind::Punct(';' | '{' | '}')))
        .find(|index| {
            tokens[*index].is_ident("const")
                || tokens[*index].is_ident("let")
                || tokens[*index].is_ident("var")
        });
    match declaration {
        Some(declaration) => tokens.get(declaration + 1)?.ident().map(str::to_owned),
        None => tokens
            .get(cursor.checked_sub(2)?)?
            .ident()
            .map(str::to_owned),
    }
}

/// Opening parenthesis of the group closed at `close`.
pub(super) fn matching_open(tokens: &[Token], close: usize) -> Option<usize> {
    let mut depth = 0_u32;
    for index in (0..=close).rev() {
        if tokens[index].is_punct(')') {
            depth += 1;
        } else if tokens[index].is_punct('(') {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn script_arrow(tokens: &[Token], arrow: usize) -> Option<(FunctionSpan, Vec<String>)> {
    let mut before = arrow.checked_sub(1)?;
    let typed_return = matches!(tokens[before].kind, TokenKind::Punct('>' | ']'))
        || before > 0 && tokens[before - 1].is_punct(':');
    if typed_return {
        before = (before.saturating_sub(32)..before)
            .rev()
            .find(|index| tokens[*index].is_punct(')') && tokens[index + 1].is_punct(':'))?;
    }
    let (start, parameters) = if tokens[before].is_punct(')') {
        let open = matching_open(tokens, before)?;
        (
            open,
            parameter_list(tokens, open, before, BraceDialect::Script),
        )
    } else {
        (before, vec![tokens[before].ident()?.to_owned()])
    };
    let name = bound_name(tokens, start)?;
    let body_start = arrow + 2;
    let end = if tokens
        .get(body_start)
        .is_some_and(|token| token.is_punct('{'))
    {
        matching(tokens, body_start, '{', '}')?
    } else {
        expression_end(tokens, body_start).saturating_sub(1)
    };
    Some((span(&name, start, (body_start, end)), parameters))
}

fn script_method(tokens: &[Token], index: usize) -> Option<(FunctionSpan, Vec<String>)> {
    let name = tokens[index].ident()?;
    if SCRIPT_KEYWORDS.contains(&name) || !tokens.get(index + 1)?.is_punct('(') {
        return None;
    }
    let previous = index.checked_sub(1).map(|previous| &tokens[previous]);
    let declares = previous.is_none_or(|token| {
        matches!(
            token.kind,
            TokenKind::Punct('{' | '}' | ';' | ',' | '*' | ')')
        ) || token
            .ident()
            .is_some_and(|word| DECLARATION_MODIFIERS.contains(&word))
    });
    if !declares {
        return None;
    }
    let close = matching(tokens, index + 1, '(', ')')?;
    if !tokens.get(close + 1)?.is_punct('{') && !tokens.get(close + 1)?.is_punct(':') {
        return None;
    }
    let body = body_after(tokens, close, close + 24)?;
    Some((
        span(name, index, body),
        parameter_list(tokens, index + 1, close, BraceDialect::Script),
    ))
}

fn go_functions(tokens: &[Token]) -> Vec<(FunctionSpan, Vec<String>)> {
    let mut functions = Vec::new();
    for index in 0..tokens.len() {
        if !tokens[index].is_ident("func") {
            continue;
        }
        let mut cursor = index + 1;
        if tokens.get(cursor).is_some_and(|token| token.is_punct('(')) {
            let Some(receiver_close) = matching(tokens, cursor, '(', ')') else {
                continue;
            };
            cursor = receiver_close + 1;
        }
        let Some(name) = tokens.get(cursor).and_then(Token::ident) else {
            continue;
        };
        let open = cursor + 1;
        let Some(close) = matching(tokens, open, '(', ')') else {
            continue;
        };
        if let Some(body) = body_after(tokens, close, tokens.len()) {
            functions.push((
                span(name, index, body),
                parameter_list(tokens, open, close, BraceDialect::Go),
            ));
        }
    }
    functions
}

fn java_functions(tokens: &[Token]) -> Vec<(FunctionSpan, Vec<String>)> {
    let mut functions = Vec::new();
    for index in 1..tokens.len() {
        let Some(name) = tokens[index].ident() else {
            continue;
        };
        if JAVA_KEYWORDS.contains(&name)
            || !tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('('))
        {
            continue;
        }
        let previous = &tokens[index - 1];
        if !(previous
            .ident()
            .is_some_and(|word| !JAVA_KEYWORDS.contains(&word))
            || previous.is_punct('>')
            || previous.is_punct(']'))
        {
            continue;
        }
        let Some(close) = matching(tokens, index + 1, '(', ')') else {
            continue;
        };
        let mut cursor = close + 1;
        if tokens
            .get(cursor)
            .is_some_and(|token| token.is_ident("throws"))
        {
            while tokens
                .get(cursor)
                .is_some_and(|token| !token.is_punct('{') && !token.is_punct(';'))
            {
                cursor += 1;
            }
        }
        if !tokens.get(cursor).is_some_and(|token| token.is_punct('{')) {
            continue;
        }
        let Some(end) = matching(tokens, cursor, '{', '}') else {
            continue;
        };
        functions.push((
            span(name, index, (cursor, end)),
            parameter_list(tokens, index + 1, close, BraceDialect::Java),
        ));
    }
    functions
}

/// Relative ES-module and `CommonJS` imports: local name to `(module, imported name)`; namespace
/// imports use `*` and default imports `default`.
fn script_imports(tokens: &[Token]) -> BTreeMap<String, (String, String)> {
    let mut imports = BTreeMap::new();
    for index in 0..tokens.len() {
        if tokens[index].is_ident("import") {
            let Some(from) = (index + 1..tokens.len().min(index + 64))
                .find(|candidate| tokens[*candidate].is_ident("from"))
            else {
                continue;
            };
            let Some(module) = tokens.get(from + 1).and_then(Token::literal) else {
                continue;
            };
            if !module.starts_with('.') {
                continue;
            }
            bind_imported_names(&tokens[index + 1..from], module, &mut imports);
        } else if tokens[index].is_ident("require")
            && tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('('))
            && let Some(module) = tokens.get(index + 2).and_then(Token::literal)
            && module.starts_with('.')
            && index >= 2
            && tokens[index - 1].is_punct('=')
        {
            let target = &tokens[..index - 1];
            if let Some(name) = target.last().and_then(Token::ident) {
                imports.insert(name.to_owned(), (module.to_owned(), "*".to_owned()));
            } else if target.last().is_some_and(|token| token.is_punct('}'))
                && let Some(open) = target.iter().rposition(|token| token.is_punct('{'))
            {
                bind_imported_names(&target[open..], module, &mut imports);
            }
        }
    }
    imports
}

fn bind_imported_names(
    clause: &[Token],
    module: &str,
    imports: &mut BTreeMap<String, (String, String)>,
) {
    let mut braced = false;
    let mut index = 0;
    while index < clause.len() {
        let token = &clause[index];
        if token.is_punct('{') {
            braced = true;
        } else if token.is_punct('}') {
            braced = false;
        } else if token.is_punct('*')
            && clause
                .get(index + 1)
                .is_some_and(|next| next.is_ident("as"))
            && let Some(alias) = clause.get(index + 2).and_then(Token::ident)
        {
            imports.insert(alias.to_owned(), (module.to_owned(), "*".to_owned()));
            index += 2;
        } else if let Some(name) = token.ident().filter(|name| *name != "type") {
            let (local, skip) = if clause
                .get(index + 1)
                .is_some_and(|next| next.is_ident("as") || next.is_punct(':'))
                && let Some(alias) = clause.get(index + 2).and_then(Token::ident)
            {
                (alias, 2)
            } else {
                (name, 0)
            };
            let imported = if braced { name } else { "default" };
            imports.insert(local.to_owned(), (module.to_owned(), imported.to_owned()));
            index += skip;
        }
        index += 1;
    }
}

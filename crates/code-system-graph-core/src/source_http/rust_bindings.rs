//! Conservative lexical binding facts used by the Rust Reqwest extractor.

use std::collections::{BTreeMap, BTreeSet};

use super::{FunctionSpan, Token, matching};

type ModuleScope = Option<(usize, usize)>;
type ReqwestClientFields = BTreeMap<(ModuleScope, String), BTreeSet<String>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindingKind {
    ReqwestClient,
    Other,
}

struct ScopedClientTypes {
    top_level: BTreeSet<String>,
    modules: BTreeMap<(usize, usize), BTreeSet<String>>,
}

impl ScopedClientTypes {
    fn for_index(&self, index: usize) -> &BTreeSet<String> {
        self.modules
            .iter()
            .filter(|((open, close), _)| *open < index && index < *close)
            .min_by_key(|((open, close), _)| close - open)
            .map_or(&self.top_level, |(_, types)| types)
    }
}

pub(super) fn reqwest_receiver_tokens(
    tokens: &[Token],
    functions: &[FunctionSpan],
) -> BTreeSet<usize> {
    let module_ranges = inline_module_ranges(tokens);
    let client_types = scoped_reqwest_client_types(tokens, &module_ranges);
    let client_fields = reqwest_client_fields(tokens, &client_types, &module_ranges);
    let parameters = functions
        .iter()
        .map(|function| {
            let visible_types = client_types.for_index(function.start_token);
            (
                function.body_start_token,
                function_parameter_bindings(tokens, function, visible_types),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut receivers = BTreeSet::new();
    let mut scopes = Vec::<BTreeMap<String, BindingKind>>::new();
    for index in 0..tokens.len() {
        if tokens[index].is_punct('{') {
            let mut bindings = parameters.get(&index).cloned().unwrap_or_default();
            let visible_types = client_types.for_index(index);
            bindings.extend(closure_parameter_bindings(tokens, index, visible_types));
            scopes.push(bindings);
            continue;
        }
        if tokens[index].is_punct('}') {
            let _ = scopes.pop();
            continue;
        }
        if tokens[index].is_ident("let") {
            let visible_types = client_types.for_index(index);
            record_let_binding(tokens, index, visible_types, &mut scopes);
            continue;
        }
        let Some(name) = tokens[index].ident() else {
            continue;
        };
        if tokens
            .get(index.wrapping_sub(2))
            .is_some_and(|token| token.is_ident("self"))
            && tokens
                .get(index.wrapping_sub(1))
                .is_some_and(|token| token.is_punct('.'))
            && tokens
                .get(index + 1)
                .is_some_and(|token| token.is_punct('.'))
            && enclosing_self_type(tokens, functions, index, &module_ranges)
                .and_then(|(scope, self_type)| client_fields.get(&(scope, self_type.to_owned())))
                .is_some_and(|fields| fields.contains(name))
        {
            receivers.insert(index);
            continue;
        }
        if tokens
            .get(index + 1)
            .is_some_and(|token| token.is_punct('='))
            && !tokens
                .get(index + 2)
                .is_some_and(|token| token.is_punct('='))
        {
            let visible_types = client_types.for_index(index);
            record_assignment(tokens, index, name, visible_types, &mut scopes);
            continue;
        }
        if tokens
            .get(index + 1)
            .is_some_and(|token| token.is_punct('.'))
            && scopes
                .iter()
                .rev()
                .find_map(|scope| scope.get(name))
                .is_some_and(|kind| *kind == BindingKind::ReqwestClient)
        {
            receivers.insert(index);
        }
    }
    receivers
}

fn record_let_binding(
    tokens: &[Token],
    let_index: usize,
    client_types: &BTreeSet<String>,
    scopes: &mut [BTreeMap<String, BindingKind>],
) {
    let name_index = if tokens
        .get(let_index + 1)
        .is_some_and(|token| token.is_ident("mut"))
    {
        let_index + 2
    } else {
        let_index + 1
    };
    let Some(name) = tokens.get(name_index).and_then(Token::ident) else {
        record_pattern_shadowing(tokens, name_index, scopes);
        return;
    };
    let end = (name_index + 1..tokens.len())
        .find(|candidate| tokens[*candidate].is_punct(';'))
        .unwrap_or(tokens.len());
    let assignment = (name_index + 1..end).find(|index| tokens[*index].is_punct('='));
    let annotation =
        (name_index + 1..assignment.unwrap_or(end)).find(|index| tokens[*index].is_punct(':'));
    let is_reqwest = annotation.is_some_and(|colon| {
        type_is_reqwest_client(tokens, colon + 1, assignment.unwrap_or(end), client_types)
    }) || assignment.is_some_and(|equals| {
        initializer_returns_reqwest_client(tokens, equals + 1, end, client_types)
    });
    if let Some(scope) = scopes.last_mut() {
        scope.insert(
            name.to_owned(),
            if is_reqwest {
                BindingKind::ReqwestClient
            } else {
                BindingKind::Other
            },
        );
    }
}

fn record_pattern_shadowing(
    tokens: &[Token],
    start: usize,
    scopes: &mut [BTreeMap<String, BindingKind>],
) {
    let end = (start..tokens.len())
        .find(|index| tokens[*index].is_punct('='))
        .unwrap_or(start);
    if let Some(scope) = scopes.last_mut() {
        for name in tokens[start..end].iter().filter_map(Token::ident) {
            if name != "mut" && name != "ref" {
                scope.insert(name.to_owned(), BindingKind::Other);
            }
        }
    }
}

fn record_assignment(
    tokens: &[Token],
    name_index: usize,
    name: &str,
    client_types: &BTreeSet<String>,
    scopes: &mut [BTreeMap<String, BindingKind>],
) {
    let end = (name_index + 2..tokens.len())
        .find(|index| tokens[*index].is_punct(';'))
        .unwrap_or(tokens.len());
    let kind = if initializer_returns_reqwest_client(tokens, name_index + 2, end, client_types) {
        BindingKind::ReqwestClient
    } else {
        BindingKind::Other
    };
    if let Some(scope) = scopes
        .iter_mut()
        .rev()
        .find(|scope| scope.contains_key(name))
    {
        scope.insert(name.to_owned(), kind);
    }
}

fn scoped_reqwest_client_types(tokens: &[Token], ranges: &[(usize, usize)]) -> ScopedClientTypes {
    let top_level = reqwest_client_types(tokens, ranges, None);
    let modules = ranges
        .iter()
        .copied()
        .map(|range| (range, reqwest_client_types(tokens, ranges, Some(range))))
        .collect();
    ScopedClientTypes { top_level, modules }
}

fn reqwest_client_types(
    tokens: &[Token],
    ranges: &[(usize, usize)],
    scope: Option<(usize, usize)>,
) -> BTreeSet<String> {
    let mut types = reqwest_imported_client_types(tokens, ranges, scope);
    loop {
        let previous = types.len();
        for index in 0..tokens.len() {
            if !tokens[index].is_ident("type") {
                continue;
            }
            if enclosing_inline_module(index, ranges) != scope {
                continue;
            }
            let Some(alias) = tokens.get(index + 1).and_then(Token::ident) else {
                continue;
            };
            let Some(equals) = (index + 2..tokens.len())
                .take_while(|candidate| !tokens[*candidate].is_punct(';'))
                .find(|candidate| tokens[*candidate].is_punct('='))
            else {
                continue;
            };
            let end = (equals + 1..tokens.len())
                .find(|candidate| tokens[*candidate].is_punct(';'))
                .unwrap_or(tokens.len());
            if type_is_reqwest_client(tokens, equals + 1, end, &types) {
                types.insert(alias.to_owned());
            }
        }
        if types.len() == previous {
            break;
        }
    }
    types
}

fn reqwest_imported_client_types(
    tokens: &[Token],
    ranges: &[(usize, usize)],
    scope: Option<(usize, usize)>,
) -> BTreeSet<String> {
    let mut types = BTreeSet::new();
    for use_index in (0..tokens.len()).filter(|index| tokens[*index].is_ident("use")) {
        if enclosing_inline_module(use_index, ranges) != scope {
            continue;
        }
        let end = (use_index + 1..tokens.len())
            .find(|index| tokens[*index].is_punct(';'))
            .unwrap_or(tokens.len());
        let start = use_index + 1;
        if !tokens
            .get(start)
            .is_some_and(|token| token.is_ident("reqwest"))
            || !tokens
                .get(start + 1)
                .is_some_and(|token| token.is_punct(':'))
            || !tokens
                .get(start + 2)
                .is_some_and(|token| token.is_punct(':'))
        {
            continue;
        }
        let tail = start + 3;
        if tokens
            .get(tail)
            .is_some_and(|token| token.is_ident("Client"))
        {
            if let Some(alias) = import_alias(tokens, tail, end) {
                types.insert(alias);
            }
            continue;
        }
        if tokens
            .get(tail)
            .is_some_and(|token| token.is_ident("blocking"))
            && tokens
                .get(tail + 1)
                .is_some_and(|token| token.is_punct(':'))
            && tokens
                .get(tail + 2)
                .is_some_and(|token| token.is_punct(':'))
            && tokens
                .get(tail + 3)
                .is_some_and(|token| token.is_ident("Client"))
        {
            if let Some(alias) = import_alias(tokens, tail + 3, end) {
                types.insert(alias);
            }
            continue;
        }
        if tokens
            .get(tail)
            .is_some_and(|token| token.is_ident("blocking"))
            && tokens
                .get(tail + 1)
                .is_some_and(|token| token.is_punct(':'))
            && tokens
                .get(tail + 2)
                .is_some_and(|token| token.is_punct(':'))
            && tokens
                .get(tail + 3)
                .is_some_and(|token| token.is_punct('{'))
        {
            let open = tail + 3;
            let close = matching(tokens, open, '{', '}').unwrap_or(end).min(end);
            for client in open + 1..close {
                if tokens[client].is_ident("Client")
                    && let Some(alias) = import_alias(tokens, client, close)
                {
                    types.insert(alias);
                }
            }
            continue;
        }
        if !tokens.get(tail).is_some_and(|token| token.is_punct('{')) {
            continue;
        }
        let close = matching(tokens, tail, '{', '}').unwrap_or(end).min(end);
        let mut depth = 0_u32;
        for client in tail + 1..close {
            if tokens[client].is_punct('{') {
                depth += 1;
            } else if tokens[client].is_punct('}') {
                depth = depth.saturating_sub(1);
            } else if depth == 0
                && tokens[client].is_ident("Client")
                && let Some(alias) = import_alias(tokens, client, close)
            {
                types.insert(alias);
            }
        }
    }
    types
}

fn import_alias(tokens: &[Token], item: usize, end: usize) -> Option<String> {
    if item + 2 < end && tokens[item + 1].is_ident("as") {
        return tokens[item + 2].ident().map(str::to_owned);
    }
    Some("Client".to_owned())
}

fn reqwest_client_fields(
    tokens: &[Token],
    client_types: &ScopedClientTypes,
    module_ranges: &[(usize, usize)],
) -> ReqwestClientFields {
    let mut output = BTreeMap::new();
    for index in 0..tokens.len() {
        if !tokens[index].is_ident("struct") {
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
        let Some(close) = matching(tokens, open, '{', '}') else {
            continue;
        };
        let mut fields = BTreeSet::new();
        for field in open + 1..close {
            let Some(field_name) = tokens[field].ident() else {
                continue;
            };
            if !tokens
                .get(field + 1)
                .is_some_and(|token| token.is_punct(':'))
            {
                continue;
            }
            let end = top_level_parameter_end(tokens, field + 2, close);
            if type_is_reqwest_client(tokens, field + 2, end, client_types.for_index(index)) {
                fields.insert(field_name.to_owned());
            }
        }
        if !fields.is_empty() {
            output.insert(
                (
                    enclosing_inline_module(index, module_ranges),
                    name.to_owned(),
                ),
                fields,
            );
        }
    }
    output
}

fn inline_module_ranges(tokens: &[Token]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for index in 0..tokens.len() {
        if !tokens[index].is_ident("mod") {
            continue;
        }
        let Some(open) = (index + 1..tokens.len())
            .take(4)
            .find(|candidate| tokens[*candidate].is_punct('{'))
        else {
            continue;
        };
        if let Some(close) = matching(tokens, open, '{', '}') {
            ranges.push((open, close));
        }
    }
    ranges
}

fn enclosing_inline_module(index: usize, ranges: &[(usize, usize)]) -> Option<(usize, usize)> {
    ranges
        .iter()
        .copied()
        .filter(|(open, close)| *open < index && index < *close)
        .min_by_key(|(open, close)| close - open)
}

fn enclosing_self_type<'a>(
    tokens: &'a [Token],
    functions: &[FunctionSpan],
    token_index: usize,
    module_ranges: &[(usize, usize)],
) -> Option<(ModuleScope, &'a str)> {
    let function = functions
        .iter()
        .filter(|function| function.start_token <= token_index && token_index <= function.end_token)
        .min_by_key(|function| function.end_token - function.start_token)?;
    let self_type = (0..function.start_token)
        .rev()
        .filter(|index| tokens[*index].is_ident("impl"))
        .find_map(|implementation| {
            let open = (implementation + 1..function.start_token)
                .find(|index| tokens[*index].is_punct('{'))?;
            let close = matching(tokens, open, '{', '}')?;
            if close < function.end_token {
                return None;
            }
            let mut type_start = (implementation + 1..open)
                .rfind(|index| tokens[*index].is_ident("for"))
                .map_or(implementation + 1, |index| index + 1);
            if tokens
                .get(type_start)
                .is_some_and(|token| token.is_punct('<'))
                && let Some(generic_end) = matching(tokens, type_start, '<', '>')
            {
                type_start = generic_end + 1;
            }
            (type_start..open).find_map(|index| tokens[index].ident())
        })?;
    Some((
        enclosing_inline_module(function.start_token, module_ranges),
        self_type,
    ))
}

fn function_parameter_bindings(
    tokens: &[Token],
    function: &FunctionSpan,
    client_types: &BTreeSet<String>,
) -> BTreeMap<String, BindingKind> {
    let Some(open) = (function.start_token..function.body_start_token)
        .find(|candidate| tokens[*candidate].is_punct('('))
    else {
        return BTreeMap::new();
    };
    let Some(close) = matching(tokens, open, '(', ')') else {
        return BTreeMap::new();
    };
    parameter_bindings(tokens, open + 1, close, client_types)
}

fn closure_parameter_bindings(
    tokens: &[Token],
    body_start: usize,
    client_types: &BTreeSet<String>,
) -> BTreeMap<String, BindingKind> {
    let Some(close_pipe) = body_start
        .checked_sub(1)
        .filter(|index| tokens[*index].is_punct('|'))
    else {
        return BTreeMap::new();
    };
    let Some(open_pipe) = (0..close_pipe)
        .rev()
        .find(|index| tokens[*index].is_punct('|'))
    else {
        return BTreeMap::new();
    };
    parameter_bindings(tokens, open_pipe + 1, close_pipe, client_types)
}

fn parameter_bindings(
    tokens: &[Token],
    start: usize,
    end: usize,
    client_types: &BTreeSet<String>,
) -> BTreeMap<String, BindingKind> {
    let mut bindings = BTreeMap::new();
    for index in start..end {
        let Some(name) = tokens[index].ident() else {
            continue;
        };
        if !tokens
            .get(index + 1)
            .is_some_and(|token| token.is_punct(':'))
            || tokens
                .get(index.wrapping_sub(1))
                .is_some_and(|token| token.is_punct(':'))
        {
            continue;
        }
        let type_end = top_level_parameter_end(tokens, index + 2, end);
        bindings.insert(
            name.to_owned(),
            if type_is_reqwest_client(tokens, index + 2, type_end, client_types) {
                BindingKind::ReqwestClient
            } else {
                BindingKind::Other
            },
        );
    }
    bindings
}

fn top_level_parameter_end(tokens: &[Token], start: usize, close: usize) -> usize {
    let mut delimiters = Vec::new();
    for (index, token) in tokens.iter().enumerate().take(close).skip(start) {
        match token.ident() {
            None if matches!(token.kind, super::TokenKind::Punct('(' | '[' | '<')) => {
                delimiters.push(index);
            }
            None if matches!(token.kind, super::TokenKind::Punct(')' | ']' | '>')) => {
                let _ = delimiters.pop();
            }
            None if token.is_punct(',') && delimiters.is_empty() => return index,
            Some(_) | None => {}
        }
    }
    close
}

fn type_is_reqwest_client(
    tokens: &[Token],
    start: usize,
    end: usize,
    client_types: &BTreeSet<String>,
) -> bool {
    let mut significant = Vec::new();
    let mut index = start;
    while index < end {
        if tokens[index].is_punct('&') || tokens[index].is_ident("mut") {
            index += 1;
            continue;
        }
        if tokens[index].is_punct('\'') && tokens.get(index + 1).and_then(Token::ident).is_some() {
            index += 2;
            continue;
        }
        significant.push(index);
        index += 1;
    }
    match significant.as_slice() {
        [index] => tokens[*index]
            .ident()
            .is_some_and(|name| client_types.contains(name)),
        [reqwest, first_colon, second_colon, client] => {
            tokens[*reqwest].is_ident("reqwest")
                && tokens[*first_colon].is_punct(':')
                && tokens[*second_colon].is_punct(':')
                && tokens[*client].is_ident("Client")
        }
        [
            reqwest,
            first_colon,
            second_colon,
            blocking,
            third_colon,
            fourth_colon,
            client,
        ] => {
            tokens[*reqwest].is_ident("reqwest")
                && tokens[*first_colon].is_punct(':')
                && tokens[*second_colon].is_punct(':')
                && tokens[*blocking].is_ident("blocking")
                && tokens[*third_colon].is_punct(':')
                && tokens[*fourth_colon].is_punct(':')
                && tokens[*client].is_ident("Client")
        }
        _ => false,
    }
}

fn initializer_returns_reqwest_client(
    tokens: &[Token],
    start: usize,
    end: usize,
    client_types: &BTreeSet<String>,
) -> bool {
    let Some(first) = tokens.get(start).and_then(Token::ident) else {
        return false;
    };
    let (client_index, separator) = if first == "reqwest"
        && tokens
            .get(start + 1)
            .is_some_and(|token| token.is_punct(':'))
        && tokens
            .get(start + 2)
            .is_some_and(|token| token.is_punct(':'))
        && tokens
            .get(start + 3)
            .is_some_and(|token| token.is_ident("Client"))
    {
        (start + 3, start + 4)
    } else if first == "reqwest"
        && tokens
            .get(start + 1)
            .is_some_and(|token| token.is_punct(':'))
        && tokens
            .get(start + 2)
            .is_some_and(|token| token.is_punct(':'))
        && tokens
            .get(start + 3)
            .is_some_and(|token| token.is_ident("blocking"))
        && tokens
            .get(start + 4)
            .is_some_and(|token| token.is_punct(':'))
        && tokens
            .get(start + 5)
            .is_some_and(|token| token.is_punct(':'))
        && tokens
            .get(start + 6)
            .is_some_and(|token| token.is_ident("Client"))
    {
        (start + 6, start + 7)
    } else if client_types.contains(first) {
        (start, start + 1)
    } else {
        return false;
    };
    if !tokens
        .get(separator)
        .is_some_and(|token| token.is_punct(':'))
        || !tokens
            .get(separator + 1)
            .is_some_and(|token| token.is_punct(':'))
    {
        return false;
    }
    let constructor = separator + 2;
    let Some(name) = tokens.get(constructor).and_then(Token::ident) else {
        return false;
    };
    if !tokens
        .get(constructor + 1)
        .is_some_and(|token| token.is_punct('('))
    {
        return false;
    }
    let Some(close) = matching(tokens, constructor + 1, '(', ')').filter(|close| *close < end)
    else {
        return false;
    };
    if matches!(name, "new" | "default") {
        return client_index < end;
    }
    name == "builder"
        && (close + 1..end).any(|index| {
            tokens[index].is_ident("build")
                && tokens
                    .get(index.wrapping_sub(1))
                    .is_some_and(|token| token.is_punct('.'))
                && tokens
                    .get(index + 1)
                    .is_some_and(|token| token.is_punct('('))
                && matching(tokens, index + 1, '(', ')')
                    .is_some_and(|build_close| build_close < end)
        })
}

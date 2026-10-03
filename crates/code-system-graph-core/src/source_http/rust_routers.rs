//! Router ownership and mounts for Axum and Actix Web builder chains.
//!
//! A builder chain belongs to the variable it is bound to, to the function it is returned from,
//! or, when it is passed as an argument, to a synthetic router keyed by its first token. Actix
//! `web::scope` chains prefix every route and service registered on them.

use super::{FunctionSpan, Token, enclosing_symbol, matching, top_level_arguments};
use crate::router_mounts::mount_observation;
use crate::{SourceFramework, SourceLanguage, SourceLineRange, SourceObservation, SymbolRef};

/// Router facts of one Rust file.
pub(super) struct RustRouters<'a> {
    tokens: &'a [Token],
    functions: &'a [FunctionSpan],
}

/// Router that owns a builder chain plus the prefix its `web::scope` base applies.
pub(super) struct ChainOwner {
    pub(super) router: SymbolRef,
    pub(super) prefix: Option<String>,
}

impl<'a> RustRouters<'a> {
    pub(super) fn new(tokens: &'a [Token], functions: &'a [FunctionSpan]) -> Self {
        Self { tokens, functions }
    }

    /// Owner of the chain whose method call is introduced by the `.` at `dot`.
    pub(super) fn chain_owner(&self, dot: usize) -> ChainOwner {
        let base = self.chain_start(dot);
        ChainOwner {
            router: self.base_router(base),
            prefix: self.scope_prefix(base),
        }
    }

    /// Records `nest`, `merge`, `service`, and `configure` mounts.
    pub(super) fn mounts(&self, actix: bool) -> Vec<SourceObservation> {
        let mut output = Vec::new();
        for (index, token) in self.tokens.iter().enumerate() {
            let Some(name) = token.ident() else {
                continue;
            };
            let framework = match name {
                "nest" | "merge" if !actix => SourceFramework::Axum,
                "service" | "configure" if actix => SourceFramework::ActixWeb,
                _ => continue,
            };
            if index == 0
                || !self.tokens[index - 1].is_punct('.')
                || !self
                    .tokens
                    .get(index + 1)
                    .is_some_and(|next| next.is_punct('('))
            {
                continue;
            }
            let open = index + 1;
            let Some(close) = matching(self.tokens, open, '(', ')') else {
                continue;
            };
            let arguments = top_level_arguments(self.tokens, open, close);
            let (prefix, argument) = match (name, arguments.as_slice()) {
                ("nest", [path, argument]) => (self.tokens[*path].literal(), *argument),
                ("merge" | "service" | "configure", [argument]) => (None, *argument),
                _ => continue,
            };
            let end = arguments
                .iter()
                .find(|start| **start > argument)
                .map_or(close, |next| next - 1);
            let Some(child) = self.argument_router(name, argument, end) else {
                continue;
            };
            let owner = self.chain_owner(index - 1);
            let prefix = join_prefixes(owner.prefix.as_deref(), prefix);
            output.push(mount_observation(
                SourceLanguage::Rust,
                framework,
                child,
                Some(owner.router),
                prefix.as_deref(),
                SourceLineRange {
                    start: token.line,
                    end: self.tokens[close].end_line,
                },
            ));
        }
        output
    }

    /// Router passed as an argument spanning `start..end`.
    fn argument_router(&self, method: &str, start: usize, end: usize) -> Option<SymbolRef> {
        let span = self.tokens.get(start..end)?;
        if span.iter().any(|token| token.is_punct('.')) {
            return Some(SymbolRef::Local(format!("chain@{start}")));
        }
        let path = span
            .iter()
            .take_while(|token| !token.is_punct('('))
            .filter_map(Token::ident)
            .collect::<Vec<_>>();
        if path.is_empty() {
            return None;
        }
        let called = span.iter().any(|token| token.is_punct('('));
        match (method, called, path.as_slice()) {
            ("nest" | "merge", false, [variable]) => Some(self.local(start, variable)),
            _ => Some(SymbolRef::Call(path.join("::"))),
        }
    }

    fn local(&self, index: usize, variable: &str) -> SymbolRef {
        match enclosing_symbol(self.functions, index) {
            Some(function) => SymbolRef::Local(format!("{function}.{variable}")),
            None => SymbolRef::Local(variable.to_owned()),
        }
    }

    /// Walks back from a method-call `.` over `ident`, `::`, `.`, and call groups.
    fn chain_start(&self, dot: usize) -> usize {
        let tokens = self.tokens;
        let mut cursor = dot;
        loop {
            let Some(previous) = cursor.checked_sub(1) else {
                return cursor;
            };
            let segment = if tokens[previous].is_punct(')') {
                match matching_backward(tokens, previous) {
                    Some(open) if open > 0 && tokens[open - 1].ident().is_some() => open - 1,
                    _ => return cursor,
                }
            } else if tokens[previous].ident().is_some() {
                previous
            } else {
                return cursor;
            };
            match segment.checked_sub(1).map(|index| &tokens[index]) {
                Some(token) if token.is_punct('.') => cursor = segment - 1,
                Some(token)
                    if token.is_punct(':') && segment >= 2 && tokens[segment - 2].is_punct(':') =>
                {
                    cursor = segment - 2;
                }
                _ => return segment,
            }
        }
    }

    fn base_router(&self, base: usize) -> SymbolRef {
        let tokens = self.tokens;
        let previous = base.checked_sub(1).map(|index| &tokens[index]);
        if previous.is_some_and(|token| token.is_punct('(') || token.is_punct(',')) {
            return SymbolRef::Local(format!("chain@{base}"));
        }
        let is_variable = tokens[base].ident().is_some()
            && tokens
                .get(base + 1)
                .is_some_and(|token| token.is_punct('.'));
        if is_variable && let Some(variable) = tokens[base].ident() {
            if self.is_parameter(base, variable)
                && let Some(function) = enclosing_symbol(self.functions, base)
            {
                return SymbolRef::Function(function);
            }
            return self.local(base, variable);
        }
        if previous.is_some_and(|token| token.is_punct('='))
            && let Some(binding) = (base.saturating_sub(8)..base.saturating_sub(1))
                .rev()
                .find(|index| tokens[*index].is_ident("let"))
                .and_then(|index| {
                    let name = tokens.get(index + 1)?;
                    let name = if name.is_ident("mut") {
                        tokens.get(index + 2)?
                    } else {
                        name
                    };
                    name.ident()
                })
        {
            return self.local(base, binding);
        }
        enclosing_symbol(self.functions, base).map_or_else(
            || SymbolRef::Local(format!("chain@{base}")),
            SymbolRef::Function,
        )
    }

    fn is_parameter(&self, index: usize, variable: &str) -> bool {
        let Some(function) = self
            .functions
            .iter()
            .filter(|function| function.start_token <= index && index <= function.end_token)
            .min_by_key(|function| function.end_token - function.start_token)
        else {
            return false;
        };
        let tokens = self.tokens;
        let Some(open) = (function.start_token..function.body_start_token)
            .find(|candidate| tokens[*candidate].is_punct('('))
        else {
            return false;
        };
        let close = matching(tokens, open, '(', ')').unwrap_or(function.body_start_token);
        (open + 1..close).any(|candidate| {
            tokens[candidate].is_ident(variable)
                && tokens
                    .get(candidate + 1)
                    .is_some_and(|token| token.is_punct(':'))
                && !tokens
                    .get(candidate + 2)
                    .is_some_and(|token| token.is_punct(':'))
        })
    }

    /// Literal of a `web::scope("/p")` chain base.
    fn scope_prefix(&self, base: usize) -> Option<String> {
        let tokens = self.tokens;
        let scope = (base..base + 4).find(|index| {
            tokens
                .get(*index)
                .is_some_and(|token| token.is_ident("scope"))
                && tokens
                    .get(index + 1)
                    .is_some_and(|token| token.is_punct('('))
        })?;
        tokens.get(scope + 2)?.literal().map(str::to_owned)
    }
}

fn matching_backward(tokens: &[Token], close: usize) -> Option<usize> {
    let mut depth = 0_u32;
    for index in (0..=close).rev() {
        if tokens[index].is_punct(')') {
            depth += 1;
        } else if tokens[index].is_punct('(') {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

/// Joins an outer chain prefix with an inner literal prefix.
pub(super) fn join_prefixes(outer: Option<&str>, inner: Option<&str>) -> Option<String> {
    match (outer, inner) {
        (None, None) => None,
        (Some(prefix), None) | (None, Some(prefix)) => Some(prefix.to_owned()),
        (Some(outer), Some(inner)) => Some(format!("{outer}/{inner}")),
    }
}

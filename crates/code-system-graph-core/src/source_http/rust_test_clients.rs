//! In-process Rust test requests: axum `Request` builders sent through `oneshot`, and actix
//! `test::TestRequest` builders.

use super::rust_flows::RustScopes;
use super::{
    FunctionSpan, SourceObservationCollector, Token, canonical_method, enclosing_symbol, http_from_literal, matching, rust_method_expression
};
use crate::{SourceFramework, SourceLanguage, SourceLineRange, SourceRole, UrlTemplate};

/// A request builder chain: its method, URI argument span, and last token.
struct RequestChain {
    method: Option<String>,
    uri: Option<(usize, usize)>,
    end: usize,
}

/// Records axum and actix test requests of one Rust file.
pub(super) fn parse_rust_test_requests(
    tokens: &[Token],
    functions: &[FunctionSpan],
    scopes: &RustScopes<'_>,
    observations: &mut SourceObservationCollector<'_>,
) {
    let oneshot = tokens.iter().any(|token| token.is_ident("oneshot"));
    for index in 0..tokens.len() {
        let framework = match tokens[index].ident() {
            Some("Request") if oneshot => SourceFramework::AxumOneshot,
            Some("TestRequest") => SourceFramework::ActixTest,
            _ => continue,
        };
        if !path_separator(tokens, index + 1) {
            continue;
        }
        let Some(constructor) = tokens.get(index + 3).and_then(Token::ident) else {
            continue;
        };
        let Some(chain) = request_chain(tokens, index + 3, constructor) else {
            continue;
        };
        let template = chain
            .uri
            .map(|(start, end)| scopes.template(start, end, index));
        let literal = template.as_ref().and_then(UrlTemplate::client_literal);
        let mut observation = http_from_literal(
            SourceLanguage::Rust,
            framework,
            SourceRole::Consumer,
            chain.method,
            literal.as_deref(),
            enclosing_symbol(functions, index),
            SourceLineRange {
                start: tokens[index].line,
                end: tokens[chain.end].end_line,
            },
            false,
        );
        observation.url = template.filter(UrlTemplate::has_parameters);
        observations.push(observation);
    }
}

/// The builder chain whose constructor `Type::constructor(...)` is at token `constructor_index`.
fn request_chain(
    tokens: &[Token],
    constructor_index: usize,
    constructor: &str,
) -> Option<RequestChain> {
    let open = constructor_index + 1;
    if !tokens.get(open)?.is_punct('(') {
        return None;
    }
    let close = matching(tokens, open, '(', ')')?;
    let mut chain = RequestChain {
        method: None,
        uri: None,
        end: close,
    };
    match constructor {
        "builder" | "default" => chain.method = Some("GET".to_owned()),
        constructor => {
            chain.method = Some(canonical_method(constructor)?.to_owned());
            if close > open + 1 {
                chain.uri = Some((open + 1, close));
            }
        }
    }
    let mut cursor = close + 1;
    while tokens.get(cursor).is_some_and(|token| token.is_punct('.')) {
        let Some(name) = tokens.get(cursor + 1).and_then(Token::ident) else {
            break;
        };
        if !tokens
            .get(cursor + 2)
            .is_some_and(|token| token.is_punct('('))
        {
            break;
        }
        let Some(call_close) = matching(tokens, cursor + 2, '(', ')') else {
            break;
        };
        match name {
            "uri" => chain.uri = Some((cursor + 3, call_close)),
            "method" => chain.method = rust_method_expression(tokens, cursor + 3, call_close),
            _ => {}
        }
        chain.end = call_close;
        cursor = call_close + 1;
    }
    Some(chain)
}

fn path_separator(tokens: &[Token], at: usize) -> bool {
    tokens.get(at).is_some_and(|token| token.is_punct(':'))
        && tokens.get(at + 1).is_some_and(|token| token.is_punct(':'))
}

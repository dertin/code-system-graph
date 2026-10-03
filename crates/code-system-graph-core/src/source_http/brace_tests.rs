//! Tests of JavaScript and TypeScript, Go, and Java test frameworks.
//!
//! Script tests have no named function: `it` and `test` callbacks are named by their `describe`
//! chain and title, and `beforeEach`-style callbacks by their `describe` chain, so each test keeps
//! a stable identity and calls the setup blocks that run before it. Go tests are `TestX`
//! functions taking `*testing.T`, and Java tests are methods annotated with `@Test`.

use super::brace_flows::{BraceScopes, matching_open};
use super::{
    FunctionSpan, SourceObservationCollector, Token, TokenKind, call_observation, confirmed_test, matching, split_operands
};
use crate::{SourceFramework, SourceLanguage, SourceLineRange, SymbolRef};

const SUITES: [&str; 3] = ["describe", "context", "suite"];
const TESTS: [&str; 3] = ["it", "test", "specify"];
const HOOKS: [&str; 3] = ["beforeEach", "beforeAll", "before"];
const SUITE_MODIFIERS: [&str; 5] = ["only", "skip", "serial", "parallel", "concurrent"];
const TEST_MODIFIERS: [&str; 7] = [
    "only",
    "skip",
    "concurrent",
    "fails",
    "fixme",
    "fail",
    "slow",
];
const JAVA_TEST_ANNOTATIONS: [&str; 4] =
    ["Test", "ParameterizedTest", "RepeatedTest", "TestFactory"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Suite,
    Test,
    Hook,
}

/// A `describe`, `it`/`test`, or setup hook call with its callback.
pub(super) struct ScriptBlock {
    kind: BlockKind,
    title: String,
    name: String,
    start: usize,
    body: (usize, usize),
    parameters: Vec<String>,
    /// Tokens of the innermost enclosing suite body, or of the whole file.
    scope: (usize, usize),
}

impl ScriptBlock {
    /// Function span of a test or hook callback.
    pub(super) fn function(&self) -> Option<(FunctionSpan, Vec<String>)> {
        (self.kind != BlockKind::Suite).then(|| {
            (
                FunctionSpan {
                    name: self.name.clone(),
                    start_token: self.start,
                    body_start_token: self.body.0,
                    end_token: self.body.1,
                },
                self.parameters.clone(),
            )
        })
    }
}

/// Test blocks of one script file, named by their `describe` chain.
pub(super) fn script_blocks(tokens: &[Token]) -> Vec<ScriptBlock> {
    let mut blocks = (0..tokens.len())
        .filter_map(|index| script_block(tokens, index))
        .collect::<Vec<_>>();
    let suites = blocks
        .iter()
        .filter(|block| block.kind == BlockKind::Suite)
        .map(|block| (block.body, block.title.clone()))
        .collect::<Vec<_>>();
    for block in &mut blocks {
        let enclosing = suites
            .iter()
            .filter(|((start, end), _)| *start < block.start && block.start < *end)
            .collect::<Vec<_>>();
        if let Some(((start, end), _)) = enclosing.last() {
            block.scope = (*start, *end);
        }
        block.name = enclosing
            .iter()
            .map(|(_, title)| title.as_str())
            .chain([block.title.as_str()])
            .collect::<Vec<_>>()
            .join(" > ");
    }
    blocks
}

fn script_block(tokens: &[Token], index: usize) -> Option<ScriptBlock> {
    let head = tokens[index].ident()?;
    if index > 0 && tokens[index - 1].is_punct('.') {
        return None;
    }
    let mut open = index + 1;
    let mut modifier = None;
    if tokens.get(open)?.is_punct('.') {
        modifier = Some(tokens.get(open + 1)?.ident()?);
        open += 2;
    }
    if !tokens.get(open)?.is_punct('(') {
        return None;
    }
    let kind = match (head, modifier) {
        (head, None) if SUITES.contains(&head) => BlockKind::Suite,
        (head, Some(modifier)) if SUITES.contains(&head) && SUITE_MODIFIERS.contains(&modifier) => {
            BlockKind::Suite
        }
        ("test", Some("describe")) => BlockKind::Suite,
        (head, None) if HOOKS.contains(&head) => BlockKind::Hook,
        ("test", Some(modifier)) if HOOKS.contains(&modifier) => BlockKind::Hook,
        (head, None) if TESTS.contains(&head) => BlockKind::Test,
        (head, Some(modifier)) if TESTS.contains(&head) && TEST_MODIFIERS.contains(&modifier) => {
            BlockKind::Test
        }
        _ => return None,
    };
    let close = matching(tokens, open, '(', ')')?;
    let arguments = split_operands(tokens, open + 1, close, ',');
    let title = match kind {
        BlockKind::Hook => modifier.unwrap_or(head).to_owned(),
        BlockKind::Suite | BlockKind::Test => {
            let (start, end) = *arguments.first()?;
            static_title(tokens, start, end)?
        }
    };
    let (callback_start, callback_end) = *arguments.last()?;
    let (body, parameters) = callback(tokens, callback_start, callback_end)?;
    Some(ScriptBlock {
        kind,
        title: title.clone(),
        name: title,
        start: index,
        body,
        parameters,
        scope: (0, tokens.len()),
    })
}

fn static_title(tokens: &[Token], start: usize, end: usize) -> Option<String> {
    if end != start + 1 {
        return None;
    }
    match &tokens[start].kind {
        TokenKind::Literal(Some(value)) => Some(value.clone()),
        TokenKind::Template(body) if !body.contains("${") => Some(body.clone()),
        _ => None,
    }
}

/// Body and parameters of the function or arrow expression in `start..end`.
fn callback(tokens: &[Token], start: usize, end: usize) -> Option<((usize, usize), Vec<String>)> {
    let mut cursor = start;
    if tokens.get(cursor)?.is_ident("async") {
        cursor += 1;
    }
    if tokens.get(cursor)?.is_ident("function") {
        cursor += 1;
        if tokens.get(cursor)?.ident().is_some() {
            cursor += 1;
        }
        let close = matching(tokens, cursor, '(', ')')?;
        let body = (close + 1..end).find(|index| tokens[*index].is_punct('{'))?;
        return Some((
            (body, matching(tokens, body, '{', '}')?),
            parameter_names(tokens, cursor, close),
        ));
    }
    let (parameters, arrow) = if tokens.get(cursor)?.is_punct('(') {
        let close = matching(tokens, cursor, '(', ')')?;
        let arrow = (close + 1..end).find(|index| {
            tokens[*index].is_punct('=')
                && tokens.get(index + 1).is_some_and(|next| next.is_punct('>'))
        })?;
        (parameter_names(tokens, cursor, close), arrow)
    } else {
        let name = tokens.get(cursor)?.ident()?;
        if !tokens.get(cursor + 1)?.is_punct('=') || !tokens.get(cursor + 2)?.is_punct('>') {
            return None;
        }
        (vec![name.to_owned()], cursor + 1)
    };
    let body_start = arrow + 2;
    if body_start >= end {
        return None;
    }
    let body_end = if tokens[body_start].is_punct('{') {
        matching(tokens, body_start, '{', '}')?
    } else {
        end - 1
    };
    Some(((body_start, body_end), parameters))
}

fn parameter_names(tokens: &[Token], open: usize, close: usize) -> Vec<String> {
    split_operands(tokens, open + 1, close, ',')
        .into_iter()
        .filter(|(start, end)| start < end)
        .filter_map(|(start, _)| tokens[start].ident().map(str::to_owned))
        .collect()
}

/// Framework of a script test file, from the test runner it imports; Jest otherwise.
fn script_framework(tokens: &[Token]) -> SourceFramework {
    let imports = |module: &str| tokens.iter().any(|token| token.literal() == Some(module));
    if imports("vitest") {
        SourceFramework::Vitest
    } else if imports("@playwright/test") {
        SourceFramework::Playwright
    } else if imports("mocha") || imports("chai") {
        SourceFramework::Mocha
    } else {
        SourceFramework::Jest
    }
}

/// Records script tests and a call from each test to every setup hook that runs before it.
pub(super) fn record_script_tests(
    tokens: &[Token],
    blocks: &[ScriptBlock],
    language: SourceLanguage,
    observations: &mut SourceObservationCollector<'_>,
) {
    let framework = script_framework(tokens);
    for test in blocks.iter().filter(|block| block.kind == BlockKind::Test) {
        let lines = SourceLineRange {
            start: tokens[test.start].line,
            end: tokens[test.body.0].end_line,
        };
        observations.push(confirmed_test(
            language,
            framework,
            test.name.clone(),
            lines.start,
            lines.end,
        ));
        for hook in blocks.iter().filter(|block| {
            block.kind == BlockKind::Hook
                && block.scope.0 <= test.start
                && test.start < block.scope.1
        }) {
            observations.push(call_observation(
                language,
                SymbolRef::Local(hook.name.clone()),
                Vec::new(),
                test.name.clone(),
                lines,
            ));
        }
    }
}

/// Records Go `TestX(t *testing.T)` functions.
pub(super) fn record_go_tests(
    scopes: &BraceScopes<'_>,
    observations: &mut SourceObservationCollector<'_>,
) {
    let tokens = scopes.tokens;
    for function in scopes.functions() {
        let signature = &tokens[function.start_token..function.body_start_token];
        let takes_testing = signature.windows(3).any(|window| {
            window[0].is_ident("testing") && window[1].is_punct('.') && window[2].is_ident("T")
        });
        let named = function.name.strip_prefix("Test").is_some_and(|rest| {
            rest.chars()
                .next()
                .is_none_or(|first| !first.is_lowercase())
        });
        if !named || !takes_testing {
            continue;
        }
        observations.push(confirmed_test(
            SourceLanguage::Go,
            SourceFramework::GoTest,
            function.name.clone(),
            tokens[function.start_token].line,
            tokens[function.body_start_token].end_line,
        ));
    }
}

/// Records Java methods annotated with `@Test` or another `JUnit` test annotation.
pub(super) fn record_java_tests(
    scopes: &BraceScopes<'_>,
    observations: &mut SourceObservationCollector<'_>,
) {
    let tokens = scopes.tokens;
    for function in scopes.functions() {
        let mut cursor = function.start_token;
        let mut annotated = false;
        while cursor > 1 && !annotated {
            cursor -= 1;
            let token = &tokens[cursor];
            if token.is_punct(')') {
                let Some(open) = matching_open(tokens, cursor) else {
                    break;
                };
                cursor = open;
                continue;
            }
            if matches!(token.kind, TokenKind::Punct(';' | '{' | '}')) {
                break;
            }
            annotated = tokens[cursor - 1].is_punct('@')
                && token
                    .ident()
                    .is_some_and(|name| JAVA_TEST_ANNOTATIONS.contains(&name));
        }
        if !annotated {
            continue;
        }
        observations.push(confirmed_test(
            SourceLanguage::Java,
            SourceFramework::JUnit,
            function.name.clone(),
            tokens[function.start_token].line,
            tokens[function.body_start_token].end_line,
        ));
    }
}

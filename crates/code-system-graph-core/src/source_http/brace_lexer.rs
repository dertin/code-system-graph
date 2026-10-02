//! Lexer for the brace languages recognized by client-flow analysis: JavaScript and TypeScript,
//! Go, and Java.
//!
//! Comments are dropped, string literals become literal tokens, JavaScript template literals keep
//! their raw body, and JavaScript regular expressions become opaque literals so that quotes inside
//! them cannot desynchronize the token stream. Single- and double-quoted strings end at a newline,
//! which confines the damage of stray apostrophes such as those in JSX text to one line.

use super::{Token, TokenKind};

/// Source language whose lexical rules apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BraceDialect {
    /// JavaScript and TypeScript.
    Script,
    /// Go.
    Go,
    /// Java.
    Java,
}

/// Keywords after which a JavaScript `/` starts a regular expression.
const REGEX_KEYWORDS: [&str; 13] = [
    "return", "typeof", "case", "do", "else", "in", "of", "new", "delete", "void", "throw",
    "yield", "await",
];

pub(super) fn lex_brace(source: &str, dialect: BraceDialect) -> Vec<Token> {
    let characters = source.chars().collect::<Vec<_>>();
    let mut tokens = Vec::<Token>::new();
    let mut index = 0;
    let mut line = 1_u32;
    let push = |tokens: &mut Vec<Token>, kind: TokenKind, start: u32, end: u32| {
        tokens.push(Token {
            kind,
            line: start,
            end_line: end,
        });
    };
    while index < characters.len() {
        let character = characters[index];
        let next = characters.get(index + 1).copied();
        match character {
            '\n' => {
                line += 1;
                index += 1;
            }
            character if character.is_whitespace() => index += 1,
            '/' if next == Some('/') => {
                while index < characters.len() && characters[index] != '\n' {
                    index += 1;
                }
            }
            '/' if next == Some('*') => {
                index += 2;
                while index < characters.len()
                    && !(characters[index] == '*' && characters.get(index + 1) == Some(&'/'))
                {
                    if characters[index] == '\n' {
                        line += 1;
                    }
                    index += 1;
                }
                index += 2;
            }
            '/' if dialect == BraceDialect::Script && regex_allowed(tokens.last()) => {
                index = skip_regex(&characters, index);
                push(&mut tokens, TokenKind::Literal(None), line, line);
            }
            '"' if dialect == BraceDialect::Java
                && next == Some('"')
                && characters.get(index + 2) == Some(&'"') =>
            {
                let start = line;
                let (end, value) = text_block(&characters, index + 3, &mut line);
                index = end;
                push(&mut tokens, TokenKind::Literal(value), start, line);
            }
            '"' | '\'' => {
                let (end, value) = quoted(&characters, index, character);
                index = end;
                push(&mut tokens, TokenKind::Literal(value), line, line);
            }
            '`' if dialect == BraceDialect::Script => {
                let start = line;
                let (end, body) = template(&characters, index + 1, &mut line);
                index = end;
                push(&mut tokens, TokenKind::Template(body), start, line);
            }
            '`' if dialect == BraceDialect::Go => {
                let start = line;
                let (end, value) = raw_string(&characters, index + 1, &mut line);
                index = end;
                push(&mut tokens, TokenKind::Literal(Some(value)), start, line);
            }
            character if character.is_ascii_digit() => {
                while index < characters.len()
                    && (characters[index].is_ascii_alphanumeric()
                        || matches!(characters[index], '.' | '_'))
                {
                    index += 1;
                }
                push(&mut tokens, TokenKind::Literal(None), line, line);
            }
            character if is_word_start(character, dialect) => {
                let start = index;
                index += 1;
                while index < characters.len() && is_word_continue(characters[index], dialect) {
                    index += 1;
                }
                let word = characters[start..index].iter().collect();
                push(&mut tokens, TokenKind::Ident(word), line, line);
            }
            character => {
                push(&mut tokens, TokenKind::Punct(character), line, line);
                index += 1;
            }
        }
    }
    tokens
}

fn is_word_start(character: char, dialect: BraceDialect) -> bool {
    character == '_'
        || character.is_alphabetic()
        || (character == '$' && dialect != BraceDialect::Go)
}

fn is_word_continue(character: char, dialect: BraceDialect) -> bool {
    character == '_'
        || character.is_alphanumeric()
        || (character == '$' && dialect != BraceDialect::Go)
}

fn regex_allowed(previous: Option<&Token>) -> bool {
    match previous.map(|token| &token.kind) {
        None => true,
        Some(TokenKind::Punct(character)) => !matches!(character, ')' | ']' | '}'),
        Some(TokenKind::Ident(word)) => REGEX_KEYWORDS.contains(&word.as_str()),
        Some(TokenKind::Literal(_) | TokenKind::Template(_)) => false,
    }
}

/// Skips a regular expression literal and its flags; an unterminated one ends at the newline.
fn skip_regex(characters: &[char], start: usize) -> usize {
    let mut index = start + 1;
    let mut class = false;
    while index < characters.len() && characters[index] != '\n' {
        match characters[index] {
            '\\' => index += 1,
            '[' => class = true,
            ']' => class = false,
            '/' if !class => {
                index += 1;
                while index < characters.len() && characters[index].is_ascii_alphabetic() {
                    index += 1;
                }
                return index;
            }
            _ => {}
        }
        index += 1;
    }
    index
}

/// Single-line quoted string; `None` when it is unterminated on its line.
fn quoted(characters: &[char], start: usize, quote: char) -> (usize, Option<String>) {
    let mut value = String::new();
    let mut index = start + 1;
    while index < characters.len() {
        match characters[index] {
            '\n' => return (index, None),
            '\\' => {
                if let Some(escaped) = characters.get(index + 1) {
                    value.push(match escaped {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        other => *other,
                    });
                }
                index += 2;
            }
            character if character == quote => return (index + 1, Some(value)),
            character => {
                value.push(character);
                index += 1;
            }
        }
    }
    (index, None)
}

/// Go raw string body up to the closing backtick.
fn raw_string(characters: &[char], start: usize, line: &mut u32) -> (usize, String) {
    let mut index = start;
    while index < characters.len() && characters[index] != '`' {
        if characters[index] == '\n' {
            *line += 1;
        }
        index += 1;
    }
    (index + 1, characters[start..index].iter().collect())
}

fn text_block(characters: &[char], start: usize, line: &mut u32) -> (usize, Option<String>) {
    let mut index = start;
    while index + 2 < characters.len() {
        if characters[index] == '"' && characters[index + 1] == '"' && characters[index + 2] == '"'
        {
            let value = characters[start..index].iter().collect::<String>();
            return (index + 3, Some(value.trim().to_owned()));
        }
        if characters[index] == '\n' {
            *line += 1;
        }
        index += 1;
    }
    (characters.len(), None)
}

/// Raw template body up to the closing backtick, skipping nested `${...}` expressions.
fn template(characters: &[char], start: usize, line: &mut u32) -> (usize, String) {
    let mut index = start;
    let mut depth = 0_u32;
    while index < characters.len() {
        match characters[index] {
            '\n' => *line += 1,
            '\\' => index += 1,
            '$' if depth == 0 && characters.get(index + 1) == Some(&'{') => {
                depth = 1;
                index += 1;
            }
            '{' if depth > 0 => depth += 1,
            '}' if depth > 0 => depth -= 1,
            '`' if depth == 0 => {
                return (index + 1, characters[start..index].iter().collect());
            }
            '`' => {
                let (end, _) = template(characters, index + 1, line);
                index = end;
                continue;
            }
            quote @ ('"' | '\'') if depth > 0 => {
                let (end, _) = quoted(characters, index, quote);
                index = end;
                continue;
            }
            _ => {}
        }
        index += 1;
    }
    (index, characters[start..].iter().collect())
}

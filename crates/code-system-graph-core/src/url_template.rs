//! Client URL templates assembled from literals, constants, format strings, and parameters.
//!
//! Extractors evaluate URL expressions into [`UrlTemplate`]s. A template yields an exact client
//! path when its runtime values are confined to the scheme, the authority, or whole path segments;
//! values in whole segments become path parameters. Templates that depend on parameters of the
//! enclosing function are instantiated at call sites by repository-level composition.

use std::collections::BTreeMap;

use crate::{UrlPart, UrlTemplate};

/// Placeholder for one runtime value while a template is flattened to text.
const HOLE: char = '\u{E000}';

/// Calls whose result is their single argument rendered as text.
pub(crate) const CONVERSIONS: [&str; 9] = [
    "String",
    "encodeURIComponent",
    "encodeURI",
    "Itoa",
    "FormatInt",
    "valueOf",
    "toString",
    "Sprint",
    "PathEscape",
];

impl UrlTemplate {
    /// Template of one literal.
    #[must_use]
    pub(crate) fn text(value: &str) -> Self {
        let mut template = Self::default();
        template.push_text(value);
        template
    }

    /// Template of one runtime value.
    #[must_use]
    pub(crate) fn part(part: UrlPart) -> Self {
        let mut template = Self::default();
        template.push(part);
        template
    }

    pub(crate) fn push_text(&mut self, value: &str) {
        if value.is_empty() {
            return;
        }
        if let Some(UrlPart::Text(last)) = self.parts.last_mut() {
            last.push_str(value);
        } else {
            self.parts.push(UrlPart::Text(value.to_owned()));
        }
    }

    pub(crate) fn push(&mut self, part: UrlPart) {
        match part {
            UrlPart::Text(value) => self.push_text(&value),
            part => self.parts.push(part),
        }
    }

    pub(crate) fn extend(&mut self, other: Self) {
        for part in other.parts {
            self.push(part);
        }
    }

    /// Literal value when the template has no runtime parts.
    #[must_use]
    pub(crate) fn as_literal(&self) -> Option<&str> {
        match self.parts.as_slice() {
            [] => Some(""),
            [UrlPart::Text(value)] => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub(crate) fn has_parameters(&self) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, UrlPart::Parameter { .. }))
    }

    /// Replaces parameters with the templates bound to their name or position.
    ///
    /// Unbound parameters become anonymous runtime values.
    #[must_use]
    pub(crate) fn bind(&self, arguments: &BoundArguments) -> Self {
        let mut bound = Self::default();
        for part in &self.parts {
            match part {
                UrlPart::Parameter { name, index } => {
                    match arguments
                        .by_name
                        .get(name)
                        .or_else(|| arguments.by_index.get(index))
                    {
                        Some(value) => bound.extend(value.clone()),
                        None => bound.push(UrlPart::Value(Some(name.clone()))),
                    }
                }
                part => bound.push(part.clone()),
            }
        }
        bound
    }

    /// Client literal with the same meaning for the client URL parser, or `None` when the path
    /// depends on runtime values that are not whole path segments.
    #[must_use]
    pub(crate) fn client_literal(&self) -> Option<String> {
        if let Some(literal) = self.as_literal() {
            return Some(literal.to_owned());
        }
        let mut text = String::new();
        let mut names = Vec::new();
        for part in &self.parts {
            match part {
                UrlPart::Text(value) => text.push_str(value),
                UrlPart::Parameter { name, .. } => {
                    text.push(HOLE);
                    names.push(Some(name.as_str()));
                }
                UrlPart::Value(name) => {
                    text.push(HOLE);
                    names.push(name.as_deref());
                }
            }
        }
        let (origin, path) = split_origin(&text)?;
        let path = path.split(['?', '#']).next().unwrap_or_default();
        if !path.is_empty() && !path.starts_with('/') {
            return None;
        }
        let mut holes = names.into_iter();
        let mut origin_holes = origin
            .chars()
            .filter(|character| *character == HOLE)
            .count();
        while origin_holes > 0 {
            holes.next();
            origin_holes -= 1;
        }
        let mut literal = if origin.contains(HOLE) {
            String::new()
        } else {
            origin.to_owned()
        };
        if path.is_empty() {
            literal.push('/');
        }
        for (position, segment) in path.split('/').enumerate() {
            if position > 0 {
                literal.push('/');
            }
            if !segment.contains(HOLE) {
                literal.push_str(segment);
                continue;
            }
            if segment.chars().count() != 1 {
                return None;
            }
            let name = holes.next().flatten().map_or("value", parameter_name);
            literal.push('{');
            literal.push_str(name);
            literal.push('}');
        }
        Some(literal)
    }
}

/// Arguments bound to a wrapper's parameters at one call site.
#[derive(Debug, Default)]
pub(crate) struct BoundArguments {
    pub(crate) by_name: BTreeMap<String, UrlTemplate>,
    pub(crate) by_index: BTreeMap<usize, UrlTemplate>,
}

/// Last identifier segment of a runtime value, used as a path parameter name.
fn parameter_name(name: &str) -> &str {
    let name = name.rsplit(['.', ':']).next().unwrap_or(name);
    if !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        name
    } else {
        "value"
    }
}

/// Splits flattened text into its scheme and authority and the path that follows.
///
/// A leading runtime value directly followed by `/` is a base URL and contributes no path. A
/// runtime value that follows a literal authority starts the path unless it continues a host
/// label, a port, or user information.
fn split_origin(text: &str) -> Option<(&str, &str)> {
    let after_scheme = text
        .find("://")
        .filter(|position| {
            text[..*position]
                .chars()
                .all(|character| character == HOLE || character.is_ascii_alphabetic())
        })
        .map(|position| position + 3)
        .or_else(|| text.starts_with("//").then_some(2));
    if let Some(start) = after_scheme {
        let mut end = text[start..]
            .find(['/', '?', '#'])
            .map_or(text.len(), |offset| start + offset);
        if let Some(hole) = text[start..end].find(HOLE).map(|offset| start + offset)
            && hole > start
            && !text[..hole].ends_with(['.', '-', ':', '@'])
        {
            end = hole;
        }
        if end == start {
            return None;
        }
        return Some((&text[..end], &text[end..]));
    }
    if text.starts_with('/') {
        return Some(("", text));
    }
    let mut characters = text.chars();
    (characters.next() == Some(HOLE) && characters.next() == Some('/'))
        .then(|| (&text[..HOLE.len_utf8()], &text[HOLE.len_utf8()..]))
}

/// Expands a Python `str.format` or Rust `format!` string.
///
/// `{}` takes the next positional argument, `{0}` a numbered one, and `{name}` a named one;
/// `resolve_inline` resolves names that are not passed explicitly, such as f-string expressions
/// and Rust inline arguments. `{{` and `}}` are literal braces.
pub(crate) fn brace_format(
    format: &str,
    positional: &[UrlTemplate],
    named: &BTreeMap<String, UrlTemplate>,
    resolve_inline: &dyn Fn(&str) -> UrlTemplate,
) -> UrlTemplate {
    let mut template = UrlTemplate::default();
    let mut next = 0_usize;
    let mut characters = format.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '{' if characters.peek() == Some(&'{') => {
                characters.next();
                template.push_text("{");
            }
            '}' if characters.peek() == Some(&'}') => {
                characters.next();
                template.push_text("}");
            }
            '{' => {
                let mut field = String::new();
                let mut depth = 1_u32;
                for inner in characters.by_ref() {
                    match inner {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    field.push(inner);
                }
                let name = field
                    .split(['!', ':', '='])
                    .next()
                    .unwrap_or_default()
                    .trim();
                let value = if name.is_empty() {
                    let value = positional.get(next).cloned();
                    next += 1;
                    value
                } else if let Ok(position) = name.parse::<usize>() {
                    positional.get(position).cloned()
                } else {
                    named
                        .get(name)
                        .cloned()
                        .or_else(|| Some(resolve_inline(name)))
                };
                template.extend(value.unwrap_or_else(|| UrlTemplate::part(UrlPart::Value(None))));
            }
            character => {
                let mut buffer = [0_u8; 4];
                template.push_text(character.encode_utf8(&mut buffer));
            }
        }
    }
    template
}

/// Expands a `printf`-style format string, as used by Go `fmt.Sprintf` and Java `String.format`.
pub(crate) fn printf_format(format: &str, arguments: &[UrlTemplate]) -> UrlTemplate {
    let mut template = UrlTemplate::default();
    let mut next = 0_usize;
    let mut characters = format.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '%' {
            let mut buffer = [0_u8; 4];
            template.push_text(character.encode_utf8(&mut buffer));
            continue;
        }
        if characters.peek() == Some(&'%') {
            characters.next();
            template.push_text("%");
            continue;
        }
        while characters
            .peek()
            .is_some_and(|flag| matches!(flag, '-' | '+' | '#' | ' ' | '0'..='9' | '.'))
        {
            characters.next();
        }
        if characters.next().is_none() {
            break;
        }
        let value = arguments
            .get(next)
            .cloned()
            .unwrap_or_else(|| UrlTemplate::part(UrlPart::Value(None)));
        next += 1;
        template.extend(value);
    }
    template
}

/// Evaluates a JavaScript, TypeScript, Go, or Java string expression written as source text.
///
/// Supported forms are string and template literals, `+` concatenation, parentheses, plain or
/// dotted identifiers, `fmt.Sprintf`, and `String.format`. Any other form yields `None`.
pub(crate) fn text_expression(
    expression: &str,
    resolve: &dyn Fn(&str) -> UrlTemplate,
) -> Option<UrlTemplate> {
    let mut template = UrlTemplate::default();
    for operand in split_top_level(expression.trim(), '+') {
        template.extend(text_operand(operand.trim(), resolve)?);
    }
    (!template.parts.is_empty()).then_some(template)
}

fn text_operand(operand: &str, resolve: &dyn Fn(&str) -> UrlTemplate) -> Option<UrlTemplate> {
    let first = operand.chars().next()?;
    if matches!(first, '"' | '\'') {
        let value = operand.strip_prefix(first)?.strip_suffix(first)?;
        return (!value.contains(first) || value.contains('\\')).then(|| UrlTemplate::text(value));
    }
    if first == '`' {
        return template_literal(operand.strip_prefix('`')?.strip_suffix('`')?, resolve);
    }
    if first == '(' && operand.ends_with(')') {
        return text_expression(&operand[1..operand.len() - 1], resolve);
    }
    for (call, printf) in [
        ("fmt.Sprintf(", true),
        ("String.format(", true),
        ("fmt.Sprint(", false),
    ] {
        if let Some(arguments) = operand
            .strip_prefix(call)
            .and_then(|rest| rest.strip_suffix(')'))
        {
            let arguments = split_top_level(arguments, ',');
            let values = arguments
                .iter()
                .map(|argument| {
                    text_expression(argument, resolve)
                        .unwrap_or_else(|| UrlTemplate::part(UrlPart::Value(None)))
                })
                .collect::<Vec<_>>();
            if !printf {
                let mut joined = UrlTemplate::default();
                for value in values {
                    joined.extend(value);
                }
                return Some(joined);
            }
            let format = values.first()?.as_literal()?.to_owned();
            return Some(printf_format(&format, &values[1..]));
        }
    }
    if let Some((callee, argument)) = operand
        .strip_suffix(')')
        .and_then(|call| call.split_once('('))
        && CONVERSIONS.contains(&callee.rsplit('.').next().unwrap_or(callee))
    {
        return text_expression(argument, resolve);
    }
    let identifier = operand
        .strip_suffix(".toString()")
        .or_else(|| operand.strip_suffix(".String()"))
        .unwrap_or(operand);
    let is_identifier = identifier.split('.').all(|segment| {
        !segment.is_empty()
            && segment.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '$')
            })
    });
    is_identifier.then(|| resolve(identifier))
}

pub(crate) fn template_literal(
    body: &str,
    resolve: &dyn Fn(&str) -> UrlTemplate,
) -> Option<UrlTemplate> {
    let mut template = UrlTemplate::default();
    let mut rest = body;
    while let Some(start) = rest.find("${") {
        template.push_text(&rest[..start]);
        let inner = &rest[start + 2..];
        let mut depth = 1_u32;
        let end = inner.char_indices().find_map(|(offset, character)| {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(offset);
                    }
                }
                _ => {}
            }
            None
        })?;
        template.extend(
            text_expression(&inner[..end], resolve)
                .unwrap_or_else(|| UrlTemplate::part(UrlPart::Value(None))),
        );
        rest = &inner[end + 1..];
    }
    template.push_text(rest);
    Some(template)
}

/// Splits `text` at `separator` outside quotes, template literals, and brackets.
pub(crate) fn split_top_level(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut quote = None::<char>;
    let mut escaped = false;
    let mut start = 0;
    for (index, character) in text.char_indices() {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == active {
                quote = None;
            }
            continue;
        }
        match character {
            '"' | '\'' | '`' => quote = Some(character),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            character if character == separator && depth == 0 => {
                parts.push(&text[start..index]);
                start = index + character.len_utf8();
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{BoundArguments, brace_format, printf_format, text_expression};
    use crate::{UrlPart, UrlTemplate};

    fn parameter(name: &str, index: usize) -> UrlPart {
        UrlPart::Parameter {
            name: name.to_owned(),
            index,
        }
    }

    fn template(parts: Vec<UrlPart>) -> UrlTemplate {
        let mut template = UrlTemplate::default();
        for part in parts {
            template.push(part);
        }
        template
    }

    #[test]
    fn runtime_values_in_origin_or_whole_segments_should_keep_exact_paths() {
        let cases = [
            (
                vec![
                    UrlPart::Value(Some("BASE_URL".to_owned())),
                    UrlPart::Text("/orders/".to_owned()),
                    UrlPart::Value(Some("order.id".to_owned())),
                ],
                Some("/orders/{id}"),
            ),
            (
                vec![
                    UrlPart::Text("http://".to_owned()),
                    UrlPart::Value(Some("host".to_owned())),
                    UrlPart::Text(":8080/v1/users?page=".to_owned()),
                    UrlPart::Value(None),
                ],
                Some("/v1/users"),
            ),
            (
                vec![
                    UrlPart::Text("https://api.test/items/".to_owned()),
                    parameter("item_id", 0),
                ],
                Some("https://api.test/items/{item_id}"),
            ),
            (
                vec![
                    UrlPart::Text("/files/".to_owned()),
                    parameter("name", 0),
                    UrlPart::Text(".json".to_owned()),
                ],
                None,
            ),
            (vec![parameter("url", 0)], None),
            (
                vec![UrlPart::Value(None), UrlPart::Text("orders".to_owned())],
                None,
            ),
            (
                vec![
                    UrlPart::Text("http://localhost:8080".to_owned()),
                    parameter("path", 0),
                ],
                None,
            ),
            (
                vec![
                    UrlPart::Text("https://api.".to_owned()),
                    UrlPart::Value(Some("domain".to_owned())),
                    UrlPart::Text("/v1/items".to_owned()),
                ],
                Some("/v1/items"),
            ),
        ];

        for (parts, expected) in cases {
            assert_eq!(template(parts).client_literal().as_deref(), expected);
        }
    }

    #[test]
    fn parameters_should_bind_by_name_or_position() {
        let wrapper = template(vec![
            UrlPart::Value(Some("BASE".to_owned())),
            parameter("path", 0),
        ]);
        let mut arguments = BoundArguments::default();
        arguments
            .by_index
            .insert(0, UrlTemplate::text("/api/orders/7"));

        assert_eq!(
            wrapper.bind(&arguments).client_literal().as_deref(),
            Some("/api/orders/7")
        );
        let absolute = template(vec![
            UrlPart::Text("http://localhost:8080".to_owned()),
            parameter("path", 0),
        ]);
        let mut named = BoundArguments::default();
        named
            .by_name
            .insert("path".to_owned(), UrlTemplate::text("/payments/1"));
        assert_eq!(
            absolute.bind(&named).client_literal().as_deref(),
            Some("http://localhost:8080/payments/1")
        );
    }

    #[test]
    fn format_strings_should_expand_into_templates() {
        let resolve = |name: &str| UrlTemplate::part(UrlPart::Value(Some(name.to_owned())));
        let base = UrlTemplate::text("http://orders:8080");
        let python = brace_format(
            "{}/orders/{order_id}/items/{{literal}}",
            std::slice::from_ref(&base),
            &BTreeMap::new(),
            &resolve,
        );
        let go = printf_format("%s/v1/users/%d", &[base, resolve("id")]);
        let script = text_expression("`${API}/users/${user.id}` + '/roles'", &resolve);

        assert_eq!(
            python.client_literal().as_deref(),
            Some("http://orders:8080/orders/{order_id}/items/{literal}")
        );
        assert_eq!(
            go.client_literal().as_deref(),
            Some("http://orders:8080/v1/users/{id}")
        );
        assert_eq!(
            script
                .and_then(|template| template.client_literal())
                .as_deref(),
            Some("/users/{id}/roles")
        );
    }
}

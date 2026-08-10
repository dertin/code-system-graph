//! Bounded Markdown presentation for agent-facing MCP delivery.

use serde::Serialize;
use serde_json::{Map, Value};

const TRUNCATION_NOTICE: &str = "\n## Truncation\n\nAdditional result content was omitted by the effective MCP response-byte limit.\n";

/// Renders one typed tool result as bounded UTF-8 Markdown without embedding a JSON envelope.
pub(crate) fn render_tool<T: Serialize>(tool: &str, value: &T, maximum: usize) -> String {
    let value = serde_json::to_value(value).unwrap_or_else(|error| {
        serde_json::json!({
            "status": "error",
            "warnings": [format!("result serialization failed: {error}")]
        })
    });
    let mut blocks = vec![format!("# {}\n", heading(tool))];
    if let Some(object) = value.as_object() {
        push_control_blocks(object, &mut blocks);
        if let Some(data) = object.get("data") {
            blocks.push("\n## Result\n".to_owned());
            render_value_blocks(None, data, 3, &mut blocks);
        }
        for (key, value) in prioritized_fields(
            object,
            &["schema_version", "status", "freshness", "warnings", "data"],
        ) {
            render_value_blocks(Some(key), value, 2, &mut blocks);
        }
    } else {
        render_value_blocks(Some("result"), &value, 2, &mut blocks);
    }
    fit_blocks(blocks, maximum)
}

/// Renders a JSON resource contract as Markdown. Schema catalogs retain fenced JSON entries.
pub(crate) fn render_resource_json(text: &str, schema_catalog: bool, maximum: usize) -> String {
    let blocks = match serde_json::from_str::<Value>(text) {
        Ok(value) if schema_catalog => {
            let mut blocks = vec!["# Schema Catalog\n".to_owned()];
            if let Some(object) = value.as_object() {
                for (name, schema) in object {
                    let encoded =
                        serde_json::to_string_pretty(schema).unwrap_or_else(|_| "null".to_owned());
                    blocks.push(format!(
                        "\n## {}\n\n```json\n{}\n```\n",
                        heading(name),
                        encoded
                    ));
                }
            } else {
                blocks.push(fenced("json", text));
            }
            blocks
        }
        Ok(value) => {
            let mut blocks = vec!["# Code System Graph Resource\n".to_owned()];
            render_value_blocks(None, &value, 2, &mut blocks);
            blocks
        }
        Err(_) => vec![
            "# Code System Graph Resource\n".to_owned(),
            fenced("text", text),
        ],
    };
    fit_blocks(blocks, maximum)
}

fn push_control_blocks(object: &Map<String, Value>, blocks: &mut Vec<String>) {
    let status = object
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let schema = object
        .get("schema_version")
        .and_then(Value::as_u64)
        .map_or_else(|| "unknown".to_owned(), |value| value.to_string());
    blocks.push(format!(
        "\n## Status\n\n- State: `{}`\n- Delivery schema: `{}`\n",
        inline(status),
        schema
    ));
    if let Some(freshness) = object.get("freshness") {
        blocks.push("\n## Freshness\n".to_owned());
        render_value_blocks(None, freshness, 3, blocks);
    }
    if let Some(warnings) = object.get("warnings") {
        blocks.push("\n## Warnings\n".to_owned());
        render_value_blocks(None, warnings, 3, blocks);
    }
}

fn prioritized_fields<'a>(
    object: &'a Map<String, Value>,
    excluded: &[&str],
) -> Vec<(&'a str, &'a Value)> {
    const PRIORITY: &[&str] = &[
        "repository",
        "locations",
        "evidence",
        "coverage",
        "truncations",
        "next_actions",
        "resolved_symbols",
        "local_relationships",
        "federated_handoffs",
        "source_markdown",
        "execution",
    ];
    let mut fields = object
        .iter()
        .filter(|(key, _)| !excluded.contains(&key.as_str()))
        .map(|(key, value)| (key.as_str(), value))
        .collect::<Vec<_>>();
    fields.sort_by_key(|(key, _)| {
        PRIORITY
            .iter()
            .position(|candidate| candidate == key)
            .unwrap_or(PRIORITY.len())
    });
    fields
}

fn render_value_blocks(name: Option<&str>, value: &Value, level: usize, blocks: &mut Vec<String>) {
    if let Some(name) = name {
        blocks.push(format!(
            "\n{} {}\n",
            "#".repeat(level.min(6)),
            heading(name)
        ));
    }
    match value {
        Value::Null => blocks.push("\n_None._\n".to_owned()),
        Value::Bool(value) => blocks.push(format!("\n`{value}`\n")),
        Value::Number(value) => blocks.push(format!("\n`{value}`\n")),
        Value::String(value) if value.contains('\n') || value.len() > 160 => {
            blocks.push(fenced("text", value));
        }
        Value::String(value) => blocks.push(format!("\n{}\n", inline(value))),
        Value::Array(values) if values.is_empty() => blocks.push("\n_None._\n".to_owned()),
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                match value {
                    Value::Object(_) | Value::Array(_) => {
                        blocks.push(format!(
                            "\n{} Item {}\n",
                            "#".repeat((level + 1).min(6)),
                            index + 1
                        ));
                        render_value_blocks(None, value, level + 2, blocks);
                    }
                    _ => blocks.push(format!("\n- {}\n", compact(value))),
                }
            }
        }
        Value::Object(object) => {
            for (key, value) in prioritized_fields(object, &[]) {
                render_value_blocks(Some(key), value, level, blocks);
            }
        }
    }
}

fn compact(value: &Value) -> String {
    match value {
        Value::Null => "_none_".to_owned(),
        Value::Bool(value) => format!("`{value}`"),
        Value::Number(value) => format!("`{value}`"),
        Value::String(value) => inline(value),
        _ => "_structured item_".to_owned(),
    }
}

fn fit_blocks(blocks: Vec<String>, maximum: usize) -> String {
    let mut output = String::new();
    let mut truncated = false;
    let reserve = TRUNCATION_NOTICE.len().min(maximum);
    for block in blocks {
        if output.len().saturating_add(block.len()) <= maximum.saturating_sub(reserve) {
            output.push_str(&block);
        } else {
            truncated = true;
        }
    }
    if truncated && output.len().saturating_add(TRUNCATION_NOTICE.len()) <= maximum {
        output.push_str(TRUNCATION_NOTICE);
    }
    if output.len() > maximum {
        let mut boundary = maximum;
        while boundary > 0 && !output.is_char_boundary(boundary) {
            boundary -= 1;
        }
        output.truncate(boundary);
    }
    output
}

fn fenced(language: &str, value: &str) -> String {
    let value = safe_multiline(value);
    let longest = value
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("\n{fence}{language}\n{value}\n{fence}\n")
}

fn heading(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '_' | '-' | '\n' | '\r' | '\t' | '\0' => ' ',
            character => safe_character(character),
        })
        .collect::<String>()
}

fn inline(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        let character = match character {
            '\n' | '\r' | '\t' | '\0' => ' ',
            '|' => '¦',
            '`' => 'ˋ',
            '<' => '‹',
            '>' => '›',
            '\\' | '[' | ']' | '(' | ')' | '*' | '_' | '#' | '!' => {
                output.push('\\');
                output.push(character);
                continue;
            }
            character => safe_character(character),
        };
        output.push(character);
    }
    output
}

fn safe_multiline(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\n' | '\t' => character,
            '\r' => '\n',
            character => safe_character(character),
        })
        .collect()
}

fn safe_character(character: char) -> char {
    if character.is_control()
        || matches!(
            character,
            '\u{061c}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{2069}'
                | '\u{feff}'
        )
    {
        '�'
    } else {
        character
    }
}

#[cfg(test)]
mod tests {
    use super::{render_resource_json, render_tool};

    #[test]
    fn tool_rendering_is_bounded_utf8_markdown_without_json_envelope() {
        let rendered = render_tool(
            "query",
            &serde_json::json!({
                "schema_version": 2,
                "status": "ok",
                "freshness": {"overall": "fresh"},
                "data": {
                    "path": "src/💡.rs",
                    "items": ["one", "[untrusted](https://example.test)\u{202e}"]
                },
                "warnings": []
            }),
            512,
        );
        assert!(rendered.len() <= 512);
        assert!(rendered.contains("# query"));
        assert!(!rendered.contains("\"schema_version\""));
        assert!(!rendered.contains('\u{202e}'));
        assert!(rendered.contains("\\[untrusted\\]\\(https://example.test\\)�"));
        assert!(std::str::from_utf8(rendered.as_bytes()).is_ok());
    }

    #[test]
    fn every_mcp_tool_should_match_the_markdown_delivery_golden() {
        let value = serde_json::json!({
            "schema_version": 2,
            "status": "ok",
            "freshness": {"overall": "fresh"},
            "data": {"message": "ready"},
            "warnings": []
        });
        for tool in [
            "trace",
            "query",
            "explore",
            "communities",
            "impact",
            "analyze_changes",
            "analyze_pull_request",
            "status",
            "contracts",
            "source_context",
            "scan",
            "update_workspace",
            "write_manual_link",
            "clean_cache",
            "recompute_communities",
        ] {
            let title = tool.replace('_', " ");
            let expected = format!(
                "# {title}\n\n## Status\n\n- State: `ok`\n- Delivery schema: `2`\n\n## Freshness\n\n### overall\n\nfresh\n\n## Warnings\n\n_None._\n\n## Result\n\n### message\n\nready\n"
            );

            assert_eq!(render_tool(tool, &value, 4_096), expected, "{tool}");
        }
    }

    #[test]
    fn schema_catalog_uses_closed_json_fences() {
        let rendered = render_resource_json(r#"{"query":{"type":"object"}}"#, true, 512);
        assert_eq!(rendered.matches("```json").count(), 1);
        assert_eq!(rendered.matches("\n```\n").count(), 1);
    }

    #[test]
    fn truncation_should_preserve_atomic_fences_and_utf8_boundaries() {
        let rendered = render_tool(
            "explore",
            &serde_json::json!({
                "schema_version": 2,
                "status": "degraded",
                "freshness": {"overall": "fresh"},
                "warnings": ["bounded"],
                "data": {
                    "coverage": {"gaps": ["neighbors"]},
                    "source_markdown": "```rust\nfn 💡() {}\n```\n".repeat(30)
                }
            }),
            320,
        );

        assert!(rendered.len() <= 320);
        assert!(std::str::from_utf8(rendered.as_bytes()).is_ok());
        assert_eq!(
            rendered.matches("```text").count(),
            rendered.matches("\n```\n").count()
        );
        assert!(rendered.contains("## Truncation"));
    }
}

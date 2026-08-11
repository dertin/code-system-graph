//! Bounded Markdown presentation for agent-facing MCP delivery.

use code_system_graph_model::{FreshnessSummary, ToolStatus};
use serde::Serialize;
use serde_json::{Map, Value};

const TRUNCATION_NOTICE: &str = "\n## Truncation\n\nAdditional result content was omitted by the effective MCP response-byte limit.\n";

/// Renders one typed tool envelope as bounded UTF-8 Markdown without embedding a JSON envelope.
pub(crate) fn render_envelope<T: Serialize>(
    tool: &str,
    schema_version: u32,
    status: ToolStatus,
    freshness: &FreshnessSummary,
    warnings: &[String],
    data: Option<&T>,
    maximum: usize,
) -> String {
    let mut blocks = vec![format!("# {}\n", heading(tool))];
    push_control_blocks(schema_version, status, freshness, warnings, &mut blocks);
    let mut rendered_data = None;
    if let Some(data) = data {
        match serde_json::to_value(data) {
            Ok(value) => {
                push_report_blocks(&value, &mut blocks);
                rendered_data = Some(value);
            }
            Err(error) => blocks.push(format!(
                "\nResult serialization failed: {}\n",
                inline(&error.to_string())
            )),
        }
    }
    let compact = compact_control_block(tool, status, freshness, warnings, rendered_data.as_ref());
    fit_blocks(blocks, maximum, Some(compact))
}

/// Renders a typed resource contract as bounded Markdown.
pub(crate) fn render_resource<T: Serialize>(resource: &T, maximum: usize) -> String {
    let mut blocks = vec!["# Code System Graph Resource\n".to_owned()];
    match serde_json::to_value(resource) {
        Ok(value) => render_value_blocks(None, &value, 2, &mut blocks),
        Err(error) => blocks.push(format!(
            "\nResource serialization failed: {}\n",
            inline(&error.to_string())
        )),
    }
    fit_blocks(blocks, maximum, None)
}

/// Renders the heterogeneous schema catalog with atomic fenced JSON entries.
pub(crate) fn render_schema_catalog(value: &Value, maximum: usize) -> String {
    if let Some(object) = value.as_object()
        && let Some(schemas) = object.get("schemas").and_then(Value::as_object)
    {
        return render_nested_schema_catalog(object, schemas, maximum);
    }
    let mut blocks = vec!["# Schema Catalog\n".to_owned()];
    if let Some(object) = value.as_object() {
        for (name, schema) in object {
            let encoded =
                serde_json::to_string_pretty(schema).unwrap_or_else(|_| "null".to_owned());
            blocks.push(format!(
                "\n## {}\n{}",
                heading(name),
                fenced("json", &encoded)
            ));
        }
    } else {
        let text = serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_owned());
        blocks.push(fenced("json", &text));
    }
    fit_blocks(blocks, maximum, None)
}

fn render_nested_schema_catalog(
    object: &Map<String, Value>,
    schemas: &Map<String, Value>,
    maximum: usize,
) -> String {
    let header = "# Schema Catalog\n".to_owned();
    let total = object
        .get("schema_total")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(schemas.len())
        .max(schemas.len());
    let source_truncated = object
        .get("schemas_truncated")
        .and_then(Value::as_bool)
        .unwrap_or(total > schemas.len());
    let version = object
        .get("schema_version")
        .map_or_else(|| "_none_".to_owned(), compact);
    let media_type = object
        .get("media_type")
        .map_or_else(|| "_none_".to_owned(), compact);
    let schema_blocks = schemas
        .iter()
        .map(|(name, schema)| schema_catalog_entry(name, schema))
        .collect::<Vec<_>>();
    let optional_blocks = object
        .iter()
        .filter(|(name, _)| {
            !matches!(
                name.as_str(),
                "schema_version"
                    | "media_type"
                    | "schemas"
                    | "schema_total"
                    | "schema_retained"
                    | "schemas_truncated"
            )
        })
        .map(|(name, value)| schema_catalog_entry(name, value))
        .collect::<Vec<_>>();
    let complete_preamble = schema_catalog_preamble(
        &version,
        &media_type,
        total,
        schemas.len(),
        source_truncated || total > schemas.len(),
    );
    let complete_length = std::iter::once(&header)
        .chain(std::iter::once(&complete_preamble))
        .chain(schema_blocks.iter())
        .chain(optional_blocks.iter())
        .fold(0_usize, |length, block| length.saturating_add(block.len()));
    if complete_length <= maximum {
        return std::iter::once(header)
            .chain(std::iter::once(complete_preamble))
            .chain(schema_blocks)
            .chain(optional_blocks)
            .collect();
    }

    let available = maximum.saturating_sub(TRUNCATION_NOTICE.len());
    let mut retained_blocks = Vec::new();
    let mut retained_bytes = 0_usize;
    for block in schema_blocks {
        let candidate_count = retained_blocks.len() + 1;
        let candidate_preamble = schema_catalog_preamble(
            &version,
            &media_type,
            total,
            candidate_count,
            source_truncated || total > candidate_count,
        );
        let candidate_length = header
            .len()
            .saturating_add(candidate_preamble.len())
            .saturating_add(retained_bytes)
            .saturating_add(block.len());
        if candidate_length <= available {
            retained_bytes = retained_bytes.saturating_add(block.len());
            retained_blocks.push(block);
        }
    }
    let retained = retained_blocks.len();
    let preamble = schema_catalog_preamble(
        &version,
        &media_type,
        total,
        retained,
        source_truncated || total > retained,
    );
    let mut output = header;
    output.push_str(&preamble);
    for block in retained_blocks {
        output.push_str(&block);
    }
    for block in optional_blocks {
        if output.len().saturating_add(block.len()) <= available {
            output.push_str(&block);
        }
    }
    if output.len().saturating_add(TRUNCATION_NOTICE.len()) <= maximum {
        output.push_str(TRUNCATION_NOTICE);
    }
    output
}

fn schema_catalog_preamble(
    version: &str,
    media_type: &str,
    total: usize,
    retained: usize,
    truncated: bool,
) -> String {
    format!(
        "\n## Catalog\n\n- Schema version: {version}\n- Media type: {media_type}\n- Schemas: total `{total}`, retained `{retained}`, truncated `{truncated}`\n"
    )
}

fn schema_catalog_entry(name: &str, value: &Value) -> String {
    let encoded = serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_owned());
    format!("\n## {}\n{}", heading(name), fenced("json", &encoded))
}

fn compact_control_block(
    tool: &str,
    status: ToolStatus,
    freshness: &FreshnessSummary,
    warnings: &[String],
    data: Option<&Value>,
) -> String {
    let status = match status {
        ToolStatus::Ok => "ok",
        ToolStatus::Degraded => "degraded",
        ToolStatus::Error => "error",
    };
    let freshness = serde_json::to_value(freshness.overall)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned());
    let warning = warnings.first().map_or_else(
        || "none".to_owned(),
        |warning| compact_text(&inline(warning), 18),
    );
    let coverage = data
        .and_then(|value| value.get("coverage"))
        .map_or("absent", |_| "present");
    let path = data.and_then(first_verifiable_path).map_or_else(
        || "absent".to_owned(),
        |path| compact_text(&inline(path), 18),
    );
    format!(
        "# {}\nstatus={status} freshness={freshness} truncated=true\nwarning={warning}\ncoverage={coverage} path={path}\n",
        compact_text(&heading(tool), 24),
    )
}

fn first_verifiable_path(value: &Value) -> Option<&str> {
    match value {
        Value::Object(object) => {
            for key in ["path", "file_path", "root"] {
                if let Some(path) = object.get(key).and_then(Value::as_str) {
                    return Some(path);
                }
            }
            object.values().find_map(first_verifiable_path)
        }
        Value::Array(values) => values.iter().find_map(first_verifiable_path),
        _ => None,
    }
}

fn compact_text(value: &str, maximum: usize) -> String {
    let value = value.replace(['\n', '\r', '\t'], " ");
    if value.len() <= maximum {
        return value;
    }
    let mut boundary = maximum;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value[..boundary].to_owned()
}

fn push_control_blocks(
    schema_version: u32,
    status: ToolStatus,
    freshness: &FreshnessSummary,
    warnings: &[String],
    blocks: &mut Vec<String>,
) {
    let status = match status {
        ToolStatus::Ok => "ok",
        ToolStatus::Degraded => "degraded",
        ToolStatus::Error => "error",
    };
    blocks.push(format!(
        "\n## Status\n\n- State: `{status}`\n- Delivery schema: `{schema_version}`\n"
    ));
    blocks.push("\n## Warnings\n".to_owned());
    let warnings = serde_json::to_value(warnings).unwrap_or(Value::Null);
    render_value_blocks(None, &warnings, 3, blocks);
    blocks.push("\n## Freshness\n".to_owned());
    let freshness = serde_json::to_value(freshness).unwrap_or(Value::Null);
    render_value_blocks(None, &freshness, 3, blocks);
}

fn push_report_blocks(value: &Value, blocks: &mut Vec<String>) {
    let Value::Object(object) = value else {
        blocks.push("\n## Result\n".to_owned());
        render_value_blocks(None, value, 3, blocks);
        return;
    };
    for (field, heading_name) in [("evidence", "Evidence"), ("coverage", "Coverage")] {
        if let Some(value) = object.get(field) {
            blocks.push(format!("\n## {heading_name}\n"));
            render_value_blocks(None, value, 3, blocks);
        }
    }
    blocks.push("\n## Result\n".to_owned());
    for (name, value) in prioritized_fields(object, &["evidence", "coverage", "source_markdown"]) {
        render_value_blocks(Some(name), value, 3, blocks);
    }
    if let Some(source) = object.get("source_markdown") {
        blocks.push("\n## Source\n".to_owned());
        render_value_blocks(None, source, 3, blocks);
    }
}

fn prioritized_fields<'a>(
    object: &'a Map<String, Value>,
    excluded: &[&str],
) -> Vec<(&'a str, &'a Value)> {
    const PRIORITY: &[&str] = &[
        "repository",
        "locations",
        "truncations",
        "next_actions",
        "resolved_symbols",
        "local_relationships",
        "federated_handoffs",
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

fn fit_blocks(blocks: Vec<String>, maximum: usize, compact: Option<String>) -> String {
    let complete_length = blocks
        .iter()
        .fold(0_usize, |length, block| length.saturating_add(block.len()));
    if complete_length <= maximum {
        return blocks.concat();
    }
    let reserve = TRUNCATION_NOTICE.len().min(maximum);
    let mut output = compact
        .filter(|block| block.len() <= maximum.saturating_sub(reserve))
        .unwrap_or_default();
    for block in blocks {
        if output.len().saturating_add(block.len()) <= maximum.saturating_sub(reserve) {
            output.push_str(&block);
        }
    }
    if output.len().saturating_add(TRUNCATION_NOTICE.len()) <= maximum {
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
    use code_system_graph_model::{FreshnessSummary, OverallFreshness, ToolStatus};

    use super::{render_envelope, render_resource, render_schema_catalog};

    fn freshness() -> FreshnessSummary {
        FreshnessSummary {
            overall: OverallFreshness::Fresh,
            stale_repositories: Vec::new(),
            reasons: Vec::new(),
        }
    }

    #[test]
    fn tool_rendering_is_bounded_utf8_markdown_without_json_envelope() {
        let rendered = render_envelope(
            "query",
            2,
            ToolStatus::Ok,
            &freshness(),
            &[],
            Some(&serde_json::json!({
                "path": "src/💡.rs",
                "items": ["one", "[untrusted](https://example.test)\u{202e}"]
            })),
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
    fn every_mcp_tool_should_render_its_report_specific_fixture() {
        for (tool, value, expected_field) in [
            (
                "trace",
                serde_json::json!({"segments": ["a -> b"]}),
                "segments",
            ),
            (
                "query",
                serde_json::json!({"hits": [{"id": "node:a"}]}),
                "hits",
            ),
            (
                "explore",
                serde_json::json!({"resolved_symbols": ["a"]}),
                "resolved symbols",
            ),
            (
                "communities",
                serde_json::json!({"communities": ["one"]}),
                "communities",
            ),
            ("impact", serde_json::json!({"risk": "high"}), "risk"),
            (
                "analyze_changes",
                serde_json::json!({"changed_files": ["a.rs"]}),
                "changed files",
            ),
            (
                "analyze_pull_request",
                serde_json::json!({"pull_request": 42}),
                "pull request",
            ),
            (
                "status",
                serde_json::json!({"integrity_ok": true}),
                "integrity ok",
            ),
            (
                "contracts",
                serde_json::json!({"contracts": ["api"]}),
                "contracts",
            ),
            (
                "source_context",
                serde_json::json!({"evidence": ["ev:1"]}),
                "Evidence",
            ),
            (
                "scan",
                serde_json::json!({"snapshot_id": "snapshot:1"}),
                "snapshot id",
            ),
            (
                "update_workspace",
                serde_json::json!({"mutation": "add"}),
                "mutation",
            ),
            (
                "write_manual_link",
                serde_json::json!({"manual_link": "edge:1"}),
                "manual link",
            ),
            (
                "clean_cache",
                serde_json::json!({"removed_entries": 3}),
                "removed entries",
            ),
            (
                "recompute_communities",
                serde_json::json!({"community_count": 4}),
                "community count",
            ),
        ] {
            let rendered = render_envelope(
                tool,
                2,
                ToolStatus::Ok,
                &freshness(),
                &[],
                Some(&value),
                4_096,
            );
            assert!(rendered.starts_with(&format!("# {}", tool.replace('_', " "))));
            assert!(rendered.contains(expected_field), "{tool}: {rendered}");
        }
    }

    #[test]
    fn schema_catalog_uses_closed_json_fences() {
        let rendered =
            render_schema_catalog(&serde_json::json!({"query": {"type": "object"}}), 512);
        assert_eq!(rendered.matches("```json").count(), 1);
        assert_eq!(rendered.matches("\n```\n").count(), 1);
    }

    #[test]
    fn bounded_nested_catalog_should_retain_individual_schemas_with_exact_coverage() {
        let rendered = render_schema_catalog(
            &serde_json::json!({
                "schema_version": 2,
                "media_type": "application/schema+json",
                "schema_total": 2,
                "schema_retained": 2,
                "schemas_truncated": false,
                "schemas": {
                    "alpha": {"type": "string"},
                    "beta": {"description": "x".repeat(1_024)}
                }
            }),
            512,
        );

        assert!(rendered.contains("## alpha"), "{rendered}");
        assert!(!rendered.contains("## beta"), "{rendered}");
        assert!(rendered.contains("retained `1`"), "{rendered}");
        assert!(rendered.contains("truncated `true`"), "{rendered}");
        assert_eq!(rendered.matches("```json").count(), 1);
        assert_eq!(rendered.matches("\n```\n").count(), 1);
        assert!(rendered.contains("## Truncation"));
    }

    #[test]
    fn every_resource_shape_should_render_as_markdown() {
        for (name, resource, expected) in [
            (
                "workspaces",
                serde_json::json!({"workspaces": ["commerce"]}),
                "workspaces",
            ),
            (
                "overview",
                serde_json::json!({"snapshot": {"id": "one"}}),
                "snapshot",
            ),
            (
                "status",
                serde_json::json!({"status": {"integrity_ok": true}}),
                "integrity ok",
            ),
            (
                "repositories",
                serde_json::json!({"repositories": ["api"]}),
                "repositories",
            ),
            (
                "services",
                serde_json::json!({"entities": ["service"]}),
                "entities",
            ),
            (
                "contracts",
                serde_json::json!({"entities": ["contract"]}),
                "entities",
            ),
            (
                "communities",
                serde_json::json!({"communities": ["one"]}),
                "communities",
            ),
            (
                "coverage",
                serde_json::json!({"runs": ["extractor"]}),
                "runs",
            ),
            (
                "evidence",
                serde_json::json!({"evidence": {"id": "ev:1"}}),
                "evidence",
            ),
        ] {
            let rendered = render_resource(&resource, 4_096);
            assert!(
                rendered.starts_with("# Code System Graph Resource"),
                "{name}"
            );
            assert!(rendered.contains(expected), "{name}: {rendered}");
        }
    }

    #[test]
    fn truncation_should_preserve_atomic_fences_and_utf8_boundaries() {
        let rendered = render_envelope(
            "explore",
            2,
            ToolStatus::Degraded,
            &freshness(),
            &["bounded".to_owned()],
            Some(&serde_json::json!({
                "coverage": {"gaps": ["neighbors"]},
                "source_markdown": "```rust\nfn 💡() {}\n```\n".repeat(30)
            })),
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

    #[test]
    fn exact_response_budget_should_not_trigger_truncation() {
        let value = serde_json::json!({"message": "ready"});
        let complete = render_envelope(
            "query",
            2,
            ToolStatus::Ok,
            &freshness(),
            &[],
            Some(&value),
            4_096,
        );
        let exact = render_envelope(
            "query",
            2,
            ToolStatus::Ok,
            &freshness(),
            &[],
            Some(&value),
            complete.len(),
        );

        assert_eq!(exact, complete);
        assert!(!exact.contains("## Truncation"));
    }

    #[test]
    fn minimum_response_budget_should_keep_control_and_truncation_blocks() {
        let minimum = usize::try_from(code_system_graph_core::MIN_MCP_MARKDOWN_BYTES)
            .expect("MCP minimum is usize-representable");
        let rendered = render_envelope(
            "query",
            2,
            ToolStatus::Degraded,
            &FreshnessSummary {
                overall: OverallFreshness::Stale,
                stale_repositories: vec![code_system_graph_model::RepoId::new("repo:api")],
                reasons: vec!["snapshot is stale".to_owned()],
            },
            &["provider timed out".to_owned()],
            Some(&serde_json::json!({
                "coverage": {"gaps": ["neighbors unavailable"]},
                "locations": [{"path": "src/api.rs"}],
                "hits": ["x".repeat(512)]
            })),
            minimum,
        );

        assert_ne!(rendered, "");
        assert!(rendered.len() <= minimum);
        assert!(rendered.contains("status=degraded"));
        assert!(rendered.contains("freshness=stale"));
        assert!(rendered.contains("provider timed out"));
        assert!(rendered.contains("coverage=present"));
        assert!(rendered.contains("path=src/api.rs"));
        assert!(rendered.contains("## Truncation"));
    }
}

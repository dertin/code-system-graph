//! Typed, bounded Markdown construction for agent-facing MCP delivery.

use std::fmt::{Debug, Display};

use code_system_graph_model::{FreshnessSummary, ToolStatus};
use serde_json::{Map, Value};

const TRUNCATION_NOTICE: &str = "\n## Truncation\n\nAdditional result content was omitted by the effective MCP response-byte limit.\n";

enum DocumentBlock {
    Control(String),
    Atomic(String),
    Collection {
        heading: String,
        total: usize,
        source_truncated: bool,
        items: Vec<String>,
    },
    Source {
        heading: String,
        value: String,
    },
}

impl DocumentBlock {
    fn complete(&self) -> String {
        match self {
            Self::Control(value) | Self::Atomic(value) => value.clone(),
            Self::Collection {
                heading,
                total,
                source_truncated,
                items,
            } => collection_block(heading, *total, items.len(), *source_truncated, items),
            Self::Source {
                heading: source_heading,
                value,
            } => {
                format!(
                    "\n## {}\n{}",
                    heading(source_heading),
                    fenced("text", value)
                )
            }
        }
    }
}

/// Internal typed Markdown document. Callers select concrete report fields before adding them;
/// this builder never reflects over serialized report objects.
pub(crate) struct MarkdownDocument {
    blocks: Vec<DocumentBlock>,
    compact: String,
}

impl MarkdownDocument {
    pub(crate) fn fragment() -> Self {
        Self {
            blocks: Vec::new(),
            compact: String::new(),
        }
    }

    pub(crate) fn tool(
        name: &str,
        schema_version: u32,
        status: ToolStatus,
        freshness: &FreshnessSummary,
        warnings: &[String],
        coverage_present: bool,
        path: Option<&str>,
    ) -> Self {
        let status_text = tool_status(status);
        let freshness_text = format!("{:?}", freshness.overall).to_lowercase();
        let warning = warnings.first().map_or_else(
            || "none".to_owned(),
            |value| compact_text(&inline(value), 18),
        );
        let path = path.map_or_else(
            || "absent".to_owned(),
            |value| compact_text(&inline(value), 18),
        );
        let compact = format!(
            "# {}\nstatus={status_text} freshness={freshness_text} truncated=true\nwarning={warning}\ncoverage={} path={path}\n",
            compact_text(&heading(name), 24),
            if coverage_present {
                "present"
            } else {
                "absent"
            },
        );
        let mut document = Self {
            blocks: vec![DocumentBlock::Control(format!("# {}\n", heading(name)))],
            compact,
        };
        document.blocks.push(DocumentBlock::Control(format!(
            "\n## Status\n\n- State: `{status_text}`\n- Delivery schema: `{schema_version}`\n"
        )));
        document.control_string_collection("Warnings", warnings);
        document.blocks.push(DocumentBlock::Control(format!(
            "\n## Freshness\n\n- Overall: `{freshness_text}`\n"
        )));
        document.control_debug_collection("Stale repositories", &freshness.stale_repositories);
        document.control_string_collection("Freshness reasons", &freshness.reasons);
        document
    }

    pub(crate) fn resource(name: &str, schema_version: u32) -> Self {
        Self {
            blocks: vec![
                DocumentBlock::Control(format!("# {}\n", heading(name))),
                DocumentBlock::Control(format!(
                    "\n## Contract\n\n- Delivery schema: `{schema_version}`\n"
                )),
            ],
            compact: format!(
                "# {}\nschema={schema_version} truncated=true\n",
                compact_text(&heading(name), 32)
            ),
        }
    }

    pub(crate) fn scalar(&mut self, name: &str, value: impl Display) {
        self.blocks.push(DocumentBlock::Atomic(format!(
            "\n## {}\n\n`{}`\n",
            heading(name),
            inline(&value.to_string())
        )));
    }

    pub(crate) fn text(&mut self, name: &str, value: &str) {
        self.blocks.push(DocumentBlock::Atomic(format!(
            "\n## {}\n\n{}\n",
            heading(name),
            inline(value)
        )));
    }

    pub(crate) fn debug(&mut self, name: &str, value: &impl Debug) {
        self.text(name, &format!("{value:?}"));
    }

    pub(crate) fn debug_collection<T: Debug>(&mut self, name: &str, items: &[T]) {
        self.bounded_fragments(
            name,
            items.len(),
            false,
            items
                .iter()
                .map(|item| format!("\n- {}\n", inline(&format!("{item:?}"))))
                .collect(),
        );
    }

    pub(crate) fn string_collection(&mut self, name: &str, items: &[String]) {
        self.bounded_fragments(
            name,
            items.len(),
            false,
            items
                .iter()
                .map(|item| format!("\n- {}\n", inline(item)))
                .collect(),
        );
    }

    fn control_debug_collection<T: Debug>(&mut self, name: &str, items: &[T]) {
        self.control_collection_metadata(name, items.len(), items.len(), false);
        for item in items {
            self.blocks.push(DocumentBlock::Control(format!(
                "\n- {}\n",
                inline(&format!("{item:?}"))
            )));
        }
    }

    fn control_string_collection(&mut self, name: &str, items: &[String]) {
        self.control_collection_metadata(name, items.len(), items.len(), false);
        for item in items {
            self.blocks
                .push(DocumentBlock::Control(format!("\n- {}\n", inline(item))));
        }
    }

    pub(crate) fn bounded_collection<T: Debug>(
        &mut self,
        name: &str,
        total: usize,
        retained: usize,
        truncated: bool,
        items: &[T],
    ) {
        debug_assert_eq!(retained, items.len());
        self.bounded_fragments(
            name,
            total,
            truncated,
            items
                .iter()
                .map(|item| format!("\n- {}\n", inline(&format!("{item:?}"))))
                .collect(),
        );
    }

    pub(crate) fn bounded_fragments(
        &mut self,
        name: &str,
        total: usize,
        source_truncated: bool,
        items: Vec<String>,
    ) {
        self.blocks.push(DocumentBlock::Collection {
            heading: name.to_owned(),
            total,
            source_truncated,
            items,
        });
    }

    fn control_collection_metadata(
        &mut self,
        name: &str,
        total: usize,
        retained: usize,
        truncated: bool,
    ) {
        self.blocks.push(DocumentBlock::Control(format!(
            "\n## {}\n\n- Total: `{total}`\n- Retained: `{retained}`\n- Truncated: `{truncated}`\n",
            heading(name)
        )));
    }

    pub(crate) fn source(&mut self, name: &str, value: &str) {
        self.blocks.push(DocumentBlock::Source {
            heading: name.to_owned(),
            value: safe_multiline(value),
        });
    }

    pub(crate) fn render(self, maximum: usize) -> String {
        fit_document(self.blocks, maximum, &self.compact)
    }

    pub(crate) fn into_complete(self) -> String {
        self.blocks.iter().map(DocumentBlock::complete).collect()
    }
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
    fit_atomic_blocks(blocks, maximum)
}

fn fit_document(blocks: Vec<DocumentBlock>, maximum: usize, compact: &str) -> String {
    let complete = blocks
        .iter()
        .map(DocumentBlock::complete)
        .collect::<String>();
    if complete.len() <= maximum {
        return complete;
    }
    let available = maximum.saturating_sub(TRUNCATION_NOTICE.len());
    let mut output = if compact.len() <= available {
        compact.to_owned()
    } else {
        String::new()
    };
    for block in blocks {
        match block {
            DocumentBlock::Control(_) => {}
            DocumentBlock::Atomic(value) => {
                if output.len().saturating_add(value.len()) <= available {
                    output.push_str(&value);
                }
            }
            DocumentBlock::Collection {
                heading,
                total,
                source_truncated,
                items,
            } => {
                let remaining = available.saturating_sub(output.len());
                if let Some(value) =
                    bounded_collection_block(&heading, total, source_truncated, &items, remaining)
                {
                    output.push_str(&value);
                }
            }
            DocumentBlock::Source { heading, value } => {
                let remaining = available.saturating_sub(output.len());
                if let Some(value) = bounded_source_block(&heading, &value, remaining) {
                    output.push_str(&value);
                }
            }
        }
    }
    if output.len().saturating_add(TRUNCATION_NOTICE.len()) <= maximum {
        output.push_str(TRUNCATION_NOTICE);
    }
    output
}

fn collection_block(
    name: &str,
    total: usize,
    retained: usize,
    truncated: bool,
    items: &[String],
) -> String {
    format!(
        "\n## {}\n\n- Total: `{total}`\n- Retained: `{retained}`\n- Truncated: `{truncated}`\n{}",
        heading(name),
        items.concat()
    )
}

fn bounded_collection_block(
    name: &str,
    total: usize,
    source_truncated: bool,
    items: &[String],
    maximum: usize,
) -> Option<String> {
    let mut retained = 0_usize;
    let mut item_bytes = 0_usize;
    for item in items {
        let candidate_retained = retained + 1;
        let metadata = collection_block(
            name,
            total,
            candidate_retained,
            source_truncated || candidate_retained < total,
            &[],
        );
        if metadata
            .len()
            .saturating_add(item_bytes)
            .saturating_add(item.len())
            > maximum
        {
            break;
        }
        retained = candidate_retained;
        item_bytes = item_bytes.saturating_add(item.len());
    }
    let mut output = collection_block(
        name,
        total,
        retained,
        source_truncated || retained < total,
        &[],
    );
    if output.len().saturating_add(item_bytes) > maximum {
        return None;
    }
    for item in items.iter().take(retained) {
        output.push_str(item);
    }
    Some(output)
}

fn fit_atomic_blocks(blocks: Vec<String>, maximum: usize) -> String {
    let complete = blocks.concat();
    if complete.len() <= maximum {
        return complete;
    }
    let available = maximum.saturating_sub(TRUNCATION_NOTICE.len());
    let mut output = String::new();
    for block in blocks {
        if output.len().saturating_add(block.len()) <= available {
            output.push_str(&block);
        }
    }
    if output.len().saturating_add(TRUNCATION_NOTICE.len()) <= maximum {
        output.push_str(TRUNCATION_NOTICE);
    }
    output
}

fn bounded_source_block(name: &str, value: &str, maximum: usize) -> Option<String> {
    let heading = format!("\n## {}\n\n", heading(name));
    let minimum = heading.len().saturating_add("```text\n\n```\n".len());
    if minimum > maximum {
        return None;
    }

    let mut boundary = 0_usize;
    let mut current_backticks = 0_usize;
    let mut longest_backticks = 0_usize;
    for (offset, character) in value.char_indices() {
        current_backticks = if character == '`' {
            current_backticks + 1
        } else {
            0
        };
        longest_backticks = longest_backticks.max(current_backticks);
        let next_boundary = offset + character.len_utf8();
        let fence_bytes = longest_backticks.max(2) + 1;
        let candidate_bytes = heading
            .len()
            .saturating_add(next_boundary)
            .saturating_add(fence_bytes.saturating_mul(2))
            .saturating_add("text\n\n\n".len());
        if candidate_bytes > maximum {
            break;
        }
        boundary = next_boundary;
    }

    let retained = &value[..boundary];
    let fence = fence_marker(retained);
    Some(format!("{heading}{fence}text\n{retained}\n{fence}\n"))
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
        .map_or_else(|| "_none_".to_owned(), compact_json);
    let media_type = object
        .get("media_type")
        .map_or_else(|| "_none_".to_owned(), compact_json);
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

fn compact_json(value: &Value) -> String {
    match value {
        Value::Null => "_none_".to_owned(),
        Value::Bool(value) => format!("`{value}`"),
        Value::Number(value) => format!("`{value}`"),
        Value::String(value) => inline(value),
        _ => "_structured item_".to_owned(),
    }
}

fn tool_status(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Ok => "ok",
        ToolStatus::Degraded => "degraded",
        ToolStatus::Error => "error",
    }
}

fn compact_text(value: &str, maximum: usize) -> String {
    let value = value.replace(['\n', '\r', '\t'], " ");
    let boundary = utf8_boundary_at_or_before(&value, maximum);
    value[..boundary].to_owned()
}

fn utf8_boundary_at_or_before(value: &str, maximum: usize) -> usize {
    let mut boundary = maximum.min(value.len());
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    boundary
}

fn fenced(language: &str, value: &str) -> String {
    let value = safe_multiline(value);
    let fence = fence_marker(&value);
    format!("\n{fence}{language}\n{value}\n{fence}\n")
}

fn fence_marker(value: &str) -> String {
    let longest = value
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    "`".repeat(longest.max(2) + 1)
}

fn heading(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '_' | '-' | '\n' | '\r' | '\t' | '\0' => ' ',
            character => safe_character(character),
        })
        .collect()
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

    use super::{MarkdownDocument, render_schema_catalog};

    fn freshness() -> FreshnessSummary {
        FreshnessSummary {
            overall: OverallFreshness::Fresh,
            stale_repositories: Vec::new(),
            reasons: Vec::new(),
        }
    }

    #[test]
    fn complete_document_that_exactly_fits_should_not_claim_truncation() {
        let document = MarkdownDocument::tool(
            "query",
            2,
            ToolStatus::Ok,
            &freshness(),
            &[],
            true,
            Some("src/lib.rs"),
        );
        let complete = document.render(4_096);
        let exact = complete.len();
        let document = MarkdownDocument::tool(
            "query",
            2,
            ToolStatus::Ok,
            &freshness(),
            &[],
            true,
            Some("src/lib.rs"),
        );
        assert_eq!(document.render(exact), complete);
        assert!(!complete.contains("## Truncation"));
    }

    #[test]
    fn minimum_budget_should_keep_mandatory_tool_control() {
        let minimum =
            usize::try_from(code_system_graph_core::MIN_MCP_MARKDOWN_BYTES).expect("MCP minimum");
        let rendered = MarkdownDocument::tool(
            "query",
            2,
            ToolStatus::Degraded,
            &freshness(),
            &["provider timed out".to_owned()],
            true,
            Some("src/💡.rs"),
        )
        .render(minimum);
        assert!(rendered.len() <= minimum);
        for required in [
            "status=degraded",
            "freshness=fresh",
            "warning=provider timed out",
            "coverage=present",
            "path=src/💡.rs",
            "## Truncation",
        ] {
            assert!(rendered.contains(required), "{rendered}");
        }
    }

    #[test]
    fn oversized_source_should_keep_utf8_prefix_and_close_fence() {
        let mut document = MarkdownDocument::tool(
            "explore",
            2,
            ToolStatus::Degraded,
            &freshness(),
            &[],
            true,
            Some("src/lib.rs"),
        );
        document.source("Source", &"💡 source line\n".repeat(128));
        let rendered = document.render(512);
        assert!(rendered.contains("💡 source"), "{rendered}");
        assert_eq!(rendered.matches("```text").count(), 1);
        assert_eq!(rendered.matches("\n```\n").count(), 1);
        assert!(rendered.contains("## Truncation"));
        assert!(std::str::from_utf8(rendered.as_bytes()).is_ok());
    }

    #[test]
    fn source_fitting_should_ignore_backticks_outside_the_retained_prefix() {
        let mut document = MarkdownDocument::tool(
            "explore",
            2,
            ToolStatus::Degraded,
            &freshness(),
            &[],
            true,
            Some("src/lib.rs"),
        );
        document.source("Source", &format!("useful source\n{}", "`".repeat(2_048)));
        let rendered = document.render(512);

        assert!(rendered.contains("useful source"), "{rendered}");
        assert!(rendered.contains("## Source"), "{rendered}");
        assert!(rendered.contains("## Truncation"), "{rendered}");
    }

    #[test]
    fn byte_fitting_should_recompute_delivered_collection_metadata() {
        let mut document = MarkdownDocument::resource("bounded", 2);
        document.bounded_collection("Items", 2, 2, false, &["a".repeat(128), "b".repeat(128)]);
        let rendered = document.render(256);

        assert!(rendered.contains("- Total: `2`"), "{rendered}");
        assert!(rendered.contains("- Retained: `0`"), "{rendered}");
        assert!(rendered.contains("- Truncated: `true`"), "{rendered}");
        assert!(!rendered.contains(&"a".repeat(32)), "{rendered}");
    }

    #[test]
    fn large_collection_fitting_should_remain_linear_and_exact() {
        let items = (0..10_000)
            .map(|index| format!("item-{index}"))
            .collect::<Vec<_>>();
        let mut document = MarkdownDocument::resource("bounded", 2);
        document.bounded_collection("Items", items.len(), items.len(), false, &items);
        let rendered = document.render(1_048_576);

        assert!(rendered.contains("- Retained: `10000`"), "{rendered}");
        assert!(rendered.contains("- Truncated: `false`"), "{rendered}");
        assert!(rendered.contains("item-9999"), "{rendered}");
    }

    #[test]
    fn bounded_nested_catalog_should_retain_individual_schemas() {
        let rendered = render_schema_catalog(
            &serde_json::json!({
                "schema_version": 2,
                "media_type": "application/schema+json",
                "schema_total": 2,
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
    }
}

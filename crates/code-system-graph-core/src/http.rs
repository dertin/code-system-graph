use code_system_graph_model::{
    Evidence, EvidenceId, Node, NodeId, NodeKind, Provenance, RepoId, stable_id
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ExtractionBudgets, ExtractionLimitExceeded, ExtractionTracker, HttpConsumerConfig};

const HTTP_METHODS: [&str; 8] = [
    "DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT", "TRACE",
];

/// Direction of an extracted HTTP operation at a repository boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryRole {
    /// The repository serves the operation.
    Provider,
    /// The repository invokes the operation.
    Consumer,
}

/// HTTP operation plus the evidence that established it.
#[derive(Debug, Clone, PartialEq)]
pub struct HttpBoundary {
    /// Federated graph node for this repository-local boundary.
    pub node: Node,
    /// Canonical upper-case HTTP method.
    pub method: String,
    /// Canonical path template.
    pub path: String,
    /// Consumer or provider direction.
    pub role: BoundaryRole,
    /// Direct evidence for the boundary.
    pub evidence: Evidence,
}

impl HttpBoundary {
    /// Creates a declared HTTP consumer from strict manifest configuration.
    #[must_use]
    pub fn consumer(repo_id: RepoId, config: &HttpConsumerConfig) -> Self {
        let method = config.method.trim().to_ascii_uppercase();
        let path = normalize_http_path(&config.path);
        boundary(
            repo_id,
            &method,
            &path,
            BoundaryRole::Consumer,
            &config.source,
            1.0,
        )
    }
}

/// Error returned while extracting an `OpenAPI` document.
#[derive(Debug, Error)]
pub enum HttpExtractionError {
    /// YAML or JSON syntax is invalid.
    #[error("invalid OpenAPI document: parser rejected malformed input")]
    InvalidDocument,
    /// Root document is not an object.
    #[error("invalid OpenAPI document: root must be an object")]
    InvalidRoot,
    /// Required contract version is absent or unsupported.
    #[error("unsupported HTTP contract version; expected OpenAPI 3.x or Swagger 2.0")]
    UnsupportedVersion,
    /// Required `paths` map is absent.
    #[error("invalid OpenAPI document: `paths` must be an object")]
    InvalidPaths,
    /// Extraction exceeded one configured invocation resource.
    #[error(transparent)]
    LimitExceeded(#[from] ExtractionLimitExceeded),
}

/// Normalizes an HTTP path template for deterministic contract matching.
#[must_use]
pub fn normalize_http_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() || trimmed == "/" {
        return "/".to_owned();
    }

    let mut normalized = String::with_capacity(trimmed.len() + 1);
    if !trimmed.starts_with('/') {
        normalized.push('/');
    }
    let mut previous_slash = false;
    for character in trimmed.chars() {
        if character == '/' {
            if !previous_slash {
                normalized.push(character);
            }
            previous_slash = true;
        } else {
            normalized.push(character);
            previous_slash = false;
        }
    }
    while normalized.len() > 1 && normalized.ends_with('/') {
        normalized.pop();
    }
    normalized
}

/// Extracts provider operations from an `OpenAPI` 3.x or Swagger 2.0 YAML/JSON document.
///
/// # Errors
///
/// Returns [`HttpExtractionError`] when the document is malformed, is not `OpenAPI` 3.x, or
/// does not contain an object-valued `paths` field.
pub fn extract_openapi(
    repo_id: &RepoId,
    source_path: &str,
    input: &str,
) -> Result<Vec<HttpBoundary>, HttpExtractionError> {
    let mut tracker = ExtractionTracker::new(
        source_path,
        "code-system-graph.http.openapi",
        &ExtractionBudgets::default(),
    );
    extract_openapi_with_tracker(repo_id, source_path, input, &mut tracker)
}

/// Extracts provider operations using an existing per-invocation tracker.
///
/// # Errors
///
/// Returns [`HttpExtractionError`] when the document is invalid or a configured extraction
/// budget is exhausted.
pub fn extract_openapi_with_tracker(
    repo_id: &RepoId,
    source_path: &str,
    input: &str,
    tracker: &mut ExtractionTracker,
) -> Result<Vec<HttpBoundary>, HttpExtractionError> {
    tracker.check_input_bytes(u64::try_from(input.len()).unwrap_or(u64::MAX))?;
    tracker.charge_portable_path(source_path)?;
    tracker.check_structured_time()?;
    precheck_openapi_depth(input, tracker)?;
    let parsed = crate::yaml::from_str_with_extraction_budgets(input, tracker.budgets());
    tracker.check_structured_time()?;
    let document: serde_json::Value =
        parsed.map_err(|error| openapi_yaml_error(&error, tracker))?;
    charge_openapi_document(&document, tracker)?;
    let root = document
        .as_object()
        .ok_or(HttpExtractionError::InvalidRoot)?;
    let openapi_version = root.get("openapi").and_then(serde_json::Value::as_str);
    let swagger_version = root.get("swagger").and_then(serde_json::Value::as_str);
    let is_openapi = openapi_version.is_some_and(|version| version.starts_with("3."));
    let is_swagger = swagger_version == Some("2.0");
    if !is_openapi && !is_swagger {
        return Err(HttpExtractionError::UnsupportedVersion);
    }
    let base_path = if is_swagger {
        root.get("basePath")
            .and_then(serde_json::Value::as_str)
            .map(|value| {
                tracker.charge_portable_path(value)?;
                Ok::<_, ExtractionLimitExceeded>(normalize_http_path(value))
            })
            .transpose()?
    } else {
        None
    };
    let paths = root
        .get("paths")
        .and_then(serde_json::Value::as_object)
        .ok_or(HttpExtractionError::InvalidPaths)?;

    let mut boundaries = Vec::new();
    for (raw_path, path_item) in paths {
        tracker.charge_work(1)?;
        tracker.charge_portable_path(raw_path)?;
        let Some(operations) = path_item.as_object() else {
            continue;
        };
        let path = base_path.as_ref().map_or_else(
            || normalize_http_path(raw_path),
            |base| normalize_http_path(&format!("{base}/{raw_path}")),
        );
        tracker.charge_portable_path(&path)?;
        for (raw_method, operation) in operations {
            tracker.charge_work(1)?;
            let Some(method) = HTTP_METHODS
                .iter()
                .find(|candidate| raw_method.eq_ignore_ascii_case(candidate))
                .copied()
            else {
                continue;
            };
            if !operation.is_object() {
                continue;
            }
            tracker.charge_identifier(method)?;
            tracker.charge_observation(1)?;
            boundaries.push(boundary(
                repo_id.clone(),
                method,
                &path,
                BoundaryRole::Provider,
                source_path,
                1.0,
            ));
        }
    }
    boundaries.sort_by(|left, right| (&left.path, &left.method).cmp(&(&right.path, &right.method)));
    tracker.check_structured_time()?;
    Ok(boundaries)
}

fn precheck_openapi_depth(
    input: &str,
    tracker: &mut ExtractionTracker,
) -> Result<(), ExtractionLimitExceeded> {
    let mut block_indents = Vec::new();
    let mut flow_depth = 0_u64;
    let mut quote = None;
    let mut escaped = false;
    let mut block_scalar: Option<(usize, u64)> = None;
    let mut inspected = 0_u64;

    for source_line in input.lines() {
        let indentation = source_line
            .as_bytes()
            .iter()
            .take_while(|byte| **byte == b' ')
            .count();
        let trimmed = source_line.trim();
        if let Some((parent_indent, scalar_bytes)) = block_scalar {
            if trimmed.is_empty() || indentation > parent_indent {
                let content_bytes = source_line.len().saturating_sub(indentation);
                let observed = scalar_bytes
                    .saturating_add(u64::try_from(content_bytes).unwrap_or(u64::MAX))
                    .saturating_add(1);
                tracker.check_string_bytes(observed)?;
                block_scalar = Some((parent_indent, observed));
                continue;
            }
            block_scalar = None;
        }
        if trimmed.is_empty()
            || trimmed.starts_with('#')
            || matches!(trimmed, "---" | "...")
            || trimmed.starts_with('%')
        {
            continue;
        }
        tracker.charge_work(1)?;
        precheck_yaml_scalar_values(source_line, tracker)?;
        while block_indents
            .last()
            .is_some_and(|parent| *parent >= indentation)
        {
            block_indents.pop();
        }
        let block_depth = u64::try_from(block_indents.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let compact_sequence_depth = compact_yaml_sequence_depth(trimmed);
        tracker.check_structural_depth(
            block_depth
                .saturating_add(flow_depth)
                .saturating_add(compact_sequence_depth),
        )?;

        let mut comment = false;
        for current in source_line.chars() {
            inspected = inspected.saturating_add(1);
            if inspected.is_multiple_of(1_024) {
                tracker.check_structured_time()?;
            }
            if comment {
                continue;
            }
            if let Some(delimiter) = quote {
                if delimiter == '"' && escaped {
                    escaped = false;
                } else if delimiter == '"' && current == '\\' {
                    escaped = true;
                } else if current == delimiter {
                    quote = None;
                }
                continue;
            }
            match current {
                '"' | '\'' => quote = Some(current),
                '#' => comment = true,
                '{' | '[' => {
                    flow_depth = flow_depth.saturating_add(1);
                    tracker.check_structural_depth(block_depth.saturating_add(flow_depth))?;
                }
                '}' | ']' => flow_depth = flow_depth.saturating_sub(1),
                _ => {}
            }
        }
        quote = None;
        escaped = false;
        let structural = trimmed.split('#').next().unwrap_or(trimmed).trim_end();
        if structural.ends_with(['|', '>']) {
            block_scalar = Some((indentation, 0_u64));
        } else if structural == "-" || structural.ends_with(':') {
            tracker.check_structural_depth(
                block_depth
                    .saturating_add(flow_depth)
                    .saturating_add(compact_sequence_depth)
                    .saturating_add(1),
            )?;
            block_indents.push(indentation);
        }
    }
    tracker.check_structured_time()?;
    Ok(())
}

fn precheck_yaml_scalar_values(
    source_line: &str,
    tracker: &ExtractionTracker,
) -> Result<(), ExtractionLimitExceeded> {
    let bytes = source_line.as_bytes();
    let mut quote = None;
    let mut quote_start = 0_usize;
    let mut escaped = false;
    let mut colon = None;
    let mut comment = bytes.len();
    let mut cursor = 0_usize;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(delimiter) = quote {
            if delimiter == b'"' && escaped {
                escaped = false;
            } else if delimiter == b'"' && byte == b'\\' {
                escaped = true;
            } else if byte == delimiter {
                let observed =
                    u64::try_from(cursor.saturating_sub(quote_start)).unwrap_or(u64::MAX);
                tracker.check_string_bytes(observed)?;
                quote = None;
            }
        } else {
            match byte {
                b'"' | b'\'' => {
                    quote = Some(byte);
                    quote_start = cursor.saturating_add(1);
                }
                b'#' => {
                    comment = cursor;
                    break;
                }
                b':' if colon.is_none() => colon = Some(cursor),
                _ => {}
            }
        }
        cursor = cursor.saturating_add(1);
    }

    let meaningful = source_line[..comment].trim();
    let sequence_value = meaningful
        .strip_prefix('-')
        .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
        .map(str::trim_start);
    if let Some(colon) = colon.filter(|colon| *colon < comment) {
        let key = source_line[..colon].trim().trim_start_matches('-').trim();
        tracker.check_string_bytes(u64::try_from(key.len()).unwrap_or(u64::MAX))?;
        let value = source_line[colon.saturating_add(1)..comment].trim();
        if !value.is_empty() && !value.starts_with(['"', '\'', '{', '[', '|', '>', '&', '*', '!']) {
            tracker.check_string_bytes(u64::try_from(value.len()).unwrap_or(u64::MAX))?;
        }
    } else if let Some(value) = sequence_value
        && !value.is_empty()
        && !value.starts_with(['"', '\'', '{', '[', '|', '>', '&', '*', '!'])
    {
        tracker.check_string_bytes(u64::try_from(value.len()).unwrap_or(u64::MAX))?;
    }
    Ok(())
}

fn openapi_yaml_error(
    error: &serde_saphyr::DeserializeError,
    tracker: &ExtractionTracker,
) -> HttpExtractionError {
    use serde_saphyr::budget::BudgetBreach;

    let checked = match error {
        serde_saphyr::Error::Budget { breach, .. } => match breach {
            BudgetBreach::Events { events } => tracker.check_work_units(to_u64(*events)),
            BudgetBreach::Aliases { aliases } => tracker.check_work_units(to_u64(*aliases)),
            BudgetBreach::Anchors { anchors } => tracker.check_work_units(to_u64(*anchors)),
            BudgetBreach::Depth { depth } => tracker.check_structural_depth(to_u64(*depth)),
            BudgetBreach::Nodes { nodes } => tracker.check_work_units(to_u64(*nodes)),
            BudgetBreach::ScalarBytes { total_scalar_bytes } => {
                tracker.check_accumulated_string_bytes(to_u64(*total_scalar_bytes))
            }
            BudgetBreach::CommentBytes {
                total_comment_bytes,
            } => tracker.check_input_bytes(to_u64(*total_comment_bytes)),
            BudgetBreach::MergeKeys { merge_keys } => tracker.check_work_units(to_u64(*merge_keys)),
            BudgetBreach::InputBytes { input_bytes } => {
                tracker.check_input_bytes(to_u64(*input_bytes))
            }
            _ => return HttpExtractionError::InvalidDocument,
        },
        serde_saphyr::Error::AliasReplayCounterOverflow { .. } => {
            tracker.check_work_units(u64::MAX)
        }
        serde_saphyr::Error::AliasReplayLimitExceeded {
            total_replayed_events,
            ..
        } => tracker.check_work_units(to_u64(*total_replayed_events)),
        serde_saphyr::Error::AliasExpansionLimitExceeded { expansions, .. } => {
            tracker.check_work_units(to_u64(*expansions))
        }
        serde_saphyr::Error::AliasReplayStackDepthExceeded { depth, .. } => {
            tracker.check_structural_depth(to_u64(*depth))
        }
        _ => return HttpExtractionError::InvalidDocument,
    };
    checked.err().map_or(
        HttpExtractionError::InvalidDocument,
        HttpExtractionError::from,
    )
}

fn to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn compact_yaml_sequence_depth(mut value: &str) -> u64 {
    let mut depth = 0_u64;
    loop {
        let Some(remainder) = value.strip_prefix('-') else {
            return depth;
        };
        if !remainder.is_empty()
            && !remainder
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_whitespace)
        {
            return depth;
        }
        depth = depth.saturating_add(1);
        value = remainder.trim_start();
    }
}

fn charge_openapi_document(
    document: &serde_json::Value,
    tracker: &mut ExtractionTracker,
) -> Result<(), ExtractionLimitExceeded> {
    tracker.check_structural_depth(1)?;
    tracker.charge_work(1)?;
    let mut pending = vec![(document, 1_u64)];
    while let Some((value, depth)) = pending.pop() {
        match value {
            serde_json::Value::Object(object) => {
                let child_depth = depth.saturating_add(1);
                for (key, child) in object {
                    tracker.charge_string(key)?;
                    tracker.check_structural_depth(child_depth)?;
                    tracker.charge_work(1)?;
                    pending.push((child, child_depth));
                }
            }
            serde_json::Value::Array(array) => {
                let child_depth = depth.saturating_add(1);
                for child in array {
                    tracker.check_structural_depth(child_depth)?;
                    tracker.charge_work(1)?;
                    pending.push((child, child_depth));
                }
            }
            serde_json::Value::String(value) => tracker.charge_string(value)?,
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            }
        }
    }
    Ok(())
}

fn boundary(
    repo_id: RepoId,
    method: &str,
    path: &str,
    role: BoundaryRole,
    source_path: &str,
    confidence: f32,
) -> HttpBoundary {
    let (role_key, extractor, provenance) = match role {
        BoundaryRole::Provider => (
            "provider",
            "code-system-graph.http.openapi",
            Provenance::Extracted,
        ),
        BoundaryRole::Consumer => (
            "consumer",
            "code-system-graph.http.declared",
            Provenance::Declared,
        ),
    };
    let stable_key = format!("http:{}:{role_key}:{method}:{path}", repo_id.as_str());
    let evidence_key = format!("{stable_key}:{source_path}");
    HttpBoundary {
        node: Node {
            id: NodeId::new(stable_id("node", &stable_key)),
            kind: NodeKind::HttpOperation,
            repo_id: Some(repo_id.clone()),
            stable_key,
            label: format!("{method} {path}"),
        },
        method: method.to_owned(),
        path: path.to_owned(),
        role,
        evidence: Evidence {
            id: EvidenceId::new(stable_id("evidence", &evidence_key)),
            repo_id: Some(repo_id),
            file_path: Some(source_path.to_owned()),
            start_line: None,
            end_line: None,
            extractor: extractor.to_owned(),
            extractor_version: env!("CARGO_PKG_VERSION").to_owned(),
            provenance,
            confidence,
            observed_at_commit: None,
            content_hash: Some(stable_id("content", &evidence_key)),
            note: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::RepoId;

    use super::{
        extract_openapi, extract_openapi_with_tracker, normalize_http_path, precheck_openapi_depth
    };
    use crate::{ExtractionBudgets, ExtractionResource, ExtractionTracker};

    #[test]
    fn normalize_http_path_should_collapse_slashes_and_trailing_separator() {
        assert_eq!(normalize_http_path("api//orders/"), "/api/orders");
    }

    #[test]
    fn extract_openapi_should_return_sorted_provider_operations() {
        let input = r"
openapi: 3.1.0
paths:
  /api/orders:
    post:
      operationId: createOrder
    get:
      operationId: listOrders
";
        let result = extract_openapi(&RepoId::new("repo:api"), "openapi.yaml", input);
        let methods = result.map(|items| {
            items
                .into_iter()
                .map(|item| item.method)
                .collect::<Vec<_>>()
        });

        assert!(matches!(
            methods,
            Ok(value) if value == vec!["GET".to_owned(), "POST".to_owned()]
        ));
    }

    #[test]
    fn tracked_openapi_should_enforce_observation_and_structural_budgets() {
        let input = r"
openapi: 3.1.0
paths:
  /orders:
    get: {}
    post: {}
";
        let observations = ExtractionBudgets {
            max_observations_per_artifact: 1,
            ..ExtractionBudgets::default()
        };
        let mut observation_tracker =
            ExtractionTracker::new("openapi.yaml", "openapi", &observations);
        assert!(matches!(
            extract_openapi_with_tracker(
                &RepoId::new("repo:api"),
                "openapi.yaml",
                input,
                &mut observation_tracker,
            ),
            Err(super::HttpExtractionError::LimitExceeded(error))
                if error.resource == ExtractionResource::Observations
        ));

        let depth = ExtractionBudgets {
            max_structural_depth_per_artifact: 3,
            ..ExtractionBudgets::default()
        };
        let mut depth_tracker = ExtractionTracker::new("openapi.yaml", "openapi", &depth);
        assert!(matches!(
            extract_openapi_with_tracker(
                &RepoId::new("repo:api"),
                "openapi.yaml",
                input,
                &mut depth_tracker,
            ),
            Err(super::HttpExtractionError::LimitExceeded(error))
                if error.resource == ExtractionResource::StructuralDepth
        ));
    }

    #[test]
    fn openapi_precheck_should_accept_depth_64_and_reject_65_before_parsing() {
        fn nested_openapi(depth: usize) -> String {
            let mut source = String::from("openapi: 3.1.0\npaths: {}\n");
            for level in 0..depth.saturating_sub(1) {
                source.push_str(&" ".repeat(level));
                source.push_str("x-");
                source.push_str(&level.to_string());
                source.push_str(":\n");
            }
            source
        }

        let budgets = ExtractionBudgets {
            max_structural_depth_per_artifact: 64,
            ..ExtractionBudgets::default()
        };
        let mut exact = ExtractionTracker::new("exact.yaml", "openapi", &budgets);
        let mut above = ExtractionTracker::new("above.yaml", "openapi", &budgets);
        assert!(
            extract_openapi_with_tracker(
                &RepoId::new("repo:api"),
                "exact.yaml",
                &nested_openapi(64),
                &mut exact,
            )
            .is_ok()
        );
        assert!(matches!(
            extract_openapi_with_tracker(
                &RepoId::new("repo:api"),
                "above.yaml",
                &nested_openapi(65),
                &mut above,
            ),
            Err(super::HttpExtractionError::LimitExceeded(error))
                if error.resource == ExtractionResource::StructuralDepth
                    && error.observed == 65
                    && error.maximum == 64
        ));
    }

    #[test]
    fn openapi_precheck_should_count_compact_yaml_sequence_depth() {
        fn compact_sequence(depth: usize) -> String {
            format!("x:\n {}value\n", "- ".repeat(depth))
        }

        let budgets = ExtractionBudgets {
            max_structural_depth_per_artifact: 64,
            ..ExtractionBudgets::default()
        };
        let mut exact = ExtractionTracker::new("exact.yaml", "openapi", &budgets);
        let mut above = ExtractionTracker::new("above.yaml", "openapi", &budgets);
        assert!(precheck_openapi_depth(&compact_sequence(62), &mut exact).is_ok());
        assert!(matches!(
            precheck_openapi_depth(&compact_sequence(63), &mut above),
            Err(error)
                if error.resource == ExtractionResource::StructuralDepth
                    && error.observed == 65
                    && error.maximum == 64
        ));
    }

    #[test]
    fn openapi_parser_should_honor_structural_overrides_above_its_library_default() {
        let mut input = String::from("openapi: 3.1.0\npaths: {}\nroot:\n");
        for depth in 1..80 {
            input.push_str(&" ".repeat(depth));
            input.push_str("child:\n");
        }
        input.push_str(&" ".repeat(80));
        input.push_str("value: true\n");
        let budgets = ExtractionBudgets {
            max_structural_depth_per_artifact: 96,
            ..ExtractionBudgets::default()
        };
        let mut tracker = ExtractionTracker::new("deep.yaml", "openapi", &budgets);

        let result = extract_openapi_with_tracker(
            &RepoId::new("repo:api"),
            "deep.yaml",
            &input,
            &mut tracker,
        );
        assert!(
            result.is_ok(),
            "configured depth was not honored: {result:?}"
        );
    }

    #[test]
    fn openapi_scalar_limit_should_apply_before_yaml_dom_materialization() {
        let secret = "sensitive-scalar-value";
        let input = format!("openapi: 3.1.0\ninfo:\n  description: {secret}\npaths: {{}}\n");
        let budgets = ExtractionBudgets {
            max_string_bytes_per_value: 12,
            ..ExtractionBudgets::default()
        };
        let mut tracker = ExtractionTracker::new("secret.yaml", "openapi", &budgets);

        let result = extract_openapi_with_tracker(
            &RepoId::new("repo:api"),
            "secret.yaml",
            &input,
            &mut tracker,
        );
        assert!(
            matches!(
                &result,
                Err(super::HttpExtractionError::LimitExceeded(error))
                    if error.resource == ExtractionResource::StringBytesPerValue
                        && error.observed == u64::try_from(secret.len()).unwrap_or(u64::MAX)
                        && error.maximum == 12
            ),
            "unexpected scalar budget result: {result:?}"
        );
        let diagnostic = format!("{result:?} {result:?}");
        assert!(!diagnostic.contains(secret));
    }

    #[test]
    fn malformed_openapi_error_should_not_echo_source_literals() {
        let secret = "sensitive-duplicate-key-value";
        let input =
            format!("openapi: 3.1.0\ninfo:\n  {secret}: first\n  {secret}: second\npaths: {{}}\n");
        let result = extract_openapi(&RepoId::new("repo:api"), "invalid.yaml", &input);

        assert!(matches!(
            &result,
            Err(super::HttpExtractionError::InvalidDocument)
        ));
        let diagnostic = format!(
            "{result:?} {}",
            result.as_ref().expect_err("malformed input")
        );
        assert!(!diagnostic.contains(secret));
    }

    #[test]
    fn yaml_node_budget_should_abort_before_openapi_dom_materialization() {
        let input = "openapi: 3.1.0\npaths: { /orders: { get: {} } }\n";
        let budgets = ExtractionBudgets {
            max_work_units_per_artifact: 4,
            ..ExtractionBudgets::default()
        };
        let mut tracker = ExtractionTracker::new("nodes.yaml", "openapi", &budgets);

        assert!(matches!(
            extract_openapi_with_tracker(
                &RepoId::new("repo:api"),
                "nodes.yaml",
                input,
                &mut tracker,
            ),
            Err(super::HttpExtractionError::LimitExceeded(error))
                if error.resource == ExtractionResource::WorkUnits
                    && error.observed == 5
                    && error.maximum == 4
        ));
    }

    #[test]
    fn extract_swagger_should_conservatively_apply_base_path() {
        let input = r#"
swagger: "2.0"
basePath: /api
paths:
  /orders/{id}:
    get:
      operationId: getOrder
"#;
        let result = extract_openapi(&RepoId::new("repo:api"), "swagger.yaml", input);

        assert!(matches!(
            result,
            Ok(boundaries)
                if boundaries.len() == 1
                    && boundaries[0].method == "GET"
                    && boundaries[0].path == "/api/orders/{id}"
        ));
    }
}

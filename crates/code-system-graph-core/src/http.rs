use std::collections::BTreeSet;

use code_system_graph_model::{
    Evidence, EvidenceId, Node, NodeId, NodeKind, Provenance, RepoId, stable_id
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::HttpConsumerConfig;

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
    #[error("invalid OpenAPI document: {0}")]
    InvalidDocument(#[from] serde_saphyr::DeserializeError),
    /// Root document is not an object.
    #[error("invalid OpenAPI document: root must be an object")]
    InvalidRoot,
    /// Required contract version is absent or unsupported.
    #[error("unsupported HTTP contract version; expected OpenAPI 3.x or Swagger 2.0")]
    UnsupportedVersion,
    /// Required `paths` map is absent.
    #[error("invalid OpenAPI document: `paths` must be an object")]
    InvalidPaths,
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
    let document: serde_json::Value = crate::yaml::from_str(input)?;
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
    let base_path = is_swagger
        .then(|| {
            root.get("basePath")
                .and_then(serde_json::Value::as_str)
                .map(normalize_http_path)
        })
        .flatten();
    let paths = root
        .get("paths")
        .and_then(serde_json::Value::as_object)
        .ok_or(HttpExtractionError::InvalidPaths)?;

    let allowed_methods = BTreeSet::from(HTTP_METHODS);
    let mut boundaries = Vec::new();
    for (raw_path, path_item) in paths {
        let Some(operations) = path_item.as_object() else {
            continue;
        };
        let path = base_path.as_ref().map_or_else(
            || normalize_http_path(raw_path),
            |base| normalize_http_path(&format!("{base}/{raw_path}")),
        );
        for (raw_method, operation) in operations {
            let method = raw_method.to_ascii_uppercase();
            if !allowed_methods.contains(method.as_str()) || !operation.is_object() {
                continue;
            }
            boundaries.push(boundary(
                repo_id.clone(),
                &method,
                &path,
                BoundaryRole::Provider,
                source_path,
                1.0,
            ));
        }
    }
    boundaries.sort_by(|left, right| (&left.path, &left.method).cmp(&(&right.path, &right.method)));
    Ok(boundaries)
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

    use super::{extract_openapi, normalize_http_path};

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

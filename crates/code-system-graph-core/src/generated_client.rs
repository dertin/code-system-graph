use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Exact generated-client configuration or manifest observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedClientMetadata {
    /// Generator tool family.
    pub tool: String,
    /// Named generator configuration when available.
    pub name: Option<String>,
    /// Generator implementation, such as `typescript-fetch`.
    pub generator_name: Option<String>,
    /// Repository-relative input contract path when explicitly configured.
    pub input_spec: Option<String>,
    /// Repository-relative generated output root when explicitly configured.
    pub output: Option<String>,
    /// Generator version when explicitly recorded.
    pub version: Option<String>,
    /// Generated file path when listed by a tool-owned manifest.
    pub generated_file: Option<String>,
    /// One-based direct evidence line.
    pub line: u32,
}

/// Error returned for malformed generated-client metadata.
#[derive(Debug, Error)]
pub enum GeneratedClientError {
    /// Path is not a supported public metadata artifact.
    #[error("unsupported generated-client metadata `{0}`")]
    UnsupportedPath(String),
    /// Structured metadata is malformed.
    #[error("invalid generated-client metadata: {0}")]
    InvalidMetadata(#[from] serde_json::Error),
    /// Metadata contains an unsafe repository-relative path.
    #[error("generated-client metadata contains unsafe path `{0}`")]
    UnsafePath(String),
}

/// Extracts `OpenAPI` Generator public configuration and output manifests.
///
/// # Errors
///
/// Returns [`GeneratedClientError`] for unsupported paths, malformed JSON, or unsafe paths.
pub fn extract_generated_client_metadata(
    source_path: &str,
    content: &str,
) -> Result<Vec<GeneratedClientMetadata>, GeneratedClientError> {
    let normalized = source_path.replace('\\', "/");
    if normalized.ends_with("openapitools.json") {
        return extract_openapitools(&normalized, content);
    }
    if normalized.ends_with(".openapi-generator/VERSION") {
        let version = content.trim();
        return Ok((!version.is_empty())
            .then(|| GeneratedClientMetadata {
                tool: "openapi-generator".to_owned(),
                name: None,
                generator_name: None,
                input_spec: None,
                output: None,
                version: Some(version.to_owned()),
                generated_file: None,
                line: 1,
            })
            .into_iter()
            .collect());
    }
    if normalized.ends_with(".openapi-generator/FILES") {
        return content
            .lines()
            .enumerate()
            .filter_map(|(index, line)| {
                let path = line.trim();
                (!path.is_empty()).then_some((index, path))
            })
            .map(|(index, path)| {
                validate_relative(path)?;
                Ok(GeneratedClientMetadata {
                    tool: "openapi-generator".to_owned(),
                    name: None,
                    generator_name: None,
                    input_spec: None,
                    output: None,
                    version: None,
                    generated_file: Some(path.to_owned()),
                    line: u32::try_from(index + 1).unwrap_or(u32::MAX),
                })
            })
            .collect();
    }
    Err(GeneratedClientError::UnsupportedPath(
        source_path.to_owned(),
    ))
}

fn extract_openapitools(
    source_path: &str,
    content: &str,
) -> Result<Vec<GeneratedClientMetadata>, GeneratedClientError> {
    let root: serde_json::Value = serde_json::from_str(content)?;
    let generators = root
        .get("generator-cli")
        .and_then(|value| value.get("generators"))
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "`generator-cli.generators` must be an object",
            ))
        })?;
    let version = root
        .get("generator-cli")
        .and_then(|value| value.get("version"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let base = Path::new(source_path)
        .parent()
        .unwrap_or_else(|| Path::new(""));
    let mut output = Vec::new();
    for (name, value) in generators {
        let generator_name = value
            .get("generatorName")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        let input_spec = value
            .get("inputSpec")
            .and_then(serde_json::Value::as_str)
            .map(|path| resolve_relative(base, path))
            .transpose()?;
        let generated_output = value
            .get("output")
            .and_then(serde_json::Value::as_str)
            .map(|path| resolve_relative(base, path))
            .transpose()?;
        output.push(GeneratedClientMetadata {
            tool: "openapi-generator".to_owned(),
            name: Some(name.clone()),
            generator_name,
            input_spec,
            output: generated_output,
            version: version.clone(),
            generated_file: None,
            line: find_line(content, name),
        });
    }
    output.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(output)
}

fn resolve_relative(base: &Path, value: &str) -> Result<String, GeneratedClientError> {
    if portable_absolute(value) {
        return Err(GeneratedClientError::UnsafePath(value.to_owned()));
    }
    let mut normalized = PathBuf::new();
    for component in base.join(value).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(component) => normalized.push(component),
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(GeneratedClientError::UnsafePath(value.to_owned()));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(GeneratedClientError::UnsafePath(value.to_owned()));
            }
        }
    }
    Ok(normalized.to_string_lossy().replace('\\', "/"))
}

fn validate_relative(value: &str) -> Result<(), GeneratedClientError> {
    let path = Path::new(value);
    if portable_absolute(value)
        || path
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(GeneratedClientError::UnsafePath(value.to_owned()));
    }
    Ok(())
}

fn portable_absolute(value: &str) -> bool {
    value.starts_with(['/', '\\'])
        || value
            .as_bytes()
            .get(1)
            .is_some_and(|separator| *separator == b':')
}

fn find_line(content: &str, token: &str) -> u32 {
    content
        .lines()
        .position(|line| line.contains(token))
        .and_then(|index| u32::try_from(index + 1).ok())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::extract_generated_client_metadata;

    #[test]
    fn openapitools_should_resolve_exact_input_and_output_paths() {
        let result = extract_generated_client_metadata(
            "clients/openapitools.json",
            r#"{"generator-cli":{"version":"7.12.0","generators":{"web":{"generatorName":"typescript-fetch","inputSpec":"../openapi.yaml","output":"generated"}}}}"#,
        );

        assert!(matches!(
            result,
            Ok(items)
                if items.len() == 1
                    && items[0].input_spec.as_deref() == Some("openapi.yaml")
                    && items[0].output.as_deref() == Some("clients/generated")
        ));
    }

    #[test]
    fn generated_file_manifest_should_reject_checkout_escape() {
        let result =
            extract_generated_client_metadata(".openapi-generator/FILES", "../secret.txt\n");

        assert!(result.is_err());
    }
}

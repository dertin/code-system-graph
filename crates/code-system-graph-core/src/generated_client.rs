use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ExtractionLimitExceeded, ExtractionTracker};

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
    /// Extraction exceeded one configured invocation resource.
    #[error(transparent)]
    LimitExceeded(#[from] ExtractionLimitExceeded),
}

/// Extracts `OpenAPI` Generator public configuration and output manifests.
///
/// # Errors
///
/// Returns [`GeneratedClientError`] for unsupported paths, malformed JSON, or unsafe paths.
pub fn extract_generated_client_metadata(
    source_path: &str,
    content: &str,
    tracker: &mut ExtractionTracker,
) -> Result<Vec<GeneratedClientMetadata>, GeneratedClientError> {
    let normalized = source_path.replace('\\', "/");
    tracker.charge_portable_path(&normalized)?;
    if normalized.ends_with("openapitools.json") {
        return extract_openapitools(&normalized, content, tracker);
    }
    if normalized.ends_with(".openapi-generator/VERSION") {
        for _ in content.lines() {
            tracker.charge_work(1)?;
        }
        let version = content.trim();
        if version.is_empty() {
            return Ok(Vec::new());
        }
        tracker.charge_string(version)?;
        tracker.charge_observation(1)?;
        return Ok(vec![GeneratedClientMetadata {
            tool: "openapi-generator".to_owned(),
            name: None,
            generator_name: None,
            input_spec: None,
            output: None,
            version: Some(version.to_owned()),
            generated_file: None,
            line: 1,
        }]);
    }
    if normalized.ends_with(".openapi-generator/FILES") {
        let mut output = Vec::new();
        for (index, line) in content.lines().enumerate() {
            tracker.charge_work(1)?;
            let path = line.trim();
            if path.is_empty() {
                continue;
            }
            let path = validate_relative(path)?;
            tracker.charge_portable_path(&path)?;
            tracker.charge_observation(1)?;
            output.push(GeneratedClientMetadata {
                tool: "openapi-generator".to_owned(),
                name: None,
                generator_name: None,
                input_spec: None,
                output: None,
                version: None,
                generated_file: Some(path),
                line: u32::try_from(index + 1).unwrap_or(u32::MAX),
            });
        }
        return Ok(output);
    }
    Err(GeneratedClientError::UnsupportedPath(
        source_path.to_owned(),
    ))
}

fn extract_openapitools(
    source_path: &str,
    content: &str,
    tracker: &mut ExtractionTracker,
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
        .and_then(serde_json::Value::as_str);
    let base = source_path.rsplit_once('/').map_or("", |(base, _)| base);
    let mut output = Vec::new();
    for (name, value) in generators {
        tracker.charge_work(1)?;
        tracker.charge_identifier(name)?;
        let generator_name = value
            .get("generatorName")
            .and_then(serde_json::Value::as_str)
            .map(|value| {
                tracker.charge_identifier(value)?;
                Ok::<_, ExtractionLimitExceeded>(value.to_owned())
            })
            .transpose()?;
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
        if let Some(path) = &input_spec {
            tracker.charge_portable_path(path)?;
        }
        if let Some(path) = &generated_output {
            tracker.charge_portable_path(path)?;
        }
        let version = version
            .map(|value| {
                tracker.charge_string(value)?;
                Ok::<_, ExtractionLimitExceeded>(value.to_owned())
            })
            .transpose()?;
        tracker.charge_observation(1)?;
        output.push(GeneratedClientMetadata {
            tool: "openapi-generator".to_owned(),
            name: Some(name.clone()),
            generator_name,
            input_spec,
            output: generated_output,
            version,
            generated_file: None,
            line: find_line(content, name),
        });
    }
    output.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(output)
}

fn resolve_relative(base: &str, value: &str) -> Result<String, GeneratedClientError> {
    if portable_absolute(value) {
        return Err(GeneratedClientError::UnsafePath(value.to_owned()));
    }
    let portable = value.replace('\\', "/");
    let mut normalized = base
        .split('/')
        .filter(|component| !component.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for component in portable.split('/') {
        match component {
            "" => return Err(GeneratedClientError::UnsafePath(value.to_owned())),
            "." => {}
            ".." => {
                if normalized.pop().is_none() {
                    return Err(GeneratedClientError::UnsafePath(value.to_owned()));
                }
            }
            component => {
                if component.contains(':') {
                    return Err(GeneratedClientError::UnsafePath(value.to_owned()));
                }
                normalized.push(component.to_owned());
            }
        }
    }
    Ok(normalized.join("/"))
}

fn validate_relative(value: &str) -> Result<String, GeneratedClientError> {
    let normalized = value.replace('\\', "/");
    if portable_absolute(&normalized)
        || normalized
            .split('/')
            .any(|component| component.is_empty() || component == ".." || component.contains(':'))
    {
        return Err(GeneratedClientError::UnsafePath(value.to_owned()));
    }
    Ok(normalized)
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
    use super::{GeneratedClientError, extract_generated_client_metadata};
    use crate::{ExtractionBudgets, ExtractionResource, ExtractionTracker};

    fn tracker(path: &str) -> ExtractionTracker {
        ExtractionTracker::new(path, "generated-client", &ExtractionBudgets::default())
    }

    #[test]
    fn openapitools_should_resolve_exact_input_and_output_paths() {
        let result = extract_generated_client_metadata(
            "clients/openapitools.json",
            r#"{"generator-cli":{"version":"7.12.0","generators":{"web":{"generatorName":"typescript-fetch","inputSpec":"../openapi.yaml","output":"generated"}}}}"#,
            &mut tracker("clients/openapitools.json"),
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
        let result = extract_generated_client_metadata(
            ".openapi-generator/FILES",
            "../secret.txt\n",
            &mut tracker(".openapi-generator/FILES"),
        );

        assert!(result.is_err());
    }

    #[test]
    fn generated_file_manifest_should_normalize_safe_mixed_separators() {
        let result = extract_generated_client_metadata(
            ".openapi-generator\\FILES",
            "src\\generated/api.ts\n",
            &mut tracker(".openapi-generator\\FILES"),
        );

        assert!(matches!(
            result,
            Ok(items) if items[0].generated_file.as_deref() == Some("src/generated/api.ts")
        ));
    }

    #[test]
    fn generated_file_manifest_should_reject_drives_empty_components_and_parent_segments() {
        for unsafe_path in [
            "C:\\secret.txt",
            "//server/share.txt",
            "src//generated.ts",
            "src\\..\\secret.txt",
            "src/../secret.txt",
        ] {
            let result = extract_generated_client_metadata(
                ".openapi-generator/FILES",
                unsafe_path,
                &mut tracker(".openapi-generator/FILES"),
            );
            assert!(
                matches!(result, Err(GeneratedClientError::UnsafePath(_))),
                "unsafe path was accepted: {unsafe_path}"
            );
        }
    }

    #[test]
    fn generated_file_lines_and_facts_should_be_charged_before_accumulation() {
        let work_budgets = ExtractionBudgets {
            max_work_units_per_artifact: 2,
            ..ExtractionBudgets::default()
        };
        let mut work = ExtractionTracker::new("FILES", "generated-client", &work_budgets);
        let result = extract_generated_client_metadata(
            ".openapi-generator/FILES",
            "one.ts\ntwo.ts\nthree.ts\n",
            &mut work,
        );
        assert!(matches!(
            result,
            Err(GeneratedClientError::LimitExceeded(error))
                if error.resource == ExtractionResource::WorkUnits
                    && error.observed == 3
                    && error.maximum == 2
        ));

        let observation_budgets = ExtractionBudgets {
            max_observations_per_artifact: 2,
            ..ExtractionBudgets::default()
        };
        let mut observations =
            ExtractionTracker::new("FILES", "generated-client", &observation_budgets);
        let result = extract_generated_client_metadata(
            ".openapi-generator/FILES",
            "one.ts\ntwo.ts\nthree.ts\n",
            &mut observations,
        );
        assert!(matches!(
            result,
            Err(GeneratedClientError::LimitExceeded(error))
                if error.resource == ExtractionResource::Observations
                    && error.observed == 3
                    && error.maximum == 2
        ));
    }
}

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use code_system_graph_model::stable_id;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ContractImplementationConfig, HttpConsumerConfig, IntegrationTestConfig, RepositoryConfig
};

const LOCAL_CONFIG_NAME: &str = ".code-system-graph.yaml";
const MAX_OPENAPI_DISCOVERY_DEPTH: usize = 8;
const MAX_OPENAPI_CANDIDATES: usize = 32;

/// Origin of an effective repository configuration value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    /// Explicit command-line override.
    CliOverride,
    /// Explicit workspace manifest value.
    WorkspaceManifest,
    /// Repository-local `.code-system-graph.yaml` value.
    RepositoryLocal,
    /// Deterministic filesystem auto-detection.
    AutoDetected,
    /// Safe built-in default.
    Default,
}

/// Applies the highest-precedence command-line `OpenAPI` override.
///
/// # Errors
///
/// Returns [`ConfigError::EmptyField`] when the override is empty.
pub fn apply_openapi_override(
    config: &mut EffectiveRepositoryConfig,
    openapi: &str,
) -> Result<(), ConfigError> {
    if openapi.trim().is_empty() {
        return Err(ConfigError::EmptyField {
            path: PathBuf::from("<cli>"),
            field: "repoOpenapi".to_owned(),
        });
    }
    let previous_fingerprint = config.fingerprint.clone();
    config.openapi = vec![openapi.to_owned()];
    config.openapi_source = ConfigSource::CliOverride;
    config.fingerprint = stable_id(
        "repo-config",
        &format!("base={previous_fingerprint};cli_openapi={openapi}"),
    );
    Ok(())
}

/// Strict merged repository configuration after precedence resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EffectiveRepositoryConfig {
    /// Selected `OpenAPI` artifacts relative to the checkout root.
    pub openapi: Vec<String>,
    /// Source that selected the `OpenAPI` artifacts.
    pub openapi_source: ConfigSource,
    /// Selected explicit HTTP consumers.
    pub http_consumers: Vec<HttpConsumerConfig>,
    /// Source that selected the HTTP consumers.
    pub http_consumers_source: ConfigSource,
    /// Selected explicit cross-language integration tests.
    pub integration_tests: Vec<IntegrationTestConfig>,
    /// Source that selected integration tests.
    pub integration_tests_source: ConfigSource,
    /// Selected explicit contract implementation anchors.
    pub implementations: Vec<ContractImplementationConfig>,
    /// Source that selected implementation anchors.
    pub implementations_source: ConfigSource,
    /// Fingerprint of all effective values and relevant local configuration.
    pub fingerprint: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepositoryLocalConfig {
    version: u32,
    openapi: Option<String>,
    http_consumers: Option<Vec<HttpConsumerConfig>>,
    integration_tests: Option<Vec<IntegrationTestConfig>>,
    implementations: Option<Vec<ContractImplementationConfig>>,
}

/// Error returned while resolving repository configuration precedence.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// Repository-local configuration could not be read.
    #[error("failed to read repository config `{path}`: {source}")]
    Read {
        /// Configuration path.
        path: PathBuf,
        /// Underlying filesystem error.
        source: std::io::Error,
    },
    /// Repository-local configuration is malformed or contains unknown keys.
    #[error("invalid repository config `{path}`: {source}")]
    Invalid {
        /// Configuration path.
        path: PathBuf,
        /// YAML decoding error.
        source: Box<serde_saphyr::DeserializeError>,
    },
    /// Repository-local configuration schema version is unsupported.
    #[error("unsupported repository config version {found} in `{path}`; expected version 1")]
    UnsupportedVersion {
        /// Configuration path.
        path: PathBuf,
        /// Version found in the file.
        found: u32,
    },
    /// A configured value is empty.
    #[error("repository config field `{field}` in `{path}` must not be empty")]
    EmptyField {
        /// Configuration path.
        path: PathBuf,
        /// Dot-style field location.
        field: String,
    },
    /// Auto-detection found multiple plausible `OpenAPI` artifacts.
    #[error("ambiguous OpenAPI auto-detection in `{root}`: {candidates:?}")]
    AmbiguousOpenApi {
        /// Repository checkout root.
        root: PathBuf,
        /// Candidate paths in deterministic order.
        candidates: Vec<String>,
    },
}

/// Resolves workspace, repository-local, auto-detected, and default values in that order.
///
/// An explicit empty workspace `httpConsumers` list suppresses lower-precedence values.
/// Auto-detection never chooses among multiple candidates.
///
/// # Errors
///
/// Returns [`ConfigError`] for unreadable or invalid local configuration, empty values, or
/// ambiguous auto-detection.
pub fn resolve_repository_config(
    checkout_path: &Path,
    workspace: &RepositoryConfig,
) -> Result<EffectiveRepositoryConfig, ConfigError> {
    let local_path = checkout_path.join(LOCAL_CONFIG_NAME);
    let (local, local_source) = if local_path.exists() {
        let source = std::fs::read_to_string(&local_path).map_err(|source| ConfigError::Read {
            path: local_path.clone(),
            source,
        })?;
        let config: RepositoryLocalConfig =
            crate::yaml::from_str(&source).map_err(|source| ConfigError::Invalid {
                path: local_path.clone(),
                source: Box::new(source),
            })?;
        validate_local(&local_path, &config)?;
        (Some(config), Some(source))
    } else {
        (None, None)
    };

    let (openapi, openapi_source) = if let Some(openapi) = &workspace.openapi {
        (vec![openapi.clone()], ConfigSource::WorkspaceManifest)
    } else if let Some(openapi) = local.as_ref().and_then(|config| config.openapi.clone()) {
        (vec![openapi], ConfigSource::RepositoryLocal)
    } else {
        let candidates = discover_openapi_candidates(checkout_path)?;
        if candidates.is_empty() {
            (Vec::new(), ConfigSource::Default)
        } else {
            reject_competing_openapi_formats(checkout_path, &candidates)?;
            (candidates, ConfigSource::AutoDetected)
        }
    };
    let (http_consumers, http_consumers_source) = if let Some(consumers) = &workspace.http_consumers
    {
        (consumers.clone(), ConfigSource::WorkspaceManifest)
    } else if let Some(consumers) = local
        .as_ref()
        .and_then(|config| config.http_consumers.clone())
    {
        (consumers, ConfigSource::RepositoryLocal)
    } else {
        (Vec::new(), ConfigSource::Default)
    };
    let (integration_tests, integration_tests_source) =
        if let Some(tests) = &workspace.integration_tests {
            (tests.clone(), ConfigSource::WorkspaceManifest)
        } else if let Some(tests) = local
            .as_ref()
            .and_then(|config| config.integration_tests.clone())
        {
            (tests, ConfigSource::RepositoryLocal)
        } else {
            (Vec::new(), ConfigSource::Default)
        };
    let (implementations, implementations_source) =
        if let Some(implementations) = &workspace.implementations {
            (implementations.clone(), ConfigSource::WorkspaceManifest)
        } else if let Some(implementations) = local
            .as_ref()
            .and_then(|config| config.implementations.clone())
        {
            (implementations, ConfigSource::RepositoryLocal)
        } else {
            (Vec::new(), ConfigSource::Default)
        };
    let fingerprint_material = format!(
        "openapi={openapi:?};openapi_source={openapi_source:?};\
         consumers={http_consumers:?};consumers_source={http_consumers_source:?};\
         tests={integration_tests:?};tests_source={integration_tests_source:?};\
         implementations={implementations:?};implementations_source={implementations_source:?};\
         local={local_source:?}"
    );
    Ok(EffectiveRepositoryConfig {
        openapi,
        openapi_source,
        http_consumers,
        http_consumers_source,
        integration_tests,
        integration_tests_source,
        implementations,
        implementations_source,
        fingerprint: stable_id("repo-config", &fingerprint_material),
    })
}

fn discover_openapi_candidates(root: &Path) -> Result<Vec<String>, ConfigError> {
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    let mut candidates = Vec::new();
    while let Some((directory, depth)) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|source| ConfigError::Read {
            path: directory.clone(),
            source,
        })?;
        let mut entries =
            entries
                .collect::<Result<Vec<_>, _>>()
                .map_err(|source| ConfigError::Read {
                    path: directory.clone(),
                    source,
                })?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let file_type = entry.file_type().map_err(|source| ConfigError::Read {
                path: entry.path(),
                source,
            })?;
            let name = entry.file_name().to_string_lossy().to_string();
            if file_type.is_dir() {
                if depth < MAX_OPENAPI_DISCOVERY_DEPTH && !ignored_discovery_directory(&name) {
                    pending.push((entry.path(), depth.saturating_add(1)));
                }
                continue;
            }
            if !file_type.is_file() || !openapi_filename(&name) {
                continue;
            }
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .unwrap_or(path.as_path())
                .to_string_lossy()
                .replace('\\', "/");
            candidates.push(relative);
            if candidates.len() >= MAX_OPENAPI_CANDIDATES {
                break;
            }
        }
        if candidates.len() >= MAX_OPENAPI_CANDIDATES {
            break;
        }
    }
    candidates.sort();
    candidates.dedup();
    Ok(candidates)
}

fn ignored_discovery_directory(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | ".codegraph"
            | ".next"
            | ".venv"
            | "venv"
            | "node_modules"
            | "target"
            | "__pycache__"
    )
}

fn openapi_filename(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("openapi")
        && matches!(
            Path::new(&lower)
                .extension()
                .and_then(|value| value.to_str()),
            Some("json" | "yaml" | "yml")
        )
}

fn reject_competing_openapi_formats(root: &Path, candidates: &[String]) -> Result<(), ConfigError> {
    let mut by_stem = BTreeMap::<String, Vec<String>>::new();
    for candidate in candidates {
        let path = Path::new(candidate);
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or(candidate);
        by_stem
            .entry(parent.join(stem).to_string_lossy().to_string())
            .or_default()
            .push(candidate.clone());
    }
    let ambiguous = by_stem
        .into_values()
        .filter(|values| values.len() > 1)
        .flatten()
        .collect::<Vec<_>>();
    if ambiguous.is_empty() {
        Ok(())
    } else {
        Err(ConfigError::AmbiguousOpenApi {
            root: root.to_path_buf(),
            candidates: ambiguous,
        })
    }
}

fn validate_local(path: &Path, config: &RepositoryLocalConfig) -> Result<(), ConfigError> {
    if config.version != 1 {
        return Err(ConfigError::UnsupportedVersion {
            path: path.to_path_buf(),
            found: config.version,
        });
    }
    if config
        .openapi
        .as_ref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(ConfigError::EmptyField {
            path: path.to_path_buf(),
            field: "openapi".to_owned(),
        });
    }
    for (index, consumer) in config.http_consumers.iter().flatten().enumerate() {
        for (field, value) in [
            ("method", consumer.method.as_str()),
            ("path", consumer.path.as_str()),
            ("source", consumer.source.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ConfigError::EmptyField {
                    path: path.to_path_buf(),
                    field: format!("httpConsumers[{index}].{field}"),
                });
            }
        }
    }
    for (index, test) in config.integration_tests.iter().flatten().enumerate() {
        for (field, value) in [
            ("name", test.name.as_str()),
            ("path", test.path.as_str()),
            ("framework", test.framework.as_str()),
            ("language", test.language.as_str()),
            ("validates.method", test.validates.method.as_str()),
            ("validates.path", test.validates.path.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ConfigError::EmptyField {
                    path: path.to_path_buf(),
                    field: format!("integrationTests[{index}].{field}"),
                });
            }
        }
    }
    for (index, implementation) in config.implementations.iter().flatten().enumerate() {
        for (field, value) in [
            ("language", implementation.language.as_str()),
            ("path", implementation.path.as_str()),
            ("symbol", implementation.symbol.as_str()),
            (
                "implements.method",
                implementation.implements.method.as_str(),
            ),
            ("implements.path", implementation.implements.path.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(ConfigError::EmptyField {
                    path: path.to_path_buf(),
                    field: format!("implementations[{index}].{field}"),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{ConfigError, ConfigSource, resolve_repository_config};
    use crate::{HttpConsumerConfig, RepositoryConfig};

    #[test]
    fn workspace_manifest_should_override_repository_local_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = tempfile::tempdir()?;
        fs::write(
            repository.path().join(".code-system-graph.yaml"),
            "version: 1\nopenapi: local.yaml\nhttpConsumers: []\n",
        )?;
        let workspace = RepositoryConfig {
            path: ".".to_owned(),
            openapi: Some("workspace.yaml".to_owned()),
            http_consumers: Some(vec![HttpConsumerConfig {
                method: "GET".to_owned(),
                path: "/health".to_owned(),
                source: "client.rs".to_owned(),
            }]),
            integration_tests: None,
            implementations: None,
        };

        let resolved = resolve_repository_config(repository.path(), &workspace)?;

        assert_eq!(
            (
                resolved.openapi,
                resolved.openapi_source,
                resolved.http_consumers.len(),
                resolved.http_consumers_source,
            ),
            (
                vec!["workspace.yaml".to_owned()],
                ConfigSource::WorkspaceManifest,
                1,
                ConfigSource::WorkspaceManifest,
            )
        );
        Ok(())
    }

    #[test]
    fn auto_detection_should_keep_distinct_nested_openapi_documents()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = tempfile::tempdir()?;
        fs::create_dir_all(repository.path().join("backend/docs"))?;
        fs::create_dir_all(repository.path().join("frontend/mockoon"))?;
        fs::write(repository.path().join("backend/docs/openapi.json"), "{}")?;
        fs::write(
            repository
                .path()
                .join("frontend/mockoon/mock_openapi3.json"),
            "{}",
        )?;
        let workspace = RepositoryConfig {
            path: ".".to_owned(),
            openapi: None,
            http_consumers: None,
            integration_tests: None,
            implementations: None,
        };

        let resolved = resolve_repository_config(repository.path(), &workspace)?;

        assert_eq!(
            resolved.openapi,
            vec![
                "backend/docs/openapi.json".to_owned(),
                "frontend/mockoon/mock_openapi3.json".to_owned(),
            ]
        );
        assert_eq!(resolved.openapi_source, ConfigSource::AutoDetected);
        Ok(())
    }

    #[test]
    fn auto_detection_should_reject_multiple_openapi_candidates()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = tempfile::tempdir()?;
        fs::write(repository.path().join("openapi.yaml"), "openapi: 3.1.0")?;
        fs::write(repository.path().join("openapi.json"), "{}")?;
        let workspace = RepositoryConfig {
            path: ".".to_owned(),
            openapi: None,
            http_consumers: None,
            integration_tests: None,
            implementations: None,
        };

        let result = resolve_repository_config(repository.path(), &workspace);

        assert!(matches!(result, Err(ConfigError::AmbiguousOpenApi { .. })));
        Ok(())
    }

    #[test]
    fn repository_local_config_should_reject_unknown_keys() -> Result<(), Box<dyn std::error::Error>>
    {
        let repository = tempfile::tempdir()?;
        fs::write(
            repository.path().join(".code-system-graph.yaml"),
            "version: 1\nunknown: true\n",
        )?;
        let workspace = RepositoryConfig {
            path: ".".to_owned(),
            openapi: None,
            http_consumers: None,
            integration_tests: None,
            implementations: None,
        };

        let result = resolve_repository_config(repository.path(), &workspace);

        assert!(matches!(result, Err(ConfigError::Invalid { .. })));
        Ok(())
    }

    #[test]
    fn cli_openapi_should_override_workspace_value() -> Result<(), Box<dyn std::error::Error>> {
        let repository = tempfile::tempdir()?;
        let workspace = RepositoryConfig {
            path: ".".to_owned(),
            openapi: Some("workspace.yaml".to_owned()),
            http_consumers: None,
            integration_tests: None,
            implementations: None,
        };
        let mut resolved = resolve_repository_config(repository.path(), &workspace)?;

        super::apply_openapi_override(&mut resolved, "cli.yaml")?;

        assert_eq!(
            (resolved.openapi, resolved.openapi_source),
            (vec!["cli.yaml".to_owned()], ConfigSource::CliOverride)
        );
        Ok(())
    }
}

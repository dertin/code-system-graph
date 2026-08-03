use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use code_system_graph_model::stable_id;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    CapabilityDir, CapabilityError, ContractImplementationConfig, HttpConsumerConfig, IgnorePatternError, IgnorePolicy, IntegrationTestConfig, MAX_REPOSITORY_CONFIG_BYTES, RepositoryConfig, validate_excludes, validate_include_defaults
};

const LOCAL_CONFIG_NAME: &str = ".code-system-graph.yaml";
const MAX_OPENAPI_DISCOVERY_DEPTH: usize = 8;
const MAX_OPENAPI_CANDIDATES: usize = 32;

/// Origin of an effective repository configuration value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    /// Effective automatic-discovery exclusion policy.
    pub ignore_policy: IgnorePolicy,
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
    excludes: Option<Vec<String>>,
    include_defaults: Option<Vec<String>>,
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
    /// A repository-local discovery pattern is malformed or unsafe.
    #[error("repository config field `{field}` in `{path}` is invalid: {source}")]
    InvalidIgnorePattern {
        /// Configuration path.
        path: PathBuf,
        /// Dot-style field location.
        field: String,
        /// Pattern validation failure.
        #[source]
        source: IgnorePatternError,
    },
    /// Repository-local configuration is a symbolic link or reparse point.
    #[error("repository config `{path}` is a symbolic link or reparse point")]
    Symlink {
        /// Configuration path.
        path: PathBuf,
    },
    /// Repository-local configuration is not a regular file.
    #[error("repository config `{path}` is not a regular file")]
    NotRegularFile {
        /// Configuration path.
        path: PathBuf,
    },
    /// Repository-local configuration exceeds the accepted size limit.
    #[error("repository config `{path}` exceeds {limit} bytes")]
    TooLarge {
        /// Configuration path.
        path: PathBuf,
        /// Maximum accepted size in bytes.
        limit: usize,
    },
    /// Repository-local configuration resolves outside the checkout root.
    #[error("repository config `{path}` is outside checkout `{checkout}`")]
    OutsideCheckout {
        /// Configuration path.
        path: PathBuf,
        /// Canonical checkout root.
        checkout: PathBuf,
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
    let checkout = CapabilityDir::open(checkout_path)
        .map_err(|error| map_capability_error(error, checkout_path, LOCAL_CONFIG_NAME))?;
    let local_relative = Path::new(LOCAL_CONFIG_NAME);
    let (local, local_source) = if checkout
        .regular_file_exists(local_relative)
        .map_err(|error| map_capability_error(error, checkout_path, LOCAL_CONFIG_NAME))?
    {
        let local_path = checkout_path.join(LOCAL_CONFIG_NAME);
        let source = checkout
            .read_utf8_file_bounded(local_relative, MAX_REPOSITORY_CONFIG_BYTES)
            .map_err(|error| map_capability_error(error, checkout_path, LOCAL_CONFIG_NAME))?;
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

    let ignore_policy = resolve_ignore_policy(checkout_path, workspace, local.as_ref())?;
    let (openapi, openapi_source) = if let Some(openapi) = &workspace.openapi {
        (vec![openapi.clone()], ConfigSource::WorkspaceManifest)
    } else if let Some(openapi) = local.as_ref().and_then(|config| config.openapi.clone()) {
        (vec![openapi], ConfigSource::RepositoryLocal)
    } else {
        let candidates = discover_openapi_candidates(checkout_path, &ignore_policy)?;
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
         ignore_policy={};\
         local={local_source:?}",
        ignore_policy.fingerprint_material()
    );
    Ok(EffectiveRepositoryConfig {
        ignore_policy,
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

fn map_capability_error(error: CapabilityError, checkout: &Path, config_name: &str) -> ConfigError {
    let path = checkout.join(config_name);
    match error {
        CapabilityError::Io { path, source } => ConfigError::Read { path, source },
        CapabilityError::Symlink { .. } => ConfigError::Symlink { path },
        CapabilityError::NotRegularFile { .. } => ConfigError::NotRegularFile { path },
        CapabilityError::TooLarge { limit, .. } => ConfigError::TooLarge { path, limit },
        CapabilityError::OutsideRoot { root, .. } => ConfigError::OutsideCheckout {
            path,
            checkout: root,
        },
        CapabilityError::InvalidRelativePath { .. }
        | CapabilityError::NotDirectory { .. }
        | CapabilityError::InvalidUtf8 { .. } => ConfigError::Read {
            path,
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()),
        },
    }
}

fn discover_openapi_candidates(
    root: &Path,
    ignore_policy: &IgnorePolicy,
) -> Result<Vec<String>, ConfigError> {
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
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap_or(path.as_path());
            if file_type.is_dir() {
                if depth < MAX_OPENAPI_DISCOVERY_DEPTH && !ignore_policy.excludes(relative, true) {
                    pending.push((path, depth.saturating_add(1)));
                }
                continue;
            }
            if !file_type.is_file()
                || ignore_policy.excludes(relative, false)
                || !openapi_filename(&name)
            {
                continue;
            }
            let relative = relative.to_string_lossy().replace('\\', "/");
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
    if let Some(patterns) = &config.excludes {
        validate_excludes(patterns).map_err(|source| ConfigError::InvalidIgnorePattern {
            path: path.to_path_buf(),
            field: "excludes".to_owned(),
            source,
        })?;
    }
    if let Some(patterns) = &config.include_defaults {
        validate_include_defaults(patterns).map_err(|source| {
            ConfigError::InvalidIgnorePattern {
                path: path.to_path_buf(),
                field: "includeDefaults".to_owned(),
                source,
            }
        })?;
    }
    Ok(())
}

fn select_patterns(
    workspace: Option<&Vec<String>>,
    local: Option<&Vec<String>>,
) -> (Vec<String>, ConfigSource) {
    if let Some(patterns) = workspace {
        (patterns.clone(), ConfigSource::WorkspaceManifest)
    } else if let Some(patterns) = local {
        (patterns.clone(), ConfigSource::RepositoryLocal)
    } else {
        (Vec::new(), ConfigSource::Default)
    }
}

fn resolve_ignore_policy(
    checkout_path: &Path,
    workspace: &RepositoryConfig,
    local: Option<&RepositoryLocalConfig>,
) -> Result<IgnorePolicy, ConfigError> {
    let (excludes, excludes_source) = select_patterns(
        workspace.excludes.as_ref(),
        local.and_then(|config| config.excludes.as_ref()),
    );
    let (include_defaults, include_defaults_source) = select_patterns(
        workspace.include_defaults.as_ref(),
        local.and_then(|config| config.include_defaults.as_ref()),
    );
    IgnorePolicy::new(
        excludes,
        excludes_source,
        include_defaults,
        include_defaults_source,
    )
    .map_err(|source| ConfigError::InvalidIgnorePattern {
        path: checkout_path.to_path_buf(),
        field: "ignorePolicy".to_owned(),
        source,
    })
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
            excludes: None,
            include_defaults: None,
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
    fn workspace_ignore_fields_should_override_repository_local_lists()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = tempfile::tempdir()?;
        fs::write(
            repository.path().join(".code-system-graph.yaml"),
            "version: 1\nexcludes: [local/**]\nincludeDefaults: [vendor/local/**]\n",
        )?;
        let workspace = RepositoryConfig {
            path: ".".to_owned(),
            openapi: None,
            http_consumers: None,
            integration_tests: None,
            implementations: None,
            excludes: Some(vec!["workspace/**".to_owned()]),
            include_defaults: Some(Vec::new()),
        };

        let resolved = resolve_repository_config(repository.path(), &workspace)?;

        assert_eq!(
            (
                resolved.ignore_policy.configured_excludes().to_vec(),
                resolved.ignore_policy.configured_excludes_source(),
                resolved.ignore_policy.include_defaults().to_vec(),
                resolved.ignore_policy.include_defaults_source(),
            ),
            (
                vec!["workspace/**".to_owned()],
                ConfigSource::WorkspaceManifest,
                Vec::new(),
                ConfigSource::WorkspaceManifest,
            )
        );
        Ok(())
    }

    #[test]
    fn canonical_equivalent_patterns_should_produce_the_same_repository_fingerprint()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = tempfile::tempdir()?;
        let canonical = RepositoryConfig {
            path: ".".to_owned(),
            openapi: None,
            http_consumers: None,
            integration_tests: None,
            implementations: None,
            excludes: Some(vec!["coverage/**".to_owned()]),
            include_defaults: Some(vec!["vendor/internal-sdk/**".to_owned()]),
        };
        let redundant = RepositoryConfig {
            excludes: Some(vec!["./coverage//./**".to_owned()]),
            include_defaults: Some(vec!["./vendor//internal-sdk/./**".to_owned()]),
            ..canonical.clone()
        };

        let canonical = resolve_repository_config(repository.path(), &canonical)?;
        let redundant = resolve_repository_config(repository.path(), &redundant)?;

        assert_eq!(canonical.fingerprint, redundant.fingerprint);
        Ok(())
    }

    #[test]
    fn openapi_auto_detection_should_respect_reopened_default_subtree()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = tempfile::tempdir()?;
        fs::create_dir_all(repository.path().join("vendor/internal-sdk"))?;
        fs::create_dir_all(repository.path().join("vendor/external"))?;
        fs::write(
            repository.path().join("vendor/internal-sdk/openapi.yaml"),
            "{}",
        )?;
        fs::write(repository.path().join("vendor/external/openapi.yaml"), "{}")?;
        let workspace = RepositoryConfig {
            path: ".".to_owned(),
            openapi: None,
            http_consumers: None,
            integration_tests: None,
            implementations: None,
            excludes: None,
            include_defaults: Some(vec!["vendor/internal-sdk/**".to_owned()]),
        };

        let resolved = resolve_repository_config(repository.path(), &workspace)?;

        assert_eq!(
            resolved.openapi,
            vec!["vendor/internal-sdk/openapi.yaml".to_owned()]
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
            excludes: None,
            include_defaults: None,
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
            excludes: None,
            include_defaults: None,
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
            excludes: None,
            include_defaults: None,
        };

        let result = resolve_repository_config(repository.path(), &workspace);

        assert!(matches!(result, Err(ConfigError::Invalid { .. })));
        Ok(())
    }

    #[test]
    fn repository_local_config_should_reject_symlink() -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let repository = temporary.path().join("repository");
        let outside = temporary.path().join("outside.yaml");
        std::fs::create_dir_all(&repository)?;
        std::fs::write(&outside, "version: 1\n")?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, repository.join(".code-system-graph.yaml"))?;
        let workspace = RepositoryConfig {
            path: ".".to_owned(),
            openapi: None,
            http_consumers: None,
            integration_tests: None,
            implementations: None,
            excludes: None,
            include_defaults: None,
        };

        let result = resolve_repository_config(&repository, &workspace);

        assert!(matches!(
            result,
            Err(ConfigError::Symlink { .. }
                | ConfigError::OutsideCheckout { .. }
                | ConfigError::NotRegularFile { .. })
        ));
        Ok(())
    }

    #[test]
    fn repository_local_config_should_reject_oversized_file()
    -> Result<(), Box<dyn std::error::Error>> {
        let repository = tempfile::tempdir()?;
        std::fs::write(
            repository.path().join(".code-system-graph.yaml"),
            "x".repeat(crate::capability_dir::MAX_REPOSITORY_CONFIG_BYTES + 1),
        )?;
        let workspace = RepositoryConfig {
            path: ".".to_owned(),
            openapi: None,
            http_consumers: None,
            integration_tests: None,
            implementations: None,
            excludes: None,
            include_defaults: None,
        };

        let result = resolve_repository_config(repository.path(), &workspace);

        assert!(matches!(result, Err(ConfigError::TooLarge { .. })));
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
            excludes: None,
            include_defaults: None,
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

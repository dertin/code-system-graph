use std::collections::BTreeMap;

use code_system_graph_model::{EdgeKind, contains_unsafe_metadata_characters};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::execution_policy::{
    CodeGraphCorroborationAnchorLimit, ExecutionPolicy, ExecutionPolicyOverrides, InvalidExecutionPolicy
};
use crate::extraction_budget::{
    ExtractionBudgetOverrides, ExtractionBudgets, InvalidExtractionBudget
};
use crate::ignore_policy::{IgnorePatternError, validate_excludes, validate_include_defaults};

const MANUAL_ENDPOINT_MAX_BYTES: usize = 2_048;
const MANUAL_CONTRACT_MAX_BYTES: usize = 1_024;
const MANUAL_REASON_MAX_BYTES: usize = 4_096;

/// Strict workspace manifest accepted by the initial registry slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceManifest {
    /// Manifest schema version.
    pub version: u32,
    /// Stable workspace name.
    pub name: String,
    /// Additional filesystem roots repositories may resolve beneath.
    #[serde(rename = "allowedRoots", default)]
    pub allowed_roots: Vec<String>,
    /// Repositories indexed by unique alias.
    pub repos: BTreeMap<String, RepositoryConfig>,
    /// Versioned exact relationship declarations and suppressions.
    #[serde(rename = "manualLinks", default)]
    pub manual_links: Vec<ManualLinkConfig>,
    /// Optional operator-owned extraction safety limit overrides.
    #[serde(rename = "extractionBudgets", default)]
    pub extraction_budgets: Option<ExtractionBudgetOverrides>,
    /// Optional operator-owned supervised-execution policy overrides.
    #[serde(rename = "executionPolicy", default)]
    pub execution_policy: Option<ExecutionPolicyOverrides>,
}

/// Repository registration and boundary inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RepositoryConfig {
    /// Native path as written in the manifest.
    pub path: String,
    /// Optional `OpenAPI` artifact relative to the repository root.
    pub openapi: Option<String>,
    /// Explicit HTTP consumers that cannot yet be extracted from source.
    pub http_consumers: Option<Vec<HttpConsumerConfig>>,
    /// Explicit cross-language integration tests and validated HTTP contracts.
    pub integration_tests: Option<Vec<IntegrationTestConfig>>,
    /// Explicit contract-to-source implementation anchors.
    pub implementations: Option<Vec<ContractImplementationConfig>>,
    /// Additional repository-relative globs omitted from automatic discovery.
    pub excludes: Option<Vec<String>>,
    /// Repository-relative exceptions to reactivable built-in exclusions.
    pub include_defaults: Option<Vec<String>>,
}

/// Additive manifest settings introduced without changing exhaustively constructible public
/// configuration structs.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ManifestExtensions {
    repository_use_gitignore: BTreeMap<String, bool>,
    max_codegraph_corroboration_anchors_per_repo: Option<i64>,
}

impl ManifestExtensions {
    /// Returns the workspace-level `.gitignore` choice for one repository alias.
    #[must_use]
    pub fn repository_use_gitignore(&self, alias: &str) -> Option<bool> {
        self.repository_use_gitignore.get(alias).copied()
    }

    /// Returns the configured `CodeGraph` corroboration bound, including `-1` for unlimited.
    #[must_use]
    pub const fn max_codegraph_corroboration_anchors_per_repo(&self) -> Option<i64> {
        self.max_codegraph_corroboration_anchors_per_repo
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceManifestWire {
    version: u32,
    name: String,
    #[serde(rename = "allowedRoots", default)]
    allowed_roots: Vec<String>,
    repos: BTreeMap<String, RepositoryConfigWire>,
    #[serde(rename = "manualLinks", default)]
    manual_links: Vec<ManualLinkConfig>,
    #[serde(rename = "extractionBudgets", default)]
    extraction_budgets: Option<ExtractionBudgetOverrides>,
    #[serde(rename = "executionPolicy", default)]
    execution_policy: Option<ExecutionPolicyOverridesWire>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RepositoryConfigWire {
    path: String,
    openapi: Option<String>,
    http_consumers: Option<Vec<HttpConsumerConfig>>,
    integration_tests: Option<Vec<IntegrationTestConfig>>,
    implementations: Option<Vec<ContractImplementationConfig>>,
    excludes: Option<Vec<String>>,
    include_defaults: Option<Vec<String>>,
    use_gitignore: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExecutionPolicyOverridesWire {
    max_scan_wall_time_ms: Option<u64>,
    max_no_progress_time_ms: Option<u64>,
    #[serde(rename = "maxCodeGraphSyncWallTimeMsPerRepo")]
    max_codegraph_sync_wall_time_ms_per_repo: Option<u64>,
    #[serde(rename = "maxCodeGraphCorroborationAnchorsPerRepo")]
    max_codegraph_corroboration_anchors_per_repo: Option<i64>,
    max_worker_memory_bytes: Option<u64>,
    graceful_termination_ms: Option<u64>,
    watch_idle_timeout_ms: Option<u64>,
    max_watch_session_wall_time_ms: Option<u64>,
    min_watch_rescan_interval_ms: Option<u64>,
    max_checkpoint_cache_bytes: Option<u64>,
}

/// Exact manual relationship or automatic-link suppression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ManualLinkConfig {
    /// Exact source node identifier or stable key.
    pub from: String,
    /// Exact target node identifier or stable key.
    pub to: String,
    /// Concrete graph relationship.
    pub relation: EdgeKind,
    /// Optional contract identity retained for audit context.
    pub contract: Option<String>,
    /// Mandatory human explanation for the override.
    pub reason: String,
    /// Whether to remove the exact automatic relationship instead of creating one.
    #[serde(default)]
    pub suppress: bool,
}

/// Explicit HTTP consumer boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HttpConsumerConfig {
    /// Upper- or lower-case HTTP method.
    pub method: String,
    /// HTTP path template.
    pub path: String,
    /// Repository-relative evidence path.
    pub source: String,
}

/// Explicit test case that validates an HTTP contract across a repository boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IntegrationTestConfig {
    /// Test function or scenario name.
    pub name: String,
    /// Repository-relative test source path.
    pub path: String,
    /// Test framework, such as `pytest`.
    pub framework: String,
    /// Source language, such as `python`.
    pub language: String,
    /// HTTP contract validated by this test.
    pub validates: HttpContractConfig,
}

/// Canonicalizable HTTP contract target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HttpContractConfig {
    /// HTTP method.
    pub method: String,
    /// HTTP path template.
    pub path: String,
}

/// Explicit source symbol that implements an HTTP contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContractImplementationConfig {
    /// Source language, such as `rust`.
    pub language: String,
    /// Repository-relative implementation source path.
    pub path: String,
    /// Qualified or local symbol name.
    pub symbol: String,
    /// HTTP contract implemented by the symbol.
    pub implements: HttpContractConfig,
}

/// Error returned while validating a workspace manifest.
#[derive(Debug, Error)]
pub enum ManifestError {
    /// YAML could not be decoded according to the strict schema.
    #[error("invalid workspace manifest: {0}")]
    InvalidYaml(#[from] serde_saphyr::DeserializeError),
    /// The manifest uses an unsupported schema version.
    #[error("unsupported manifest version {found}; expected version 1")]
    UnsupportedVersion {
        /// Version supplied by the user.
        found: u32,
    },
    /// A required string is empty after trimming.
    #[error("manifest field `{field}` must not be empty")]
    EmptyField {
        /// Dot-style field location.
        field: String,
    },
    /// A metadata field contains terminal control or bidirectional formatting characters.
    #[error("manifest field `{field}` contains unsafe control or bidirectional characters")]
    UnsafeMetadata {
        /// Dot-style field location.
        field: String,
    },
    /// A bounded manifest metadata field exceeds its hard byte limit.
    #[error("manifest field `{field}` exceeds the {maximum}-byte limit")]
    FieldTooLong {
        /// Dot-style field location.
        field: String,
        /// Non-overridable maximum UTF-8 byte length.
        maximum: usize,
    },
    /// A repository discovery pattern is malformed or unsafe.
    #[error("manifest field `{field}` is invalid: {source}")]
    InvalidIgnorePattern {
        /// Dot-style field location.
        field: String,
        /// Pattern validation failure.
        #[source]
        source: IgnorePatternError,
    },
    /// An extraction budget is zero or cannot be represented internally.
    #[error("invalid workspace manifest: {0}")]
    InvalidExtractionBudget(#[from] InvalidExtractionBudget),
    /// An execution policy value or relationship is invalid.
    #[error("invalid workspace manifest: {0}")]
    InvalidExecutionPolicy(#[from] InvalidExecutionPolicy),
    /// The workspace does not register any repositories.
    #[error("manifest field `repos` must contain at least one repository")]
    EmptyRepositories,
    /// A manual relationship uses the reserved generic relationship kind.
    #[error("manifest field `manualLinks[{index}].relation` must be a concrete edge kind")]
    NonConcreteManualRelation {
        /// Zero-based declaration index.
        index: usize,
    },
    /// A manual relationship names the same literal endpoint twice.
    #[error("manual link at `manualLinks[{index}]` cannot link endpoint `{endpoint}` to itself")]
    ManualSelfLink {
        /// Zero-based declaration index.
        index: usize,
        /// Repeated endpoint literal.
        endpoint: String,
    },
    /// Two manual declarations target the same relationship identity.
    #[error(
        "manual link at `manualLinks[{duplicate}]` duplicates `manualLinks[{first}]` for `{from}` -> `{to}` ({relation:?})"
    )]
    DuplicateManualLink {
        /// Index of the first declaration.
        first: usize,
        /// Index of the repeated declaration.
        duplicate: usize,
        /// Exact source endpoint literal.
        from: String,
        /// Exact target endpoint literal.
        to: String,
        /// Repeated relationship.
        relation: EdgeKind,
    },
}

/// Parses and semantically validates a strict workspace manifest.
///
/// # Errors
///
/// Returns [`ManifestError`] for malformed YAML, unknown keys, unsupported versions, empty
/// fields, or an empty repository registry.
pub fn parse_manifest(input: &str) -> Result<WorkspaceManifest, ManifestError> {
    parse_manifest_with_extensions(input).map(|(manifest, _)| manifest)
}

/// Parses a strict workspace manifest together with additive patch-compatible settings.
///
/// # Errors
///
/// Returns [`ManifestError`] under the same conditions as [`parse_manifest`].
pub fn parse_manifest_with_extensions(
    input: &str,
) -> Result<(WorkspaceManifest, ManifestExtensions), ManifestError> {
    let wire: WorkspaceManifestWire = crate::yaml::from_str(input)?;
    let mut repository_use_gitignore = BTreeMap::new();
    let repos = wire
        .repos
        .into_iter()
        .map(|(alias, repository)| {
            if let Some(value) = repository.use_gitignore {
                repository_use_gitignore.insert(alias.clone(), value);
            }
            (
                alias,
                RepositoryConfig {
                    path: repository.path,
                    openapi: repository.openapi,
                    http_consumers: repository.http_consumers,
                    integration_tests: repository.integration_tests,
                    implementations: repository.implementations,
                    excludes: repository.excludes,
                    include_defaults: repository.include_defaults,
                },
            )
        })
        .collect();
    let (execution_policy, max_codegraph_corroboration_anchors_per_repo) = wire
        .execution_policy
        .map(ExecutionPolicyOverridesWire::into_parts)
        .map_or((None, None), |(policy, limit)| (Some(policy), limit));
    if let Some(value) = max_codegraph_corroboration_anchors_per_repo {
        CodeGraphCorroborationAnchorLimit::try_from(value)?;
    }
    let manifest = WorkspaceManifest {
        version: wire.version,
        name: wire.name,
        allowed_roots: wire.allowed_roots,
        repos,
        manual_links: wire.manual_links,
        extraction_budgets: wire.extraction_budgets,
        execution_policy,
    };
    validate_manifest(manifest).map(|manifest| {
        (
            manifest,
            ManifestExtensions {
                repository_use_gitignore,
                max_codegraph_corroboration_anchors_per_repo,
            },
        )
    })
}

impl ExecutionPolicyOverridesWire {
    fn into_parts(self) -> (ExecutionPolicyOverrides, Option<i64>) {
        (
            ExecutionPolicyOverrides {
                max_scan_wall_time_ms: self.max_scan_wall_time_ms,
                max_no_progress_time_ms: self.max_no_progress_time_ms,
                max_codegraph_sync_wall_time_ms_per_repo: self
                    .max_codegraph_sync_wall_time_ms_per_repo,
                max_worker_memory_bytes: self.max_worker_memory_bytes,
                graceful_termination_ms: self.graceful_termination_ms,
                watch_idle_timeout_ms: self.watch_idle_timeout_ms,
                max_watch_session_wall_time_ms: self.max_watch_session_wall_time_ms,
                min_watch_rescan_interval_ms: self.min_watch_rescan_interval_ms,
                max_checkpoint_cache_bytes: self.max_checkpoint_cache_bytes,
            },
            self.max_codegraph_corroboration_anchors_per_repo,
        )
    }
}

fn validate_manifest(manifest: WorkspaceManifest) -> Result<WorkspaceManifest, ManifestError> {
    if manifest.version != 1 {
        return Err(ManifestError::UnsupportedVersion {
            found: manifest.version,
        });
    }
    validate_not_empty("name", &manifest.name)?;
    for (index, root) in manifest.allowed_roots.iter().enumerate() {
        validate_not_empty(&format!("allowedRoots[{index}]"), root)?;
    }
    if manifest.repos.is_empty() {
        return Err(ManifestError::EmptyRepositories);
    }
    validate_manual_links(&manifest.manual_links)?;
    ExtractionBudgets::resolve(manifest.extraction_budgets.as_ref())?;
    ExecutionPolicy::resolve(manifest.execution_policy.as_ref())?;
    for (alias, repository) in &manifest.repos {
        validate_not_empty(&format!("repos.{alias}"), alias)?;
        validate_not_empty(&format!("repos.{alias}.path"), &repository.path)?;
        if let Some(openapi) = &repository.openapi {
            validate_not_empty(&format!("repos.{alias}.openapi"), openapi)?;
        }
        for (index, consumer) in repository.http_consumers.iter().flatten().enumerate() {
            validate_not_empty(
                &format!("repos.{alias}.httpConsumers[{index}].method"),
                &consumer.method,
            )?;
            validate_not_empty(
                &format!("repos.{alias}.httpConsumers[{index}].path"),
                &consumer.path,
            )?;
            validate_not_empty(
                &format!("repos.{alias}.httpConsumers[{index}].source"),
                &consumer.source,
            )?;
        }
        for (index, test) in repository.integration_tests.iter().flatten().enumerate() {
            for (field, value) in [
                ("name", test.name.as_str()),
                ("path", test.path.as_str()),
                ("framework", test.framework.as_str()),
                ("language", test.language.as_str()),
                ("validates.method", test.validates.method.as_str()),
                ("validates.path", test.validates.path.as_str()),
            ] {
                validate_not_empty(
                    &format!("repos.{alias}.integrationTests[{index}].{field}"),
                    value,
                )?;
            }
        }
        for (index, implementation) in repository.implementations.iter().flatten().enumerate() {
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
                validate_not_empty(
                    &format!("repos.{alias}.implementations[{index}].{field}"),
                    value,
                )?;
            }
        }
        validate_repository_ignore_patterns(alias, repository)?;
    }
    Ok(manifest)
}

fn validate_repository_ignore_patterns(
    alias: &str,
    repository: &RepositoryConfig,
) -> Result<(), ManifestError> {
    if let Some(patterns) = &repository.excludes {
        validate_excludes(patterns).map_err(|source| ManifestError::InvalidIgnorePattern {
            field: format!("repos.{alias}.excludes"),
            source,
        })?;
    }
    if let Some(patterns) = &repository.include_defaults {
        validate_include_defaults(patterns).map_err(|source| {
            ManifestError::InvalidIgnorePattern {
                field: format!("repos.{alias}.includeDefaults"),
                source,
            }
        })?;
    }
    Ok(())
}

/// Validates manually constructed relationship declarations.
///
/// # Errors
///
/// Returns [`ManifestError`] for empty or unsafe metadata, reserved relationship kinds,
/// self-links, or duplicate source/target/relation declarations.
pub fn validate_manual_links(links: &[ManualLinkConfig]) -> Result<(), ManifestError> {
    let mut identities = BTreeMap::new();
    for (index, link) in links.iter().enumerate() {
        validate_not_empty(&format!("manualLinks[{index}].from"), &link.from)?;
        validate_not_empty(&format!("manualLinks[{index}].to"), &link.to)?;
        validate_not_empty(&format!("manualLinks[{index}].reason"), &link.reason)?;
        validate_maximum(
            &format!("manualLinks[{index}].from"),
            &link.from,
            MANUAL_ENDPOINT_MAX_BYTES,
        )?;
        validate_maximum(
            &format!("manualLinks[{index}].to"),
            &link.to,
            MANUAL_ENDPOINT_MAX_BYTES,
        )?;
        validate_maximum(
            &format!("manualLinks[{index}].reason"),
            &link.reason,
            MANUAL_REASON_MAX_BYTES,
        )?;
        if let Some(contract) = &link.contract {
            validate_not_empty(&format!("manualLinks[{index}].contract"), contract)?;
            validate_maximum(
                &format!("manualLinks[{index}].contract"),
                contract,
                MANUAL_CONTRACT_MAX_BYTES,
            )?;
        }
        if link.relation == EdgeKind::ManualLink {
            return Err(ManifestError::NonConcreteManualRelation { index });
        }
        if link.from == link.to {
            return Err(ManifestError::ManualSelfLink {
                index,
                endpoint: link.from.clone(),
            });
        }
        let identity = (link.from.clone(), link.to.clone(), link.relation);
        if let Some(first) = identities.insert(identity, index) {
            return Err(ManifestError::DuplicateManualLink {
                first,
                duplicate: index,
                from: link.from.clone(),
                to: link.to.clone(),
                relation: link.relation,
            });
        }
    }
    Ok(())
}

fn validate_not_empty(field: &str, value: &str) -> Result<(), ManifestError> {
    if value.trim().is_empty() {
        return Err(ManifestError::EmptyField {
            field: field.to_owned(),
        });
    }
    if contains_unsafe_metadata_characters(value) {
        return Err(ManifestError::UnsafeMetadata {
            field: field.to_owned(),
        });
    }
    Ok(())
}

fn validate_maximum(field: &str, value: &str, maximum: usize) -> Result<(), ManifestError> {
    if value.len() > maximum {
        return Err(ManifestError::FieldTooLong {
            field: field.to_owned(),
            maximum,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::EdgeKind;

    use super::{
        MANUAL_REASON_MAX_BYTES, ManifestError, parse_manifest, parse_manifest_with_extensions
    };
    use crate::{ExecutionPolicy, ExtractionBudgets, IgnorePatternError};

    const VALID: &str = r"
version: 1
name: commerce
repos:
  web:
    path: ../web
    httpConsumers:
      - method: POST
        path: /api/orders
        source: src/checkout.ts
";

    #[test]
    fn parse_manifest_should_accept_strict_valid_input() {
        let result = parse_manifest(VALID);

        assert!(result.is_ok(), "unexpected manifest error: {result:?}");
    }

    #[test]
    fn extraction_budgets_should_be_optional_and_resolve_safe_defaults() {
        let manifest = parse_manifest(VALID).expect("manifest without advanced budgets is valid");
        let effective = ExtractionBudgets::resolve(manifest.extraction_budgets.as_ref())
            .expect("safe defaults are valid");

        assert_eq!(effective, ExtractionBudgets::default());
    }

    #[test]
    fn extraction_budgets_should_accept_partial_higher_and_lower_overrides() {
        let input = VALID.replace(
            "name: commerce",
            "name: commerce\nextractionBudgets:\n  maxInputBytesPerArtifact: 1024\n  maxStructuralDepthPerArtifact: 128",
        );
        let manifest = parse_manifest(&input).expect("partial override should be valid");
        let effective = ExtractionBudgets::resolve(manifest.extraction_budgets.as_ref())
            .expect("partial override should resolve");

        assert_eq!(effective.max_input_bytes_per_artifact, 1_024);
        assert_eq!(effective.max_structural_depth_per_artifact, 128);
        assert_eq!(
            effective.max_ast_depth_per_artifact,
            ExtractionBudgets::default().max_ast_depth_per_artifact
        );
    }

    #[test]
    fn extraction_budgets_should_accept_a_complete_override() {
        let input = VALID.replace(
            "name: commerce",
            "name: commerce\nextractionBudgets:\n  maxInputBytesPerArtifact: 1\n  maxStructuralDepthPerArtifact: 2\n  maxAstDepthPerArtifact: 3\n  maxWorkUnitsPerArtifact: 4\n  maxTreeSitterNodesPerArtifact: 5\n  maxObservationsPerArtifact: 6\n  maxAccumulatedStringBytesPerArtifact: 7\n  maxSerializedOutputBytesPerArtifact: 8\n  maxStringBytesPerValue: 9\n  maxPortablePathBytesPerValue: 10\n  maxIdentifierBytesPerValue: 11\n  maxStructuredWallTimeMsPerArtifact: 12\n  maxTreeSitterWallTimeMsPerArtifact: 13",
        );
        let manifest = parse_manifest(&input).expect("complete override should be valid");
        let effective = ExtractionBudgets::resolve(manifest.extraction_budgets.as_ref())
            .expect("complete override should resolve");

        assert_eq!(effective.max_input_bytes_per_artifact, 1);
        assert_eq!(effective.max_structural_depth_per_artifact, 2);
        assert_eq!(effective.max_ast_depth_per_artifact, 3);
        assert_eq!(effective.max_work_units_per_artifact, 4);
        assert_eq!(effective.max_tree_sitter_nodes_per_artifact, 5);
        assert_eq!(effective.max_observations_per_artifact, 6);
        assert_eq!(effective.max_accumulated_string_bytes_per_artifact, 7);
        assert_eq!(effective.max_serialized_output_bytes_per_artifact, 8);
        assert_eq!(effective.max_string_bytes_per_value, 9);
        assert_eq!(effective.max_portable_path_bytes_per_value, 10);
        assert_eq!(effective.max_identifier_bytes_per_value, 11);
        assert_eq!(effective.max_structured_wall_time_ms_per_artifact, 12);
        assert_eq!(effective.max_tree_sitter_wall_time_ms_per_artifact, 13);
    }

    #[test]
    fn extraction_budgets_should_reject_zero_overflow_and_unknown_fields() {
        let zero = VALID.replace(
            "name: commerce",
            "name: commerce\nextractionBudgets:\n  maxWorkUnitsPerArtifact: 0",
        );
        let overflow = VALID.replace(
            "name: commerce",
            "name: commerce\nextractionBudgets:\n  maxWorkUnitsPerArtifact: 18446744073709551616",
        );
        let unknown = VALID.replace(
            "name: commerce",
            "name: commerce\nextractionBudgets:\n  maxWorkUnitsPerArtifact: 1\n  maximumMagic: 2",
        );

        assert!(matches!(
            parse_manifest(&zero),
            Err(ManifestError::InvalidExtractionBudget(_))
        ));
        assert!(matches!(
            parse_manifest(&overflow),
            Err(ManifestError::InvalidYaml(_))
        ));
        assert!(matches!(
            parse_manifest(&unknown),
            Err(ManifestError::InvalidYaml(_))
        ));
    }

    #[test]
    fn execution_policy_should_resolve_defaults_and_partial_overrides() {
        let manifest = parse_manifest(VALID).expect("manifest without execution policy is valid");
        assert_eq!(
            ExecutionPolicy::resolve(manifest.execution_policy.as_ref()).expect("defaults"),
            ExecutionPolicy::default()
        );

        let input = VALID.replace(
            "name: commerce",
            "name: commerce\nexecutionPolicy:\n  maxScanWallTimeMs: 28800000\n  maxNoProgressTimeMs: 600000",
        );
        let manifest = parse_manifest(&input).expect("partial policy override is valid");
        let effective = ExecutionPolicy::resolve(manifest.execution_policy.as_ref())
            .expect("partial policy resolves");
        assert_eq!(effective.max_scan_wall_time_ms, 28_800_000);
        assert_eq!(effective.max_no_progress_time_ms, 600_000);
        assert_eq!(
            effective.max_worker_memory_bytes,
            ExecutionPolicy::default().max_worker_memory_bytes
        );
    }

    #[test]
    fn execution_policy_should_reject_zero_overflow_unknown_and_invalid_relationships() {
        let zero = VALID.replace(
            "name: commerce",
            "name: commerce\nexecutionPolicy:\n  maxWorkerMemoryBytes: 0",
        );
        let overflow = VALID.replace(
            "name: commerce",
            "name: commerce\nexecutionPolicy:\n  maxWorkerMemoryBytes: 18446744073709551616",
        );
        let unknown = VALID.replace(
            "name: commerce",
            "name: commerce\nexecutionPolicy:\n  maximumMagic: 1",
        );
        let invalid = VALID.replace(
            "name: commerce",
            "name: commerce\nexecutionPolicy:\n  maxScanWallTimeMs: 1000\n  maxNoProgressTimeMs: 1001\n  maxCodeGraphSyncWallTimeMsPerRepo: 1000\n  gracefulTerminationMs: 1",
        );
        let invalid_anchor_limit = VALID.replace(
            "name: commerce",
            "name: commerce\nexecutionPolicy:\n  maxCodeGraphCorroborationAnchorsPerRepo: -2",
        );

        assert!(matches!(
            parse_manifest(&zero),
            Err(ManifestError::InvalidExecutionPolicy(_))
        ));
        assert!(matches!(
            parse_manifest(&overflow),
            Err(ManifestError::InvalidYaml(_))
        ));
        assert!(matches!(
            parse_manifest(&unknown),
            Err(ManifestError::InvalidYaml(_))
        ));
        assert!(matches!(
            parse_manifest(&invalid_anchor_limit),
            Err(ManifestError::InvalidExecutionPolicy(_))
        ));
        let invalid_result = parse_manifest(&invalid);
        assert!(
            matches!(
                invalid_result,
                Err(ManifestError::InvalidExecutionPolicy(_))
            ),
            "unexpected invalid relationship result: {invalid_result:?}"
        );
    }

    #[test]
    fn parse_manifest_should_accept_repository_ignore_patterns() {
        let input = VALID.replace(
            "    path: ../web",
            "    path: ../web\n    excludes: [coverage/**]\n    includeDefaults: [vendor/internal/**]",
        );

        let result = parse_manifest(&input);

        assert!(result.is_ok(), "unexpected manifest error: {result:?}");
    }

    #[test]
    fn parse_manifest_should_keep_additive_settings_out_of_public_structs() {
        let input = VALID.replace(
            "name: commerce",
            "name: commerce\nexecutionPolicy:\n  maxCodeGraphCorroborationAnchorsPerRepo: 12",
        );
        let input = input.replace(
            "    path: ../web",
            "    path: ../web\n    useGitignore: true",
        );

        let (manifest, extensions) =
            parse_manifest_with_extensions(&input).expect("additive settings are valid");

        assert!(manifest.execution_policy.is_some());
        assert_eq!(extensions.repository_use_gitignore("web"), Some(true));
        assert_eq!(
            extensions.max_codegraph_corroboration_anchors_per_repo(),
            Some(12)
        );
    }

    #[test]
    fn parse_manifest_should_reject_protected_default_include() {
        let input = VALID.replace(
            "    path: ../web",
            "    path: ../web\n    includeDefaults: [.codegraph/**]",
        );

        let result = parse_manifest(&input);

        assert!(matches!(
            result,
            Err(ManifestError::InvalidIgnorePattern { field, .. })
                if field == "repos.web.includeDefaults"
        ));
    }

    #[test]
    fn parse_manifest_should_reject_unsupported_ignore_syntax() {
        let input = VALID.replace(
            "    path: ../web",
            "    path: ../web\n    excludes: [\"src/[ab]/**\"]",
        );

        let result = parse_manifest(&input);

        assert!(matches!(
            result,
            Err(ManifestError::InvalidIgnorePattern {
                source: IgnorePatternError::UnsupportedSyntax(_),
                ..
            })
        ));
    }

    #[test]
    fn parse_manifest_should_reject_unknown_keys() {
        let result =
            parse_manifest(&VALID.replace("name: commerce", "name: commerce\nextra: true"));

        assert!(matches!(result, Err(ManifestError::InvalidYaml(_))));
    }

    #[test]
    fn parse_manifest_should_reject_unsupported_version() {
        let result = parse_manifest(&VALID.replace("version: 1", "version: 2"));

        assert!(matches!(
            result,
            Err(ManifestError::UnsupportedVersion { found: 2 })
        ));
    }

    #[test]
    fn parse_manifest_should_reject_bidi_metadata() {
        let result = parse_manifest(&VALID.replace("commerce", "commerce\u{202e}txt"));

        assert!(matches!(
            result,
            Err(ManifestError::UnsafeMetadata { field }) if field == "name"
        ));
    }

    #[test]
    fn parse_manifest_should_accept_exact_manual_link() {
        let input = format!(
            "{VALID}manualLinks:\n  - from: node:web\n    to: service:api\n    relation: consumes\n    contract: POST /orders\n    reason: Manual checkout boundary\n"
        );

        let result = parse_manifest(&input).map(|manifest| manifest.manual_links);

        assert!(matches!(
            result,
            Ok(links)
                if links.len() == 1
                    && links[0].relation == EdgeKind::Consumes
                    && !links[0].suppress
        ));
    }

    #[test]
    fn parse_manifest_should_reject_unknown_manual_link_fields() {
        let input = format!(
            "{VALID}manualLinks:\n  - from: node:web\n    to: service:api\n    relation: consumes\n    reason: Explicit dependency\n    approximate: true\n"
        );

        let result = parse_manifest(&input);

        assert!(matches!(result, Err(ManifestError::InvalidYaml(_))));
    }

    #[test]
    fn parse_manifest_should_reject_empty_manual_link_reason() {
        let input = format!(
            "{VALID}manualLinks:\n  - from: node:web\n    to: service:api\n    relation: consumes\n    reason: '   '\n"
        );

        let result = parse_manifest(&input);

        assert!(matches!(
            result,
            Err(ManifestError::EmptyField { field })
                if field == "manualLinks[0].reason"
        ));
    }

    #[test]
    fn parse_manifest_should_reject_oversized_manual_link_reason() {
        let input = format!(
            "{VALID}manualLinks:\n  - from: node:web\n    to: service:api\n    relation: consumes\n    reason: {}\n",
            "x".repeat(MANUAL_REASON_MAX_BYTES + 1)
        );

        let result = parse_manifest(&input);

        assert!(matches!(
            result,
            Err(ManifestError::FieldTooLong { field, maximum })
                if field == "manualLinks[0].reason" && maximum == MANUAL_REASON_MAX_BYTES
        ));
    }

    #[test]
    fn parse_manifest_should_reject_unsafe_manual_link_metadata() {
        let input = format!(
            "{VALID}manualLinks:\n  - from: node:web\u{202e}\n    to: service:api\n    relation: consumes\n    reason: Explicit dependency\n"
        );

        let result = parse_manifest(&input);

        assert!(matches!(
            result,
            Err(ManifestError::UnsafeMetadata { field })
                if field == "manualLinks[0].from"
        ));
    }

    #[test]
    fn parse_manifest_should_reject_duplicate_manual_link_identity() {
        let declaration = "  - from: node:web\n    to: service:api\n    relation: consumes\n    reason: Explicit dependency\n";
        let input = format!("{VALID}manualLinks:\n{declaration}{declaration}");

        let result = parse_manifest(&input);

        assert!(matches!(
            result,
            Err(ManifestError::DuplicateManualLink {
                first: 0,
                duplicate: 1,
                ..
            })
        ));
    }

    #[test]
    fn parse_manifest_should_reject_literal_manual_self_link() {
        let input = format!(
            "{VALID}manualLinks:\n  - from: node:web\n    to: node:web\n    relation: consumes\n    reason: Invalid self link\n"
        );

        let result = parse_manifest(&input);

        assert!(matches!(
            result,
            Err(ManifestError::ManualSelfLink { index: 0, .. })
        ));
    }
}

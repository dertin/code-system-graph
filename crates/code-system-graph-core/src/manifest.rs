use std::collections::BTreeMap;

use code_system_graph_model::{EdgeKind, contains_unsafe_metadata_characters};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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
    let manifest: WorkspaceManifest = crate::yaml::from_str(input)?;
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

    use super::{MANUAL_REASON_MAX_BYTES, ManifestError, parse_manifest};
    use crate::IgnorePatternError;

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
    fn parse_manifest_should_accept_repository_ignore_patterns() {
        let input = VALID.replace(
            "    path: ../web",
            "    path: ../web\n    excludes: [coverage/**]\n    includeDefaults: [vendor/internal/**]",
        );

        let result = parse_manifest(&input);

        assert!(result.is_ok(), "unexpected manifest error: {result:?}");
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

use std::time::Instant;

use async_trait::async_trait;
use code_system_graph_model::ArtifactFingerprint;
use semver::Version;
use serde::Serialize;

use crate::{
    BoundaryExtractor, ContentFingerprint, DiscoverContext, DiscoveredInput, ExtractInput, ExtractionBatch, ExtractionBudgets, ExtractionCompleteness, ExtractionReport, ExtractionTracker, ExtractorError, FileDescriptor, SourceEpistemicStatus, SourceObservation, SourceSyntaxLanguage, extract_generated_client_metadata, extract_package_manifest_with_tracker, inspect_source_syntax, parse_go_source, parse_java_source, parse_javascript_source_at_path, parse_python_source, parse_rust_source, parse_typescript_source_at_path
};

/// Focused source language selected for HTTP and test extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusedSourceLanguage {
    /// JavaScript framework syntax.
    JavaScript,
    /// TypeScript framework syntax.
    TypeScript,
    /// Rust framework syntax.
    Rust,
    /// Python framework syntax.
    Python,
    /// Go framework syntax.
    Go,
    /// Java framework syntax.
    Java,
}

/// Built-in focused source HTTP and test extractor.
pub struct FocusedSourceExtractor {
    language: FocusedSourceLanguage,
    budgets: ExtractionBudgets,
}

impl FocusedSourceExtractor {
    /// Creates an extractor for one mandatory source-language matrix.
    #[must_use]
    pub fn new(language: FocusedSourceLanguage) -> Self {
        Self::with_budgets(language, ExtractionBudgets::default())
    }

    /// Creates an extractor with explicit effective per-invocation budgets.
    #[must_use]
    pub fn with_budgets(language: FocusedSourceLanguage, budgets: ExtractionBudgets) -> Self {
        Self { language, budgets }
    }
}

#[async_trait]
impl BoundaryExtractor for FocusedSourceExtractor {
    fn id(&self) -> &'static str {
        match self.language {
            FocusedSourceLanguage::JavaScript => "code-system-graph.source.javascript",
            FocusedSourceLanguage::TypeScript => "code-system-graph.source.typescript",
            FocusedSourceLanguage::Rust => "code-system-graph.source.rust",
            FocusedSourceLanguage::Python => "code-system-graph.source.python",
            FocusedSourceLanguage::Go => "code-system-graph.source.go",
            FocusedSourceLanguage::Java => "code-system-graph.source.java",
        }
    }

    fn version(&self) -> Version {
        Version::new(1, 0, 0)
    }

    fn supports(&self, file: &FileDescriptor) -> bool {
        let extension = file
            .path
            .display
            .rsplit_once('.')
            .map(|(_, extension)| extension);
        matches!(
            (self.language, extension),
            (FocusedSourceLanguage::JavaScript, Some("js" | "jsx"))
                | (FocusedSourceLanguage::TypeScript, Some("ts" | "tsx"))
                | (FocusedSourceLanguage::Rust, Some("rs"))
                | (FocusedSourceLanguage::Python, Some("py"))
                | (FocusedSourceLanguage::Go, Some("go"))
                | (FocusedSourceLanguage::Java, Some("java"))
        )
    }

    async fn discover(
        &self,
        context: &DiscoverContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, ExtractorError> {
        Ok(context
            .files
            .iter()
            .filter(|file| self.supports(file))
            .cloned()
            .map(|file| DiscoveredInput { file })
            .collect())
    }

    async fn extract(&self, input: &ExtractInput<'_>) -> Result<ExtractionBatch, ExtractorError> {
        if !self.supports(input.file) {
            return Err(ExtractorError::InvalidInput(format!(
                "{} does not support `{}`",
                self.id(),
                input.file.path.display
            )));
        }
        let started = Instant::now();
        let mut tracker =
            crate::ExtractionTracker::new(&input.file.path.display, self.id(), &self.budgets);
        tracker.check_input_bytes(u64::try_from(input.content.len()).unwrap_or(u64::MAX))?;
        let fingerprint = fingerprint_with_budgets(input, self.id(), &self.budgets)?;
        let source = std::str::from_utf8(input.content)?;
        let syntax = inspect_source_syntax(
            syntax_language(self.language),
            &input.file.path.display,
            source,
            &mut tracker,
        )
        .map_err(extractor_source_syntax_error)?;
        let reserved_observations = u64::try_from(syntax.boundary_candidate_count)
            .map_err(|_| ExtractorError::InvalidInput("too many syntax candidates".to_owned()))?;
        tracker.check_observations(reserved_observations)?;
        tracker.charge_work(reserved_observations)?;
        precheck_focused_source_values(source, &mut tracker)?;
        let observations = match self.language {
            FocusedSourceLanguage::JavaScript => {
                parse_javascript_source_at_path(&input.file.path.display, source)
            }
            FocusedSourceLanguage::TypeScript => {
                parse_typescript_source_at_path(&input.file.path.display, source)
            }
            FocusedSourceLanguage::Rust => parse_rust_source(source),
            FocusedSourceLanguage::Python => parse_python_source(source),
            FocusedSourceLanguage::Go => parse_go_source(source),
            FocusedSourceLanguage::Java => parse_java_source(source),
        };
        if !observations.is_empty() && syntax.boundary_candidate_count == 0 {
            return Err(ExtractorError::InvalidInput(
                "framework observation lacked a Tree-sitter boundary candidate".to_owned(),
            ));
        }
        let completeness = if !syntax.has_error
            && observations
                .iter()
                .all(|observation| observation.status == SourceEpistemicStatus::Confirmed)
        {
            ExtractionCompleteness::Complete
        } else {
            ExtractionCompleteness::Partial
        };
        let mut warnings = observations
            .iter()
            .flat_map(|observation| &observation.warnings)
            .map(|warning| format!("{warning:?}"))
            .collect::<Vec<_>>();
        if syntax.has_error {
            warnings.push("Tree-sitter recovered from source syntax errors".to_owned());
        }
        let output_count = u64::try_from(observations.len())
            .map_err(|_| ExtractorError::InvalidInput("too many observations".to_owned()))?;
        if output_count > reserved_observations {
            return Err(ExtractorError::InvalidInput(
                "focused parser exceeded its Tree-sitter candidate reservation".to_owned(),
            ));
        }
        tracker.charge_observation(output_count)?;
        for observation in &observations {
            charge_source_observation(observation, &mut tracker)?;
        }
        let payload = serialize_bounded(&observations, &tracker)?;
        tracker.check_structured_time()?;
        Ok(ExtractionBatch {
            source: ArtifactFingerprint {
                repo_id: input.file.repo_id.clone(),
                checkout_id: input.file.checkout_id.clone(),
                path: input.file.path.clone(),
                extractor: self.id().to_owned(),
                content_hash: fingerprint.content_hash,
                size_bytes: fingerprint.size_bytes,
            },
            payload,
            output_count,
            report: ExtractionReport {
                discovered_files: 1,
                parsed_files: 1,
                skipped_files: 0,
                completeness,
                warnings,
                evidence_count: output_count,
                extractor_version: self.version(),
                elapsed_ms: elapsed_ms(started),
            },
        })
    }

    async fn fingerprint(
        &self,
        input: &ExtractInput<'_>,
    ) -> Result<crate::ContentFingerprint, ExtractorError> {
        fingerprint_with_budgets(input, self.id(), &self.budgets)
    }
}

fn syntax_language(language: FocusedSourceLanguage) -> SourceSyntaxLanguage {
    match language {
        FocusedSourceLanguage::JavaScript => SourceSyntaxLanguage::JavaScript,
        FocusedSourceLanguage::TypeScript => SourceSyntaxLanguage::TypeScript,
        FocusedSourceLanguage::Rust => SourceSyntaxLanguage::Rust,
        FocusedSourceLanguage::Python => SourceSyntaxLanguage::Python,
        FocusedSourceLanguage::Go => SourceSyntaxLanguage::Go,
        FocusedSourceLanguage::Java => SourceSyntaxLanguage::Java,
    }
}

/// Preflights source tokens before focused parsers allocate owned observation fields.
///
/// # Errors
///
/// Returns [`crate::ExtractionLimitExceeded`] when work, value, accumulated-string, or time
/// budgets are exceeded.
#[doc(hidden)]
pub fn precheck_focused_source_values(
    source: &str,
    tracker: &mut ExtractionTracker,
) -> Result<(), crate::ExtractionLimitExceeded> {
    let bytes = source.as_bytes();
    let mut cursor = 0_usize;
    let mut accumulated = 0_u64;
    while cursor < bytes.len() {
        if cursor.is_multiple_of(1_024) {
            tracker.check_structured_time()?;
        }
        let byte = bytes[cursor];
        if matches!(byte, b'"' | b'\'' | b'`') {
            tracker.charge_work(1)?;
            let delimiter = byte;
            cursor = cursor.saturating_add(1);
            let start = cursor;
            let mut escaped = false;
            while cursor < bytes.len() {
                let byte = bytes[cursor];
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == delimiter {
                    break;
                }
                cursor = cursor.saturating_add(1);
                if cursor.is_multiple_of(1_024) {
                    tracker.check_structured_time()?;
                }
            }
            let observed = u64::try_from(cursor.saturating_sub(start)).unwrap_or(u64::MAX);
            tracker.check_string_bytes(observed)?;
            accumulated = accumulated.saturating_add(observed);
            tracker.check_accumulated_string_bytes(accumulated)?;
        } else if byte == b'_' || byte.is_ascii_alphabetic() {
            tracker.charge_work(1)?;
            let start = cursor;
            cursor = cursor.saturating_add(1);
            while cursor < bytes.len()
                && (bytes[cursor] == b'_' || bytes[cursor].is_ascii_alphanumeric())
            {
                cursor = cursor.saturating_add(1);
            }
            let observed = u64::try_from(cursor.saturating_sub(start)).unwrap_or(u64::MAX);
            tracker.check_identifier_bytes(observed)?;
            accumulated = accumulated.saturating_add(observed);
            tracker.check_accumulated_string_bytes(accumulated)?;
            continue;
        }
        cursor = cursor.saturating_add(1);
    }
    Ok(())
}

/// Built-in `OpenAPI` Generator metadata extractor.
pub struct GeneratedClientMetadataExtractor {
    budgets: ExtractionBudgets,
}

impl GeneratedClientMetadataExtractor {
    /// Creates an extractor with safe default budgets.
    #[must_use]
    pub fn new() -> Self {
        Self::with_budgets(ExtractionBudgets::default())
    }

    /// Creates an extractor with explicit effective per-invocation budgets.
    #[must_use]
    pub const fn with_budgets(budgets: ExtractionBudgets) -> Self {
        Self { budgets }
    }
}

impl Default for GeneratedClientMetadataExtractor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BoundaryExtractor for GeneratedClientMetadataExtractor {
    fn id(&self) -> &'static str {
        "code-system-graph.http.generated-client"
    }

    fn version(&self) -> Version {
        Version::new(1, 0, 0)
    }

    fn supports(&self, file: &FileDescriptor) -> bool {
        generated_client_path_supported(&file.path.display)
    }

    async fn discover(
        &self,
        context: &DiscoverContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, ExtractorError> {
        Ok(context
            .files
            .iter()
            .filter(|file| self.supports(file))
            .cloned()
            .map(|file| DiscoveredInput { file })
            .collect())
    }

    async fn extract(&self, input: &ExtractInput<'_>) -> Result<ExtractionBatch, ExtractorError> {
        if !self.supports(input.file) {
            return Err(ExtractorError::InvalidInput(format!(
                "{} does not support `{}`",
                self.id(),
                input.file.path.display
            )));
        }
        let started = Instant::now();
        let mut tracker =
            crate::ExtractionTracker::new(&input.file.path.display, self.id(), &self.budgets);
        tracker.check_input_bytes(u64::try_from(input.content.len()).unwrap_or(u64::MAX))?;
        let fingerprint = fingerprint_with_budgets(input, self.id(), &self.budgets)?;
        let source = std::str::from_utf8(input.content)?;
        let metadata =
            extract_generated_client_metadata(&input.file.path.display, source, &mut tracker)
                .map_err(|error| match error {
                    crate::GeneratedClientError::LimitExceeded(limit) => {
                        ExtractorError::LimitExceeded(limit)
                    }
                    error => ExtractorError::InvalidInput(error.to_string()),
                })?;
        let output_count = u64::try_from(metadata.len())
            .map_err(|_| ExtractorError::InvalidInput("too many metadata facts".to_owned()))?;
        let payload = serialize_bounded(&metadata, &tracker)?;
        tracker.check_structured_time()?;
        Ok(ExtractionBatch {
            source: ArtifactFingerprint {
                repo_id: input.file.repo_id.clone(),
                checkout_id: input.file.checkout_id.clone(),
                path: input.file.path.clone(),
                extractor: self.id().to_owned(),
                content_hash: fingerprint.content_hash,
                size_bytes: fingerprint.size_bytes,
            },
            payload,
            output_count,
            report: ExtractionReport {
                discovered_files: 1,
                parsed_files: 1,
                skipped_files: 0,
                completeness: ExtractionCompleteness::Complete,
                warnings: Vec::new(),
                evidence_count: output_count,
                extractor_version: self.version(),
                elapsed_ms: elapsed_ms(started),
            },
        })
    }

    async fn fingerprint(
        &self,
        input: &ExtractInput<'_>,
    ) -> Result<crate::ContentFingerprint, ExtractorError> {
        fingerprint_with_budgets(input, self.id(), &self.budgets)
    }
}

/// Built-in package-manifest extractor for the mandatory package ecosystems.
pub struct PackageManifestExtractor {
    budgets: ExtractionBudgets,
}

impl PackageManifestExtractor {
    /// Creates an extractor with safe default budgets.
    #[must_use]
    pub fn new() -> Self {
        Self::with_budgets(ExtractionBudgets::default())
    }

    /// Creates an extractor with explicit effective per-invocation budgets.
    #[must_use]
    pub const fn with_budgets(budgets: ExtractionBudgets) -> Self {
        Self { budgets }
    }
}

impl Default for PackageManifestExtractor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl BoundaryExtractor for PackageManifestExtractor {
    fn id(&self) -> &'static str {
        "code-system-graph.packages"
    }

    fn version(&self) -> Version {
        Version::new(1, 0, 0)
    }

    fn supports(&self, file: &FileDescriptor) -> bool {
        package_path_supported(&file.path.display)
    }

    async fn discover(
        &self,
        context: &DiscoverContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, ExtractorError> {
        Ok(context
            .files
            .iter()
            .filter(|file| self.supports(file))
            .cloned()
            .map(|file| DiscoveredInput { file })
            .collect())
    }

    async fn extract(&self, input: &ExtractInput<'_>) -> Result<ExtractionBatch, ExtractorError> {
        if !self.supports(input.file) {
            return Err(ExtractorError::InvalidInput(format!(
                "{} does not support `{}`",
                self.id(),
                input.file.path.display
            )));
        }
        let started = Instant::now();
        let mut tracker =
            crate::ExtractionTracker::new(&input.file.path.display, self.id(), &self.budgets);
        tracker.check_input_bytes(u64::try_from(input.content.len()).unwrap_or(u64::MAX))?;
        let fingerprint = fingerprint_with_budgets(input, self.id(), &self.budgets)?;
        let source = std::str::from_utf8(input.content)?;
        let manifest =
            extract_package_manifest_with_tracker(&input.file.path.display, source, &mut tracker)
                .map_err(|error| match error {
                crate::PackageManifestError::LimitExceeded(limit) => {
                    ExtractorError::LimitExceeded(limit)
                }
                error => ExtractorError::InvalidInput(error.to_string()),
            })?;
        let output_count = [
            manifest.packages.len(),
            manifest.dependencies.len(),
            manifest.workspace_members.len(),
            manifest.exports.len(),
            manifest.features.len(),
            manifest.lockfiles.len(),
        ]
        .into_iter()
        .try_fold(0_u64, |total, count| {
            u64::try_from(count)
                .ok()
                .and_then(|count| total.checked_add(count))
        })
        .ok_or_else(|| ExtractorError::InvalidInput("too many package facts".to_owned()))?;
        let payload = serialize_bounded(&manifest, &tracker)?;
        tracker.check_structured_time()?;
        Ok(ExtractionBatch {
            source: ArtifactFingerprint {
                repo_id: input.file.repo_id.clone(),
                checkout_id: input.file.checkout_id.clone(),
                path: input.file.path.clone(),
                extractor: self.id().to_owned(),
                content_hash: fingerprint.content_hash,
                size_bytes: fingerprint.size_bytes,
            },
            payload,
            output_count,
            report: ExtractionReport {
                discovered_files: 1,
                parsed_files: 1,
                skipped_files: 0,
                completeness: ExtractionCompleteness::Complete,
                warnings: Vec::new(),
                evidence_count: output_count,
                extractor_version: self.version(),
                elapsed_ms: elapsed_ms(started),
            },
        })
    }

    async fn fingerprint(
        &self,
        input: &ExtractInput<'_>,
    ) -> Result<crate::ContentFingerprint, ExtractorError> {
        fingerprint_with_budgets(input, self.id(), &self.budgets)
    }
}

fn extractor_source_syntax_error(error: crate::SourceSyntaxError) -> ExtractorError {
    match error {
        crate::SourceSyntaxError::LimitExceeded(limit) => ExtractorError::LimitExceeded(limit),
        error => ExtractorError::InvalidInput(error.to_string()),
    }
}

fn fingerprint_with_budgets(
    input: &ExtractInput<'_>,
    extractor: &str,
    budgets: &ExtractionBudgets,
) -> Result<ContentFingerprint, ExtractorError> {
    let observed = u64::try_from(input.content.len()).unwrap_or(u64::MAX);
    ExtractionTracker::new(&input.file.path.display, extractor, budgets)
        .check_input_bytes(observed)?;
    Ok(ContentFingerprint {
        content_hash: blake3::hash(input.content).to_hex().to_string(),
        size_bytes: observed,
    })
}

/// Charges every retained value in one focused source observation.
///
/// # Errors
///
/// Returns [`crate::ExtractionLimitExceeded`] before retaining values beyond effective budgets.
#[doc(hidden)]
pub fn charge_source_observation(
    observation: &SourceObservation,
    tracker: &mut ExtractionTracker,
) -> Result<(), crate::ExtractionLimitExceeded> {
    if let Some(method) = &observation.method {
        tracker.charge_identifier(method)?;
    }
    if let Some(path) = &observation.path {
        tracker.charge_portable_path(path)?;
    }
    for symbol in [
        observation.symbol_name.as_deref(),
        observation.related_symbol.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        tracker.charge_identifier(symbol)?;
    }
    if let Some(path) = &observation.related_path {
        tracker.charge_portable_path(path)?;
    }
    Ok(())
}

fn serialize_bounded<T: Serialize>(
    value: &T,
    tracker: &ExtractionTracker,
) -> Result<Vec<u8>, ExtractorError> {
    let mut writer = tracker.bounded_json_writer();
    if let Err(error) = serde_json::to_writer(&mut writer, value) {
        if let Some(limit) = tracker.output_limit_error(&writer) {
            return Err(ExtractorError::LimitExceeded(limit));
        }
        return Err(ExtractorError::InvalidOutput(error));
    }
    Ok(writer.into_inner())
}

fn package_path_supported(path: &str) -> bool {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    matches!(
        name,
        "package.json"
            | "package-lock.json"
            | "npm-shrinkwrap.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "pyproject.toml"
            | "Cargo.toml"
            | "go.mod"
            | "go.work"
            | "pom.xml"
            | "build.gradle"
            | "build.gradle.kts"
            | "packages.config"
    ) || (name.starts_with("requirements")
        && std::path::Path::new(name)
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt")))
        || name.to_ascii_lowercase().ends_with(".csproj")
}

fn generated_client_path_supported(path: &str) -> bool {
    let path = std::path::Path::new(path);
    let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
        return false;
    };
    name == "openapitools.json"
        || (matches!(name, "FILES" | "VERSION")
            && path
                .parent()
                .and_then(std::path::Path::file_name)
                .is_some_and(|parent| parent == ".openapi-generator"))
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{CheckoutId, NativePath, NativePathEncoding, RepoId};

    use super::{
        BoundaryExtractor, ExtractInput, ExtractionBudgets, ExtractorError, FileDescriptor, FocusedSourceExtractor, FocusedSourceLanguage, GeneratedClientMetadataExtractor, PackageManifestExtractor
    };

    fn file(path: &str) -> FileDescriptor {
        FileDescriptor {
            repo_id: RepoId::new("repo:test"),
            checkout_id: CheckoutId::new("checkout:test"),
            path: NativePath {
                encoding: NativePathEncoding::Utf8,
                bytes: path.as_bytes().to_vec(),
                display: path.to_owned(),
            },
            size_bytes: 1,
        }
    }

    #[tokio::test]
    async fn source_extractor_should_emit_versioned_json_without_source_text() {
        let extractor = FocusedSourceExtractor::new(FocusedSourceLanguage::Rust);
        let file = file("src/routes.rs");
        let source =
            b"use axum::{Router, routing::get}; Router::new().route(\"/health\", get(health));";
        let result = extractor
            .extract(&ExtractInput {
                file: &file,
                content: source,
            })
            .await;

        assert!(matches!(
            result,
            Ok(batch)
                if batch.output_count == 1
                    && !String::from_utf8_lossy(&batch.payload).contains("Router::new")
        ));
    }

    #[tokio::test]
    async fn source_extractor_should_preflight_work_observations_and_values() {
        let file = file("src/routes.rs");
        let source =
            b"use axum::{Router, routing::get}; Router::new().route(\"/health\", get(health));";
        let cases = [
            (
                ExtractionBudgets {
                    max_work_units_per_artifact: 1,
                    ..ExtractionBudgets::default()
                },
                crate::ExtractionResource::WorkUnits,
            ),
            (
                ExtractionBudgets {
                    max_observations_per_artifact: 1,
                    ..ExtractionBudgets::default()
                },
                crate::ExtractionResource::Observations,
            ),
            (
                ExtractionBudgets {
                    max_string_bytes_per_value: 3,
                    ..ExtractionBudgets::default()
                },
                crate::ExtractionResource::StringBytesPerValue,
            ),
        ];

        for (budgets, resource) in cases {
            let extractor =
                FocusedSourceExtractor::with_budgets(FocusedSourceLanguage::Rust, budgets);
            let result = extractor
                .extract(&ExtractInput {
                    file: &file,
                    content: source,
                })
                .await;
            assert!(matches!(
                result,
                Err(ExtractorError::LimitExceeded(error)) if error.resource == resource
            ));
        }
    }

    #[tokio::test]
    async fn javascript_extractor_should_preserve_next_file_route_context() {
        let extractor = FocusedSourceExtractor::new(FocusedSourceLanguage::JavaScript);
        let file = file("frontend/src/app/api/logout/route.js");
        let source = b"export async function GET() { return new Response(); }\n";
        let batch = extractor
            .extract(&ExtractInput {
                file: &file,
                content: source,
            })
            .await
            .expect("Next.js route should extract");
        let observations: Vec<crate::SourceObservation> =
            serde_json::from_slice(&batch.payload).expect("valid observation payload");

        assert!(observations.iter().any(|observation| {
            observation.framework == crate::SourceFramework::NextJs
                && observation.method.as_deref() == Some("GET")
                && observation.path.as_deref() == Some("/api/logout")
        }));
    }

    #[tokio::test]
    async fn generated_client_extractor_should_emit_explicit_config_facts() {
        let extractor = GeneratedClientMetadataExtractor::default();
        let file = file("openapitools.json");
        let result = extractor
            .extract(&ExtractInput {
                file: &file,
                content: br#"{"generator-cli":{"generators":{"client":{"generatorName":"rust","inputSpec":"openapi.yaml"}}}}"#,
            })
            .await;

        assert!(matches!(result, Ok(batch) if batch.output_count == 1));
    }

    #[tokio::test]
    async fn package_extractor_should_omit_evidence_source_lines_from_payload() {
        let extractor = PackageManifestExtractor::default();
        let file = file("Cargo.toml");
        let source = b"[package]\nname = \"api\"\nversion = \"1.0.0\"\n";
        let result = extractor
            .extract(&ExtractInput {
                file: &file,
                content: source,
            })
            .await;

        assert!(matches!(
            result,
            Ok(batch)
                if batch.output_count == 1
                    && !String::from_utf8_lossy(&batch.payload).contains("name =")
        ));
    }

    #[tokio::test]
    async fn built_in_extractors_should_honor_explicit_effective_budgets() {
        let budgets = ExtractionBudgets {
            max_serialized_output_bytes_per_artifact: 1,
            ..ExtractionBudgets::default()
        };
        let extractor = GeneratedClientMetadataExtractor::with_budgets(budgets);
        let file = file("openapitools.json");
        let result = extractor
            .extract(&ExtractInput {
                file: &file,
                content: br#"{"generator-cli":{"generators":{"client":{"generatorName":"rust"}}}}"#,
            })
            .await;

        assert!(matches!(
            result,
            Err(ExtractorError::LimitExceeded(error))
                if error.resource
                    == crate::ExtractionResource::SerializedOutputBytes
                    && error.maximum == 1
        ));
    }
}

use std::time::Instant;

use async_trait::async_trait;
use code_system_graph_model::ArtifactFingerprint;
use semver::Version;

use crate::{
    BoundaryExtractor, DiscoverContext, DiscoveredInput, ExtractInput, ExtractionBatch, ExtractionCompleteness, ExtractionReport, ExtractorError, FileDescriptor, SourceEpistemicStatus, SourceSyntaxLanguage, extract_generated_client_metadata, extract_package_manifest, fingerprint_content, inspect_source_syntax, parse_go_source, parse_java_source, parse_javascript_source_at_path, parse_python_source, parse_rust_source, parse_typescript_source_at_path
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
}

impl FocusedSourceExtractor {
    /// Creates an extractor for one mandatory source-language matrix.
    #[must_use]
    pub fn new(language: FocusedSourceLanguage) -> Self {
        Self { language }
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
        let fingerprint = fingerprint_content(input.content)?;
        let source = std::str::from_utf8(input.content)?;
        let syntax = inspect_source_syntax(
            syntax_language(self.language),
            &input.file.path.display,
            source,
        )
        .map_err(|error| ExtractorError::InvalidInput(error.to_string()))?;
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
        Ok(ExtractionBatch {
            source: ArtifactFingerprint {
                repo_id: input.file.repo_id.clone(),
                checkout_id: input.file.checkout_id.clone(),
                path: input.file.path.clone(),
                extractor: self.id().to_owned(),
                content_hash: fingerprint.content_hash,
                size_bytes: fingerprint.size_bytes,
            },
            payload: serde_json::to_vec(&observations)?,
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
        fingerprint_content(input.content)
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

/// Built-in `OpenAPI` Generator metadata extractor.
pub struct GeneratedClientMetadataExtractor;

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
        let fingerprint = fingerprint_content(input.content)?;
        let source = std::str::from_utf8(input.content)?;
        let metadata = extract_generated_client_metadata(&input.file.path.display, source)
            .map_err(|error| ExtractorError::InvalidInput(error.to_string()))?;
        let output_count = u64::try_from(metadata.len())
            .map_err(|_| ExtractorError::InvalidInput("too many metadata facts".to_owned()))?;
        Ok(ExtractionBatch {
            source: ArtifactFingerprint {
                repo_id: input.file.repo_id.clone(),
                checkout_id: input.file.checkout_id.clone(),
                path: input.file.path.clone(),
                extractor: self.id().to_owned(),
                content_hash: fingerprint.content_hash,
                size_bytes: fingerprint.size_bytes,
            },
            payload: serde_json::to_vec(&metadata)?,
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
        fingerprint_content(input.content)
    }
}

/// Built-in package-manifest extractor for the mandatory package ecosystems.
pub struct PackageManifestExtractor;

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
        let fingerprint = fingerprint_content(input.content)?;
        let source = std::str::from_utf8(input.content)?;
        let manifest = extract_package_manifest(&input.file.path.display, source)
            .map_err(|error| ExtractorError::InvalidInput(error.to_string()))?;
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
        Ok(ExtractionBatch {
            source: ArtifactFingerprint {
                repo_id: input.file.repo_id.clone(),
                checkout_id: input.file.checkout_id.clone(),
                path: input.file.path.clone(),
                extractor: self.id().to_owned(),
                content_hash: fingerprint.content_hash,
                size_bytes: fingerprint.size_bytes,
            },
            payload: serde_json::to_vec(&manifest)?,
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
        fingerprint_content(input.content)
    }
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
        BoundaryExtractor, ExtractInput, FileDescriptor, FocusedSourceExtractor, FocusedSourceLanguage, GeneratedClientMetadataExtractor, PackageManifestExtractor
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
        let extractor = GeneratedClientMetadataExtractor;
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
        let extractor = PackageManifestExtractor;
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
}

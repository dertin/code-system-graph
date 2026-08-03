use async_trait::async_trait;
use code_system_graph_model::{
    ArtifactFingerprint, CheckoutId, NativePath, RepoId, StoredExtractorBatch
};
use semver::Version;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum source size accepted by focused boundary extractors.
pub const MAX_EXTRACTOR_INPUT_BYTES: usize = 8 * 1024 * 1024;

/// Repository file metadata available during extractor discovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDescriptor {
    /// Repository containing the file.
    pub repo_id: RepoId,
    /// Concrete checkout containing the file.
    pub checkout_id: CheckoutId,
    /// Lossless repository-relative path.
    pub path: NativePath,
    /// Exact source size in bytes.
    pub size_bytes: u64,
}

/// Bounded repository inventory supplied to an extractor.
#[derive(Debug, Clone)]
pub struct DiscoverContext<'a> {
    /// Candidate files in deterministic path order.
    pub files: &'a [FileDescriptor],
}

/// One file selected by an extractor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredInput {
    /// Selected file metadata.
    pub file: FileDescriptor,
}

/// Immutable source input supplied to a focused extractor.
#[derive(Debug, Clone)]
pub struct ExtractInput<'a> {
    /// Selected file metadata.
    pub file: &'a FileDescriptor,
    /// Bounded source content.
    pub content: &'a [u8],
}

/// Content identity returned independently from extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentFingerprint {
    /// BLAKE3 content hash.
    pub content_hash: String,
    /// Exact source size in bytes.
    pub size_bytes: u64,
}

/// Explicit completeness of one focused extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionCompleteness {
    /// Every supported construct in the input was parsed.
    Complete,
    /// Unsupported or dynamic constructs were observed and reported.
    Partial,
}

/// Audit metrics and diagnostics for one source-owned batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionReport {
    /// Number of files selected by this batch.
    pub discovered_files: u64,
    /// Number of files parsed successfully.
    pub parsed_files: u64,
    /// Number of files skipped.
    pub skipped_files: u64,
    /// Explicit result completeness.
    pub completeness: ExtractionCompleteness,
    /// Bounded warnings; these must not contain source contents or secrets.
    pub warnings: Vec<String>,
    /// Number of evidence records emitted into the payload.
    pub evidence_count: u64,
    /// Extractor semantic version.
    pub extractor_version: Version,
    /// Bounded elapsed wall-clock time.
    pub elapsed_ms: u64,
}

/// Versioned, source-owned output produced by a boundary extractor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractionBatch {
    /// Source fingerprint that owns and invalidates this output.
    pub source: ArtifactFingerprint,
    /// Deterministic UTF-8 JSON observations without source text.
    pub payload: Vec<u8>,
    /// Number of observations encoded in the payload.
    pub output_count: u64,
    /// Extraction diagnostics.
    pub report: ExtractionReport,
}

impl ExtractionBatch {
    /// Converts the batch to its persistence representation.
    #[must_use]
    pub fn into_stored(
        self,
        budget_fingerprint: String,
        source_was_lossy: bool,
    ) -> StoredExtractorBatch {
        StoredExtractorBatch {
            source: self.source,
            extractor_version: self.report.extractor_version.to_string(),
            budget_fingerprint,
            source_was_lossy,
            output_count: self.output_count,
            payload: self.payload,
        }
    }
}

/// Failure returned by focused boundary extractors.
#[derive(Debug, Error)]
pub enum ExtractorError {
    /// Extraction exceeded one configured invocation resource.
    #[error(transparent)]
    LimitExceeded(#[from] crate::ExtractionLimitExceeded),
    /// Input exceeds the documented extraction budget.
    #[error("extractor input is {actual} bytes; maximum is {maximum}")]
    InputTooLarge {
        /// Observed byte count.
        actual: usize,
        /// Configured maximum.
        maximum: usize,
    },
    /// Source bytes are not valid for a text extractor.
    #[error("extractor input is not valid UTF-8")]
    InvalidUtf8(#[from] std::str::Utf8Error),
    /// Structured output could not be encoded.
    #[error("extractor output could not be encoded: {0}")]
    InvalidOutput(#[from] serde_json::Error),
    /// Extractor-specific failure with a bounded, non-sensitive explanation.
    #[error("{0}")]
    InvalidInput(String),
}

/// Focused, deterministic contract extractor.
#[async_trait]
pub trait BoundaryExtractor: Send + Sync {
    /// Stable extractor identity.
    fn id(&self) -> &'static str;

    /// Extractor and payload-schema semantic version.
    fn version(&self) -> Version;

    /// Returns whether this extractor can consume a candidate file.
    fn supports(&self, file: &FileDescriptor) -> bool;

    /// Selects supported files from a bounded deterministic inventory.
    async fn discover(
        &self,
        context: &DiscoverContext<'_>,
    ) -> Result<Vec<DiscoveredInput>, ExtractorError>;

    /// Produces a complete replacement batch for one source file.
    async fn extract(&self, input: &ExtractInput<'_>) -> Result<ExtractionBatch, ExtractorError>;

    /// Computes the source fingerprint used for incremental reuse.
    async fn fingerprint(
        &self,
        input: &ExtractInput<'_>,
    ) -> Result<ContentFingerprint, ExtractorError>;
}

/// Computes a bounded source fingerprint shared by focused extractors.
///
/// # Errors
///
/// Returns [`ExtractorError::InputTooLarge`] when the input exceeds the extraction budget.
pub fn fingerprint_content(content: &[u8]) -> Result<ContentFingerprint, ExtractorError> {
    if content.len() > MAX_EXTRACTOR_INPUT_BYTES {
        return Err(ExtractorError::InputTooLarge {
            actual: content.len(),
            maximum: MAX_EXTRACTOR_INPUT_BYTES,
        });
    }
    Ok(ContentFingerprint {
        content_hash: blake3::hash(content).to_hex().to_string(),
        size_bytes: u64::try_from(content.len()).map_err(|_| {
            ExtractorError::InvalidInput("source size exceeds the supported range".to_owned())
        })?,
    })
}

#[cfg(test)]
mod tests {
    use super::{ExtractorError, MAX_EXTRACTOR_INPUT_BYTES, fingerprint_content};

    #[test]
    fn fingerprint_should_be_deterministic_and_bounded() {
        let first = fingerprint_content(b"GET /orders");
        let second = fingerprint_content(b"GET /orders");

        assert!(matches!(
            (first, second),
            (Ok(left), Ok(right)) if left == right
        ));
        assert!(matches!(
            fingerprint_content(&vec![0; MAX_EXTRACTOR_INPUT_BYTES + 1]),
            Err(ExtractorError::InputTooLarge { .. })
        ));
    }
}

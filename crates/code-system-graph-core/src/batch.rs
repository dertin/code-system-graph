use code_system_graph_model::ArtifactFingerprint;
use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::{
    EXTRACTION_CONTRACT_VERSION, ExtractionBudgets, ExtractionLimitExceeded, ExtractionResource, ExtractionTracker
};

/// Complete transient output owned by one extractor input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractorBatch<T> {
    /// Fingerprint that identifies and invalidates this batch.
    pub source: ArtifactFingerprint,
    /// Deterministically ordered extracted outputs.
    pub outputs: Vec<T>,
}

impl<T> ExtractorBatch<T> {
    /// Creates one source-owned extractor batch.
    #[must_use]
    pub fn new(source: ArtifactFingerprint, outputs: Vec<T>) -> Self {
        Self { source, outputs }
    }
}

/// Error returned while encoding or decoding a source-owned extractor batch.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BatchPlanError {
    /// A source-owned output payload could not be serialized or decoded.
    #[error("invalid extractor batch payload: {0}")]
    InvalidPayload(String),
    /// One per-invocation extraction resource exceeded its configured maximum.
    #[error(transparent)]
    ExtractionLimit(#[from] ExtractionLimitExceeded),
    /// An output count cannot be represented by the persistence model.
    #[error("extractor batch output count exceeds the supported range")]
    OutputCountOverflow,
    /// Persisted output count does not match the decoded payload.
    #[error("extractor batch output count mismatch: stored {stored}, decoded {decoded}")]
    OutputCountMismatch {
        /// Count stored alongside the payload.
        stored: u64,
        /// Number of decoded outputs.
        decoded: usize,
    },
}

/// Encodes one typed extractor batch for atomic snapshot persistence.
///
/// # Errors
///
/// Returns [`BatchPlanError`] when outputs cannot be encoded or their count exceeds `u64`.
pub fn store_extractor_batch<T: Serialize>(
    batch: &ExtractorBatch<T>,
    tracker: &mut ExtractionTracker,
    source_was_lossy: bool,
) -> Result<code_system_graph_model::StoredExtractorBatch, BatchPlanError> {
    tracker.ensure_observations(u64::try_from(batch.outputs.len()).unwrap_or(u64::MAX))?;
    let mut writer = tracker.bounded_json_writer();
    if let Err(error) = serde_json::to_writer(&mut writer, &batch.outputs) {
        if let Some(limit) = tracker.output_limit_error(&writer) {
            return Err(limit.into());
        }
        return Err(BatchPlanError::InvalidPayload(error.to_string()));
    }
    Ok(code_system_graph_model::StoredExtractorBatch {
        source: batch.source.clone(),
        extractor_version: EXTRACTION_CONTRACT_VERSION.to_owned(),
        budget_fingerprint: tracker.budgets().fingerprint(),
        source_was_lossy,
        output_count: u64::try_from(batch.outputs.len())
            .map_err(|_| BatchPlanError::OutputCountOverflow)?,
        payload: writer.into_inner(),
    })
}

/// Decodes one persisted source-owned output batch.
///
/// # Errors
///
/// Returns [`BatchPlanError`] when the payload schema is invalid or its count is inconsistent.
pub fn load_extractor_batch<T: DeserializeOwned>(
    stored: &code_system_graph_model::StoredExtractorBatch,
) -> Result<ExtractorBatch<T>, BatchPlanError> {
    load_extractor_batch_with_budgets(stored, &ExtractionBudgets::default())
}

/// Decodes one persisted source-owned output batch after enforcing effective budgets.
///
/// The count and byte checks deliberately precede deserialization so a corrupt or untrusted
/// persisted batch cannot force allocations beyond the active extraction policy.
///
/// # Errors
///
/// Returns [`BatchPlanError`] when the payload exceeds `budgets`, its schema is invalid, or its
/// count is inconsistent.
pub fn load_extractor_batch_with_budgets<T: DeserializeOwned>(
    stored: &code_system_graph_model::StoredExtractorBatch,
    budgets: &ExtractionBudgets,
) -> Result<ExtractorBatch<T>, BatchPlanError> {
    if stored.output_count > budgets.max_observations_per_artifact {
        return Err(ExtractionLimitExceeded {
            artifact: stored.source.path.display.clone(),
            extractor: stored.source.extractor.clone(),
            resource: ExtractionResource::Observations,
            observed: stored.output_count,
            maximum: budgets.max_observations_per_artifact,
        }
        .into());
    }
    let observed = u64::try_from(stored.payload.len()).unwrap_or(u64::MAX);
    if observed > budgets.max_serialized_output_bytes_per_artifact {
        return Err(ExtractionLimitExceeded {
            artifact: stored.source.path.display.clone(),
            extractor: stored.source.extractor.clone(),
            resource: ExtractionResource::SerializedOutputBytes,
            observed,
            maximum: budgets.max_serialized_output_bytes_per_artifact,
        }
        .into());
    }
    let outputs: Vec<T> = serde_json::from_slice(&stored.payload)
        .map_err(|error| BatchPlanError::InvalidPayload(error.to_string()))?;
    if usize::try_from(stored.output_count).ok() != Some(outputs.len()) {
        return Err(BatchPlanError::OutputCountMismatch {
            stored: stored.output_count,
            decoded: outputs.len(),
        });
    }
    Ok(ExtractorBatch::new(stored.source.clone(), outputs))
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{
        ArtifactFingerprint, CheckoutId, NativePath, NativePathEncoding, RepoId
    };

    use super::{
        BatchPlanError, ExtractorBatch, load_extractor_batch, load_extractor_batch_with_budgets, store_extractor_batch
    };
    use crate::{ExtractionBudgets, ExtractionLimitExceeded, ExtractionTracker};

    fn tracker() -> ExtractionTracker {
        ExtractionTracker::new("src/routes.rs", "test", &ExtractionBudgets::default())
    }

    fn path(value: &str) -> NativePath {
        NativePath {
            encoding: NativePathEncoding::Utf8,
            bytes: value.as_bytes().to_vec(),
            display: value.to_owned(),
        }
    }

    fn batch(source: &str, hash: &str, outputs: &[&str]) -> ExtractorBatch<String> {
        ExtractorBatch::new(
            ArtifactFingerprint {
                repo_id: RepoId::new("repo:api"),
                checkout_id: CheckoutId::new("checkout:api"),
                path: path(source),
                extractor: "code-system-graph.http.openapi".to_owned(),
                content_hash: hash.to_owned(),
                size_bytes: 1,
            },
            outputs.iter().map(|output| (*output).to_owned()).collect(),
        )
    }

    #[test]
    fn stored_batch_should_round_trip_without_source_text() {
        let original = batch("src/routes.rs", "hash", &["GET:/orders", "POST:/orders"]);
        let result = store_extractor_batch(&original, &mut tracker(), false)
            .and_then(|stored| load_extractor_batch::<String>(&stored));

        assert_eq!(result, Ok(original));
    }

    #[test]
    fn stored_batch_should_reject_inconsistent_output_count() {
        let original = batch("src/routes.rs", "hash", &["GET:/orders"]);
        let result =
            store_extractor_batch(&original, &mut tracker(), false).and_then(|mut stored| {
                stored.output_count = 2;
                load_extractor_batch::<String>(&stored)
            });

        assert!(matches!(
            result,
            Err(BatchPlanError::OutputCountMismatch {
                stored: 2,
                decoded: 1
            })
        ));
    }

    #[test]
    fn stored_batch_should_check_payload_limit_before_decoding() {
        let original = batch("src/routes.rs", "hash", &["GET:/orders"]);
        let stored = store_extractor_batch(&original, &mut tracker(), false).expect("stored batch");
        let exact = u64::try_from(stored.payload.len()).expect("payload length");
        let exact_budgets = ExtractionBudgets {
            max_serialized_output_bytes_per_artifact: exact,
            ..ExtractionBudgets::default()
        };
        let below_budgets = ExtractionBudgets {
            max_serialized_output_bytes_per_artifact: exact - 1,
            ..ExtractionBudgets::default()
        };

        assert_eq!(
            load_extractor_batch_with_budgets::<String>(&stored, &exact_budgets),
            Ok(original)
        );
        assert!(matches!(
            load_extractor_batch_with_budgets::<String>(&stored, &below_budgets),
            Err(BatchPlanError::ExtractionLimit(ExtractionLimitExceeded {
                resource: crate::ExtractionResource::SerializedOutputBytes,
                observed,
                maximum,
                ..
            })) if observed == exact && maximum == exact - 1
        ));
    }

    #[test]
    fn stored_batch_should_check_observation_limit_before_decoding() {
        let original = batch("src/routes.rs", "hash", &["GET:/orders", "POST:/orders"]);
        let stored = store_extractor_batch(&original, &mut tracker(), false).expect("stored batch");
        let budgets = ExtractionBudgets {
            max_observations_per_artifact: 1,
            ..ExtractionBudgets::default()
        };

        assert!(matches!(
            load_extractor_batch_with_budgets::<String>(&stored, &budgets),
            Err(BatchPlanError::ExtractionLimit(ExtractionLimitExceeded {
                resource: crate::ExtractionResource::Observations,
                observed: 2,
                maximum: 1,
                ..
            }))
        ));
    }
}

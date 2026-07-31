use std::collections::{BTreeMap, BTreeSet};

use code_system_graph_model::{
    ArtifactChangeKind, ArtifactFingerprint, CheckoutId, NativePath, RepoId
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

use crate::IncrementalPlan;

/// Stable identity of one extractor input within a concrete checkout.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ArtifactKey {
    /// Repository identity shared by linked worktrees.
    pub repo_id: RepoId,
    /// Concrete checkout identity.
    pub checkout_id: CheckoutId,
    /// Lossless repository-relative artifact path.
    pub path: NativePath,
    /// Extractor that owns this artifact.
    pub extractor: String,
}

impl From<&ArtifactFingerprint> for ArtifactKey {
    fn from(fingerprint: &ArtifactFingerprint) -> Self {
        Self {
            repo_id: fingerprint.repo_id.clone(),
            checkout_id: fingerprint.checkout_id.clone(),
            path: fingerprint.path.clone(),
            extractor: fingerprint.extractor.clone(),
        }
    }
}

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

    /// Returns the stable source key for planning and persistence.
    #[must_use]
    pub fn key(&self) -> ArtifactKey {
        ArtifactKey::from(&self.source)
    }
}

/// Required action for one source-owned extractor batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchAction {
    /// Execute the extractor for a newly discovered source.
    Add,
    /// Execute the extractor and replace an existing source batch.
    Replace,
    /// Reuse the previous batch without extractor work.
    Reuse,
    /// Remove the previous batch because its source disappeared.
    Delete,
}

/// One deterministic source action derived from an incremental artifact plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedBatch {
    /// Stable extractor input identity.
    pub key: ArtifactKey,
    /// Required action.
    pub action: BatchAction,
}

/// Source-level extraction and deletion work for one scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractorBatchPlan {
    /// Actions sorted by repository, checkout, path, and extractor.
    pub batches: Vec<PlannedBatch>,
}

impl ExtractorBatchPlan {
    /// Returns only source batches requiring extraction or deletion.
    pub fn changed(&self) -> impl Iterator<Item = &PlannedBatch> {
        self.batches
            .iter()
            .filter(|batch| batch.action != BatchAction::Reuse)
    }
}

/// Converts artifact changes into source-owned extractor batch actions.
#[must_use]
pub fn plan_extractor_batches(plan: &IncrementalPlan) -> ExtractorBatchPlan {
    let batches = plan
        .changes
        .iter()
        .map(|change| PlannedBatch {
            key: ArtifactKey {
                repo_id: change.repo_id.clone(),
                checkout_id: change.checkout_id.clone(),
                path: change.path.clone(),
                extractor: change.extractor.clone(),
            },
            action: match change.kind {
                ArtifactChangeKind::Added => BatchAction::Add,
                ArtifactChangeKind::Modified => BatchAction::Replace,
                ArtifactChangeKind::Deleted => BatchAction::Delete,
                ArtifactChangeKind::Unchanged => BatchAction::Reuse,
            },
        })
        .collect();
    ExtractorBatchPlan { batches }
}

/// Error returned when affected-neighborhood planning lacks a required batch.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BatchPlanError {
    /// Multiple batches claim the same source key.
    #[error("duplicate {side} extractor batch for `{extractor}` at `{path}`")]
    DuplicateBatch {
        /// Previous or current batch set.
        side: &'static str,
        /// Extractor owning the duplicate input.
        extractor: String,
        /// Diagnostic path display.
        path: String,
    },
    /// A changed source has no required previous or current output batch.
    #[error("missing {side} extractor batch for `{extractor}` at `{path}`")]
    MissingBatch {
        /// Previous or current batch set.
        side: &'static str,
        /// Extractor owning the missing input.
        extractor: String,
        /// Diagnostic path display.
        path: String,
    },
    /// A source-owned output payload could not be serialized or decoded.
    #[error("invalid extractor batch payload: {0}")]
    InvalidPayload(String),
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
    extractor_version: impl Into<String>,
) -> Result<code_system_graph_model::StoredExtractorBatch, BatchPlanError> {
    Ok(code_system_graph_model::StoredExtractorBatch {
        source: batch.source.clone(),
        extractor_version: extractor_version.into(),
        output_count: u64::try_from(batch.outputs.len())
            .map_err(|_| BatchPlanError::OutputCountOverflow)?,
        payload: serde_json::to_vec(&batch.outputs)
            .map_err(|error| BatchPlanError::InvalidPayload(error.to_string()))?,
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

/// Computes exact link neighborhoods affected by add, modify, and delete actions.
///
/// Modified sources contribute old and new keys, deleted sources contribute old keys, and added
/// sources contribute new keys. Unchanged sources do not trigger relinking.
///
/// # Errors
///
/// Returns [`BatchPlanError`] for duplicate source batches or when a changed action lacks the
/// required previous/current batch.
pub fn affected_link_keys<T, K>(
    plan: &ExtractorBatchPlan,
    previous: &[ExtractorBatch<T>],
    current: &[ExtractorBatch<T>],
    link_key: impl Fn(&T) -> K,
) -> Result<BTreeSet<K>, BatchPlanError>
where
    K: Ord,
{
    let previous = batch_map(previous, "previous")?;
    let current = batch_map(current, "current")?;
    let mut keys = BTreeSet::new();
    for batch in plan.changed() {
        match batch.action {
            BatchAction::Add => {
                extend_link_keys(
                    &mut keys,
                    required_batch(&current, batch, "current")?,
                    &link_key,
                );
            }
            BatchAction::Replace => {
                extend_link_keys(
                    &mut keys,
                    required_batch(&previous, batch, "previous")?,
                    &link_key,
                );
                extend_link_keys(
                    &mut keys,
                    required_batch(&current, batch, "current")?,
                    &link_key,
                );
            }
            BatchAction::Delete => {
                extend_link_keys(
                    &mut keys,
                    required_batch(&previous, batch, "previous")?,
                    &link_key,
                );
            }
            BatchAction::Reuse => {}
        }
    }
    Ok(keys)
}

fn batch_map<'a, T>(
    batches: &'a [ExtractorBatch<T>],
    side: &'static str,
) -> Result<BTreeMap<ArtifactKey, &'a ExtractorBatch<T>>, BatchPlanError> {
    let mut map = BTreeMap::new();
    for batch in batches {
        let key = batch.key();
        if map.insert(key.clone(), batch).is_some() {
            return Err(BatchPlanError::DuplicateBatch {
                side,
                extractor: key.extractor,
                path: key.path.display,
            });
        }
    }
    Ok(map)
}

fn required_batch<'a, T>(
    batches: &BTreeMap<ArtifactKey, &'a ExtractorBatch<T>>,
    planned: &PlannedBatch,
    side: &'static str,
) -> Result<&'a ExtractorBatch<T>, BatchPlanError> {
    batches
        .get(&planned.key)
        .copied()
        .ok_or_else(|| BatchPlanError::MissingBatch {
            side,
            extractor: planned.key.extractor.clone(),
            path: planned.key.path.display.clone(),
        })
}

fn extend_link_keys<T, K>(
    keys: &mut BTreeSet<K>,
    batch: &ExtractorBatch<T>,
    link_key: &impl Fn(&T) -> K,
) where
    K: Ord,
{
    keys.extend(batch.outputs.iter().map(link_key));
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{
        ArtifactChange, ArtifactChangeKind, ArtifactFingerprint, CheckoutId, NativePath, NativePathEncoding, RepoId
    };

    use super::{
        BatchAction, BatchPlanError, ExtractorBatch, affected_link_keys, load_extractor_batch, plan_extractor_batches, store_extractor_batch
    };
    use crate::IncrementalPlan;

    fn path(value: &str) -> NativePath {
        NativePath {
            encoding: NativePathEncoding::Utf8,
            bytes: value.as_bytes().to_vec(),
            display: value.to_owned(),
        }
    }

    fn change(source: &str, kind: ArtifactChangeKind) -> ArtifactChange {
        ArtifactChange {
            repo_id: RepoId::new("repo:api"),
            checkout_id: CheckoutId::new("checkout:api"),
            path: path(source),
            extractor: "code-system-graph.http.openapi".to_owned(),
            kind,
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
    fn batch_plan_should_preserve_deterministic_source_actions() {
        let plan = plan_extractor_batches(&IncrementalPlan {
            changes: vec![
                change("added.yaml", ArtifactChangeKind::Added),
                change("deleted.yaml", ArtifactChangeKind::Deleted),
                change("same.yaml", ArtifactChangeKind::Unchanged),
            ],
        });

        assert_eq!(
            plan.batches
                .iter()
                .map(|batch| batch.action)
                .collect::<Vec<_>>(),
            vec![BatchAction::Add, BatchAction::Delete, BatchAction::Reuse]
        );
    }

    #[test]
    fn affected_keys_should_include_old_and_new_modified_neighborhoods() {
        let plan = plan_extractor_batches(&IncrementalPlan {
            changes: vec![change("openapi.yaml", ArtifactChangeKind::Modified)],
        });
        let result = affected_link_keys(
            &plan,
            &[batch("openapi.yaml", "old", &["POST:/v1/orders"])],
            &[batch("openapi.yaml", "new", &["POST:/v2/orders"])],
            Clone::clone,
        );

        assert_eq!(
            result,
            Ok(["POST:/v1/orders".to_owned(), "POST:/v2/orders".to_owned()]
                .into_iter()
                .collect())
        );
    }

    #[test]
    fn affected_keys_should_require_deleted_previous_batch() {
        let plan = plan_extractor_batches(&IncrementalPlan {
            changes: vec![change("deleted.yaml", ArtifactChangeKind::Deleted)],
        });
        let previous: Vec<ExtractorBatch<String>> = Vec::new();
        let current: Vec<ExtractorBatch<String>> = Vec::new();
        let result = affected_link_keys(&plan, &previous, &current, Clone::clone);

        assert!(matches!(
            result,
            Err(BatchPlanError::MissingBatch {
                side: "previous",
                ..
            })
        ));
    }

    #[test]
    fn stored_batch_should_round_trip_without_source_text() {
        let original = batch("src/routes.rs", "hash", &["GET:/orders", "POST:/orders"]);
        let result = store_extractor_batch(&original, "1.0.0")
            .and_then(|stored| load_extractor_batch::<String>(&stored));

        assert_eq!(result, Ok(original));
    }

    #[test]
    fn stored_batch_should_reject_inconsistent_output_count() {
        let original = batch("src/routes.rs", "hash", &["GET:/orders"]);
        let result = store_extractor_batch(&original, "1.0.0").and_then(|mut stored| {
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
}

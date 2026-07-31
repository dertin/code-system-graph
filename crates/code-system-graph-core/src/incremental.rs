use std::collections::{BTreeMap, BTreeSet};

use code_system_graph_model::{
    ArtifactChange, ArtifactChangeKind, ArtifactFingerprint, CheckoutId, NativePath, RepoId
};

type ArtifactKey = (RepoId, CheckoutId, NativePath, String);

/// Deterministic incremental plan for extractor-relevant artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncrementalPlan {
    /// Changes sorted by repository, checkout, path, and extractor.
    pub changes: Vec<ArtifactChange>,
}

impl IncrementalPlan {
    /// Returns whether any artifact requires add, modify, or delete processing.
    #[must_use]
    pub fn has_changes(&self) -> bool {
        self.changes
            .iter()
            .any(|change| change.kind != ArtifactChangeKind::Unchanged)
    }

    /// Returns the number of artifacts requiring extractor or deletion work.
    #[must_use]
    pub fn changed_count(&self) -> usize {
        self.changes
            .iter()
            .filter(|change| change.kind != ArtifactChangeKind::Unchanged)
            .count()
    }
}

/// Compares current artifact fingerprints with the previous published snapshot.
#[must_use]
pub fn plan_incremental_scan(
    previous: &[ArtifactFingerprint],
    current: &[ArtifactFingerprint],
) -> IncrementalPlan {
    let previous = fingerprint_map(previous);
    let current = fingerprint_map(current);
    let keys = previous
        .keys()
        .chain(current.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let changes = keys
        .into_iter()
        .filter_map(|key| {
            let kind = match (previous.get(&key), current.get(&key)) {
                (None, Some(_)) => ArtifactChangeKind::Added,
                (Some(_), None) => ArtifactChangeKind::Deleted,
                (Some(before), Some(after)) if before.content_hash != after.content_hash => {
                    ArtifactChangeKind::Modified
                }
                (Some(_), Some(_)) => ArtifactChangeKind::Unchanged,
                (None, None) => return None,
            };
            Some(ArtifactChange {
                repo_id: key.0,
                checkout_id: key.1,
                path: key.2,
                extractor: key.3,
                kind,
            })
        })
        .collect();
    IncrementalPlan { changes }
}

fn fingerprint_map(
    fingerprints: &[ArtifactFingerprint],
) -> BTreeMap<ArtifactKey, &ArtifactFingerprint> {
    fingerprints
        .iter()
        .map(|fingerprint| {
            (
                (
                    fingerprint.repo_id.clone(),
                    fingerprint.checkout_id.clone(),
                    fingerprint.path.clone(),
                    fingerprint.extractor.clone(),
                ),
                fingerprint,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use code_system_graph_model::{
        ArtifactChangeKind, ArtifactFingerprint, CheckoutId, NativePath, NativePathEncoding, RepoId
    };

    use super::plan_incremental_scan;

    fn fingerprint(path: &str, hash: &str) -> ArtifactFingerprint {
        ArtifactFingerprint {
            repo_id: RepoId::new("repo:api"),
            checkout_id: CheckoutId::new("checkout:api"),
            path: NativePath {
                encoding: NativePathEncoding::Utf8,
                bytes: path.as_bytes().to_vec(),
                display: path.to_owned(),
            },
            extractor: "openapi".to_owned(),
            content_hash: hash.to_owned(),
            size_bytes: 1,
        }
    }

    #[test]
    fn plan_should_classify_add_modify_delete_and_unchanged() {
        let previous = vec![
            fingerprint("deleted.yaml", "a"),
            fingerprint("modified.yaml", "a"),
            fingerprint("same.yaml", "a"),
        ];
        let current = vec![
            fingerprint("added.yaml", "a"),
            fingerprint("modified.yaml", "b"),
            fingerprint("same.yaml", "a"),
        ];

        let plan = plan_incremental_scan(&previous, &current);
        let kinds = plan
            .changes
            .iter()
            .map(|change| change.kind)
            .collect::<Vec<_>>();

        assert_eq!(
            kinds,
            vec![
                ArtifactChangeKind::Added,
                ArtifactChangeKind::Deleted,
                ArtifactChangeKind::Modified,
                ArtifactChangeKind::Unchanged,
            ]
        );
    }
}

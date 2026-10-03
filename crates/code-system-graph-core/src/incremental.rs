use code_system_graph_model::{
    ArtifactChange, ArtifactChangeKind, ArtifactFingerprint, CheckoutId, NativePath, RepoId
};

type ArtifactKey<'a> = (&'a RepoId, &'a CheckoutId, &'a NativePath, &'a str);

/// Deterministic incremental plan for extractor-relevant artifacts.
///
/// Only artifacts that require extraction or deletion are listed; every current artifact absent
/// from [`Self::changes`] is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncrementalPlan {
    /// Changes sorted by repository, checkout, path, and extractor.
    pub changes: Vec<ArtifactChange>,
}

impl IncrementalPlan {
    /// Returns whether any artifact requires add, modify, or delete processing.
    #[must_use]
    pub fn has_changes(&self) -> bool {
        !self.changes.is_empty()
    }

    /// Returns the number of artifacts requiring extractor or deletion work.
    #[must_use]
    pub fn changed_count(&self) -> usize {
        self.changes.len()
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
    let added_or_modified = current.iter().filter_map(|(key, after)| {
        let kind = match previous.get(key) {
            None => ArtifactChangeKind::Added,
            Some(before) if before.content_hash != after.content_hash => {
                ArtifactChangeKind::Modified
            }
            Some(_) => return None,
        };
        Some((*key, kind))
    });
    let deleted = previous
        .keys()
        .filter(|key| !current.contains_key(*key))
        .map(|key| (*key, ArtifactChangeKind::Deleted));
    let mut changes = added_or_modified
        .chain(deleted)
        .map(|(key, kind)| ArtifactChange {
            repo_id: key.0.clone(),
            checkout_id: key.1.clone(),
            path: key.2.clone(),
            extractor: key.3.to_owned(),
            kind,
        })
        .collect::<Vec<_>>();
    changes.sort_by(|left, right| {
        (
            &left.repo_id,
            &left.checkout_id,
            &left.path,
            &left.extractor,
        )
            .cmp(&(
                &right.repo_id,
                &right.checkout_id,
                &right.path,
                &right.extractor,
            ))
    });
    IncrementalPlan { changes }
}

fn fingerprint_map(
    fingerprints: &[ArtifactFingerprint],
) -> foldhash::HashMap<ArtifactKey<'_>, &ArtifactFingerprint> {
    fingerprints
        .iter()
        .map(|fingerprint| {
            (
                (
                    &fingerprint.repo_id,
                    &fingerprint.checkout_id,
                    &fingerprint.path,
                    fingerprint.extractor.as_str(),
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
    fn plan_should_list_only_added_deleted_and_modified_artifacts_in_key_order() {
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
        let changes = plan
            .changes
            .iter()
            .map(|change| (change.path.display.as_str(), change.kind))
            .collect::<Vec<_>>();

        assert_eq!(
            changes,
            vec![
                ("added.yaml", ArtifactChangeKind::Added),
                ("deleted.yaml", ArtifactChangeKind::Deleted),
                ("modified.yaml", ArtifactChangeKind::Modified),
            ]
        );
        assert_eq!(plan.changed_count(), 3);
    }

    #[test]
    fn plan_should_report_no_changes_for_identical_artifact_sets() {
        let fingerprints = vec![fingerprint("same.yaml", "a")];

        let plan = plan_incremental_scan(&fingerprints, &fingerprints);

        assert!(!plan.has_changes());
    }
}

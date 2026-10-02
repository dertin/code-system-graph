//! Artifact discovery and fingerprinting with one canonicalization, stat, and read per file.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::Metadata;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use code_system_graph_core::{
    ExtractionBudgets, ExtractionTracker, IgnorePolicy, JobPhase, discover_repository_files, encode_native_path, parallel
};
use code_system_graph_model::{ArtifactFingerprint, RepositoryRecord, stable_id_bytes};

use super::{ApplicationError, WorkspaceContext, focused_extractors_for_path, read_bounded_bytes};
use crate::telemetry::PhaseRecorder;
use crate::work_state::{FileStat, FileStatDelta, FileStatKey};
use crate::{focused_extraction, worker};

/// A stat entry is trusted only when the file was last modified at least this long before the
/// entry was recorded, so a same-timestamp rewrite after recording always changes the metadata.
const RACY_WINDOW_NS: i64 = 2_000_000_000;

/// Fingerprints and stat-cache maintenance produced by one discovery pass.
pub(crate) struct DiscoveredArtifacts {
    pub(crate) fingerprints: Vec<ArtifactFingerprint>,
    pub(crate) stat_delta: FileStatDelta,
    /// Files whose content hash came from the stat cache without a read.
    pub(crate) stat_hits: u64,
}

struct FileRequest<'a> {
    repository: &'a RepositoryRecord,
    checkout: &'a Path,
    relative: PathBuf,
    extractors: BTreeSet<String>,
}

struct FileOutcome {
    fingerprints: Vec<ArtifactFingerprint>,
    stat: Option<(FileStatKey, FileStat)>,
    stat_hit: bool,
}

/// Discovers and fingerprints every artifact of the selected repositories.
///
/// `selected` limits discovery to those manifest aliases; `None` scans every repository.
pub(crate) fn discover_artifact_fingerprints(
    context: &WorkspaceContext,
    selected: Option<&BTreeSet<String>>,
    stat_cache: &HashMap<FileStatKey, FileStat>,
    phases: &mut PhaseRecorder,
) -> Result<DiscoveredArtifacts, ApplicationError> {
    let workers = context.execution_policy.effective_extraction_workers();
    let repository_records = context
        .registry
        .record
        .repositories
        .iter()
        .map(|repository| (repository.alias.as_str(), repository))
        .collect::<BTreeMap<_, _>>();
    let aliases = context
        .manifest
        .repos
        .keys()
        .filter(|alias| selected.is_none_or(|selected| selected.contains(alias.as_str())))
        .collect::<Vec<_>>();
    let per_repository = parallel::map_ordered(&aliases, workers, |alias| {
        repository_requests(context, &repository_records, alias)
    });
    let mut requests = Vec::new();
    for result in per_repository {
        requests.extend(result?);
    }
    let scanned_checkouts = requests
        .iter()
        .map(|request| request.repository.checkout_id.as_str().to_owned())
        .collect::<BTreeSet<_>>();
    phases.complete(JobPhase::Discovery);

    let recorded_before_ns = unix_nanos(SystemTime::now()).saturating_sub(RACY_WINDOW_NS);
    let outcomes = parallel::map_ordered(&requests, workers, |request| {
        fingerprint_file(
            request,
            &context.extraction_budgets,
            stat_cache,
            recorded_before_ns,
        )
    });

    let mut fingerprints = Vec::new();
    let mut seen = HashSet::new();
    let mut stat_delta = FileStatDelta::default();
    let mut stat_hits = 0_u64;
    for outcome in outcomes {
        let outcome = outcome?;
        stat_hits = stat_hits.saturating_add(u64::from(outcome.stat_hit));
        if let Some((key, stat)) = outcome.stat {
            if stat_cache.get(&key) != Some(&stat) {
                stat_delta.upserts.push((key.clone(), stat));
            }
            seen.insert(key);
        }
        fingerprints.extend(outcome.fingerprints);
    }
    // Canonicalized symlinks can resolve two requests to one artifact; the last request wins.
    fingerprints.sort_by(|left, right| {
        focused_extraction::artifact_key(left).cmp(&focused_extraction::artifact_key(right))
    });
    fingerprints.dedup_by(|later, earlier| {
        let duplicate = later.checkout_id == earlier.checkout_id
            && later.path == earlier.path
            && later.extractor == earlier.extractor;
        if duplicate {
            std::mem::swap(later, earlier);
        }
        duplicate
    });
    stat_delta.removals = stat_cache
        .keys()
        .filter(|key| scanned_checkouts.contains(&key.0) && !seen.contains(*key))
        .cloned()
        .collect();
    stat_delta.removals.sort();
    Ok(DiscoveredArtifacts {
        fingerprints,
        stat_delta,
        stat_hits,
    })
}

fn repository_requests<'a>(
    context: &'a WorkspaceContext,
    repository_records: &BTreeMap<&str, &'a RepositoryRecord>,
    alias: &str,
) -> Result<Vec<FileRequest<'a>>, ApplicationError> {
    let repository = *repository_records
        .get(alias)
        .ok_or_else(|| ApplicationError::RegistryAliasMissing(alias.to_owned()))?;
    let checkout = context
        .registry
        .checkout_path(alias)
        .ok_or_else(|| ApplicationError::RegistryAliasMissing(alias.to_owned()))?;
    let effective = context
        .repository_configs
        .get(alias)
        .ok_or_else(|| ApplicationError::RegistryAliasMissing(alias.to_owned()))?;
    let mut files = BTreeMap::<PathBuf, BTreeSet<String>>::new();
    let mut declare = |path: &str, extractor: &str| {
        files
            .entry(PathBuf::from(path))
            .or_default()
            .insert(extractor.to_owned());
    };
    for openapi in &effective.openapi {
        declare(openapi, "code-system-graph.http.openapi");
    }
    for consumer in &effective.http_consumers {
        declare(&consumer.source, "code-system-graph.http.declared");
    }
    for test in &effective.integration_tests {
        declare(&test.path, "code-system-graph.tests.declared");
    }
    for implementation in &effective.implementations {
        declare(
            &implementation.path,
            "code-system-graph.implementations.declared",
        );
    }
    for (relative, extractors) in discover_focused_artifacts(checkout, &effective.ignore_policy)? {
        files
            .entry(relative)
            .or_default()
            .extend(extractors.into_iter().map(str::to_owned));
    }
    Ok(files
        .into_iter()
        .map(|(relative, extractors)| FileRequest {
            repository,
            checkout,
            relative,
            extractors,
        })
        .collect())
}

fn discover_focused_artifacts(
    checkout: &Path,
    ignore_policy: &IgnorePolicy,
) -> Result<Vec<(PathBuf, Vec<&'static str>)>, ApplicationError> {
    let mut discovered = Vec::new();
    for relative in discover_repository_files(checkout, ignore_policy, None)? {
        worker::report_progress(JobPhase::Discovery, 1);
        let extractors = focused_extractors_for_path(checkout, &relative);
        if !extractors.is_empty() {
            discovered.push((relative, extractors));
        }
    }
    Ok(discovered)
}

fn fingerprint_file(
    request: &FileRequest<'_>,
    budgets: &ExtractionBudgets,
    stat_cache: &HashMap<FileStatKey, FileStat>,
    recorded_before_ns: i64,
) -> Result<FileOutcome, ApplicationError> {
    let checkout = request.checkout;
    let configured_path = checkout.join(&request.relative);
    let canonical_path =
        std::fs::canonicalize(&configured_path).map_err(|source| ApplicationError::ReadFile {
            path: configured_path.clone(),
            source,
        })?;
    let relative = canonical_path.strip_prefix(checkout).map_err(|_| {
        ApplicationError::ArtifactOutsideCheckout {
            path: configured_path.clone(),
            checkout: checkout.to_path_buf(),
        }
    })?;
    let metadata =
        std::fs::metadata(&canonical_path).map_err(|source| ApplicationError::ReadFile {
            path: canonical_path.clone(),
            source,
        })?;
    let path = encode_native_path(relative);
    code_system_graph_model::validate_safe_path_display(&path.display)
        .map_err(|_| ApplicationError::UnsafeArtifactPath)?;
    let first_extractor = request
        .extractors
        .first()
        .map_or("code-system-graph.discovery", String::as_str);
    let tracker = ExtractionTracker::new(&path.display, first_extractor, budgets);
    tracker.check_input_bytes(metadata.len())?;

    let key = (
        request.repository.checkout_id.as_str().to_owned(),
        path.bytes.clone(),
    );
    let observed = observed_stat(&metadata);
    let cached_hash = stat_cache.get(&key).and_then(|stat| {
        observed
            .as_ref()
            .filter(|(size, modified, identity)| {
                stat.size_bytes == *size
                    && stat.modified_unix_ns == *modified
                    && &stat.file_identity == identity
            })
            .map(|_| stat.content_hash.clone())
    });
    let stat_hit = cached_hash.is_some();
    let content_hash = if let Some(hash) = cached_hash {
        hash
    } else {
        let mut tracker = tracker;
        let content = read_bounded_bytes(&canonical_path, &mut tracker)?;
        stable_id_bytes("artifact-content", &content)
    };
    let stat = observed
        .filter(|(_, modified, _)| *modified < recorded_before_ns)
        .map(|(size_bytes, modified_unix_ns, file_identity)| {
            (
                key,
                FileStat {
                    size_bytes,
                    modified_unix_ns,
                    file_identity,
                    content_hash: content_hash.clone(),
                },
            )
        });
    let fingerprints = request
        .extractors
        .iter()
        .map(|extractor| ArtifactFingerprint {
            repo_id: request.repository.id.clone(),
            checkout_id: request.repository.checkout_id.clone(),
            path: path.clone(),
            extractor: extractor.clone(),
            content_hash: content_hash.clone(),
            size_bytes: metadata.len(),
        })
        .collect::<Vec<_>>();
    worker::report_progress(
        JobPhase::Fingerprinting,
        u64::try_from(fingerprints.len()).unwrap_or(u64::MAX),
    );
    Ok(FileOutcome {
        fingerprints,
        stat,
        stat_hit,
    })
}

/// Returns `(size, modified_unix_ns, file_identity)` when the platform exposes a modification
/// time; files without one are always read.
fn observed_stat(metadata: &Metadata) -> Option<(u64, i64, String)> {
    let modified = metadata.modified().ok()?;
    Some((
        metadata.len(),
        unix_nanos(modified),
        file_identity(metadata),
    ))
}

fn unix_nanos(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(0)
}

#[cfg(unix)]
fn file_identity(metadata: &Metadata) -> String {
    format!(
        "{}:{}:{}.{}",
        metadata.dev(),
        metadata.ino(),
        metadata.ctime(),
        metadata.ctime_nsec()
    )
}

#[cfg(not(unix))]
fn file_identity(_metadata: &Metadata) -> String {
    String::new()
}

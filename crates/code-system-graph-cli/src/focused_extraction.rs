//! Focused extractor batches: reuse of published batches, checkpoint persistence, and bounded
//! parallel extraction that reads each physical file once for all of its extractors.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use code_system_graph_core::{
    DataDocument, DocumentationDocument, EXTRACTION_CONTRACT_VERSION, EventDocument, ExtractionBudgets, ExtractionTracker, ExtractorBatch, GeneratedClientMetadata, GraphqlDocument, IncrementalPlan, InfrastructureDocument, JobPhase, PackageManifest, ProtobufDocument, SafeConfigDocument, SourceEpistemicStatus, SourceLanguage, SourceObservation, SourceRole, SourceWarning, extract_asyncapi, extract_codeowners, extract_data_artifact, extract_docker_compose, extract_generated_client_metadata, extract_graphql_document_with_tracker, extract_graphql_persisted_operations_with_tracker, extract_helm, extract_kubernetes, extract_markdown, extract_package_manifest_with_tracker, extract_protobuf_with_tracker, extract_safe_config, extract_service_catalog, extract_terraform, inspect_source_syntax, load_extractor_batch_with_budgets, parallel, parse_event_source, parse_go_source_with_tracker, parse_graphql_source_with_tracker, parse_java_source_with_tracker, parse_javascript_source_at_path_with_tracker, parse_literal_sql_source_at_root, parse_protobuf_generated_source, parse_python_source_with_tracker, parse_rust_source_with_tracker, parse_typescript_source_at_path_with_tracker, precheck_focused_source_values, store_extractor_batch
};
use code_system_graph_model::{
    ArtifactFingerprint, CheckoutId, NativePath, RepoId, StoredExtractorBatch
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{
    ApplicationError, FocusedBatchState, WorkspaceContext, cargo_crate_root, current_unix_millis, data_extractor, documentation_extractor, duration_millis, event_extractor, focused_extractor, graphql_extractor, infrastructure_extractor, native_relative_path, portable_path, protobuf_extractor, read_source_file, source_extractor, source_language_for_path, source_syntax_language
};
use crate::{work_state, worker};

/// Typed outputs of one focused extractor invocation.
enum FocusedOutput {
    Source(ExtractorBatch<SourceObservation>),
    Package(ExtractorBatch<PackageManifest>),
    GeneratedClient(ExtractorBatch<GeneratedClientMetadata>),
    Graphql(ExtractorBatch<GraphqlDocument>),
    Event(ExtractorBatch<EventDocument>),
    Protobuf(ExtractorBatch<ProtobufDocument>),
    Data(ExtractorBatch<DataDocument>),
    Infrastructure(ExtractorBatch<InfrastructureDocument>),
    Documentation(ExtractorBatch<DocumentationDocument>),
    Config(ExtractorBatch<SafeConfigDocument>),
}

/// Owner of an outcome's persisted batch; reused batches stay in their input vectors until the
/// ordered merge moves them out, so no payload is copied.
enum StoredSlot {
    Fresh(Box<StoredExtractorBatch>),
    Previous(usize),
    Checkpointed(usize),
}

struct ArtifactOutcome {
    stored: StoredSlot,
    /// `None` when the document holds no graph facts.
    output: Option<Box<FocusedOutput>>,
    degradations: Vec<String>,
    duration_ms: u64,
}

enum FocusedJob<'a> {
    Reuse {
        batch: &'a StoredExtractorBatch,
        slot: fn(usize) -> StoredSlot,
        index: usize,
    },
    /// Every fingerprint shares one checkout and one relative path.
    Extract {
        checkout: &'a Path,
        fingerprints: Vec<&'a ArtifactFingerprint>,
    },
}

/// Artifact identity borrowed from a fingerprint, so lookups over every artifact clone nothing.
pub(super) type BorrowedArtifactKey<'a> = (&'a RepoId, &'a CheckoutId, &'a NativePath, &'a str);

pub(super) fn artifact_key(fingerprint: &ArtifactFingerprint) -> BorrowedArtifactKey<'_> {
    (
        &fingerprint.repo_id,
        &fingerprint.checkout_id,
        &fingerprint.path,
        fingerprint.extractor.as_str(),
    )
}

/// Lowest job index that failed, so later jobs can stop while every earlier job still runs and
/// the reported error stays independent of scheduling.
struct FailureFence(AtomicUsize);

impl FailureFence {
    fn new() -> Self {
        Self(AtomicUsize::new(usize::MAX))
    }

    fn skips(&self, index: usize) -> bool {
        index > self.0.load(Ordering::Acquire)
    }

    fn record(&self, index: usize) {
        self.0.fetch_min(index, Ordering::AcqRel);
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "job planning, ordered merge, and checkpoint persistence share one fail-closed boundary"
)]
pub(super) fn assemble_focused_batches(
    context: &WorkspaceContext,
    fingerprints: &[ArtifactFingerprint],
    previous: Vec<StoredExtractorBatch>,
    plan: &IncrementalPlan,
    checkpointed: Vec<StoredExtractorBatch>,
    work_state: &mut work_state::WorkState,
) -> Result<FocusedBatchState, ApplicationError> {
    let budgets = &context.extraction_budgets;
    let budget_fingerprint = budgets.fingerprint();
    let previous_by_key = previous
        .iter()
        .enumerate()
        .map(|(index, batch)| (artifact_key(&batch.source), (index, batch)))
        .collect::<foldhash::HashMap<_, _>>();
    let checkpointed_by_key = checkpointed
        .iter()
        .enumerate()
        .map(|(index, batch)| (artifact_key(&batch.source), (index, batch)))
        .collect::<foldhash::HashMap<_, _>>();
    let changed = plan
        .changes
        .iter()
        .map(|change| {
            (
                &change.repo_id,
                &change.checkout_id,
                &change.path,
                change.extractor.as_str(),
            )
        })
        .collect::<foldhash::HashSet<_>>();
    let checkouts = context
        .registry
        .record
        .repositories
        .iter()
        .filter_map(|repository| {
            context
                .registry
                .checkout_path(&repository.alias)
                .map(|path| (repository.id.clone(), path))
        })
        .collect::<BTreeMap<RepoId, &Path>>();

    let mut jobs = Vec::<FocusedJob<'_>>::new();
    for fingerprint in fingerprints
        .iter()
        .filter(|fingerprint| focused_extractor(&fingerprint.extractor))
    {
        let key = artifact_key(fingerprint);
        let matches_fingerprint = |(_, batch): &(usize, &StoredExtractorBatch)| {
            batch.source.content_hash == fingerprint.content_hash
                && batch.extractor_version == EXTRACTION_CONTRACT_VERSION
                && batch.budget_fingerprint == budget_fingerprint
        };
        let reusable = checkpointed_by_key
            .get(&key)
            .copied()
            .filter(matches_fingerprint)
            .map(|(index, batch)| (index, batch, StoredSlot::Checkpointed as fn(usize) -> _))
            .or_else(|| {
                previous_by_key
                    .get(&key)
                    .copied()
                    .filter(|_| !changed.contains(&key))
                    .filter(matches_fingerprint)
                    .map(|(index, batch)| (index, batch, StoredSlot::Previous as fn(usize) -> _))
            });
        if let Some((index, batch, slot)) = reusable {
            jobs.push(FocusedJob::Reuse { batch, slot, index });
            continue;
        }
        let checkout = *checkouts.get(&fingerprint.repo_id).ok_or_else(|| {
            ApplicationError::RegistryAliasMissing(fingerprint.repo_id.as_str().to_owned())
        })?;
        if let Some(FocusedJob::Extract {
            fingerprints: group,
            ..
        }) = jobs.last_mut()
            && group.last().is_some_and(|last| {
                last.checkout_id == fingerprint.checkout_id && last.path == fingerprint.path
            })
        {
            group.push(fingerprint);
            continue;
        }
        jobs.push(FocusedJob::Extract {
            checkout,
            fingerprints: vec![fingerprint],
        });
    }

    let indexed_jobs = jobs.iter().enumerate().collect::<Vec<_>>();
    let fence = FailureFence::new();
    let results = parallel::map_ordered(
        &indexed_jobs,
        context.execution_policy.effective_extraction_workers(),
        |(index, job)| {
            if fence.skips(*index) {
                return None;
            }
            let result = run_job(context, job);
            if result.is_err() {
                fence.record(*index);
            }
            Some(result)
        },
    );

    drop(indexed_jobs);
    drop(jobs);
    drop(previous_by_key);
    drop(checkpointed_by_key);
    drop(changed);
    let mut state = FocusedBatchState {
        source_batches: Vec::new(),
        package_batches: Vec::new(),
        generated_client_batches: Vec::new(),
        graphql_batches: Vec::new(),
        event_batches: Vec::new(),
        protobuf_batches: Vec::new(),
        data_batches: Vec::new(),
        infrastructure_batches: Vec::new(),
        documentation_batches: Vec::new(),
        config_batches: Vec::new(),
        stored_batches: Vec::new(),
        unpublished_batches: Vec::new(),
        degradations: Vec::new(),
        checkpoint_writes: 0,
        artifact_durations_ms: Vec::new(),
    };
    let mut previous = previous.into_iter().map(Some).collect::<Vec<_>>();
    let mut checkpointed = checkpointed.into_iter().map(Some).collect::<Vec<_>>();
    let mut stored_batches = Vec::<(StoredExtractorBatch, bool)>::new();
    let mut fresh_indices = Vec::new();
    let mut first_error = None;
    for result in results.into_iter().flatten() {
        let outcomes = match result {
            Ok(outcomes) => outcomes,
            Err(error) => {
                first_error.get_or_insert(error);
                continue;
            }
        };
        for outcome in outcomes {
            let published = matches!(outcome.stored, StoredSlot::Previous(_));
            let stored = match outcome.stored {
                StoredSlot::Fresh(stored) => {
                    fresh_indices.push(stored_batches.len());
                    Some(*stored)
                }
                StoredSlot::Previous(index) => previous.get_mut(index).and_then(Option::take),
                StoredSlot::Checkpointed(index) => {
                    checkpointed.get_mut(index).and_then(Option::take)
                }
            };
            let stored = stored.ok_or_else(|| {
                ApplicationError::Initialization(
                    "reused extractor batch was claimed twice".to_owned(),
                )
            })?;
            stored_batches.push((stored, published));
            state.degradations.extend(outcome.degradations);
            state.artifact_durations_ms.push(outcome.duration_ms);
            let Some(output) = outcome.output else {
                continue;
            };
            match *output {
                FocusedOutput::Source(batch) => state.source_batches.push(batch),
                FocusedOutput::Package(batch) => state.package_batches.push(batch),
                FocusedOutput::GeneratedClient(batch) => {
                    state.generated_client_batches.push(batch);
                }
                FocusedOutput::Graphql(batch) => state.graphql_batches.push(batch),
                FocusedOutput::Event(batch) => state.event_batches.push(batch),
                FocusedOutput::Protobuf(batch) => state.protobuf_batches.push(batch),
                FocusedOutput::Data(batch) => state.data_batches.push(batch),
                FocusedOutput::Infrastructure(batch) => state.infrastructure_batches.push(batch),
                FocusedOutput::Documentation(batch) => state.documentation_batches.push(batch),
                FocusedOutput::Config(batch) => state.config_batches.push(batch),
            }
        }
    }
    drop(previous);
    drop(checkpointed);
    let fresh = fresh_indices
        .iter()
        .map(|index| &stored_batches[*index].0)
        .collect::<Vec<_>>();
    state.checkpoint_writes = work_state
        .put_batches(
            &fresh,
            context.execution_policy.max_checkpoint_cache_bytes,
            current_unix_millis(),
        )
        .map_err(ApplicationError::Initialization)?;
    if let Some(error) = first_error {
        return Err(error);
    }
    sort_by_source(&mut state.source_batches);
    sort_by_source(&mut state.package_batches);
    sort_by_source(&mut state.generated_client_batches);
    sort_by_source(&mut state.graphql_batches);
    sort_by_source(&mut state.event_batches);
    sort_by_source(&mut state.protobuf_batches);
    sort_by_source(&mut state.data_batches);
    sort_by_source(&mut state.infrastructure_batches);
    sort_by_source(&mut state.documentation_batches);
    sort_by_source(&mut state.config_batches);
    stored_batches
        .sort_by(|left, right| artifact_key(&left.0.source).cmp(&artifact_key(&right.0.source)));
    state.stored_batches.reserve_exact(stored_batches.len());
    for (index, (stored, published)) in stored_batches.into_iter().enumerate() {
        if !published {
            state.unpublished_batches.push(index);
        }
        state.stored_batches.push(stored);
    }
    Ok(state)
}

fn sort_by_source<T>(batches: &mut [ExtractorBatch<T>]) {
    batches.sort_by(|left, right| artifact_key(&left.source).cmp(&artifact_key(&right.source)));
}

/// Payload of a batch without outputs; zero-output batches contribute nothing to the graph, so
/// reusing one needs no decoding.
const EMPTY_PAYLOAD: &[u8] = b"[]";

/// Whether a source-scan document holds no facts, so it is persisted without outputs: it only
/// records that the file was scanned and cannot add anything to the graph.
fn fact_free_source_output(output: &FocusedOutput) -> bool {
    match output {
        FocusedOutput::Graphql(batch) => {
            batch.source.extractor == "code-system-graph.graphql.source"
                && batch.outputs.iter().all(|document| {
                    document.types.is_empty()
                        && document.operations.is_empty()
                        && document.fragments.is_empty()
                        && document.persisted_operations.is_empty()
                        && document.resolvers.is_empty()
                        && document.federation.is_empty()
                        && document.warnings.is_empty()
                        && document.complete
                })
        }
        FocusedOutput::Event(batch) => {
            batch.source.extractor == "code-system-graph.events.source"
                && batch.outputs.iter().all(|document| {
                    document.observations.is_empty()
                        && document.warnings.is_empty()
                        && document.specification.is_none()
                        && !document.incomplete
                })
        }
        FocusedOutput::Protobuf(batch) => {
            batch.source.extractor == "code-system-graph.protobuf.generated"
                && batch.outputs.iter().all(|document| {
                    matches!(document, ProtobufDocument::Generated(markers) if markers.is_empty())
                })
        }
        FocusedOutput::Data(batch) => {
            batch.source.extractor == "code-system-graph.data.source"
                && batch.outputs.iter().all(|document| {
                    document.tables.is_empty()
                        && document.migration.is_none()
                        && document.accesses.is_empty()
                        && document.frameworks.is_empty()
                        && document.references.is_empty()
                        && document.owners.is_empty()
                        && document.warnings.is_empty()
                        && !document.incomplete
                })
        }
        FocusedOutput::Source(_)
        | FocusedOutput::Package(_)
        | FocusedOutput::GeneratedClient(_)
        | FocusedOutput::Infrastructure(_)
        | FocusedOutput::Documentation(_)
        | FocusedOutput::Config(_) => false,
    }
}

fn run_job(
    context: &WorkspaceContext,
    job: &FocusedJob<'_>,
) -> Result<Vec<ArtifactOutcome>, ApplicationError> {
    match job {
        FocusedJob::Reuse { batch, slot, index } => {
            let started = Instant::now();
            let output = if batch.output_count == 0 && batch.payload == EMPTY_PAYLOAD {
                None
            } else {
                let output = decode_stored_batch(batch, &context.extraction_budgets)?;
                (!fact_free_source_output(&output)).then(|| Box::new(output))
            };
            worker::report_progress(JobPhase::Extraction, 1);
            Ok(vec![ArtifactOutcome {
                stored: slot(*index),
                output,
                degradations: Vec::new(),
                duration_ms: duration_millis(started.elapsed()),
            }])
        }
        FocusedJob::Extract {
            checkout,
            fingerprints,
        } => extract_file(context, checkout, fingerprints),
    }
}

fn decode_stored_batch(
    stored: &StoredExtractorBatch,
    budgets: &ExtractionBudgets,
) -> Result<FocusedOutput, ApplicationError> {
    fn decode<T: DeserializeOwned>(
        stored: &StoredExtractorBatch,
        budgets: &ExtractionBudgets,
    ) -> Result<ExtractorBatch<T>, ApplicationError> {
        Ok(load_extractor_batch_with_budgets(stored, budgets)?)
    }
    let extractor = stored.source.extractor.as_str();
    Ok(if source_extractor(extractor) {
        FocusedOutput::Source(decode(stored, budgets)?)
    } else if extractor == "code-system-graph.packages" {
        FocusedOutput::Package(decode(stored, budgets)?)
    } else if graphql_extractor(extractor) {
        FocusedOutput::Graphql(decode(stored, budgets)?)
    } else if event_extractor(extractor) {
        FocusedOutput::Event(decode(stored, budgets)?)
    } else if protobuf_extractor(extractor) {
        FocusedOutput::Protobuf(decode(stored, budgets)?)
    } else if data_extractor(extractor) {
        FocusedOutput::Data(decode(stored, budgets)?)
    } else if infrastructure_extractor(extractor) {
        FocusedOutput::Infrastructure(decode(stored, budgets)?)
    } else if documentation_extractor(extractor) {
        FocusedOutput::Documentation(decode(stored, budgets)?)
    } else if extractor == "code-system-graph.config.safe" {
        FocusedOutput::Config(decode(stored, budgets)?)
    } else {
        FocusedOutput::GeneratedClient(decode(stored, budgets)?)
    })
}

fn extract_file(
    context: &WorkspaceContext,
    checkout: &Path,
    fingerprints: &[&ArtifactFingerprint],
) -> Result<Vec<ArtifactOutcome>, ApplicationError> {
    let Some(first) = fingerprints.first() else {
        return Ok(Vec::new());
    };
    let budgets = &context.extraction_budgets;
    let read_started = Instant::now();
    let relative_path = native_relative_path(&first.path);
    let artifact_path = checkout.join(&relative_path);
    let mut read_tracker = ExtractionTracker::new(&first.path.display, &first.extractor, budgets);
    let (source, source_was_lossy) = read_source_file(&artifact_path, &mut read_tracker)?;
    let mut read_ms = duration_millis(read_started.elapsed());
    let file = SharedSource {
        checkout,
        artifact_path: &artifact_path,
        relative_path: &relative_path,
        text: &source,
        lossy: source_was_lossy,
    };
    let mut outcomes = Vec::with_capacity(fingerprints.len());
    for fingerprint in fingerprints {
        let started = Instant::now();
        let mut tracker =
            ExtractionTracker::new(&fingerprint.path.display, &fingerprint.extractor, budgets);
        let mut degradations = Vec::new();
        if source_was_lossy {
            degradations.push(format!(
                "{} contains invalid UTF-8 and was decoded lossily; extracted evidence is incomplete",
                fingerprint.path.display
            ));
        }
        let (mut stored, output) =
            extract_artifact(&file, fingerprint, &mut tracker, &mut degradations)?;
        let output = if fact_free_source_output(&output) {
            stored.output_count = 0;
            stored.payload = EMPTY_PAYLOAD.to_vec();
            None
        } else {
            Some(Box::new(output))
        };
        worker::report_progress(JobPhase::Extraction, 1);
        outcomes.push(ArtifactOutcome {
            stored: StoredSlot::Fresh(Box::new(stored)),
            output,
            degradations,
            duration_ms: duration_millis(started.elapsed()).saturating_add(read_ms),
        });
        read_ms = 0;
    }
    Ok(outcomes)
}

/// One file read shared by every extractor of that file.
struct SharedSource<'a> {
    checkout: &'a Path,
    artifact_path: &'a Path,
    relative_path: &'a Path,
    text: &'a str,
    lossy: bool,
}

impl SharedSource<'_> {
    fn language(
        &self,
        purpose: &str,
        portable_path: &str,
    ) -> Result<SourceLanguage, ApplicationError> {
        source_language_for_path(self.relative_path).ok_or_else(|| {
            ApplicationError::InvalidSourceObservation(format!(
                "unsupported {purpose} language for `{portable_path}`"
            ))
        })
    }
}

fn finish<T: Serialize>(
    batch: ExtractorBatch<T>,
    tracker: &mut ExtractionTracker,
    lossy: bool,
    wrap: fn(ExtractorBatch<T>) -> FocusedOutput,
) -> Result<(StoredExtractorBatch, FocusedOutput), ApplicationError> {
    let stored = store_extractor_batch(&batch, tracker, lossy)?;
    Ok((stored, wrap(batch)))
}

#[expect(
    clippy::too_many_lines,
    reason = "one exhaustive dispatch keeps every focused extractor's typed contract visible"
)]
fn extract_artifact(
    file: &SharedSource<'_>,
    fingerprint: &ArtifactFingerprint,
    tracker: &mut ExtractionTracker,
    degradations: &mut Vec<String>,
) -> Result<(StoredExtractorBatch, FocusedOutput), ApplicationError> {
    let source = file.text;
    let lossy = file.lossy;
    let extractor = fingerprint.extractor.as_str();
    let portable_path = portable_path(&fingerprint.path.display);
    if source_extractor(extractor) {
        let observations = extract_source_observations(file, fingerprint, tracker)?;
        return finish(
            ExtractorBatch::new(fingerprint.clone(), observations),
            tracker,
            lossy,
            FocusedOutput::Source,
        );
    }
    if extractor == "code-system-graph.packages" {
        let manifest = extract_package_manifest_with_tracker(&portable_path, source, tracker)?;
        return finish(
            ExtractorBatch::new(fingerprint.clone(), vec![manifest]),
            tracker,
            lossy,
            FocusedOutput::Package,
        );
    }
    if graphql_extractor(extractor) {
        let document = match extractor {
            "code-system-graph.graphql.document" => {
                extract_graphql_document_with_tracker(&portable_path, source, tracker)?
            }
            "code-system-graph.graphql.persisted" => GraphqlDocument {
                source_path: portable_path.clone(),
                types: Vec::new(),
                operations: Vec::new(),
                fragments: Vec::new(),
                persisted_operations: extract_graphql_persisted_operations_with_tracker(
                    &portable_path,
                    source,
                    tracker,
                )?,
                resolvers: Vec::new(),
                federation: Vec::new(),
                complete: true,
                warnings: Vec::new(),
            },
            _ => {
                let language = file.language("GraphQL source", &portable_path)?;
                let mut document = parse_graphql_source_with_tracker(language, source, tracker)?;
                document.source_path = portable_path;
                document
            }
        };
        return finish(
            ExtractorBatch::new(fingerprint.clone(), vec![document]),
            tracker,
            lossy,
            FocusedOutput::Graphql,
        );
    }
    if event_extractor(extractor) {
        let document = if extractor == "code-system-graph.events.asyncapi" {
            extract_asyncapi(&portable_path, source)?
        } else {
            let language = file.language("event source", &portable_path)?;
            let mut document = parse_event_source(language, source);
            document.source_path = Some(portable_path);
            document
        };
        return finish(
            ExtractorBatch::new(fingerprint.clone(), vec![document]),
            tracker,
            lossy,
            FocusedOutput::Event,
        );
    }
    if protobuf_extractor(extractor) {
        let document = if extractor == "code-system-graph.protobuf" {
            ProtobufDocument::File(Box::new(extract_protobuf_with_tracker(
                &portable_path,
                source,
                tracker,
            )?))
        } else {
            let language = file.language("generated protobuf source", &portable_path)?;
            ProtobufDocument::Generated(parse_protobuf_generated_source(
                language,
                &portable_path,
                source,
            ))
        };
        return finish(
            ExtractorBatch::new(fingerprint.clone(), vec![document]),
            tracker,
            lossy,
            FocusedOutput::Protobuf,
        );
    }
    if data_extractor(extractor) {
        let document = if extractor == "code-system-graph.data.source" {
            let language = file.language("data source", &portable_path)?;
            // The crate root only resolves Rust database-crate migration paths, so the ancestor
            // walk is skipped for every file that cannot reach that recognizer.
            let crate_root = if language == SourceLanguage::Rust
                && (source.contains("sqlx") || source.contains("mysql_async"))
            {
                cargo_crate_root(file.checkout, file.artifact_path)
            } else {
                String::new()
            };
            let source = if language == SourceLanguage::JavaScript && is_minified_script(source) {
                ""
            } else {
                source
            };
            parse_literal_sql_source_at_root(language, &portable_path, &crate_root, source)
        } else {
            extract_data_artifact(&portable_path, source)?
        };
        if extractor == "code-system-graph.data.artifact" && document.incomplete {
            degradations.push(format!(
                "{} data extraction is incomplete: {:?}",
                fingerprint.path.display, document.warnings
            ));
        }
        return finish(
            ExtractorBatch::new(fingerprint.clone(), vec![document]),
            tracker,
            lossy,
            FocusedOutput::Data,
        );
    }
    if infrastructure_extractor(extractor) {
        let document = match extractor {
            "code-system-graph.infrastructure.compose" => {
                extract_docker_compose(&portable_path, source)?
            }
            "code-system-graph.infrastructure.kubernetes" => {
                extract_kubernetes(&portable_path, source)?
            }
            "code-system-graph.infrastructure.helm" => extract_helm(&portable_path, source)?,
            _ => extract_terraform(&portable_path, source)?,
        };
        return finish(
            ExtractorBatch::new(fingerprint.clone(), vec![document]),
            tracker,
            lossy,
            FocusedOutput::Infrastructure,
        );
    }
    if documentation_extractor(extractor) {
        let document = match extractor {
            "code-system-graph.documents.markdown" => extract_markdown(&portable_path, source)?,
            "code-system-graph.documents.codeowners" => extract_codeowners(&portable_path, source)?,
            _ => extract_service_catalog(&portable_path, source)?,
        };
        return finish(
            ExtractorBatch::new(fingerprint.clone(), vec![document]),
            tracker,
            lossy,
            FocusedOutput::Documentation,
        );
    }
    if extractor == "code-system-graph.config.safe" {
        let document = extract_safe_config(&portable_path, source)?;
        return finish(
            ExtractorBatch::new(fingerprint.clone(), vec![document]),
            tracker,
            lossy,
            FocusedOutput::Config,
        );
    }
    let metadata = extract_generated_client_metadata(&portable_path, source, tracker)?;
    finish(
        ExtractorBatch::new(fingerprint.clone(), metadata),
        tracker,
        lossy,
        FocusedOutput::GeneratedClient,
    )
}

fn extract_source_observations(
    file: &SharedSource<'_>,
    fingerprint: &ArtifactFingerprint,
    tracker: &mut ExtractionTracker,
) -> Result<Vec<SourceObservation>, ApplicationError> {
    let source = file.text;
    let portable_path = portable_path(&fingerprint.path.display);
    let syntax_language = source_syntax_language(&fingerprint.extractor);
    let syntax = inspect_source_syntax(syntax_language, &portable_path, source, tracker)?;
    let reserved_observations = u64::try_from(syntax.boundary_candidate_count).map_err(|_| {
        ApplicationError::InvalidSourceObservation(format!(
            "{} contains too many syntax candidates",
            fingerprint.path.display
        ))
    })?;
    tracker.charge_work(reserved_observations)?;
    precheck_focused_source_values(source, syntax_language, tracker)?;
    let mut observations = match fingerprint.extractor.as_str() {
        "code-system-graph.source.javascript" => {
            parse_javascript_source_at_path_with_tracker(&portable_path, source, tracker)?
        }
        "code-system-graph.source.typescript" => {
            parse_typescript_source_at_path_with_tracker(&portable_path, source, tracker)?
        }
        "code-system-graph.source.rust" => parse_rust_source_with_tracker(source, tracker)?,
        "code-system-graph.source.python" => parse_python_source_with_tracker(source, tracker)?,
        "code-system-graph.source.go" => parse_go_source_with_tracker(source, tracker)?,
        "code-system-graph.source.java" => parse_java_source_with_tracker(source, tracker)?,
        _ => Vec::new(),
    };
    if observations
        .iter()
        .any(|observation| observation.role != SourceRole::Test)
        && syntax.boundary_candidate_count == 0
    {
        return Err(ApplicationError::InvalidSourceObservation(format!(
            "{} produced framework facts without a Tree-sitter boundary candidate",
            fingerprint.path.display
        )));
    }
    if syntax.has_error {
        for observation in &mut observations {
            observation.status = SourceEpistemicStatus::Incomplete;
            if !observation
                .warnings
                .contains(&SourceWarning::SyntaxErrorRecovery)
            {
                observation
                    .warnings
                    .push(SourceWarning::SyntaxErrorRecovery);
            }
        }
    }
    Ok(observations)
}

/// Whether a script is minified: at least 4 KiB with lines averaging 500 bytes or more.
fn is_minified_script(source: &str) -> bool {
    const MINIMUM_BYTES: usize = 4096;
    const MINIMUM_AVERAGE_LINE: usize = 500;
    source.len() >= MINIMUM_BYTES
        && source.len() / source.lines().count().max(1) >= MINIMUM_AVERAGE_LINE
}

//! Internal Explore execution state and budget accounting.

use std::sync::{Arc, OnceLock};

mod correlation;
mod model;

use code_system_graph_core::{
    ExecutionPolicy, LocalNeighborResult, ProviderError, ProviderExecution, ResolvedSymbol
};
use correlation::{
    ExploreCorrelationInput, ExploreCorrelationOutput, correlate_explore_handoffs, explore_repository_context, explore_repository_contexts
};
pub use model::{
    ExploreCoverage, ExploreEvidenceLocation, ExploreExecution, ExploreFederatedHandoff, ExploreInput, ExploreLocalRelationship, ExploreReport, ExploreRepositoryContext
};
use tokio_util::sync::CancellationToken;

use crate::{
    AgentNextAction, BTreeMap, BTreeSet, CodeGraphConfig, CodeGraphProvider, Duration, Edge, Evidence, FreshnessSummary, LocalCodeIntelligenceProvider, LocalContextRequest, LocalNeighborDirection, LocalNeighborsRequest, Node, OverallFreshness, Path, ProviderBudget, ProviderRequest, RepoFreshness, RepositoryRecord, SqliteStore, ToolEnvelope, ToolStatus, WorkspaceRecord, freshness_summary, native_relative_path
};

fn explore_provider_request(
    repository: &RepositoryRecord,
    project_path: &Path,
    max_output_bytes: usize,
    max_items: usize,
    deadline: tokio::time::Instant,
    cancellation: &CancellationToken,
) -> ProviderRequest {
    ProviderRequest {
        repo_id: repository.id.clone(),
        project_path: project_path.to_path_buf(),
        budget: ProviderBudget {
            timeout: deadline
                .saturating_duration_since(tokio::time::Instant::now())
                .max(Duration::from_millis(1)),
            max_output_bytes: max_output_bytes.max(1),
            max_items: max_items.max(1),
        },
        cancellation: cancellation.clone(),
    }
}

fn truncate_utf8_owned(mut value: String, maximum: usize) -> String {
    if value.len() <= maximum {
        return value;
    }
    let mut boundary = maximum;
    while boundary > 0 && !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value
}

fn fallback_explore_anchors(source_markdown: &str, maximum: usize) -> Vec<ResolvedSymbol> {
    let mut in_blast_radius = false;
    let mut anchors = Vec::new();
    for line in source_markdown.lines() {
        if line.starts_with("**Blast radius") && line.ends_with("**") {
            in_blast_radius = true;
            continue;
        }
        if in_blast_radius && line.starts_with("**") && line.ends_with("**") {
            break;
        }
        if !in_blast_radius || anchors.len() == maximum {
            continue;
        }
        let Some(entry) = line.strip_prefix("- `") else {
            continue;
        };
        let Some((name, location)) = entry.split_once("` (") else {
            continue;
        };
        let Some((location, suffix)) = location.split_once(')') else {
            continue;
        };
        if !suffix.is_empty() && !suffix.starts_with(" — ") {
            continue;
        }
        let Some((path, line)) = location.rsplit_once(':') else {
            continue;
        };
        let Ok(start_line) = line.parse::<usize>() else {
            continue;
        };
        let path = path.trim_start_matches("./");
        let safe_path = !path.is_empty()
            && Path::new(path).is_relative()
            && Path::new(path).components().all(|component| {
                matches!(
                    component,
                    std::path::Component::Normal(_) | std::path::Component::CurDir
                )
            });
        if name.is_empty()
            || start_line == 0
            || !safe_path
            || name.chars().any(char::is_control)
            || path.chars().any(char::is_control)
        {
            continue;
        }
        anchors.push(ResolvedSymbol {
            local_id: None,
            name: name.to_owned(),
            qualified_name: None,
            kind: "unknown".to_owned(),
            file_path: path.to_owned(),
            start_line,
            score: None,
        });
    }
    anchors
}

fn select_explore_anchors(symbols: &[ResolvedSymbol], maximum: usize) -> Vec<ResolvedSymbol> {
    let mut ranked = symbols.to_vec();
    ranked.sort_by(|left, right| {
        right
            .score
            .unwrap_or(f64::NEG_INFINITY)
            .total_cmp(&left.score.unwrap_or(f64::NEG_INFINITY))
            .then_with(|| left.file_path.cmp(&right.file_path))
            .then_with(|| left.start_line.cmp(&right.start_line))
            .then_with(|| left.name.cmp(&right.name))
    });
    let mut selected = Vec::new();
    let mut files = BTreeSet::new();
    for symbol in &ranked {
        if selected.len() == maximum {
            break;
        }
        if files.insert(symbol.file_path.clone()) {
            selected.push(symbol.clone());
        }
    }
    for symbol in ranked {
        if selected.len() == maximum {
            break;
        }
        if !selected.iter().any(|item| {
            item.file_path == symbol.file_path
                && item.start_line == symbol.start_line
                && item.name == symbol.name
        }) {
            selected.push(symbol);
        }
    }
    selected
}

fn explore_next_actions(
    workspace: &str,
    repository: &RepositoryRecord,
    query: &str,
    handoffs: &[ExploreFederatedHandoff],
    policy: &ExecutionPolicy,
) -> Vec<AgentNextAction> {
    let maximum = usize::try_from(policy.max_agent_next_actions_per_response)
        .expect("validated policy count is usize-representable");
    let mut actions = handoffs
        .iter()
        .map(|handoff| AgentNextAction {
            tool: "source_context".to_owned(),
            arguments: BTreeMap::from([
                ("workspace".to_owned(), workspace.to_owned()),
                ("node_id".to_owned(), handoff.node_id.as_str().to_owned()),
            ]),
            rationale: format!(
                "Inspect persisted evidence for the federated entity linked from `{}`.",
                handoff.anchor
            ),
        })
        .collect::<Vec<_>>();
    if actions.len() < maximum {
        actions.push(AgentNextAction {
            tool: "explore".to_owned(),
            arguments: BTreeMap::from([
                ("workspace".to_owned(), workspace.to_owned()),
                ("repository".to_owned(), repository.alias.clone()),
                ("query".to_owned(), query.to_owned()),
            ]),
            rationale:
                "Refine the local source question while retaining the selected repository scope."
                    .to_owned(),
        });
    }
    actions.truncate(maximum);
    actions
}

fn select_explore_repository<'a>(
    registry: &'a WorkspaceRecord,
    requested_alias: Option<&str>,
    workspace: &str,
) -> Result<&'a RepositoryRecord, String> {
    if let Some(alias) = requested_alias {
        return registry
            .repositories
            .iter()
            .find(|repository| repository.alias == alias)
            .ok_or_else(|| {
                format!("repository alias `{alias}` is not registered in workspace `{workspace}`")
            });
    }
    if registry.repositories.len() == 1 {
        return registry.repositories.first().ok_or_else(|| {
            format!("workspace `{workspace}` unexpectedly has no registered repository")
        });
    }
    Err(format!(
        "repository is required because workspace `{workspace}` contains {} repositories",
        registry.repositories.len()
    ))
}

fn explore_scoped_error_envelope(
    freshness: FreshnessSummary,
    status: ToolStatus,
    message: String,
) -> ToolEnvelope<ExploreReport> {
    ToolEnvelope {
        schema_version: 2,
        status,
        data: None,
        freshness,
        warnings: vec![message],
    }
}

fn explore_error_envelope(message: String) -> ToolEnvelope<ExploreReport> {
    ToolEnvelope {
        schema_version: 2,
        status: ToolStatus::Error,
        data: None,
        freshness: FreshnessSummary {
            overall: OverallFreshness::Unknown,
            stale_repositories: Vec::new(),
            reasons: vec!["Local repository exploration could not start.".to_owned()],
        },
        warnings: vec![message],
    }
}

fn explore_deadline_envelope(stage: &str) -> ToolEnvelope<ExploreReport> {
    ToolEnvelope {
        schema_version: 2,
        status: ToolStatus::Degraded,
        data: None,
        freshness: FreshnessSummary {
            overall: OverallFreshness::Unknown,
            stale_repositories: Vec::new(),
            reasons: vec!["Explore exhausted its global wall-time budget.".to_owned()],
        },
        warnings: vec![format!(
            "{stage} exceeded maxExploreWallTimeMs; no later Explore stages were started"
        )],
    }
}

struct ExploreSnapshotData {
    registry: WorkspaceRecord,
    freshness: Vec<RepoFreshness>,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    evidence: Vec<Evidence>,
}

enum ExploreSnapshotLoadError {
    Failed(String),
    Deadline,
}

fn explore_blocking_permits() -> Arc<tokio::sync::Semaphore> {
    static PERMITS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    PERMITS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
        .clone()
}

async fn run_bounded_explore_blocking<T, F>(
    context: &ExploreExecutionContext,
    operation: F,
) -> Result<T, ExploreSnapshotLoadError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    let permit =
        match tokio::time::timeout_at(context.deadline, explore_blocking_permits().acquire_owned())
            .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(error)) => return Err(ExploreSnapshotLoadError::Failed(error.to_string())),
            Err(_) => {
                context.cancellation.cancel();
                return Err(ExploreSnapshotLoadError::Deadline);
            }
        };
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    });
    match tokio::time::timeout_at(context.deadline, task).await {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(error))) => Err(ExploreSnapshotLoadError::Failed(error)),
        Ok(Err(error)) => Err(ExploreSnapshotLoadError::Failed(format!(
            "Explore blocking stage failed: {error}"
        ))),
        Err(_) => {
            context.cancellation.cancel();
            Err(ExploreSnapshotLoadError::Deadline)
        }
    }
}

async fn load_explore_snapshot(
    database_path: &Path,
    workspace: &str,
    context: &ExploreExecutionContext,
) -> Result<ExploreSnapshotData, ExploreSnapshotLoadError> {
    let database_path = database_path.to_path_buf();
    let workspace = workspace.to_owned();
    let cancellation = context.cancellation.clone();
    let deadline = context.deadline;
    let result =
        run_bounded_explore_blocking(context, move || -> Result<ExploreSnapshotData, String> {
            let store =
                SqliteStore::open_read_only(&database_path).map_err(|error| error.to_string())?;
            store
                .interrupt_queries_when(move || {
                    cancellation.is_cancelled() || tokio::time::Instant::now() >= deadline
                })
                .map_err(|error| error.to_string())?;
            let registry = store
                .load_workspace_registry(&workspace)
                .map_err(|error| error.to_string())?;
            let freshness = store
                .load_current_freshness(&workspace)
                .map_err(|error| error.to_string())?;
            let (nodes, edges) = store
                .load_current_graph(&workspace)
                .map_err(|error| error.to_string())?;
            let evidence = store
                .load_current_evidence(&workspace)
                .map_err(|error| error.to_string())?;
            Ok(ExploreSnapshotData {
                registry,
                freshness,
                nodes,
                edges,
                evidence,
            })
        })
        .await;
    match result {
        Err(ExploreSnapshotLoadError::Failed(_))
            if context.expired() || context.cancellation.is_cancelled() =>
        {
            Err(ExploreSnapshotLoadError::Deadline)
        }
        other => other,
    }
}

struct ExploreProviderStage<'a> {
    provider: &'a CodeGraphProvider,
    repository: &'a RepositoryRecord,
    project_path: &'a Path,
    query: &'a str,
    context: &'a ExploreExecutionContext,
}

struct ExploreProviderInput<'a> {
    provider: &'a CodeGraphProvider,
    repository: &'a RepositoryRecord,
    project_path: &'a Path,
    query: &'a str,
    policy: &'a ExecutionPolicy,
    context: &'a ExploreExecutionContext,
    max_files: usize,
}

struct ExploreProviderOutcome {
    data: ExploreProviderData,
    ledger: ExploreBudgetLedger,
    anchors: Vec<ResolvedSymbol>,
}

async fn run_explore_provider_stages(input: ExploreProviderInput<'_>) -> ExploreProviderOutcome {
    let mut ledger = ExploreBudgetLedger::default();
    let mut data = ExploreProviderData::default();
    let stage = ExploreProviderStage {
        provider: input.provider,
        repository: input.repository,
        project_path: input.project_path,
        query: input.query,
        context: input.context,
    };
    let operation_limit = usize::try_from(input.policy.max_explore_codegraph_operations)
        .expect("validated policy count is usize-representable");
    let source_bytes = usize::try_from(input.policy.max_explore_source_markdown_bytes)
        .expect("validated policy bytes are usize-representable");
    let enrichment_bytes = usize::try_from(input.policy.max_explore_enrichment_bytes)
        .expect("validated policy bytes are usize-representable");
    let resolved_limit = usize::try_from(input.policy.max_explore_resolved_symbols)
        .expect("validated policy count is usize-representable");
    collect_explore_source(
        &stage,
        input.max_files,
        source_bytes,
        operation_limit,
        &mut ledger,
        &mut data,
    )
    .await;
    resolve_explore_symbols(
        &stage,
        operation_limit,
        enrichment_bytes,
        resolved_limit,
        &mut ledger,
        &mut data,
    )
    .await;
    let anchor_limit = usize::try_from(input.policy.max_explore_anchors)
        .expect("validated policy count is usize-representable");
    let anchors = select_explore_anchors(&data.resolved_symbols, anchor_limit);
    collect_explore_neighbors(
        &stage,
        &anchors,
        &ExploreNeighborLimits {
            operation_limit,
            enrichment_bytes,
            neighbor_limit: usize::try_from(input.policy.max_explore_neighbors_per_direction)
                .expect("validated policy count is usize-representable"),
            relationship_limit: usize::try_from(input.policy.max_explore_local_relationships)
                .expect("validated policy count is usize-representable"),
        },
        &mut ledger,
        &mut data,
    )
    .await;
    ExploreProviderOutcome {
        data,
        ledger,
        anchors,
    }
}

fn create_explore_provider(
    binary: Option<std::ffi::OsString>,
    policy: &ExecutionPolicy,
) -> Result<CodeGraphProvider, String> {
    let mut config = CodeGraphConfig {
        max_concurrent_processes: usize::try_from(
            policy.max_explore_concurrent_codegraph_processes,
        )
        .expect("validated policy count is usize-representable"),
        ..CodeGraphConfig::default()
    };
    if let Some(binary) = binary {
        config.binary = binary;
    }
    CodeGraphProvider::new(config).map_err(|error| error.to_string())
}

fn validate_explore_input(
    workspace: &str,
    input: &ExploreInput,
    policy: &ExecutionPolicy,
) -> Result<usize, String> {
    if input.workspace != workspace {
        return Err(format!(
            "workspace `{}` is outside this server's configured workspace `{workspace}`",
            input.workspace
        ));
    }
    let source_file_limit = usize::try_from(policy.max_explore_source_files)
        .expect("validated policy count is usize-representable");
    let max_files = input.max_files.unwrap_or(source_file_limit.min(12));
    if max_files == 0 || max_files > source_file_limit {
        return Err(format!(
            "max_files must be between 1 and the effective workspace limit ({source_file_limit})"
        ));
    }
    Ok(max_files)
}

async fn collect_explore_source(
    stage: &ExploreProviderStage<'_>,
    max_files: usize,
    source_bytes: usize,
    operation_limit: usize,
    ledger: &mut ExploreBudgetLedger,
    data: &mut ExploreProviderData,
) {
    if !ledger.try_reserve_operations(1, operation_limit, stage.context) {
        ledger.gaps.push(
            "source context skipped because the Explore deadline or operation budget was exhausted"
                .to_owned(),
        );
        return;
    }
    let result = stage
        .provider
        .build_local_context(LocalContextRequest {
            request: explore_provider_request(
                stage.repository,
                stage.project_path,
                source_bytes,
                max_files,
                stage.context.deadline,
                &stage.context.cancellation,
            ),
            query: stage.query.to_owned(),
            max_files,
        })
        .await;
    match result {
        Ok(result) => {
            data.source_markdown = truncate_utf8_owned(result.content, source_bytes);
            data.source_context = true;
            if result.execution.truncated {
                ledger
                    .truncations
                    .push("maxExploreSourceMarkdownBytes".to_owned());
            }
            ledger.record_execution(result.execution);
        }
        Err(error) => ledger
            .gaps
            .push(format!("source context unavailable: {error}")),
    }
}

async fn resolve_explore_symbols(
    stage: &ExploreProviderStage<'_>,
    operation_limit: usize,
    enrichment_bytes: usize,
    resolved_limit: usize,
    ledger: &mut ExploreBudgetLedger,
    data: &mut ExploreProviderData,
) {
    if ledger.try_reserve_operations(1, operation_limit, stage.context) {
        let result = stage
            .provider
            .resolve_symbols(code_system_graph_core::ResolveSymbolsRequest {
                request: explore_provider_request(
                    stage.repository,
                    stage.project_path,
                    enrichment_bytes,
                    resolved_limit,
                    stage.context.deadline,
                    &stage.context.cancellation,
                ),
                query: stage.query.to_owned(),
            })
            .await;
        match result {
            Ok(mut result) => {
                data.symbol_resolution = true;
                if result.symbols.len() > resolved_limit || result.execution.truncated {
                    ledger
                        .truncations
                        .push("maxExploreResolvedSymbols".to_owned());
                }
                result.symbols.truncate(resolved_limit);
                data.resolved_symbols = result.symbols;
                ledger.retain_enrichment_bytes(result.execution.output_bytes, enrichment_bytes);
                ledger.record_execution(result.execution);
            }
            Err(error) => ledger
                .gaps
                .push(format!("symbol resolution unavailable: {error}")),
        }
    } else {
        ledger.gaps.push(
            "symbol resolution skipped because the Explore deadline or operation budget was exhausted"
                .to_owned(),
        );
    }
    if data.resolved_symbols.is_empty() {
        data.resolved_symbols = fallback_explore_anchors(&data.source_markdown, resolved_limit);
        ledger.gaps.push(if data.resolved_symbols.is_empty() {
            "no Explore anchors were available from structured symbol resolution or recognized CodeGraph blast-radius entries".to_owned()
        } else {
            "structured symbol anchors were unavailable; strict fallback anchors were derived from explicit CodeGraph blast-radius entries".to_owned()
        });
    }
}

struct ExploreNeighborLimits {
    operation_limit: usize,
    enrichment_bytes: usize,
    neighbor_limit: usize,
    relationship_limit: usize,
}

async fn collect_explore_neighbors(
    stage: &ExploreProviderStage<'_>,
    anchors: &[ResolvedSymbol],
    limits: &ExploreNeighborLimits,
    ledger: &mut ExploreBudgetLedger,
    data: &mut ExploreProviderData,
) {
    for anchor in anchors {
        let remaining = ledger.remaining_enrichment_bytes(limits.enrichment_bytes);
        if remaining < 2 {
            ledger
                .truncations
                .push("maxExploreEnrichmentBytes".to_owned());
            break;
        }
        if !ledger.try_reserve_operations(2, limits.operation_limit, stage.context) {
            ledger.gaps.push(format!(
                "neighbors for `{}` skipped because the Explore deadline or operation budget was exhausted",
                anchor.name
            ));
            break;
        }
        let symbol = anchor
            .qualified_name
            .clone()
            .unwrap_or_else(|| anchor.name.clone());
        let request = |direction, bytes| LocalNeighborsRequest {
            request: explore_provider_request(
                stage.repository,
                stage.project_path,
                bytes,
                limits.neighbor_limit,
                stage.context.deadline,
                &stage.context.cancellation,
            ),
            symbol: symbol.clone(),
            direction,
        };
        let (incoming, outgoing) = tokio::join!(
            stage
                .provider
                .get_local_neighbors(request(LocalNeighborDirection::Callers, remaining / 2,)),
            stage.provider.get_local_neighbors(request(
                LocalNeighborDirection::Callees,
                remaining - (remaining / 2),
            )),
        );
        let anchor_completed =
            record_explore_neighbor_results(&symbol, incoming, outgoing, limits, ledger, data);
        data.anchors_traversed += usize::from(anchor_completed);
        if data.local_relationships.len() >= limits.relationship_limit {
            data.local_relationships.truncate(limits.relationship_limit);
            ledger
                .truncations
                .push("maxExploreLocalRelationships".to_owned());
            break;
        }
    }
}

fn record_explore_neighbor_results(
    symbol: &str,
    incoming: Result<LocalNeighborResult, ProviderError>,
    outgoing: Result<LocalNeighborResult, ProviderError>,
    limits: &ExploreNeighborLimits,
    ledger: &mut ExploreBudgetLedger,
    data: &mut ExploreProviderData,
) -> bool {
    let mut completed_directions = 0_usize;
    for (direction, result) in [("callers", incoming), ("callees", outgoing)] {
        match result {
            Ok(mut result) => {
                completed_directions += 1;
                if result.neighbors.len() > limits.neighbor_limit || result.execution.truncated {
                    ledger
                        .truncations
                        .push("maxExploreNeighborsPerDirection".to_owned());
                }
                result.neighbors.truncate(limits.neighbor_limit);
                ledger.retain_enrichment_bytes(
                    result.execution.output_bytes,
                    limits.enrichment_bytes,
                );
                data.local_relationships
                    .extend(result.neighbors.into_iter().map(|neighbor| {
                        ExploreLocalRelationship {
                            anchor: symbol.to_owned(),
                            direction: result.direction,
                            neighbor,
                        }
                    }));
                ledger.record_execution(result.execution);
            }
            Err(error) => ledger.gaps.push(format!(
                "{direction} neighbors for `{symbol}` degraded: {error}"
            )),
        }
    }
    completed_directions == 2
}

async fn correlate_explore_with_deadline(
    input: ExploreCorrelationInput,
    context: &ExploreExecutionContext,
    ledger: &mut ExploreBudgetLedger,
) -> Vec<ExploreFederatedHandoff> {
    let cancellation = context.cancellation.clone();
    let deadline = context.deadline;
    let result = run_bounded_explore_blocking(context, move || {
        correlate_explore_handoffs(&input, || {
            cancellation.is_cancelled() || tokio::time::Instant::now() >= deadline
        })
        .map_err(|()| "federated handoff correlation was cancelled".to_owned())
    })
    .await;
    match result {
        Ok(ExploreCorrelationOutput {
            handoffs,
            truncations,
        }) => {
            ledger.truncations.extend(truncations);
            handoffs
        }
        Err(ExploreSnapshotLoadError::Failed(error)) => {
            ledger
                .gaps
                .push(format!("federated handoff correlation failed: {error}"));
            Vec::new()
        }
        Err(ExploreSnapshotLoadError::Deadline) => {
            ledger
                .gaps
                .push("federated handoff correlation exceeded maxExploreWallTimeMs".to_owned());
            Vec::new()
        }
    }
}

struct ExploreEnvelopeInput {
    repository: RepositoryRecord,
    persisted_freshness: Vec<RepoFreshness>,
    freshness: FreshnessSummary,
    provider_data: ExploreProviderData,
    federated_handoffs: Vec<ExploreFederatedHandoff>,
    next_actions: Vec<AgentNextAction>,
    policy: ExecutionPolicy,
    maximum_concurrency_observed: usize,
    ledger: ExploreBudgetLedger,
}

fn finish_explore_envelope(mut input: ExploreEnvelopeInput) -> ToolEnvelope<ExploreReport> {
    input.ledger.normalize();
    let status = if input.ledger.gaps.is_empty()
        && input.ledger.degradations.is_empty()
        && input.freshness.overall == OverallFreshness::Fresh
    {
        ToolStatus::Ok
    } else {
        ToolStatus::Degraded
    };
    let retained_bytes = input
        .ledger
        .operations
        .iter()
        .map(|item| item.output_bytes)
        .sum();
    ToolEnvelope {
        schema_version: 2,
        status,
        data: Some(ExploreReport {
            repository: explore_repository_context(&input.repository, &input.persisted_freshness),
            source_markdown: input.provider_data.source_markdown,
            resolved_symbols: input.provider_data.resolved_symbols,
            local_relationships: input.provider_data.local_relationships,
            federated_handoffs: input.federated_handoffs,
            coverage: ExploreCoverage {
                source_context: input.provider_data.source_context,
                symbol_resolution: input.provider_data.symbol_resolution,
                anchors_traversed: input.provider_data.anchors_traversed,
                gaps: input.ledger.gaps,
                truncations: input.ledger.truncations,
            },
            next_actions: input.next_actions,
            execution: ExploreExecution {
                effective_policy: input.policy,
                provider_operations: input.ledger.provider_operations,
                maximum_concurrency_observed: input.maximum_concurrency_observed,
                retained_bytes,
                operations: input.ledger.operations,
                degradations: input.ledger.degradations.clone(),
            },
        }),
        freshness: input.freshness,
        warnings: input.ledger.degradations,
    }
}

/// Explores one registered checkout through bounded, ephemeral local code intelligence.
///
/// The returned [`ExploreReport::source_markdown`] may contain source code and must never be
/// persisted, logged, cached, or included in audit records.
///
/// # Panics
///
/// Panics if `policy` was manually constructed with a count or byte limit that cannot be
/// represented by the current platform. Server entry points always pass a validated, resolved
/// policy.
pub async fn explore_repository(
    database_path: &Path,
    workspace: &str,
    input: &ExploreInput,
    binary: Option<std::ffi::OsString>,
    policy: &ExecutionPolicy,
) -> ToolEnvelope<ExploreReport> {
    let execution_context = ExploreExecutionContext::new(policy);
    let max_files = match validate_explore_input(workspace, input, policy) {
        Ok(max_files) => max_files,
        Err(error) => return explore_error_envelope(error),
    };
    let snapshot = match load_explore_snapshot(database_path, workspace, &execution_context).await {
        Ok(snapshot) => snapshot,
        Err(ExploreSnapshotLoadError::Failed(error)) => return explore_error_envelope(error),
        Err(ExploreSnapshotLoadError::Deadline) => {
            execution_context.cancellation.cancel();
            return explore_deadline_envelope("snapshot loading");
        }
    };
    let ExploreSnapshotData {
        registry,
        freshness: persisted_freshness,
        nodes,
        edges,
        evidence,
    } = snapshot;
    let freshness = freshness_summary(&persisted_freshness);
    let requested_alias = input
        .repository
        .as_deref()
        .map(str::trim)
        .filter(|alias| !alias.is_empty());
    let repository = match select_explore_repository(&registry, requested_alias, workspace) {
        Ok(repository) => repository.clone(),
        Err(message) => {
            return explore_scoped_error_envelope(freshness, ToolStatus::Error, message);
        }
    };
    let provider = match create_explore_provider(binary, policy) {
        Ok(provider) => provider,
        Err(message) => {
            return explore_scoped_error_envelope(freshness, ToolStatus::Degraded, message);
        }
    };
    let project_path = native_relative_path(&repository.canonical_path);
    let outcome = run_explore_provider_stages(ExploreProviderInput {
        provider: &provider,
        repository: &repository,
        project_path: &project_path,
        query: &input.query,
        policy,
        context: &execution_context,
        max_files,
    })
    .await;
    let ExploreProviderOutcome {
        data: provider_data,
        mut ledger,
        anchors,
    } = outcome;

    let repository_contexts = explore_repository_contexts(&registry, &persisted_freshness);
    let federated_handoffs = correlate_explore_with_deadline(
        ExploreCorrelationInput {
            repository: repository.clone(),
            anchors,
            nodes,
            edges,
            evidence,
            repositories: repository_contexts,
            policy: policy.clone(),
        },
        &execution_context,
        &mut ledger,
    )
    .await;
    let next_actions = explore_next_actions(
        workspace,
        &repository,
        &input.query,
        &federated_handoffs,
        policy,
    );
    let shutdown = provider.shutdown().await;
    if let Err(error) = shutdown {
        ledger
            .degradations
            .push(format!("CodeGraph shutdown degraded: {error}"));
    }
    finish_explore_envelope(ExploreEnvelopeInput {
        repository,
        persisted_freshness,
        freshness,
        provider_data,
        federated_handoffs,
        next_actions,
        policy: policy.clone(),
        maximum_concurrency_observed: provider.maximum_concurrency_observed(),
        ledger,
    })
}

struct ExploreExecutionContext {
    deadline: tokio::time::Instant,
    cancellation: CancellationToken,
}

impl ExploreExecutionContext {
    fn new(policy: &ExecutionPolicy) -> Self {
        Self {
            deadline: tokio::time::Instant::now()
                + std::time::Duration::from_millis(policy.max_explore_wall_time_ms),
            cancellation: CancellationToken::new(),
        }
    }

    fn expired(&self) -> bool {
        tokio::time::Instant::now() >= self.deadline
    }
}

#[derive(Default)]
struct ExploreBudgetLedger {
    operations: Vec<ProviderExecution>,
    degradations: Vec<String>,
    truncations: Vec<String>,
    gaps: Vec<String>,
    provider_operations: usize,
    enrichment_retained_bytes: usize,
}

impl ExploreBudgetLedger {
    fn try_reserve_operations(
        &mut self,
        count: usize,
        limit: usize,
        context: &ExploreExecutionContext,
    ) -> bool {
        if context.expired() || self.provider_operations.saturating_add(count) > limit {
            return false;
        }
        self.provider_operations += count;
        true
    }

    fn remaining_enrichment_bytes(&self, limit: usize) -> usize {
        limit.saturating_sub(self.enrichment_retained_bytes)
    }

    fn retain_enrichment_bytes(&mut self, bytes: usize, limit: usize) {
        self.enrichment_retained_bytes = self
            .enrichment_retained_bytes
            .saturating_add(bytes)
            .min(limit);
    }

    fn record_execution(&mut self, execution: ProviderExecution) {
        self.degradations.extend(
            execution
                .degradations
                .iter()
                .map(|item| item.message.clone()),
        );
        self.operations.push(execution);
    }

    fn normalize(&mut self) {
        self.truncations.sort();
        self.truncations.dedup();
        self.gaps.sort();
        self.gaps.dedup();
        self.degradations.sort();
        self.degradations.dedup();
    }
}

#[derive(Default)]
struct ExploreProviderData {
    source_markdown: String,
    source_context: bool,
    resolved_symbols: Vec<ResolvedSymbol>,
    symbol_resolution: bool,
    local_relationships: Vec<ExploreLocalRelationship>,
    anchors_traversed: usize,
}

#[cfg(test)]
mod tests;

//! Bounded Explore orchestration.

mod correlation;
mod model;
mod provider;
mod runtime;

use code_system_graph_core::{ExecutionPolicy, ResolvedSymbol};
use correlation::{
    ExploreCorrelationInput, ExploreCorrelationOutput, correlate_explore_handoffs, explore_repository_context, explore_repository_contexts
};
pub use model::{
    ExploreCoverage, ExploreEvidenceLocation, ExploreExecution, ExploreFederatedHandoff, ExploreInput, ExploreLocalRelationship, ExploreReport, ExploreRepositoryContext
};
use provider::{
    ExploreProviderInput, ExploreProviderOutcome, create_explore_provider, run_explore_provider_stages, validate_explore_input
};
use runtime::{
    ExploreBlockingError, ExploreBudgetLedger, ExploreExecutionContext, ExploreProviderData, ExploreSnapshotData, load_explore_snapshot, run_bounded_explore_blocking
};
use tokio_util::sync::CancellationToken;

use crate::{
    AgentNextAction, BTreeMap, BTreeSet, Duration, FreshnessSummary, LocalCodeIntelligenceProvider, OverallFreshness, Path, ProviderBudget, ProviderRequest, RepoFreshness, RepositoryRecord, ToolEnvelope, ToolStatus, WorkspaceRecord, freshness_summary, native_relative_path
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

fn explore_deadline_without_context(stage: &str) -> ToolEnvelope<ExploreReport> {
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
            "{stage} exceeded maxExploreWallTimeMs before repository context was available"
        )],
    }
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
        Err(ExploreBlockingError::Failed(_))
            if context.expired() || context.cancellation.is_cancelled() =>
        {
            ledger
                .gaps
                .push("federated handoff correlation exceeded maxExploreWallTimeMs".to_owned());
            Vec::new()
        }
        Err(ExploreBlockingError::Failed(error)) => {
            ledger
                .gaps
                .push(format!("federated handoff correlation failed: {error}"));
            Vec::new()
        }
        Err(ExploreBlockingError::Deadline) => {
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

fn snapshot_freshness(snapshot: &ExploreSnapshotData) -> FreshnessSummary {
    if snapshot.freshness_loaded {
        freshness_summary(&snapshot.freshness)
    } else {
        FreshnessSummary {
            overall: OverallFreshness::Unknown,
            stale_repositories: Vec::new(),
            reasons: vec!["Persisted freshness was not loaded before Explore degraded.".to_owned()],
        }
    }
}

fn select_requested_repository(
    snapshot: &ExploreSnapshotData,
    requested: Option<&str>,
    workspace: &str,
) -> Result<RepositoryRecord, String> {
    let requested = requested.map(str::trim).filter(|alias| !alias.is_empty());
    select_explore_repository(&snapshot.registry, requested, workspace).cloned()
}

fn partial_snapshot_envelope(
    workspace: &str,
    input: &ExploreInput,
    repository: RepositoryRecord,
    snapshot: ExploreSnapshotData,
    policy: &ExecutionPolicy,
) -> ToolEnvelope<ExploreReport> {
    let stage = snapshot.incomplete_stage.unwrap_or("snapshot loading");
    let freshness = snapshot_freshness(&snapshot);
    let mut ledger = ExploreBudgetLedger::default();
    ledger.gaps.extend(snapshot.gaps);
    ledger.gaps.push(format!(
        "{stage} exceeded maxExploreWallTimeMs; no later Explore stages were started"
    ));
    finish_explore_envelope(ExploreEnvelopeInput {
        next_actions: explore_next_actions(workspace, &repository, &input.query, &[], policy),
        repository,
        persisted_freshness: snapshot.freshness,
        freshness,
        provider_data: ExploreProviderData::default(),
        federated_handoffs: Vec::new(),
        policy: policy.clone(),
        maximum_concurrency_observed: 0,
        ledger,
    })
}

fn provider_unavailable_envelope(
    workspace: &str,
    query: &str,
    repository: RepositoryRecord,
    snapshot: ExploreSnapshotData,
    freshness: FreshnessSummary,
    policy: &ExecutionPolicy,
    message: &str,
) -> ToolEnvelope<ExploreReport> {
    let mut ledger = ExploreBudgetLedger::default();
    ledger.gaps.extend(snapshot.gaps);
    ledger
        .gaps
        .push(format!("CodeGraph provider unavailable: {message}"));
    finish_explore_envelope(ExploreEnvelopeInput {
        next_actions: explore_next_actions(workspace, &repository, query, &[], policy),
        repository,
        persisted_freshness: snapshot.freshness,
        freshness,
        provider_data: ExploreProviderData::default(),
        federated_handoffs: Vec::new(),
        policy: policy.clone(),
        maximum_concurrency_observed: 0,
        ledger,
    })
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
        Err(ExploreBlockingError::Failed(error)) => return explore_error_envelope(error),
        Err(ExploreBlockingError::Deadline) => {
            execution_context.cancellation.cancel();
            return explore_deadline_without_context("workspace registry loading");
        }
    };
    let freshness = snapshot_freshness(&snapshot);
    let repository =
        match select_requested_repository(&snapshot, input.repository.as_deref(), workspace) {
            Ok(repository) => repository,
            Err(message) => {
                return explore_scoped_error_envelope(freshness, ToolStatus::Error, message);
            }
        };
    if snapshot.incomplete_stage.is_some() {
        return partial_snapshot_envelope(workspace, input, repository, snapshot, policy);
    }
    let provider = match create_explore_provider(binary, policy) {
        Ok(provider) => provider,
        Err(message) => {
            return provider_unavailable_envelope(
                workspace,
                &input.query,
                repository,
                snapshot,
                freshness,
                policy,
                &message,
            );
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
    ledger.gaps.extend(snapshot.gaps);

    let repository_contexts = explore_repository_contexts(&snapshot.registry, &snapshot.freshness);
    let federated_handoffs = correlate_explore_with_deadline(
        ExploreCorrelationInput {
            repository: repository.clone(),
            anchors,
            nodes: snapshot.nodes,
            edges: snapshot.edges,
            evidence: snapshot.evidence,
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
    if let Err(error) = provider.shutdown().await {
        ledger
            .degradations
            .push(format!("CodeGraph shutdown degraded: {error}"));
    }
    finish_explore_envelope(ExploreEnvelopeInput {
        repository,
        persisted_freshness: snapshot.freshness,
        freshness,
        provider_data,
        federated_handoffs,
        next_actions,
        policy: policy.clone(),
        maximum_concurrency_observed: provider.maximum_concurrency_observed(),
        ledger,
    })
}

#[cfg(test)]
mod tests;

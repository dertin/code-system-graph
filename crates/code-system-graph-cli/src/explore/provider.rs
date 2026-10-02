//! Bounded `CodeGraph` provider stages for Explore.

use std::path::Path;

use code_system_graph_core::{
    ExecutionPolicy, LocalNeighbor, LocalNeighborResult, ProviderError, ResolvedSymbol
};
use code_system_graph_model::RepositoryRecord;

use super::runtime::{ExploreBudgetLedger, ExploreExecutionContext, ExploreProviderData};
use super::{
    ExploreInput, ExploreLocalRelationship, exact_query_symbols_with_source_fallback, explore_provider_request, fallback_explore_anchors, select_explore_anchors, source_markdown_for_exact_symbols, truncate_utf8_owned
};
use crate::{
    CodeGraphConfig, CodeGraphProvider, LocalCodeIntelligenceProvider, LocalContextRequest, LocalNeighborDirection, LocalNeighborsRequest
};

struct ExploreProviderStage<'a> {
    provider: &'a CodeGraphProvider,
    repository: &'a RepositoryRecord,
    project_path: &'a Path,
    query: &'a str,
    context: &'a ExploreExecutionContext,
}

pub(super) struct ExploreProviderInput<'a> {
    pub(super) provider: &'a CodeGraphProvider,
    pub(super) repository: &'a RepositoryRecord,
    pub(super) project_path: &'a Path,
    pub(super) query: &'a str,
    pub(super) policy: &'a ExecutionPolicy,
    pub(super) context: &'a ExploreExecutionContext,
    pub(super) max_files: usize,
}

pub(super) struct ExploreProviderOutcome {
    pub(super) data: ExploreProviderData,
    pub(super) ledger: ExploreBudgetLedger,
    pub(super) anchors: Vec<ResolvedSymbol>,
}

pub(super) async fn run_explore_provider_stages(
    input: ExploreProviderInput<'_>,
) -> ExploreProviderOutcome {
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
    let exact_symbols = exact_query_symbols_with_source_fallback(
        input.query,
        &data.resolved_symbols,
        &data.source_markdown,
        resolved_limit,
    );
    recover_exact_file_source(
        &stage,
        &exact_symbols,
        input.max_files,
        source_bytes,
        operation_limit,
        &mut ledger,
        &mut data,
    )
    .await;
    if !exact_symbols.is_empty() {
        let (source_markdown, narrowed_source_truncated) =
            source_markdown_for_exact_symbols(&data.source_markdown, &exact_symbols, source_bytes);
        data.source_markdown = source_markdown;
        if narrowed_source_truncated {
            ledger
                .truncations
                .push("maxExploreSourceMarkdownBytes".to_owned());
        }
        data.resolved_symbols = exact_symbols;
    }
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

async fn recover_exact_file_source(
    stage: &ExploreProviderStage<'_>,
    exact_symbols: &[ResolvedSymbol],
    max_files: usize,
    source_bytes: usize,
    operation_limit: usize,
    ledger: &mut ExploreBudgetLedger,
    data: &mut ExploreProviderData,
) {
    if data.source_context || exact_symbols.is_empty() {
        return;
    }
    let file_paths = exact_symbols
        .iter()
        .map(|symbol| symbol.file_path.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .take(max_files);
    let mut recovered = String::new();
    for file_path in file_paths {
        if !ledger.try_reserve_operations(1, operation_limit, stage.context) {
            ledger.gaps.push(
                "exact file source fallback stopped because the Explore deadline or operation budget was exhausted"
                    .to_owned(),
            );
            break;
        }
        let remaining = source_bytes.saturating_sub(recovered.len());
        if remaining == 0 {
            ledger
                .truncations
                .push("maxExploreSourceMarkdownBytes".to_owned());
            break;
        }
        let result = stage
            .provider
            .build_local_file_context(
                LocalContextRequest {
                    request: explore_provider_request(
                        stage.repository,
                        stage.project_path,
                        remaining,
                        1,
                        stage.context.deadline,
                        &stage.context.cancellation,
                    ),
                    query: file_path.to_owned(),
                    max_files: 1,
                },
                file_path,
            )
            .await;
        match result {
            Ok(result) => {
                if let Some(source) = retained_provider_source(result.content, remaining) {
                    if !recovered.is_empty() {
                        recovered.push_str("\n\n");
                    }
                    recovered.push_str(&source);
                }
                if result.execution.truncated {
                    ledger
                        .truncations
                        .push("maxExploreSourceMarkdownBytes".to_owned());
                }
                ledger.record_execution(result.execution);
            }
            Err(error) => ledger.gaps.push(format!(
                "exact file source for `{file_path}` unavailable: {error}"
            )),
        }
    }
    if recovered.is_empty() {
        ledger.gaps.push(
            "source context did not return source for the resolved exact file anchors".to_owned(),
        );
    } else {
        data.source_markdown = recovered;
        data.source_context = true;
    }
}

fn retained_provider_source(content: String, maximum_bytes: usize) -> Option<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() || is_codegraph_no_match(trimmed) {
        return None;
    }
    Some(truncate_utf8_owned(content, maximum_bytes))
}

fn is_codegraph_no_match(content: &str) -> bool {
    let line = content.trim();
    if line.contains(['\n', '\r']) {
        return false;
    }
    let Some(query) = line
        .strip_prefix("No relevant code found for \"")
        .and_then(|remainder| {
            remainder
                .strip_suffix("\".")
                .or_else(|| remainder.strip_suffix('\"'))
        })
    else {
        return false;
    };
    !query.is_empty() && !query.contains('\"')
}

pub(super) fn create_explore_provider(
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

pub(super) fn validate_explore_input(
    workspace: &str,
    input: &ExploreInput,
    policy: &ExecutionPolicy,
) -> Result<usize, String> {
    if input
        .workspace
        .as_deref()
        .is_some_and(|requested| requested != workspace)
    {
        return Err(format!(
            "workspace `{}` is outside this server's configured workspace `{workspace}`",
            input.workspace.as_deref().unwrap_or_default()
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
            if let Some(source) = retained_provider_source(result.content, source_bytes) {
                data.source_markdown = source;
                data.source_context = true;
            }
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

pub(super) struct ExploreNeighborLimits {
    pub(super) operation_limit: usize,
    pub(super) enrichment_bytes: usize,
    pub(super) neighbor_limit: usize,
    pub(super) relationship_limit: usize,
}

async fn collect_explore_neighbors(
    stage: &ExploreProviderStage<'_>,
    anchors: &[ResolvedSymbol],
    limits: &ExploreNeighborLimits,
    ledger: &mut ExploreBudgetLedger,
    data: &mut ExploreProviderData,
) {
    for (anchor_index, anchor) in anchors.iter().enumerate() {
        if !supports_exact_local_relationships(anchor) {
            ledger.gaps.push(format!(
                "local caller/callee relationships for method `{}` were omitted because the provider does not prove receiver dispatch",
                anchor
                    .qualified_name
                    .as_deref()
                    .unwrap_or(anchor.name.as_str())
            ));
            continue;
        }
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
                .get_local_neighbors(request(LocalNeighborDirection::Callers, remaining / 2)),
            stage.provider.get_local_neighbors(request(
                LocalNeighborDirection::Callees,
                remaining - (remaining / 2),
            )),
        );
        let anchor_completed =
            record_explore_neighbor_results(&symbol, incoming, outgoing, limits, ledger, data);
        data.anchors_traversed += usize::from(anchor_completed);
        let observed_relationships = data.local_relationships.len();
        if observed_relationships >= limits.relationship_limit {
            data.local_relationships.truncate(limits.relationship_limit);
            if local_relationships_truncated(
                observed_relationships,
                limits.relationship_limit,
                anchor_index + 1 < anchors.len(),
            ) {
                ledger
                    .truncations
                    .push("maxExploreLocalRelationships".to_owned());
            }
            break;
        }
    }
}

fn supports_exact_local_relationships(anchor: &ResolvedSymbol) -> bool {
    !anchor.kind.eq_ignore_ascii_case("method")
}

fn unproven_receiver_dispatch(neighbor: &LocalNeighbor) -> bool {
    neighbor.kind.eq_ignore_ascii_case("method")
}

pub(super) fn local_relationships_truncated(
    observed: usize,
    maximum: usize,
    anchors_remaining: bool,
) -> bool {
    observed > maximum || (observed == maximum && anchors_remaining)
}

pub(super) fn record_explore_neighbor_results(
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
                let provider_neighbor_count = result.neighbors.len();
                result
                    .neighbors
                    .retain(|neighbor| !unproven_receiver_dispatch(neighbor));
                if result.neighbors.len() != provider_neighbor_count {
                    ledger.gaps.push(format!(
                        "generic method neighbors for `{symbol}` were omitted because the provider does not prove receiver dispatch"
                    ));
                }
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

#[cfg(test)]
mod tests {
    use code_system_graph_core::{LocalNeighbor, ResolvedSymbol};

    use super::{
        is_codegraph_no_match, local_relationships_truncated, retained_provider_source, supports_exact_local_relationships, unproven_receiver_dispatch
    };

    #[test]
    fn codegraph_no_match_should_be_an_absence_state_instead_of_source() {
        assert!(is_codegraph_no_match(
            "No relevant code found for \"client.py\""
        ));
        assert!(is_codegraph_no_match(
            "No relevant code found for \"client.py\"."
        ));
        assert_eq!(
            retained_provider_source("No relevant code found for \"client.py\"".to_owned(), 1_024,),
            None
        );
    }

    #[test]
    fn source_containing_the_no_match_phrase_should_remain_source() {
        let source =
            "```rust\nconst MESSAGE: &str = \"No relevant code found for client.py\";\n```";

        assert!(!is_codegraph_no_match(source));
        assert_eq!(
            retained_provider_source(source.to_owned(), 1_024).as_deref(),
            Some(source)
        );
    }

    #[test]
    fn exact_relationship_limit_only_truncates_when_work_remains() {
        assert!(!local_relationships_truncated(4, 4, false));
        assert!(local_relationships_truncated(4, 4, true));
        assert!(local_relationships_truncated(5, 4, false));
    }

    #[test]
    fn method_neighbors_should_be_omitted_when_receiver_dispatch_is_unproven() {
        let method = ResolvedSymbol {
            local_id: None,
            name: "get".to_owned(),
            qualified_name: Some("FakeClient::get".to_owned()),
            kind: "method".to_owned(),
            file_path: "src/lib.rs".to_owned(),
            start_line: 6,
            score: None,
        };
        let function = ResolvedSymbol {
            kind: "function".to_owned(),
            ..method.clone()
        };

        assert!(!supports_exact_local_relationships(&method));
        assert!(supports_exact_local_relationships(&function));
    }

    #[test]
    fn every_method_neighbor_should_require_proven_receiver_dispatch() {
        let method = LocalNeighbor {
            name: "execute".to_owned(),
            kind: "method".to_owned(),
            file_path: "src/lib.rs".to_owned(),
            start_line: 6,
        };
        let function = LocalNeighbor {
            kind: "function".to_owned(),
            ..method.clone()
        };

        assert!(unproven_receiver_dispatch(&method));
        assert!(!unproven_receiver_dispatch(&function));
    }
}

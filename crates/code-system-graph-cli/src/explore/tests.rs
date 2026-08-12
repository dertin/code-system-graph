//! Unit tests for independently bounded Explore stages.

use code_system_graph_core::{
    LocalNeighbor, LocalNeighborDirection, LocalNeighborResult, ProviderError, ProviderExecution, ProviderTransport, ResolvedSymbol
};
use code_system_graph_model::{
    CheckoutId, NativePath, NativePathEncoding, OverallFreshness, RepoId, RepositoryRecord, ToolStatus, WorkspaceId, WorkspaceRecord
};

use super::correlation::ExploreCorrelationInput;
use super::provider::{
    ExploreNeighborLimits, local_relationships_truncated, record_explore_neighbor_results
};
use super::runtime::{
    ExploreBlockingError, ExploreBudgetLedger, ExploreExecutionContext, ExploreProviderData, ExploreSnapshotData, run_bounded_explore_blocking
};
use super::{correlate_explore_with_deadline, explore_next_actions, partial_snapshot_envelope};

fn repository() -> RepositoryRecord {
    RepositoryRecord {
        id: RepoId::new("repo:api"),
        checkout_id: CheckoutId::new("checkout:api"),
        alias: "api".to_owned(),
        canonical_path: NativePath {
            encoding: NativePathEncoding::Utf8,
            bytes: b"/api".to_vec(),
            display: "/api".to_owned(),
        },
        git_common_dir: None,
        normalized_remote: None,
        head_commit: None,
        is_linked_worktree: false,
        working_tree_dirty: false,
    }
}

fn workspace(repository: RepositoryRecord) -> WorkspaceRecord {
    WorkspaceRecord {
        id: WorkspaceId::new("workspace:commerce"),
        name: "commerce".to_owned(),
        manifest_hash: "manifest".to_owned(),
        config_path: None,
        repositories: vec![repository],
    }
}

fn neighbor_result(direction: LocalNeighborDirection) -> LocalNeighborResult {
    LocalNeighborResult {
        symbol: "anchor".to_owned(),
        direction,
        neighbors: vec![LocalNeighbor {
            name: "neighbor".to_owned(),
            kind: "function".to_owned(),
            file_path: "src/lib.rs".to_owned(),
            start_line: 1,
        }],
        execution: ProviderExecution {
            transport: ProviderTransport::Cli,
            output_bytes: 32,
            truncated: false,
            degradations: Vec::new(),
        },
    }
}

#[test]
fn isolated_neighbor_direction_failure_should_not_complete_anchor() {
    for fail_callers in [true, false] {
        let incoming = if fail_callers {
            Err(ProviderError::Cancelled)
        } else {
            Ok(neighbor_result(LocalNeighborDirection::Callers))
        };
        let outgoing = if fail_callers {
            Ok(neighbor_result(LocalNeighborDirection::Callees))
        } else {
            Err(ProviderError::Cancelled)
        };
        let mut ledger = ExploreBudgetLedger::default();
        let mut data = ExploreProviderData::default();
        let completed = record_explore_neighbor_results(
            "anchor",
            incoming,
            outgoing,
            &ExploreNeighborLimits {
                operation_limit: 4,
                enrichment_bytes: 1_024,
                neighbor_limit: 4,
                relationship_limit: 8,
            },
            &mut ledger,
            &mut data,
        );

        assert!(!completed);
        assert_eq!(data.local_relationships.len(), 1);
        let failed_direction = if fail_callers { "callers" } else { "callees" };
        assert!(ledger.gaps.iter().any(|gap| gap.contains(failed_direction)));
    }
}

#[test]
fn both_neighbor_directions_should_complete_one_anchor() {
    let mut ledger = ExploreBudgetLedger::default();
    let mut data = ExploreProviderData::default();
    let completed = record_explore_neighbor_results(
        "anchor",
        Ok(neighbor_result(LocalNeighborDirection::Callers)),
        Ok(neighbor_result(LocalNeighborDirection::Callees)),
        &ExploreNeighborLimits {
            operation_limit: 4,
            enrichment_bytes: 1_024,
            neighbor_limit: 4,
            relationship_limit: 8,
        },
        &mut ledger,
        &mut data,
    );

    assert!(completed);
    assert_eq!(data.local_relationships.len(), 2);
    assert_eq!(ledger.gaps, [] as [String; 0]);
}

#[test]
fn recognized_blast_radius_entries_should_form_strict_fallback_anchors() {
    let source = "**Blast radius — what depends on these**\n\n\
        - `ExecutionPolicy` (crates/core/src/policy.rs:198) — 34 callers\n\
        - `unsafe` (../outside.rs:2) — rejected\n\
        - `second` (src/second.rs:7) — retained\n\n\
        **Source Code**\n\n\
        - `after` (src/after.rs:3) — ignored\n";
    let anchors = super::fallback_explore_anchors(source, 2);

    assert_eq!(anchors.len(), 2);
    assert_eq!(anchors[0].name, "ExecutionPolicy");
    assert_eq!(anchors[0].file_path, "crates/core/src/policy.rs");
    assert_eq!(anchors[0].start_line, 198);
    assert_eq!(anchors[1].name, "second");
}

#[test]
fn arbitrary_markdown_should_not_form_fallback_anchors() {
    let anchors = super::fallback_explore_anchors(
        "# Symbols\n\n- `invented` (src/invented.rs:9) — untrusted\n",
        5,
    );
    assert_eq!(anchors, [] as [code_system_graph_core::ResolvedSymbol; 0]);
}

#[test]
fn exact_symbol_in_query_should_exclude_approximate_resolutions() {
    let symbols = vec![
        ResolvedSymbol {
            local_id: Some("function:fetch".to_owned()),
            name: "fetchHugint".to_owned(),
            qualified_name: Some("fetchHugint".to_owned()),
            kind: "function".to_owned(),
            file_path: "frontend/fetchHugint.js".to_owned(),
            start_line: 220,
            score: Some(120.0),
        },
        ResolvedSymbol {
            local_id: Some("function:reset".to_owned()),
            name: "reset_local_database".to_owned(),
            qualified_name: Some("reset_local_database".to_owned()),
            kind: "function".to_owned(),
            file_path: "backend/local_bootstrap.py".to_owned(),
            start_line: 205,
            score: Some(38.0),
        },
    ];

    let exact = super::exact_query_symbols("Show exact symbol fetchHugint callers", &symbols);

    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].name, "fetchHugint");
}

#[test]
fn exact_symbol_source_should_retain_only_its_defining_file() {
    let source = "**Source Code**\n\n\
        **`backend/local_bootstrap.py`** — reset_local_database(function)\n\n\
        ```python\n205 def reset_local_database():\n```\n\n\
        **`frontend/fetchHugint.js`** — fetchHugint(function)\n\n\
        ```javascript\n220 export const fetchHugint = async () => {};\n```\n\n\
        **`frontend/errors.js`** — RequestError(class)\n\n\
        ```javascript\n1 export class RequestError {}\n```";
    let exact = ResolvedSymbol {
        local_id: Some("function:fetch".to_owned()),
        name: "fetchHugint".to_owned(),
        qualified_name: Some("fetchHugint".to_owned()),
        kind: "function".to_owned(),
        file_path: "frontend/fetchHugint.js".to_owned(),
        start_line: 220,
        score: Some(120.0),
    };

    let narrowed = super::source_markdown_for_exact_symbols(source, &[exact]);

    assert!(narrowed.contains("fetchHugint.js"));
    assert!(narrowed.contains("export const fetchHugint"));
    assert!(!narrowed.contains("local_bootstrap"));
    assert!(!narrowed.contains("errors.js"));
}

#[test]
fn blast_radius_should_recover_an_exact_symbol_missed_by_structured_resolution() {
    let approximate = ResolvedSymbol {
        local_id: Some("function:normalize".to_owned()),
        name: "normalizeBaseUrl".to_owned(),
        qualified_name: Some("normalizeBaseUrl".to_owned()),
        kind: "function".to_owned(),
        file_path: "frontend/fetchHugint.js".to_owned(),
        start_line: 64,
        score: Some(34.0),
    };
    let source = "**Blast radius — what depends on these**\n\n\
        - `fetchHugint` (frontend/fetchHugint.js:220) — 108 callers\n\n\
        **Source Code**";

    let exact = super::exact_query_symbols_with_source_fallback(
        "Símbolo exacto fetchHugint y sus callers",
        &[approximate],
        source,
        5,
    );

    assert_eq!(exact.len(), 1);
    assert_eq!(exact[0].name, "fetchHugint");
    assert_eq!(exact[0].start_line, 220);
}

#[test]
fn explore_follow_up_should_retain_the_required_query() {
    let actions = explore_next_actions(
        "commerce",
        &repository(),
        "create_order callers",
        &[],
        &code_system_graph_core::ExecutionPolicy::default(),
        true,
    );
    let action = actions
        .iter()
        .find(|action| action.tool == "explore")
        .expect("Explore should advertise one scoped follow-up");

    assert_eq!(
        action.arguments.get("query").map(String::as_str),
        Some("create_order callers")
    );
    assert_eq!(
        action.arguments.get("repository").map(String::as_str),
        Some("api")
    );
}

#[test]
fn completed_exact_exploration_should_not_suggest_repeating_explore() {
    let actions = explore_next_actions(
        "commerce",
        &repository(),
        "create_order",
        &[],
        &code_system_graph_core::ExecutionPolicy::default(),
        false,
    );

    assert_eq!(actions, []);
}

#[test]
fn local_relationship_limit_should_only_report_omitted_work() {
    assert!(!local_relationships_truncated(48, 48, false));
    assert!(local_relationships_truncated(48, 48, true));
    assert!(local_relationships_truncated(49, 48, false));
}

#[test]
fn later_snapshot_deadline_should_preserve_loaded_context_in_degraded_data() {
    let repository = repository();
    let snapshot = ExploreSnapshotData {
        registry: workspace(repository.clone()),
        freshness: Vec::new(),
        freshness_loaded: true,
        nodes: Vec::new(),
        edges: Vec::new(),
        evidence: Vec::new(),
        gaps: vec!["persisted graph unavailable".to_owned()],
        incomplete_stage: Some("evidence loading"),
    };
    let envelope = partial_snapshot_envelope(
        "commerce",
        &super::ExploreInput {
            workspace: "commerce".to_owned(),
            repository: Some("api".to_owned()),
            query: "create_order callers".to_owned(),
            max_files: None,
        },
        repository,
        snapshot,
        &code_system_graph_core::ExecutionPolicy::default(),
    );

    assert_eq!(envelope.status, ToolStatus::Degraded);
    assert_eq!(envelope.freshness.overall, OverallFreshness::Fresh);
    let report = envelope.data.expect("loaded registry should remain usable");
    assert_eq!(report.repository.alias, "api");
    assert!(
        report
            .coverage
            .gaps
            .iter()
            .any(|gap| gap.contains("persisted graph"))
    );
    assert!(
        report
            .coverage
            .gaps
            .iter()
            .any(|gap| gap.contains("evidence loading"))
    );
    assert_eq!(report.execution.provider_operations, 0);
}

#[tokio::test]
async fn blocking_stage_should_signal_cancellation_at_the_global_deadline() {
    let context = ExploreExecutionContext {
        deadline: tokio::time::Instant::now() + std::time::Duration::from_millis(5),
        cancellation: tokio_util::sync::CancellationToken::new(),
    };
    let result = run_bounded_explore_blocking(&context, || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        Ok::<_, String>(())
    })
    .await;

    assert!(matches!(result, Err(ExploreBlockingError::Deadline)));
    assert!(context.cancellation.is_cancelled());
}

#[tokio::test]
async fn correlation_stage_should_record_its_own_deadline_gap() {
    let context = ExploreExecutionContext {
        deadline: tokio::time::Instant::now(),
        cancellation: tokio_util::sync::CancellationToken::new(),
    };
    let mut ledger = ExploreBudgetLedger::default();
    let handoffs = correlate_explore_with_deadline(
        ExploreCorrelationInput {
            repository: repository(),
            anchors: vec![ResolvedSymbol {
                local_id: None,
                name: "anchor".to_owned(),
                qualified_name: None,
                kind: "function".to_owned(),
                file_path: "src/lib.rs".to_owned(),
                start_line: 1,
                score: None,
            }],
            nodes: Vec::new(),
            edges: Vec::new(),
            evidence: Vec::new(),
            repositories: std::collections::BTreeMap::new(),
            policy: code_system_graph_core::ExecutionPolicy::default(),
        },
        &context,
        &mut ledger,
    )
    .await;

    assert_eq!(handoffs, [] as [super::ExploreFederatedHandoff; 0]);
    assert!(
        ledger
            .gaps
            .iter()
            .any(|gap| { gap == "federated handoff correlation exceeded maxExploreWallTimeMs" })
    );
}

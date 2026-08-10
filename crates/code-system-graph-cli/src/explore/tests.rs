//! Unit tests for independently bounded Explore stages.

use code_system_graph_core::{
    LocalNeighbor, LocalNeighborDirection, LocalNeighborResult, ProviderError, ProviderExecution, ProviderTransport
};
use code_system_graph_model::{
    CheckoutId, NativePath, NativePathEncoding, RepoId, RepositoryRecord
};

use super::{
    ExploreBudgetLedger, ExploreNeighborLimits, ExploreProviderData, explore_next_actions, record_explore_neighbor_results
};

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
fn explore_follow_up_should_retain_the_required_query() {
    let actions = explore_next_actions(
        "commerce",
        &repository(),
        "create_order callers",
        &[],
        &code_system_graph_core::ExecutionPolicy::default(),
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

#[tokio::test]
async fn blocking_stage_should_signal_cancellation_at_the_global_deadline() {
    let context = super::ExploreExecutionContext {
        deadline: tokio::time::Instant::now() + std::time::Duration::from_millis(5),
        cancellation: tokio_util::sync::CancellationToken::new(),
    };
    let result = super::run_bounded_explore_blocking(&context, || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        Ok::<_, String>(())
    })
    .await;

    assert!(matches!(
        result,
        Err(super::ExploreSnapshotLoadError::Deadline)
    ));
    assert!(context.cancellation.is_cancelled());
}

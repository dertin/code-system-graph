//! Compile-time guards for exhaustively constructible public structs published in 1.0.2.

use code_system_graph_core::{ExecutionPolicy, ExecutionPolicyOverrides, RepositoryConfig};

#[test]
fn version_1_0_2_public_struct_literals_should_still_compile() {
    let repository = RepositoryConfig {
        path: "../service".to_owned(),
        openapi: None,
        http_consumers: None,
        integration_tests: None,
        implementations: None,
        excludes: None,
        include_defaults: None,
    };
    let overrides = ExecutionPolicyOverrides {
        max_scan_wall_time_ms: None,
        max_no_progress_time_ms: None,
        max_codegraph_sync_wall_time_ms_per_repo: None,
        max_worker_memory_bytes: None,
        graceful_termination_ms: None,
        watch_idle_timeout_ms: None,
        max_watch_session_wall_time_ms: None,
        min_watch_rescan_interval_ms: None,
        max_checkpoint_cache_bytes: None,
    };
    let policy = ExecutionPolicy {
        max_scan_wall_time_ms: 1,
        max_no_progress_time_ms: 1,
        max_codegraph_sync_wall_time_ms_per_repo: 1,
        max_worker_memory_bytes: 1,
        graceful_termination_ms: 1,
        watch_idle_timeout_ms: 1,
        max_watch_session_wall_time_ms: 1,
        min_watch_rescan_interval_ms: 1,
        max_checkpoint_cache_bytes: 1,
    };

    assert_eq!(repository.path, "../service");
    assert!(overrides.max_scan_wall_time_ms.is_none());
    assert_eq!(policy.max_scan_wall_time_ms, 1);
}

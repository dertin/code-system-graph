//! Compile-time guards for the intentionally breaking 1.1.0 public structs.

use code_system_graph_core::{ExecutionPolicy, ExecutionPolicyOverrides, RepositoryConfig};

#[test]
fn version_1_1_0_public_structs_should_remain_constructible() {
    let repository = RepositoryConfig {
        path: "../service".to_owned(),
        openapi: None,
        http_consumers: None,
        integration_tests: None,
        implementations: None,
        excludes: None,
        include_defaults: None,
    };
    let overrides = ExecutionPolicyOverrides::default();
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
        ..ExecutionPolicy::default()
    };

    assert_eq!(repository.path, "../service");
    assert!(overrides.max_scan_wall_time_ms.is_none());
    assert_eq!(policy.max_scan_wall_time_ms, 1);
}

#![cfg(unix)]

//! End-to-end public-process contract tests for the production `CodeGraph` adapter.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use code_system_graph_core::{
    AffectedTestsRequest, CodeGraphConfig, CodeGraphProvider, LocalCodeIntelligenceProvider, LocalContextRequest, LocalImpactRequest, LocalNeighborDirection, LocalNeighborsRequest, ProviderBudget, ProviderError, ProviderOperation, ProviderRequest, ProviderStatus, ProviderTransport, ResolveSymbolsRequest
};
use code_system_graph_model::RepoId;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

struct TestProvider {
    _temp: TempDir,
    provider: CodeGraphProvider,
    project_path: PathBuf,
}

impl TestProvider {
    fn new(mode: &str) -> Self {
        let temp = tempfile::tempdir().expect("temporary directory should be created");
        let project_path = temp.path().join("repository");
        fs::create_dir(&project_path).expect("project directory should be created");
        let binary = temp.path().join(format!("codegraph-{mode}"));
        fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/codegraph/fake/codegraph.py"
            ),
            &binary,
        )
        .expect("fake executable should be copied");
        let mut permissions = fs::metadata(&binary)
            .expect("fake executable metadata should exist")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&binary, permissions)
            .expect("fake executable should become executable");
        let provider = CodeGraphProvider::new(CodeGraphConfig {
            binary: binary.into_os_string(),
            max_concurrent_processes: 1,
            circuit_breaker_cooldown: Duration::from_secs(10),
        })
        .expect("provider config should be valid");
        Self {
            _temp: temp,
            provider,
            project_path,
        }
    }

    fn request(&self, timeout: Duration, max_output_bytes: usize) -> ProviderRequest {
        ProviderRequest {
            repo_id: RepoId::new("repo:fixture"),
            project_path: self.project_path.clone(),
            budget: ProviderBudget {
                timeout,
                max_output_bytes,
                max_items: 8,
            },
            cancellation: CancellationToken::new(),
        }
    }
}

#[tokio::test]
async fn provider_should_probe_mcp_and_execute_all_supported_operations() {
    let test = TestProvider::new("ok");
    let capability = test
        .provider
        .probe(test.request(Duration::from_secs(2), 64 * 1024))
        .await
        .expect("probe should succeed");

    assert_eq!(capability.status, ProviderStatus::Available);
    assert_eq!(capability.version.as_deref(), Some("1.5.0"));
    assert!(capability.operations.iter().any(|operation| {
        operation.operation == ProviderOperation::LocalContext
            && operation.transport == ProviderTransport::Mcp
    }));

    let context = test
        .provider
        .build_local_context(LocalContextRequest {
            request: test.request(Duration::from_secs(2), 64 * 1024),
            query: "anchor".to_owned(),
            max_files: 4,
        })
        .await
        .expect("MCP context should succeed");
    assert_eq!(context.execution.transport, ProviderTransport::Mcp);
    assert_eq!(context.content, "ephemeral local context");

    let symbols = test
        .provider
        .resolve_symbols(ResolveSymbolsRequest {
            request: test.request(Duration::from_secs(2), 64 * 1024),
            query: "anchor".to_owned(),
        })
        .await
        .expect("symbol query should succeed");
    assert_eq!(
        symbols.symbols[0].qualified_name.as_deref(),
        Some("fixture::anchor")
    );

    let neighbors = test
        .provider
        .get_local_neighbors(LocalNeighborsRequest {
            request: test.request(Duration::from_secs(2), 64 * 1024),
            symbol: "anchor".to_owned(),
            direction: LocalNeighborDirection::Callers,
        })
        .await
        .expect("neighbor query should succeed");
    assert_eq!(neighbors.neighbors[0].name, "neighbor");

    let impact = test
        .provider
        .get_local_impact(LocalImpactRequest {
            request: test.request(Duration::from_secs(2), 64 * 1024),
            symbol: "anchor".to_owned(),
            max_depth: 2,
        })
        .await
        .expect("impact query should succeed");
    assert_eq!(impact.provider_node_count, 1);

    let tests = test
        .provider
        .get_affected_tests(AffectedTestsRequest {
            request: test.request(Duration::from_secs(2), 64 * 1024),
            changed_files: vec!["src/lib.rs".to_owned()],
            max_depth: 3,
        })
        .await
        .expect("affected-tests query should succeed")
        .expect("CodeGraph 1.5 supports affected tests");
    assert_eq!(tests.affected_tests, vec!["tests/anchor.rs"]);
}

#[tokio::test]
async fn invalid_mcp_response_should_degrade_to_cli_context() {
    let test = TestProvider::new("invalid-mcp");
    let context = test
        .provider
        .build_local_context(LocalContextRequest {
            request: test.request(Duration::from_secs(2), 64 * 1024),
            query: "anchor".to_owned(),
            max_files: 4,
        })
        .await
        .expect("CLI fallback should succeed");

    assert_eq!(context.execution.transport, ProviderTransport::Cli);
    assert_eq!(context.content.trim(), "CLI local context");
    assert!(context.execution.degradations.iter().any(|degradation| {
        degradation.kind == "invalid_response" && degradation.diagnostics_id.is_some()
    }));
}

#[tokio::test]
async fn mcp_timeout_should_open_circuit_and_allow_next_cli_fallback() {
    let test = TestProvider::new("slow-mcp");
    let first = test
        .provider
        .build_local_context(LocalContextRequest {
            request: test.request(Duration::from_millis(500), 64 * 1024),
            query: "anchor".to_owned(),
            max_files: 4,
        })
        .await;
    assert!(
        matches!(first, Err(ProviderError::Timeout { .. })),
        "expected timeout, got {first:?}"
    );

    let second = test
        .provider
        .build_local_context(LocalContextRequest {
            request: test.request(Duration::from_secs(2), 64 * 1024),
            query: "anchor".to_owned(),
            max_files: 4,
        })
        .await
        .expect("open MCP circuit should use CLI");
    assert_eq!(second.execution.transport, ProviderTransport::Cli);
}

#[tokio::test]
async fn cli_should_enforce_cancellation_and_output_caps() {
    let slow = TestProvider::new("slow-cli");
    let cancellation = CancellationToken::new();
    let signal = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        signal.cancel();
    });
    let mut request = slow.request(Duration::from_secs(2), 64 * 1024);
    request.cancellation = cancellation;
    let cancelled = slow
        .provider
        .resolve_symbols(ResolveSymbolsRequest {
            request,
            query: "anchor".to_owned(),
        })
        .await;
    assert_eq!(cancelled, Err(ProviderError::Cancelled));

    let large = TestProvider::new("large");
    let limited = large
        .provider
        .resolve_symbols(ResolveSymbolsRequest {
            request: large.request(Duration::from_secs(2), 128),
            query: "anchor".to_owned(),
        })
        .await;
    assert!(matches!(limited, Err(ProviderError::OutputLimit { .. })));
}

#[tokio::test]
#[ignore = "requires a user-installed and explicitly indexed CodeGraph checkout"]
async fn live_codegraph_1_5_should_match_the_declared_public_contract() {
    let project_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root should be canonicalized");
    let provider =
        CodeGraphProvider::new(CodeGraphConfig::default()).expect("default config should be valid");
    let request = ProviderRequest {
        repo_id: RepoId::new("repo:live-smoke"),
        project_path,
        budget: ProviderBudget {
            timeout: Duration::from_secs(10),
            max_output_bytes: 1024 * 1024,
            max_items: 10,
        },
        cancellation: CancellationToken::new(),
    };
    let capability = provider
        .probe(request)
        .await
        .expect("live CodeGraph probe should succeed");

    assert_eq!(capability.version.as_deref(), Some("1.5.0"));
    assert!(matches!(
        capability.status,
        ProviderStatus::Available | ProviderStatus::Stale
    ));
    assert!(capability.operations.iter().any(|operation| {
        operation.operation == ProviderOperation::LocalContext
            && operation.transport == ProviderTransport::Mcp
    }));
}

//! End-to-end validation for the initial HTTP boundary slice.

use std::collections::BTreeMap;
use std::path::PathBuf;

use code_system_graph::{
    ScanOverrides, TraceInput, scan_workspace, scan_workspace_with_overrides, status_workspace, trace_workspace
};
use code_system_graph_core::{RegisteredWorkspace, parse_manifest, register_workspace};
use code_system_graph_model::{
    EdgeKind, ExtractorRunStatus, OverallFreshness, RepoFreshnessState, ToolEnvelope, ToolStatus, TraceReport, stable_id
};
use code_system_graph_store_sqlite::{SqliteStore, latest_schema_version};

#[derive(Debug, PartialEq, Eq)]
struct TraceObservation {
    node_count: usize,
    edge_count: usize,
    evidence_count: usize,
    tool_status: ToolStatus,
    segment_count: usize,
    schema_version: i64,
    freshness: OverallFreshness,
    discovered_inputs: usize,
    changed_inputs: usize,
    reused_snapshot: bool,
    repeated_changed_inputs: usize,
    repeated_reused_snapshot: bool,
    same_snapshot: bool,
    cross_language_edges: Vec<EdgeKind>,
}

fn repository_id(registry: &RegisteredWorkspace, alias: &str) -> anyhow::Result<String> {
    registry
        .record
        .repositories
        .iter()
        .find(|repository| repository.alias == alias)
        .map(|repository| repository.id.as_str().to_owned())
        .ok_or_else(|| anyhow::anyhow!("{alias} repository missing"))
}

fn segment_count(envelope: &ToolEnvelope<TraceReport>) -> usize {
    envelope
        .data
        .as_ref()
        .map_or(0, |report| report.segments.len())
}

fn edge_kinds(envelope: &ToolEnvelope<TraceReport>) -> Vec<EdgeKind> {
    envelope.data.as_ref().map_or_else(Vec::new, |report| {
        report
            .segments
            .iter()
            .map(|segment| segment.edge.kind)
            .collect()
    })
}

fn assert_contract_traces(
    database: &std::path::Path,
    web_repo: &str,
    api_repo: &str,
    worker_repo: &str,
) -> anyhow::Result<()> {
    let event_publisher = stable_id(
        "node",
        &format!("event-source:{api_repo}:src/lib.rs:Publisher:orders.created:Kafka"),
    );
    let event_subscriber = stable_id(
        "node",
        &format!("event-source:{worker_repo}:worker.py:Subscriber:orders.created:Kafka"),
    );
    let event_trace = trace_workspace(
        database,
        "commerce-platform",
        &TraceInput {
            from: event_publisher,
            to: event_subscriber,
            max_depth: 3,
        },
    )?;
    assert_eq!(
        edge_kinds(&event_trace),
        vec![EdgeKind::Publishes, EdgeKind::DeliversTo]
    );

    let grpc_client = stable_id(
        "node",
        &format!(
            "rpc:{worker_repo}:generated:orders_pb2_grpc.py:commerce.orders.v1.Orders/GetOrder:Client"
        ),
    );
    let grpc_provider = stable_id(
        "node",
        &format!("rpc:{api_repo}:provider:commerce.orders.v1.Orders/GetOrder"),
    );
    let grpc_trace = trace_workspace(
        database,
        "commerce-platform",
        &TraceInput {
            from: grpc_client,
            to: grpc_provider,
            max_depth: 2,
        },
    )?;
    assert_eq!(edge_kinds(&grpc_trace), vec![EdgeKind::CallsRemote]);

    let graphql_consumer = stable_id(
        "node",
        &format!("graphql:{web_repo}:consumer:src/orders.graphql:query:OrderDetails:order"),
    );
    let graphql_provider = stable_id("node", &format!("graphql:{api_repo}:provider:Query:order"));
    let graphql_trace = trace_workspace(
        database,
        "commerce-platform",
        &TraceInput {
            from: graphql_consumer,
            to: graphql_provider,
            max_depth: 2,
        },
    )?;
    assert_eq!(edge_kinds(&graphql_trace), vec![EdgeKind::CallsRemote]);
    Ok(())
}

fn assert_resource_traces(
    database: &std::path::Path,
    worker_repo: &str,
    infra_repo: &str,
    docs_repo: &str,
) -> anyhow::Result<()> {
    let data_reader = stable_id(
        "node",
        &format!("data-symbol:{worker_repo}:worker.py:load_order"),
    );
    let orders_table = stable_id("node", "table::commerce:orders");
    let data_trace = trace_workspace(
        database,
        "commerce-platform",
        &TraceInput {
            from: data_reader,
            to: orders_table,
            max_depth: 2,
        },
    )?;
    assert_eq!(edge_kinds(&data_trace), vec![EdgeKind::ReadsTable]);

    let repository = stable_id("node", &format!("repository:{infra_repo}"));
    let deployment = stable_id("node", "deployment:docker_compose_service::api");
    let compose_service = stable_id("node", &format!("service:{infra_repo}:api"));
    let repository_trace = trace_workspace(
        database,
        "commerce-platform",
        &TraceInput {
            from: repository,
            to: deployment.clone(),
            max_depth: 2,
        },
    )?;
    assert_eq!(edge_kinds(&repository_trace), vec![EdgeKind::Deploys]);
    let service_trace = trace_workspace(
        database,
        "commerce-platform",
        &TraceInput {
            from: deployment,
            to: compose_service,
            max_depth: 2,
        },
    )?;
    assert_eq!(edge_kinds(&service_trace), vec![EdgeKind::Provides]);

    let document = stable_id("node", &format!("document:{docs_repo}:README.md"));
    let service = stable_id("node", &format!("service:{infra_repo}:orders-api"));
    let documentation_trace = trace_workspace(
        database,
        "commerce-platform",
        &TraceInput {
            from: document,
            to: service,
            max_depth: 2,
        },
    )?;
    assert_eq!(edge_kinds(&documentation_trace), vec![EdgeKind::Documents]);

    let batches = SqliteStore::open_read_only(database)?
        .load_current_extractor_batches("commerce-platform")?;
    let persisted = batches
        .iter()
        .flat_map(|batch| batch.payload.iter().copied())
        .collect::<Vec<_>>();
    assert!(!String::from_utf8_lossy(&persisted).contains("never-persist"));
    Ok(())
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "The E2E validates one atomic multi-contract snapshot and its trace identities"
)]
fn scan_and_trace_should_link_python_test_to_rust_implementation() -> anyhow::Result<()> {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/platform-demo/code-system-graph.yaml");
    let temporary = tempfile::tempdir()?;
    let database = temporary.path().join("code-system-graph.db");

    let summary = scan_workspace(&fixture, &database)?;
    let repeated_summary = scan_workspace(&fixture, &database)?;
    let manifest_source = std::fs::read_to_string(&fixture)?;
    let manifest = parse_manifest(&manifest_source)?;
    let registry = register_workspace(&fixture, &manifest_source, &manifest)?;
    let web_repo = repository_id(&registry, "web")?;
    let api_repo = repository_id(&registry, "api")?;
    let tests_repo = repository_id(&registry, "tests")?;
    let worker_repo = repository_id(&registry, "worker")?;
    let infra_repo = repository_id(&registry, "infra")?;
    let docs_repo = repository_id(&registry, "docs")?;
    let from = stable_id(
        "node",
        &format!("http:{web_repo}:consumer:POST:/api/orders"),
    );
    let to = stable_id(
        "node",
        &format!("http:{api_repo}:provider:POST:/api/orders"),
    );
    let envelope = trace_workspace(
        &database,
        "commerce-platform",
        &TraceInput {
            from,
            to: to.clone(),
            max_depth: 4,
        },
    )?;
    let generated_client = stable_id(
        "node",
        &format!("generated-client:{api_repo}:openapitools.json:rust-client:"),
    );
    let generated_trace = trace_workspace(
        &database,
        "commerce-platform",
        &TraceInput {
            from: generated_client,
            to,
            max_depth: 2,
        },
    )?;
    assert_eq!(edge_kinds(&generated_trace), vec![EdgeKind::Consumes]);
    let test_node = stable_id(
        "node",
        &format!("test:{tests_repo}:python:pytest:tests/test_orders.py:test_create_order"),
    );
    let rust_symbol = stable_id(
        "node",
        &format!("symbol:{api_repo}:rust:src/lib.rs:create_order"),
    );
    let test_trace = trace_workspace(
        &database,
        "commerce-platform",
        &TraceInput {
            from: test_node,
            to: rust_symbol,
            max_depth: 4,
        },
    )?;
    assert_contract_traces(&database, &web_repo, &api_repo, &worker_repo)?;
    assert_resource_traces(&database, &worker_repo, &infra_repo, &docs_repo)?;
    let status = status_workspace(&fixture, &database)?;
    let segment_count = segment_count(&envelope);
    let same_snapshot = summary.snapshot_id == repeated_summary.snapshot_id;
    let cross_language_edges = edge_kinds(&test_trace);

    assert_eq!(
        TraceObservation {
            node_count: summary.node_count,
            edge_count: summary.edge_count,
            evidence_count: summary.evidence_count,
            tool_status: envelope.status,
            segment_count,
            schema_version: status.schema_version,
            freshness: status.freshness.overall,
            discovered_inputs: summary.discovered_input_count,
            changed_inputs: summary.changed_input_count,
            reused_snapshot: summary.reused_snapshot,
            repeated_changed_inputs: repeated_summary.changed_input_count,
            repeated_reused_snapshot: repeated_summary.reused_snapshot,
            same_snapshot,
            cross_language_edges,
        },
        TraceObservation {
            node_count: 101,
            edge_count: 117,
            evidence_count: 105,
            tool_status: ToolStatus::Ok,
            segment_count: 1,
            schema_version: latest_schema_version(),
            freshness: OverallFreshness::Fresh,
            discovered_inputs: 45,
            changed_inputs: 45,
            reused_snapshot: false,
            repeated_changed_inputs: 0,
            repeated_reused_snapshot: true,
            same_snapshot: true,
            cross_language_edges: vec![EdgeKind::Validates, EdgeKind::ImplementedBy],
        }
    );
    Ok(())
}

#[test]
fn status_should_mark_manifest_change_as_stale() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("api");
    std::fs::create_dir_all(&repository)?;
    std::fs::write(
        repository.join("openapi.yaml"),
        "openapi: 3.1.0\ninfo:\n  title: API\n  version: 1.0.0\npaths: {}\n",
    )?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    let initial =
        "version: 1\nname: test\nrepos:\n  api:\n    path: api\n    openapi: openapi.yaml\n";
    std::fs::write(&manifest, initial)?;
    let database = temporary.path().join("code-system-graph.db");
    scan_workspace(&manifest, &database)?;
    let changed = initial.replace("repos:", "allowedRoots: [.]\nrepos:");
    std::fs::write(&manifest, changed)?;

    let status = status_workspace(&manifest, &database)?;

    assert_eq!(
        (status.freshness.overall, status.repositories[0].state,),
        (OverallFreshness::Stale, RepoFreshnessState::ConfigChanged)
    );
    Ok(())
}

#[test]
fn scan_should_plan_add_modify_delete_and_unchanged_inputs() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("api");
    std::fs::create_dir_all(&repository)?;
    let openapi = repository.join("openapi.yaml");
    std::fs::write(
        &openapi,
        "openapi: 3.1.0\ninfo:\n  title: API\n  version: 1.0.0\npaths: {}\n",
    )?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: incremental\nrepos:\n  api:\n    path: api\n",
    )?;
    let database = temporary.path().join("code-system-graph.db");
    let added = scan_workspace(&manifest, &database)?;
    std::fs::write(
        &openapi,
        "openapi: 3.1.0\ninfo:\n  title: API\n  version: 1.0.1\npaths: {}\n",
    )?;
    let modified = scan_workspace(&manifest, &database)?;
    std::fs::remove_file(&openapi)?;
    let deleted = scan_workspace(&manifest, &database)?;
    let unchanged = scan_workspace(&manifest, &database)?;

    assert_eq!(
        (
            added.changed_input_count,
            modified.changed_input_count,
            deleted.changed_input_count,
            deleted.discovered_input_count,
            unchanged.changed_input_count,
            unchanged.reused_snapshot,
        ),
        (1, 1, 1, 0, 0, true)
    );
    Ok(())
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "The E2E keeps all four incremental snapshots and their assertions in one scenario"
)]
fn source_batches_should_reuse_replace_and_delete_affected_links() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let api = temporary.path().join("api");
    let tests = temporary.path().join("tests");
    std::fs::create_dir_all(api.join("src"))?;
    std::fs::create_dir_all(tests.join("tests"))?;
    let rust_source = api.join("src/routes.rs");
    let python_source = tests.join("tests/test_api.py");
    std::fs::write(
        &rust_source,
        r#"use axum::{Router, routing::post};
fn router() { Router::new().route("/v1/orders", post(create_order)); }
async fn create_order() {}
"#,
    )?;
    std::fs::write(
        &python_source,
        r#"import requests
def test_create_order():
    requests.post("https://api.test/v1/orders")
"#,
    )?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: source-incremental\nrepos:\n  api:\n    path: api\n  tests:\n    path: tests\n",
    )?;
    let database = temporary.path().join("code-system-graph.db");
    let first = scan_workspace(&manifest, &database)?;
    let store = SqliteStore::open_read_only(&database)?;
    let first_batches = store.load_current_extractor_batches("source-incremental")?;
    let (_, first_edges) = store.load_current_graph("source-incremental")?;
    drop(store);

    std::fs::write(
        &rust_source,
        r#"use axum::{Router, routing::post};
fn router() { Router::new().route("/v2/orders", post(create_order)); }
async fn create_order() {}
"#,
    )?;
    let replaced = scan_workspace(&manifest, &database)?;
    let store = SqliteStore::open_read_only(&database)?;
    let replaced_batches = store.load_current_extractor_batches("source-incremental")?;
    let replaced_runs = store.load_current_extractor_runs("source-incremental")?;
    let (_, replaced_edges) = store.load_current_graph("source-incremental")?;
    drop(store);

    std::fs::write(
        &python_source,
        r#"import requests
def test_create_order():
    requests.post("https://api.test/v2/orders")
"#,
    )?;
    let relinked = scan_workspace(&manifest, &database)?;
    let store = SqliteStore::open_read_only(&database)?;
    let (_, relinked_edges) = store.load_current_graph("source-incremental")?;
    drop(store);

    std::fs::remove_file(&python_source)?;
    let deleted = scan_workspace(&manifest, &database)?;
    let store = SqliteStore::open_read_only(&database)?;
    let deleted_batches = store.load_current_extractor_batches("source-incremental")?;
    let (_, deleted_edges) = store.load_current_graph("source-incremental")?;

    let edge_kinds = |edges: &[code_system_graph_model::Edge]| {
        let mut kinds = edges
            .iter()
            .filter(|edge| {
                matches!(
                    edge.kind,
                    EdgeKind::CallsRemote | EdgeKind::ImplementedBy | EdgeKind::Validates
                )
            })
            .map(|edge| edge.kind)
            .collect::<Vec<EdgeKind>>();
        kinds.sort_by_key(|kind| format!("{kind:?}"));
        kinds
    };
    let python_first = first_batches
        .iter()
        .find(|batch| batch.source.extractor == "code-system-graph.source.python")
        .ok_or_else(|| anyhow::anyhow!("first Python batch missing"))?;
    let python_reused = replaced_batches
        .iter()
        .find(|batch| batch.source.extractor == "code-system-graph.source.python")
        .ok_or_else(|| anyhow::anyhow!("reused Python batch missing"))?;

    let complete_links = vec![
        EdgeKind::CallsRemote,
        EdgeKind::ImplementedBy,
        EdgeKind::Validates,
    ];
    assert_eq!(
        (
            first.changed_input_count,
            first_batches.len(),
            edge_kinds(&first_edges),
        ),
        (10, 10, complete_links.clone())
    );
    assert_eq!(replaced.changed_input_count, 5);
    assert_eq!(python_reused, python_first);
    assert_eq!(
        replaced_runs
            .iter()
            .filter(|run| run.status == ExtractorRunStatus::SkippedUnchanged)
            .count(),
        5
    );
    assert_eq!(edge_kinds(&replaced_edges), vec![EdgeKind::ImplementedBy]);
    assert_eq!(
        (relinked.changed_input_count, edge_kinds(&relinked_edges)),
        (5, complete_links)
    );
    assert_eq!(
        (
            deleted.changed_input_count,
            deleted_batches.len(),
            edge_kinds(&deleted_edges),
        ),
        (5, 5, vec![EdgeKind::ImplementedBy])
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn scan_should_apply_exact_codegraph_corroboration_without_source_payloads() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("api");
    std::fs::create_dir_all(repository.join("src"))?;
    std::fs::write(
        repository.join("src/lib.rs"),
        "// one\n// two\n// three\n// four\n// five\nuse axum::{Router, routing::post};\nfn router() { Router::new().route(\"/orders\", post(anchor)); }\nasync fn anchor() {}\n",
    )?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: codegraph-scan\nrepos:\n  api:\n    path: api\n",
    )?;
    let fake = temporary.path().join("codegraph-ok");
    std::fs::copy(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/codegraph/fake/codegraph.py"),
        &fake,
    )?;
    let mut permissions = std::fs::metadata(&fake)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake, permissions)?;
    let database = temporary.path().join("code-system-graph.db");

    let summary = scan_workspace_with_overrides(
        &manifest,
        &database,
        &ScanOverrides {
            repo_openapi: BTreeMap::new(),
            codegraph: true,
            codegraph_binary: Some(fake),
            ..ScanOverrides::default()
        },
    )?;
    let store = SqliteStore::open_read_only(&database)?;
    let (_, edges) = store.load_current_graph("codegraph-scan")?;
    let source = std::fs::read_to_string(&manifest)?;
    let parsed = parse_manifest(&source)?;
    let registry = register_workspace(&manifest, &source, &parsed)?;
    let repo_id = &registry.record.repositories[0].id;
    let capability =
        store.load_provider_capabilities("codegraph-scan", repo_id, "codegraph", "1.5.0")?;
    let codegraph_evidence = store
        .search_current_nodes("codegraph-scan", "anchor", 10)?
        .len();

    assert_eq!(
        (
            summary.corroborated_symbol_count,
            summary.affected_test_count,
            summary.degradations,
            codegraph_evidence,
        ),
        (1, 1, Vec::<String>::new(), 1)
    );
    assert!(
        edges
            .iter()
            .any(|edge| { edge.kind == EdgeKind::ImplementedBy && edge.evidence.len() == 3 })
    );
    assert!(capability.is_some_and(|record| {
        record
            .capabilities
            .iter()
            .any(|item| item == "status:available")
            && record
                .capabilities
                .iter()
                .any(|item| item.starts_with("operation:"))
    }));
    Ok(())
}

#[test]
fn cli_openapi_override_should_take_precedence_and_invalidate_plain_status() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("api");
    std::fs::create_dir_all(&repository)?;
    std::fs::write(
        repository.join("workspace.yaml"),
        "openapi: 3.1.0\ninfo:\n  title: API\n  version: 1\npaths:\n  /workspace:\n    get: {}\n",
    )?;
    std::fs::write(
        repository.join("cli.yaml"),
        "openapi: 3.1.0\ninfo:\n  title: API\n  version: 1\npaths:\n  /cli:\n    get: {}\n",
    )?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: override\nrepos:\n  api:\n    path: api\n    openapi: workspace.yaml\n",
    )?;
    let database = temporary.path().join("code-system-graph.db");
    let summary = scan_workspace_with_overrides(
        &manifest,
        &database,
        &ScanOverrides {
            repo_openapi: BTreeMap::from([("api".to_owned(), "cli.yaml".to_owned())]),
            codegraph: false,
            codegraph_binary: None,
            ..ScanOverrides::default()
        },
    )?;
    let (nodes, _) = SqliteStore::open_read_only(&database)?.load_current_graph("override")?;
    let status = status_workspace(&manifest, &database)?;

    assert_eq!(
        (
            summary.node_count,
            nodes.first().map(|node| node.label.as_str()),
            status.freshness.overall,
        ),
        (1, Some("GET /cli"), OverallFreshness::Stale)
    );
    Ok(())
}

#[test]
fn cli_repo_openapi_flag_should_override_manifest() -> anyhow::Result<()> {
    let temporary = tempfile::tempdir()?;
    let repository = temporary.path().join("api");
    std::fs::create_dir_all(&repository)?;
    std::fs::write(
        repository.join("workspace.yaml"),
        "openapi: 3.1.0\ninfo:\n  title: API\n  version: 1\npaths: {}\n",
    )?;
    std::fs::write(
        repository.join("cli.yaml"),
        "openapi: 3.1.0\ninfo:\n  title: API\n  version: 1\npaths:\n  /cli:\n    get: {}\n",
    )?;
    let manifest = temporary.path().join("code-system-graph.yaml");
    std::fs::write(
        &manifest,
        "version: 1\nname: cli-override\nrepos:\n  api:\n    path: api\n    openapi: workspace.yaml\n",
    )?;
    let database = temporary.path().join("code-system-graph.db");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_csgraph"))
        .args(["scan", "--config"])
        .arg(&manifest)
        .arg("--database")
        .arg(&database)
        .args(["--repo-openapi", "api=cli.yaml"])
        .output()?;
    let (nodes, _) = SqliteStore::open_read_only(&database)?.load_current_graph("cli-override")?;

    assert_eq!(
        (
            output.status.success(),
            nodes.first().map(|node| node.label.as_str()),
        ),
        (true, Some("GET /cli"))
    );
    Ok(())
}

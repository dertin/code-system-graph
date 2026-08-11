//! End-to-end acceptance tests for optional authenticated HTTP delivery.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::Context;
use code_system_graph::http_server::{
    BearerToken, DEFAULT_HTTP_BIND, HttpServerConfig, create_router, serve_http_on_listener
};
use code_system_graph::scan_workspace;
#[cfg(unix)]
use code_system_graph_store_sqlite::SqliteStore;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const TEST_TOKEN: &str = "http-test-token";
static FIXTURE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn is_early_body_rejection(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(source) = current {
        if source
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_error| {
                matches!(
                    io_error.kind(),
                    std::io::ErrorKind::BrokenPipe
                        | std::io::ErrorKind::ConnectionAborted
                        | std::io::ErrorKind::ConnectionReset
                )
            })
        {
            return true;
        }
        current = source.source();
    }
    false
}

struct Fixture {
    temporary: TempDir,
    manifest: PathBuf,
    database: PathBuf,
    workspace_name: String,
}

impl Fixture {
    fn create() -> anyhow::Result<Self> {
        let workspace_name = format!(
            "http-test-{}",
            FIXTURE_COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let temporary = tempfile::tempdir()?;
        let repository = temporary.path().join("api");
        std::fs::create_dir_all(&repository)?;
        std::fs::write(
            repository.join("openapi.yaml"),
            "openapi: 3.1.0\ninfo:\n  title: API\n  version: 1\npaths:\n  /orders:\n    get: {}\n",
        )?;
        let manifest = temporary.path().join("code-system-graph.yaml");
        std::fs::write(
            &manifest,
            format!(
                "version: 1\nname: {workspace_name}\nrepos:\n  api:\n    path: api\n    openapi: openapi.yaml\n"
            ),
        )?;
        let database = temporary.path().join("graph.db");
        scan_workspace(&manifest, &database)?;
        Ok(Self {
            temporary,
            manifest,
            database,
            workspace_name,
        })
    }

    fn server_config(&self) -> HttpServerConfig {
        debug_assert!(self.temporary.path().is_dir());
        HttpServerConfig::new(&self.manifest, &self.database, &self.workspace_name)
    }
}

struct RunningServer {
    address: SocketAddr,
    cancellation: CancellationToken,
    task: JoinHandle<Result<(), code_system_graph::http_server::HttpServerError>>,
    fixture: Fixture,
}

impl RunningServer {
    async fn start(token: Option<BearerToken>) -> anyhow::Result<Self> {
        let fixture = Fixture::create()?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let mut config = fixture.server_config().with_bind(address);
        if let Some(token) = token {
            config = config.with_bearer_token(token);
        }
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            serve_http_on_listener(listener, config, task_cancellation).await
        });
        Ok(Self {
            address,
            cancellation,
            task,
            fixture,
        })
    }

    async fn start_for_query(codegraph_enabled: bool) -> anyhow::Result<Self> {
        let fixture = Fixture::create()?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let config = fixture
            .server_config()
            .with_bind(address)
            .with_codegraph(codegraph_enabled, None);
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            serve_http_on_listener(listener, config, task_cancellation).await
        });
        Ok(Self {
            address,
            cancellation,
            task,
            fixture,
        })
    }

    #[cfg(unix)]
    async fn start_with_codegraph() -> anyhow::Result<Self> {
        let fixture = Fixture::create()?;
        let binary = fixture.temporary.path().join("codegraph-ok");
        std::fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/codegraph/fake/codegraph.py"),
            &binary,
        )?;
        let mut permissions = std::fs::metadata(&binary)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions)?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let config = fixture
            .server_config()
            .with_bind(address)
            .with_codegraph(true, Some(binary.into_os_string()));
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            serve_http_on_listener(listener, config, task_cancellation).await
        });
        Ok(Self {
            address,
            cancellation,
            task,
            fixture,
        })
    }

    #[cfg(unix)]
    async fn start_with_disabled_codegraph() -> anyhow::Result<(Self, PathBuf)> {
        let fixture = Fixture::create()?;
        let binary = fixture.temporary.path().join("codegraph-marker");
        let marker = fixture.temporary.path().join("codegraph-invoked");
        std::fs::write(
            &binary,
            "#!/bin/sh\n: > \"$(dirname \"$0\")/codegraph-invoked\"\nexit 1\n",
        )?;
        let mut permissions = std::fs::metadata(&binary)?.permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions)?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let config = fixture
            .server_config()
            .with_bind(address)
            .with_codegraph(false, Some(binary.into_os_string()));
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            serve_http_on_listener(listener, config, task_cancellation).await
        });
        Ok((
            Self {
                address,
                cancellation,
                task,
                fixture,
            },
            marker,
        ))
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }

    async fn stop(self) -> anyhow::Result<()> {
        self.cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(2), self.task)
            .await
            .context("HTTP server did not stop after cancellation")?
            .context("HTTP server task failed")??;
        Ok(())
    }
}

fn bearer() -> anyhow::Result<BearerToken> {
    BearerToken::new(TEST_TOKEN).map_err(Into::into)
}

async fn response_json(response: reqwest::Response) -> anyhow::Result<(StatusCode, Value)> {
    let status = response.status();
    let body = response.json().await?;
    Ok((status, body))
}

async fn server_serves_workspace(client: &Client, address: SocketAddr, workspace: &str) -> bool {
    let Ok(response) = client
        .get(format!("http://{address}/v1/status"))
        .send()
        .await
    else {
        return false;
    };
    if response.status() != StatusCode::OK {
        return false;
    }
    let Ok((_, body)) = response_json(response).await else {
        return false;
    };
    body["data"]["workspace"].as_str() == Some(workspace)
}

#[tokio::test]
async fn loopback_should_serve_anonymous_health_and_status() -> anyhow::Result<()> {
    let server = RunningServer::start(None).await?;
    let client = Client::new();
    let health = client.get(server.url("/health")).send().await?;
    let version_header = health
        .headers()
        .get("x-code-system-graph-version")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let schema_header = health
        .headers()
        .get("x-code-system-graph-schema-version")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let content_type_policy = health
        .headers()
        .get("x-content-type-options")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let cors_header_present = health.headers().contains_key("access-control-allow-origin");
    let (health_status, health_body) = response_json(health).await?;
    let (status_status, status_body) =
        response_json(client.get(server.url("/v1/status")).send().await?).await?;

    assert_eq!(
        (
            health_status,
            health_body["status"].as_str(),
            version_header.as_deref(),
            content_type_policy.as_deref(),
            schema_header.as_deref(),
            cors_header_present,
            status_status,
            status_body["data"]["workspace"].as_str(),
        ),
        (
            StatusCode::OK,
            Some("ok"),
            Some(env!("CARGO_PKG_VERSION")),
            Some("nosniff"),
            Some("2"),
            false,
            StatusCode::OK,
            Some(server.fixture.workspace_name.as_str()),
        )
    );
    server.stop().await
}

#[test]
fn configuration_should_default_to_fixed_loopback_address() -> anyhow::Result<()> {
    let fixture = Fixture::create()?;

    assert_eq!(fixture.server_config().bind, DEFAULT_HTTP_BIND);
    Ok(())
}

#[test]
fn non_loopback_should_require_authentication() -> anyhow::Result<()> {
    let fixture = Fixture::create()?;
    let config = fixture
        .server_config()
        .with_bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 4767));

    let error = create_router(config).expect_err("anonymous non-loopback bind must fail");

    assert_eq!(
        error.to_string(),
        "non-loopback HTTP bind requires a bearer token"
    );
    Ok(())
}

#[tokio::test]
async fn configured_auth_should_reject_missing_and_wrong_tokens() -> anyhow::Result<()> {
    let token = bearer()?;
    let debug = format!("{token:?}");
    let server = RunningServer::start(Some(token)).await?;
    let client = Client::new();
    let missing = client.get(server.url("/health")).send().await?;
    let wrong = client
        .get(server.url("/health"))
        .bearer_auth("wrong-token")
        .send()
        .await?;

    assert_eq!(
        (missing.status(), wrong.status(), debug),
        (
            StatusCode::UNAUTHORIZED,
            StatusCode::UNAUTHORIZED,
            "BearerToken([REDACTED])".to_owned(),
        )
    );
    server.stop().await
}

#[tokio::test]
async fn configured_auth_should_accept_valid_bearer_token() -> anyhow::Result<()> {
    let server = RunningServer::start(Some(bearer()?)).await?;
    let response = Client::new()
        .post(server.url("/v1/tools/status"))
        .bearer_auth(TEST_TOKEN)
        .json(&json!({}))
        .send()
        .await?;
    let (status, body) = response_json(response).await?;

    assert_eq!(
        (
            status,
            body["status"].as_str(),
            body["data"]["workspace"].as_str()
        ),
        (
            StatusCode::OK,
            Some("ok"),
            Some(server.fixture.workspace_name.as_str())
        )
    );
    server.stop().await
}

#[tokio::test]
async fn query_should_only_advertise_http_available_actions() -> anyhow::Result<()> {
    let disabled = RunningServer::start_for_query(false).await?;
    let client = Client::new();
    let hit = client
        .post(disabled.url("/v1/tools/query"))
        .json(&json!({
            "query": "orders",
            "node_kinds": [],
            "repo_ids": [],
            "service_ids": [],
            "community_ids": [],
            "offset": 0,
            "limit": 5
        }))
        .send()
        .await?;
    let (hit_status, hit_body) = response_json(hit).await?;
    let hit_actions = hit_body["data"]["next_actions"]
        .as_array()
        .context("query hit actions")?;
    assert_eq!(hit_status, StatusCode::OK);
    assert!(
        !hit_body["data"]["hits"]
            .as_array()
            .is_none_or(Vec::is_empty)
    );
    assert!(
        hit_actions
            .iter()
            .all(|action| action["tool"] != "source_context")
    );

    let missing = client
        .post(disabled.url("/v1/tools/query"))
        .json(&json!({
            "query": "api source_literal_that_does_not_exist",
            "node_kinds": [],
            "repo_ids": [],
            "service_ids": [],
            "community_ids": [],
            "offset": 0,
            "limit": 5
        }))
        .send()
        .await?;
    let (_, missing_body) = response_json(missing).await?;
    assert!(
        missing_body["data"]["next_actions"]
            .as_array()
            .context("disabled query actions")?
            .iter()
            .all(|action| action["tool"] != "explore")
    );
    assert!(
        missing_body["data"]["coverage"]["gaps"]
            .as_array()
            .context("disabled query gaps")?
            .iter()
            .all(|gap| !gap.as_str().is_some_and(|gap| gap.contains("use Explore")))
    );
    disabled.stop().await?;

    let enabled = RunningServer::start_for_query(true).await?;
    let response = client
        .post(enabled.url("/v1/tools/query"))
        .json(&json!({
            "query": "api source_literal_that_does_not_exist",
            "node_kinds": [],
            "repo_ids": [],
            "service_ids": [],
            "community_ids": [],
            "offset": 0,
            "limit": 5
        }))
        .send()
        .await?;
    let (_, body) = response_json(response).await?;
    assert!(
        body["data"]["next_actions"]
            .as_array()
            .context("enabled query actions")?
            .iter()
            .any(|action| action["tool"] == "explore")
    );
    enabled.stop().await
}

#[cfg(unix)]
#[tokio::test]
async fn explore_route_should_return_ephemeral_local_context() -> anyhow::Result<()> {
    let server = RunningServer::start_with_codegraph().await?;
    let workspace = server.fixture.workspace_name.clone();
    let response = Client::new()
        .post(server.url("/v1/tools/explore"))
        .json(&json!({
            "workspace": workspace,
            "query": "orders implementation",
            "max_files": 4
        }))
        .send()
        .await?;
    let (status, body) = response_json(response).await?;

    assert_eq!(
        (
            status,
            body["data"]["source_markdown"].as_str(),
            body["schema_version"].as_u64()
        ),
        (StatusCode::OK, Some("ephemeral local context"), Some(2))
    );
    let target = SqliteStore::open_read_only(&server.fixture.database)?
        .load_current_graph(&server.fixture.workspace_name)?
        .0
        .first()
        .ok_or_else(|| anyhow::anyhow!("impact target"))?
        .id
        .as_str()
        .to_owned();
    let impact = Client::new()
        .post(server.url("/v1/tools/impact"))
        .json(&json!({
            "target": {"kind": "node_id", "value": target},
            "direction": "upstream"
        }))
        .send()
        .await?;
    let (impact_status, impact_body) = response_json(impact).await?;
    assert!(
        impact_status == StatusCode::OK && impact_body["data"]["local_impact_summaries"].is_array()
    );
    server.stop().await
}

#[cfg(unix)]
#[tokio::test]
async fn explore_route_should_reject_disabled_codegraph_before_execution() -> anyhow::Result<()> {
    let (server, marker) = RunningServer::start_with_disabled_codegraph().await?;
    let response = Client::new()
        .post(server.url("/v1/tools/explore"))
        .json(&json!({
            "workspace": server.fixture.workspace_name,
            "query": "orders implementation",
            "max_files": 4
        }))
        .send()
        .await?;
    let (status, body) = response_json(response).await?;

    assert_eq!(
        (status, body["data"]["code"].as_str(), marker.exists()),
        (StatusCode::FORBIDDEN, Some("codegraph_disabled"), false)
    );
    server.stop().await
}

#[tokio::test]
async fn tool_request_should_reject_body_larger_than_one_mibibyte() -> anyhow::Result<()> {
    let server = RunningServer::start(None).await?;
    let client = Client::new();
    let response = client
        .post(server.url("/v1/tools/query"))
        .json(&json!({
            "query": "x".repeat(1024 * 1024),
            "node_kinds": [],
            "repo_ids": [],
            "service_ids": [],
            "community_ids": [],
            "offset": 0,
            "limit": 1
        }))
        .send()
        .await;
    match response {
        Ok(response) => {
            let (status, body) = response_json(response).await?;
            assert_eq!(
                (status, body["data"]["code"].as_str()),
                (StatusCode::PAYLOAD_TOO_LARGE, Some("payload_too_large"))
            );
        }
        Err(error) => {
            // Some kernels reset a connection when the server rejects the declared oversized body
            // before the client finishes writing it.
            assert!(
                is_early_body_rejection(&error),
                "unexpected oversized-body transport error: {error:#}"
            );
        }
    }
    assert_eq!(
        client.get(server.url("/health")).send().await?.status(),
        StatusCode::OK
    );
    server.stop().await
}

#[tokio::test]
async fn client_should_be_limited_to_sixty_requests_per_minute() -> anyhow::Result<()> {
    let server = RunningServer::start(None).await?;
    let client = Client::new();
    for _request in 0..60 {
        let response = client.get(server.url("/health")).send().await?;
        anyhow::ensure!(response.status() == StatusCode::OK);
    }
    let limited = client.get(server.url("/health")).send().await?;
    let (status, body) = response_json(limited).await?;

    assert_eq!(
        (status, body["data"]["code"].as_str()),
        (StatusCode::TOO_MANY_REQUESTS, Some("rate_limited"))
    );
    server.stop().await
}

#[tokio::test]
async fn unsupported_and_mutating_routes_should_be_rejected() -> anyhow::Result<()> {
    let server = RunningServer::start(None).await?;
    let client = Client::new();
    let unsupported = client.get(server.url("/v1/admin")).send().await?;
    let mutation = client.delete(server.url("/v1/status")).send().await?;

    assert_eq!(
        (unsupported.status(), mutation.status()),
        (StatusCode::NOT_FOUND, StatusCode::METHOD_NOT_ALLOWED)
    );
    server.stop().await
}

#[tokio::test]
async fn cancellation_should_stop_accepting_connections() -> anyhow::Result<()> {
    let server = RunningServer::start(None).await?;
    let address = server.address;
    let workspace = server.fixture.workspace_name.clone();
    let client = Client::builder()
        .timeout(Duration::from_millis(500))
        .build()?;

    let startup_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    let mut accepted_before_cancellation = false;
    while tokio::time::Instant::now() < startup_deadline {
        if server_serves_workspace(&client, address, &workspace).await {
            accepted_before_cancellation = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        accepted_before_cancellation,
        "server should accept connections before cancellation"
    );

    server.cancellation.cancel();
    tokio::time::timeout(Duration::from_secs(2), server.task)
        .await
        .context("HTTP server did not stop after cancellation")?
        .context("HTTP server task failed")??;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        if !server_serves_workspace(&client, address, &workspace).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    panic!("cancelled server should stop accepting connections");
}

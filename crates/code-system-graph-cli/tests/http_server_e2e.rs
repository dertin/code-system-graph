//! End-to-end acceptance tests for optional authenticated HTTP delivery.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context;
use code_system_graph::http_server::{
    BearerToken, DEFAULT_HTTP_BIND, HttpServerConfig, create_router, serve_http_on_listener
};
use code_system_graph::scan_workspace;
use code_system_graph_store_sqlite::SqliteStore;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

const TEST_TOKEN: &str = "http-test-token";

struct Fixture {
    temporary: TempDir,
    manifest: PathBuf,
    database: PathBuf,
}

impl Fixture {
    fn create() -> anyhow::Result<Self> {
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
            "version: 1\nname: http-test\nrepos:\n  api:\n    path: api\n    openapi: openapi.yaml\n",
        )?;
        let database = temporary.path().join("graph.db");
        scan_workspace(&manifest, &database)?;
        Ok(Self {
            temporary,
            manifest,
            database,
        })
    }

    fn server_config(&self) -> HttpServerConfig {
        HttpServerConfig::new(&self.manifest, &self.database, "http-test")
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
            cors_header_present,
            status_status,
            status_body["data"]["workspace"].as_str(),
        ),
        (
            StatusCode::OK,
            Some("ok"),
            Some(env!("CARGO_PKG_VERSION")),
            Some("nosniff"),
            false,
            StatusCode::OK,
            Some("http-test"),
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
        (StatusCode::OK, Some("ok"), Some("http-test"))
    );
    server.stop().await
}

#[cfg(unix)]
#[tokio::test]
async fn explore_route_should_return_ephemeral_local_context() -> anyhow::Result<()> {
    let server = RunningServer::start_with_codegraph().await?;
    let response = Client::new()
        .post(server.url("/v1/tools/explore"))
        .json(&json!({
            "workspace": "http-test",
            "query": "orders implementation",
            "max_files": 4
        }))
        .send()
        .await?;
    let (status, body) = response_json(response).await?;

    assert_eq!(
        (
            status,
            body["data"]["content"].as_str(),
            body["schema_version"].as_u64()
        ),
        (StatusCode::OK, Some("ephemeral local context"), Some(1))
    );
    let target = SqliteStore::open_read_only(&server.fixture.database)?
        .load_current_graph("http-test")?
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
            "workspace": "http-test",
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
    let response = Client::new()
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
        .await?;
    let (status, body) = response_json(response).await?;

    assert_eq!(
        (status, body["data"]["code"].as_str()),
        (StatusCode::PAYLOAD_TOO_LARGE, Some("payload_too_large"))
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
    server.cancellation.cancel();
    tokio::time::timeout(Duration::from_secs(2), server.task)
        .await
        .context("HTTP server did not stop after cancellation")?
        .context("HTTP server task failed")??;

    let connection = tokio::net::TcpStream::connect(address).await;

    assert!(
        connection.is_err(),
        "cancelled server should stop accepting connections"
    );
    Ok(())
}

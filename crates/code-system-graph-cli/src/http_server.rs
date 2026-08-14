//! Optional authenticated HTTP delivery for read-only application services.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Json, Request, State};
use axum::http::header::{
    AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_SECURITY_POLICY, HeaderName, HeaderValue, REFERRER_POLICY, WWW_AUTHENTICATE, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS
};
use axum::http::{Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use code_system_graph_core::{
    ChangeAnalysisOptions, ExecutionPolicy, ImpactReport, ImpactRequest, SearchReport
};
use code_system_graph_model::{
    FreshnessSummary, NodeKind, OverallFreshness, RepoId, ToolEnvelope, ToolStatus
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use thiserror::Error;
use tokio::net::TcpListener;
use tokio::sync::{Mutex, Semaphore};
use tokio_util::sync::CancellationToken;
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;

use crate::{
    ApplicationError, CODEGRAPH_DISABLED_CODE, CODEGRAPH_DISABLED_MESSAGE, ChangesInput, CommunityInput, CommunityReport, ExploreInput, ExploreReport, QueryActionCapabilities, SearchInput, TraceInput, analyze_workspace_changes, communities_workspace, explore_repository, impact_workspace, impact_workspace_with_codegraph, search_workspace_for_delivery, status_workspace, trace_workspace
};

/// Default loopback address used by optional HTTP delivery.
pub const DEFAULT_HTTP_BIND: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4767);

const BODY_LIMIT_BYTES: usize = 1024 * 1024;
const BODY_LIMIT_BYTES_U64: u64 = 1024 * 1024;
const CONCURRENCY_LIMIT: usize = 32;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const RATE_LIMIT_REQUESTS: u32 = 60;
const RATE_LIMIT_WINDOW: Duration = Duration::from_mins(1);
const MAX_RATE_LIMIT_CLIENTS: usize = 4096;
const MAX_TOKEN_BYTES: usize = 4096;
const SCHEMA_VERSION: u32 = 2;

const VERSION_HEADER: HeaderName = HeaderName::from_static("x-code-system-graph-version");
const SCHEMA_HEADER: HeaderName = HeaderName::from_static("x-code-system-graph-schema-version");
const PERMISSIONS_POLICY: HeaderName = HeaderName::from_static("permissions-policy");

/// A bearer token whose formatting and errors never reveal its contents.
pub struct BearerToken {
    bytes: Box<[u8; MAX_TOKEN_BYTES]>,
    len: usize,
}

impl BearerToken {
    /// Creates a non-empty bearer token within the fixed comparison budget.
    ///
    /// # Errors
    ///
    /// Returns [`BearerTokenError`] when the value is empty, whitespace-only, or larger than the
    /// fixed token budget.
    pub fn new(value: impl AsRef<str>) -> Result<Self, BearerTokenError> {
        let value = value.as_ref();
        if value.trim().is_empty() {
            return Err(BearerTokenError::Invalid);
        }
        let source = value.as_bytes();
        if source.len() > MAX_TOKEN_BYTES {
            return Err(BearerTokenError::Invalid);
        }
        let mut bytes = Box::new([0_u8; MAX_TOKEN_BYTES]);
        bytes[..source.len()].copy_from_slice(source);
        Ok(Self {
            bytes,
            len: source.len(),
        })
    }

    fn matches(&self, candidate: &[u8]) -> bool {
        let mut padded = [0_u8; MAX_TOKEN_BYTES];
        let candidate_fits = candidate.len() <= MAX_TOKEN_BYTES;
        if candidate_fits {
            padded[..candidate.len()].copy_from_slice(candidate);
        }
        let content_matches = self.bytes.as_ref().ct_eq(&padded);
        let length_matches = self.len.ct_eq(&candidate.len());
        bool::from(content_matches & length_matches) && candidate_fits
    }
}

impl Clone for BearerToken {
    fn clone(&self) -> Self {
        Self {
            bytes: self.bytes.clone(),
            len: self.len,
        }
    }
}

impl fmt::Debug for BearerToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("BearerToken([REDACTED])")
    }
}

impl Drop for BearerToken {
    fn drop(&mut self) {
        self.bytes.fill(0);
        self.len = 0;
    }
}

/// Failure to construct a safe bearer token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BearerTokenError {
    /// The supplied token does not satisfy the fixed non-empty token contract.
    #[error("bearer token must be non-empty and within the supported size limit")]
    Invalid,
}

/// Fixed workspace and database configuration for one HTTP server.
#[derive(Debug, Clone)]
pub struct HttpServerConfig {
    /// Socket address on which the server listens.
    pub bind: SocketAddr,
    /// Workspace manifest used by status operations.
    pub config_path: PathBuf,
    /// Immutable database selection used by every request.
    pub database_path: PathBuf,
    /// Immutable workspace selection used by every tool request.
    pub workspace: String,
    /// Optional bearer token required by every route when configured.
    pub bearer_token: Option<BearerToken>,
    /// Whether automatic bounded `CodeGraph` enrichment is enabled.
    pub codegraph_enabled: bool,
    /// Optional explicit `CodeGraph` executable used by local exploration and impact enrichment.
    pub codegraph_binary: Option<OsString>,
    /// Immutable effective workspace policy shared by all handlers.
    pub execution_policy: ExecutionPolicy,
}

impl HttpServerConfig {
    /// Creates loopback-only anonymous HTTP configuration for one workspace.
    #[must_use]
    pub fn new(
        config_path: impl Into<PathBuf>,
        database_path: impl Into<PathBuf>,
        workspace: impl Into<String>,
    ) -> Self {
        Self {
            bind: DEFAULT_HTTP_BIND,
            config_path: config_path.into(),
            database_path: database_path.into(),
            workspace: workspace.into(),
            bearer_token: None,
            codegraph_enabled: false,
            codegraph_binary: None,
            execution_policy: ExecutionPolicy::default(),
        }
    }

    /// Selects an explicit bind address.
    #[must_use]
    pub const fn with_bind(mut self, bind: SocketAddr) -> Self {
        self.bind = bind;
        self
    }

    /// Requires the supplied redacted bearer token on every route.
    #[must_use]
    pub fn with_bearer_token(mut self, token: BearerToken) -> Self {
        self.bearer_token = Some(token);
        self
    }

    /// Applies trusted process-level `CodeGraph` policy to local intelligence requests.
    #[must_use]
    pub fn with_codegraph(mut self, enabled: bool, binary: Option<OsString>) -> Self {
        self.codegraph_enabled = enabled;
        self.codegraph_binary = binary;
        self
    }

    /// Applies the validated immutable workspace execution policy.
    #[must_use]
    pub fn with_execution_policy(mut self, policy: ExecutionPolicy) -> Self {
        self.execution_policy = policy;
        self
    }

    fn validate_for(&self, bind: SocketAddr) -> Result<(), HttpServerError> {
        if self.workspace.trim().is_empty() {
            return Err(HttpServerError::InvalidConfiguration(
                "HTTP workspace must be non-empty",
            ));
        }
        if !bind.ip().is_loopback() && self.bearer_token.is_none() {
            return Err(HttpServerError::InvalidConfiguration(
                "non-loopback HTTP bind requires a bearer token",
            ));
        }
        Ok(())
    }
}

/// HTTP server startup or serving failure.
#[derive(Debug, Error)]
pub enum HttpServerError {
    /// Configuration violates a fail-closed server invariant.
    #[error("{0}")]
    InvalidConfiguration(&'static str),
    /// The configured socket could not be bound.
    #[error("failed to bind HTTP server")]
    Bind(#[source] std::io::Error),
    /// The HTTP transport stopped unexpectedly.
    #[error("HTTP server failed")]
    Serve(#[source] std::io::Error),
    /// An externally supplied listener could not report its local address.
    #[error("failed to inspect HTTP listener")]
    ListenerAddress(#[source] std::io::Error),
}

/// Empty input accepted by `status`.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusInput {}

/// Input for a contract-only ranked query.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContractsInput {
    /// Text matched against contract identities and labels.
    #[serde(default)]
    pub query: String,
    /// Optional repository filter.
    #[serde(default)]
    pub repo_ids: Vec<RepoId>,
    /// Zero-based result offset.
    #[serde(default)]
    pub offset: usize,
    /// Bounded page size.
    #[serde(default = "default_contract_limit")]
    pub limit: usize,
}

const fn default_contract_limit() -> usize {
    20
}

#[derive(Clone)]
struct HttpState {
    config: Arc<HttpServerConfig>,
    limiter: Arc<RateLimiter>,
    concurrency: Arc<Semaphore>,
}

#[derive(Debug)]
struct RateWindow {
    started: Instant,
    requests: u32,
}

#[derive(Debug, Default)]
struct RateLimiter {
    clients: Mutex<HashMap<IpAddr, RateWindow>>,
}

impl RateLimiter {
    async fn check(&self, client: IpAddr) -> bool {
        let now = Instant::now();
        let mut clients = self.clients.lock().await;
        clients.retain(|_, window| now.duration_since(window.started) < RATE_LIMIT_WINDOW);

        if let Some(window) = clients.get_mut(&client) {
            if window.requests >= RATE_LIMIT_REQUESTS {
                return false;
            }
            window.requests += 1;
            return true;
        }

        if clients.len() >= MAX_RATE_LIMIT_CLIENTS
            && let Some(oldest) = clients
                .iter()
                .min_by_key(|(_, window)| window.started)
                .map(|(address, _)| *address)
        {
            clients.remove(&oldest);
        }
        clients.insert(
            client,
            RateWindow {
                started: now,
                requests: 1,
            },
        );
        true
    }
}

#[derive(Debug, Serialize)]
struct HealthReport {
    healthy: bool,
    version: &'static str,
}

#[derive(Debug, Serialize)]
struct ErrorReport {
    code: &'static str,
    message: &'static str,
}

/// Builds the read-only HTTP router after validating bind and authentication policy.
///
/// Requests executed directly against this router without transport connection metadata are
/// rate-limited as loopback requests.
///
/// # Errors
///
/// Returns [`HttpServerError::InvalidConfiguration`] for an empty workspace or for an anonymous
/// non-loopback bind.
pub fn create_router(config: HttpServerConfig) -> Result<Router, HttpServerError> {
    config.validate_for(config.bind)?;
    let state = HttpState {
        config: Arc::new(config),
        limiter: Arc::new(RateLimiter::default()),
        concurrency: Arc::new(Semaphore::new(CONCURRENCY_LIMIT)),
    };

    Ok(Router::new()
        .route("/health", get(health))
        .route("/v1/status", get(system_status))
        .route("/v1/tools/status", post(status))
        .route("/v1/tools/query", post(query))
        .route("/v1/tools/trace", post(trace))
        .route("/v1/tools/explore", post(explore))
        .route("/v1/tools/impact", post(impact))
        .route("/v1/tools/analyze_changes", post(analyze_changes))
        .route("/v1/tools/contracts", post(contracts))
        .route("/v1/tools/communities", post(communities))
        .fallback(unsupported_route)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(BODY_LIMIT_BYTES))
        .layer(SetSensitiveRequestHeadersLayer::new(std::iter::once(
            AUTHORIZATION,
        )))
        .layer(middleware::from_fn_with_state(state.clone(), request_guard))
        .with_state(state))
}

/// Binds and serves optional HTTP delivery until cancellation requests graceful shutdown.
///
/// # Errors
///
/// Returns [`HttpServerError`] when configuration validation, binding, or serving fails.
pub async fn serve_http(
    config: HttpServerConfig,
    cancellation: CancellationToken,
) -> Result<(), HttpServerError> {
    config.validate_for(config.bind)?;
    let listener = TcpListener::bind(config.bind)
        .await
        .map_err(HttpServerError::Bind)?;
    serve_http_on_listener(listener, config, cancellation).await
}

/// Serves on an existing listener, retaining the listener's actual bind policy.
///
/// This entry point supports race-free host integration tests and embedding. The listener address
/// is independently validated so a loopback configuration cannot authorize a non-loopback socket.
///
/// # Errors
///
/// Returns [`HttpServerError`] when listener inspection, configuration validation, or serving
/// fails.
pub async fn serve_http_on_listener(
    listener: TcpListener,
    mut config: HttpServerConfig,
    cancellation: CancellationToken,
) -> Result<(), HttpServerError> {
    let address = listener
        .local_addr()
        .map_err(HttpServerError::ListenerAddress)?;
    config.validate_for(address)?;
    config.bind = address;
    let router = create_router(config)?;
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(cancellation.cancelled_owned())
    .await
    .map_err(HttpServerError::Serve)
}

async fn request_guard(State(state): State<HttpState>, request: Request, next: Next) -> Response {
    if request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > BODY_LIMIT_BYTES_U64)
    {
        return secure_response(error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "The request body exceeds the 1 MiB limit.",
        ));
    }

    let client = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(IpAddr::V4(Ipv4Addr::LOCALHOST), |address| address.ip());
    if !state.limiter.check(client).await {
        let mut response = error_response(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "The per-client request limit has been exceeded.",
        );
        response
            .headers_mut()
            .insert("retry-after", HeaderValue::from_static("60"));
        return secure_response(response);
    }

    if !authorized(&state.config, &request) {
        let mut response = error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "A valid bearer token is required.",
        );
        response
            .headers_mut()
            .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        return secure_response(response);
    }

    let Ok(_permit) = state.concurrency.clone().try_acquire_owned() else {
        return secure_response(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "busy",
            "The server concurrency limit has been reached.",
        ));
    };

    match tokio::time::timeout(REQUEST_TIMEOUT, next.run(request)).await {
        Ok(response) => secure_response(response),
        Err(_) => secure_response(error_response(
            StatusCode::GATEWAY_TIMEOUT,
            "timeout",
            "The request exceeded the server time limit.",
        )),
    }
}

fn authorized(config: &HttpServerConfig, request: &Request) -> bool {
    let Some(expected) = config.bearer_token.as_ref() else {
        return true;
    };
    request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|header| header.as_bytes().strip_prefix(b"Bearer "))
        .is_some_and(|candidate| expected.matches(candidate))
}

fn secure_response(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        VERSION_HEADER,
        HeaderValue::from_static(env!("CARGO_PKG_VERSION")),
    );
    headers.insert(SCHEMA_HEADER, HeaderValue::from_static("2"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'"),
    );
    headers.insert(
        PERMISSIONS_POLICY,
        HeaderValue::from_static(
            "accelerometer=(), camera=(), geolocation=(), microphone=(), payment=(), usb=()",
        ),
    );
    response
}

async fn health() -> Response {
    success_response(ToolEnvelope {
        schema_version: SCHEMA_VERSION,
        status: ToolStatus::Ok,
        data: Some(HealthReport {
            healthy: true,
            version: env!("CARGO_PKG_VERSION"),
        }),
        freshness: unknown_freshness(),
        warnings: Vec::new(),
    })
}

async fn system_status(State(state): State<HttpState>) -> Response {
    status_response(state).await
}

async fn status(
    State(state): State<HttpState>,
    payload: Result<Json<StatusInput>, JsonRejection>,
) -> Response {
    if let Err(rejection) = payload {
        return json_rejection_response(&rejection);
    }
    status_response(state).await
}

async fn status_response(state: HttpState) -> Response {
    let config = Arc::clone(&state.config);
    run_blocking(move || status_workspace(&config.config_path, &config.database_path))
        .await
        .map_or_else(tool_failure_response, |status| {
            if status.workspace != state.config.workspace {
                return error_response(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "workspace_mismatch",
                    "The configured manifest does not match the fixed HTTP workspace.",
                );
            }
            let freshness = status.freshness.clone();
            success_response(ToolEnvelope {
                schema_version: SCHEMA_VERSION,
                status: tool_status_from_freshness(&freshness),
                data: Some(status),
                freshness,
                warnings: Vec::new(),
            })
        })
}

async fn query(
    State(state): State<HttpState>,
    payload: Result<Json<SearchInput>, JsonRejection>,
) -> Response {
    let Json(input) = match payload {
        Ok(input) => input,
        Err(rejection) => return json_rejection_response(&rejection),
    };
    let config = Arc::clone(&state.config);
    tool_service_response(
        run_blocking(move || {
            search_workspace_for_delivery(
                &config.database_path,
                &config.workspace,
                &input,
                &config.execution_policy,
                QueryActionCapabilities {
                    source_context: false,
                    explore: config.codegraph_enabled,
                },
            )
        })
        .await,
    )
}

async fn trace(
    State(state): State<HttpState>,
    payload: Result<Json<TraceInput>, JsonRejection>,
) -> Response {
    let Json(input) = match payload {
        Ok(input) => input,
        Err(rejection) => return json_rejection_response(&rejection),
    };
    let config = Arc::clone(&state.config);
    tool_service_response(
        run_blocking(move || trace_workspace(&config.database_path, &config.workspace, &input))
            .await,
    )
}

async fn explore(
    State(state): State<HttpState>,
    payload: Result<Json<ExploreInput>, JsonRejection>,
) -> Response {
    if !state.config.codegraph_enabled {
        return error_response(
            StatusCode::FORBIDDEN,
            CODEGRAPH_DISABLED_CODE,
            CODEGRAPH_DISABLED_MESSAGE,
        );
    }
    let Json(input) = match payload {
        Ok(input) => input,
        Err(rejection) => return json_rejection_response(&rejection),
    };
    let envelope: ToolEnvelope<ExploreReport> = explore_repository(
        &state.config.database_path,
        &state.config.workspace,
        &input,
        state.config.codegraph_binary.clone(),
        &state.config.execution_policy,
    )
    .await;
    success_response(envelope)
}

async fn impact(
    State(state): State<HttpState>,
    payload: Result<Json<ImpactRequest>, JsonRejection>,
) -> Response {
    let Json(input) = match payload {
        Ok(input) => input,
        Err(rejection) => return json_rejection_response(&rejection),
    };
    let config = Arc::clone(&state.config);
    let result: Result<ToolEnvelope<ImpactReport>, ToolFailure> = if config.codegraph_enabled {
        impact_workspace_with_codegraph(
            &config.database_path,
            &config.workspace,
            &input,
            config.codegraph_binary.clone(),
        )
        .await
        .map_err(|_| ToolFailure::Application)
    } else {
        run_blocking(move || impact_workspace(&config.database_path, &config.workspace, &input))
            .await
    };
    tool_service_response(result)
}

async fn analyze_changes(
    State(state): State<HttpState>,
    payload: Result<Json<ChangesInput>, JsonRejection>,
) -> Response {
    let Json(input) = match payload {
        Ok(input) => input,
        Err(rejection) => return json_rejection_response(&rejection),
    };
    tool_service_response(
        analyze_workspace_changes(
            &state.config.database_path,
            &state.config.workspace,
            &input,
            &ChangeAnalysisOptions::default(),
            None,
        )
        .await
        .map_err(|_| ToolFailure::Application),
    )
}

async fn contracts(
    State(state): State<HttpState>,
    payload: Result<Json<ContractsInput>, JsonRejection>,
) -> Response {
    let Json(input) = match payload {
        Ok(input) => input,
        Err(rejection) => return json_rejection_response(&rejection),
    };
    let search = SearchInput {
        query: input.query,
        node_kinds: vec![
            NodeKind::HttpOperation,
            NodeKind::GraphqlOperation,
            NodeKind::RpcMethod,
            NodeKind::EventChannel,
            NodeKind::EventSchema,
        ],
        repo_ids: input.repo_ids,
        service_ids: Vec::new(),
        community_ids: Vec::new(),
        offset: input.offset,
        limit: input.limit,
    };
    let config = Arc::clone(&state.config);
    let result: Result<ToolEnvelope<SearchReport>, ToolFailure> = run_blocking(move || {
        search_workspace_for_delivery(
            &config.database_path,
            &config.workspace,
            &search,
            &config.execution_policy,
            QueryActionCapabilities {
                source_context: false,
                explore: config.codegraph_enabled,
            },
        )
    })
    .await;
    tool_service_response(result)
}

async fn communities(
    State(state): State<HttpState>,
    payload: Result<Json<CommunityInput>, JsonRejection>,
) -> Response {
    let Json(input) = match payload {
        Ok(input) => input,
        Err(rejection) => return json_rejection_response(&rejection),
    };
    let config = Arc::clone(&state.config);
    let result: Result<ToolEnvelope<CommunityReport>, ToolFailure> = run_blocking(move || {
        communities_workspace(&config.database_path, &config.workspace, &input)
    })
    .await;
    tool_service_response(result)
}

async fn unsupported_route(method: Method) -> Response {
    let message = if matches!(
        method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) {
        "Mutating and administrative HTTP routes are disabled."
    } else {
        "The requested HTTP route is not supported."
    };
    error_response(StatusCode::NOT_FOUND, "not_found", message)
}

async fn method_not_allowed() -> Response {
    error_response(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "The HTTP method is not supported for this read-only route.",
    )
}

#[derive(Debug, Clone, Copy)]
enum ToolFailure {
    Application,
    Worker,
}

async fn run_blocking<T, F>(operation: F) -> Result<T, ToolFailure>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ApplicationError> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| ToolFailure::Worker)?
        .map_err(|_| ToolFailure::Application)
}

fn tool_service_response<T: Serialize>(result: Result<ToolEnvelope<T>, ToolFailure>) -> Response {
    result.map_or_else(tool_failure_response, success_response)
}

fn tool_failure_response(failure: ToolFailure) -> Response {
    let status = match failure {
        ToolFailure::Application => StatusCode::UNPROCESSABLE_ENTITY,
        ToolFailure::Worker => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error_response(
        status,
        "tool_failed",
        "The application service could not complete the request.",
    )
}

fn json_rejection_response(rejection: &JsonRejection) -> Response {
    let status = rejection.status();
    let (code, message) = if status == StatusCode::PAYLOAD_TOO_LARGE {
        (
            "payload_too_large",
            "The request body exceeds the 1 MiB limit.",
        )
    } else {
        ("invalid_json", "The JSON request body is invalid.")
    };
    error_response(status, code, message)
}

fn success_response<T: Serialize>(envelope: ToolEnvelope<T>) -> Response {
    (StatusCode::OK, Json(envelope)).into_response()
}

fn error_response(status: StatusCode, code: &'static str, message: &'static str) -> Response {
    (
        status,
        Json(ToolEnvelope {
            schema_version: SCHEMA_VERSION,
            status: ToolStatus::Error,
            data: Some(ErrorReport { code, message }),
            freshness: unknown_freshness(),
            warnings: Vec::new(),
        }),
    )
        .into_response()
}

fn unknown_freshness() -> FreshnessSummary {
    FreshnessSummary {
        overall: OverallFreshness::Unknown,
        stale_repositories: Vec::new(),
        reasons: Vec::new(),
    }
}

const fn tool_status_from_freshness(freshness: &FreshnessSummary) -> ToolStatus {
    match freshness.overall {
        OverallFreshness::Fresh => ToolStatus::Ok,
        OverallFreshness::Stale | OverallFreshness::Partial | OverallFreshness::Unknown => {
            ToolStatus::Degraded
        }
    }
}

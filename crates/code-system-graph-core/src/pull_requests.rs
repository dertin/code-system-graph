//! Explicitly enabled, transport-independent pull-request inspection.
//!
//! Provider clients in this module only speak documented public REST contracts. They never
//! perform network I/O themselves and never retain source patches or raw HTTP responses.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use url::Url;

const REDACTED: &str = "[REDACTED]";
const DEFAULT_MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
const DEFAULT_MAX_ITEMS: usize = 500;
const DEFAULT_MAX_PAGES: usize = 20;
const MAX_SAME_ORIGIN_REDIRECTS: usize = 3;
const MAX_PULL_REQUEST_LIST_LIMIT: usize = 100;

/// Pull-request service understood by the public provider adapter.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestProviderKind {
    /// GitHub's public REST API.
    GitHub,
    /// Bitbucket Cloud's public REST API 2.0.
    BitbucketCloud,
    /// Bitbucket Data Center, represented for configuration compatibility but not implemented.
    BitbucketDataCenter,
}

/// Stable provider coordinates for one pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestCoordinates {
    /// Provider that owns the pull request.
    pub provider: PullRequestProviderKind,
    /// GitHub owner or Bitbucket workspace.
    pub owner: String,
    /// GitHub repository name or Bitbucket repository slug.
    pub repository: String,
    /// Provider-native pull-request number.
    pub number: u64,
}

/// Secret bearer token whose debug representation is always redacted.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct PrAuthToken(String);

impl PrAuthToken {
    /// Wraps a token without validating or persisting it.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// Exposes the token only to an injected transport implementation.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for PrAuthToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PrAuthToken([REDACTED])")
    }
}

/// Explicit provider configuration and hard request bounds.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestProviderConfig {
    /// Provider contract selected for this client.
    pub provider: PullRequestProviderKind,
    /// Master opt-in switch. The default is `false`.
    pub enabled: bool,
    /// Provider API base URL.
    pub api_base_url: String,
    /// Exact allowed API base URLs.
    pub api_base_url_allowlist: Vec<String>,
    /// Allows plain HTTP only for loopback test servers.
    pub allow_loopback_http: bool,
    /// Ephemeral bearer token, excluded from serialization and schemas.
    #[serde(skip)]
    #[schemars(skip)]
    pub auth_token: PrAuthToken,
    /// Ephemeral Atlassian account email for Bitbucket API-token Basic authentication.
    #[serde(skip)]
    #[schemars(skip)]
    pub basic_auth_username: Option<String>,
    /// Per-request timeout in milliseconds.
    pub request_timeout_ms: u64,
    /// Maximum cumulative response bytes accepted by one inspection.
    pub max_output_bytes: usize,
    /// Maximum changed files, checks, or reviews retained per collection.
    pub max_items: usize,
    /// Maximum pages fetched per paginated endpoint.
    pub max_pages: usize,
    /// Structured result cache lifetime in seconds.
    pub cache_ttl_seconds: u64,
}

impl fmt::Debug for PullRequestProviderConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PullRequestProviderConfig")
            .field("provider", &self.provider)
            .field("enabled", &self.enabled)
            .field("api_base_url", &self.api_base_url)
            .field("api_base_url_allowlist", &self.api_base_url_allowlist)
            .field("allow_loopback_http", &self.allow_loopback_http)
            .field("auth_token", &REDACTED)
            .field(
                "basic_auth_username",
                &self.basic_auth_username.as_ref().map(|_| REDACTED),
            )
            .field("request_timeout_ms", &self.request_timeout_ms)
            .field("max_output_bytes", &self.max_output_bytes)
            .field("max_items", &self.max_items)
            .field("max_pages", &self.max_pages)
            .field("cache_ttl_seconds", &self.cache_ttl_seconds)
            .finish()
    }
}

impl Default for PullRequestProviderConfig {
    fn default() -> Self {
        Self {
            provider: PullRequestProviderKind::GitHub,
            enabled: false,
            api_base_url: "https://api.github.com".to_owned(),
            api_base_url_allowlist: vec!["https://api.github.com".to_owned()],
            allow_loopback_http: false,
            auth_token: PrAuthToken::default(),
            basic_auth_username: None,
            request_timeout_ms: 10_000,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            max_items: DEFAULT_MAX_ITEMS,
            max_pages: DEFAULT_MAX_PAGES,
            cache_ttl_seconds: 60,
        }
    }
}

impl PullRequestProviderConfig {
    /// Returns a disabled Bitbucket Cloud configuration with the public endpoint allowlisted.
    #[must_use]
    pub fn bitbucket_cloud() -> Self {
        Self {
            provider: PullRequestProviderKind::BitbucketCloud,
            api_base_url: "https://api.bitbucket.org/2.0".to_owned(),
            api_base_url_allowlist: vec!["https://api.bitbucket.org/2.0".to_owned()],
            ..Self::default()
        }
    }

    /// Validates URL policy and non-zero request bounds.
    ///
    /// # Errors
    ///
    /// Returns [`PullRequestError::InvalidConfiguration`] for an unsafe URL or empty bound.
    pub fn validate(&self) -> Result<(), PullRequestError> {
        if self.request_timeout_ms == 0
            || self.max_output_bytes == 0
            || self.max_items == 0
            || self.max_pages == 0
        {
            return Err(PullRequestError::InvalidConfiguration(
                "request bounds must be greater than zero".to_owned(),
            ));
        }
        if self.basic_auth_username.as_ref().is_some_and(|username| {
            username.trim().is_empty() || username.chars().any(char::is_control)
        }) {
            return Err(PullRequestError::InvalidConfiguration(
                "Basic-auth username must be non-empty and control-free".to_owned(),
            ));
        }
        let base = parse_base_url(&self.api_base_url)?;
        if is_loopback(&base) && self.allow_loopback_http {
            return Ok(());
        }
        if base.scheme() != "https" {
            return Err(PullRequestError::InvalidConfiguration(
                "provider API base URL must use HTTPS".to_owned(),
            ));
        }
        let normalized = normalize_base_url(base);
        let allowed = self
            .api_base_url_allowlist
            .iter()
            .filter_map(|candidate| Url::parse(candidate).ok())
            .any(|candidate| {
                candidate.scheme() == "https" && normalize_base_url(candidate) == normalized
            });
        if !allowed {
            return Err(PullRequestError::InvalidConfiguration(
                "provider API base URL is not allowlisted".to_owned(),
            ));
        }
        Ok(())
    }
}

/// One opt-in inspection request.
#[derive(Debug, Clone)]
pub struct PullRequestInspectRequest {
    /// Provider coordinates to inspect.
    pub coordinates: PullRequestCoordinates,
    /// Per-call confirmation that remote access is allowed.
    pub consent_to_remote_access: bool,
    /// Cooperative cancellation signal.
    pub cancellation: CancellationToken,
}

/// Provider-native lifecycle filter for pull-request listings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestListState {
    /// Return only pull requests that remain open.
    Open,
    /// Return provider-native closed pull requests, including merged pull requests.
    Closed,
    /// Return pull requests in every provider-native state.
    #[default]
    All,
}

/// One explicitly authorized request for a bounded pull-request list page.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestListRequest {
    /// Provider contract used for the request.
    pub provider: PullRequestProviderKind,
    /// GitHub owner or Bitbucket workspace.
    pub owner: String,
    /// GitHub repository name or Bitbucket repository slug.
    pub repository: String,
    /// Provider-native lifecycle filter.
    #[serde(default)]
    pub state: PullRequestListState,
    /// Opaque positive page cursor returned by a previous list response.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Maximum number of summaries to return, from 1 through 100.
    #[schemars(range(min = 1, max = 100))]
    pub limit: usize,
    /// Per-call confirmation that remote access is allowed.
    pub consent_to_remote_access: bool,
    /// Cooperative cancellation signal, excluded from serialized requests and schemas.
    #[serde(skip, default)]
    #[schemars(skip)]
    pub cancellation: CancellationToken,
}

/// Source-free pull-request metadata returned by list operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestSummary {
    /// Stable provider coordinates.
    pub coordinates: PullRequestCoordinates,
    /// Pull-request title.
    pub title: String,
    /// Human-facing provider URL.
    pub url: String,
    /// Normalized lifecycle state.
    pub state: PullRequestState,
    /// Whether the provider marks the pull request as draft.
    pub draft: bool,
    /// Provider-native author identity.
    pub author: String,
    /// Merge destination branch.
    pub base_branch: String,
    /// Proposed branch name, without source content.
    pub head_branch: String,
    /// Provider creation timestamp retained as an RFC 3339 string.
    pub created_at: String,
    /// Provider update timestamp retained as an RFC 3339 string.
    pub updated_at: String,
}

/// One bounded, deterministically ordered pull-request list page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestListPage {
    /// Source-free summaries sorted by provider-native pull-request number.
    pub items: Vec<PullRequestSummary>,
    /// Cursor for the next provider page, or `None` when no next page was reported.
    pub next_cursor: Option<String>,
    /// Whether the provider reported another page.
    pub has_more: bool,
    /// Whether a configured item bound reduced the caller's requested page size.
    pub truncated: bool,
    /// Last observed provider rate-limit metadata.
    pub rate_limit: PullRequestRateLimit,
    /// Non-fatal truncation or compatibility warnings.
    pub warnings: Vec<PullRequestWarning>,
}

/// Repository identity attached to a pull-request ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestRepository {
    /// Provider-native owner/name identity.
    pub full_name: String,
    /// Public repository URL when supplied by the provider.
    pub url: Option<String>,
}

/// Base or head ref metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestRef {
    /// Branch or named ref.
    pub name: String,
    /// Commit SHA or hash.
    pub sha: String,
    /// Repository containing the ref.
    pub repository: PullRequestRepository,
}

/// Provider-neutral pull-request state.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestState {
    /// Open for work or review.
    Open,
    /// Closed without a confirmed merge.
    Closed,
    /// Merged.
    Merged,
    /// Provider returned an unrecognized state.
    Unknown,
}

/// Common pull-request metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestMetadata {
    /// Provider-native identifier.
    pub id: String,
    /// Pull-request title.
    pub title: String,
    /// Human-facing URL.
    pub url: String,
    /// Normalized lifecycle state.
    pub state: PullRequestState,
    /// Whether the provider marks the pull request as draft.
    pub draft: bool,
    /// Provider-native author identity.
    pub author: String,
    /// Merge destination.
    pub base: PullRequestRef,
    /// Proposed source.
    pub head: PullRequestRef,
    /// Provider timestamp, retained as its RFC 3339 string.
    pub created_at: String,
    /// Provider timestamp, retained as its RFC 3339 string.
    pub updated_at: String,
}

/// Normalized changed-file status.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ChangedFileStatus {
    /// Newly added path.
    Added,
    /// Modified path.
    Modified,
    /// Removed path.
    Removed,
    /// Renamed path.
    Renamed,
    /// Copied path.
    Copied,
    /// Provider status was not recognized.
    Unknown,
}

/// Source-free changed-file summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestChangedFile {
    /// Normalized provider status.
    pub status: ChangedFileStatus,
    /// Previous path for renames or deletions.
    pub old_path: Option<String>,
    /// Current path for additions, modifications, or renames.
    pub new_path: Option<String>,
    /// Added line count.
    pub additions: u64,
    /// Deleted line count.
    pub deletions: u64,
    /// Whether provider metadata identifies or strongly implies a binary file.
    pub binary: bool,
    /// Whether a provider patch was present but intentionally discarded.
    pub patch_truncated: bool,
}

/// Normalized CI or check outcome.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    /// All reported checks completed successfully.
    Success,
    /// At least one check failed.
    Failure,
    /// Checks are queued or running.
    Pending,
    /// No usable status was reported.
    Unknown,
}

/// One provider check or commit status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestCheck {
    /// Check name or key.
    pub name: String,
    /// Normalized check state.
    pub state: CheckState,
    /// Provider details URL.
    pub url: Option<String>,
}

/// Aggregate CI state and bounded checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestCi {
    /// Aggregate state.
    pub state: CheckState,
    /// Individual checks or statuses.
    pub checks: Vec<PullRequestCheck>,
}

/// One review or participant decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestReview {
    /// Provider-native review or participant identifier.
    pub id: String,
    /// Reviewer identity.
    pub author: String,
    /// Provider review state.
    pub state: String,
    /// Submission timestamp when available.
    pub submitted_at: Option<String>,
}

/// Aggregate review readiness.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    /// At least one current approval and no current change request.
    Approved,
    /// A current review requests changes.
    ChangesRequested,
    /// Reviews exist but do not establish readiness.
    Pending,
    /// No usable review information was reported.
    Unknown,
}

/// Aggregate approvals and bounded review decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestReviewSummary {
    /// Aggregate review state.
    pub state: ReviewState,
    /// Number of current approving reviewers.
    pub approvals: usize,
    /// Reviews or participants retained under the item limit.
    pub reviews: Vec<PullRequestReview>,
}

/// Provider rate-limit metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestRateLimit {
    /// Request quota limit.
    pub limit: Option<u64>,
    /// Remaining requests.
    pub remaining: Option<u64>,
    /// Provider reset timestamp or duration string.
    pub reset: Option<String>,
    /// Retry delay in seconds supplied by `Retry-After`.
    pub retry_after_seconds: Option<u64>,
}

/// Non-fatal provider degradation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestWarning {
    /// Stable machine-readable warning code.
    pub code: String,
    /// Secret-free explanation.
    pub message: String,
}

/// Complete source-free inspection result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestInspection {
    /// Provider coordinates.
    pub coordinates: PullRequestCoordinates,
    /// Common metadata.
    pub metadata: PullRequestMetadata,
    /// Bounded changed-file summaries.
    pub changed_files: Vec<PullRequestChangedFile>,
    /// CI/check state.
    pub ci: PullRequestCi,
    /// Review/approval state.
    pub review: PullRequestReviewSummary,
    /// Last observed rate-limit metadata.
    pub rate_limit: PullRequestRateLimit,
    /// Non-fatal truncation or compatibility warnings.
    pub warnings: Vec<PullRequestWarning>,
    /// Deterministic fingerprint of metadata and changed-file summaries.
    pub fingerprint: String,
    /// Whether this result came directly from the structured cache.
    pub from_cache: bool,
}

/// Bounded HTTP method set needed by provider adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrHttpMethod {
    /// HTTP GET.
    Get,
}

/// Ephemeral provider authentication passed only to the HTTP transport.
#[derive(Clone, Default)]
pub enum PrHttpAuthentication {
    /// Anonymous public API request.
    #[default]
    None,
    /// OAuth or repository/project/workspace access token.
    Bearer(PrAuthToken),
    /// Atlassian account email and API token.
    Basic {
        /// Atlassian account email.
        username: String,
        /// Ephemeral API token used as the Basic-auth password.
        token: PrAuthToken,
    },
}

impl fmt::Debug for PrHttpAuthentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("PrHttpAuthentication::None"),
            Self::Bearer(_) => formatter.write_str("PrHttpAuthentication::Bearer([REDACTED])"),
            Self::Basic { .. } => formatter.write_str("PrHttpAuthentication::Basic([REDACTED])"),
        }
    }
}

/// HTTP request passed to an injected transport.
#[derive(Clone)]
pub struct PrHttpRequest {
    /// Request method.
    pub method: PrHttpMethod,
    /// Fully validated provider URL.
    pub url: String,
    /// Non-secret request headers.
    pub headers: BTreeMap<String, String>,
    /// Ephemeral provider authentication sent separately from ordinary headers.
    pub authentication: PrHttpAuthentication,
    /// Maximum accepted response body size.
    pub max_response_bytes: usize,
    /// Request timeout.
    pub timeout: Duration,
    /// Cooperative cancellation signal.
    pub cancellation: CancellationToken,
}

impl fmt::Debug for PrHttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrHttpRequest")
            .field("method", &self.method)
            .field("url", &self.url)
            .field("headers", &self.headers)
            .field("authentication", &self.authentication)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// Bounded HTTP response returned by an injected transport.
#[derive(Clone, PartialEq, Eq)]
pub struct PrHttpResponse {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: BTreeMap<String, String>,
    /// Response bytes, bounded by the request contract.
    pub body: Vec<u8>,
}

impl fmt::Debug for PrHttpResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrHttpResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

/// Secret-free transport failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("pull-request transport failed: {message}")]
pub struct PrHttpTransportError {
    /// Transport-supplied message, sanitized again by the provider.
    pub message: String,
}

/// Injectable asynchronous HTTP boundary.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait PrHttpTransport: Send + Sync {
    /// Sends one bounded request.
    ///
    /// # Errors
    ///
    /// Returns [`PrHttpTransportError`] for transport failures.
    async fn send(&self, request: PrHttpRequest) -> Result<PrHttpResponse, PrHttpTransportError>;
}

/// Production HTTPS transport with redirects disabled and bounded streaming responses.
#[derive(Debug, Clone)]
pub struct ReqwestPrHttpTransport {
    client: reqwest::Client,
}

impl ReqwestPrHttpTransport {
    /// Creates a reusable provider HTTP client.
    ///
    /// # Errors
    ///
    /// Returns [`PrHttpTransportError`] when the client cannot be constructed.
    pub fn new() -> Result<Self, PrHttpTransportError> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("code-system-graph/", env!("CARGO_PKG_VERSION")))
            .build()
            .map(|client| Self { client })
            .map_err(|error| PrHttpTransportError {
                message: error.to_string(),
            })
    }
}

#[async_trait]
impl PrHttpTransport for ReqwestPrHttpTransport {
    async fn send(&self, request: PrHttpRequest) -> Result<PrHttpResponse, PrHttpTransportError> {
        if request.cancellation.is_cancelled() {
            return Err(PrHttpTransportError {
                message: "request was cancelled".to_owned(),
            });
        }
        let mut builder = match request.method {
            PrHttpMethod::Get => self.client.get(&request.url),
        }
        .timeout(request.timeout);
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        match &request.authentication {
            PrHttpAuthentication::None => {}
            PrHttpAuthentication::Bearer(token) => {
                if !token.is_empty() {
                    builder = builder.bearer_auth(token.expose_secret());
                }
            }
            PrHttpAuthentication::Basic { username, token } => {
                builder = builder.basic_auth(username, Some(token.expose_secret()));
            }
        }
        let mut response = tokio::select! {
            () = request.cancellation.cancelled() => {
                return Err(PrHttpTransportError {
                    message: "request was cancelled".to_owned(),
                });
            }
            result = builder.send() => result.map_err(|error| PrHttpTransportError {
                message: error.to_string(),
            })?,
        };
        if response
            .content_length()
            .is_some_and(|length| length > request.max_response_bytes as u64)
        {
            return Err(PrHttpTransportError {
                message: "response exceeded the configured output limit".to_owned(),
            });
        }
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_owned(), value.to_owned()))
            })
            .collect();
        let mut body = Vec::new();
        loop {
            let chunk = tokio::select! {
                () = request.cancellation.cancelled() => {
                    return Err(PrHttpTransportError {
                        message: "request was cancelled".to_owned(),
                    });
                }
                result = response.chunk() => result.map_err(|error| PrHttpTransportError {
                    message: error.to_string(),
                })?,
            };
            let Some(chunk) = chunk else {
                break;
            };
            if body.len().saturating_add(chunk.len()) > request.max_response_bytes {
                return Err(PrHttpTransportError {
                    message: "response exceeded the configured output limit".to_owned(),
                });
            }
            body.extend_from_slice(&chunk);
        }
        Ok(PrHttpResponse {
            status,
            headers,
            body,
        })
    }
}

/// Pull-request provider failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PullRequestError {
    /// Provider use was not explicitly enabled.
    #[error("pull-request provider is disabled")]
    Disabled,
    /// The caller did not consent to remote access.
    #[error("remote pull-request access requires explicit per-request consent")]
    ConsentRequired,
    /// Configuration violates URL or budget policy.
    #[error("invalid pull-request provider configuration: {0}")]
    InvalidConfiguration(String),
    /// Request coordinates do not match the configured provider.
    #[error("pull-request coordinates do not match the configured provider")]
    ProviderMismatch,
    /// The selected provider contract is represented but intentionally unsupported.
    #[error("pull-request provider is not supported")]
    UnsupportedProvider,
    /// Operation was cancelled.
    #[error("pull-request inspection was cancelled")]
    Cancelled,
    /// Provider request exceeded its timeout.
    #[error("pull-request provider request timed out")]
    Timeout,
    /// Response exceeded the configured byte budget.
    #[error("pull-request provider response exceeded the configured output limit")]
    OutputLimitExceeded,
    /// Provider requested caller-managed backoff.
    #[error("pull-request provider rate limited the request")]
    RateLimited {
        /// HTTP response status.
        status: u16,
        /// Provider rate-limit metadata.
        metadata: PullRequestRateLimit,
    },
    /// Provider returned an unsuccessful API response.
    #[error("pull-request provider returned HTTP {status}: {message}")]
    Api {
        /// HTTP response status.
        status: u16,
        /// Bounded, token-redacted provider message.
        message: String,
    },
    /// Provider returned malformed or contract-incompatible JSON.
    #[error("malformed pull-request provider response: {0}")]
    MalformedResponse(String),
    /// Injected transport failed.
    #[error("pull-request transport failed: {0}")]
    Transport(String),
    /// Internal cache mutex was poisoned.
    #[error("pull-request cache is unavailable")]
    CacheUnavailable,
}

/// Asynchronous provider interface.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait PullRequestProvider: Send + Sync {
    /// Returns the provider contract implemented by this client.
    fn kind(&self) -> PullRequestProviderKind;

    /// Inspects one pull request after checking both opt-in gates.
    ///
    /// # Errors
    ///
    /// Returns [`PullRequestError`] for policy, transport, API, cancellation, or parse failures.
    async fn inspect(
        &self,
        request: PullRequestInspectRequest,
    ) -> Result<PullRequestInspection, PullRequestError>;

    /// Lists one bounded, source-free page after checking both opt-in gates.
    ///
    /// # Errors
    ///
    /// Returns [`PullRequestError`] for policy, bounds, transport, API, cancellation, or parse
    /// failures.
    async fn list(
        &self,
        request: PullRequestListRequest,
    ) -> Result<PullRequestListPage, PullRequestError>;
}

#[derive(Clone)]
struct CacheEntry {
    inspection: PullRequestInspection,
    etag: Option<String>,
    expires_at: Instant,
}

struct ProviderCore {
    config: PullRequestProviderConfig,
    transport: Arc<dyn PrHttpTransport>,
    cache: Mutex<BTreeMap<PullRequestCoordinatesKey, CacheEntry>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PullRequestCoordinatesKey {
    owner: String,
    repository: String,
    number: u64,
}

impl From<&PullRequestCoordinates> for PullRequestCoordinatesKey {
    fn from(value: &PullRequestCoordinates) -> Self {
        Self {
            owner: value.owner.clone(),
            repository: value.repository.clone(),
            number: value.number,
        }
    }
}

impl ProviderCore {
    fn new(
        config: PullRequestProviderConfig,
        expected: PullRequestProviderKind,
        transport: Arc<dyn PrHttpTransport>,
    ) -> Result<Self, PullRequestError> {
        config.validate()?;
        if config.provider != expected {
            return Err(PullRequestError::ProviderMismatch);
        }
        Ok(Self {
            config,
            transport,
            cache: Mutex::new(BTreeMap::new()),
        })
    }

    fn authorize(
        &self,
        request: &PullRequestInspectRequest,
        expected: PullRequestProviderKind,
    ) -> Result<(), PullRequestError> {
        if !self.config.enabled {
            return Err(PullRequestError::Disabled);
        }
        if !request.consent_to_remote_access {
            return Err(PullRequestError::ConsentRequired);
        }
        if request.coordinates.provider != expected {
            return Err(PullRequestError::ProviderMismatch);
        }
        if request.cancellation.is_cancelled() {
            return Err(PullRequestError::Cancelled);
        }
        Ok(())
    }

    fn authorize_list(
        &self,
        request: &PullRequestListRequest,
        expected: PullRequestProviderKind,
    ) -> Result<(), PullRequestError> {
        if !self.config.enabled {
            return Err(PullRequestError::Disabled);
        }
        if !request.consent_to_remote_access {
            return Err(PullRequestError::ConsentRequired);
        }
        if request.provider == PullRequestProviderKind::BitbucketDataCenter {
            return Err(PullRequestError::UnsupportedProvider);
        }
        if request.provider != expected {
            return Err(PullRequestError::ProviderMismatch);
        }
        if request.cancellation.is_cancelled() {
            return Err(PullRequestError::Cancelled);
        }
        validate_list_request(request)
    }

    fn authentication(&self) -> PrHttpAuthentication {
        if self.config.auth_token.is_empty() {
            PrHttpAuthentication::None
        } else if let Some(username) = &self.config.basic_auth_username {
            PrHttpAuthentication::Basic {
                username: username.clone(),
                token: self.config.auth_token.clone(),
            }
        } else {
            PrHttpAuthentication::Bearer(self.config.auth_token.clone())
        }
    }

    fn fresh_cache(
        &self,
        coordinates: &PullRequestCoordinates,
    ) -> Result<Option<PullRequestInspection>, PullRequestError> {
        let cache = self
            .cache
            .lock()
            .map_err(|_| PullRequestError::CacheUnavailable)?;
        Ok(cache
            .get(&PullRequestCoordinatesKey::from(coordinates))
            .filter(|entry| Instant::now() < entry.expires_at)
            .map(|entry| {
                let mut inspection = entry.inspection.clone();
                inspection.from_cache = true;
                inspection
            }))
    }

    fn stale_cache(
        &self,
        coordinates: &PullRequestCoordinates,
    ) -> Result<Option<CacheEntry>, PullRequestError> {
        self.cache
            .lock()
            .map_err(|_| PullRequestError::CacheUnavailable)
            .map(|cache| {
                cache
                    .get(&PullRequestCoordinatesKey::from(coordinates))
                    .cloned()
            })
    }

    fn store_cache(
        &self,
        coordinates: &PullRequestCoordinates,
        inspection: &PullRequestInspection,
        etag: Option<String>,
    ) -> Result<(), PullRequestError> {
        let mut cached = inspection.clone();
        cached.from_cache = false;
        let entry = CacheEntry {
            inspection: cached,
            etag,
            expires_at: Instant::now() + Duration::from_secs(self.config.cache_ttl_seconds),
        };
        self.cache
            .lock()
            .map_err(|_| PullRequestError::CacheUnavailable)?
            .insert(PullRequestCoordinatesKey::from(coordinates), entry);
        Ok(())
    }

    async fn get_json(
        &self,
        url: Url,
        etag: Option<&str>,
        cancellation: &CancellationToken,
        budget: &mut ResponseBudget,
    ) -> Result<FetchJson, PullRequestError> {
        let response = self
            .send_json_request(url, etag, cancellation, budget)
            .await?;
        if response.status == 304 {
            return Ok(FetchJson::NotModified);
        }
        let rate_limited = response.status == 429
            || (response.status == 403
                && (budget.rate_limit.remaining == Some(0)
                    || budget.rate_limit.retry_after_seconds.is_some()));
        if rate_limited {
            return Err(PullRequestError::RateLimited {
                status: response.status,
                metadata: budget.rate_limit.clone(),
            });
        }
        if !(200..300).contains(&response.status) {
            let message = api_error_message(&response.body, &self.config.auth_token);
            return Err(PullRequestError::Api {
                status: response.status,
                message,
            });
        }
        let value = serde_json::from_slice(&response.body)
            .map_err(|error| PullRequestError::MalformedResponse(error.to_string()))?;
        Ok(FetchJson::Value {
            value,
            etag: header(&response.headers, "etag").map(str::to_owned),
            headers: response.headers,
        })
    }

    async fn send_json_request(
        &self,
        mut url: Url,
        etag: Option<&str>,
        cancellation: &CancellationToken,
        budget: &mut ResponseBudget,
    ) -> Result<PrHttpResponse, PullRequestError> {
        let mut redirects = 0;
        loop {
            if cancellation.is_cancelled() {
                return Err(PullRequestError::Cancelled);
            }
            let remaining = self
                .config
                .max_output_bytes
                .saturating_sub(budget.output_bytes);
            if remaining == 0 {
                return Err(PullRequestError::OutputLimitExceeded);
            }
            let mut headers = BTreeMap::from([
                ("accept".to_owned(), "application/json".to_owned()),
                ("user-agent".to_owned(), "code-system-graph".to_owned()),
            ]);
            if let Some(etag) = etag {
                headers.insert("if-none-match".to_owned(), etag.to_owned());
            }
            let transport_request = PrHttpRequest {
                method: PrHttpMethod::Get,
                url: url.to_string(),
                headers,
                authentication: self.authentication(),
                max_response_bytes: remaining,
                timeout: Duration::from_millis(self.config.request_timeout_ms),
                cancellation: cancellation.clone(),
            };
            let timeout = tokio::time::sleep(Duration::from_millis(self.config.request_timeout_ms));
            tokio::pin!(timeout);
            let response = tokio::select! {
                () = cancellation.cancelled() => return Err(PullRequestError::Cancelled),
                () = &mut timeout => return Err(PullRequestError::Timeout),
                result = self.transport.send(transport_request) => {
                    result.map_err(|error| {
                        PullRequestError::Transport(redact_message(
                            &error.message,
                            &self.config.auth_token,
                        ))
                    })?
                }
            };
            if response.body.len() > remaining {
                return Err(PullRequestError::OutputLimitExceeded);
            }
            budget.output_bytes += response.body.len();
            budget.rate_limit = rate_limit_from_headers(&response.headers);
            if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                if redirects >= MAX_SAME_ORIGIN_REDIRECTS {
                    return Err(PullRequestError::Api {
                        status: response.status,
                        message: "provider redirect limit exceeded".to_owned(),
                    });
                }
                let location = header(&response.headers, "location").ok_or_else(|| {
                    PullRequestError::MalformedResponse(
                        "provider redirect omitted the location header".to_owned(),
                    )
                })?;
                let redirected = url.join(location).map_err(|_| {
                    PullRequestError::MalformedResponse(
                        "provider returned an invalid redirect URL".to_owned(),
                    )
                })?;
                if redirected.origin() != url.origin() {
                    return Err(PullRequestError::Api {
                        status: response.status,
                        message: "provider redirect crossed the configured origin".to_owned(),
                    });
                }
                url = redirected;
                redirects += 1;
                continue;
            }
            return Ok(response);
        }
    }

    fn url(&self, segments: &[&str]) -> Result<Url, PullRequestError> {
        let mut url = parse_base_url(&self.config.api_base_url)?;
        {
            let mut path = url.path_segments_mut().map_err(|()| {
                PullRequestError::InvalidConfiguration(
                    "provider API base URL cannot be a base".to_owned(),
                )
            })?;
            path.pop_if_empty();
            path.extend(segments);
        }
        Ok(url)
    }
}

#[derive(Default)]
struct ResponseBudget {
    output_bytes: usize,
    rate_limit: PullRequestRateLimit,
}

enum FetchJson {
    NotModified,
    Value {
        value: Value,
        etag: Option<String>,
        headers: BTreeMap<String, String>,
    },
}

/// GitHub public REST pull-request adapter.
pub struct GitHubProvider {
    core: ProviderCore,
}

impl GitHubProvider {
    /// Creates a GitHub provider over an injected transport.
    ///
    /// # Errors
    ///
    /// Returns [`PullRequestError`] when configuration is invalid or selects another provider.
    pub fn new(
        config: PullRequestProviderConfig,
        transport: Arc<dyn PrHttpTransport>,
    ) -> Result<Self, PullRequestError> {
        ProviderCore::new(config, PullRequestProviderKind::GitHub, transport)
            .map(|core| Self { core })
    }
}

#[async_trait]
impl PullRequestProvider for GitHubProvider {
    fn kind(&self) -> PullRequestProviderKind {
        PullRequestProviderKind::GitHub
    }

    async fn inspect(
        &self,
        request: PullRequestInspectRequest,
    ) -> Result<PullRequestInspection, PullRequestError> {
        self.core
            .authorize(&request, PullRequestProviderKind::GitHub)?;
        if let Some(cached) = self.core.fresh_cache(&request.coordinates)? {
            return Ok(cached);
        }
        let stale = self.core.stale_cache(&request.coordinates)?;
        let mut budget = ResponseBudget::default();
        let number = request.coordinates.number.to_string();
        let metadata_url = self.core.url(&[
            "repos",
            &request.coordinates.owner,
            &request.coordinates.repository,
            "pulls",
            &number,
        ])?;
        let metadata_fetch = self
            .core
            .get_json(
                metadata_url,
                stale.as_ref().and_then(|entry| entry.etag.as_deref()),
                &request.cancellation,
                &mut budget,
            )
            .await?;
        let (metadata_json, etag) = match metadata_fetch {
            FetchJson::NotModified => {
                let mut cached = stale
                    .ok_or_else(|| {
                        PullRequestError::MalformedResponse(
                            "provider returned 304 without a cached representation".to_owned(),
                        )
                    })?
                    .inspection;
                cached.from_cache = true;
                self.core.store_cache(
                    &request.coordinates,
                    &cached,
                    self.core
                        .stale_cache(&request.coordinates)?
                        .and_then(|entry| entry.etag),
                )?;
                return Ok(cached);
            }
            FetchJson::Value { value, etag, .. } => (value, etag),
        };
        let metadata = parse_github_metadata(&metadata_json)?;
        let mut warnings = Vec::new();
        let files = self
            .github_files(&request, &number, &mut budget, &mut warnings)
            .await?;
        let checks = self
            .github_checks(&request, &metadata.head.sha, &mut budget, &mut warnings)
            .await?;
        let reviews = self
            .github_reviews(&request, &number, &mut budget, &mut warnings)
            .await?;
        let fingerprint = fingerprint(&metadata, &files)?;
        let inspection = PullRequestInspection {
            coordinates: request.coordinates.clone(),
            metadata,
            changed_files: files,
            ci: summarize_checks(checks),
            review: summarize_github_reviews(reviews),
            rate_limit: budget.rate_limit,
            warnings,
            fingerprint,
            from_cache: false,
        };
        self.core
            .store_cache(&request.coordinates, &inspection, etag)?;
        Ok(inspection)
    }

    async fn list(
        &self,
        request: PullRequestListRequest,
    ) -> Result<PullRequestListPage, PullRequestError> {
        self.core
            .authorize_list(&request, PullRequestProviderKind::GitHub)?;
        let page_limit = request
            .limit
            .min(self.core.config.max_items)
            .min(MAX_PULL_REQUEST_LIST_LIMIT);
        let mut url = self
            .core
            .url(&["repos", &request.owner, &request.repository, "pulls"])?;
        let expected_path = url.path().to_owned();
        let page_limit_text = page_limit.to_string();
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("state", github_list_state(request.state))
                .append_pair("per_page", &page_limit_text)
                .append_pair("sort", "created")
                .append_pair("direction", "asc");
            if let Some(cursor) = &request.cursor {
                query.append_pair("page", cursor);
            }
        }
        let mut budget = ResponseBudget::default();
        let fetch = self
            .core
            .get_json(url, None, &request.cancellation, &mut budget)
            .await?;
        let FetchJson::Value { value, headers, .. } = fetch else {
            return Err(PullRequestError::MalformedResponse(
                "unexpected 304 for uncached pull-request list".to_owned(),
            ));
        };
        let values = required_array(&value, "pull requests")?;
        if values.len() > page_limit {
            return Err(PullRequestError::MalformedResponse(
                "GitHub returned more pull requests than the requested page size".to_owned(),
            ));
        }
        let mut items = values
            .iter()
            .map(|value| parse_github_summary(value, &request.owner, &request.repository))
            .collect::<Result<Vec<_>, _>>()?;
        items.sort_by_key(|summary| summary.coordinates.number);
        let base = parse_base_url(&self.core.config.api_base_url)?;
        let next_cursor = github_next_url(&headers, &base)?
            .map(|next| pagination_cursor(&next, &base, &expected_path, "per_page"))
            .transpose()?;
        Ok(list_page(
            items,
            next_cursor,
            request.limit,
            page_limit,
            budget.rate_limit,
        ))
    }
}

impl GitHubProvider {
    async fn github_files(
        &self,
        request: &PullRequestInspectRequest,
        number: &str,
        budget: &mut ResponseBudget,
        warnings: &mut Vec<PullRequestWarning>,
    ) -> Result<Vec<PullRequestChangedFile>, PullRequestError> {
        let initial = self.core.url(&[
            "repos",
            &request.coordinates.owner,
            &request.coordinates.repository,
            "pulls",
            number,
            "files",
        ])?;
        self.github_paginated(initial, &request.cancellation, budget, warnings, |value| {
            parse_github_file(value)
        })
        .await
    }

    async fn github_checks(
        &self,
        request: &PullRequestInspectRequest,
        sha: &str,
        budget: &mut ResponseBudget,
        warnings: &mut Vec<PullRequestWarning>,
    ) -> Result<Vec<PullRequestCheck>, PullRequestError> {
        let initial = self.core.url(&[
            "repos",
            &request.coordinates.owner,
            &request.coordinates.repository,
            "commits",
            sha,
            "check-runs",
        ])?;
        self.github_paginated_key(
            initial,
            "check_runs",
            &request.cancellation,
            budget,
            warnings,
            parse_github_check,
        )
        .await
    }

    async fn github_reviews(
        &self,
        request: &PullRequestInspectRequest,
        number: &str,
        budget: &mut ResponseBudget,
        warnings: &mut Vec<PullRequestWarning>,
    ) -> Result<Vec<PullRequestReview>, PullRequestError> {
        let initial = self.core.url(&[
            "repos",
            &request.coordinates.owner,
            &request.coordinates.repository,
            "pulls",
            number,
            "reviews",
        ])?;
        self.github_paginated(initial, &request.cancellation, budget, warnings, |value| {
            parse_github_review(value)
        })
        .await
    }

    async fn github_paginated<T, F>(
        &self,
        initial: Url,
        cancellation: &CancellationToken,
        budget: &mut ResponseBudget,
        warnings: &mut Vec<PullRequestWarning>,
        parse: F,
    ) -> Result<Vec<T>, PullRequestError>
    where
        F: Fn(&Value) -> Result<T, PullRequestError>,
    {
        self.github_paginated_impl(initial, None, cancellation, budget, warnings, parse)
            .await
    }

    async fn github_paginated_key<T, F>(
        &self,
        initial: Url,
        key: &str,
        cancellation: &CancellationToken,
        budget: &mut ResponseBudget,
        warnings: &mut Vec<PullRequestWarning>,
        parse: F,
    ) -> Result<Vec<T>, PullRequestError>
    where
        F: Fn(&Value) -> Result<T, PullRequestError>,
    {
        self.github_paginated_impl(initial, Some(key), cancellation, budget, warnings, parse)
            .await
    }

    async fn github_paginated_impl<T, F>(
        &self,
        initial: Url,
        key: Option<&str>,
        cancellation: &CancellationToken,
        budget: &mut ResponseBudget,
        warnings: &mut Vec<PullRequestWarning>,
        parse: F,
    ) -> Result<Vec<T>, PullRequestError>
    where
        F: Fn(&Value) -> Result<T, PullRequestError>,
    {
        let mut output = Vec::new();
        let mut next = Some(initial);
        let mut pages = 0;
        while let Some(url) = next {
            if pages >= self.core.config.max_pages || output.len() >= self.core.config.max_items {
                warnings.push(limit_warning("pagination"));
                break;
            }
            pages += 1;
            let response = self.core.get_json(url, None, cancellation, budget).await?;
            let FetchJson::Value { value, headers, .. } = response else {
                return Err(PullRequestError::MalformedResponse(
                    "unexpected 304 for uncached page".to_owned(),
                ));
            };
            let values = match key {
                Some(key) => required_array(required_field(&value, key)?, key)?,
                None => required_array(&value, "page")?,
            };
            for item in values {
                if output.len() >= self.core.config.max_items {
                    warnings.push(limit_warning("items"));
                    break;
                }
                output.push(parse(item)?);
            }
            next = github_next_url(&headers, &parse_base_url(&self.core.config.api_base_url)?)?;
        }
        Ok(output)
    }
}

/// Bitbucket Cloud public REST 2.0 pull-request adapter.
pub struct BitbucketProvider {
    core: ProviderCore,
}

impl BitbucketProvider {
    /// Creates a Bitbucket Cloud provider over an injected transport.
    ///
    /// # Errors
    ///
    /// Returns [`PullRequestError`] when configuration is invalid or selects another provider.
    pub fn new(
        config: PullRequestProviderConfig,
        transport: Arc<dyn PrHttpTransport>,
    ) -> Result<Self, PullRequestError> {
        ProviderCore::new(config, PullRequestProviderKind::BitbucketCloud, transport)
            .map(|core| Self { core })
    }
}

#[async_trait]
impl PullRequestProvider for BitbucketProvider {
    fn kind(&self) -> PullRequestProviderKind {
        PullRequestProviderKind::BitbucketCloud
    }

    async fn inspect(
        &self,
        request: PullRequestInspectRequest,
    ) -> Result<PullRequestInspection, PullRequestError> {
        self.core
            .authorize(&request, PullRequestProviderKind::BitbucketCloud)?;
        if let Some(cached) = self.core.fresh_cache(&request.coordinates)? {
            return Ok(cached);
        }
        let stale = self.core.stale_cache(&request.coordinates)?;
        let mut budget = ResponseBudget::default();
        let number = request.coordinates.number.to_string();
        let metadata_url = self.core.url(&[
            "repositories",
            &request.coordinates.owner,
            &request.coordinates.repository,
            "pullrequests",
            &number,
        ])?;
        let metadata_fetch = self
            .core
            .get_json(
                metadata_url,
                stale.as_ref().and_then(|entry| entry.etag.as_deref()),
                &request.cancellation,
                &mut budget,
            )
            .await?;
        let (metadata_json, etag) = match metadata_fetch {
            FetchJson::NotModified => {
                let entry = stale.ok_or_else(|| {
                    PullRequestError::MalformedResponse(
                        "provider returned 304 without a cached representation".to_owned(),
                    )
                })?;
                let mut cached = entry.inspection;
                cached.from_cache = true;
                self.core
                    .store_cache(&request.coordinates, &cached, entry.etag)?;
                return Ok(cached);
            }
            FetchJson::Value { value, etag, .. } => (value, etag),
        };
        let metadata = parse_bitbucket_metadata(&metadata_json)?;
        let mut warnings = Vec::new();
        let files_url = self.core.url(&[
            "repositories",
            &request.coordinates.owner,
            &request.coordinates.repository,
            "pullrequests",
            &number,
            "diffstat",
        ])?;
        let files = self
            .bitbucket_optional_paginated(
                files_url,
                &request.cancellation,
                &mut budget,
                &mut warnings,
                "changed files",
                parse_bitbucket_file,
            )
            .await?;
        let checks_url = self.core.url(&[
            "repositories",
            &request.coordinates.owner,
            &request.coordinates.repository,
            "commit",
            &metadata.head.sha,
            "statuses",
        ])?;
        let checks = self
            .bitbucket_optional_paginated(
                checks_url,
                &request.cancellation,
                &mut budget,
                &mut warnings,
                "commit statuses",
                parse_bitbucket_check,
            )
            .await?;
        let review = parse_bitbucket_reviews(&metadata_json, self.core.config.max_items)?;
        let fingerprint = fingerprint(&metadata, &files)?;
        let inspection = PullRequestInspection {
            coordinates: request.coordinates.clone(),
            metadata,
            changed_files: files,
            ci: summarize_checks(checks),
            review,
            rate_limit: budget.rate_limit,
            warnings,
            fingerprint,
            from_cache: false,
        };
        self.core
            .store_cache(&request.coordinates, &inspection, etag)?;
        Ok(inspection)
    }

    async fn list(
        &self,
        request: PullRequestListRequest,
    ) -> Result<PullRequestListPage, PullRequestError> {
        self.core
            .authorize_list(&request, PullRequestProviderKind::BitbucketCloud)?;
        let page_limit = request
            .limit
            .min(self.core.config.max_items)
            .min(MAX_PULL_REQUEST_LIST_LIMIT);
        let mut url = self.core.url(&[
            "repositories",
            &request.owner,
            &request.repository,
            "pullrequests",
        ])?;
        let expected_path = url.path().to_owned();
        let page_limit_text = page_limit.to_string();
        let page = request
            .cursor
            .as_deref()
            .map(parse_list_cursor)
            .transpose()?;
        let page_text = page.map(|page| page.to_string());
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("pagelen", &page_limit_text)
                .append_pair("sort", "created_on");
            match request.state {
                PullRequestListState::Open => {
                    query.append_pair("state", "OPEN");
                }
                PullRequestListState::Closed => {
                    query
                        .append_pair("state", "MERGED")
                        .append_pair("state", "FULFILLED")
                        .append_pair("state", "DECLINED")
                        .append_pair("state", "SUPERSEDED");
                }
                PullRequestListState::All => {}
            }
            if let Some(page) = &page_text {
                query.append_pair("page", page);
            }
        }
        let mut budget = ResponseBudget::default();
        let fetch = self
            .core
            .get_json(url, None, &request.cancellation, &mut budget)
            .await?;
        let FetchJson::Value { value, .. } = fetch else {
            return Err(PullRequestError::MalformedResponse(
                "unexpected 304 for uncached pull-request list".to_owned(),
            ));
        };
        let values = required_array(required_field(&value, "values")?, "values")?;
        if values.len() > page_limit {
            return Err(PullRequestError::MalformedResponse(
                "Bitbucket returned more pull requests than the requested page size".to_owned(),
            ));
        }
        let mut items = values
            .iter()
            .map(|value| parse_bitbucket_summary(value, &request.owner, &request.repository))
            .collect::<Result<Vec<_>, _>>()?;
        items.sort_by_key(|summary| summary.coordinates.number);
        let base = parse_base_url(&self.core.config.api_base_url)?;
        let next_cursor = optional_string(&value, "next")
            .map(Url::parse)
            .transpose()
            .map_err(|_| {
                PullRequestError::MalformedResponse(
                    "Bitbucket returned an invalid pagination URL".to_owned(),
                )
            })?
            .map(|next| pagination_cursor(&next, &base, &expected_path, "pagelen"))
            .transpose()?;
        Ok(list_page(
            items,
            next_cursor,
            request.limit,
            page_limit,
            budget.rate_limit,
        ))
    }
}

impl BitbucketProvider {
    async fn bitbucket_optional_paginated<T, F>(
        &self,
        initial: Url,
        cancellation: &CancellationToken,
        budget: &mut ResponseBudget,
        warnings: &mut Vec<PullRequestWarning>,
        subject: &str,
        parse: F,
    ) -> Result<Vec<T>, PullRequestError>
    where
        F: Fn(&Value) -> Result<T, PullRequestError>,
    {
        match self
            .bitbucket_paginated(initial, cancellation, budget, warnings, parse)
            .await
        {
            Ok(items) => Ok(items),
            Err(PullRequestError::Api {
                status: 401 | 403, ..
            }) => {
                warnings.push(provider_scope_warning(subject));
                Ok(Vec::new())
            }
            Err(error) => Err(error),
        }
    }

    async fn bitbucket_paginated<T, F>(
        &self,
        initial: Url,
        cancellation: &CancellationToken,
        budget: &mut ResponseBudget,
        warnings: &mut Vec<PullRequestWarning>,
        parse: F,
    ) -> Result<Vec<T>, PullRequestError>
    where
        F: Fn(&Value) -> Result<T, PullRequestError>,
    {
        let mut output = Vec::new();
        let mut next = Some(initial);
        let mut pages = 0;
        while let Some(url) = next {
            if pages >= self.core.config.max_pages || output.len() >= self.core.config.max_items {
                warnings.push(limit_warning("pagination"));
                break;
            }
            pages += 1;
            let response = self.core.get_json(url, None, cancellation, budget).await?;
            let FetchJson::Value { value, .. } = response else {
                return Err(PullRequestError::MalformedResponse(
                    "unexpected 304 for uncached page".to_owned(),
                ));
            };
            for item in required_array(required_field(&value, "values")?, "values")? {
                if output.len() >= self.core.config.max_items {
                    warnings.push(limit_warning("items"));
                    break;
                }
                output.push(parse(item)?);
            }
            next = optional_string(&value, "next")
                .map(Url::parse)
                .transpose()
                .map_err(|_| {
                    PullRequestError::MalformedResponse(
                        "Bitbucket returned an invalid pagination URL".to_owned(),
                    )
                })?;
            if let Some(candidate) = &next {
                validate_pagination_origin(
                    &parse_base_url(&self.core.config.api_base_url)?,
                    candidate,
                )?;
            }
        }
        Ok(output)
    }
}

fn github_next_url(
    headers: &BTreeMap<String, String>,
    base: &Url,
) -> Result<Option<Url>, PullRequestError> {
    let Some(link) = header(headers, "link") else {
        return Ok(None);
    };
    for part in link.split(',') {
        let mut sections = part.trim().split(';');
        let Some(target) = sections.next() else {
            continue;
        };
        let is_next = sections.any(|section| section.trim() == r#"rel="next""#);
        if !is_next {
            continue;
        }
        let raw = target.trim().trim_start_matches('<').trim_end_matches('>');
        let candidate = Url::parse(raw).map_err(|_| {
            PullRequestError::MalformedResponse(
                "GitHub returned an invalid pagination URL".to_owned(),
            )
        })?;
        validate_pagination_origin(base, &candidate)?;
        return Ok(Some(candidate));
    }
    Ok(None)
}

fn validate_list_request(request: &PullRequestListRequest) -> Result<(), PullRequestError> {
    if !(1..=MAX_PULL_REQUEST_LIST_LIMIT).contains(&request.limit) {
        return Err(PullRequestError::InvalidConfiguration(
            "pull-request list limit must be between 1 and 100".to_owned(),
        ));
    }
    for (name, value) in [
        ("owner", request.owner.as_str()),
        ("repository", request.repository.as_str()),
    ] {
        if value.trim().is_empty()
            || matches!(value, "." | "..")
            || value.chars().any(char::is_control)
        {
            return Err(PullRequestError::InvalidConfiguration(format!(
                "pull-request list {name} must be non-empty and control-free"
            )));
        }
    }
    if let Some(cursor) = request.cursor.as_deref() {
        parse_list_cursor(cursor)?;
    }
    Ok(())
}

fn parse_list_cursor(cursor: &str) -> Result<u64, PullRequestError> {
    cursor
        .parse::<u64>()
        .ok()
        .filter(|page| *page > 0)
        .ok_or_else(|| {
            PullRequestError::InvalidConfiguration(
                "pull-request list cursor must be a positive page number".to_owned(),
            )
        })
}

fn github_list_state(state: PullRequestListState) -> &'static str {
    match state {
        PullRequestListState::Open => "open",
        PullRequestListState::Closed => "closed",
        PullRequestListState::All => "all",
    }
}

fn pagination_cursor(
    candidate: &Url,
    base: &Url,
    expected_path: &str,
    page_size_parameter: &str,
) -> Result<String, PullRequestError> {
    validate_pagination_origin(base, candidate)?;
    if !candidate.username().is_empty()
        || candidate.password().is_some()
        || candidate.fragment().is_some()
        || candidate.path() != expected_path
    {
        return Err(PullRequestError::MalformedResponse(
            "pagination URL escaped the pull-request list endpoint".to_owned(),
        ));
    }
    if let Some(page_size) = candidate
        .query_pairs()
        .find(|(name, _)| name == page_size_parameter)
        .map(|(_, value)| value)
    {
        let valid = page_size
            .parse::<usize>()
            .is_ok_and(|value| (1..=MAX_PULL_REQUEST_LIST_LIMIT).contains(&value));
        if !valid {
            return Err(PullRequestError::MalformedResponse(
                "provider pagination exceeded the pull-request list limit".to_owned(),
            ));
        }
    }
    let mut pages = candidate
        .query_pairs()
        .filter(|(name, _)| name == "page")
        .map(|(_, value)| value);
    let page = pages.next().ok_or_else(|| {
        PullRequestError::MalformedResponse(
            "provider pagination omitted a positive page cursor".to_owned(),
        )
    })?;
    if pages.next().is_some() {
        return Err(PullRequestError::MalformedResponse(
            "provider pagination returned multiple page cursors".to_owned(),
        ));
    }
    let page = page
        .parse::<u64>()
        .ok()
        .filter(|page| *page > 0)
        .ok_or_else(|| {
            PullRequestError::MalformedResponse(
                "provider pagination returned an invalid page cursor".to_owned(),
            )
        })?;
    Ok(page.to_string())
}

fn list_page(
    items: Vec<PullRequestSummary>,
    next_cursor: Option<String>,
    requested_limit: usize,
    effective_limit: usize,
    rate_limit: PullRequestRateLimit,
) -> PullRequestListPage {
    let has_more = next_cursor.is_some();
    let truncated = requested_limit > effective_limit && has_more;
    let warnings = truncated
        .then(|| PullRequestWarning {
            code: "configured_limit_applied".to_owned(),
            message: format!(
                "Pull-request list page size was reduced to the configured limit of \
                 {effective_limit}"
            ),
        })
        .into_iter()
        .collect();
    PullRequestListPage {
        items,
        next_cursor,
        has_more,
        truncated,
        rate_limit,
        warnings,
    }
}

fn parse_base_url(value: &str) -> Result<Url, PullRequestError> {
    let url = Url::parse(value).map_err(|_| {
        PullRequestError::InvalidConfiguration("provider API base URL is invalid".to_owned())
    })?;
    if url.cannot_be_a_base()
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(PullRequestError::InvalidConfiguration(
            "provider API base URL must be an absolute credential-free URL".to_owned(),
        ));
    }
    Ok(url)
}

fn normalize_base_url(mut url: Url) -> String {
    url.set_fragment(None);
    url.set_query(None);
    let trimmed = url.path().trim_end_matches('/').to_owned();
    url.set_path(&trimmed);
    url.to_string().trim_end_matches('/').to_owned()
}

fn is_loopback(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

fn validate_pagination_origin(base: &Url, candidate: &Url) -> Result<(), PullRequestError> {
    if base.scheme() != candidate.scheme()
        || base.host_str() != candidate.host_str()
        || base.port_or_known_default() != candidate.port_or_known_default()
    {
        return Err(PullRequestError::MalformedResponse(
            "pagination URL escaped the configured provider origin".to_owned(),
        ));
    }
    Ok(())
}

fn header<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn rate_limit_from_headers(headers: &BTreeMap<String, String>) -> PullRequestRateLimit {
    PullRequestRateLimit {
        limit: header(headers, "x-ratelimit-limit").and_then(|value| value.parse().ok()),
        remaining: header(headers, "x-ratelimit-remaining").and_then(|value| value.parse().ok()),
        reset: header(headers, "x-ratelimit-reset").map(str::to_owned),
        retry_after_seconds: header(headers, "retry-after").and_then(|value| value.parse().ok()),
    }
}

fn api_error_message(body: &[u8], token: &PrAuthToken) -> String {
    let parsed = serde_json::from_slice::<Value>(body).ok();
    let message = parsed
        .as_ref()
        .and_then(|value| {
            optional_string(value, "message").or_else(|| {
                value
                    .get("error")
                    .and_then(|error| optional_string(error, "message"))
            })
        })
        .unwrap_or("provider returned an error");
    redact_message(message, token)
}

fn redact_message(message: &str, token: &PrAuthToken) -> String {
    let bounded: String = message.chars().take(512).collect();
    if token.is_empty() {
        bounded
    } else {
        bounded.replace(token.expose_secret(), REDACTED)
    }
}

fn required_field<'a>(value: &'a Value, field: &str) -> Result<&'a Value, PullRequestError> {
    value.get(field).ok_or_else(|| {
        PullRequestError::MalformedResponse(format!("missing required field `{field}`"))
    })
}

fn required_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, PullRequestError> {
    required_field(value, field)?
        .as_str()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| {
            PullRequestError::MalformedResponse(format!("field `{field}` must be a string"))
        })
}

fn optional_string<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str)
}

fn required_array<'a>(value: &'a Value, field: &str) -> Result<&'a [Value], PullRequestError> {
    value.as_array().map(Vec::as_slice).ok_or_else(|| {
        PullRequestError::MalformedResponse(format!("field `{field}` must be an array"))
    })
}

fn nested<'a>(value: &'a Value, fields: &[&str]) -> Result<&'a Value, PullRequestError> {
    fields
        .iter()
        .try_fold(value, |current, field| required_field(current, field))
}

fn nested_string<'a>(value: &'a Value, fields: &[&str]) -> Result<&'a str, PullRequestError> {
    let (last, parents) = fields
        .split_last()
        .ok_or_else(|| PullRequestError::MalformedResponse("empty JSON field path".to_owned()))?;
    required_string(nested(value, parents)?, last)
}

fn value_id(value: &Value, field: &str) -> Result<String, PullRequestError> {
    let id = required_field(value, field)?;
    id.as_str()
        .map(str::to_owned)
        .or_else(|| id.as_u64().map(|number| number.to_string()))
        .ok_or_else(|| {
            PullRequestError::MalformedResponse(format!(
                "field `{field}` must be a string or integer"
            ))
        })
}

fn required_u64(value: &Value, field: &str) -> Result<u64, PullRequestError> {
    required_field(value, field)?.as_u64().ok_or_else(|| {
        PullRequestError::MalformedResponse(format!(
            "field `{field}` must be a non-negative integer"
        ))
    })
}

fn parse_github_summary(
    value: &Value,
    owner: &str,
    repository: &str,
) -> Result<PullRequestSummary, PullRequestError> {
    let state = if value.get("merged_at").is_some_and(|value| !value.is_null()) {
        PullRequestState::Merged
    } else {
        match required_string(value, "state")? {
            "open" => PullRequestState::Open,
            "closed" => PullRequestState::Closed,
            _ => PullRequestState::Unknown,
        }
    };
    Ok(PullRequestSummary {
        coordinates: PullRequestCoordinates {
            provider: PullRequestProviderKind::GitHub,
            owner: owner.to_owned(),
            repository: repository.to_owned(),
            number: required_u64(value, "number")?,
        },
        title: required_string(value, "title")?.to_owned(),
        url: required_string(value, "html_url")?.to_owned(),
        state,
        draft: value.get("draft").and_then(Value::as_bool).unwrap_or(false),
        author: nested_string(value, &["user", "login"])?.to_owned(),
        base_branch: nested_string(value, &["base", "ref"])?.to_owned(),
        head_branch: nested_string(value, &["head", "ref"])?.to_owned(),
        created_at: required_string(value, "created_at")?.to_owned(),
        updated_at: required_string(value, "updated_at")?.to_owned(),
    })
}

fn parse_github_metadata(value: &Value) -> Result<PullRequestMetadata, PullRequestError> {
    let merged = value
        .get("merged")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let state = if merged {
        PullRequestState::Merged
    } else {
        match required_string(value, "state")? {
            "open" => PullRequestState::Open,
            "closed" => PullRequestState::Closed,
            _ => PullRequestState::Unknown,
        }
    };
    Ok(PullRequestMetadata {
        id: value_id(value, "id")?,
        title: required_string(value, "title")?.to_owned(),
        url: required_string(value, "html_url")?.to_owned(),
        state,
        draft: value.get("draft").and_then(Value::as_bool).unwrap_or(false),
        author: nested_string(value, &["user", "login"])?.to_owned(),
        base: parse_github_ref(required_field(value, "base")?)?,
        head: parse_github_ref(required_field(value, "head")?)?,
        created_at: required_string(value, "created_at")?.to_owned(),
        updated_at: required_string(value, "updated_at")?.to_owned(),
    })
}

fn parse_github_ref(value: &Value) -> Result<PullRequestRef, PullRequestError> {
    Ok(PullRequestRef {
        name: required_string(value, "ref")?.to_owned(),
        sha: required_string(value, "sha")?.to_owned(),
        repository: PullRequestRepository {
            full_name: nested_string(value, &["repo", "full_name"])?.to_owned(),
            url: nested(value, &["repo"])
                .ok()
                .and_then(|repo| optional_string(repo, "html_url"))
                .map(str::to_owned),
        },
    })
}

fn parse_github_file(value: &Value) -> Result<PullRequestChangedFile, PullRequestError> {
    let filename = required_string(value, "filename")?.to_owned();
    let provider_status = required_string(value, "status")?;
    let status = match provider_status {
        "added" => ChangedFileStatus::Added,
        "modified" | "changed" => ChangedFileStatus::Modified,
        "removed" => ChangedFileStatus::Removed,
        "renamed" => ChangedFileStatus::Renamed,
        "copied" => ChangedFileStatus::Copied,
        _ => ChangedFileStatus::Unknown,
    };
    let old_path = if matches!(status, ChangedFileStatus::Renamed) {
        optional_string(value, "previous_filename").map(str::to_owned)
    } else if matches!(status, ChangedFileStatus::Removed) {
        Some(filename.clone())
    } else {
        None
    };
    let new_path = (!matches!(status, ChangedFileStatus::Removed)).then_some(filename);
    let additions = value.get("additions").and_then(Value::as_u64).unwrap_or(0);
    let deletions = value.get("deletions").and_then(Value::as_u64).unwrap_or(0);
    let patch_present = value.get("patch").and_then(Value::as_str).is_some();
    Ok(PullRequestChangedFile {
        status,
        old_path,
        new_path,
        additions,
        deletions,
        binary: !patch_present && additions.saturating_add(deletions) > 0,
        patch_truncated: patch_present,
    })
}

fn parse_github_check(value: &Value) -> Result<PullRequestCheck, PullRequestError> {
    let status = optional_string(value, "status").unwrap_or("unknown");
    let conclusion = optional_string(value, "conclusion").unwrap_or("unknown");
    let state = if status == "completed" {
        match conclusion {
            "success" | "neutral" | "skipped" => CheckState::Success,
            "failure" | "timed_out" | "cancelled" | "action_required" => CheckState::Failure,
            _ => CheckState::Unknown,
        }
    } else {
        CheckState::Pending
    };
    Ok(PullRequestCheck {
        name: required_string(value, "name")?.to_owned(),
        state,
        url: optional_string(value, "details_url").map(str::to_owned),
    })
}

fn parse_github_review(value: &Value) -> Result<PullRequestReview, PullRequestError> {
    Ok(PullRequestReview {
        id: value_id(value, "id")?,
        author: nested_string(value, &["user", "login"])?.to_owned(),
        state: required_string(value, "state")?.to_ascii_lowercase(),
        submitted_at: optional_string(value, "submitted_at").map(str::to_owned),
    })
}

fn parse_bitbucket_metadata(value: &Value) -> Result<PullRequestMetadata, PullRequestError> {
    let state = match required_string(value, "state")? {
        "OPEN" => PullRequestState::Open,
        "MERGED" | "FULFILLED" => PullRequestState::Merged,
        "DECLINED" | "SUPERSEDED" => PullRequestState::Closed,
        _ => PullRequestState::Unknown,
    };
    Ok(PullRequestMetadata {
        id: value_id(value, "id")?,
        title: required_string(value, "title")?.to_owned(),
        url: nested_string(value, &["links", "html", "href"])?.to_owned(),
        state,
        draft: value.get("draft").and_then(Value::as_bool).unwrap_or(false),
        author: nested_string(value, &["author", "display_name"])?.to_owned(),
        base: parse_bitbucket_ref(required_field(value, "destination")?)?,
        head: parse_bitbucket_ref(required_field(value, "source")?)?,
        created_at: required_string(value, "created_on")?.to_owned(),
        updated_at: required_string(value, "updated_on")?.to_owned(),
    })
}

fn parse_bitbucket_summary(
    value: &Value,
    owner: &str,
    repository: &str,
) -> Result<PullRequestSummary, PullRequestError> {
    let state = match required_string(value, "state")? {
        "OPEN" => PullRequestState::Open,
        "MERGED" | "FULFILLED" => PullRequestState::Merged,
        "DECLINED" | "SUPERSEDED" => PullRequestState::Closed,
        _ => PullRequestState::Unknown,
    };
    Ok(PullRequestSummary {
        coordinates: PullRequestCoordinates {
            provider: PullRequestProviderKind::BitbucketCloud,
            owner: owner.to_owned(),
            repository: repository.to_owned(),
            number: required_u64(value, "id")?,
        },
        title: required_string(value, "title")?.to_owned(),
        url: nested_string(value, &["links", "html", "href"])?.to_owned(),
        state,
        draft: value.get("draft").and_then(Value::as_bool).unwrap_or(false),
        author: nested_string(value, &["author", "display_name"])?.to_owned(),
        base_branch: nested_string(value, &["destination", "branch", "name"])?.to_owned(),
        head_branch: nested_string(value, &["source", "branch", "name"])?.to_owned(),
        created_at: required_string(value, "created_on")?.to_owned(),
        updated_at: required_string(value, "updated_on")?.to_owned(),
    })
}

fn parse_bitbucket_ref(value: &Value) -> Result<PullRequestRef, PullRequestError> {
    let repository = required_field(value, "repository")?;
    Ok(PullRequestRef {
        name: nested_string(value, &["branch", "name"])?.to_owned(),
        sha: nested_string(value, &["commit", "hash"])?.to_owned(),
        repository: PullRequestRepository {
            full_name: required_string(repository, "full_name")?.to_owned(),
            url: nested(repository, &["links", "html"])
                .ok()
                .and_then(|html| optional_string(html, "href"))
                .map(str::to_owned),
        },
    })
}

fn parse_bitbucket_file(value: &Value) -> Result<PullRequestChangedFile, PullRequestError> {
    let status_text = required_string(value, "status")?;
    let status = match status_text {
        "added" => ChangedFileStatus::Added,
        "modified" => ChangedFileStatus::Modified,
        "removed" => ChangedFileStatus::Removed,
        "renamed" => ChangedFileStatus::Renamed,
        _ => ChangedFileStatus::Unknown,
    };
    let old_path = value
        .get("old")
        .and_then(|old| optional_string(old, "path"))
        .map(str::to_owned);
    let new_path = value
        .get("new")
        .and_then(|new| optional_string(new, "path"))
        .map(str::to_owned);
    let binary = value
        .get("new")
        .or_else(|| value.get("old"))
        .and_then(|entry| optional_string(entry, "type"))
        .is_some_and(|kind| kind.eq_ignore_ascii_case("binary"));
    Ok(PullRequestChangedFile {
        status,
        old_path,
        new_path,
        additions: value
            .get("lines_added")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        deletions: value
            .get("lines_removed")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        binary,
        patch_truncated: false,
    })
}

fn parse_bitbucket_check(value: &Value) -> Result<PullRequestCheck, PullRequestError> {
    let state = match required_string(value, "state")? {
        "SUCCESSFUL" => CheckState::Success,
        "FAILED" | "STOPPED" => CheckState::Failure,
        "INPROGRESS" => CheckState::Pending,
        _ => CheckState::Unknown,
    };
    Ok(PullRequestCheck {
        name: required_string(value, "key")?.to_owned(),
        state,
        url: optional_string(value, "url").map(str::to_owned),
    })
}

fn parse_bitbucket_reviews(
    metadata: &Value,
    max_items: usize,
) -> Result<PullRequestReviewSummary, PullRequestError> {
    let Some(participants) = metadata.get("participants") else {
        return Ok(PullRequestReviewSummary {
            state: ReviewState::Unknown,
            approvals: 0,
            reviews: Vec::new(),
        });
    };
    let mut reviews = Vec::new();
    let mut approvals = 0;
    let mut changes_requested = false;
    for participant in required_array(participants, "participants")?
        .iter()
        .take(max_items)
    {
        let approved = participant
            .get("approved")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let state = optional_string(participant, "state")
            .unwrap_or(if approved { "approved" } else { "pending" })
            .to_ascii_lowercase();
        approvals += usize::from(approved);
        changes_requested |= state.contains("changes_requested");
        let user = required_field(participant, "user")?;
        let author = optional_string(user, "display_name")
            .or_else(|| optional_string(user, "nickname"))
            .ok_or_else(|| {
                PullRequestError::MalformedResponse(
                    "Bitbucket participant has no identity".to_owned(),
                )
            })?;
        let id = optional_string(user, "uuid").unwrap_or(author);
        reviews.push(PullRequestReview {
            id: id.to_owned(),
            author: author.to_owned(),
            state,
            submitted_at: None,
        });
    }
    let state = if changes_requested {
        ReviewState::ChangesRequested
    } else if approvals > 0 {
        ReviewState::Approved
    } else if reviews.is_empty() {
        ReviewState::Unknown
    } else {
        ReviewState::Pending
    };
    Ok(PullRequestReviewSummary {
        state,
        approvals,
        reviews,
    })
}

fn summarize_checks(checks: Vec<PullRequestCheck>) -> PullRequestCi {
    let state = if checks.is_empty() {
        CheckState::Unknown
    } else if checks
        .iter()
        .any(|check| check.state == CheckState::Failure)
    {
        CheckState::Failure
    } else if checks
        .iter()
        .any(|check| check.state == CheckState::Pending)
    {
        CheckState::Pending
    } else if checks
        .iter()
        .all(|check| check.state == CheckState::Success)
    {
        CheckState::Success
    } else {
        CheckState::Unknown
    };
    PullRequestCi { state, checks }
}

fn summarize_github_reviews(reviews: Vec<PullRequestReview>) -> PullRequestReviewSummary {
    let mut latest = BTreeMap::new();
    for review in &reviews {
        latest.insert(review.author.as_str(), review.state.as_str());
    }
    let approvals = latest
        .values()
        .filter(|state| **state == "approved")
        .count();
    let changes_requested = latest.values().any(|state| *state == "changes_requested");
    let state = if changes_requested {
        ReviewState::ChangesRequested
    } else if approvals > 0 {
        ReviewState::Approved
    } else if reviews.is_empty() {
        ReviewState::Unknown
    } else {
        ReviewState::Pending
    };
    PullRequestReviewSummary {
        state,
        approvals,
        reviews,
    }
}

fn fingerprint(
    metadata: &PullRequestMetadata,
    files: &[PullRequestChangedFile],
) -> Result<String, PullRequestError> {
    serde_json::to_vec(&(metadata, files))
        .map(|bytes| blake3::hash(&bytes).to_hex().to_string())
        .map_err(|error| PullRequestError::MalformedResponse(error.to_string()))
}

fn limit_warning(subject: &str) -> PullRequestWarning {
    PullRequestWarning {
        code: "limit_reached".to_owned(),
        message: format!("{subject} stopped at the configured limit"),
    }
}

fn provider_scope_warning(subject: &str) -> PullRequestWarning {
    PullRequestWarning {
        code: "provider_scope_missing".to_owned(),
        message: format!(
            "Provider credentials cannot read {subject}; the corresponding result is unknown"
        ),
    }
}

/// Contract role used by semantic overlap and ordering.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ContractRole {
    /// Defines or serves the contract.
    Provider,
    /// Consumes the contract.
    Consumer,
}

/// Contract touched by a pull request.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestContractChange {
    /// Stable contract identity.
    pub contract: String,
    /// Relationship of the changed repository to the contract.
    pub role: ContractRole,
    /// Whether compatibility analysis classified the change as breaking.
    pub breaking: bool,
}

/// Readiness freshness for conservative ordering.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestReadiness {
    /// CI and review data establish readiness.
    Ready,
    /// CI or review data establishes a blocker.
    Blocked,
    /// Required readiness data is unavailable.
    Unknown,
    /// Readiness data is stale.
    Stale,
}

/// Provider-neutral semantic inputs for one pull request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestSemanticInput {
    /// Stable fingerprint used as an ordering identity.
    pub fingerprint: String,
    /// Changed paths.
    pub files: BTreeSet<String>,
    /// Contracts touched by the change.
    pub contracts: BTreeSet<PullRequestContractChange>,
    /// Stable service identities touched by the change.
    pub services: BTreeSet<String>,
    /// Stable community identities touched by the change.
    pub communities: BTreeSet<String>,
    /// Fingerprints that must be merged before this change.
    pub depends_on: BTreeSet<String>,
    /// Current CI and review readiness.
    pub readiness: PullRequestReadiness,
}

/// Strongest deterministic semantic overlap classification.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestOverlapKind {
    /// No common semantic input.
    Disjoint,
    /// At least one path overlaps.
    File,
    /// At least one contract overlaps.
    Contract,
    /// At least one service overlaps.
    Service,
    /// At least one graph community overlaps.
    Community,
    /// Shared migration or breaking-contract changes require coordination.
    Conflicting,
}

/// Deterministic semantic overlap result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestOverlap {
    /// Strongest overlap classification.
    pub kind: PullRequestOverlapKind,
    /// Shared paths in lexical order.
    pub files: Vec<String>,
    /// Shared contract identities in lexical order.
    pub contracts: Vec<String>,
    /// Shared service identities in lexical order.
    pub services: Vec<String>,
    /// Shared community identities in lexical order.
    pub communities: Vec<String>,
}

/// Computes deterministic overlap without treating missing evidence as safety.
#[must_use]
pub fn semantic_pull_request_overlap(
    left: &PullRequestSemanticInput,
    right: &PullRequestSemanticInput,
) -> PullRequestOverlap {
    let files = intersection(&left.files, &right.files);
    let left_contracts: BTreeSet<_> = left
        .contracts
        .iter()
        .map(|change| change.contract.clone())
        .collect();
    let right_contracts: BTreeSet<_> = right
        .contracts
        .iter()
        .map(|change| change.contract.clone())
        .collect();
    let contracts = intersection(&left_contracts, &right_contracts);
    let services = intersection(&left.services, &right.services);
    let communities = intersection(&left.communities, &right.communities);
    let migration_conflict = files.iter().any(|path| is_migration_path(path));
    let breaking_contract_conflict = contracts.iter().any(|contract| {
        left.contracts
            .iter()
            .chain(&right.contracts)
            .any(|change| change.contract == *contract && change.breaking)
    });
    let kind = if migration_conflict || breaking_contract_conflict {
        PullRequestOverlapKind::Conflicting
    } else if !contracts.is_empty() {
        PullRequestOverlapKind::Contract
    } else if !services.is_empty() {
        PullRequestOverlapKind::Service
    } else if !communities.is_empty() {
        PullRequestOverlapKind::Community
    } else if !files.is_empty() {
        PullRequestOverlapKind::File
    } else {
        PullRequestOverlapKind::Disjoint
    };
    PullRequestOverlap {
        kind,
        files,
        contracts,
        services,
        communities,
    }
}

fn intersection(left: &BTreeSet<String>, right: &BTreeSet<String>) -> Vec<String> {
    left.intersection(right).cloned().collect()
}

fn is_migration_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    normalized.contains("/migrations/")
        || normalized.starts_with("migrations/")
        || normalized.ends_with(".migration.sql")
}

/// Suggested relationship between two review or merge operations.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PullRequestOrder {
    /// Review or merge the left input first.
    LeftFirst,
    /// Review or merge the right input first.
    RightFirst,
    /// Changes should be coordinated rather than linearly ordered.
    Coordinated,
    /// Evidence does not justify a definitive order.
    NoDefinitiveOrder,
}

/// Deterministic review/merge ordering suggestion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PullRequestOrderSuggestion {
    /// Suggested relationship.
    pub order: PullRequestOrder,
    /// Stable machine-readable reasons in lexical order.
    pub reasons: Vec<String>,
}

/// Suggests a conservative review/merge order from dependencies, contracts, and readiness.
#[must_use]
pub fn suggest_pull_request_order(
    left: &PullRequestSemanticInput,
    right: &PullRequestSemanticInput,
) -> PullRequestOrderSuggestion {
    if matches!(
        left.readiness,
        PullRequestReadiness::Unknown | PullRequestReadiness::Stale
    ) || matches!(
        right.readiness,
        PullRequestReadiness::Unknown | PullRequestReadiness::Stale
    ) {
        return suggestion(
            PullRequestOrder::NoDefinitiveOrder,
            ["readiness_unknown_or_stale"],
        );
    }
    let overlap = semantic_pull_request_overlap(left, right);
    if overlap.kind == PullRequestOverlapKind::Conflicting {
        return suggestion(
            PullRequestOrder::Coordinated,
            ["shared_migration_or_breaking_contract"],
        );
    }
    let left_depends_on_right = left.depends_on.contains(&right.fingerprint);
    let right_depends_on_left = right.depends_on.contains(&left.fingerprint);
    match (left_depends_on_right, right_depends_on_left) {
        (true, true) => {
            return suggestion(PullRequestOrder::Coordinated, ["dependency_cycle"]);
        }
        (true, false) => {
            return suggestion(PullRequestOrder::RightFirst, ["dependency_direction"]);
        }
        (false, true) => {
            return suggestion(PullRequestOrder::LeftFirst, ["dependency_direction"]);
        }
        (false, false) => {}
    }
    if let Some(order) = breaking_provider_order(left, right) {
        return suggestion(order, ["breaking_contract_provider_before_consumer"]);
    }
    match (left.readiness, right.readiness) {
        (PullRequestReadiness::Ready, PullRequestReadiness::Blocked) => {
            suggestion(PullRequestOrder::LeftFirst, ["ci_review_readiness"])
        }
        (PullRequestReadiness::Blocked, PullRequestReadiness::Ready) => {
            suggestion(PullRequestOrder::RightFirst, ["ci_review_readiness"])
        }
        _ => suggestion(
            PullRequestOrder::NoDefinitiveOrder,
            ["no_ordering_evidence"],
        ),
    }
}

fn breaking_provider_order(
    left: &PullRequestSemanticInput,
    right: &PullRequestSemanticInput,
) -> Option<PullRequestOrder> {
    for left_change in &left.contracts {
        for right_change in &right.contracts {
            if left_change.contract != right_change.contract {
                continue;
            }
            if left_change.breaking && left_change.role == ContractRole::Provider {
                return Some(PullRequestOrder::LeftFirst);
            }
            if right_change.breaking && right_change.role == ContractRole::Provider {
                return Some(PullRequestOrder::RightFirst);
            }
        }
    }
    None
}

fn suggestion(
    order: PullRequestOrder,
    reasons: impl IntoIterator<Item = &'static str>,
) -> PullRequestOrderSuggestion {
    let mut reasons: Vec<_> = reasons.into_iter().map(str::to_owned).collect();
    reasons.sort();
    reasons.dedup();
    PullRequestOrderSuggestion { order, reasons }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use serde_json::json;

    use super::*;

    fn one_response_server(response: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let address = listener.local_addr().expect("test server address");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept test request");
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).expect("read test request");
            stream.write_all(response).expect("write test response");
        });
        format!("http://{address}/test")
    }

    #[tokio::test]
    async fn production_transport_streams_bounded_responses_without_redirects() {
        let url = one_response_server(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/blocked\r\nContent-Length: 4\r\nConnection: close\r\n\r\nbody");
        let transport = ReqwestPrHttpTransport::new().expect("production transport");
        let response = transport
            .send(PrHttpRequest {
                method: PrHttpMethod::Get,
                url,
                headers: BTreeMap::new(),
                authentication: PrHttpAuthentication::None,
                max_response_bytes: 4,
                timeout: Duration::from_secs(2),
                cancellation: CancellationToken::new(),
            })
            .await
            .expect("bounded response");
        assert_eq!((response.status, response.body), (302, b"body".to_vec()));
    }

    #[tokio::test]
    async fn production_transport_rejects_declared_oversized_responses() {
        let url = one_response_server(
            b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\noversize",
        );
        let transport = ReqwestPrHttpTransport::new().expect("production transport");
        let error = transport
            .send(PrHttpRequest {
                method: PrHttpMethod::Get,
                url,
                headers: BTreeMap::new(),
                authentication: PrHttpAuthentication::None,
                max_response_bytes: 4,
                timeout: Duration::from_secs(2),
                cancellation: CancellationToken::new(),
            })
            .await
            .expect_err("oversized response");
        assert!(error.message.contains("output limit"));
    }

    #[derive(Default)]
    struct MockTransport {
        responses: Mutex<VecDeque<Result<PrHttpResponse, PrHttpTransportError>>>,
        requests: Mutex<Vec<PrHttpRequest>>,
    }

    impl MockTransport {
        fn with(responses: Vec<PrHttpResponse>) -> Arc<Self> {
            Arc::new(Self {
                responses: Mutex::new(responses.into_iter().map(Ok).collect()),
                requests: Mutex::default(),
            })
        }

        fn request_count(&self) -> usize {
            self.requests.lock().expect("requests lock").len()
        }

        fn request_header(&self, index: usize, name: &str) -> Option<String> {
            self.requests
                .lock()
                .expect("requests lock")
                .get(index)
                .and_then(|request| request.headers.get(name))
                .cloned()
        }

        fn request_url(&self, index: usize) -> Option<String> {
            self.requests
                .lock()
                .expect("requests lock")
                .get(index)
                .map(|request| request.url.clone())
        }

        fn request_authentication(&self, index: usize) -> Option<PrHttpAuthentication> {
            self.requests
                .lock()
                .expect("requests lock")
                .get(index)
                .map(|request| request.authentication.clone())
        }
    }

    #[async_trait]
    impl PrHttpTransport for MockTransport {
        async fn send(
            &self,
            request: PrHttpRequest,
        ) -> Result<PrHttpResponse, PrHttpTransportError> {
            self.requests.lock().expect("requests lock").push(request);
            self.responses
                .lock()
                .expect("responses lock")
                .pop_front()
                .expect("mock response")
        }
    }

    fn response(status: u16, value: &Value) -> PrHttpResponse {
        PrHttpResponse {
            status,
            headers: BTreeMap::new(),
            body: serde_json::to_vec(value).expect("JSON"),
        }
    }

    fn github_metadata() -> Value {
        json!({
            "id": 10, "title": "Change", "html_url": "https://example/pr/1",
            "state": "open", "draft": false, "user": {"login": "alice"},
            "base": {"ref": "main", "sha": "base", "repo": {"full_name": "o/r", "html_url": "https://example/o/r"}},
            "head": {"ref": "feature", "sha": "head", "repo": {"full_name": "o/r", "html_url": "https://example/o/r"}},
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z"
        })
    }

    fn bitbucket_metadata() -> Value {
        json!({
            "id": 7, "title": "Change", "state": "OPEN", "draft": false,
            "links": {"html": {"href": "https://example/pr/7"}},
            "author": {"display_name": "Alice"},
            "destination": {"branch": {"name": "main"}, "commit": {"hash": "base"},
                "repository": {"full_name": "w/r", "links": {"html": {"href": "https://example/w/r"}}}},
            "source": {"branch": {"name": "feature"}, "commit": {"hash": "head"},
                "repository": {"full_name": "w/r", "links": {"html": {"href": "https://example/w/r"}}}},
            "created_on": "2026-01-01T00:00:00Z", "updated_on": "2026-01-02T00:00:00Z",
            "participants": [{"approved": true, "state": "approved",
                "user": {"display_name": "Bob", "uuid": "{bob}"}}]
        })
    }

    fn github_summary(number: u64, state: &str) -> Value {
        json!({
            "number": number,
            "title": format!("Change {number}"),
            "html_url": format!("https://github.example/o/r/pull/{number}"),
            "state": state,
            "merged_at": null,
            "draft": false,
            "user": {"login": "alice"},
            "base": {"ref": "main"},
            "head": {"ref": format!("feature-{number}")},
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-02T00:00:00Z"
        })
    }

    fn bitbucket_summary(number: u64, state: &str) -> Value {
        json!({
            "id": number,
            "title": format!("Change {number}"),
            "state": state,
            "draft": false,
            "links": {"html": {"href": format!("https://bitbucket.example/w/r/pull-requests/{number}")}},
            "author": {"display_name": "Alice"},
            "destination": {"branch": {"name": "main"}},
            "source": {"branch": {"name": format!("feature-{number}")}},
            "created_on": "2026-01-01T00:00:00Z",
            "updated_on": "2026-01-02T00:00:00Z"
        })
    }

    fn github_config() -> PullRequestProviderConfig {
        PullRequestProviderConfig {
            enabled: true,
            cache_ttl_seconds: 60,
            ..PullRequestProviderConfig::default()
        }
    }

    fn bitbucket_config() -> PullRequestProviderConfig {
        PullRequestProviderConfig {
            enabled: true,
            cache_ttl_seconds: 60,
            ..PullRequestProviderConfig::bitbucket_cloud()
        }
    }

    fn request(kind: PullRequestProviderKind) -> PullRequestInspectRequest {
        PullRequestInspectRequest {
            coordinates: PullRequestCoordinates {
                provider: kind,
                owner: "o".to_owned(),
                repository: "r".to_owned(),
                number: 1,
            },
            consent_to_remote_access: true,
            cancellation: CancellationToken::new(),
        }
    }

    fn list_request(kind: PullRequestProviderKind) -> PullRequestListRequest {
        PullRequestListRequest {
            provider: kind,
            owner: "o".to_owned(),
            repository: "r".to_owned(),
            state: PullRequestListState::All,
            cursor: None,
            limit: 10,
            consent_to_remote_access: true,
            cancellation: CancellationToken::new(),
        }
    }

    fn github_success_responses() -> Vec<PrHttpResponse> {
        vec![
            response(200, &github_metadata()),
            response(
                200,
                &json!([{"filename":"src/lib.rs","status":"modified","additions":2,"deletions":1,"patch":"secret source"}]),
            ),
            response(
                200,
                &json!({"check_runs":[{"name":"test","status":"completed","conclusion":"success"}]}),
            ),
            response(
                200,
                &json!([{"id":1,"user":{"login":"bob"},"state":"APPROVED","submitted_at":"2026-01-02T00:00:00Z"}]),
            ),
        ]
    }

    fn semantic(name: &str) -> PullRequestSemanticInput {
        PullRequestSemanticInput {
            fingerprint: name.to_owned(),
            files: BTreeSet::new(),
            contracts: BTreeSet::new(),
            services: BTreeSet::new(),
            communities: BTreeSet::new(),
            depends_on: BTreeSet::new(),
            readiness: PullRequestReadiness::Ready,
        }
    }

    #[test]
    fn default_configuration_is_disabled() {
        assert!(!PullRequestProviderConfig::default().enabled);
    }

    #[test]
    fn configuration_rejects_non_https_remote_url() {
        let config = PullRequestProviderConfig {
            api_base_url: "http://example.com".to_owned(),
            api_base_url_allowlist: vec!["http://example.com".to_owned()],
            ..PullRequestProviderConfig::default()
        };
        assert!(matches!(
            config.validate(),
            Err(PullRequestError::InvalidConfiguration(_))
        ));
    }

    #[test]
    fn configuration_allows_explicit_loopback_mock() {
        let config = PullRequestProviderConfig {
            api_base_url: "http://127.0.0.1:8080".to_owned(),
            allow_loopback_http: true,
            ..PullRequestProviderConfig::default()
        };
        assert_eq!(config.validate(), Ok(()));
    }

    #[test]
    fn token_is_redacted_from_debug_and_serialization() {
        let config = PullRequestProviderConfig {
            auth_token: PrAuthToken::new("top-secret"),
            ..PullRequestProviderConfig::default()
        };
        let rendered = format!(
            "{config:?} {}",
            serde_json::to_string(&config).expect("JSON")
        );
        assert!(!rendered.contains("top-secret"));
    }

    #[tokio::test]
    async fn github_rejects_disabled_provider_without_transport() {
        let transport = MockTransport::default();
        let provider =
            GitHubProvider::new(PullRequestProviderConfig::default(), Arc::new(transport))
                .expect("provider");
        let error = provider
            .inspect(request(PullRequestProviderKind::GitHub))
            .await
            .expect_err("disabled");
        assert_eq!(error, PullRequestError::Disabled);
    }

    #[tokio::test]
    async fn github_requires_per_request_consent() {
        let transport = Arc::new(MockTransport::default());
        let provider = GitHubProvider::new(github_config(), transport.clone()).expect("provider");
        let mut input = request(PullRequestProviderKind::GitHub);
        input.consent_to_remote_access = false;
        let error = provider.inspect(input).await.expect_err("consent");
        assert_eq!(
            (error, transport.request_count()),
            (PullRequestError::ConsentRequired, 0)
        );
    }

    #[tokio::test]
    async fn github_parses_success_without_retaining_patch() {
        let transport = MockTransport::with(github_success_responses());
        let provider = GitHubProvider::new(github_config(), transport).expect("provider");
        let result = provider
            .inspect(request(PullRequestProviderKind::GitHub))
            .await
            .expect("inspection");
        let serialized = serde_json::to_string(&result).expect("JSON");
        assert_eq!(
            (
                result.changed_files[0].patch_truncated,
                result.ci.state,
                result.review.state,
                serialized.contains("secret source"),
            ),
            (true, CheckState::Success, ReviewState::Approved, false)
        );
    }

    #[tokio::test]
    async fn github_uses_fresh_structured_cache() {
        let transport = MockTransport::with(github_success_responses());
        let provider = GitHubProvider::new(github_config(), transport.clone()).expect("provider");
        provider
            .inspect(request(PullRequestProviderKind::GitHub))
            .await
            .expect("first");
        let second = provider
            .inspect(request(PullRequestProviderKind::GitHub))
            .await
            .expect("second");
        assert_eq!((second.from_cache, transport.request_count()), (true, 4));
    }

    #[tokio::test]
    async fn github_reports_rate_limit_without_sleeping() {
        let mut limited = response(429, &json!({"message":"slow down"}));
        limited
            .headers
            .insert("retry-after".to_owned(), "12".to_owned());
        let transport = MockTransport::with(vec![limited]);
        let provider = GitHubProvider::new(github_config(), transport).expect("provider");
        let error = provider
            .inspect(request(PullRequestProviderKind::GitHub))
            .await
            .expect_err("rate limit");
        assert!(matches!(
            error,
            PullRequestError::RateLimited {
                metadata: PullRequestRateLimit {
                    retry_after_seconds: Some(12),
                    ..
                },
                ..
            }
        ));
    }

    #[tokio::test]
    async fn github_redacts_token_from_api_error() {
        let transport = MockTransport::with(vec![response(
            401,
            &json!({"message":"token top-secret is invalid"}),
        )]);
        let mut config = github_config();
        config.auth_token = PrAuthToken::new("top-secret");
        let provider = GitHubProvider::new(config, transport).expect("provider");
        let error = provider
            .inspect(request(PullRequestProviderKind::GitHub))
            .await
            .expect_err("API error");
        assert!(!error.to_string().contains("top-secret"));
    }

    #[tokio::test]
    async fn github_rejects_malformed_metadata() {
        let transport = MockTransport::with(vec![response(200, &json!({"id": 1}))]);
        let provider = GitHubProvider::new(github_config(), transport).expect("provider");
        assert!(matches!(
            provider
                .inspect(request(PullRequestProviderKind::GitHub))
                .await,
            Err(PullRequestError::MalformedResponse(_))
        ));
    }

    #[tokio::test]
    async fn github_follows_link_header_pagination() {
        let mut first_page = response(
            200,
            &json!([{"filename":"a.rs","status":"added","additions":1,"deletions":0}]),
        );
        first_page.headers.insert(
            "link".to_owned(),
            r#"<https://api.github.com/repos/o/r/pulls/1/files?page=2>; rel="next""#.to_owned(),
        );
        let transport = MockTransport::with(vec![
            response(200, &github_metadata()),
            first_page,
            response(
                200,
                &json!([{"filename":"b.rs","status":"added","additions":1,"deletions":0}]),
            ),
            response(200, &json!({"check_runs":[]})),
            response(200, &json!([])),
        ]);
        let provider = GitHubProvider::new(github_config(), transport).expect("provider");
        let result = provider
            .inspect(request(PullRequestProviderKind::GitHub))
            .await
            .expect("inspection");
        assert_eq!(result.changed_files.len(), 2);
    }

    #[tokio::test]
    async fn github_revalidates_stale_cache_with_etag() {
        let mut responses = github_success_responses();
        responses[0]
            .headers
            .insert("etag".to_owned(), r#""version-1""#.to_owned());
        responses.push(PrHttpResponse {
            status: 304,
            headers: BTreeMap::new(),
            body: Vec::new(),
        });
        let transport = MockTransport::with(responses);
        let mut config = github_config();
        config.cache_ttl_seconds = 0;
        let provider = GitHubProvider::new(config, transport.clone()).expect("provider");
        provider
            .inspect(request(PullRequestProviderKind::GitHub))
            .await
            .expect("first");
        let second = provider
            .inspect(request(PullRequestProviderKind::GitHub))
            .await
            .expect("revalidated");
        assert_eq!(
            (
                second.from_cache,
                transport.request_count(),
                transport.request_header(4, "if-none-match"),
            ),
            (true, 5, Some(r#""version-1""#.to_owned()))
        );
    }

    #[tokio::test]
    async fn github_honors_cancellation_before_transport() {
        let transport = Arc::new(MockTransport::default());
        let provider = GitHubProvider::new(github_config(), transport.clone()).expect("provider");
        let input = request(PullRequestProviderKind::GitHub);
        input.cancellation.cancel();
        let error = provider.inspect(input).await.expect_err("cancelled");
        assert_eq!(
            (error, transport.request_count()),
            (PullRequestError::Cancelled, 0)
        );
    }

    #[tokio::test]
    async fn github_list_returns_source_free_summaries_in_provider_number_order() {
        let mut page = response(
            200,
            &json!([github_summary(9, "open"), github_summary(2, "open")]),
        );
        page.headers
            .insert("x-ratelimit-remaining".to_owned(), "42".to_owned());
        let transport = MockTransport::with(vec![page]);
        let provider = GitHubProvider::new(github_config(), transport.clone()).expect("provider");
        let result = provider
            .list(list_request(PullRequestProviderKind::GitHub))
            .await
            .expect("list page");
        let serialized = serde_json::to_string(&result).expect("JSON");
        assert_eq!(
            (
                result
                    .items
                    .iter()
                    .map(|summary| summary.coordinates.number)
                    .collect::<Vec<_>>(),
                result.rate_limit.remaining,
                serialized.contains("patch"),
                transport
                    .request_url(0)
                    .is_some_and(|url| url.contains("state=all") && url.contains("per_page=10")),
            ),
            (vec![2, 9], Some(42), false, true)
        );
    }

    #[tokio::test]
    async fn github_list_requires_consent_before_transport() {
        let transport = Arc::new(MockTransport::default());
        let provider = GitHubProvider::new(github_config(), transport.clone()).expect("provider");
        let mut input = list_request(PullRequestProviderKind::GitHub);
        input.consent_to_remote_access = false;
        let error = provider.list(input).await.expect_err("consent");
        assert_eq!(
            (error, transport.request_count()),
            (PullRequestError::ConsentRequired, 0)
        );
    }

    #[tokio::test]
    async fn github_list_rejects_disabled_provider_before_transport() {
        let transport = Arc::new(MockTransport::default());
        let provider = GitHubProvider::new(PullRequestProviderConfig::default(), transport.clone())
            .expect("provider");
        let error = provider
            .list(list_request(PullRequestProviderKind::GitHub))
            .await
            .expect_err("disabled");
        assert_eq!(
            (error, transport.request_count()),
            (PullRequestError::Disabled, 0)
        );
    }

    #[tokio::test]
    async fn github_list_rejects_zero_limit_before_transport() {
        let transport = Arc::new(MockTransport::default());
        let provider = GitHubProvider::new(github_config(), transport.clone()).expect("provider");
        let mut input = list_request(PullRequestProviderKind::GitHub);
        input.limit = 0;
        let error = provider.list(input).await.expect_err("invalid limit");
        assert!(matches!(
            (error, transport.request_count()),
            (PullRequestError::InvalidConfiguration(_), 0)
        ));
    }

    #[tokio::test]
    async fn github_list_rejects_limit_above_one_hundred_before_transport() {
        let transport = Arc::new(MockTransport::default());
        let provider = GitHubProvider::new(github_config(), transport.clone()).expect("provider");
        let mut input = list_request(PullRequestProviderKind::GitHub);
        input.limit = 101;
        let error = provider.list(input).await.expect_err("invalid limit");
        assert!(matches!(
            (error, transport.request_count()),
            (PullRequestError::InvalidConfiguration(_), 0)
        ));
    }

    #[tokio::test]
    async fn github_list_round_trips_same_origin_page_cursor() {
        let mut first = response(200, &json!([github_summary(1, "open")]));
        first.headers.insert(
            "link".to_owned(),
            r#"<https://api.github.com/repos/o/r/pulls?state=all&per_page=10&page=2>; rel="next""#
                .to_owned(),
        );
        let transport = MockTransport::with(vec![
            first,
            response(200, &json!([github_summary(2, "open")])),
        ]);
        let provider = GitHubProvider::new(github_config(), transport.clone()).expect("provider");
        let first_page = provider
            .list(list_request(PullRequestProviderKind::GitHub))
            .await
            .expect("first page");
        let mut second_request = list_request(PullRequestProviderKind::GitHub);
        second_request.cursor = first_page.next_cursor;
        let second_page = provider.list(second_request).await.expect("second page");
        assert_eq!(
            (
                first_page.has_more,
                second_page.items[0].coordinates.number,
                transport
                    .request_url(1)
                    .is_some_and(|url| url.contains("page=2")),
            ),
            (true, 2, true)
        );
    }

    #[tokio::test]
    async fn github_list_rejects_cross_origin_pagination() {
        let mut page = response(200, &json!([github_summary(1, "open")]));
        page.headers.insert(
            "link".to_owned(),
            r#"<https://attacker.invalid/repos/o/r/pulls?page=2>; rel="next""#.to_owned(),
        );
        let transport = MockTransport::with(vec![page]);
        let provider = GitHubProvider::new(github_config(), transport).expect("provider");
        assert!(matches!(
            provider
                .list(list_request(PullRequestProviderKind::GitHub))
                .await,
            Err(PullRequestError::MalformedResponse(_))
        ));
    }

    #[tokio::test]
    async fn github_list_reports_missing_scope_as_api_error() {
        let transport = MockTransport::with(vec![response(
            403,
            &json!({"message":"resource not accessible by token"}),
        )]);
        let provider = GitHubProvider::new(github_config(), transport).expect("provider");
        assert!(matches!(
            provider
                .list(list_request(PullRequestProviderKind::GitHub))
                .await,
            Err(PullRequestError::Api { status: 403, .. })
        ));
    }

    #[tokio::test]
    async fn github_list_redacts_token_from_api_error() {
        let transport = MockTransport::with(vec![response(
            401,
            &json!({"message":"token list-secret is invalid"}),
        )]);
        let mut config = github_config();
        config.auth_token = PrAuthToken::new("list-secret");
        let provider = GitHubProvider::new(config, transport).expect("provider");
        let error = provider
            .list(list_request(PullRequestProviderKind::GitHub))
            .await
            .expect_err("API error");
        assert!(!error.to_string().contains("list-secret"));
    }

    #[tokio::test]
    async fn github_list_marks_configured_page_reduction_as_truncated() {
        let mut page = response(200, &json!([github_summary(1, "open")]));
        page.headers.insert(
            "link".to_owned(),
            r#"<https://api.github.com/repos/o/r/pulls?state=all&per_page=1&page=2>; rel="next""#
                .to_owned(),
        );
        let transport = MockTransport::with(vec![page]);
        let mut config = github_config();
        config.max_items = 1;
        let provider = GitHubProvider::new(config, transport).expect("provider");
        let result = provider
            .list(list_request(PullRequestProviderKind::GitHub))
            .await
            .expect("bounded page");
        assert_eq!(
            (
                result.truncated,
                result.warnings.first().map(|warning| warning.code.as_str()),
            ),
            (true, Some("configured_limit_applied"))
        );
    }

    #[tokio::test]
    async fn list_reports_bitbucket_data_center_as_unsupported_without_transport() {
        let transport = Arc::new(MockTransport::default());
        let provider = GitHubProvider::new(github_config(), transport.clone()).expect("provider");
        let error = provider
            .list(list_request(PullRequestProviderKind::BitbucketDataCenter))
            .await
            .expect_err("unsupported provider");
        assert_eq!(
            (error, transport.request_count()),
            (PullRequestError::UnsupportedProvider, 0)
        );
    }

    #[tokio::test]
    async fn bitbucket_parses_success_and_participant_approval() {
        let transport = MockTransport::with(vec![
            response(200, &bitbucket_metadata()),
            response(
                200,
                &json!({"values":[{"status":"modified","old":{"path":"a"},"new":{"path":"a"},"lines_added":3,"lines_removed":1}]}),
            ),
            response(
                200,
                &json!({"values":[{"key":"build","state":"SUCCESSFUL"}]}),
            ),
        ]);
        let provider = BitbucketProvider::new(bitbucket_config(), transport).expect("provider");
        let result = provider
            .inspect(request(PullRequestProviderKind::BitbucketCloud))
            .await
            .expect("inspection");
        assert_eq!(
            (result.review.approvals, result.ci.state),
            (1, CheckState::Success)
        );
    }

    #[tokio::test]
    async fn bitbucket_api_token_should_use_basic_auth_with_atlassian_email() {
        let transport = MockTransport::with(vec![
            response(200, &bitbucket_metadata()),
            response(200, &json!({"values":[]})),
            response(200, &json!({"values":[]})),
        ]);
        let mut config = bitbucket_config();
        config.auth_token = PrAuthToken::new("api-token");
        config.basic_auth_username = Some("user@example.com".to_owned());
        let provider =
            BitbucketProvider::new(config, transport.clone()).expect("Bitbucket provider");
        provider
            .inspect(request(PullRequestProviderKind::BitbucketCloud))
            .await
            .expect("inspection");
        assert!(matches!(
            transport.request_authentication(0),
            Some(PrHttpAuthentication::Basic { username, token })
                if username == "user@example.com" && token.expose_secret() == "api-token"
        ));
    }

    #[tokio::test]
    async fn bitbucket_should_follow_same_origin_diffstat_redirect_and_degrade_missing_scopes() {
        let transport = MockTransport::with(vec![
            response(200, &bitbucket_metadata()),
            PrHttpResponse {
                status: 302,
                headers: BTreeMap::from([(
                    "location".to_owned(),
                    "https://api.bitbucket.org/2.0/repositories/o/r/diffstat/base..head".to_owned(),
                )]),
                body: Vec::new(),
            },
            response(
                403,
                &json!({"error":{"message":"missing repository scope"}}),
            ),
            response(
                403,
                &json!({"error":{"message":"missing repository scope"}}),
            ),
        ]);
        let provider =
            BitbucketProvider::new(bitbucket_config(), transport.clone()).expect("provider");
        let result = provider
            .inspect(request(PullRequestProviderKind::BitbucketCloud))
            .await
            .expect("degraded inspection");
        assert_eq!(
            (
                result.changed_files.len(),
                result.ci.state,
                result.warnings.len(),
                transport.request_count(),
            ),
            (0, CheckState::Unknown, 2, 4)
        );
        assert!(
            result
                .warnings
                .iter()
                .all(|warning| warning.code == "provider_scope_missing")
        );
    }

    #[tokio::test]
    async fn bitbucket_should_reject_cross_origin_redirect_without_forwarding_credentials() {
        let transport = MockTransport::with(vec![
            response(200, &bitbucket_metadata()),
            PrHttpResponse {
                status: 302,
                headers: BTreeMap::from([(
                    "location".to_owned(),
                    "https://attacker.invalid/diffstat".to_owned(),
                )]),
                body: Vec::new(),
            },
        ]);
        let provider =
            BitbucketProvider::new(bitbucket_config(), transport.clone()).expect("provider");
        let error = provider
            .inspect(request(PullRequestProviderKind::BitbucketCloud))
            .await
            .expect_err("cross-origin redirect");
        assert!(matches!(
            error,
            PullRequestError::Api { message, .. } if message.contains("crossed")
        ));
        assert_eq!(transport.request_count(), 2);
    }

    #[tokio::test]
    async fn bitbucket_follows_diffstat_pagination() {
        let transport = MockTransport::with(vec![
            response(200, &bitbucket_metadata()),
            response(
                200,
                &json!({"values":[], "next":"https://api.bitbucket.org/2.0/next"}),
            ),
            response(
                200,
                &json!({"values":[{"status":"added","new":{"path":"a"},"lines_added":1,"lines_removed":0}]}),
            ),
            response(200, &json!({"values":[]})),
        ]);
        let provider =
            BitbucketProvider::new(bitbucket_config(), transport.clone()).expect("provider");
        let result = provider
            .inspect(request(PullRequestProviderKind::BitbucketCloud))
            .await
            .expect("inspection");
        assert_eq!(
            (result.changed_files.len(), transport.request_count()),
            (1, 4)
        );
    }

    #[tokio::test]
    async fn bitbucket_uses_fresh_structured_cache() {
        let transport = MockTransport::with(vec![
            response(200, &bitbucket_metadata()),
            response(200, &json!({"values":[]})),
            response(200, &json!({"values":[]})),
        ]);
        let provider =
            BitbucketProvider::new(bitbucket_config(), transport.clone()).expect("provider");
        provider
            .inspect(request(PullRequestProviderKind::BitbucketCloud))
            .await
            .expect("first");
        let second = provider
            .inspect(request(PullRequestProviderKind::BitbucketCloud))
            .await
            .expect("second");
        assert_eq!((second.from_cache, transport.request_count()), (true, 3));
    }

    #[tokio::test]
    async fn bitbucket_reports_rate_limit_metadata() {
        let mut limited = response(403, &json!({"error":{"message":"quota"}}));
        limited
            .headers
            .insert("x-ratelimit-remaining".to_owned(), "0".to_owned());
        let transport = MockTransport::with(vec![limited]);
        let provider = BitbucketProvider::new(bitbucket_config(), transport).expect("provider");
        let error = provider
            .inspect(request(PullRequestProviderKind::BitbucketCloud))
            .await
            .expect_err("rate limit");
        assert!(matches!(
            error,
            PullRequestError::RateLimited {
                metadata: PullRequestRateLimit {
                    remaining: Some(0),
                    ..
                },
                ..
            }
        ));
    }

    #[tokio::test]
    async fn bitbucket_requires_per_request_consent() {
        let transport = Arc::new(MockTransport::default());
        let provider =
            BitbucketProvider::new(bitbucket_config(), transport.clone()).expect("provider");
        let mut input = request(PullRequestProviderKind::BitbucketCloud);
        input.consent_to_remote_access = false;
        let error = provider.inspect(input).await.expect_err("consent");
        assert_eq!(
            (error, transport.request_count()),
            (PullRequestError::ConsentRequired, 0)
        );
    }

    #[tokio::test]
    async fn bitbucket_redacts_token_from_api_error() {
        let transport = MockTransport::with(vec![response(
            400,
            &json!({"error":{"message":"credential bb-secret rejected"}}),
        )]);
        let mut config = bitbucket_config();
        config.auth_token = PrAuthToken::new("bb-secret");
        let provider = BitbucketProvider::new(config, transport).expect("provider");
        let error = provider
            .inspect(request(PullRequestProviderKind::BitbucketCloud))
            .await
            .expect_err("API error");
        assert!(!error.to_string().contains("bb-secret"));
    }

    #[tokio::test]
    async fn bitbucket_rejects_cross_origin_pagination() {
        let transport = MockTransport::with(vec![
            response(200, &bitbucket_metadata()),
            response(
                200,
                &json!({"values":[], "next":"https://evil.example/next"}),
            ),
        ]);
        let provider = BitbucketProvider::new(bitbucket_config(), transport).expect("provider");
        assert!(matches!(
            provider
                .inspect(request(PullRequestProviderKind::BitbucketCloud))
                .await,
            Err(PullRequestError::MalformedResponse(_))
        ));
    }

    #[tokio::test]
    async fn bitbucket_list_returns_source_free_summaries_in_provider_number_order() {
        let transport = MockTransport::with(vec![response(
            200,
            &json!({
                "values": [
                    bitbucket_summary(8, "OPEN"),
                    bitbucket_summary(3, "DECLINED")
                ]
            }),
        )]);
        let provider =
            BitbucketProvider::new(bitbucket_config(), transport.clone()).expect("provider");
        let mut input = list_request(PullRequestProviderKind::BitbucketCloud);
        input.state = PullRequestListState::Open;
        let result = provider.list(input).await.expect("list page");
        assert_eq!(
            (
                result
                    .items
                    .iter()
                    .map(|summary| summary.coordinates.number)
                    .collect::<Vec<_>>(),
                transport.request_url(0).is_some_and(|url| {
                    url.contains("pagelen=10") && url.contains("state=OPEN")
                }),
            ),
            (vec![3, 8], true)
        );
    }

    #[tokio::test]
    async fn bitbucket_list_requires_consent_before_transport() {
        let transport = Arc::new(MockTransport::default());
        let provider =
            BitbucketProvider::new(bitbucket_config(), transport.clone()).expect("provider");
        let mut input = list_request(PullRequestProviderKind::BitbucketCloud);
        input.consent_to_remote_access = false;
        let error = provider.list(input).await.expect_err("consent");
        assert_eq!(
            (error, transport.request_count()),
            (PullRequestError::ConsentRequired, 0)
        );
    }

    #[tokio::test]
    async fn bitbucket_list_round_trips_same_origin_page_cursor() {
        let transport = MockTransport::with(vec![
            response(
                200,
                &json!({
                    "values": [bitbucket_summary(1, "OPEN")],
                    "next": "https://api.bitbucket.org/2.0/repositories/o/r/pullrequests?pagelen=10&page=2"
                }),
            ),
            response(200, &json!({"values": [bitbucket_summary(2, "OPEN")]})),
        ]);
        let provider =
            BitbucketProvider::new(bitbucket_config(), transport.clone()).expect("provider");
        let first_page = provider
            .list(list_request(PullRequestProviderKind::BitbucketCloud))
            .await
            .expect("first page");
        let mut second_request = list_request(PullRequestProviderKind::BitbucketCloud);
        second_request.cursor = first_page.next_cursor;
        let second_page = provider.list(second_request).await.expect("second page");
        assert_eq!(
            (
                first_page.has_more,
                second_page.items[0].coordinates.number,
                transport
                    .request_url(1)
                    .is_some_and(|url| url.contains("page=2")),
            ),
            (true, 2, true)
        );
    }

    #[tokio::test]
    async fn bitbucket_list_rejects_cross_origin_pagination() {
        let transport = MockTransport::with(vec![response(
            200,
            &json!({
                "values": [bitbucket_summary(1, "OPEN")],
                "next": "https://attacker.invalid/repositories/o/r/pullrequests?page=2"
            }),
        )]);
        let provider = BitbucketProvider::new(bitbucket_config(), transport).expect("provider");
        assert!(matches!(
            provider
                .list(list_request(PullRequestProviderKind::BitbucketCloud))
                .await,
            Err(PullRequestError::MalformedResponse(_))
        ));
    }

    #[tokio::test]
    async fn bitbucket_list_reports_missing_scope_as_api_error() {
        let transport = MockTransport::with(vec![response(
            403,
            &json!({"error":{"message":"missing pullrequest scope"}}),
        )]);
        let provider = BitbucketProvider::new(bitbucket_config(), transport).expect("provider");
        assert!(matches!(
            provider
                .list(list_request(PullRequestProviderKind::BitbucketCloud))
                .await,
            Err(PullRequestError::Api { status: 403, .. })
        ));
    }

    #[tokio::test]
    async fn bitbucket_list_redacts_token_from_api_error() {
        let transport = MockTransport::with(vec![response(
            401,
            &json!({"error":{"message":"credential bb-list-secret rejected"}}),
        )]);
        let mut config = bitbucket_config();
        config.auth_token = PrAuthToken::new("bb-list-secret");
        let provider = BitbucketProvider::new(config, transport).expect("provider");
        let error = provider
            .list(list_request(PullRequestProviderKind::BitbucketCloud))
            .await
            .expect_err("API error");
        assert!(!error.to_string().contains("bb-list-secret"));
    }

    #[test]
    fn overlap_is_disjoint_for_unrelated_inputs() {
        assert_eq!(
            semantic_pull_request_overlap(&semantic("a"), &semantic("b")).kind,
            PullRequestOverlapKind::Disjoint
        );
    }

    #[test]
    fn overlap_classifies_shared_file_deterministically() {
        let mut left = semantic("a");
        let mut right = semantic("b");
        left.files.extend(["z.rs".to_owned(), "a.rs".to_owned()]);
        right.files.extend(["a.rs".to_owned(), "z.rs".to_owned()]);
        assert_eq!(
            semantic_pull_request_overlap(&left, &right).files,
            vec!["a.rs".to_owned(), "z.rs".to_owned()]
        );
    }

    #[test]
    fn overlap_classifies_shared_service() {
        let mut left = semantic("a");
        let mut right = semantic("b");
        left.services.insert("billing".to_owned());
        right.services.insert("billing".to_owned());
        assert_eq!(
            semantic_pull_request_overlap(&left, &right).kind,
            PullRequestOverlapKind::Service
        );
    }

    #[test]
    fn overlap_classifies_shared_community() {
        let mut left = semantic("a");
        let mut right = semantic("b");
        left.communities.insert("community-1".to_owned());
        right.communities.insert("community-1".to_owned());
        assert_eq!(
            semantic_pull_request_overlap(&left, &right).kind,
            PullRequestOverlapKind::Community
        );
    }

    #[test]
    fn overlap_marks_shared_migration_conflicting() {
        let mut left = semantic("a");
        let mut right = semantic("b");
        left.files.insert("migrations/001.sql".to_owned());
        right.files.insert("migrations/001.sql".to_owned());
        assert_eq!(
            semantic_pull_request_overlap(&left, &right).kind,
            PullRequestOverlapKind::Conflicting
        );
    }

    #[test]
    fn order_follows_dependency_direction() {
        let left = semantic("a");
        let mut right = semantic("b");
        right.depends_on.insert("a".to_owned());
        assert_eq!(
            suggest_pull_request_order(&left, &right).order,
            PullRequestOrder::LeftFirst
        );
    }

    #[test]
    fn order_coordinates_dependency_cycle() {
        let mut left = semantic("a");
        let mut right = semantic("b");
        left.depends_on.insert("b".to_owned());
        right.depends_on.insert("a".to_owned());
        assert_eq!(
            suggest_pull_request_order(&left, &right).order,
            PullRequestOrder::Coordinated
        );
    }

    #[test]
    fn order_is_indefinite_for_stale_readiness() {
        let mut left = semantic("a");
        left.readiness = PullRequestReadiness::Stale;
        assert_eq!(
            suggest_pull_request_order(&left, &semantic("b")).order,
            PullRequestOrder::NoDefinitiveOrder
        );
    }

    #[test]
    fn order_prefers_ready_change_when_other_is_blocked() {
        let left = semantic("a");
        let mut right = semantic("b");
        right.readiness = PullRequestReadiness::Blocked;
        assert_eq!(
            suggest_pull_request_order(&left, &right).order,
            PullRequestOrder::LeftFirst
        );
    }
}

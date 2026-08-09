use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use code_system_graph_model::RepoId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

/// Availability state reported by a local intelligence provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStatus {
    /// Provider and repository index are ready.
    Available,
    /// Provider is installed but the repository has no existing index.
    IndexMissing,
    /// Existing index does not match the repository state.
    Stale,
    /// Provider is unavailable.
    Unavailable,
    /// Public capabilities are incompatible with the adapter.
    Incompatible,
    /// Provider returned an invalid public protocol response.
    InvalidResponse,
    /// Provider probing exceeded its configured deadline.
    TimedOut,
}

/// Public transport used for one local-intelligence operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderTransport {
    /// Model Context Protocol over child-process standard I/O.
    Mcp,
    /// Direct public command-line invocation without shell interpolation.
    Cli,
}

/// Repository-local operation understood by the provider boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderOperation {
    /// Resolve a textual anchor to exact local symbols.
    ResolveSymbols,
    /// Load bounded callers or callees for one symbol.
    LocalNeighbors,
    /// Compute bounded local impact.
    LocalImpact,
    /// Build ephemeral local source and flow context.
    LocalContext,
    /// Recommend tests affected by changed files.
    AffectedTests,
}

/// One discovered operation and the public interface that provides it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderOperationCapability {
    /// `Code System Graph` operation.
    pub operation: ProviderOperation,
    /// Preferred transport for this operation.
    pub transport: ProviderTransport,
    /// Public MCP tool or CLI command name.
    pub public_name: String,
    /// Supported public input parameter names.
    pub parameters: Vec<String>,
}

/// Non-fatal provider limitation observed while probing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderDegradation {
    /// Stable machine-readable degradation kind.
    pub kind: String,
    /// Bounded explanation that never includes returned source.
    pub message: String,
    /// Process-local diagnostic identifier when a failed request produced one.
    pub diagnostics_id: Option<String>,
}

/// Capability report discovered from a provider's public interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCapability {
    /// Provider name.
    pub provider: String,
    /// Reported provider version when available.
    pub version: Option<String>,
    /// Availability state for the requested repository.
    pub status: ProviderStatus,
    /// Public tool names discovered from the provider.
    pub tools: Vec<String>,
    /// MCP protocol version negotiated during initialization.
    pub protocol_version: Option<String>,
    /// Operations available through compatible public contracts.
    pub operations: Vec<ProviderOperationCapability>,
    /// Non-fatal limitations and fallbacks observed during probing.
    pub degradations: Vec<ProviderDegradation>,
    /// Bounded remediation guidance.
    pub remediation: Option<String>,
}

/// Bounds applied independently to one provider request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderBudget {
    /// End-to-end request timeout.
    pub timeout: Duration,
    /// Maximum provider output retained in memory.
    pub max_output_bytes: usize,
    /// Maximum typed items returned to the caller.
    pub max_items: usize,
}

impl Default for ProviderBudget {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            max_output_bytes: 1024 * 1024,
            max_items: 50,
        }
    }
}

/// Repository scope and controls shared by all provider requests.
#[derive(Debug, Clone)]
pub struct ProviderRequest {
    /// Stable repository identity.
    pub repo_id: RepoId,
    /// Native absolute checkout path passed directly to the child process.
    pub project_path: PathBuf,
    /// Explicit execution limits.
    pub budget: ProviderBudget,
    /// Cooperative cancellation signal.
    pub cancellation: CancellationToken,
}

/// Request to resolve one textual symbol anchor.
#[derive(Debug, Clone)]
pub struct ResolveSymbolsRequest {
    /// Shared repository scope and controls.
    pub request: ProviderRequest,
    /// Symbol name, qualified name, or exact local anchor.
    pub query: String,
}

/// Direction used for local neighbor traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalNeighborDirection {
    /// Functions or methods that call the anchor.
    Callers,
    /// Functions, methods, or declarations called by the anchor.
    Callees,
}

/// Request for bounded local callers or callees.
#[derive(Debug, Clone)]
pub struct LocalNeighborsRequest {
    /// Shared repository scope and controls.
    pub request: ProviderRequest,
    /// Exact or qualified symbol anchor.
    pub symbol: String,
    /// Requested traversal direction.
    pub direction: LocalNeighborDirection,
}

/// Request for bounded local impact.
#[derive(Debug, Clone)]
pub struct LocalImpactRequest {
    /// Shared repository scope and controls.
    pub request: ProviderRequest,
    /// Exact or qualified symbol anchor.
    pub symbol: String,
    /// Maximum local dependency depth.
    pub max_depth: usize,
}

/// Request for ephemeral source and local-flow context.
#[derive(Debug, Clone)]
pub struct LocalContextRequest {
    /// Shared repository scope and controls.
    pub request: ProviderRequest,
    /// Focused context query.
    pub query: String,
    /// Maximum source files requested from the provider.
    pub max_files: usize,
}

/// Request for tests affected by changed local files.
#[derive(Debug, Clone)]
pub struct AffectedTestsRequest {
    /// Shared repository scope and controls.
    pub request: ProviderRequest,
    /// Repository-relative changed file paths.
    pub changed_files: Vec<String>,
    /// Maximum dependency traversal depth.
    pub max_depth: usize,
}

/// Metadata describing one bounded provider execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderExecution {
    /// Public transport that produced the result.
    pub transport: ProviderTransport,
    /// Bytes retained from provider output.
    pub output_bytes: usize,
    /// Whether configured item or byte limits truncated the result.
    pub truncated: bool,
    /// Non-fatal failures that caused fallback or partial execution.
    pub degradations: Vec<ProviderDegradation>,
}

/// Exact local symbol returned by a compatible provider contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedSymbol {
    /// Local provider symbol identifier; never treated as a global identity.
    pub local_id: Option<String>,
    /// Unqualified symbol name.
    pub name: String,
    /// Provider-qualified symbol name when available.
    pub qualified_name: Option<String>,
    /// Provider node kind.
    pub kind: String,
    /// Repository-relative source path.
    pub file_path: String,
    /// One-based source start line.
    pub start_line: usize,
    /// Provider ranking score when available.
    pub score: Option<f64>,
}

/// Result of one symbol-resolution request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolveSymbolsResult {
    /// Bounded matching symbols.
    pub symbols: Vec<ResolvedSymbol>,
    /// Execution metadata.
    pub execution: ProviderExecution,
}

/// One bounded caller or callee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalNeighbor {
    /// Local symbol name.
    pub name: String,
    /// Provider node kind.
    pub kind: String,
    /// Repository-relative source path.
    pub file_path: String,
    /// One-based source start line.
    pub start_line: usize,
}

/// Result of one local-neighbor request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalNeighborResult {
    /// Requested anchor.
    pub symbol: String,
    /// Requested direction.
    pub direction: LocalNeighborDirection,
    /// Bounded local neighbors.
    pub neighbors: Vec<LocalNeighbor>,
    /// Execution metadata.
    pub execution: ProviderExecution,
}

/// Result of one bounded local-impact request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalImpactResult {
    /// Requested anchor.
    pub symbol: String,
    /// Effective traversal depth.
    pub depth: usize,
    /// Total local nodes reported before `Code System Graph` item truncation.
    pub provider_node_count: usize,
    /// Bounded affected local symbols.
    pub affected: Vec<LocalNeighbor>,
    /// Execution metadata.
    pub execution: ProviderExecution,
}

/// Ephemeral local source and flow context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LocalContextResult {
    /// Opaque provider text; callers must not persist this field.
    pub content: String,
    /// Execution metadata.
    pub execution: ProviderExecution,
}

/// Result of one affected-tests request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AffectedTestsResult {
    /// Changed files accepted by the provider.
    pub changed_files: Vec<String>,
    /// Bounded repository-relative test paths.
    pub affected_tests: Vec<String>,
    /// Total local dependents traversed by the provider.
    pub total_dependents_traversed: usize,
    /// Execution metadata.
    pub execution: ProviderExecution,
}

/// Error returned by a local intelligence provider adapter.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProviderError {
    /// Operation was cancelled.
    #[error("local intelligence request was cancelled")]
    Cancelled,
    /// Operation exceeded its configured deadline.
    #[error("local intelligence provider timed out (diagnostics: {diagnostics_id})")]
    Timeout {
        /// Process-local diagnostic identifier.
        diagnostics_id: String,
    },
    /// Operation exceeded its configured output limit.
    #[error(
        "local intelligence provider exceeded the {limit_bytes}-byte output limit (diagnostics: {diagnostics_id})"
    )]
    OutputLimit {
        /// Configured output limit.
        limit_bytes: usize,
        /// Process-local diagnostic identifier.
        diagnostics_id: String,
    },
    /// A temporary circuit breaker is open after a provider timeout.
    #[error("local intelligence provider circuit is temporarily open")]
    CircuitOpen,
    /// Request controls or repository scope were invalid.
    #[error("invalid local intelligence request: {0}")]
    InvalidRequest(String),
    /// Provider returned an invalid public response.
    #[error("invalid local intelligence response: {message} (diagnostics: {diagnostics_id})")]
    InvalidResponse {
        /// Bounded response validation failure.
        message: String,
        /// Process-local diagnostic identifier.
        diagnostics_id: String,
    },
    /// Provider process or transport failed.
    #[error("local intelligence transport failed: {message} (diagnostics: {diagnostics_id})")]
    Transport {
        /// Bounded transport failure.
        message: String,
        /// Process-local diagnostic identifier.
        diagnostics_id: String,
    },
}

/// Optional boundary for repository-local code intelligence.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait LocalCodeIntelligenceProvider: Send + Sync {
    /// Returns the stable provider name.
    fn provider_name(&self) -> &'static str;

    /// Discovers public capabilities for one repository without creating or modifying indexes.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] for cancellation, timeout, invalid output, or transport failure.
    async fn probe(&self, request: ProviderRequest) -> Result<ProviderCapability, ProviderError>;

    /// Resolves a textual anchor to bounded exact local symbols.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] when no compatible public contract can complete the request.
    async fn resolve_symbols(
        &self,
        input: ResolveSymbolsRequest,
    ) -> Result<ResolveSymbolsResult, ProviderError>;

    /// Returns bounded callers or callees for one local symbol.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] when no compatible public contract can complete the request.
    async fn get_local_neighbors(
        &self,
        input: LocalNeighborsRequest,
    ) -> Result<LocalNeighborResult, ProviderError>;

    /// Returns bounded local impact for one symbol.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] when no compatible public contract can complete the request.
    async fn get_local_impact(
        &self,
        input: LocalImpactRequest,
    ) -> Result<LocalImpactResult, ProviderError>;

    /// Builds ephemeral local source and flow context.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] when no compatible public contract can complete the request.
    async fn build_local_context(
        &self,
        input: LocalContextRequest,
    ) -> Result<LocalContextResult, ProviderError>;

    /// Returns bounded tests affected by changed files when supported.
    ///
    /// `Ok(None)` means the optional capability is unavailable.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] when a declared compatible contract fails.
    async fn get_affected_tests(
        &self,
        input: AffectedTestsRequest,
    ) -> Result<Option<AffectedTestsResult>, ProviderError>;

    /// Releases provider resources and terminates owned child processes.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] if orderly shutdown fails.
    async fn shutdown(&self) -> Result<(), ProviderError>;
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use async_trait::async_trait;
    use code_system_graph_model::RepoId;
    use tokio_util::sync::CancellationToken;

    use super::{
        AffectedTestsRequest, AffectedTestsResult, LocalCodeIntelligenceProvider, LocalContextRequest, LocalContextResult, LocalImpactRequest, LocalImpactResult, LocalNeighborResult, LocalNeighborsRequest, ProviderBudget, ProviderCapability, ProviderError, ProviderRequest, ProviderStatus, ResolveSymbolsRequest, ResolveSymbolsResult
    };

    struct FakeProvider;

    #[async_trait]
    impl LocalCodeIntelligenceProvider for FakeProvider {
        fn provider_name(&self) -> &'static str {
            "fake"
        }

        async fn probe(
            &self,
            request: ProviderRequest,
        ) -> Result<ProviderCapability, ProviderError> {
            if request.cancellation.is_cancelled() {
                return Err(ProviderError::Cancelled);
            }
            Ok(ProviderCapability {
                provider: self.provider_name().to_owned(),
                version: None,
                status: ProviderStatus::IndexMissing,
                tools: Vec::new(),
                protocol_version: None,
                operations: Vec::new(),
                degradations: Vec::new(),
                remediation: Some("Create the index explicitly with CodeGraph.".to_owned()),
            })
        }

        async fn resolve_symbols(
            &self,
            _input: ResolveSymbolsRequest,
        ) -> Result<ResolveSymbolsResult, ProviderError> {
            Err(ProviderError::CircuitOpen)
        }

        async fn get_local_neighbors(
            &self,
            _input: LocalNeighborsRequest,
        ) -> Result<LocalNeighborResult, ProviderError> {
            Err(ProviderError::CircuitOpen)
        }

        async fn get_local_impact(
            &self,
            _input: LocalImpactRequest,
        ) -> Result<LocalImpactResult, ProviderError> {
            Err(ProviderError::CircuitOpen)
        }

        async fn build_local_context(
            &self,
            _input: LocalContextRequest,
        ) -> Result<LocalContextResult, ProviderError> {
            Err(ProviderError::CircuitOpen)
        }

        async fn get_affected_tests(
            &self,
            _input: AffectedTestsRequest,
        ) -> Result<Option<AffectedTestsResult>, ProviderError> {
            Ok(None)
        }

        async fn shutdown(&self) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn probe_should_preserve_missing_index_degradation() {
        let result = FakeProvider
            .probe(ProviderRequest {
                repo_id: RepoId::new("repo:web"),
                project_path: PathBuf::from("/workspace/web"),
                budget: ProviderBudget::default(),
                cancellation: CancellationToken::new(),
            })
            .await;
        let status = result.map(|capability| capability.status);

        assert_eq!(status, Ok(ProviderStatus::IndexMissing));
    }
}

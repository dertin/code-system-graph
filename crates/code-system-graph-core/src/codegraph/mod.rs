mod cli;
mod contract;
mod mcp;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Component, Path};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use code_system_graph_model::RepoId;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

use self::cli::CodeGraphCli;
use self::contract::{cli_operations, map_mcp_tools, supports_cli_contract};
use self::mcp::{CodeGraphMcp, McpProbe};
use crate::{
    AffectedTestsRequest, AffectedTestsResult, LocalCodeIntelligenceProvider, LocalContextRequest, LocalContextResult, LocalImpactRequest, LocalImpactResult, LocalNeighborResult, LocalNeighborsRequest, ProviderCapability, ProviderDegradation, ProviderError, ProviderRequest, ProviderStatus, ResolveSymbolsRequest, ResolveSymbolsResult
};

static DIAGNOSTIC_SEQUENCE: AtomicU64 = AtomicU64::new(1);
const MAX_QUERY_BYTES: usize = 4096;
const MAX_LOCAL_DEPTH: usize = 64;
const MAX_CHANGED_FILES: usize = 1024;

/// Process and circuit-breaker settings for the production `CodeGraph` adapter.
#[derive(Debug, Clone)]
pub struct CodeGraphConfig {
    /// Executable path or `PATH`-resolved binary name.
    pub binary: OsString,
    /// Maximum concurrent `CodeGraph` child processes.
    pub max_concurrent_processes: usize,
    /// Time that MCP is bypassed for one repository after an MCP timeout.
    pub circuit_breaker_cooldown: Duration,
}

impl Default for CodeGraphConfig {
    fn default() -> Self {
        Self {
            binary: OsString::from("codegraph"),
            max_concurrent_processes: 2,
            circuit_breaker_cooldown: Duration::from_secs(30),
        }
    }
}

/// Production local-intelligence provider using public `CodeGraph` MCP and CLI interfaces.
pub struct CodeGraphProvider {
    cli: CodeGraphCli,
    mcp: CodeGraphMcp,
    permits: Arc<Semaphore>,
    circuit_breaker_cooldown: Duration,
    mcp_circuits: Mutex<BTreeMap<RepoId, tokio::time::Instant>>,
    cli_version: Mutex<Option<String>>,
    closed: AtomicBool,
    active_processes: Arc<AtomicUsize>,
    maximum_concurrency_observed: Arc<AtomicUsize>,
}

struct ProviderPermit {
    _permit: OwnedSemaphorePermit,
    active_processes: Arc<AtomicUsize>,
}

impl Drop for ProviderPermit {
    fn drop(&mut self) {
        self.active_processes.fetch_sub(1, Ordering::AcqRel);
    }
}

struct CliProbe {
    version: String,
    status: ProviderStatus,
    degradations: Vec<ProviderDegradation>,
}

enum CliProbeOutcome {
    Ready(CliProbe),
    Degraded(ProviderCapability),
}

impl CodeGraphProvider {
    /// Builds a production adapter without starting or modifying any `CodeGraph` index.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::InvalidRequest`] when child-process concurrency is zero.
    pub fn new(config: CodeGraphConfig) -> Result<Self, ProviderError> {
        if config.max_concurrent_processes == 0 {
            return Err(ProviderError::InvalidRequest(
                "max_concurrent_processes must be greater than zero".to_owned(),
            ));
        }
        Ok(Self {
            cli: CodeGraphCli::new(config.binary.clone()),
            mcp: CodeGraphMcp::new(config.binary),
            permits: Arc::new(Semaphore::new(config.max_concurrent_processes)),
            circuit_breaker_cooldown: config.circuit_breaker_cooldown,
            mcp_circuits: Mutex::new(BTreeMap::new()),
            cli_version: Mutex::new(None),
            closed: AtomicBool::new(false),
            active_processes: Arc::new(AtomicUsize::new(0)),
            maximum_concurrency_observed: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Returns the maximum number of provider operations that simultaneously held process permits.
    #[must_use]
    pub fn maximum_concurrency_observed(&self) -> usize {
        self.maximum_concurrency_observed.load(Ordering::Acquire)
    }

    /// Reads the structured local-index status without starting MCP or modifying the index.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] when the request is invalid, the compatible CLI cannot be
    /// executed, or its bounded JSON status does not satisfy the validated contract.
    pub async fn index_status(
        &self,
        mut request: ProviderRequest,
    ) -> Result<ProviderStatus, ProviderError> {
        let deadline = tokio::time::Instant::now() + request.budget.timeout;
        let _permit = self.enter(&request, deadline).await?;
        self.compatible_cli_version(&mut request, deadline).await?;
        update_remaining_timeout(&mut request, deadline)?;
        self.cli
            .status(&request)
            .await
            .map(|status| status.status())
    }

    async fn enter(
        &self,
        request: &ProviderRequest,
        deadline: tokio::time::Instant,
    ) -> Result<ProviderPermit, ProviderError> {
        validate_request(request)?;
        if self.closed.load(Ordering::Acquire) {
            return Err(ProviderError::InvalidRequest(
                "provider has already been shut down".to_owned(),
            ));
        }
        tokio::select! {
            biased;
            () = request.cancellation.cancelled() => Err(ProviderError::Cancelled),
            () = tokio::time::sleep_until(deadline) => Err(timeout_error()),
            permit = self.permits.clone().acquire_owned() => {
                let permit = permit.map_err(|_| {
                    ProviderError::InvalidRequest("provider has already been shut down".to_owned())
                })?;
                let active = self.active_processes.fetch_add(1, Ordering::AcqRel) + 1;
                self.maximum_concurrency_observed.fetch_max(active, Ordering::AcqRel);
                Ok(ProviderPermit {
                    _permit: permit,
                    active_processes: self.active_processes.clone(),
                })
            },
        }
    }

    async fn compatible_cli_version(
        &self,
        request: &mut ProviderRequest,
        deadline: tokio::time::Instant,
    ) -> Result<String, ProviderError> {
        if let Some(version) = self.cli_version.lock().await.clone() {
            if supports_cli_contract(&version) {
                return Ok(version);
            }
            return Err(invalid_response(format!(
                "CodeGraph CLI {version} has no validated structured-output adapter"
            )));
        }
        update_remaining_timeout(request, deadline)?;
        let version = self.cli.version(request).await?;
        *self.cli_version.lock().await = Some(version.clone());
        if !supports_cli_contract(&version) {
            return Err(invalid_response(format!(
                "CodeGraph CLI {version} has no validated structured-output adapter"
            )));
        }
        Ok(version)
    }

    async fn mcp_is_available(&self, repo_id: &RepoId) -> bool {
        let now = tokio::time::Instant::now();
        let mut circuits = self.mcp_circuits.lock().await;
        circuits.retain(|_, until| *until > now);
        !circuits.contains_key(repo_id)
    }

    async fn open_mcp_circuit(&self, repo_id: RepoId) {
        self.mcp_circuits.lock().await.insert(
            repo_id,
            tokio::time::Instant::now() + self.circuit_breaker_cooldown,
        );
    }

    async fn probe_cli(
        &self,
        request: &mut ProviderRequest,
        deadline: tokio::time::Instant,
    ) -> Result<CliProbeOutcome, ProviderError> {
        update_remaining_timeout(request, deadline)?;
        let version = match self.cli.version(request).await {
            Ok(version) => version,
            Err(ProviderError::Cancelled) => return Err(ProviderError::Cancelled),
            Err(error) => return Ok(CliProbeOutcome::Degraded(unavailable_capability(&error))),
        };
        *self.cli_version.lock().await = Some(version.clone());
        if !supports_cli_contract(&version) {
            return Ok(CliProbeOutcome::Ready(CliProbe {
                status: ProviderStatus::Incompatible,
                degradations: vec![ProviderDegradation {
                    kind: "unsupported_cli_version".to_owned(),
                    message: format!(
                        "CodeGraph CLI {version} is outside the validated structured-output matrix."
                    ),
                    diagnostics_id: None,
                }],
                version,
            }));
        }
        update_remaining_timeout(request, deadline)?;
        let mut degradations = Vec::new();
        let status = match self.cli.status(request).await {
            Ok(status) => {
                if status
                    .version
                    .as_deref()
                    .is_some_and(|value| value != version)
                {
                    degradations.push(ProviderDegradation {
                        kind: "version_mismatch".to_owned(),
                        message:
                            "CodeGraph version and status contracts reported different versions."
                                .to_owned(),
                        diagnostics_id: None,
                    });
                }
                status.status()
            }
            Err(error) => {
                let status = if matches!(error, ProviderError::Timeout { .. }) {
                    ProviderStatus::TimedOut
                } else {
                    ProviderStatus::InvalidResponse
                };
                degradations.push(degradation_from_error(&error));
                status
            }
        };
        Ok(CliProbeOutcome::Ready(CliProbe {
            version,
            status,
            degradations,
        }))
    }

    async fn probe_mcp(
        &self,
        request: &mut ProviderRequest,
        deadline: tokio::time::Instant,
        degradations: &mut Vec<ProviderDegradation>,
    ) -> Result<Option<McpProbe>, ProviderError> {
        if !self.mcp_is_available(&request.repo_id).await {
            degradations.push(ProviderDegradation {
                kind: "mcp_circuit_open".to_owned(),
                message: "MCP was bypassed after a recent timeout; compatible CLI fallback remains available.".to_owned(),
                diagnostics_id: None,
            });
            return Ok(None);
        }
        update_remaining_timeout(request, deadline)?;
        match self.mcp.probe(request).await {
            Ok(probe) => Ok(Some(probe)),
            Err(ProviderError::Cancelled) => Err(ProviderError::Cancelled),
            Err(error) => {
                if matches!(error, ProviderError::Timeout { .. }) {
                    self.open_mcp_circuit(request.repo_id.clone()).await;
                }
                degradations.push(degradation_from_error(&error));
                Ok(None)
            }
        }
    }

    fn assemble_capability(
        request: &ProviderRequest,
        mut cli_probe: CliProbe,
        mcp_probe: Option<McpProbe>,
    ) -> ProviderCapability {
        if mcp_probe
            .as_ref()
            .and_then(|probe| probe.version.as_deref())
            .is_some_and(|version| version != cli_probe.version)
        {
            cli_probe.degradations.push(ProviderDegradation {
                kind: "mcp_cli_version_mismatch".to_owned(),
                message: "CodeGraph MCP and CLI reported different public implementation versions."
                    .to_owned(),
                diagnostics_id: None,
            });
        }
        let mut operations = BTreeMap::new();
        if let Some(probe) = &mcp_probe {
            for operation in map_mcp_tools(&probe.tools) {
                operations.insert(operation.operation, operation);
            }
        }
        for operation in cli_operations(&cli_probe.version) {
            operations.entry(operation.operation).or_insert(operation);
        }
        let status = if operations.is_empty() && cli_probe.status == ProviderStatus::Available {
            ProviderStatus::Incompatible
        } else {
            cli_probe.status
        };
        ProviderCapability {
            provider: "codegraph".to_owned(),
            version: Some(cli_probe.version),
            status,
            tools: mcp_tool_names(mcp_probe.as_ref()),
            protocol_version: mcp_probe.map(|probe| probe.protocol_version),
            operations: operations.into_values().collect(),
            degradations: cli_probe.degradations,
            remediation: remediation(status, &request.project_path),
        }
    }
}

#[async_trait]
impl LocalCodeIntelligenceProvider for CodeGraphProvider {
    fn provider_name(&self) -> &'static str {
        "codegraph"
    }

    async fn probe(
        &self,
        mut request: ProviderRequest,
    ) -> Result<ProviderCapability, ProviderError> {
        let deadline = tokio::time::Instant::now() + request.budget.timeout;
        let _permit = self.enter(&request, deadline).await?;
        let mut cli_probe = match self.probe_cli(&mut request, deadline).await? {
            CliProbeOutcome::Ready(probe) => probe,
            CliProbeOutcome::Degraded(capability) => return Ok(capability),
        };
        let mcp_probe = self
            .probe_mcp(&mut request, deadline, &mut cli_probe.degradations)
            .await?;
        Ok(Self::assemble_capability(&request, cli_probe, mcp_probe))
    }

    async fn resolve_symbols(
        &self,
        mut input: ResolveSymbolsRequest,
    ) -> Result<ResolveSymbolsResult, ProviderError> {
        validate_query(&input.query)?;
        let deadline = tokio::time::Instant::now() + input.request.budget.timeout;
        let _permit = self.enter(&input.request, deadline).await?;
        self.compatible_cli_version(&mut input.request, deadline)
            .await?;
        update_remaining_timeout(&mut input.request, deadline)?;
        self.cli.resolve_symbols(&input).await
    }

    async fn get_local_neighbors(
        &self,
        mut input: LocalNeighborsRequest,
    ) -> Result<LocalNeighborResult, ProviderError> {
        validate_query(&input.symbol)?;
        let deadline = tokio::time::Instant::now() + input.request.budget.timeout;
        let _permit = self.enter(&input.request, deadline).await?;
        self.compatible_cli_version(&mut input.request, deadline)
            .await?;
        update_remaining_timeout(&mut input.request, deadline)?;
        self.cli.local_neighbors(&input).await
    }

    async fn get_local_impact(
        &self,
        mut input: LocalImpactRequest,
    ) -> Result<LocalImpactResult, ProviderError> {
        validate_query(&input.symbol)?;
        validate_depth(input.max_depth)?;
        let deadline = tokio::time::Instant::now() + input.request.budget.timeout;
        let _permit = self.enter(&input.request, deadline).await?;
        self.compatible_cli_version(&mut input.request, deadline)
            .await?;
        update_remaining_timeout(&mut input.request, deadline)?;
        self.cli.local_impact(&input).await
    }

    async fn build_local_context(
        &self,
        mut input: LocalContextRequest,
    ) -> Result<LocalContextResult, ProviderError> {
        validate_query(&input.query)?;
        if input.max_files == 0 || input.max_files > input.request.budget.max_items {
            return Err(ProviderError::InvalidRequest(format!(
                "max_files must be between 1 and max_items ({})",
                input.request.budget.max_items
            )));
        }
        let deadline = tokio::time::Instant::now() + input.request.budget.timeout;
        let _permit = self.enter(&input.request, deadline).await?;
        let mut degradations = Vec::new();
        if self.mcp_is_available(&input.request.repo_id).await {
            update_remaining_timeout(&mut input.request, deadline)?;
            match self.mcp.local_context(&input).await {
                Ok(result) => return Ok(result),
                Err(ProviderError::Cancelled) => return Err(ProviderError::Cancelled),
                Err(error) => {
                    if matches!(error, ProviderError::Timeout { .. }) {
                        self.open_mcp_circuit(input.request.repo_id.clone()).await;
                    }
                    degradations.push(degradation_from_error(&error));
                }
            }
        } else {
            degradations.push(ProviderDegradation {
                kind: "mcp_circuit_open".to_owned(),
                message: "MCP was bypassed after a recent timeout.".to_owned(),
                diagnostics_id: None,
            });
        }
        self.compatible_cli_version(&mut input.request, deadline)
            .await?;
        update_remaining_timeout(&mut input.request, deadline)?;
        let mut result = self.cli.local_context(&input).await?;
        result.execution.degradations = degradations;
        Ok(result)
    }

    async fn get_affected_tests(
        &self,
        mut input: AffectedTestsRequest,
    ) -> Result<Option<AffectedTestsResult>, ProviderError> {
        validate_depth(input.max_depth)?;
        validate_changed_files(&input.changed_files)?;
        let deadline = tokio::time::Instant::now() + input.request.budget.timeout;
        let _permit = self.enter(&input.request, deadline).await?;
        match self
            .compatible_cli_version(&mut input.request, deadline)
            .await
        {
            Ok(_) => {}
            Err(ProviderError::InvalidResponse { .. }) => return Ok(None),
            Err(error) => return Err(error),
        }
        update_remaining_timeout(&mut input.request, deadline)?;
        self.cli.affected_tests(&input).await.map(Some)
    }

    async fn shutdown(&self) -> Result<(), ProviderError> {
        self.closed.store(true, Ordering::Release);
        self.permits.close();
        Ok(())
    }
}

fn unavailable_capability(error: &ProviderError) -> ProviderCapability {
    let status = match error {
        ProviderError::Timeout { .. } => ProviderStatus::TimedOut,
        ProviderError::InvalidResponse { .. } => ProviderStatus::InvalidResponse,
        _ => ProviderStatus::Unavailable,
    };
    ProviderCapability {
        provider: "codegraph".to_owned(),
        version: None,
        status,
        tools: Vec::new(),
        protocol_version: None,
        operations: Vec::new(),
        degradations: vec![degradation_from_error(error)],
        remediation: Some("Install CodeGraph or configure its public executable path.".to_owned()),
    }
}

fn mcp_tool_names(probe: Option<&McpProbe>) -> Vec<String> {
    let mut names = probe
        .map(|probe| {
            probe
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn remediation(status: ProviderStatus, project_path: &Path) -> Option<String> {
    match status {
        ProviderStatus::IndexMissing => Some(format!("Create the index explicitly with `codegraph init {}`.", project_path.display())),
        ProviderStatus::Stale => Some(format!("Refresh the index explicitly with `codegraph sync {}`.", project_path.display())),
        ProviderStatus::Incompatible => Some("Use a CodeGraph version listed in the compatibility matrix or rely on federated boundaries without local enrichment.".to_owned()),
        _ => None,
    }
}

fn validate_request(request: &ProviderRequest) -> Result<(), ProviderError> {
    if request.cancellation.is_cancelled() {
        return Err(ProviderError::Cancelled);
    }
    if request.budget.timeout.is_zero() {
        return Err(ProviderError::InvalidRequest(
            "timeout must be greater than zero".to_owned(),
        ));
    }
    if request.budget.max_output_bytes == 0 {
        return Err(ProviderError::InvalidRequest(
            "max_output_bytes must be greater than zero".to_owned(),
        ));
    }
    if request.budget.max_items == 0 {
        return Err(ProviderError::InvalidRequest(
            "max_items must be greater than zero".to_owned(),
        ));
    }
    if !request.project_path.is_absolute() {
        return Err(ProviderError::InvalidRequest(
            "project_path must be absolute".to_owned(),
        ));
    }
    if !request.project_path.is_dir() {
        return Err(ProviderError::InvalidRequest(
            "project_path must identify an existing directory".to_owned(),
        ));
    }
    Ok(())
}

fn validate_query(query: &str) -> Result<(), ProviderError> {
    if query.trim().is_empty() {
        return Err(ProviderError::InvalidRequest(
            "provider query must not be empty".to_owned(),
        ));
    }
    if query.len() > MAX_QUERY_BYTES {
        return Err(ProviderError::InvalidRequest(format!(
            "provider query exceeds {MAX_QUERY_BYTES} bytes"
        )));
    }
    Ok(())
}

fn validate_depth(depth: usize) -> Result<(), ProviderError> {
    if depth == 0 || depth > MAX_LOCAL_DEPTH {
        return Err(ProviderError::InvalidRequest(format!(
            "local depth must be between 1 and {MAX_LOCAL_DEPTH}"
        )));
    }
    Ok(())
}

fn validate_changed_files(files: &[String]) -> Result<(), ProviderError> {
    if files.is_empty() || files.len() > MAX_CHANGED_FILES {
        return Err(ProviderError::InvalidRequest(format!(
            "changed_files must contain between 1 and {MAX_CHANGED_FILES} paths"
        )));
    }
    for file in files {
        let path = Path::new(file);
        if file.is_empty()
            || path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
        {
            return Err(ProviderError::InvalidRequest(format!(
                "changed file path must be non-empty and repository-relative: {file}"
            )));
        }
    }
    Ok(())
}

fn update_remaining_timeout(
    request: &mut ProviderRequest,
    deadline: tokio::time::Instant,
) -> Result<(), ProviderError> {
    request.budget.timeout = deadline
        .checked_duration_since(tokio::time::Instant::now())
        .ok_or_else(timeout_error)?;
    if request.budget.timeout.is_zero() {
        return Err(timeout_error());
    }
    Ok(())
}

pub(crate) fn diagnostic_id() -> String {
    format!(
        "cg-{:016x}",
        DIAGNOSTIC_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}

pub(crate) fn timeout_error() -> ProviderError {
    ProviderError::Timeout {
        diagnostics_id: diagnostic_id(),
    }
}

pub(crate) fn output_limit(limit_bytes: usize) -> ProviderError {
    ProviderError::OutputLimit {
        limit_bytes,
        diagnostics_id: diagnostic_id(),
    }
}

pub(crate) fn invalid_response(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidResponse {
        message: message.into(),
        diagnostics_id: diagnostic_id(),
    }
}

pub(crate) fn transport_error(message: impl Into<String>) -> ProviderError {
    ProviderError::Transport {
        message: message.into(),
        diagnostics_id: diagnostic_id(),
    }
}

fn degradation_from_error(error: &ProviderError) -> ProviderDegradation {
    let (kind, diagnostics_id) = match error {
        ProviderError::Cancelled => ("cancelled", None),
        ProviderError::Timeout { diagnostics_id } => ("timeout", Some(diagnostics_id.clone())),
        ProviderError::OutputLimit { diagnostics_id, .. } => {
            ("output_limit", Some(diagnostics_id.clone()))
        }
        ProviderError::CircuitOpen => ("circuit_open", None),
        ProviderError::InvalidRequest(_) => ("invalid_request", None),
        ProviderError::InvalidResponse { diagnostics_id, .. } => {
            ("invalid_response", Some(diagnostics_id.clone()))
        }
        ProviderError::Transport { diagnostics_id, .. } => {
            ("transport", Some(diagnostics_id.clone()))
        }
    };
    ProviderDegradation {
        kind: kind.to_owned(),
        message: error.to_string(),
        diagnostics_id,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::Ordering;

    use code_system_graph_model::RepoId;
    use tokio_util::sync::CancellationToken;

    use super::{CodeGraphConfig, CodeGraphProvider, validate_changed_files};
    use crate::{ProviderBudget, ProviderError, ProviderRequest};

    fn absolute_project_path() -> PathBuf {
        std::env::current_dir().expect("test process should expose an absolute current directory")
    }

    #[test]
    fn provider_should_reject_zero_process_budget() {
        let result = CodeGraphProvider::new(CodeGraphConfig {
            max_concurrent_processes: 0,
            ..CodeGraphConfig::default()
        });

        assert!(matches!(result, Err(ProviderError::InvalidRequest(_))));
    }

    #[test]
    fn changed_files_should_reject_parent_escape() {
        let result = validate_changed_files(&["../outside.rs".to_owned()]);

        assert!(matches!(result, Err(ProviderError::InvalidRequest(_))));
    }

    #[tokio::test]
    async fn cancelled_probe_should_not_start_a_child() {
        let provider =
            CodeGraphProvider::new(CodeGraphConfig::default()).expect("valid provider config");
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let result = crate::LocalCodeIntelligenceProvider::probe(
            &provider,
            ProviderRequest {
                repo_id: RepoId::new("repo:test"),
                project_path: absolute_project_path(),
                budget: ProviderBudget::default(),
                cancellation,
            },
        )
        .await;

        assert_eq!(result, Err(ProviderError::Cancelled));
    }

    #[tokio::test]
    async fn provider_should_measure_held_process_permits() {
        let provider = CodeGraphProvider::new(CodeGraphConfig {
            max_concurrent_processes: 2,
            ..CodeGraphConfig::default()
        })
        .expect("valid provider config");
        let request = ProviderRequest {
            repo_id: RepoId::new("repo:test"),
            project_path: absolute_project_path(),
            budget: ProviderBudget::default(),
            cancellation: CancellationToken::new(),
        };
        assert_eq!(provider.maximum_concurrency_observed(), 0);

        let first = provider
            .enter(
                &request,
                tokio::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .await
            .expect("first permit");
        assert_eq!(provider.maximum_concurrency_observed(), 1);
        let second = provider
            .enter(
                &request,
                tokio::time::Instant::now() + std::time::Duration::from_secs(1),
            )
            .await
            .expect("second permit");
        assert_eq!(provider.maximum_concurrency_observed(), 2);

        drop((first, second));
        assert_eq!(provider.active_processes.load(Ordering::Acquire), 0);
    }
}

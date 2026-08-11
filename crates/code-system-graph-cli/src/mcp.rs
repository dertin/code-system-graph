//! MCP stdio delivery adapter.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use code_system_graph_core::{
    ChangeAnalysisOptions, ExecutionPolicy, ImpactRequest, PullRequestProviderKind
};
use code_system_graph_model::{FreshnessSummary, OverallFreshness, ToolEnvelope, ToolStatus};
use code_system_graph_store_sqlite::{SqliteStore, StoreLock};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourceTemplatesResult, ListResourcesResult, PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, ResourceTemplate, ServerCapabilities, ServerInfo
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler, tool, tool_handler, tool_router};

use crate::{
    CODEGRAPH_DISABLED_CODE, CODEGRAPH_DISABLED_MESSAGE, ChangesInput, CommunityReport, ExploreInput, PullRequestInput, QueryActionCapabilities, ScanOverrides, SearchInput, TraceInput, add_manual_link_to_manifest, add_repository_to_manifest, analyze_workspace_changes, communities_workspace, explore_repository, impact_workspace, impact_workspace_with_codegraph, inspect_pull_request, remove_repository_from_manifest, scan_workspace_with_overrides, search_workspace_for_delivery, trace_workspace
};

#[path = "mcp_support/mod.rs"]
mod mcp_support;

use mcp_support::{
    ADMIN_TOOL_NAMES, AdminAudit, AgentToolResult, CacheCleanInput, CacheCleanReport, CommunitiesInput, ContractsInput, MARKDOWN_MIME_TYPE, ManifestAdminReport, ManualLinkWriteInput, ResourceErrorKind, SourceContextInput, WorkspaceInput, WorkspaceUpdateInput, admin_audit_envelope, admin_mutation_envelope, configured_manifest_path, contracts_envelope, read_resource, resource_templates, resource_uris, source_context_envelope, status_envelope
};

const EXPLORE_TOOL_NAME: &str = "explore";

/// Trusted process-level policy for bounded local intelligence.
#[derive(Debug, Clone, Default)]
struct CodeGraphPolicy {
    enabled: bool,
    binary: Option<OsString>,
}

/// Workspace-scoped `Code System Graph` MCP server with an optional explicit admin profile.
#[derive(Debug, Clone)]
pub struct CodeSystemGraphServer {
    database_path: PathBuf,
    workspace: String,
    admin_enabled: bool,
    codegraph: CodeGraphPolicy,
    github_pull_requests_enabled: bool,
    bitbucket_pull_requests_enabled: bool,
    execution_policy: ExecutionPolicy,
    tool_router: ToolRouter<Self>,
}

#[tool_router(router = tool_router)]
impl CodeSystemGraphServer {
    /// Creates a read-only server for one configured workspace.
    #[must_use]
    pub fn new(database_path: PathBuf, workspace: String) -> Self {
        let mut tool_router = Self::tool_router();
        for name in ADMIN_TOOL_NAMES {
            tool_router.disable_route(name);
        }
        tool_router.disable_route(EXPLORE_TOOL_NAME);
        Self {
            database_path,
            workspace,
            admin_enabled: false,
            codegraph: CodeGraphPolicy::default(),
            github_pull_requests_enabled: false,
            bitbucket_pull_requests_enabled: false,
            execution_policy: ExecutionPolicy::default(),
            tool_router,
        }
    }

    /// Enables or disables the explicit administrative MCP profile.
    ///
    /// Disabled routes are absent from discovery and cannot be called. Applications should map
    /// their trusted `CODE_SYSTEM_GRAPH_MCP_ADMIN=1`-equivalent configuration to this constructor option;
    /// request arguments can never enable the profile.
    #[must_use]
    pub fn with_admin_profile(mut self, enabled: bool) -> Self {
        self.admin_enabled = enabled;
        for name in ADMIN_TOOL_NAMES {
            if enabled {
                self.tool_router.enable_route(name);
            } else {
                self.tool_router.disable_route(name);
            }
        }
        self
    }

    /// Applies trusted process-level `CodeGraph` policy to exploration, impact, and scans.
    #[must_use]
    pub fn with_codegraph(mut self, enabled: bool, binary: Option<OsString>) -> Self {
        if enabled {
            self.tool_router.enable_route(EXPLORE_TOOL_NAME);
        } else {
            self.tool_router.disable_route(EXPLORE_TOOL_NAME);
        }
        self.codegraph = CodeGraphPolicy { enabled, binary };
        self
    }

    /// Applies the validated immutable workspace execution policy.
    #[must_use]
    pub fn with_execution_policy(mut self, policy: ExecutionPolicy) -> Self {
        self.execution_policy = policy;
        self
    }

    /// Enables selected public pull-request providers while retaining per-request consent.
    #[must_use]
    pub fn with_pull_request_providers(mut self, github: bool, bitbucket: bool) -> Self {
        self.github_pull_requests_enabled = github;
        self.bitbucket_pull_requests_enabled = bitbucket;
        self
    }

    fn markdown_result(&self, result: AgentToolResult<'_>) -> CallToolResult {
        let maximum = usize::try_from(self.execution_policy.max_mcp_tool_response_bytes)
            .expect("validated policy bytes are usize-representable");
        let (markdown, is_error) = result.render(maximum);
        let content = vec![ContentBlock::text(markdown)];
        if is_error {
            CallToolResult::error(content)
        } else {
            CallToolResult::success(content)
        }
    }

    /// Traces a bounded path through the current federated snapshot.
    #[tool(
        name = "trace",
        description = "Finds a bounded, explainable path between two persisted entities across repository boundaries. Use when both endpoint identifiers are known; use query first to discover identifiers. Returns bounded Markdown with trace segments and freshness metadata.",
        annotations(
            title = "Cross-repository path trace",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn trace(&self, Parameters(input): Parameters<TraceInput>) -> CallToolResult {
        let envelope = match trace_workspace(&self.database_path, &self.workspace, &input) {
            Ok(envelope) => envelope,
            Err(error) => ToolEnvelope {
                schema_version: 2,
                status: ToolStatus::Error,
                data: None,
                freshness: FreshnessSummary {
                    overall: OverallFreshness::Unknown,
                    stale_repositories: Vec::new(),
                    reasons: vec!["Trace inputs could not be validated.".to_owned()],
                },
                warnings: vec![error.to_string()],
            },
        };
        self.markdown_result(AgentToolResult::Trace(&envelope))
    }

    /// Searches ranked federated entities without returning source bodies.
    #[tool(
        name = "query",
        description = "Searches persisted architecture entities, contracts, and communities across the workspace without returning source bodies. Use to discover entity identifiers before trace, source_context, or impact. Returns bounded Markdown with ranked matches and freshness metadata.",
        annotations(
            title = "Federated entity search",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn query(&self, Parameters(input): Parameters<SearchInput>) -> CallToolResult {
        let envelope = match search_workspace_for_delivery(
            &self.database_path,
            &self.workspace,
            &input,
            &self.execution_policy,
            QueryActionCapabilities {
                source_context: true,
                explore: self.codegraph.enabled,
            },
        ) {
            Ok(envelope) => envelope,
            Err(error) => error_envelope("Query inputs could not be validated.", error),
        };
        self.markdown_result(AgentToolResult::Query(&envelope))
    }

    /// Explores bounded repository-local source and flow context without persisting source.
    #[tool(
        name = "explore",
        description = "Retrieves bounded, ephemeral repository-local source and call-flow context through CodeGraph. Use for symbols, callers, callees, tests, and implementation details; use query for persisted cross-repository entities. Returns bounded Markdown with source-bearing local context that is never persisted.",
        annotations(
            title = "Repository source exploration",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn explore(
        &self,
        Parameters(input): Parameters<ExploreInput>,
    ) -> Result<CallToolResult, McpError> {
        if !self.codegraph.enabled {
            return Err(codegraph_disabled_error());
        }
        let envelope = explore_repository(
            &self.database_path,
            &self.workspace,
            &input,
            self.codegraph.binary.clone(),
            &self.execution_policy,
        )
        .await;
        Ok(self.markdown_result(AgentToolResult::Explore(&envelope)))
    }

    /// Lists, inspects, or compares persisted deterministic communities.
    #[tool(
        name = "communities",
        description = "Lists, shows, or compares deterministic persisted communities using one explicit action. Use for inferred service boundaries, memberships, metrics, and snapshot comparisons; use query for general entity search. Returns bounded Markdown with community results and freshness metadata.",
        annotations(
            title = "Community inspection",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn communities(
        &self,
        Parameters(input): Parameters<CommunitiesInput>,
    ) -> CallToolResult {
        if input.workspace() != self.workspace {
            let envelope = error_envelope::<CommunityReport>(
                "Workspace policy rejected the request.",
                format!(
                    "workspace `{}` is outside this server's configured workspace `{}`",
                    input.workspace(),
                    self.workspace
                ),
            );
            return self.markdown_result(AgentToolResult::Communities(&envelope));
        }
        let envelope = match communities_workspace(
            &self.database_path,
            &self.workspace,
            &input.application_input(),
        ) {
            Ok(envelope) => envelope,
            Err(error) => error_envelope("Community inputs could not be validated.", error),
        };
        self.markdown_result(AgentToolResult::Communities(&envelope))
    }

    /// Computes conservative impact without executing tests or repository commands.
    #[tool(
        name = "impact",
        description = "Analyzes bounded upstream or downstream effects and conservative risk for one persisted graph target across repositories. Use for a known entity; use analyze_changes for staged, worktree, or committed Git changes. Returns bounded Markdown with impact, freshness, and optional ephemeral CodeGraph enrichment.",
        annotations(
            title = "Cross-repository impact analysis",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn impact(&self, Parameters(input): Parameters<ImpactRequest>) -> CallToolResult {
        let result = if self.codegraph.enabled {
            impact_workspace_with_codegraph(
                &self.database_path,
                &self.workspace,
                &input,
                self.codegraph.binary.clone(),
            )
            .await
        } else {
            impact_workspace(&self.database_path, &self.workspace, &input)
        };
        let envelope = match result {
            Ok(envelope) => envelope,
            Err(error) => error_envelope("Impact inputs could not be validated.", error),
        };
        self.markdown_result(AgentToolResult::Impact(&envelope))
    }

    /// Inspects bounded local Git changes without modifying repository state.
    #[tool(
        name = "analyze_changes",
        description = "Analyzes fingerprinted staged, worktree, or committed local Git changes and maps them to conservative graph impact. Use for repository diffs; use impact for one known persisted target. Returns bounded Markdown with change impact and freshness metadata without modifying Git state.",
        annotations(
            title = "Local change impact analysis",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn analyze_changes(
        &self,
        Parameters(input): Parameters<ChangesInput>,
    ) -> CallToolResult {
        let envelope = match analyze_workspace_changes(
            &self.database_path,
            &self.workspace,
            &input,
            &ChangeAnalysisOptions::default(),
            None,
        )
        .await
        {
            Ok(envelope) => envelope,
            Err(error) => error_envelope("Change inputs could not be validated.", error),
        };
        self.markdown_result(AgentToolResult::AnalyzeChanges(&envelope))
    }

    /// Inspects one explicitly enabled public pull-request provider.
    #[tool(
        name = "analyze_pull_request",
        description = "Fetches and analyzes one consented GitHub or Bitbucket Cloud pull request from a provider enabled at server startup. Use for remote pull-request metadata and changed-file context; use analyze_changes for local Git state. Returns bounded Markdown with pull-request inspection and provider freshness metadata.",
        annotations(
            title = "Pull request analysis",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = true
        )
    )]
    pub async fn analyze_pull_request(
        &self,
        Parameters(input): Parameters<PullRequestInput>,
    ) -> CallToolResult {
        let (enabled, token, basic_auth_username) = match input.provider {
            PullRequestProviderKind::GitHub => (
                self.github_pull_requests_enabled,
                std::env::var("GITHUB_TOKEN").ok(),
                None,
            ),
            PullRequestProviderKind::BitbucketCloud => (
                self.bitbucket_pull_requests_enabled,
                std::env::var("BITBUCKET_TOKEN").ok(),
                std::env::var("BITBUCKET_USER").ok(),
            ),
            PullRequestProviderKind::BitbucketDataCenter => (false, None, None),
        };
        let envelope = match inspect_pull_request(&input, enabled, token, basic_auth_username).await
        {
            Ok(envelope) => envelope,
            Err(error) => error_envelope(
                "Pull-request inputs or provider response could not be validated.",
                error,
            ),
        };
        self.markdown_result(AgentToolResult::AnalyzePullRequest(&envelope))
    }

    /// Reports persisted snapshot health without reading repository source files.
    #[tool(
        name = "status",
        description = "Reports persisted graph health, freshness, and current snapshot metadata for the configured workspace. Use to verify workspace readiness before other analysis; not for entity search. Returns bounded source-free Markdown.",
        annotations(
            title = "Workspace graph status",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn status(&self, Parameters(input): Parameters<WorkspaceInput>) -> CallToolResult {
        let envelope = status_envelope(&self.database_path, &self.workspace, &input.workspace);
        self.markdown_result(AgentToolResult::Status(&envelope))
    }

    /// Lists a bounded page of contract entities from the immutable graph snapshot.
    #[tool(
        name = "contracts",
        description = "Lists, shows, validates, diffs, or explains persisted contracts using one explicit action. Use for API, event, database, package, and infrastructure contract analysis; use query for non-contract entities. Returns bounded Markdown with contract results and freshness metadata.",
        annotations(
            title = "Contract inspection and validation",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn contracts(&self, Parameters(input): Parameters<ContractsInput>) -> CallToolResult {
        let envelope = contracts_envelope(&self.database_path, &self.workspace, &input);
        self.markdown_result(AgentToolResult::Contracts(&envelope))
    }

    /// Returns bounded persisted graph and evidence metadata for one entity.
    #[tool(
        name = "source_context",
        description = "Returns bounded, source-free persisted graph context and evidence metadata for one exact entity. Use after query to explain relationships and provenance without source bodies. It does not return implementation source. Returns bounded Markdown with evidence and freshness metadata.",
        annotations(
            title = "Persisted entity context",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn source_context(
        &self,
        Parameters(input): Parameters<SourceContextInput>,
    ) -> CallToolResult {
        let envelope = source_context_envelope(&self.database_path, &self.workspace, &input);
        self.markdown_result(AgentToolResult::SourceContext(&envelope))
    }

    /// Scans and atomically publishes the configured workspace.
    #[tool(
        name = "scan",
        description = "Scans the configured workspace and atomically publishes a new persisted graph snapshot. Use only in the enabled admin profile after repository or configuration changes; use status for a read-only health check. Returns bounded audited Markdown with scan, snapshot, freshness, and visible CodeGraph degradation details.",
        annotations(
            title = "Publish workspace snapshot",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn scan(&self, Parameters(input): Parameters<WorkspaceInput>) -> CallToolResult {
        let envelope = self.run_admin_scan(&input.workspace, "scan");
        self.markdown_result(AgentToolResult::Scan(&envelope))
    }

    /// Adds or removes one repository entry through the constrained manifest editor.
    #[tool(
        name = "update_workspace",
        description = "Adds or removes one repository registration in the configured workspace manifest, then validates and rescans it. Use only in the enabled admin profile for constrained workspace membership changes; use scan when membership is unchanged. Returns bounded audited Markdown with mutation, backup, and scan details.",
        annotations(
            title = "Update workspace repositories",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn update_workspace(
        &self,
        Parameters(input): Parameters<WorkspaceUpdateInput>,
    ) -> CallToolResult {
        let envelope = self.run_workspace_update(&input);
        self.markdown_result(AgentToolResult::UpdateWorkspace(&envelope))
    }

    /// Appends one exact manual relationship declaration and republishes the workspace.
    #[tool(
        name = "write_manual_link",
        description = "Adds or suppresses one exact manual graph relationship in the configured manifest, then validates and rescans it. Use only in the enabled admin profile when an operator must record a reasoned relationship decision. Returns bounded audited Markdown with mutation, backup, and scan details.",
        annotations(
            title = "Write manual relationship",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn write_manual_link(
        &self,
        Parameters(input): Parameters<ManualLinkWriteInput>,
    ) -> CallToolResult {
        let envelope = self.run_manual_link_write(&input);
        self.markdown_result(AgentToolResult::WriteManualLink(&envelope))
    }

    /// Removes bounded reusable query results for the configured workspace.
    #[tool(
        name = "clean_cache",
        description = "Removes reusable query-summary cache entries for the configured workspace without changing graph snapshots. Use only in the enabled admin profile to invalidate cached query results; not to rescan source. Returns bounded audited Markdown with removed-entry and snapshot details.",
        annotations(
            title = "Clear query cache",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub async fn clean_cache(
        &self,
        Parameters(input): Parameters<CacheCleanInput>,
    ) -> CallToolResult {
        let envelope = self.run_cache_clean(&input);
        self.markdown_result(AgentToolResult::CleanCache(&envelope))
    }

    /// Recomputes communities by publishing a freshly analyzed workspace snapshot.
    #[tool(
        name = "recompute_communities",
        description = "Rescans the configured workspace and deterministically recomputes communities in a newly published snapshot. Use only in the enabled admin profile when community results must be refreshed; use communities for read-only inspection. Returns bounded audited Markdown with scan, snapshot, and freshness details.",
        annotations(
            title = "Recompute workspace communities",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub async fn recompute_communities(
        &self,
        Parameters(input): Parameters<WorkspaceInput>,
    ) -> CallToolResult {
        let envelope = self.run_admin_scan(&input.workspace, "community_recompute");
        self.markdown_result(AgentToolResult::RecomputeCommunities(&envelope))
    }

    fn run_admin_scan(
        &self,
        workspace: &str,
        operation: &str,
    ) -> ToolEnvelope<AdminAudit<crate::ScanSummary>> {
        if let Err(error) = self.validate_admin_workspace(workspace) {
            return error;
        }
        let manifest = match configured_manifest_path(&self.database_path, &self.workspace) {
            Ok(path) => path,
            Err(error) => {
                return error_envelope("The configured manifest path could not be loaded.", error);
            }
        };
        match scan_workspace_with_overrides(
            &manifest,
            &self.database_path,
            &ScanOverrides {
                workspace: Some(workspace.to_owned()),
                codegraph: self.codegraph.enabled,
                codegraph_binary: self.codegraph.binary.as_ref().map(PathBuf::from),
                ..ScanOverrides::default()
            },
        ) {
            Ok(summary) => {
                admin_audit_envelope(&self.database_path, &self.workspace, operation, summary)
            }
            Err(error) => error_envelope("Administrative scan failed.", error),
        }
    }

    fn run_workspace_update(
        &self,
        input: &WorkspaceUpdateInput,
    ) -> ToolEnvelope<AdminAudit<ManifestAdminReport>> {
        if let Err(error) = self.validate_admin_workspace(input.workspace()) {
            return error;
        }
        let manifest = match configured_manifest_path(&self.database_path, &self.workspace) {
            Ok(path) => path,
            Err(error) => {
                return error_envelope("The configured manifest path could not be loaded.", error);
            }
        };
        let mutation_lock = match StoreLock::acquire(&self.database_path, Duration::from_mins(5)) {
            Ok(lock) => lock,
            Err(error) => {
                return error_envelope("Workspace update lock acquisition failed.", error);
            }
        };
        let mutation = match input.repository_path() {
            Some(path) => add_repository_to_manifest(&manifest, input.alias(), path, false),
            None => remove_repository_from_manifest(&manifest, input.alias(), false),
        };
        let mutation = match mutation {
            Ok(summary) => summary,
            Err(error) => return error_envelope("Workspace manifest update failed.", error),
        };
        drop(mutation_lock);
        let scan = match scan_workspace_with_overrides(
            &manifest,
            &self.database_path,
            &ScanOverrides {
                workspace: Some(self.workspace.clone()),
                codegraph: self.codegraph.enabled,
                codegraph_binary: self.codegraph.binary.as_ref().map(PathBuf::from),
                ..ScanOverrides::default()
            },
        ) {
            Ok(summary) => summary,
            Err(error) => {
                return error_envelope(
                    "Workspace manifest changed but the follow-up scan failed; use the recorded backup to restore if needed.",
                    error,
                );
            }
        };
        let snapshot_id = scan.snapshot_id.clone();
        admin_mutation_envelope(
            &self.database_path,
            &self.workspace,
            "workspace_update",
            &snapshot_id,
            ManifestAdminReport { mutation, scan },
        )
    }

    fn run_manual_link_write(
        &self,
        input: &ManualLinkWriteInput,
    ) -> ToolEnvelope<AdminAudit<ManifestAdminReport>> {
        if let Err(error) = self.validate_admin_workspace(input.workspace()) {
            return error;
        }
        let manifest = match configured_manifest_path(&self.database_path, &self.workspace) {
            Ok(path) => path,
            Err(error) => {
                return error_envelope("The configured manifest path could not be loaded.", error);
            }
        };
        let mutation_lock = match StoreLock::acquire(&self.database_path, Duration::from_mins(5)) {
            Ok(lock) => lock,
            Err(error) => {
                return error_envelope("Manual-link write lock acquisition failed.", error);
            }
        };
        let declaration = input.declaration();
        let mutation = match add_manual_link_to_manifest(&manifest, &declaration, false) {
            Ok(summary) => summary,
            Err(error) => return error_envelope("Manual-link manifest update failed.", error),
        };
        drop(mutation_lock);
        let scan = match scan_workspace_with_overrides(
            &manifest,
            &self.database_path,
            &ScanOverrides {
                workspace: Some(self.workspace.clone()),
                codegraph: self.codegraph.enabled,
                codegraph_binary: self.codegraph.binary.as_ref().map(PathBuf::from),
                ..ScanOverrides::default()
            },
        ) {
            Ok(summary) => summary,
            Err(error) => {
                return error_envelope(
                    "Manual-link manifest changed but the follow-up scan failed; use the recorded backup to restore if needed.",
                    error,
                );
            }
        };
        let snapshot_id = scan.snapshot_id.clone();
        admin_mutation_envelope(
            &self.database_path,
            &self.workspace,
            "manual_link_write",
            &snapshot_id,
            ManifestAdminReport { mutation, scan },
        )
    }

    fn run_cache_clean(
        &self,
        input: &CacheCleanInput,
    ) -> ToolEnvelope<AdminAudit<CacheCleanReport>> {
        if let Err(error) = self.validate_admin_workspace(&input.workspace) {
            return error;
        }
        let _lock = match StoreLock::acquire(&self.database_path, Duration::from_mins(5)) {
            Ok(lock) => lock,
            Err(error) => return error_envelope("Query cache lock acquisition failed.", error),
        };
        let mut store = match SqliteStore::open(&self.database_path) {
            Ok(store) => store,
            Err(error) => return error_envelope("Query cache store could not be opened.", error),
        };
        let snapshot = match store.current_snapshot_summary(&self.workspace) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return error_envelope("Current snapshot could not be resolved.", error);
            }
        };
        let removed_entries = match store.clear_query_cache(&self.workspace) {
            Ok(count) => count,
            Err(error) => return error_envelope("Query cache cleanup failed.", error),
        };
        admin_mutation_envelope(
            &self.database_path,
            &self.workspace,
            "cache_clean",
            &snapshot.snapshot_id,
            CacheCleanReport { removed_entries },
        )
    }

    fn validate_admin_workspace<T>(&self, workspace: &str) -> Result<(), ToolEnvelope<T>> {
        if !self.admin_enabled {
            return Err(error_envelope(
                "Administrative profile is disabled.",
                "set the trusted constructor admin option before server startup",
            ));
        }
        if workspace != self.workspace {
            return Err(error_envelope(
                "Workspace policy rejected the request.",
                format!(
                    "workspace `{workspace}` is outside this server's configured `{}` policy",
                    self.workspace
                ),
            ));
        }
        Ok(())
    }
}

fn error_envelope<T>(reason: &str, error: impl std::fmt::Display) -> ToolEnvelope<T> {
    ToolEnvelope {
        schema_version: 2,
        status: ToolStatus::Error,
        data: None,
        freshness: FreshnessSummary {
            overall: OverallFreshness::Unknown,
            stale_repositories: Vec::new(),
            reasons: vec![reason.to_owned()],
        },
        warnings: vec![error.to_string()],
    }
}

fn codegraph_disabled_error() -> McpError {
    McpError::invalid_request(
        CODEGRAPH_DISABLED_MESSAGE,
        Some(serde_json::json!({ "code": CODEGRAPH_DISABLED_CODE })),
    )
}

#[allow(unknown_lints)]
#[allow(
    clippy::unused_async_trait_impl,
    reason = "rmcp requires async trait methods even when resource reads complete synchronously"
)]
#[tool_handler(router = self.tool_router)]
impl ServerHandler for CodeSystemGraphServer {
    fn get_info(&self) -> ServerInfo {
        let instructions = if self.codegraph.enabled {
            "Code System Graph exposes bounded cross-repository intelligence. Tool and resource results \
             are delivered as one bounded Markdown text block without structuredContent; schema version 2 \
             remains the logical contract, and only the schema catalog embeds fenced JSON. Use query for persisted entity discovery, explore for ephemeral \
             repository source, source_context for source-free evidence, impact for known targets, \
             and analyze_changes for Git diffs. Administrative tools mutate state only when enabled."
        } else {
            "Code System Graph exposes bounded cross-repository intelligence. Tool and resource results \
             are delivered as one bounded Markdown text block without structuredContent; schema version 2 \
             remains the logical contract, and only the schema catalog embeds fenced JSON. Use query for persisted entity discovery, source_context for source-free \
             evidence, impact for known targets, and analyze_changes for Git diffs. Repository-local \
             source access is unavailable because CodeGraph is disabled. Administrative tools mutate \
             state only when enabled."
        };
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(Implementation::new(
            "code_system_graph",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(instructions)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let resources = resource_uris(&self.workspace)
            .into_iter()
            .map(|(uri, name, description)| {
                Resource::new(uri, name)
                    .with_description(description)
                    .with_mime_type(MARKDOWN_MIME_TYPE)
            })
            .collect();
        Ok(ListResourcesResult::with_all_items(resources).with_ttl_ms(1_000))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        let templates = resource_templates()
            .into_iter()
            .map(|(uri, name, description)| {
                ResourceTemplate::new(uri, name)
                    .with_description(description)
                    .with_mime_type(MARKDOWN_MIME_TYPE)
            })
            .collect();
        Ok(ListResourceTemplatesResult::with_all_items(templates).with_ttl_ms(1_000))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let uri = request.uri;
        match read_resource(
            &self.database_path,
            &self.workspace,
            &uri,
            &self.execution_policy,
        ) {
            Ok(text) => Ok(ReadResourceResult::new(vec![
                ResourceContents::text(text, uri).with_mime_type(MARKDOWN_MIME_TYPE),
            ])
            .into()),
            Err(error) => match error.kind {
                ResourceErrorKind::Invalid => Err(McpError::invalid_params(error.message, None)),
                ResourceErrorKind::Missing => {
                    Err(McpError::resource_not_found(error.message, None))
                }
                ResourceErrorKind::Internal => Err(McpError::internal_error(error.message, None)),
            },
        }
    }
}

#[cfg(test)]
mod tests;

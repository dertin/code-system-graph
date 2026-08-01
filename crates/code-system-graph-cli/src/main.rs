//! `Code System Graph` command-line and MCP stdio entry point.

mod sync_watch;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::{ArgAction, CommandFactory, Parser, Subcommand, ValueEnum};
use code_system_graph::http_server::{BearerToken, HttpServerConfig, serve_http};
use code_system_graph::mcp::CodeSystemGraphServer;
use code_system_graph::{
    ApplicationError, ChangesInput, CommunityInput, PullRequestInput, PullRequestListInput, ScanOverrides, SearchInput, TraceInput, add_repository_to_manifest, add_workspace_to_registry, analyze_workspace_changes_with_cancellation, application_exit_code, backup_database, communities_workspace, contracts_workspace, create_diagnostic_bundle, doctor_workspace, export_workspace, impact_workspace, impact_workspace_with_codegraph, initialize_workspace, inspect_pull_request_with_cancellation, list_pull_requests, list_repository_registry, list_workspace_registry, migrate_database, remove_repository_from_manifest, remove_workspace_from_registry, restore_database, scan_workspace_with_overrides, search_workspace, show_config, status_workspace, sync_workspace_with_overrides, trace_workspace, traverse_workspace
};
use code_system_graph_core::{
    ChangeAnalysisOptions, ChangeScope, ContractAction, ContractRequest, ExitCode, ExportFormat, ExportRequest, ImpactDirection, ImpactOptions, ImpactRequest, ImpactTarget, PullRequestListState, PullRequestOrderSuggestion, PullRequestOverlap, PullRequestProviderKind, PullRequestSemanticInput, TraversalAlgorithm, TraversalDirection, TraversalFilters, TraversalOptions, TraversalRequest, semantic_pull_request_overlap, suggest_pull_request_order
};
use code_system_graph_hooks::{
    HookMode, HostKind, InstallRequest, install as install_hook, status as hook_status, uninstall as uninstall_hook
};
use code_system_graph_model::NodeId;
use rmcp::ServiceExt;
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(
    name = "csgraph",
    version,
    about = "Federated multi-repository code intelligence",
    after_help = "Examples:\n  csgraph init . --name commerce\n  csgraph scan --database .code-system-graph/code-system-graph.db\n  csgraph query \"orders contract\" --workspace commerce --database .code-system-graph/code-system-graph.db --json\n  csgraph mcp --workspace commerce --database .code-system-graph/code-system-graph.db"
)]
struct Cli {
    /// Suppress non-result diagnostics.
    #[arg(long, global = true, conflicts_with = "verbose")]
    quiet: bool,
    /// Increase diagnostic verbosity without changing JSON stdout.
    #[arg(long, short = 'v', global = true, action = ArgAction::Count)]
    verbose: u8,
    /// Diagnostic format written only to stderr.
    #[arg(long, global = true, default_value = "text")]
    log_format: LogFormat,
    /// Request machine-readable output; structured commands already use JSON by default.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum LogFormat {
    Text,
    Json,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a strict minimal `code-system-graph.yaml` without overwriting.
    Init {
        /// Directory in which to create the manifest.
        #[arg(default_value = ".")]
        path: PathBuf,
        /// Explicit workspace name; defaults to the directory name.
        #[arg(long)]
        name: Option<String>,
    },
    /// Scan declared HTTP boundaries and publish an atomic snapshot.
    Scan {
        /// Optional workspace name, verified against the manifest.
        workspace: Option<String>,
        /// Workspace manifest path.
        #[arg(long, default_value = "code-system-graph.yaml")]
        config: PathBuf,
        /// `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
        /// Override one repository `OpenAPI` path as `alias=path`.
        #[arg(long = "repo-openapi", value_parser = parse_repo_openapi)]
        repo_openapi: Vec<(String, String)>,
        /// Enable best-effort `CodeGraph` symbol and affected-test corroboration.
        #[arg(long)]
        codegraph: bool,
        /// Explicit `CodeGraph` executable path.
        #[arg(long, requires = "codegraph")]
        codegraph_binary: Option<PathBuf>,
        /// Restrict extractor work to one registered repository alias.
        #[arg(long)]
        repo: Option<String>,
        /// Reuse unchanged extractor batches; this is the default behavior.
        #[arg(long, conflicts_with = "force")]
        changed: bool,
        /// Recompute the selected extractor batches.
        #[arg(long)]
        force: bool,
    },
    /// Incrementally synchronize local indexes and publish an atomic snapshot.
    Sync {
        /// Optional workspace name, verified against the manifest.
        workspace: Option<String>,
        /// Workspace manifest path.
        #[arg(long, default_value = "code-system-graph.yaml")]
        config: PathBuf,
        /// `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
        /// Override one repository `OpenAPI` path as `alias=path`.
        #[arg(long = "repo-openapi", value_parser = parse_repo_openapi)]
        repo_openapi: Vec<(String, String)>,
        /// Skip synchronization and corroboration of initialized local `CodeGraph` indexes.
        #[arg(long = "no-codegraph", action = ArgAction::SetFalse, default_value_t = true)]
        codegraph: bool,
        /// Explicit `CodeGraph` executable path.
        #[arg(long)]
        codegraph_binary: Option<PathBuf>,
        /// Restrict synchronization and extractor work to one repository alias.
        #[arg(long)]
        repo: Option<String>,
        /// Recompute selected extractor batches even when fingerprints are unchanged.
        #[arg(long)]
        force: bool,
        /// Continue watching the manifest and repository trees after the initial pass.
        #[arg(long)]
        watch: bool,
        /// Quiet period used to coalesce filesystem events in watch mode.
        #[arg(long, default_value_t = 2_000, requires = "watch", value_parser = clap::value_parser!(u64).range(50..))]
        debounce_ms: u64,
        /// Use portable polling instead of native notifications, at this interval.
        #[arg(long, requires = "watch", value_parser = clap::value_parser!(u64).range(100..))]
        poll_interval_ms: Option<u64>,
    },
    /// Report registry, schema, integrity, and snapshot freshness.
    Status {
        /// Optional workspace name, verified against the manifest.
        workspace: Option<String>,
        /// Workspace manifest path.
        #[arg(long, default_value = "code-system-graph.yaml")]
        config: PathBuf,
        /// `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
    },
    /// Inspect effective workspace configuration without opening a graph database.
    Config {
        /// Configuration inspection operation.
        #[command(subcommand)]
        action: ConfigCommand,
    },
    /// Create a validated online database backup.
    Backup {
        /// Source `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
        /// New backup path; existing files are never overwritten.
        #[arg(long)]
        output: PathBuf,
    },
    /// Restore a backup after preserving the current database.
    Restore {
        /// Destination `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
        /// Validated backup source.
        #[arg(long)]
        input: PathBuf,
        /// Confirm replacement of the destination database.
        #[arg(long)]
        yes: bool,
    },
    /// Plan or apply schema migrations with automatic backup.
    Migrate {
        /// `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
        /// Report required work without modifying the database.
        #[arg(long)]
        dry_run: bool,
    },
    /// Inspect persisted workspace registrations.
    Workspace {
        /// Workspace registry operation.
        #[command(subcommand)]
        action: WorkspaceCommand,
    },
    /// Inspect persisted repository registrations.
    Repo {
        /// Repository registry operation.
        #[command(subcommand)]
        action: RepoCommand,
    },
    /// Trace between two stable node identifiers.
    Trace {
        /// `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
        /// Registered workspace name.
        #[arg(long)]
        workspace: String,
        /// Stable source node identifier.
        #[arg(long)]
        from: String,
        /// Stable target node identifier.
        #[arg(long)]
        to: String,
        /// Maximum number of traversed edges.
        #[arg(long, default_value_t = 8)]
        max_depth: usize,
    },
    /// Search ranked entities in the current federated snapshot.
    Query {
        /// `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
        /// Registered workspace name.
        #[arg(long)]
        workspace: String,
        /// Text matched against graph identities and FTS5.
        question: String,
        /// Zero-based result offset.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Maximum results.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Traverse the current graph with explicit algorithm and safety bounds.
    Traverse {
        /// `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
        /// Registered workspace name.
        #[arg(long)]
        workspace: String,
        /// Stable source node identifier.
        #[arg(long)]
        from: String,
        /// Stable target node identifier.
        #[arg(long)]
        to: String,
        /// `bfs`, `dijkstra`, or `k-shortest`.
        #[arg(long, default_value = "bfs", value_parser = parse_traversal_algorithm)]
        algorithm: TraversalAlgorithm,
        /// `outgoing`, `incoming`, or `both`.
        #[arg(long, default_value = "outgoing", value_parser = parse_traversal_direction)]
        direction: TraversalDirection,
        /// Maximum traversed edges per path.
        #[arg(long, default_value_t = 8)]
        max_depth: usize,
        /// Maximum cross-repository segments per path.
        #[arg(long, default_value_t = 4)]
        max_cross_repo_hops: usize,
        /// Minimum relationship confidence.
        #[arg(long, default_value_t = 0.0)]
        min_confidence: f64,
        /// Maximum returned paths for k-shortest traversal.
        #[arg(long, default_value_t = 1)]
        k: usize,
    },
    /// List, inspect, or compare deterministic graph communities.
    Communities {
        /// `list`, `show`, `compare`, or `recompute`; omitted form preserves flag-based usage.
        action: Option<String>,
        /// Community ID for `show` or historical snapshot ID for `compare`.
        subject: Option<String>,
        /// `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
        /// Registered workspace name.
        #[arg(long)]
        workspace: String,
        /// Optional exact community identity.
        #[arg(long)]
        community_id: Option<String>,
        /// Optional historical snapshot identity to compare.
        #[arg(long)]
        compare_snapshot: Option<String>,
        /// Zero-based result offset.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Maximum communities.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Workspace manifest used by `recompute`.
        #[arg(long, default_value = "code-system-graph.yaml")]
        config: PathBuf,
    },
    /// Analyze conservative cross-repository impact and risk.
    Impact {
        /// `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
        /// Registered workspace name.
        #[arg(long)]
        workspace: String,
        /// Stable graph node identity or stable key.
        #[arg(long)]
        target: String,
        /// Interpret `target` as a stable key instead of a node identity.
        #[arg(long)]
        stable_key: bool,
        /// `upstream`, `downstream`, or `both`.
        #[arg(long, default_value = "upstream", value_parser = parse_impact_direction)]
        direction: ImpactDirection,
        /// Maximum graph traversal depth.
        #[arg(long, default_value_t = 8)]
        max_depth: usize,
        /// Return aggregate impact without detailed item lists.
        #[arg(long)]
        summary_only: bool,
        /// Zero-based detailed-item offset.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Maximum detailed impact items.
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Enrich repository-local impact through public `CodeGraph` contracts.
        #[arg(long)]
        codegraph: bool,
        /// Optional `CodeGraph` executable path.
        #[arg(long)]
        codegraph_binary: Option<OsString>,
    },
    /// Inspect bounded local Git changes without modifying repository state.
    Changes {
        /// `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
        /// Registered workspace name.
        #[arg(long)]
        workspace: String,
        /// Registered repository alias.
        #[arg(long = "repo", visible_alias = "repository")]
        repository: String,
        /// `unstaged`, `staged`, `all`, `compare:<ref>`, `commit:<sha>`, or `range:<base>..<head>`.
        #[arg(long, default_value = "all", value_parser = parse_change_scope)]
        scope: ChangeScope,
        /// Optional Git executable path.
        #[arg(long)]
        git_binary: Option<OsString>,
        /// Impact propagation direction.
        #[arg(long, default_value = "upstream", value_parser = parse_impact_direction)]
        direction: ImpactDirection,
        /// Maximum semantic impact depth per changed boundary.
        #[arg(long, default_value_t = 8)]
        max_depth: usize,
        /// Return aggregate semantic impact without detailed entities.
        #[arg(long)]
        summary_only: bool,
        /// Changed-entity pagination offset.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Maximum changed entities to return.
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Inspect one explicitly enabled GitHub or Bitbucket Cloud pull request.
    PullRequest {
        /// `github` or `bitbucket`.
        #[arg(long, value_parser = parse_pull_request_provider)]
        provider: PullRequestProviderKind,
        /// GitHub owner or Bitbucket workspace.
        #[arg(long)]
        owner: String,
        /// GitHub repository or Bitbucket repository slug.
        #[arg(long)]
        repository: String,
        /// Provider-native pull-request number.
        #[arg(long)]
        number: u64,
        /// Enable the selected remote provider for this invocation.
        #[arg(long)]
        enabled: bool,
        /// Confirm remote API access for this request.
        #[arg(long)]
        consent: bool,
        /// Environment variable containing an optional ephemeral API token.
        #[arg(long)]
        token_env: Option<String>,
        /// Environment variable containing the Atlassian email for Bitbucket API tokens.
        #[arg(long)]
        user_env: Option<String>,
    },
    /// List, show, or compare hosted pull requests.
    Pr {
        /// Pull-request operation.
        #[command(subcommand)]
        command: PrCommand,
    },
    /// List, inspect, validate, compare, or explain contract links.
    Contracts {
        /// `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
        /// Registered workspace name.
        #[arg(long)]
        workspace: String,
        /// Contract operation.
        #[command(subcommand)]
        command: ContractCommand,
    },
    /// Export a deterministic bounded source-free graph.
    Export {
        /// `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
        /// Registered workspace name.
        #[arg(long)]
        workspace: String,
        /// `json`, `graphml`, or `markdown`.
        #[arg(long, value_parser = parse_export_format)]
        format: ExportFormat,
        /// Maximum exported nodes.
        #[arg(long, default_value_t = 10_000)]
        max_nodes: usize,
        /// Maximum exported edges.
        #[arg(long, default_value_t = 50_000)]
        max_edges: usize,
    },
    /// Run conservative configuration, store, and freshness diagnostics.
    Doctor {
        /// Workspace manifest path.
        #[arg(long, default_value = "code-system-graph.yaml")]
        config: PathBuf,
        /// `SQLite` database path.
        #[arg(long, default_value = ".code-system-graph/code-system-graph.db")]
        database: PathBuf,
    },
    /// Write an explicit source-free diagnostic bundle without overwriting.
    Diagnostics {
        /// Workspace manifest path.
        #[arg(long, default_value = "code-system-graph.yaml")]
        config: PathBuf,
        /// `SQLite` database path.
        #[arg(long, default_value = ".code-system-graph/code-system-graph.db")]
        database: PathBuf,
        /// New JSON bundle path.
        #[arg(long)]
        output: PathBuf,
    },
    /// Remove one registered workspace and its persisted snapshots.
    Clean {
        /// `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
        /// Registered workspace name.
        workspace: String,
        /// Confirm destructive workspace removal.
        #[arg(long)]
        force: bool,
    },
    /// Generate shell completion definitions.
    Completions {
        /// Target shell.
        shell: clap_complete::Shell,
    },
    /// Serve optional bounded read-only HTTP delivery.
    Serve {
        /// Workspace manifest path.
        #[arg(long, default_value = "code-system-graph.yaml")]
        config: PathBuf,
        /// `SQLite` database path.
        #[arg(long, default_value = ".code-system-graph/code-system-graph.db")]
        database: PathBuf,
        /// Registered workspace name.
        #[arg(long)]
        workspace: String,
        /// Bind host; non-loopback addresses require a bearer token.
        #[arg(long, default_value = "127.0.0.1")]
        host: IpAddr,
        /// Bind port.
        #[arg(long, default_value_t = 4767)]
        port: u16,
        /// Environment variable containing an ephemeral bearer token.
        #[arg(long)]
        bearer_token_env: Option<String>,
        /// Enable automatic bounded `CodeGraph` enrichment.
        #[arg(long)]
        codegraph: bool,
        /// Explicit `CodeGraph` executable path.
        #[arg(long, requires = "codegraph")]
        codegraph_binary: Option<PathBuf>,
    },
    /// Install, inspect, or remove optional host routing hooks.
    Hooks {
        /// Hook lifecycle operation.
        #[command(subcommand)]
        command: HooksCommand,
    },
    /// Run the read-only MCP server over stdio.
    Mcp {
        /// `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
        /// Registered workspace name.
        #[arg(long)]
        workspace: String,
        /// Enable GitHub pull-request inspection; each request still requires consent.
        #[arg(long)]
        enable_github_pull_requests: bool,
        /// Enable Bitbucket Cloud pull-request inspection; each request still requires consent.
        #[arg(long)]
        enable_bitbucket_pull_requests: bool,
        /// Expose bounded administrative tools for this server process.
        #[arg(long)]
        admin: bool,
        /// Enable automatic bounded `CodeGraph` enrichment and scan corroboration.
        #[arg(long)]
        codegraph: bool,
        /// Explicit `CodeGraph` executable path.
        #[arg(long, requires = "codegraph")]
        codegraph_binary: Option<PathBuf>,
    },
}

const fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Init { .. } => "init",
        Command::Scan { .. } => "scan",
        Command::Sync { .. } => "sync",
        Command::Status { .. } => "status",
        Command::Config { .. } => "config",
        Command::Backup { .. } => "backup",
        Command::Restore { .. } => "restore",
        Command::Migrate { .. } => "migrate",
        Command::Workspace { .. } => "workspace",
        Command::Repo { .. } => "repo",
        Command::Trace { .. } => "trace",
        Command::Query { .. } => "query",
        Command::Traverse { .. } => "traverse",
        Command::Communities { .. } => "communities",
        Command::Impact { .. } => "impact",
        Command::Changes { .. } => "changes",
        Command::PullRequest { .. } => "pull-request",
        Command::Pr { .. } => "pr",
        Command::Contracts { .. } => "contracts",
        Command::Export { .. } => "export",
        Command::Doctor { .. } => "doctor",
        Command::Diagnostics { .. } => "diagnostics",
        Command::Clean { .. } => "clean",
        Command::Completions { .. } => "completions",
        Command::Serve { .. } => "serve",
        Command::Hooks { .. } => "hooks",
        Command::Mcp { .. } => "mcp",
    }
}

fn parse_repo_openapi(value: &str) -> Result<(String, String), String> {
    let (alias, path) = value
        .split_once('=')
        .ok_or_else(|| "expected `alias=path`".to_owned())?;
    if alias.is_empty() || path.is_empty() {
        return Err("both alias and path must be non-empty".to_owned());
    }
    Ok((alias.to_owned(), path.to_owned()))
}

fn codegraph_server_policy(enabled: bool, binary: Option<PathBuf>) -> (bool, Option<OsString>) {
    let environment_binary =
        std::env::var_os("CODE_SYSTEM_GRAPH_CODEGRAPH_BINARY").filter(|value| !value.is_empty());
    let binary = binary.map(PathBuf::into_os_string).or(environment_binary);
    let environment_enabled =
        std::env::var("CODE_SYSTEM_GRAPH_CODEGRAPH").is_ok_and(|value| value.trim() == "1");
    (enabled || environment_enabled || binary.is_some(), binary)
}

fn parse_traversal_algorithm(value: &str) -> Result<TraversalAlgorithm, String> {
    match value {
        "bfs" => Ok(TraversalAlgorithm::Bfs),
        "dijkstra" => Ok(TraversalAlgorithm::Dijkstra),
        "k-shortest" => Ok(TraversalAlgorithm::KShortest),
        _ => Err("expected `bfs`, `dijkstra`, or `k-shortest`".to_owned()),
    }
}

fn parse_traversal_direction(value: &str) -> Result<TraversalDirection, String> {
    match value {
        "outgoing" => Ok(TraversalDirection::Outgoing),
        "incoming" => Ok(TraversalDirection::Incoming),
        "both" => Ok(TraversalDirection::Both),
        _ => Err("expected `outgoing`, `incoming`, or `both`".to_owned()),
    }
}

fn parse_impact_direction(value: &str) -> Result<ImpactDirection, String> {
    match value {
        "upstream" => Ok(ImpactDirection::Upstream),
        "downstream" => Ok(ImpactDirection::Downstream),
        "both" => Ok(ImpactDirection::Both),
        _ => Err("expected `upstream`, `downstream`, or `both`".to_owned()),
    }
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

fn parse_change_scope(value: &str) -> Result<ChangeScope, String> {
    match value {
        "unstaged" => Ok(ChangeScope::Unstaged),
        "staged" => Ok(ChangeScope::Staged),
        "all" => Ok(ChangeScope::All),
        _ => {
            if let Some(reference) = value.strip_prefix("compare:") {
                return Ok(ChangeScope::Compare {
                    reference: reference.to_owned(),
                });
            }
            if let Some(sha) = value.strip_prefix("commit:") {
                return Ok(ChangeScope::Commit {
                    sha: sha.to_owned(),
                });
            }
            if let Some(range) = value.strip_prefix("range:")
                && let Some((base, head)) = range.split_once("..")
            {
                return Ok(ChangeScope::Range {
                    base: base.to_owned(),
                    head: head.to_owned(),
                });
            }
            Err("expected unstaged, staged, all, compare:<ref>, commit:<sha>, or range:<base>..<head>".to_owned())
        }
    }
}

fn parse_pull_request_provider(value: &str) -> Result<PullRequestProviderKind, String> {
    match value {
        "github" => Ok(PullRequestProviderKind::GitHub),
        "bitbucket" | "bitbucket-cloud" => Ok(PullRequestProviderKind::BitbucketCloud),
        _ => Err("expected github or bitbucket".to_owned()),
    }
}

fn parse_pull_request_state(value: &str) -> Result<PullRequestListState, String> {
    match value {
        "open" => Ok(PullRequestListState::Open),
        "closed" => Ok(PullRequestListState::Closed),
        "all" => Ok(PullRequestListState::All),
        _ => Err("expected open, closed, or all".to_owned()),
    }
}

fn parse_hook_host(value: &str) -> Result<HostKind, String> {
    value.parse().map_err(|error| format!("{error}"))
}

fn parse_export_format(value: &str) -> Result<ExportFormat, String> {
    match value {
        "json" => Ok(ExportFormat::Json),
        "graphml" => Ok(ExportFormat::GraphMl),
        "markdown" => Ok(ExportFormat::Markdown),
        _ => Err("expected json, graphml, or markdown".to_owned()),
    }
}

fn contract_request(command: ContractCommand) -> ContractRequest {
    match command {
        ContractCommand::List { limit } => ContractRequest {
            action: ContractAction::List,
            limit,
            ..ContractRequest::default()
        },
        ContractCommand::Show { contract } => ContractRequest {
            action: ContractAction::Show,
            contract: Some(NodeId::new(contract)),
            ..ContractRequest::default()
        },
        ContractCommand::Validate { contract } => ContractRequest {
            action: ContractAction::Validate,
            contract: contract.map(NodeId::new),
            ..ContractRequest::default()
        },
        ContractCommand::Diff {
            contract,
            related_contract,
        } => ContractRequest {
            action: ContractAction::Diff,
            contract: Some(NodeId::new(contract)),
            related_contract: Some(NodeId::new(related_contract)),
            ..ContractRequest::default()
        },
        ContractCommand::ExplainLink {
            contract,
            related_contract,
        } => ContractRequest {
            action: ContractAction::ExplainLink,
            contract: Some(NodeId::new(contract)),
            related_contract: Some(NodeId::new(related_contract)),
            ..ContractRequest::default()
        },
    }
}

async fn handle_pr_show(
    input: PullRequestInput,
    enabled: bool,
    token_env: Option<String>,
    user_env: Option<String>,
) -> anyhow::Result<()> {
    let (token, basic_auth_username) = pull_request_credentials(token_env, user_env)?;
    let cancellation = tokio_util::sync::CancellationToken::new();
    let inspection = inspect_pull_request_with_cancellation(
        &input,
        enabled,
        token,
        basic_auth_username,
        cancellation.clone(),
    );
    tokio::pin!(inspection);
    let envelope = tokio::select! {
        result = &mut inspection => result?,
        signal = shutdown_signal() => {
            signal.context("failed to listen for pull-request cancellation")?;
            cancellation.cancel();
            inspection.await?
        }
    };
    println!("{}", serde_json::to_string(&envelope)?);
    Ok(())
}

fn pull_request_credentials(
    token_env: Option<String>,
    user_env: Option<String>,
) -> anyhow::Result<(Option<String>, Option<String>)> {
    let token = token_env
        .map(|name| {
            std::env::var(&name)
                .with_context(|| format!("token environment variable `{name}` is not set"))
        })
        .transpose()?;
    let basic_auth_username = user_env
        .map(|name| {
            std::env::var(&name)
                .with_context(|| format!("user environment variable `{name}` is not set"))
        })
        .transpose()?;
    Ok((token, basic_auth_username))
}

async fn handle_pr_list(
    input: PullRequestListInput,
    enabled: bool,
    token_env: Option<String>,
    user_env: Option<String>,
) -> anyhow::Result<()> {
    let (token, basic_auth_username) = pull_request_credentials(token_env, user_env)?;
    let cancellation = tokio_util::sync::CancellationToken::new();
    let listing = list_pull_requests(
        &input,
        enabled,
        token,
        basic_auth_username,
        cancellation.clone(),
    );
    tokio::pin!(listing);
    let envelope = tokio::select! {
        result = &mut listing => result?,
        signal = shutdown_signal() => {
            signal.context("failed to listen for pull-request cancellation")?;
            cancellation.cancel();
            listing.await?
        }
    };
    println!("{}", serde_json::to_string(&envelope)?);
    Ok(())
}

fn handle_pr_overlap(left: &std::path::Path, right: &std::path::Path) -> anyhow::Result<()> {
    let left: PullRequestSemanticInput = serde_json::from_slice(
        &std::fs::read(left).with_context(|| format!("failed to read `{}`", left.display()))?,
    )
    .context("failed to parse left semantic pull-request input")?;
    let right: PullRequestSemanticInput = serde_json::from_slice(
        &std::fs::read(right).with_context(|| format!("failed to read `{}`", right.display()))?,
    )
    .context("failed to parse right semantic pull-request input")?;
    let overlap = semantic_pull_request_overlap(&left, &right);
    let order = suggest_pull_request_order(&left, &right);
    println!(
        "{}",
        serde_json::to_string(&PrOverlapReport {
            schema_version: 1,
            overlap,
            order
        })?
    );
    Ok(())
}

fn hook_request(target: HookTarget) -> anyhow::Result<InstallRequest> {
    Ok(InstallRequest {
        root: target.root,
        host: target.host,
        mode: if target.strict {
            HookMode::Strict
        } else {
            HookMode::Advisory
        },
        code_system_graph_binary: target
            .code_system_graph_binary
            .map_or_else(std::env::current_exe, Ok)
            .context("failed to resolve the Code System Graph executable")?,
        database: target.database,
        workspace: target.workspace,
        repository: target.repository,
        codegraph_enabled: target.codegraph,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "Scan delivery keeps each explicit override visible at the CLI boundary"
)]
fn handle_scan(
    config: &std::path::Path,
    database: &std::path::Path,
    workspace: Option<String>,
    pairs: Vec<(String, String)>,
    codegraph: bool,
    codegraph_binary: Option<PathBuf>,
    repository: Option<String>,
    force: bool,
) -> anyhow::Result<()> {
    let mut repo_openapi = BTreeMap::new();
    for (alias, path) in pairs {
        if repo_openapi.insert(alias.clone(), path).is_some() {
            anyhow::bail!("duplicate --repo-openapi override for `{alias}`");
        }
    }
    let summary = scan_workspace_with_overrides(
        config,
        database,
        &ScanOverrides {
            workspace,
            repo_openapi,
            codegraph,
            codegraph_binary,
            repository,
            force,
        },
    )?;
    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "Sync delivery keeps each explicit override visible at the CLI boundary"
)]
async fn handle_sync(
    config: PathBuf,
    database: PathBuf,
    workspace: Option<String>,
    pairs: Vec<(String, String)>,
    codegraph: bool,
    codegraph_binary: Option<PathBuf>,
    repository: Option<String>,
    force: bool,
    watch: bool,
    debounce_ms: u64,
    poll_interval_ms: Option<u64>,
) -> anyhow::Result<()> {
    if !codegraph && codegraph_binary.is_some() {
        anyhow::bail!("--codegraph-binary cannot be combined with --no-codegraph");
    }
    let mut repo_openapi = BTreeMap::new();
    for (alias, path) in pairs {
        if repo_openapi.insert(alias.clone(), path).is_some() {
            anyhow::bail!("duplicate --repo-openapi override for `{alias}`");
        }
    }
    let overrides = ScanOverrides {
        workspace,
        repo_openapi,
        codegraph: false,
        codegraph_binary,
        repository,
        force,
    };
    if watch {
        return sync_watch::watch_workspace(
            config,
            database,
            overrides,
            codegraph,
            std::time::Duration::from_millis(debounce_ms),
            poll_interval_ms.map(std::time::Duration::from_millis),
        )
        .await;
    }
    let summary = sync_workspace_with_overrides(&config, &database, &overrides, codegraph)?;
    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}

#[derive(Debug, Subcommand)]
enum WorkspaceCommand {
    /// List all persisted workspaces.
    List {
        /// `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
    },
    /// Validate and add a workspace manifest to the registry.
    Add {
        /// Workspace name; must match the manifest.
        name: String,
        /// Workspace manifest path.
        #[arg(long)]
        config: PathBuf,
        /// `SQLite` registry database path.
        #[arg(long)]
        database: PathBuf,
    },
    /// Remove one workspace and its persisted snapshot state.
    Remove {
        /// Existing workspace name.
        name: String,
        /// `SQLite` registry database path.
        #[arg(long)]
        database: PathBuf,
        /// Confirm destructive registry removal.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Show configured and built-in native discovery exclusions.
    Show {
        /// Workspace manifest path.
        #[arg(long, default_value = "code-system-graph.yaml")]
        config: PathBuf,
        /// Restrict output to one repository alias.
        #[arg(long)]
        repo: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum RepoCommand {
    /// List repositories registered in one workspace.
    List {
        /// `SQLite` database path.
        #[arg(long)]
        database: PathBuf,
        /// Workspace name.
        #[arg(long)]
        workspace: String,
    },
    /// Add a minimal repository entry to a workspace manifest.
    Add {
        /// Workspace manifest path.
        #[arg(long, default_value = "code-system-graph.yaml")]
        config: PathBuf,
        /// Unique repository alias.
        alias: String,
        /// Repository path relative to the manifest or absolute.
        path: String,
        /// Render the complete proposed manifest without writing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove one repository entry from a workspace manifest.
    Remove {
        /// Workspace manifest path.
        #[arg(long, default_value = "code-system-graph.yaml")]
        config: PathBuf,
        /// Existing repository alias.
        alias: String,
        /// Render the complete proposed manifest without writing.
        #[arg(long)]
        dry_run: bool,
        /// Confirm destructive manifest mutation.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
enum HooksCommand {
    /// Install or update one marker-owned host integration.
    Install(HookTarget),
    /// Inspect one host integration without modifying it.
    Status(HookTarget),
    /// Remove only marker-owned host integration content.
    Uninstall(HookTarget),
}

#[derive(Debug, Subcommand)]
enum ContractCommand {
    /// List contract nodes in stable order.
    List {
        /// Maximum contracts.
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    /// Show one exact contract and direct links.
    Show {
        /// Stable contract node identifier.
        contract: String,
    },
    /// Validate all contracts or one exact contract.
    Validate {
        /// Optional stable contract node identifier.
        contract: Option<String>,
    },
    /// Compare two exact contract nodes structurally.
    Diff {
        /// Previous contract node identifier.
        contract: String,
        /// Candidate contract node identifier.
        related_contract: String,
    },
    /// Explain direct graph links between two contracts.
    ExplainLink {
        /// First contract node identifier.
        contract: String,
        /// Related contract node identifier.
        related_contract: String,
    },
}

#[derive(Debug, Subcommand)]
enum PrCommand {
    /// List one bounded provider page without source or patches.
    List {
        /// `github` or `bitbucket`.
        #[arg(long, value_parser = parse_pull_request_provider)]
        provider: PullRequestProviderKind,
        /// GitHub owner or Bitbucket workspace.
        #[arg(long)]
        owner: String,
        /// GitHub repository or Bitbucket repository slug.
        #[arg(long)]
        repository: String,
        /// `open`, `closed`, or `all`.
        #[arg(long, default_value = "all", value_parser = parse_pull_request_state)]
        state: PullRequestListState,
        /// Opaque page cursor returned by a prior list response.
        #[arg(long)]
        cursor: Option<String>,
        /// Maximum pull-request summaries.
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// Enable the selected remote provider for this invocation.
        #[arg(long)]
        enabled: bool,
        /// Confirm remote API access for this request.
        #[arg(long)]
        consent: bool,
        /// Environment variable containing an optional ephemeral API token.
        #[arg(long)]
        token_env: Option<String>,
        /// Environment variable containing the Atlassian email for Bitbucket API tokens.
        #[arg(long)]
        user_env: Option<String>,
    },
    /// Inspect one explicitly enabled hosted pull request.
    Show {
        /// `github` or `bitbucket`.
        #[arg(long, value_parser = parse_pull_request_provider)]
        provider: PullRequestProviderKind,
        /// GitHub owner or Bitbucket workspace.
        #[arg(long)]
        owner: String,
        /// GitHub repository or Bitbucket repository slug.
        #[arg(long)]
        repository: String,
        /// Provider-native pull-request number.
        #[arg(long)]
        number: u64,
        /// Enable the selected remote provider for this invocation.
        #[arg(long)]
        enabled: bool,
        /// Confirm remote API access for this request.
        #[arg(long)]
        consent: bool,
        /// Environment variable containing an optional ephemeral API token.
        #[arg(long)]
        token_env: Option<String>,
        /// Environment variable containing the Atlassian email for Bitbucket API tokens.
        #[arg(long)]
        user_env: Option<String>,
    },
    /// Compare two source-free semantic PR inputs from JSON files.
    Overlap {
        /// First `PullRequestSemanticInput` JSON document.
        #[arg(long)]
        left: PathBuf,
        /// Second `PullRequestSemanticInput` JSON document.
        #[arg(long)]
        right: PathBuf,
    },
}

#[derive(Debug, Serialize)]
struct PrOverlapReport {
    schema_version: u32,
    overlap: PullRequestOverlap,
    order: PullRequestOrderSuggestion,
}

#[derive(Debug, clap::Args)]
struct HookTarget {
    /// Repository worktree root.
    #[arg(long, default_value = ".")]
    root: PathBuf,
    /// `claude-code`, `codex`, `gemini`, `antigravity`, or `cursor`.
    #[arg(long, value_parser = parse_hook_host)]
    host: HostKind,
    /// Enable the explicit fail-closed staged pre-commit gate.
    #[arg(long)]
    strict: bool,
    /// Allow routing guidance to recommend the MCP `explore` tool.
    #[arg(long)]
    codegraph: bool,
    /// `Code System Graph` executable path; defaults to the running executable.
    #[arg(long = "csgraph-binary")]
    code_system_graph_binary: Option<PathBuf>,
    /// `SQLite` database used by strict staged analysis.
    #[arg(long, default_value = ".code-system-graph/code-system-graph.db")]
    database: PathBuf,
    /// Registered workspace name.
    #[arg(long)]
    workspace: String,
    /// Registered repository alias for this worktree.
    #[arg(long)]
    repository: String,
}

fn handle_workspace_command(action: WorkspaceCommand) -> anyhow::Result<()> {
    match action {
        WorkspaceCommand::List { database } => {
            let workspaces = list_workspace_registry(&database)?;
            println!("{}", serde_json::to_string(&workspaces)?);
        }
        WorkspaceCommand::Add {
            name,
            config,
            database,
        } => {
            let summary = add_workspace_to_registry(&database, &name, &config)?;
            println!("{}", serde_json::to_string(&summary)?);
        }
        WorkspaceCommand::Remove {
            name,
            database,
            yes,
        } => {
            if !yes {
                anyhow::bail!("workspace remove requires explicit --yes confirmation");
            }
            let summary = remove_workspace_from_registry(&database, &name)?;
            println!("{}", serde_json::to_string(&summary)?);
        }
    }
    Ok(())
}

fn handle_repo_command(action: RepoCommand) -> anyhow::Result<()> {
    match action {
        RepoCommand::List {
            database,
            workspace,
        } => {
            let repositories = list_repository_registry(&database, &workspace)?;
            println!("{}", serde_json::to_string(&repositories)?);
        }
        RepoCommand::Add {
            config,
            alias,
            path,
            dry_run,
        } => {
            let summary = add_repository_to_manifest(&config, &alias, &path, dry_run)?;
            println!("{}", serde_json::to_string(&summary)?);
        }
        RepoCommand::Remove {
            config,
            alias,
            dry_run,
            yes,
        } => {
            if !dry_run && !yes {
                anyhow::bail!("repo remove requires explicit --yes confirmation");
            }
            let summary = remove_repository_from_manifest(&config, &alias, dry_run)?;
            println!("{}", serde_json::to_string(&summary)?);
        }
    }
    Ok(())
}

fn handle_query(
    database: &std::path::Path,
    workspace: &str,
    question: String,
    offset: usize,
    limit: usize,
) -> anyhow::Result<()> {
    let envelope = search_workspace(
        database,
        workspace,
        &SearchInput {
            query: question,
            node_kinds: Vec::new(),
            repo_ids: Vec::new(),
            service_ids: Vec::new(),
            community_ids: Vec::new(),
            offset,
            limit,
        },
    )?;
    println!("{}", serde_json::to_string(&envelope)?);
    Ok(())
}

fn handle_trace(
    database: &std::path::Path,
    workspace: &str,
    from: String,
    to: String,
    max_depth: usize,
) -> anyhow::Result<()> {
    let envelope = trace_workspace(
        database,
        workspace,
        &TraceInput {
            from,
            to,
            max_depth,
        },
    )?;
    println!("{}", serde_json::to_string(&envelope)?);
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "CLI traversal flags remain explicit at the delivery boundary"
)]
fn handle_traverse(
    database: &std::path::Path,
    workspace: &str,
    from: String,
    to: String,
    algorithm: TraversalAlgorithm,
    direction: TraversalDirection,
    max_depth: usize,
    max_cross_repo_hops: usize,
    min_confidence: f64,
    k: usize,
) -> anyhow::Result<()> {
    let envelope = traverse_workspace(
        database,
        workspace,
        &TraversalRequest {
            start: NodeId::new(from),
            target: NodeId::new(to),
            filters: TraversalFilters {
                min_confidence,
                ..TraversalFilters::default()
            },
            options: TraversalOptions {
                algorithm,
                direction,
                max_depth,
                max_cross_repo_hops,
                k,
                ..TraversalOptions::default()
            },
        },
    )?;
    println!("{}", serde_json::to_string(&envelope)?);
    Ok(())
}

fn handle_communities(
    database: &std::path::Path,
    workspace: &str,
    input: &CommunityInput,
) -> anyhow::Result<()> {
    let envelope = communities_workspace(database, workspace, input)?;
    println!("{}", serde_json::to_string(&envelope)?);
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "Community compatibility syntax maps positional actions and existing bounded flags"
)]
fn handle_community_command(
    database: &std::path::Path,
    workspace: &str,
    action: Option<&str>,
    subject: Option<String>,
    community_id: Option<String>,
    compare_snapshot: Option<String>,
    offset: usize,
    limit: usize,
    config: &std::path::Path,
) -> anyhow::Result<()> {
    if action == Some("recompute") {
        if subject.is_some() || community_id.is_some() || compare_snapshot.is_some() {
            anyhow::bail!("communities recompute does not accept selection arguments");
        }
        let summary = scan_workspace_with_overrides(config, database, &ScanOverrides::default())?;
        if summary.workspace != workspace {
            anyhow::bail!(
                "workspace name `{workspace}` does not match manifest name `{}`",
                summary.workspace
            );
        }
        println!("{}", serde_json::to_string(&summary)?);
        return Ok(());
    }
    let (community_id, compare_snapshot_id) = match action {
        None | Some("list") => (community_id, compare_snapshot),
        Some("show") => (
            Some(
                subject
                    .ok_or_else(|| anyhow::anyhow!("communities show requires a community ID"))?,
            ),
            None,
        ),
        Some("compare") => (
            None,
            Some(
                subject
                    .ok_or_else(|| anyhow::anyhow!("communities compare requires a snapshot ID"))?,
            ),
        ),
        Some(other) => {
            anyhow::bail!(
                "unknown communities action `{other}`; expected list, show, compare, or recompute"
            )
        }
    };
    handle_communities(
        database,
        workspace,
        &CommunityInput {
            community_id: community_id.map(code_system_graph_model::CommunityId::new),
            compare_snapshot_id,
            offset,
            limit,
        },
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "CLI impact controls remain explicit at the delivery boundary"
)]
async fn handle_impact(
    database: &std::path::Path,
    workspace: &str,
    target: String,
    stable_key: bool,
    direction: ImpactDirection,
    max_depth: usize,
    summary_only: bool,
    offset: usize,
    limit: usize,
    codegraph: bool,
    codegraph_binary: Option<OsString>,
) -> anyhow::Result<()> {
    let target = if stable_key {
        ImpactTarget::StableKey(target)
    } else {
        ImpactTarget::NodeId(NodeId::new(target))
    };
    let request = ImpactRequest {
        target,
        direction,
        options: ImpactOptions {
            max_depth,
            summary_only,
            offset,
            limit,
            ..ImpactOptions::default()
        },
    };
    let envelope = if codegraph {
        impact_workspace_with_codegraph(database, workspace, &request, codegraph_binary).await?
    } else {
        impact_workspace(database, workspace, &request)?
    };
    println!("{}", serde_json::to_string(&envelope)?);
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        let exit_code = error
            .downcast_ref::<ApplicationError>()
            .map_or(ExitCode::Internal, application_exit_code);
        eprintln!("{error:#}");
        std::process::exit(i32::from(exit_code.value()));
    }
}

async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let diagnostics_enabled = !cli.quiet
        && (cli.verbose > 0
            || std::env::var("CODE_SYSTEM_GRAPH_DEBUG").is_ok_and(|value| value.trim() == "1"));
    let command = command_name(&cli.command);
    let correlation_id = correlation_id();
    let started = Instant::now();
    if diagnostics_enabled {
        log_command_event(
            cli.log_format,
            "command_started",
            command,
            &correlation_id,
            None,
            None,
        );
    }
    let log_format = cli.log_format;
    let result = dispatch(cli).await;
    if diagnostics_enabled {
        log_command_event(
            log_format,
            "command_finished",
            command,
            &correlation_id,
            Some(started.elapsed().as_millis()),
            Some(result.is_ok()),
        );
    }
    result
}

fn correlation_id() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_micros());
    format!("rm-{}-{timestamp}", std::process::id())
}

fn log_command_event(
    format: LogFormat,
    event: &str,
    command: &str,
    correlation_id: &str,
    elapsed_ms: Option<u128>,
    success: Option<bool>,
) {
    match format {
        LogFormat::Text => eprintln!(
            "event={event} command={command} correlation_id={correlation_id} elapsed_ms={} success={}",
            elapsed_ms.map_or_else(|| "-".to_owned(), |value| value.to_string()),
            success.map_or_else(|| "-".to_owned(), |value| value.to_string())
        ),
        LogFormat::Json => eprintln!(
            "{}",
            serde_json::json!({
                "event": event,
                "command": command,
                "correlation_id": correlation_id,
                "elapsed_ms": elapsed_ms,
                "success": success
            })
        ),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "Top-level exhaustive command dispatch keeps every public CLI operation visible"
)]
async fn dispatch(cli: Cli) -> anyhow::Result<()> {
    match cli.command {
        Command::Init { path, name } => {
            let report = initialize_workspace(&path, name.as_deref())?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::Scan {
            workspace,
            config,
            database,
            repo_openapi,
            codegraph,
            codegraph_binary,
            repo,
            changed: _,
            force,
        } => handle_scan(
            &config,
            &database,
            workspace,
            repo_openapi,
            codegraph,
            codegraph_binary,
            repo,
            force,
        )?,
        Command::Sync {
            workspace,
            config,
            database,
            repo_openapi,
            codegraph,
            codegraph_binary,
            repo,
            force,
            watch,
            debounce_ms,
            poll_interval_ms,
        } => {
            handle_sync(
                config,
                database,
                workspace,
                repo_openapi,
                codegraph,
                codegraph_binary,
                repo,
                force,
                watch,
                debounce_ms,
                poll_interval_ms,
            )
            .await?;
        }
        Command::Status {
            workspace,
            config,
            database,
        } => {
            let status = status_workspace(&config, &database)?;
            if let Some(workspace) = workspace
                && workspace != status.workspace
            {
                anyhow::bail!(
                    "workspace name `{workspace}` does not match manifest name `{}`",
                    status.workspace
                );
            }
            println!("{}", serde_json::to_string(&status)?);
        }
        Command::Config {
            action: ConfigCommand::Show { config, repo },
        } => {
            let report = show_config(&config, repo.as_deref())?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::Backup { database, output } => {
            let summary = backup_database(&database, &output)?;
            println!("{}", serde_json::to_string(&summary)?);
        }
        Command::Restore {
            database,
            input,
            yes,
        } => {
            if !yes {
                anyhow::bail!("restore requires explicit --yes confirmation");
            }
            let summary = restore_database(&database, &input)?;
            println!("{}", serde_json::to_string(&summary)?);
        }
        Command::Migrate { database, dry_run } => {
            let summary = migrate_database(&database, dry_run)?;
            println!("{}", serde_json::to_string(&summary)?);
        }
        Command::Workspace { action } => handle_workspace_command(action)?,
        Command::Repo { action } => handle_repo_command(action)?,
        Command::Trace {
            database,
            workspace,
            from,
            to,
            max_depth,
        } => handle_trace(&database, &workspace, from, to, max_depth)?,
        Command::Query {
            database,
            workspace,
            question,
            offset,
            limit,
        } => handle_query(&database, &workspace, question, offset, limit)?,
        Command::Traverse {
            database,
            workspace,
            from,
            to,
            algorithm,
            direction,
            max_depth,
            max_cross_repo_hops,
            min_confidence,
            k,
        } => handle_traverse(
            &database,
            &workspace,
            from,
            to,
            algorithm,
            direction,
            max_depth,
            max_cross_repo_hops,
            min_confidence,
            k,
        )?,
        Command::Communities {
            action,
            subject,
            database,
            workspace,
            community_id,
            compare_snapshot,
            offset,
            limit,
            config,
        } => handle_community_command(
            &database,
            &workspace,
            action.as_deref(),
            subject,
            community_id,
            compare_snapshot,
            offset,
            limit,
            &config,
        )?,
        Command::Impact {
            database,
            workspace,
            target,
            stable_key,
            direction,
            max_depth,
            summary_only,
            offset,
            limit,
            codegraph,
            codegraph_binary,
        } => {
            handle_impact(
                &database,
                &workspace,
                target,
                stable_key,
                direction,
                max_depth,
                summary_only,
                offset,
                limit,
                codegraph,
                codegraph_binary,
            )
            .await?;
        }
        Command::Changes {
            database,
            workspace,
            repository,
            scope,
            git_binary,
            direction,
            max_depth,
            summary_only,
            offset,
            limit,
        } => {
            let cancellation = tokio_util::sync::CancellationToken::new();
            let input = ChangesInput { repository, scope };
            let options = ChangeAnalysisOptions {
                direction,
                max_depth,
                summary_only,
                offset,
                limit,
                ..ChangeAnalysisOptions::default()
            };
            let analysis = analyze_workspace_changes_with_cancellation(
                &database,
                &workspace,
                &input,
                &options,
                git_binary,
                &cancellation,
            );
            tokio::pin!(analysis);
            let envelope = tokio::select! {
                result = &mut analysis => result?,
                signal = shutdown_signal() => {
                    signal.context("failed to listen for change-analysis cancellation")?;
                    cancellation.cancel();
                    analysis.await?
                }
            };
            println!("{}", serde_json::to_string(&envelope)?);
        }
        Command::PullRequest {
            provider,
            owner,
            repository,
            number,
            enabled,
            consent,
            token_env,
            user_env,
        } => {
            handle_pr_show(
                PullRequestInput {
                    provider,
                    owner,
                    repository,
                    number,
                    consent_to_remote_access: consent,
                },
                enabled,
                token_env,
                user_env,
            )
            .await?;
        }
        Command::Pr { command } => match command {
            PrCommand::List {
                provider,
                owner,
                repository,
                state,
                cursor,
                limit,
                enabled,
                consent,
                token_env,
                user_env,
            } => {
                handle_pr_list(
                    PullRequestListInput {
                        provider,
                        owner,
                        repository,
                        state,
                        cursor,
                        limit,
                        consent_to_remote_access: consent,
                    },
                    enabled,
                    token_env,
                    user_env,
                )
                .await?;
            }
            PrCommand::Show {
                provider,
                owner,
                repository,
                number,
                enabled,
                consent,
                token_env,
                user_env,
            } => {
                handle_pr_show(
                    PullRequestInput {
                        provider,
                        owner,
                        repository,
                        number,
                        consent_to_remote_access: consent,
                    },
                    enabled,
                    token_env,
                    user_env,
                )
                .await?;
            }
            PrCommand::Overlap { left, right } => {
                handle_pr_overlap(&left, &right)?;
            }
        },
        Command::Contracts {
            database,
            workspace,
            command,
        } => {
            let report = contracts_workspace(&database, &workspace, &contract_request(command))?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::Export {
            database,
            workspace,
            format,
            max_nodes,
            max_edges,
        } => {
            let report = export_workspace(
                &database,
                &workspace,
                &ExportRequest {
                    format,
                    max_nodes,
                    max_edges,
                },
            )?;
            print!("{}", report.content);
        }
        Command::Doctor { config, database } => {
            let report = doctor_workspace(&config, &database)?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::Diagnostics {
            config,
            database,
            output,
        } => {
            let report = create_diagnostic_bundle(&config, &database, &output)?;
            println!("{}", serde_json::to_string(&report)?);
        }
        Command::Clean {
            database,
            workspace,
            force,
        } => {
            if !force {
                anyhow::bail!("clean requires --force because it deletes all workspace snapshots");
            }
            let summary = remove_workspace_from_registry(&database, &workspace)?;
            println!("{}", serde_json::to_string(&summary)?);
        }
        Command::Completions { shell } => {
            let mut command = Cli::command();
            clap_complete::generate(shell, &mut command, "csgraph", &mut std::io::stdout());
        }
        Command::Hooks { command } => match command {
            HooksCommand::Install(target) => {
                let report = install_hook(&hook_request(target)?)?;
                println!("{}", serde_json::to_string(&report)?);
            }
            HooksCommand::Status(target) => {
                let report = hook_status(&hook_request(target)?)?;
                println!("{}", serde_json::to_string(&report)?);
            }
            HooksCommand::Uninstall(target) => {
                let report = uninstall_hook(&hook_request(target)?)?;
                println!("{}", serde_json::to_string(&report)?);
            }
        },
        Command::Serve {
            config,
            database,
            workspace,
            host,
            port,
            bearer_token_env,
            codegraph,
            codegraph_binary,
        } => {
            let (codegraph, codegraph_binary) =
                codegraph_server_policy(codegraph, codegraph_binary);
            let mut server_config = HttpServerConfig::new(config, database, workspace)
                .with_bind(SocketAddr::new(host, port))
                .with_codegraph(codegraph, codegraph_binary);
            if let Some(name) = bearer_token_env {
                let value = std::env::var(&name).with_context(|| {
                    format!("bearer-token environment variable `{name}` is not set")
                })?;
                server_config = server_config.with_bearer_token(BearerToken::new(value)?);
            }
            let cancellation = tokio_util::sync::CancellationToken::new();
            let server = serve_http(server_config, cancellation.clone());
            tokio::pin!(server);
            tokio::select! {
                result = &mut server => result?,
                signal = shutdown_signal() => {
                    signal.context("failed to listen for HTTP shutdown signal")?;
                    cancellation.cancel();
                    server.await?;
                }
            }
        }
        Command::Mcp {
            database,
            workspace,
            enable_github_pull_requests,
            enable_bitbucket_pull_requests,
            admin,
            codegraph,
            codegraph_binary,
        } => {
            let admin = admin
                || std::env::var("CODE_SYSTEM_GRAPH_MCP_ADMIN")
                    .is_ok_and(|value| value.trim() == "1");
            let (codegraph, codegraph_binary) =
                codegraph_server_policy(codegraph, codegraph_binary);
            let service = CodeSystemGraphServer::new(database, workspace)
                .with_pull_request_providers(
                    enable_github_pull_requests,
                    enable_bitbucket_pull_requests,
                )
                .with_codegraph(codegraph, codegraph_binary)
                .with_admin_profile(admin)
                .serve(rmcp::transport::stdio())
                .await
                .context("failed to initialize MCP stdio service")?;
            service
                .waiting()
                .await
                .context("MCP stdio service failed")?;
        }
    }
    Ok(())
}

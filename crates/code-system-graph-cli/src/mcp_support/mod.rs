use std::collections::{BTreeMap, BTreeSet};
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use code_system_graph_core::{
    ChangeImpactReport, ContractAction, ContractReport, ContractRequest, ImpactReport, ImpactRequest, ManualLinkConfig, PullRequestInspection, SearchReport, inspect_contracts, public_schema_catalog
};
use code_system_graph_model::{
    CommunityId, Evidence, FreshnessSummary, Node, NodeId, OverallFreshness, RepoFreshness, RepoFreshnessState, ToolEnvelope, ToolStatus, TraceReport
};
use code_system_graph_store_sqlite::SqliteStore;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{
    ChangesInput, CommunityInput, CommunityReport, ExploreInput, ExploreReport, ManifestMutationSummary, PullRequestInput, ScanSummary, SearchInput, TraceInput
};

mod resources;

pub(super) use resources::{ResourceErrorKind, read_resource, resource_templates, resource_uris};

pub(super) const ADMIN_TOOL_NAMES: [&str; 5] = [
    "scan",
    "update_workspace",
    "write_manual_link",
    "clean_cache",
    "recompute_communities",
];
pub(super) const MARKDOWN_MIME_TYPE: &str = "text/markdown";

const MAX_PAGE_SIZE: usize = 100;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct WorkspaceInput {
    /// Workspace selected when the MCP server was constructed.
    pub workspace: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct WorkspaceUpdateInput {
    /// Workspace selected when the MCP server was constructed.
    pub workspace: String,
    /// Action-specific manifest mutation.
    #[serde(flatten)]
    operation: WorkspaceUpdateOperation,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum WorkspaceUpdateOperation {
    /// Add one minimal repository registration.
    AddRepository {
        /// Unique repository alias.
        alias: String,
        /// Repository path relative to the workspace manifest.
        repository_path: String,
    },
    /// Remove one repository registration.
    RemoveRepository {
        /// Existing repository alias.
        alias: String,
    },
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct ManualLinkWriteInput {
    /// Workspace selected when the MCP server was constructed.
    pub workspace: String,
    /// Action-specific relationship mutation.
    #[serde(flatten)]
    operation: ManualLinkWriteOperation,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum ManualLinkWriteOperation {
    /// Add one exact manual relationship.
    Add {
        /// Exact source node identifier or stable key.
        from: String,
        /// Exact target node identifier or stable key.
        to: String,
        /// Concrete graph relation to create.
        relation: code_system_graph_model::EdgeKind,
        /// Optional contract identity retained as audit context.
        #[serde(default)]
        contract: Option<String>,
        /// Required operator rationale.
        reason: String,
    },
    /// Suppress one exact automatically derived relationship.
    Suppress {
        /// Exact source node identifier or stable key.
        from: String,
        /// Exact target node identifier or stable key.
        to: String,
        /// Concrete graph relation to suppress.
        relation: code_system_graph_model::EdgeKind,
        /// Optional contract identity retained as audit context.
        #[serde(default)]
        contract: Option<String>,
        /// Required operator rationale.
        reason: String,
    },
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CacheCleanInput {
    /// Workspace selected when the MCP server was constructed.
    pub workspace: String,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct ContractsInput {
    /// Workspace selected when the MCP server was constructed.
    pub workspace: String,
    /// Action-specific contract operation.
    #[serde(flatten)]
    operation: ContractsOperation,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum ContractsOperation {
    /// List contracts in stable order.
    List {
        /// Bounded result size.
        #[serde(default = "default_page_size")]
        #[schemars(range(min = 1, max = 100))]
        limit: usize,
    },
    /// Show one contract and its direct relationships.
    Show {
        /// Exact contract node.
        contract: NodeId,
    },
    /// Validate all contracts.
    ValidateAll {
        /// Bounded result size.
        #[serde(default = "default_page_size")]
        #[schemars(range(min = 1, max = 100))]
        limit: usize,
    },
    /// Validate one exact contract.
    ValidateOne {
        /// Exact contract node.
        contract: NodeId,
    },
    /// Compare two exact contracts.
    Diff {
        /// Primary contract node.
        contract: NodeId,
        /// Comparison contract node.
        related_contract: NodeId,
    },
    /// Explain a direct relationship between two exact contracts.
    ExplainLink {
        /// Source contract node.
        contract: NodeId,
        /// Related contract node.
        related_contract: NodeId,
    },
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub(super) struct CommunitiesInput {
    /// Workspace selected when the MCP server was constructed.
    pub workspace: String,
    /// Action-specific community operation.
    #[serde(flatten)]
    operation: CommunitiesOperation,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum CommunitiesOperation {
    /// List current communities in stable order.
    List {
        /// Zero-based result offset.
        #[serde(default)]
        offset: usize,
        /// Bounded result size.
        #[serde(default = "default_page_size")]
        #[schemars(range(min = 1, max = 100))]
        limit: usize,
    },
    /// Show one exact current community.
    Show {
        /// Exact community identifier.
        community_id: CommunityId,
    },
    /// Compare the current communities with one historical snapshot.
    Compare {
        /// Historical snapshot identifier.
        snapshot_id: String,
        /// Zero-based result offset.
        #[serde(default)]
        offset: usize,
        /// Bounded result size.
        #[serde(default = "default_page_size")]
        #[schemars(range(min = 1, max = 100))]
        limit: usize,
    },
}

impl WorkspaceUpdateInput {
    pub(super) fn workspace(&self) -> &str {
        &self.workspace
    }

    pub(super) fn alias(&self) -> &str {
        match &self.operation {
            WorkspaceUpdateOperation::AddRepository { alias, .. }
            | WorkspaceUpdateOperation::RemoveRepository { alias } => alias,
        }
    }

    pub(super) fn repository_path(&self) -> Option<&str> {
        match &self.operation {
            WorkspaceUpdateOperation::AddRepository {
                repository_path, ..
            } => Some(repository_path),
            WorkspaceUpdateOperation::RemoveRepository { .. } => None,
        }
    }
}

impl ManualLinkWriteInput {
    pub(super) fn workspace(&self) -> &str {
        &self.workspace
    }

    pub(super) fn declaration(&self) -> ManualLinkConfig {
        let (from, to, relation, contract, reason, suppress) = match &self.operation {
            ManualLinkWriteOperation::Add {
                from,
                to,
                relation,
                contract,
                reason,
                ..
            } => (from, to, relation, contract, reason, false),
            ManualLinkWriteOperation::Suppress {
                from,
                to,
                relation,
                contract,
                reason,
                ..
            } => (from, to, relation, contract, reason, true),
        };
        ManualLinkConfig {
            from: from.clone(),
            to: to.clone(),
            relation: *relation,
            contract: contract.clone(),
            reason: reason.clone(),
            suppress,
        }
    }
}

impl ContractsInput {
    pub(super) fn workspace(&self) -> &str {
        &self.workspace
    }

    fn request(&self) -> ContractRequest {
        match &self.operation {
            ContractsOperation::List { limit } => ContractRequest {
                action: ContractAction::List,
                limit: *limit,
                ..ContractRequest::default()
            },
            ContractsOperation::Show { contract } => ContractRequest {
                action: ContractAction::Show,
                contract: Some(contract.clone()),
                ..ContractRequest::default()
            },
            ContractsOperation::ValidateAll { limit } => ContractRequest {
                action: ContractAction::Validate,
                limit: *limit,
                ..ContractRequest::default()
            },
            ContractsOperation::ValidateOne { contract } => ContractRequest {
                action: ContractAction::Validate,
                contract: Some(contract.clone()),
                ..ContractRequest::default()
            },
            ContractsOperation::Diff {
                contract,
                related_contract,
            } => ContractRequest {
                action: ContractAction::Diff,
                contract: Some(contract.clone()),
                related_contract: Some(related_contract.clone()),
                ..ContractRequest::default()
            },
            ContractsOperation::ExplainLink {
                contract,
                related_contract,
            } => ContractRequest {
                action: ContractAction::ExplainLink,
                contract: Some(contract.clone()),
                related_contract: Some(related_contract.clone()),
                ..ContractRequest::default()
            },
        }
    }
}

impl CommunitiesInput {
    pub(super) fn workspace(&self) -> &str {
        &self.workspace
    }

    pub(super) fn application_input(&self) -> CommunityInput {
        match &self.operation {
            CommunitiesOperation::List { offset, limit } => CommunityInput {
                community_id: None,
                compare_snapshot_id: None,
                offset: *offset,
                limit: *limit,
            },
            CommunitiesOperation::Show { community_id } => CommunityInput {
                community_id: Some(community_id.clone()),
                compare_snapshot_id: None,
                offset: 0,
                limit: 1,
            },
            CommunitiesOperation::Compare {
                snapshot_id,
                offset,
                limit,
            } => CommunityInput {
                community_id: None,
                compare_snapshot_id: Some(snapshot_id.clone()),
                offset: *offset,
                limit: *limit,
            },
        }
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct SourceContextInput {
    /// Workspace selected when the MCP server was constructed.
    pub workspace: String,
    /// Stable graph node identifier whose persisted context is requested.
    pub node_id: NodeId,
    /// Maximum number of evidence metadata records.
    #[serde(default = "default_evidence_limit")]
    #[schemars(range(min = 1, max = 100))]
    pub evidence_limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub(super) struct SnapshotMetrics {
    pub snapshot_id: String,
    pub node_count: usize,
    pub edge_count: usize,
    pub evidence_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub(super) struct GraphStatusReport {
    pub workspace: String,
    pub schema_version: i64,
    pub integrity_ok: bool,
    pub snapshot: SnapshotMetrics,
    pub repositories: Vec<RepoFreshness>,
}

#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub(super) struct SourceContextReport {
    pub workspace: String,
    pub entity: Node,
    pub related_entities: Vec<Node>,
    pub evidence: Vec<Evidence>,
    pub total_evidence: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub(super) struct AdminAudit<T> {
    pub schema_version: u32,
    pub operation: String,
    pub workspace: String,
    pub mutated: bool,
    pub audit_persisted: bool,
    pub result: T,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub(super) struct ManifestAdminReport {
    pub mutation: ManifestMutationSummary,
    pub scan: ScanSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub(super) struct CacheCleanReport {
    pub removed_entries: usize,
}

pub(super) fn status_envelope(
    database_path: &Path,
    configured_workspace: &str,
    requested_workspace: &str,
) -> ToolEnvelope<GraphStatusReport> {
    if let Err(message) = validate_workspace(configured_workspace, requested_workspace) {
        return error_envelope("Workspace policy rejected the request.", message);
    }
    match load_status(database_path, configured_workspace) {
        Ok((report, freshness)) => {
            let status = if report.integrity_ok && freshness.overall == OverallFreshness::Fresh {
                ToolStatus::Ok
            } else {
                ToolStatus::Degraded
            };
            ToolEnvelope {
                schema_version: 2,
                status,
                data: Some(report),
                freshness,
                warnings: Vec::new(),
            }
        }
        Err(error) => error_envelope("Workspace status could not be loaded.", error),
    }
}

pub(super) fn contracts_envelope(
    database_path: &Path,
    configured_workspace: &str,
    input: &ContractsInput,
) -> ToolEnvelope<ContractReport> {
    if let Err(message) = validate_workspace(configured_workspace, input.workspace()) {
        return error_envelope("Workspace policy rejected the request.", message);
    }
    let result = (|| {
        let store =
            SqliteStore::open_read_only(database_path).map_err(|error| error.to_string())?;
        let (nodes, edges) = store
            .load_current_graph(configured_workspace)
            .map_err(|error| error.to_string())?;
        let evidence = store
            .load_current_evidence(configured_workspace)
            .map_err(|error| error.to_string())?;
        let freshness = freshness_summary(
            &store
                .load_current_freshness(configured_workspace)
                .map_err(|error| error.to_string())?,
        );
        let report = inspect_contracts(&nodes, &edges, &evidence, &[], &input.request())
            .map_err(|error| error.to_string())?;
        Ok::<_, String>((report, freshness))
    })();
    match result {
        Ok((report, freshness)) => {
            let status = if !report.complete || freshness.overall != OverallFreshness::Fresh {
                ToolStatus::Degraded
            } else {
                ToolStatus::Ok
            };
            ToolEnvelope {
                schema_version: 2,
                status,
                data: Some(report),
                freshness,
                warnings: Vec::new(),
            }
        }
        Err(error) => error_envelope("Contracts could not be loaded.", error),
    }
}

pub(super) fn source_context_envelope(
    database_path: &Path,
    configured_workspace: &str,
    input: &SourceContextInput,
) -> ToolEnvelope<SourceContextReport> {
    if let Err(message) = validate_workspace(configured_workspace, &input.workspace) {
        return error_envelope("Workspace policy rejected the request.", message);
    }
    if !(1..=MAX_PAGE_SIZE).contains(&input.evidence_limit) {
        return error_envelope(
            "Source-context bounds are invalid.",
            "evidence_limit must be between 1 and 100",
        );
    }
    let result = (|| {
        let store =
            SqliteStore::open_read_only(database_path).map_err(|error| error.to_string())?;
        let (nodes, edges) = store
            .load_current_graph(configured_workspace)
            .map_err(|error| error.to_string())?;
        let entity = nodes
            .iter()
            .find(|node| node.id == input.node_id)
            .cloned()
            .ok_or_else(|| format!("node `{}` was not found", input.node_id.as_str()))?;
        let related_ids = edges
            .iter()
            .filter_map(|edge| {
                if edge.source == input.node_id {
                    Some(edge.target.clone())
                } else if edge.target == input.node_id {
                    Some(edge.source.clone())
                } else {
                    None
                }
            })
            .collect::<BTreeSet<_>>();
        let related_entities = nodes
            .into_iter()
            .filter(|node| related_ids.contains(&node.id))
            .take(MAX_PAGE_SIZE)
            .collect::<Vec<_>>();
        let evidence_ids = edges
            .iter()
            .filter(|edge| edge.source == input.node_id || edge.target == input.node_id)
            .flat_map(|edge| edge.evidence.iter().cloned())
            .collect::<BTreeSet<_>>();
        let selected = store
            .load_current_evidence(configured_workspace)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|item| evidence_ids.contains(&item.id))
            .collect::<Vec<_>>();
        let total_evidence = selected.len();
        let evidence = selected
            .into_iter()
            .take(input.evidence_limit)
            .collect::<Vec<_>>();
        let freshness = freshness_summary(
            &store
                .load_current_freshness(configured_workspace)
                .map_err(|error| error.to_string())?,
        );
        Ok::<_, String>((
            SourceContextReport {
                workspace: configured_workspace.to_owned(),
                entity,
                related_entities,
                evidence,
                total_evidence,
                truncated: total_evidence > input.evidence_limit,
            },
            freshness,
        ))
    })();
    match result {
        Ok((report, freshness)) => {
            let status = if report.truncated || freshness.overall != OverallFreshness::Fresh {
                ToolStatus::Degraded
            } else {
                ToolStatus::Ok
            };
            ToolEnvelope {
                schema_version: 2,
                status,
                data: Some(report),
                freshness,
                warnings: Vec::new(),
            }
        }
        Err(error) => error_envelope("Persisted source context could not be loaded.", error),
    }
}

pub(super) fn admin_audit_envelope(
    database_path: &Path,
    workspace: &str,
    operation: &str,
    summary: ScanSummary,
) -> ToolEnvelope<AdminAudit<ScanSummary>> {
    let snapshot_id = summary.snapshot_id.clone();
    admin_mutation_envelope(database_path, workspace, operation, &snapshot_id, summary)
}

pub(super) fn admin_mutation_envelope<T>(
    database_path: &Path,
    workspace: &str,
    operation: &str,
    snapshot_id: &str,
    result: T,
) -> ToolEnvelope<AdminAudit<T>> {
    let audit = persist_admin_audit(database_path, workspace, operation, snapshot_id);
    let audit_persisted = audit.is_ok();
    let warnings = audit.err().into_iter().collect();
    let freshness = SqliteStore::open_read_only(database_path)
        .and_then(|store| store.load_current_freshness(workspace))
        .map_or_else(
            |error| FreshnessSummary {
                overall: OverallFreshness::Unknown,
                stale_repositories: Vec::new(),
                reasons: vec![error.to_string()],
            },
            |items| freshness_summary(&items),
        );
    ToolEnvelope {
        schema_version: 2,
        status: if freshness.overall == OverallFreshness::Fresh && audit_persisted {
            ToolStatus::Ok
        } else {
            ToolStatus::Degraded
        },
        data: Some(AdminAudit {
            schema_version: 2,
            operation: operation.to_owned(),
            workspace: workspace.to_owned(),
            mutated: true,
            audit_persisted,
            result,
        }),
        freshness,
        warnings,
    }
}

fn persist_admin_audit(
    database_path: &Path,
    workspace: &str,
    operation: &str,
    snapshot_id: &str,
) -> Result<(), String> {
    let parent = database_path
        .parent()
        .ok_or_else(|| "database path has no parent for the administrative audit log".to_owned())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create audit directory: {error}"))?;
    let path = parent.join("admin-audit.jsonl");
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options
        .open(&path)
        .map_err(|error| format!("failed to open administrative audit log: {error}"))?;
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("failed to timestamp administrative audit entry: {error}"))?
        .as_secs();
    let entry = json!({
        "schema_version": 2,
        "timestamp_unix": timestamp,
        "workspace": workspace,
        "operation": operation,
        "snapshot_id": snapshot_id
    });
    serde_json::to_writer(&mut file, &entry)
        .map_err(|error| format!("failed to serialize administrative audit entry: {error}"))?;
    file.write_all(b"\n")
        .and_then(|()| file.sync_data())
        .map_err(|error| format!("failed to persist administrative audit entry: {error}"))
}

pub(super) fn configured_manifest_path(
    database_path: &Path,
    workspace: &str,
) -> Result<PathBuf, String> {
    let store = SqliteStore::open_read_only(database_path).map_err(|error| error.to_string())?;
    let registry = store
        .load_workspace_registry(workspace)
        .map_err(|error| error.to_string())?;
    let path = registry
        .config_path
        .ok_or_else(|| format!("workspace `{workspace}` has no registered manifest path"))?;
    Ok(native_path(&path))
}

pub(super) fn schema_catalog() -> Value {
    let mut schemas = BTreeMap::new();
    insert_schema::<TraceInput>(&mut schemas, "trace.input");
    insert_schema::<ToolEnvelope<TraceReport>>(&mut schemas, "trace.result");
    insert_schema::<SearchInput>(&mut schemas, "query.input");
    insert_schema::<ToolEnvelope<SearchReport>>(&mut schemas, "query.result");
    insert_schema::<CommunitiesInput>(&mut schemas, "communities.input");
    insert_schema::<ToolEnvelope<CommunityReport>>(&mut schemas, "communities.result");
    insert_schema::<ExploreInput>(&mut schemas, "explore.input");
    insert_schema::<ToolEnvelope<ExploreReport>>(&mut schemas, "explore.result");
    insert_schema::<ImpactRequest>(&mut schemas, "impact.input");
    insert_schema::<ToolEnvelope<ImpactReport>>(&mut schemas, "impact.result");
    insert_schema::<ChangesInput>(&mut schemas, "analyze_changes.input");
    insert_schema::<ToolEnvelope<ChangeImpactReport>>(&mut schemas, "analyze_changes.result");
    insert_schema::<PullRequestInput>(&mut schemas, "analyze_pull_request.input");
    insert_schema::<ToolEnvelope<PullRequestInspection>>(
        &mut schemas,
        "analyze_pull_request.result",
    );
    insert_schema::<WorkspaceInput>(&mut schemas, "status.input");
    insert_schema::<ToolEnvelope<GraphStatusReport>>(&mut schemas, "status.result");
    insert_schema::<ContractsInput>(&mut schemas, "contracts.input");
    insert_schema::<ToolEnvelope<ContractReport>>(&mut schemas, "contracts.result");
    insert_schema::<SourceContextInput>(&mut schemas, "source_context.input");
    insert_schema::<ToolEnvelope<SourceContextReport>>(&mut schemas, "source_context.result");
    insert_schema::<WorkspaceInput>(&mut schemas, "scan.input");
    insert_schema::<ToolEnvelope<AdminAudit<ScanSummary>>>(&mut schemas, "scan.result");
    insert_schema::<WorkspaceUpdateInput>(&mut schemas, "update_workspace.input");
    insert_schema::<ToolEnvelope<AdminAudit<ManifestAdminReport>>>(
        &mut schemas,
        "update_workspace.result",
    );
    insert_schema::<ManualLinkWriteInput>(&mut schemas, "write_manual_link.input");
    insert_schema::<ToolEnvelope<AdminAudit<ManifestAdminReport>>>(
        &mut schemas,
        "write_manual_link.result",
    );
    insert_schema::<CacheCleanInput>(&mut schemas, "clean_cache.input");
    insert_schema::<ToolEnvelope<AdminAudit<CacheCleanReport>>>(&mut schemas, "clean_cache.result");
    insert_schema::<WorkspaceInput>(&mut schemas, "recompute_communities.input");
    insert_schema::<ToolEnvelope<AdminAudit<ScanSummary>>>(
        &mut schemas,
        "recompute_communities.result",
    );
    let application_interfaces = public_schema_catalog().map_or_else(
        |error| {
            json!({
                "schema_version": 2,
                "error": error.to_string()
            })
        },
        |catalog| {
            serde_json::to_value(catalog).unwrap_or_else(|error| {
                json!({
                    "schema_version": 2,
                    "error": error.to_string()
                })
            })
        },
    );
    json!({
        "schema_version": 2,
        "media_type": "application/schema+json",
        "schemas": schemas,
        "application_interfaces": application_interfaces
    })
}

fn load_status(
    database_path: &Path,
    workspace: &str,
) -> Result<(GraphStatusReport, FreshnessSummary), String> {
    let store = SqliteStore::open_read_only(database_path).map_err(|error| error.to_string())?;
    if !store
        .workspace_exists(workspace)
        .map_err(|error| error.to_string())?
    {
        return Err(format!("workspace `{workspace}` is not registered"));
    }
    let repositories = store
        .load_current_freshness(workspace)
        .map_err(|error| error.to_string())?;
    let freshness = freshness_summary(&repositories);
    let snapshot = store
        .current_snapshot_summary(workspace)
        .map_err(|error| error.to_string())?;
    Ok((
        GraphStatusReport {
            workspace: workspace.to_owned(),
            schema_version: store.schema_version().map_err(|error| error.to_string())?,
            integrity_ok: store.integrity_check().map_err(|error| error.to_string())?,
            snapshot: SnapshotMetrics {
                snapshot_id: snapshot.snapshot_id,
                node_count: snapshot.node_count,
                edge_count: snapshot.edge_count,
                evidence_count: snapshot.evidence_count,
            },
            repositories,
        },
        freshness,
    ))
}

fn freshness_summary(repositories: &[RepoFreshness]) -> FreshnessSummary {
    let overall = if repositories.is_empty() {
        OverallFreshness::Unknown
    } else if repositories
        .iter()
        .all(|item| item.state == RepoFreshnessState::Fresh)
    {
        OverallFreshness::Fresh
    } else if repositories.iter().any(|item| {
        matches!(
            item.state,
            RepoFreshnessState::Unavailable
                | RepoFreshnessState::Unknown
                | RepoFreshnessState::Corrupt
        )
    }) {
        OverallFreshness::Partial
    } else {
        OverallFreshness::Stale
    };
    let stale_repositories = repositories
        .iter()
        .filter(|item| item.state != RepoFreshnessState::Fresh)
        .map(|item| item.repo_id.clone())
        .collect();
    let reasons = repositories
        .iter()
        .filter_map(|item| item.reason.clone())
        .collect();
    FreshnessSummary {
        overall,
        stale_repositories,
        reasons,
    }
}

fn validate_workspace(configured: &str, requested: &str) -> Result<(), String> {
    if requested == configured {
        Ok(())
    } else {
        Err(format!(
            "workspace `{requested}` is outside this server's configured `{configured}` policy"
        ))
    }
}

fn insert_schema<T: JsonSchema>(catalog: &mut BTreeMap<String, Value>, name: &str) {
    catalog.insert(name.to_owned(), json!(schema_for!(T)));
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

fn default_page_size() -> usize {
    50
}

fn default_evidence_limit() -> usize {
    20
}

#[cfg(unix)]
fn native_path(path: &code_system_graph_model::NativePath) -> PathBuf {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    match path.encoding {
        code_system_graph_model::NativePathEncoding::UnixBytes
        | code_system_graph_model::NativePathEncoding::Utf8 => {
            PathBuf::from(OsString::from_vec(path.bytes.clone()))
        }
        code_system_graph_model::NativePathEncoding::WindowsWide => PathBuf::from(&path.display),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn administrative_audit_should_be_append_only_and_source_free()
    -> Result<(), Box<dyn std::error::Error>> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("graph.db");
        SqliteStore::open(&database)?;

        persist_admin_audit(&database, "workspace", "scan", "snapshot:one")?;
        persist_admin_audit(&database, "workspace", "scan", "snapshot:two")?;
        let source = std::fs::read_to_string(temporary.path().join("admin-audit.jsonl"))?;
        let lines = source.lines().collect::<Vec<_>>();

        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("snapshot:one"));
        assert!(lines[1].contains("snapshot:two"));
        assert!(!source.contains("source_body"));
        Ok(())
    }
}

#[cfg(windows)]
fn native_path(path: &code_system_graph_model::NativePath) -> PathBuf {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    match path.encoding {
        code_system_graph_model::NativePathEncoding::WindowsWide => {
            let wide = path
                .bytes
                .chunks_exact(2)
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                .collect::<Vec<_>>();
            PathBuf::from(OsString::from_wide(&wide))
        }
        code_system_graph_model::NativePathEncoding::UnixBytes
        | code_system_graph_model::NativePathEncoding::Utf8 => PathBuf::from(&path.display),
    }
}

#[cfg(not(any(unix, windows)))]
fn native_path(path: &code_system_graph_model::NativePath) -> PathBuf {
    PathBuf::from(&path.display)
}

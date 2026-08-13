//! Typed, bounded MCP resource contracts and Markdown delivery.

use std::path::Path;

use code_system_graph_core::CONTRACT_NODE_KINDS;
#[cfg(test)]
use code_system_graph_model::RepoFreshnessState;
use code_system_graph_model::{
    Community, CommunityAlgorithm, CommunityConfig, CommunityEdgeWeight, CommunityId, CommunityLabelEvidence, CommunityMetrics, CommunityScope, Evidence, ExtractorRun, FreshnessSummary, Node, NodeId, NodeKind, OverallFreshness, RepoFreshness, RepoId, RepositoryRecord
};
use code_system_graph_store_sqlite::{SqliteStore, StoreError};
use serde_json::{Value, json};

#[cfg(test)]
use super::SnapshotMetrics;
use super::{
    GraphStatusReport, freshness_summary, load_status, schema_catalog, validate_workspace
};
use crate::agent_markdown::MarkdownDocument;

#[derive(Debug)]
struct BoundedCollection<T> {
    total: usize,
    items: Vec<T>,
}

impl<T> BoundedCollection<T> {
    fn retained(&self) -> usize {
        self.items.len()
    }

    fn truncated(&self) -> bool {
        self.total > self.retained()
    }
}

fn bounded_collection<T>(
    items: impl IntoIterator<Item = T>,
    item_limit: usize,
) -> BoundedCollection<T> {
    let items = items.into_iter().collect::<Vec<_>>();
    let total = items.len();
    BoundedCollection {
        total,
        items: items.into_iter().take(item_limit).collect(),
    }
}

#[derive(Debug)]
struct RepositoryView {
    id: String,
    checkout_id: String,
    alias: String,
    normalized_remote: Option<String>,
    head_commit: Option<String>,
    linked_worktree: bool,
    working_tree_dirty: bool,
}

#[derive(Debug)]
struct WorkspaceResourceItem {
    name: String,
    configured: bool,
}

#[derive(Debug)]
struct WorkspacesResource {
    schema_version: u32,
    workspaces: BoundedCollection<WorkspaceResourceItem>,
}

#[derive(Debug)]
struct OverviewSnapshot {
    id: String,
    nodes: usize,
    edges: usize,
    evidence: usize,
}

#[derive(Debug)]
struct OverviewResource {
    schema_version: u32,
    workspace: String,
    snapshot: OverviewSnapshot,
}

#[derive(Debug)]
struct FreshnessResource {
    overall: OverallFreshness,
    stale_repositories: BoundedCollection<RepoId>,
    reasons: BoundedCollection<String>,
}

#[derive(Debug)]
struct StatusResource {
    schema_version: u32,
    status: GraphStatusReport,
    repositories: BoundedCollection<RepoFreshness>,
    freshness: FreshnessResource,
}

#[derive(Debug)]
struct RepositoriesResource {
    schema_version: u32,
    workspace: String,
    repositories: BoundedCollection<RepositoryView>,
}

#[derive(Debug)]
struct EntitiesResource {
    schema_version: u32,
    workspace: String,
    entities: BoundedCollection<Node>,
}

#[derive(Debug)]
struct CommunityConfigView {
    algorithm: CommunityAlgorithm,
    scope: CommunityScope,
    seed: u64,
    resolution: f64,
    minimum_confidence: f32,
    edge_weights: BoundedCollection<CommunityEdgeWeight>,
    max_iterations: u32,
}

#[derive(Debug)]
struct CommunityView {
    id: CommunityId,
    label: String,
    members: BoundedCollection<NodeId>,
    central_nodes: BoundedCollection<NodeId>,
    repositories: BoundedCollection<RepoId>,
    services: BoundedCollection<NodeId>,
    inbound_contracts: BoundedCollection<NodeId>,
    outbound_contracts: BoundedCollection<NodeId>,
    metrics: CommunityMetrics,
    label_evidence: BoundedCollection<CommunityLabelEvidence>,
    limitations: BoundedCollection<String>,
}

#[derive(Debug)]
struct CommunitiesResource {
    schema_version: u32,
    workspace: String,
    snapshot_id: String,
    engine_version: String,
    config: CommunityConfigView,
    communities: BoundedCollection<CommunityView>,
}

#[derive(Debug)]
struct CoverageResource {
    schema_version: u32,
    workspace: String,
    runs: BoundedCollection<ExtractorRun>,
    freshness: FreshnessResource,
}

#[derive(Debug)]
struct EvidenceResource {
    schema_version: u32,
    workspace: String,
    evidence: Evidence,
}

enum ResourceDocument {
    Workspaces(WorkspacesResource),
    Overview(OverviewResource),
    Status(StatusResource),
    Repositories(RepositoriesResource),
    Services(EntitiesResource),
    Contracts(EntitiesResource),
    Communities(CommunitiesResource),
    Coverage(CoverageResource),
    Evidence(EvidenceResource),
    SchemaCatalog(Value),
}

impl ResourceDocument {
    fn render(&self, maximum: usize) -> String {
        match self {
            Self::Workspaces(resource) => render_workspaces(resource, maximum),
            Self::Overview(resource) => render_overview(resource, maximum),
            Self::Status(resource) => render_status(resource, maximum),
            Self::Repositories(resource) => render_repositories(resource, maximum),
            Self::Services(resource) => render_entities("Workspace services", resource, maximum),
            Self::Contracts(resource) => render_entities("Workspace contracts", resource, maximum),
            Self::Communities(resource) => render_communities(resource, maximum),
            Self::Coverage(resource) => render_coverage(resource, maximum),
            Self::Evidence(resource) => render_evidence(resource, maximum),
            Self::SchemaCatalog(resource) => {
                crate::agent_markdown::render_schema_catalog(resource, maximum)
            }
        }
    }
}

fn render_workspaces(resource: &WorkspacesResource, maximum: usize) -> String {
    let mut document = MarkdownDocument::resource("Configured workspaces", resource.schema_version);
    render_collection_items(
        &mut document,
        "Workspaces",
        &resource.workspaces,
        |document, index, workspace| {
            document.text(&format!("Workspace {} name", index + 1), &workspace.name);
            document.scalar(
                &format!("Workspace {} configured", index + 1),
                workspace.configured,
            );
        },
    );
    document.render(maximum)
}

fn render_overview(resource: &OverviewResource, maximum: usize) -> String {
    let mut document = MarkdownDocument::resource("Workspace overview", resource.schema_version);
    document.text("Workspace", &resource.workspace);
    document.text("Snapshot", &resource.snapshot.id);
    document.scalar("Nodes", resource.snapshot.nodes);
    document.scalar("Edges", resource.snapshot.edges);
    document.scalar("Evidence", resource.snapshot.evidence);
    document.render(maximum)
}

fn render_status(resource: &StatusResource, maximum: usize) -> String {
    let mut document = MarkdownDocument::resource("Workspace status", resource.schema_version);
    document.text("Workspace", &resource.status.workspace);
    document.scalar("Database schema", resource.status.schema_version);
    document.scalar("Integrity ok", resource.status.integrity_ok);
    document.debug("Snapshot", &resource.status.snapshot);
    render_collection(&mut document, "Repositories", &resource.repositories);
    render_freshness(&mut document, &resource.freshness);
    document.render(maximum)
}

fn render_repositories(resource: &RepositoriesResource, maximum: usize) -> String {
    let mut document =
        MarkdownDocument::resource("Workspace repositories", resource.schema_version);
    document.text("Workspace", &resource.workspace);
    render_collection_items(
        &mut document,
        "Repositories",
        &resource.repositories,
        |document, index, repository| {
            let prefix = format!("Repository {}", index + 1);
            document.text(&format!("{prefix} id"), &repository.id);
            document.text(&format!("{prefix} checkout"), &repository.checkout_id);
            document.text(&format!("{prefix} alias"), &repository.alias);
            document.debug(&format!("{prefix} remote"), &repository.normalized_remote);
            document.debug(&format!("{prefix} head"), &repository.head_commit);
            document.scalar(
                &format!("{prefix} linked worktree"),
                repository.linked_worktree,
            );
            document.scalar(&format!("{prefix} dirty"), repository.working_tree_dirty);
        },
    );
    document.render(maximum)
}

fn render_entities(name: &str, resource: &EntitiesResource, maximum: usize) -> String {
    let mut document = MarkdownDocument::resource(name, resource.schema_version);
    document.text("Workspace", &resource.workspace);
    render_collection(&mut document, "Entities", &resource.entities);
    document.render(maximum)
}

fn render_communities(resource: &CommunitiesResource, maximum: usize) -> String {
    let mut document = MarkdownDocument::resource("Workspace communities", resource.schema_version);
    document.text("Workspace", &resource.workspace);
    document.text("Snapshot", &resource.snapshot_id);
    document.text("Engine version", &resource.engine_version);
    document.debug("Algorithm", &resource.config.algorithm);
    document.debug("Scope", &resource.config.scope);
    document.scalar("Seed", resource.config.seed);
    document.scalar("Resolution", resource.config.resolution);
    document.scalar("Minimum confidence", resource.config.minimum_confidence);
    document.scalar("Maximum iterations", resource.config.max_iterations);
    render_collection(
        &mut document,
        "Configuration edge weights",
        &resource.config.edge_weights,
    );
    render_collection_items(
        &mut document,
        "Communities",
        &resource.communities,
        |document, index, community| {
            let prefix = format!("Community {}", index + 1);
            document.debug(&format!("{prefix} identity"), &community.id);
            document.text(&format!("{prefix} label"), &community.label);
            document.debug(&format!("{prefix} metrics"), &community.metrics);
            render_collection(document, &format!("{prefix} members"), &community.members);
            render_collection(
                document,
                &format!("{prefix} central nodes"),
                &community.central_nodes,
            );
            render_collection(
                document,
                &format!("{prefix} repositories"),
                &community.repositories,
            );
            render_collection(document, &format!("{prefix} services"), &community.services);
            render_collection(
                document,
                &format!("{prefix} inbound contracts"),
                &community.inbound_contracts,
            );
            render_collection(
                document,
                &format!("{prefix} outbound contracts"),
                &community.outbound_contracts,
            );
            render_collection(
                document,
                &format!("{prefix} label evidence"),
                &community.label_evidence,
            );
            render_collection(
                document,
                &format!("{prefix} limitations"),
                &community.limitations,
            );
        },
    );
    document.render(maximum)
}

fn render_coverage(resource: &CoverageResource, maximum: usize) -> String {
    let mut document = MarkdownDocument::resource("Workspace coverage", resource.schema_version);
    document.text("Workspace", &resource.workspace);
    render_collection(&mut document, "Extractor runs", &resource.runs);
    render_freshness(&mut document, &resource.freshness);
    document.render(maximum)
}

fn render_evidence(resource: &EvidenceResource, maximum: usize) -> String {
    let mut document = MarkdownDocument::resource("Evidence metadata", resource.schema_version);
    document.text("Workspace", &resource.workspace);
    document.debug("Evidence", &resource.evidence);
    document.render(maximum)
}

fn render_freshness(document: &mut MarkdownDocument, freshness: &FreshnessResource) {
    document.debug("Freshness overall", &freshness.overall);
    render_collection(
        document,
        "Freshness stale repositories",
        &freshness.stale_repositories,
    );
    render_collection(document, "Freshness reasons", &freshness.reasons);
}

fn render_collection<T: std::fmt::Debug>(
    document: &mut MarkdownDocument,
    name: &str,
    collection: &BoundedCollection<T>,
) {
    document.bounded_collection(
        name,
        collection.total,
        collection.retained(),
        collection.truncated(),
        &collection.items,
    );
}

fn render_collection_items<T>(
    document: &mut MarkdownDocument,
    name: &str,
    collection: &BoundedCollection<T>,
    mut render_item: impl FnMut(&mut MarkdownDocument, usize, &T),
) {
    let items = collection
        .items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let mut fragment = MarkdownDocument::fragment();
            render_item(&mut fragment, index, item);
            fragment.into_complete()
        })
        .collect();
    document.bounded_fragments(name, collection.total, collection.truncated(), items);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceErrorKind {
    Invalid,
    Missing,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResourceError {
    pub kind: ResourceErrorKind,
    pub message: String,
}
pub(crate) fn resource_uris(workspace: &str) -> Vec<(String, String, String)> {
    let prefix = format!("code-system-graph://workspace/{workspace}");
    vec![
        (
            "code-system-graph://workspaces".to_owned(),
            "workspaces".to_owned(),
            "Configured workspace catalog.".to_owned(),
        ),
        (
            format!("{prefix}/overview"),
            "workspace-overview".to_owned(),
            "Current immutable snapshot counts.".to_owned(),
        ),
        (
            format!("{prefix}/status"),
            "workspace-status".to_owned(),
            "Persisted workspace integrity and freshness.".to_owned(),
        ),
        (
            format!("{prefix}/repositories"),
            "workspace-repositories".to_owned(),
            "Credential-free repository registration metadata.".to_owned(),
        ),
        (
            format!("{prefix}/services"),
            "workspace-services".to_owned(),
            "Bounded service entity catalog.".to_owned(),
        ),
        (
            format!("{prefix}/contracts"),
            "workspace-contracts".to_owned(),
            "Bounded public contract entity catalog.".to_owned(),
        ),
        (
            format!("{prefix}/communities"),
            "workspace-communities".to_owned(),
            "Bounded deterministic community catalog.".to_owned(),
        ),
        (
            format!("{prefix}/schema"),
            "schema-catalog".to_owned(),
            "Versioned JSON Schema catalog for MCP tool inputs and results.".to_owned(),
        ),
        (
            format!("{prefix}/coverage"),
            "workspace-coverage".to_owned(),
            "Bounded extractor and freshness coverage metadata.".to_owned(),
        ),
    ]
}

pub(crate) fn resource_templates() -> Vec<(String, String, String)> {
    vec![(
        "code-system-graph://evidence/{id}".to_owned(),
        "evidence-metadata".to_owned(),
        "One bounded evidence metadata record selected by stable ID.".to_owned(),
    )]
}

pub(crate) fn read_resource(
    database_path: &Path,
    workspace: &str,
    uri: &str,
    policy: &code_system_graph_core::ExecutionPolicy,
) -> Result<String, ResourceError> {
    let item_limit = usize::try_from(policy.max_mcp_resource_items)
        .expect("validated policy count is usize-representable");
    let resource = if uri == "code-system-graph://workspaces" {
        let workspaces = bounded_collection(
            [WorkspaceResourceItem {
                name: workspace.to_owned(),
                configured: true,
            }],
            item_limit,
        );
        ResourceDocument::Workspaces(WorkspacesResource {
            schema_version: 2,
            workspaces,
        })
    } else if let Some(path) = uri.strip_prefix("code-system-graph://workspace/") {
        let (requested, resource) = path.split_once('/').ok_or_else(|| invalid_resource(uri))?;
        validate_workspace(workspace, requested).map_err(|message| ResourceError {
            kind: ResourceErrorKind::Invalid,
            message,
        })?;
        read_workspace_resource(database_path, workspace, resource, item_limit)?
    } else if let Some(evidence_id) = uri.strip_prefix("code-system-graph://evidence/") {
        read_evidence_resource(database_path, workspace, evidence_id)?
    } else {
        return Err(invalid_resource(uri));
    };
    let maximum = if uri.ends_with("/schema") {
        usize::try_from(policy.max_mcp_schema_catalog_bytes)
            .expect("validated policy bytes are usize-representable")
    } else {
        usize::try_from(policy.max_mcp_resource_bytes)
            .expect("validated policy bytes are usize-representable")
    };
    Ok(resource.render(maximum))
}

fn read_workspace_resource(
    database_path: &Path,
    workspace: &str,
    resource: &str,
    item_limit: usize,
) -> Result<ResourceDocument, ResourceError> {
    if resource == "schema" {
        return Ok(ResourceDocument::SchemaCatalog(bounded_schema_catalog(
            item_limit,
        )));
    }
    let store = SqliteStore::open_read_only(database_path).map_err(internal_store)?;
    match resource {
        "overview" => {
            let summary = store
                .current_snapshot_summary(workspace)
                .map_err(internal_store)?;
            Ok(ResourceDocument::Overview(OverviewResource {
                schema_version: 2,
                workspace: workspace.to_owned(),
                snapshot: OverviewSnapshot {
                    id: summary.snapshot_id,
                    nodes: summary.node_count,
                    edges: summary.edge_count,
                    evidence: summary.evidence_count,
                },
            }))
        }
        "status" => workspace_status_resource(database_path, workspace, item_limit),
        "repositories" => {
            let registry = store
                .load_workspace_registry(workspace)
                .map_err(internal_store)?;
            let repositories = bounded_collection(
                registry.repositories.iter().map(repository_view),
                item_limit,
            );
            Ok(ResourceDocument::Repositories(RepositoriesResource {
                schema_version: 2,
                workspace: workspace.to_owned(),
                repositories,
            }))
        }
        "services" => node_resource(&store, workspace, item_limit, |kind| {
            kind == NodeKind::Service
        })
        .map(ResourceDocument::Services),
        "contracts" => node_resource(&store, workspace, item_limit, is_contract)
            .map(ResourceDocument::Contracts),
        "communities" => {
            let snapshot = store
                .load_current_community_snapshot(workspace)
                .map_err(internal_store)?;
            let communities = bounded_collection(
                snapshot
                    .communities
                    .into_iter()
                    .map(|community| community_view(community, item_limit)),
                item_limit,
            );
            Ok(ResourceDocument::Communities(CommunitiesResource {
                schema_version: 2,
                workspace: workspace.to_owned(),
                snapshot_id: snapshot.snapshot_id,
                engine_version: snapshot.engine_version,
                config: community_config_view(snapshot.config, item_limit),
                communities,
            }))
        }
        "coverage" => workspace_coverage_resource(&store, workspace, item_limit),
        _ => Err(ResourceError {
            kind: ResourceErrorKind::Missing,
            message: format!("workspace resource `{resource}` was not found"),
        }),
    }
}

fn bounded_schema_catalog(item_limit: usize) -> Value {
    let mut catalog = schema_catalog();
    let Some(root) = catalog.as_object_mut() else {
        return catalog;
    };
    let (total, retained_count) = {
        let Some(schemas) = root.get_mut("schemas").and_then(Value::as_object_mut) else {
            return catalog;
        };
        let total = schemas.len();
        let retained = schemas
            .iter()
            .take(item_limit)
            .map(|(name, schema)| (name.clone(), schema.clone()))
            .collect::<serde_json::Map<_, _>>();
        *schemas = retained;
        (total, schemas.len())
    };
    root.insert("schema_total".to_owned(), json!(total));
    root.insert("schema_retained".to_owned(), json!(retained_count));
    root.insert(
        "schemas_truncated".to_owned(),
        json!(total > retained_count),
    );
    catalog
}

fn workspace_status_resource(
    database_path: &Path,
    workspace: &str,
    item_limit: usize,
) -> Result<ResourceDocument, ResourceError> {
    let (status, freshness) = load_status(database_path, workspace).map_err(internal_message)?;
    Ok(ResourceDocument::Status(status_resource_value(
        status, freshness, item_limit,
    )))
}

fn status_resource_value(
    mut status: GraphStatusReport,
    freshness: FreshnessSummary,
    item_limit: usize,
) -> StatusResource {
    let repositories = bounded_collection(std::mem::take(&mut status.repositories), item_limit);
    StatusResource {
        schema_version: 2,
        status,
        repositories,
        freshness: bounded_freshness(freshness, item_limit),
    }
}

fn workspace_coverage_resource(
    store: &SqliteStore,
    workspace: &str,
    item_limit: usize,
) -> Result<ResourceDocument, ResourceError> {
    let runs = store
        .load_current_extractor_runs(workspace)
        .map_err(internal_store)?;
    let freshness = store
        .load_current_freshness(workspace)
        .map_err(internal_store)?;
    Ok(ResourceDocument::Coverage(coverage_resource_value(
        workspace,
        runs,
        freshness_summary(&freshness),
        item_limit,
    )))
}

fn coverage_resource_value(
    workspace: &str,
    runs: Vec<ExtractorRun>,
    freshness: FreshnessSummary,
    item_limit: usize,
) -> CoverageResource {
    let runs = bounded_collection(runs, item_limit);
    CoverageResource {
        schema_version: 2,
        workspace: workspace.to_owned(),
        runs,
        freshness: bounded_freshness(freshness, item_limit),
    }
}

fn read_evidence_resource(
    database_path: &Path,
    workspace: &str,
    evidence_id: &str,
) -> Result<ResourceDocument, ResourceError> {
    if evidence_id.is_empty() || evidence_id == "{id}" {
        return Err(ResourceError {
            kind: ResourceErrorKind::Invalid,
            message: "a concrete evidence identifier is required".to_owned(),
        });
    }
    let store = SqliteStore::open_read_only(database_path).map_err(internal_store)?;
    let evidence = store
        .load_current_evidence_by_id(workspace, evidence_id)
        .map_err(|error| match error {
            StoreError::CurrentSnapshotMissing(_) => ResourceError {
                kind: ResourceErrorKind::Missing,
                message: format!("workspace `{workspace}` has no evidence snapshot"),
            },
            other => internal_store(other),
        })?
        .ok_or_else(|| ResourceError {
            kind: ResourceErrorKind::Missing,
            message: format!("evidence `{evidence_id}` was not found in workspace `{workspace}`"),
        })?;
    Ok(ResourceDocument::Evidence(EvidenceResource {
        schema_version: 2,
        workspace: workspace.to_owned(),
        evidence,
    }))
}

fn node_resource(
    store: &SqliteStore,
    workspace: &str,
    item_limit: usize,
    predicate: impl Fn(NodeKind) -> bool,
) -> Result<EntitiesResource, ResourceError> {
    let all_kinds = [
        NodeKind::Repository,
        NodeKind::Artifact,
        NodeKind::Service,
        NodeKind::Package,
        NodeKind::SymbolRef,
        NodeKind::TestCase,
        NodeKind::HttpOperation,
        NodeKind::GraphqlOperation,
        NodeKind::RpcMethod,
        NodeKind::EventChannel,
        NodeKind::EventSchema,
        NodeKind::Database,
        NodeKind::DatabaseTable,
        NodeKind::DatabaseColumn,
        NodeKind::ConfigKey,
        NodeKind::Deployment,
        NodeKind::Document,
        NodeKind::Adr,
        NodeKind::Owner,
        NodeKind::ChangeSet,
        NodeKind::PullRequest,
        NodeKind::Community,
    ];
    let kinds = all_kinds
        .into_iter()
        .filter(|kind| predicate(*kind))
        .collect::<Vec<_>>();
    let (total, items) = store
        .load_current_nodes_by_kinds(workspace, &kinds, item_limit)
        .map_err(internal_store)?;
    let entities = BoundedCollection { total, items };
    Ok(EntitiesResource {
        schema_version: 2,
        workspace: workspace.to_owned(),
        entities,
    })
}
fn bounded_freshness(freshness: FreshnessSummary, item_limit: usize) -> FreshnessResource {
    FreshnessResource {
        overall: freshness.overall,
        stale_repositories: bounded_collection(freshness.stale_repositories, item_limit),
        reasons: bounded_collection(freshness.reasons, item_limit),
    }
}

fn community_config_view(config: CommunityConfig, item_limit: usize) -> CommunityConfigView {
    CommunityConfigView {
        algorithm: config.algorithm,
        scope: config.scope,
        seed: config.seed,
        resolution: config.resolution,
        minimum_confidence: config.minimum_confidence,
        edge_weights: bounded_collection(config.edge_weights, item_limit),
        max_iterations: config.max_iterations,
    }
}

fn community_view(community: Community, item_limit: usize) -> CommunityView {
    CommunityView {
        id: community.id,
        label: community.label,
        members: bounded_collection(community.members, item_limit),
        central_nodes: bounded_collection(community.central_nodes, item_limit),
        repositories: bounded_collection(community.repositories, item_limit),
        services: bounded_collection(community.services, item_limit),
        inbound_contracts: bounded_collection(community.inbound_contracts, item_limit),
        outbound_contracts: bounded_collection(community.outbound_contracts, item_limit),
        metrics: community.metrics,
        label_evidence: bounded_collection(community.label_evidence, item_limit),
        limitations: bounded_collection(community.limitations, item_limit),
    }
}

fn is_contract(kind: NodeKind) -> bool {
    CONTRACT_NODE_KINDS.contains(&kind)
}

fn repository_view(repository: &RepositoryRecord) -> RepositoryView {
    RepositoryView {
        id: repository.id.as_str().to_owned(),
        checkout_id: repository.checkout_id.as_str().to_owned(),
        alias: repository.alias.clone(),
        normalized_remote: repository.normalized_remote.clone(),
        head_commit: repository.head_commit.clone(),
        linked_worktree: repository.is_linked_worktree,
        working_tree_dirty: repository.working_tree_dirty,
    }
}
fn invalid_resource(uri: &str) -> ResourceError {
    ResourceError {
        kind: ResourceErrorKind::Missing,
        message: format!("resource `{uri}` was not found"),
    }
}

fn internal_store(error: impl std::fmt::Display) -> ResourceError {
    internal_message(error.to_string())
}

fn internal_message(message: String) -> ResourceError {
    ResourceError {
        kind: ResourceErrorKind::Internal,
        message,
    }
}

#[cfg(test)]
mod tests;

//! Typed, bounded MCP resource contracts and Markdown delivery.

use std::path::Path;

use code_system_graph_model::{
    Community, CommunityConfig, Evidence, ExtractorRun, FreshnessSummary, Node, NodeKind, OverallFreshness, RepositoryRecord
};
#[cfg(test)]
use code_system_graph_model::{RepoFreshness, RepoFreshnessState};
use code_system_graph_store_sqlite::{SqliteStore, StoreError};
use serde::Serialize;
use serde_json::{Value, json};

#[cfg(test)]
use super::SnapshotMetrics;
use super::{
    GraphStatusReport, freshness_summary, load_status, schema_catalog, validate_workspace
};

struct BoundedItems<T> {
    total: usize,
    items: Vec<T>,
}

impl<T> BoundedItems<T> {
    fn retained(&self) -> usize {
        self.items.len()
    }

    fn truncated(&self) -> bool {
        self.total > self.retained()
    }
}

fn bounded_items<T>(items: impl IntoIterator<Item = T>, item_limit: usize) -> BoundedItems<T> {
    let items = items.into_iter().collect::<Vec<_>>();
    let total = items.len();
    BoundedItems {
        total,
        items: items.into_iter().take(item_limit).collect(),
    }
}

#[derive(Debug, Serialize)]
struct RepositoryView {
    id: String,
    checkout_id: String,
    alias: String,
    normalized_remote: Option<String>,
    head_commit: Option<String>,
    linked_worktree: bool,
    working_tree_dirty: bool,
}

#[derive(Debug, Serialize)]
struct WorkspaceResourceItem {
    name: String,
    configured: bool,
}

#[derive(Debug, Serialize)]
struct WorkspacesResource {
    schema_version: u32,
    total: usize,
    retained: usize,
    truncated: bool,
    workspaces: Vec<WorkspaceResourceItem>,
}

#[derive(Debug, Serialize)]
struct OverviewSnapshot {
    id: String,
    nodes: usize,
    edges: usize,
    evidence: usize,
}

#[derive(Debug, Serialize)]
struct OverviewResource {
    schema_version: u32,
    workspace: String,
    snapshot: OverviewSnapshot,
}

#[derive(Debug, Serialize)]
struct FreshnessResource {
    overall: OverallFreshness,
    stale_repository_total: usize,
    stale_repository_retained: usize,
    stale_repositories_truncated: bool,
    stale_repositories: Vec<code_system_graph_model::RepoId>,
    reason_total: usize,
    reason_retained: usize,
    reasons_truncated: bool,
    reasons: Vec<String>,
}

#[derive(Debug, Serialize)]
struct StatusResource {
    schema_version: u32,
    status: GraphStatusReport,
    repository_total: usize,
    repository_retained: usize,
    repositories_truncated: bool,
    freshness: FreshnessResource,
}

#[derive(Debug, Serialize)]
struct RepositoriesResource {
    schema_version: u32,
    workspace: String,
    total: usize,
    retained: usize,
    truncated: bool,
    repositories: Vec<RepositoryView>,
}

#[derive(Debug, Serialize)]
struct EntitiesResource {
    schema_version: u32,
    workspace: String,
    total: usize,
    retained: usize,
    truncated: bool,
    entities: Vec<Node>,
}

#[derive(Debug, Serialize)]
struct CommunitiesResource {
    schema_version: u32,
    workspace: String,
    snapshot_id: String,
    engine_version: String,
    config: CommunityConfig,
    total: usize,
    retained: usize,
    truncated: bool,
    communities: Vec<Community>,
}

#[derive(Debug, Serialize)]
struct CoverageResource {
    schema_version: u32,
    workspace: String,
    run_total: usize,
    run_retained: usize,
    runs_truncated: bool,
    runs: Vec<ExtractorRun>,
    freshness: FreshnessResource,
}

#[derive(Debug, Serialize)]
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
            Self::Workspaces(resource) => crate::agent_markdown::render_resource(resource, maximum),
            Self::Overview(resource) => crate::agent_markdown::render_resource(resource, maximum),
            Self::Status(resource) => crate::agent_markdown::render_resource(resource, maximum),
            Self::Repositories(resource) => {
                crate::agent_markdown::render_resource(resource, maximum)
            }
            Self::Services(resource) | Self::Contracts(resource) => {
                crate::agent_markdown::render_resource(resource, maximum)
            }
            Self::Communities(resource) => {
                crate::agent_markdown::render_resource(resource, maximum)
            }
            Self::Coverage(resource) => crate::agent_markdown::render_resource(resource, maximum),
            Self::Evidence(resource) => crate::agent_markdown::render_resource(resource, maximum),
            Self::SchemaCatalog(resource) => {
                crate::agent_markdown::render_schema_catalog(resource, maximum)
            }
        }
    }
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
        (
            "code-system-graph://evidence/{id}".to_owned(),
            "evidence-metadata".to_owned(),
            "Template URI for one evidence metadata record; replace {id} with its stable ID."
                .to_owned(),
        ),
    ]
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
        let workspaces = bounded_items(
            [WorkspaceResourceItem {
                name: workspace.to_owned(),
                configured: true,
            }],
            item_limit,
        );
        ResourceDocument::Workspaces(WorkspacesResource {
            schema_version: 2,
            total: workspaces.total,
            retained: workspaces.retained(),
            truncated: workspaces.truncated(),
            workspaces: workspaces.items,
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
            let repositories = bounded_items(
                registry.repositories.iter().map(repository_view),
                item_limit,
            );
            Ok(ResourceDocument::Repositories(RepositoriesResource {
                schema_version: 2,
                workspace: workspace.to_owned(),
                total: repositories.total,
                retained: repositories.retained(),
                truncated: repositories.truncated(),
                repositories: repositories.items,
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
            let communities = bounded_items(snapshot.communities, item_limit);
            Ok(ResourceDocument::Communities(CommunitiesResource {
                schema_version: 2,
                workspace: workspace.to_owned(),
                snapshot_id: snapshot.snapshot_id,
                engine_version: snapshot.engine_version,
                config: snapshot.config,
                total: communities.total,
                retained: communities.retained(),
                truncated: communities.truncated(),
                communities: communities.items,
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
    let repositories = bounded_items(std::mem::take(&mut status.repositories), item_limit);
    let repository_total = repositories.total;
    let repository_retained = repositories.retained();
    let repositories_truncated = repositories.truncated();
    status.repositories = repositories.items;
    StatusResource {
        schema_version: 2,
        status,
        repository_total,
        repository_retained,
        repositories_truncated,
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
    let runs = bounded_items(runs, item_limit);
    CoverageResource {
        schema_version: 2,
        workspace: workspace.to_owned(),
        run_total: runs.total,
        run_retained: runs.retained(),
        runs_truncated: runs.truncated(),
        runs: runs.items,
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
        .load_current_evidence(workspace)
        .map_err(|error| match error {
            StoreError::CurrentSnapshotMissing(_) => ResourceError {
                kind: ResourceErrorKind::Missing,
                message: format!("workspace `{workspace}` has no evidence snapshot"),
            },
            other => internal_store(other),
        })?
        .into_iter()
        .find(|item| item.id.as_str() == evidence_id)
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
    let (nodes, _) = store
        .load_current_graph(workspace)
        .map_err(internal_store)?;
    let selected = bounded_items(
        nodes.into_iter().filter(|node| predicate(node.kind)),
        item_limit,
    );
    Ok(EntitiesResource {
        schema_version: 2,
        workspace: workspace.to_owned(),
        total: selected.total,
        retained: selected.retained(),
        truncated: selected.truncated(),
        entities: selected.items,
    })
}
fn bounded_freshness(freshness: FreshnessSummary, item_limit: usize) -> FreshnessResource {
    let stale_repositories = bounded_items(freshness.stale_repositories, item_limit);
    let reasons = bounded_items(freshness.reasons, item_limit);
    FreshnessResource {
        overall: freshness.overall,
        stale_repository_total: stale_repositories.total,
        stale_repository_retained: stale_repositories.retained(),
        stale_repositories_truncated: stale_repositories.truncated(),
        stale_repositories: stale_repositories.items,
        reason_total: reasons.total,
        reason_retained: reasons.retained(),
        reasons_truncated: reasons.truncated(),
        reasons: reasons.items,
    }
}
fn is_contract(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::HttpOperation
            | NodeKind::GraphqlOperation
            | NodeKind::RpcMethod
            | NodeKind::EventChannel
            | NodeKind::EventSchema
            | NodeKind::DatabaseTable
            | NodeKind::DatabaseColumn
            | NodeKind::ConfigKey
    )
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
mod tests {
    use super::*;
    fn repository_freshness(name: &str) -> RepoFreshness {
        RepoFreshness {
            repo_id: code_system_graph_model::RepoId::new(format!("repo:{name}")),
            checkout_id: code_system_graph_model::CheckoutId::new(format!("checkout:{name}")),
            head_commit: Some(format!("commit-{name}")),
            manifest_hash: "manifest".to_owned(),
            state: RepoFreshnessState::WorkingTreeChanged,
            reason: Some(format!("{name} is stale")),
        }
    }

    fn extractor_run(name: &str) -> ExtractorRun {
        ExtractorRun {
            id: format!("run:{name}"),
            snapshot_id: "snapshot:one".to_owned(),
            repo_id: code_system_graph_model::RepoId::new(format!("repo:{name}")),
            checkout_id: code_system_graph_model::CheckoutId::new(format!("checkout:{name}")),
            extractor: name.to_owned(),
            extractor_version: "1".to_owned(),
            status: code_system_graph_model::ExtractorRunStatus::Success,
            discovered_files: 2,
            parsed_files: 2,
            skipped_files: 0,
            elapsed_ms: 1,
        }
    }

    fn empty_freshness_resource() -> FreshnessResource {
        bounded_freshness(
            FreshnessSummary {
                overall: OverallFreshness::Fresh,
                stale_repositories: Vec::new(),
                reasons: Vec::new(),
            },
            1,
        )
    }

    fn entities_resource() -> EntitiesResource {
        EntitiesResource {
            schema_version: 2,
            workspace: "workspace".to_owned(),
            total: 0,
            retained: 0,
            truncated: false,
            entities: Vec::new(),
        }
    }

    fn workspace_resource_fixtures() -> Vec<(&'static str, ResourceDocument)> {
        vec![
            (
                "workspaces",
                ResourceDocument::Workspaces(WorkspacesResource {
                    schema_version: 2,
                    total: 1,
                    retained: 1,
                    truncated: false,
                    workspaces: vec![WorkspaceResourceItem {
                        name: "workspace".to_owned(),
                        configured: true,
                    }],
                }),
            ),
            (
                "snapshot",
                ResourceDocument::Overview(OverviewResource {
                    schema_version: 2,
                    workspace: "workspace".to_owned(),
                    snapshot: OverviewSnapshot {
                        id: "snapshot:one".to_owned(),
                        nodes: 1,
                        edges: 2,
                        evidence: 3,
                    },
                }),
            ),
            (
                "integrity ok",
                ResourceDocument::Status(StatusResource {
                    schema_version: 2,
                    status: GraphStatusReport {
                        workspace: "workspace".to_owned(),
                        schema_version: 2,
                        integrity_ok: true,
                        snapshot: SnapshotMetrics {
                            snapshot_id: "snapshot:one".to_owned(),
                            node_count: 1,
                            edge_count: 2,
                            evidence_count: 3,
                        },
                        repositories: Vec::new(),
                    },
                    repository_total: 0,
                    repository_retained: 0,
                    repositories_truncated: false,
                    freshness: empty_freshness_resource(),
                }),
            ),
            (
                "repositories",
                ResourceDocument::Repositories(RepositoriesResource {
                    schema_version: 2,
                    workspace: "workspace".to_owned(),
                    total: 0,
                    retained: 0,
                    truncated: false,
                    repositories: Vec::new(),
                }),
            ),
        ]
    }

    fn graph_resource_fixtures() -> Vec<(&'static str, ResourceDocument)> {
        vec![
            ("entities", ResourceDocument::Services(entities_resource())),
            ("entities", ResourceDocument::Contracts(entities_resource())),
            (
                "communities",
                ResourceDocument::Communities(CommunitiesResource {
                    schema_version: 2,
                    workspace: "workspace".to_owned(),
                    snapshot_id: "snapshot:one".to_owned(),
                    engine_version: "1".to_owned(),
                    config: CommunityConfig {
                        algorithm: code_system_graph_model::CommunityAlgorithm::Louvain,
                        scope: code_system_graph_model::CommunityScope::Federated,
                        seed: 0,
                        resolution: 1.0,
                        minimum_confidence: 0.5,
                        edge_weights: Vec::new(),
                        max_iterations: 100,
                    },
                    total: 0,
                    retained: 0,
                    truncated: false,
                    communities: Vec::new(),
                }),
            ),
            (
                "runs",
                ResourceDocument::Coverage(CoverageResource {
                    schema_version: 2,
                    workspace: "workspace".to_owned(),
                    run_total: 0,
                    run_retained: 0,
                    runs_truncated: false,
                    runs: Vec::new(),
                    freshness: empty_freshness_resource(),
                }),
            ),
            (
                "ev:one",
                ResourceDocument::Evidence(EvidenceResource {
                    schema_version: 2,
                    workspace: "workspace".to_owned(),
                    evidence: Evidence {
                        id: code_system_graph_model::EvidenceId::new("ev:one"),
                        repo_id: None,
                        file_path: Some("src/lib.rs".to_owned()),
                        start_line: Some(1),
                        end_line: Some(1),
                        extractor: "fixture".to_owned(),
                        extractor_version: "1".to_owned(),
                        provenance: code_system_graph_model::Provenance::Extracted,
                        confidence: 1.0,
                        observed_at_commit: None,
                        content_hash: None,
                        note: None,
                    },
                }),
            ),
            (
                "```json",
                ResourceDocument::SchemaCatalog(serde_json::json!({
                    "query": {"type": "object"}
                })),
            ),
        ]
    }

    #[test]
    fn every_typed_resource_variant_should_render_its_own_markdown_fixture() {
        let resources = workspace_resource_fixtures()
            .into_iter()
            .chain(graph_resource_fixtures());

        for (expected, resource) in resources {
            let rendered = resource.render(4_096);
            assert!(rendered.contains(expected), "{expected}: {rendered}");
        }
    }
    #[test]
    fn resource_freshness_collections_should_apply_item_limits() {
        let value = bounded_freshness(
            FreshnessSummary {
                overall: OverallFreshness::Partial,
                stale_repositories: vec![
                    code_system_graph_model::RepoId::new("repo:one"),
                    code_system_graph_model::RepoId::new("repo:two"),
                ],
                reasons: vec!["one".to_owned(), "two".to_owned()],
            },
            1,
        );

        assert_eq!(value.stale_repository_total, 2);
        assert_eq!(value.stale_repository_retained, 1);
        assert!(value.stale_repositories_truncated);
        assert_eq!(value.reason_total, 2);
        assert_eq!(value.reason_retained, 1);
        assert!(value.reasons_truncated);
    }

    #[test]
    fn status_resource_should_limit_repositories_with_exact_metadata() {
        let repositories = vec![repository_freshness("one"), repository_freshness("two")];
        let value = status_resource_value(
            GraphStatusReport {
                workspace: "workspace".to_owned(),
                schema_version: 2,
                integrity_ok: true,
                snapshot: SnapshotMetrics {
                    snapshot_id: "snapshot:one".to_owned(),
                    node_count: 0,
                    edge_count: 0,
                    evidence_count: 0,
                },
                repositories: repositories.clone(),
            },
            freshness_summary(&repositories),
            1,
        );

        assert_eq!(value.repository_total, 2);
        assert_eq!(value.repository_retained, 1);
        assert!(value.repositories_truncated);
        assert_eq!(value.status.repositories.len(), 1);
    }

    #[test]
    fn coverage_resource_should_limit_runs_with_exact_metadata() {
        let value = coverage_resource_value(
            "workspace",
            vec![extractor_run("one"), extractor_run("two")],
            FreshnessSummary {
                overall: OverallFreshness::Fresh,
                stale_repositories: Vec::new(),
                reasons: Vec::new(),
            },
            1,
        );

        assert_eq!(value.run_total, 2);
        assert_eq!(value.run_retained, 1);
        assert!(value.runs_truncated);
        assert_eq!(value.runs.len(), 1);
    }

    #[test]
    fn schema_catalog_should_apply_resource_item_limit() {
        let catalog = bounded_schema_catalog(1);

        assert_eq!(catalog["schema_retained"], 1);
        assert_eq!(
            catalog["schemas"].as_object().map(serde_json::Map::len),
            Some(1)
        );
        assert_eq!(catalog["schemas_truncated"], true);
        assert!(
            catalog["schema_total"]
                .as_u64()
                .is_some_and(|total| total > 1)
        );
    }
}

//! Explore execution ownership, blocking isolation, and staged snapshot loading.

use std::future::Future;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use code_system_graph_core::{ExecutionPolicy, ProviderExecution, ResolvedSymbol};
use code_system_graph_model::{Edge, Evidence, Node, RepoFreshness, WorkspaceRecord};
use code_system_graph_store_sqlite::SqliteStore;
use tokio_util::sync::CancellationToken;

use super::ExploreLocalRelationship;

pub(super) struct ExploreExecutionContext {
    pub(super) deadline: tokio::time::Instant,
    pub(super) cancellation: CancellationToken,
}

impl ExploreExecutionContext {
    pub(super) fn new(policy: &ExecutionPolicy) -> Self {
        Self {
            deadline: tokio::time::Instant::now()
                + std::time::Duration::from_millis(policy.max_explore_wall_time_ms),
            cancellation: CancellationToken::new(),
        }
    }

    pub(super) fn expired(&self) -> bool {
        tokio::time::Instant::now() >= self.deadline
    }
}

impl Drop for ExploreExecutionContext {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

#[derive(Default)]
pub(super) struct ExploreBudgetLedger {
    pub(super) operations: Vec<ProviderExecution>,
    pub(super) degradations: Vec<String>,
    pub(super) truncations: Vec<String>,
    pub(super) gaps: Vec<String>,
    pub(super) provider_operations: usize,
    enrichment_retained_bytes: usize,
}

impl ExploreBudgetLedger {
    pub(super) fn try_reserve_operations(
        &mut self,
        count: usize,
        limit: usize,
        context: &ExploreExecutionContext,
    ) -> bool {
        if context.expired() || self.provider_operations.saturating_add(count) > limit {
            return false;
        }
        self.provider_operations += count;
        true
    }

    pub(super) fn remaining_enrichment_bytes(&self, limit: usize) -> usize {
        limit.saturating_sub(self.enrichment_retained_bytes)
    }

    pub(super) fn retain_enrichment_bytes(&mut self, bytes: usize, limit: usize) {
        self.enrichment_retained_bytes = self
            .enrichment_retained_bytes
            .saturating_add(bytes)
            .min(limit);
    }

    pub(super) fn record_execution(&mut self, execution: ProviderExecution) {
        self.degradations.extend(
            execution
                .degradations
                .iter()
                .map(|item| item.message.clone()),
        );
        self.operations.push(execution);
    }

    pub(super) fn normalize(&mut self) {
        self.truncations.sort();
        self.truncations.dedup();
        self.gaps.sort();
        self.gaps.dedup();
        self.degradations.sort();
        self.degradations.dedup();
    }
}

#[derive(Default)]
pub(super) struct ExploreProviderData {
    pub(super) source_markdown: String,
    pub(super) source_context: bool,
    pub(super) resolved_symbols: Vec<ResolvedSymbol>,
    pub(super) symbol_resolution: bool,
    pub(super) local_relationships: Vec<ExploreLocalRelationship>,
    pub(super) anchors_traversed: usize,
}

pub(super) struct ExploreSnapshotData {
    pub(super) registry: WorkspaceRecord,
    pub(super) freshness: Vec<RepoFreshness>,
    pub(super) freshness_loaded: bool,
    pub(super) nodes: Vec<Node>,
    pub(super) edges: Vec<Edge>,
    pub(super) evidence: Vec<Evidence>,
    pub(super) gaps: Vec<String>,
    pub(super) incomplete_stage: Option<&'static str>,
}

#[derive(Debug)]
pub(super) enum ExploreBlockingError {
    Failed(String),
    Deadline,
}

fn explore_blocking_permits() -> Arc<tokio::sync::Semaphore> {
    static PERMITS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    PERMITS
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
        .clone()
}

pub(super) async fn run_bounded_explore_blocking<T, F>(
    context: &ExploreExecutionContext,
    operation: F,
) -> Result<T, ExploreBlockingError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    let permit =
        match tokio::time::timeout_at(context.deadline, explore_blocking_permits().acquire_owned())
            .await
        {
            Ok(Ok(permit)) => permit,
            Ok(Err(error)) => return Err(ExploreBlockingError::Failed(error.to_string())),
            Err(_) => {
                context.cancellation.cancel();
                return Err(ExploreBlockingError::Deadline);
            }
        };
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    });
    match tokio::time::timeout_at(context.deadline, task).await {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(error))) => Err(ExploreBlockingError::Failed(error)),
        Ok(Err(error)) => Err(ExploreBlockingError::Failed(format!(
            "Explore blocking stage failed: {error}"
        ))),
        Err(_) => {
            context.cancellation.cancel();
            Err(ExploreBlockingError::Deadline)
        }
    }
}

async fn load_snapshot_stage<T, F>(
    database_path: &Path,
    workspace: &str,
    context: &ExploreExecutionContext,
    load: F,
) -> Result<T, ExploreBlockingError>
where
    T: Send + 'static,
    F: FnOnce(&SqliteStore, &str) -> Result<T, String> + Send + 'static,
{
    let database_path = database_path.to_path_buf();
    let workspace = workspace.to_owned();
    let cancellation = context.cancellation.clone();
    let deadline = context.deadline;
    let result = run_bounded_explore_blocking(context, move || {
        let store =
            SqliteStore::open_read_only(&database_path).map_err(|error| error.to_string())?;
        store
            .interrupt_queries_when(move || {
                cancellation.is_cancelled() || tokio::time::Instant::now() >= deadline
            })
            .map_err(|error| error.to_string())?;
        load(&store, &workspace)
    })
    .await;
    match result {
        Err(ExploreBlockingError::Failed(_))
            if context.expired() || context.cancellation.is_cancelled() =>
        {
            Err(ExploreBlockingError::Deadline)
        }
        other => other,
    }
}

pub(super) async fn load_explore_snapshot(
    database_path: &Path,
    workspace: &str,
    context: &ExploreExecutionContext,
) -> Result<ExploreSnapshotData, ExploreBlockingError> {
    assemble_explore_snapshot(
        load_snapshot_stage(database_path, workspace, context, |store, workspace| {
            store
                .load_workspace_registry(workspace)
                .map_err(|error| error.to_string())
        }),
        load_snapshot_stage(database_path, workspace, context, |store, workspace| {
            store
                .load_current_freshness(workspace)
                .map_err(|error| error.to_string())
        }),
        load_snapshot_stage(database_path, workspace, context, |store, workspace| {
            store
                .load_current_graph(workspace)
                .map_err(|error| error.to_string())
        }),
        load_snapshot_stage(database_path, workspace, context, |store, workspace| {
            store
                .load_current_evidence(workspace)
                .map_err(|error| error.to_string())
        }),
    )
    .await
}

async fn assemble_explore_snapshot<R, F, G, E>(
    registry: R,
    freshness: F,
    graph: G,
    evidence: E,
) -> Result<ExploreSnapshotData, ExploreBlockingError>
where
    R: Future<Output = Result<WorkspaceRecord, ExploreBlockingError>>,
    F: Future<Output = Result<Vec<RepoFreshness>, ExploreBlockingError>>,
    G: Future<Output = Result<(Vec<Node>, Vec<Edge>), ExploreBlockingError>>,
    E: Future<Output = Result<Vec<Evidence>, ExploreBlockingError>>,
{
    let registry = registry.await?;
    let mut snapshot = ExploreSnapshotData {
        registry,
        freshness: Vec::new(),
        freshness_loaded: false,
        nodes: Vec::new(),
        edges: Vec::new(),
        evidence: Vec::new(),
        gaps: Vec::new(),
        incomplete_stage: None,
    };

    match freshness.await {
        Ok(freshness) => {
            snapshot.freshness = freshness;
            snapshot.freshness_loaded = true;
        }
        Err(ExploreBlockingError::Failed(error)) => snapshot
            .gaps
            .push(format!("persisted freshness unavailable: {error}")),
        Err(ExploreBlockingError::Deadline) => {
            snapshot.incomplete_stage = Some("freshness loading");
            return Ok(snapshot);
        }
    }

    match graph.await {
        Ok((nodes, edges)) => {
            snapshot.nodes = nodes;
            snapshot.edges = edges;
        }
        Err(ExploreBlockingError::Failed(error)) => snapshot
            .gaps
            .push(format!("persisted graph unavailable: {error}")),
        Err(ExploreBlockingError::Deadline) => {
            snapshot.incomplete_stage = Some("graph loading");
            return Ok(snapshot);
        }
    }

    match evidence.await {
        Ok(evidence) => snapshot.evidence = evidence,
        Err(ExploreBlockingError::Failed(error)) => snapshot
            .gaps
            .push(format!("persisted evidence unavailable: {error}")),
        Err(ExploreBlockingError::Deadline) => {
            snapshot.incomplete_stage = Some("evidence loading");
        }
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use code_system_graph_core::ExecutionPolicy;
    use code_system_graph_model::{
        CheckoutId, NativePath, NativePathEncoding, RepoId, RepositoryRecord, WorkspaceId, WorkspaceRecord
    };
    use code_system_graph_store_sqlite::SqliteStore;

    use super::{
        ExploreBlockingError, ExploreExecutionContext, assemble_explore_snapshot, load_explore_snapshot, run_bounded_explore_blocking
    };

    fn workspace() -> WorkspaceRecord {
        WorkspaceRecord {
            id: WorkspaceId::new("workspace:commerce"),
            name: "commerce".to_owned(),
            manifest_hash: "manifest".to_owned(),
            config_path: None,
            repositories: vec![RepositoryRecord {
                id: RepoId::new("repo:api"),
                checkout_id: CheckoutId::new("checkout:api"),
                alias: "api".to_owned(),
                canonical_path: NativePath {
                    encoding: NativePathEncoding::Utf8,
                    bytes: b"/api".to_vec(),
                    display: "/api".to_owned(),
                },
                git_common_dir: None,
                normalized_remote: None,
                head_commit: None,
                is_linked_worktree: false,
                working_tree_dirty: false,
            }],
        }
    }

    #[tokio::test]
    async fn completed_context_should_survive_a_later_blocking_deadline() {
        let context = ExploreExecutionContext::new(&ExecutionPolicy {
            max_explore_wall_time_ms: 25,
            ..ExecutionPolicy::default()
        });
        let registry_context = run_bounded_explore_blocking(&context, || Ok("registry".to_owned()))
            .await
            .expect("first stage");
        let result = run_bounded_explore_blocking(&context, || {
            std::thread::sleep(Duration::from_millis(100));
            Ok(())
        })
        .await;

        assert!(matches!(result, Err(ExploreBlockingError::Deadline)));
        assert_eq!(registry_context, "registry");
    }

    #[tokio::test]
    async fn dropping_request_should_cancel_blocking_work_and_release_its_permit() {
        let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
        let (stopped_sender, stopped_receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let context = ExploreExecutionContext::new(&ExecutionPolicy {
                max_explore_wall_time_ms: 5_000,
                ..ExecutionPolicy::default()
            });
            let cancellation = context.cancellation.clone();
            run_bounded_explore_blocking(&context, move || {
                let _ = started_sender.send(());
                while !cancellation.is_cancelled() {
                    std::thread::yield_now();
                }
                let _ = stopped_sender.send(());
                Ok(())
            })
            .await
        });
        started_receiver.await.expect("blocking stage started");
        task.abort();

        tokio::time::timeout(Duration::from_millis(250), stopped_receiver)
            .await
            .expect("drop cancellation should stop blocking work promptly")
            .expect("blocking stage should report its stop");
        let context = ExploreExecutionContext::new(&ExecutionPolicy::default());
        run_bounded_explore_blocking(&context, || Ok(()))
            .await
            .expect("released permit should admit the next stage");
    }

    #[tokio::test]
    async fn snapshot_assembly_should_stop_after_a_later_stage_deadline() {
        let evidence_polled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let evidence_observer = evidence_polled.clone();
        let snapshot = assemble_explore_snapshot(
            async { Ok(workspace()) },
            async { Ok(Vec::new()) },
            async { Err(ExploreBlockingError::Deadline) },
            async move {
                evidence_observer.store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(Vec::new())
            },
        )
        .await
        .expect("loaded registry and freshness should be preserved");

        assert!(snapshot.freshness_loaded);
        assert_eq!(snapshot.incomplete_stage, Some("graph loading"));
        assert!(!evidence_polled.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn expired_deadline_should_interrupt_real_snapshot_loading() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let database = temporary.path().join("graph.db");
        let mut store = SqliteStore::open(&database).expect("store");
        store
            .save_workspace_registry(&workspace())
            .expect("workspace registry");
        drop(store);
        let context = ExploreExecutionContext {
            deadline: tokio::time::Instant::now(),
            cancellation: tokio_util::sync::CancellationToken::new(),
        };

        let result = load_explore_snapshot(&database, "commerce", &context).await;
        assert!(matches!(result, Err(ExploreBlockingError::Deadline)));
    }
}

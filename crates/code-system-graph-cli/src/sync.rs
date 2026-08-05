//! Incremental synchronization orchestration for local workspace indexes.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use code_system_graph_core::{
    CodeGraphConfig, CodeGraphProvider, ConfigSource, EffectiveRepositoryConfig, IgnorePolicy, ProviderBudget, ProviderRequest, ProviderStatus
};
use code_system_graph_model::RepoId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::{
    ApplicationError, ScanOverrides, ScanSummary, load_workspace_context, work_database_instance_id
};

const MAX_PERSISTED_WATCH_TARGET_BYTES: u64 = 8 * 1024 * 1024;

/// One registered repository that participates in synchronization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncTarget {
    /// Repository alias from the workspace manifest.
    pub alias: String,
    /// Canonical local checkout path.
    pub path: PathBuf,
    /// Effective native discovery policy for this checkout.
    pub ignore_policy: IgnorePolicy,
    /// Explicit repository-relative artifacts that must remain observable through exclusions.
    pub explicit_paths: Vec<PathBuf>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedWatchTarget {
    alias: String,
    path: PathBuf,
    configured_excludes: Vec<String>,
    configured_excludes_source: ConfigSource,
    include_defaults: Vec<String>,
    include_defaults_source: ConfigSource,
    explicit_paths: Vec<PathBuf>,
}

impl From<&SyncTarget> for PersistedWatchTarget {
    fn from(target: &SyncTarget) -> Self {
        Self {
            alias: target.alias.clone(),
            path: target.path.clone(),
            configured_excludes: target.ignore_policy.configured_excludes().to_vec(),
            configured_excludes_source: target.ignore_policy.configured_excludes_source(),
            include_defaults: target.ignore_policy.include_defaults().to_vec(),
            include_defaults_source: target.ignore_policy.include_defaults_source(),
            explicit_paths: target.explicit_paths.clone(),
        }
    }
}

impl TryFrom<PersistedWatchTarget> for SyncTarget {
    type Error = ApplicationError;

    fn try_from(target: PersistedWatchTarget) -> Result<Self, Self::Error> {
        let ignore_policy = IgnorePolicy::new(
            target.configured_excludes,
            target.configured_excludes_source,
            target.include_defaults,
            target.include_defaults_source,
        )
        .map_err(|error| {
            ApplicationError::Initialization(format!("invalid persisted watch scope: {error}"))
        })?;
        Ok(Self {
            alias: target.alias,
            path: target.path,
            ignore_policy,
            explicit_paths: target.explicit_paths,
        })
    }
}

/// Outcome of synchronizing one repository's local `CodeGraph` index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CodeGraphSyncState {
    /// `codegraph sync` completed successfully.
    Synchronized,
    /// The repository has no initialized `.codegraph` index.
    SkippedNotInitialized,
    /// The external synchronization command failed.
    Failed,
}

/// Bounded result for one repository's local `CodeGraph` index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeGraphRepositorySync {
    /// Repository alias from the workspace manifest.
    pub repository: String,
    /// Synchronization outcome.
    pub state: CodeGraphSyncState,
    /// Bounded diagnostic when the index was skipped or failed.
    pub detail: Option<String>,
}

/// Aggregate `CodeGraph` synchronization result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeGraphSyncSummary {
    /// Whether local `CodeGraph` synchronization was requested.
    pub enabled: bool,
    /// Number of selected workspace repositories.
    pub repository_count: usize,
    /// Number of local indexes synchronized successfully.
    pub synchronized_count: usize,
    /// Number of local indexes whose structured status required synchronization.
    #[serde(default)]
    pub changed_count: usize,
    /// Number of initialized local indexes that were already current.
    #[serde(default)]
    pub unchanged_count: usize,
    /// Number of repositories without an initialized local index.
    pub skipped_count: usize,
    /// Number of external synchronization failures.
    pub failed_count: usize,
    /// Deterministic per-repository outcomes.
    pub repositories: Vec<CodeGraphRepositorySync>,
}

/// Observable result of one `csgraph sync` pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SyncSummary {
    /// Output schema version.
    pub schema_version: u8,
    /// Resource accounting for the complete supervised sync pass.
    pub execution: code_system_graph_core::ExecutionSummary,
    /// Incremental Code System Graph scan result.
    pub scan: ScanSummary,
    /// Local per-repository `CodeGraph` index results.
    pub codegraph: CodeGraphSyncSummary,
}

/// Resolves and validates the repository checkouts selected for synchronization.
///
/// # Errors
///
/// Returns [`ApplicationError`] when the manifest, repository selection, or checkout registry is
/// invalid.
pub fn workspace_sync_targets(
    config_path: &Path,
    overrides: &ScanOverrides,
) -> Result<Vec<SyncTarget>, ApplicationError> {
    let context = load_workspace_context(config_path, overrides)?;
    if let Some(requested) = &overrides.workspace
        && requested != &context.manifest.name
    {
        return Err(ApplicationError::WorkspaceNameMismatch {
            requested: requested.clone(),
            manifest: context.manifest.name,
        });
    }
    if let Some(selected) = &overrides.repository
        && !context
            .registry
            .record
            .repositories
            .iter()
            .any(|repository| &repository.alias == selected)
    {
        return Err(ApplicationError::UnknownOverrideRepository(
            selected.clone(),
        ));
    }

    context
        .registry
        .record
        .repositories
        .iter()
        .filter(|repository| {
            overrides
                .repository
                .as_ref()
                .is_none_or(|selected| selected == &repository.alias)
        })
        .map(|repository| {
            let path = context
                .registry
                .checkout_path(&repository.alias)
                .ok_or_else(|| ApplicationError::RegistryAliasMissing(repository.alias.clone()))?;
            let effective = context
                .repository_configs
                .get(&repository.alias)
                .ok_or_else(|| ApplicationError::RegistryAliasMissing(repository.alias.clone()))?;
            Ok(SyncTarget {
                alias: repository.alias.clone(),
                path: path.to_path_buf(),
                ignore_policy: effective.ignore_policy.clone(),
                explicit_paths: explicit_watch_paths(effective),
            })
        })
        .collect()
}

fn explicit_watch_paths(config: &EffectiveRepositoryConfig) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from(".code-system-graph.yaml")];
    paths.extend(config.openapi.iter().map(PathBuf::from));
    paths.extend(
        config
            .http_consumers
            .iter()
            .map(|consumer| PathBuf::from(&consumer.source)),
    );
    paths.extend(
        config
            .integration_tests
            .iter()
            .map(|test| PathBuf::from(&test.path)),
    );
    paths.extend(
        config
            .implementations
            .iter()
            .map(|implementation| PathBuf::from(&implementation.path)),
    );
    paths.sort();
    paths.dedup();
    paths
}

/// Synchronizes initialized local `CodeGraph` indexes and then publishes an incremental graph
/// snapshot.
///
/// The external index is optional and best effort: a missing or failed per-repository index is
/// reported explicitly without preventing the native Code System Graph scan.
///
/// # Errors
///
/// Returns [`ApplicationError`] when workspace validation or the native incremental scan fails.
pub fn sync_workspace_with_overrides(
    config_path: &Path,
    database_path: &Path,
    overrides: &ScanOverrides,
    synchronize_codegraph: bool,
) -> Result<SyncSummary, ApplicationError> {
    super::worker::supervise_sync(config_path, database_path, overrides, synchronize_codegraph)
}

/// Synchronizes through an explicitly selected compatible worker executable.
///
/// Embedding applications can pass their own executable after dispatching `__worker-v1` to
/// [`crate::run_worker_from_stdio`].
///
/// # Errors
///
/// Returns [`ApplicationError`] when the worker cannot start or synchronization fails.
pub fn sync_workspace_with_worker_executable(
    config_path: &Path,
    database_path: &Path,
    overrides: &ScanOverrides,
    synchronize_codegraph: bool,
    worker_executable: &Path,
) -> Result<SyncSummary, ApplicationError> {
    super::worker::supervise_sync_with_executable(
        config_path,
        database_path,
        overrides,
        synchronize_codegraph,
        worker_executable,
    )
}

#[doc(hidden)]
pub fn sync_workspace_with_wall_time_cap(
    config_path: &Path,
    database_path: &Path,
    overrides: &ScanOverrides,
    synchronize_codegraph: bool,
    wall_time_cap_ms: u64,
) -> Result<SyncSummary, ApplicationError> {
    super::worker::supervise_sync_with_wall_time_cap(
        config_path,
        database_path,
        overrides,
        synchronize_codegraph,
        wall_time_cap_ms,
    )
}

pub(crate) fn sync_workspace_direct(
    config_path: &Path,
    database_path: &Path,
    overrides: &ScanOverrides,
    synchronize_codegraph: bool,
) -> Result<SyncSummary, ApplicationError> {
    let context = load_workspace_context(config_path, overrides)?;
    let policy = context.execution_policy;
    let workspace = context.manifest.name;
    let targets = workspace_sync_targets(config_path, overrides)?;
    persist_watch_targets(database_path, &workspace, &targets)?;
    let binary = codegraph_binary(overrides);
    let codegraph_timeout = Duration::from_millis(policy.max_codegraph_sync_wall_time_ms_per_repo);
    let codegraph = synchronize_codegraph_targets(
        &targets,
        synchronize_codegraph,
        codegraph_timeout,
        |target, deadline| run_codegraph_status(&binary, target, deadline),
        |path, deadline| run_codegraph_sync(&binary, path, deadline),
    );
    let mut scan_overrides = overrides.clone();
    scan_overrides.codegraph = codegraph.synchronized_count > 0;
    let scan = super::scan_workspace_direct_for_sync(
        config_path,
        database_path,
        &scan_overrides,
        codegraph.changed_count == 0,
    )?;
    Ok(SyncSummary {
        schema_version: 1,
        execution: code_system_graph_core::ExecutionSummary::default(),
        scan,
        codegraph,
    })
}

fn run_codegraph_status(
    binary: &OsStr,
    target: &SyncTarget,
    deadline: Instant,
) -> Result<ProviderStatus, String> {
    let binary = binary.to_owned();
    let alias = target.alias.clone();
    let project_path = target.path.clone();
    std::thread::spawn(move || {
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return Err(
                "CodeGraph status exceeded the repository synchronization deadline".to_owned(),
            );
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| format!("cannot start CodeGraph status runtime: {error}"))?;
        let provider = CodeGraphProvider::new(CodeGraphConfig {
            binary,
            max_concurrent_processes: 1,
            ..CodeGraphConfig::default()
        })
        .map_err(|error| format!("cannot configure CodeGraph status provider: {error}"))?;
        runtime
            .block_on(provider.index_status(ProviderRequest {
                repo_id: RepoId::new(format!("sync:{alias}")),
                project_path,
                budget: ProviderBudget {
                    timeout,
                    max_output_bytes: 256 * 1024,
                    max_items: 1,
                },
                cancellation: CancellationToken::new(),
            }))
            .map_err(|error| format!("cannot inspect CodeGraph status: {error}"))
    })
    .join()
    .map_err(|_| "CodeGraph status worker terminated unexpectedly".to_owned())?
}

fn persist_watch_targets(
    database_path: &Path,
    workspace: &str,
    targets: &[SyncTarget],
) -> Result<(), ApplicationError> {
    let mut encoded = Vec::with_capacity(targets.len());
    for target in targets {
        let payload = serde_json::to_vec(&PersistedWatchTarget::from(target))
            .map_err(|error| ApplicationError::Initialization(error.to_string()))?;
        if u64::try_from(payload.len()).unwrap_or(u64::MAX) > MAX_PERSISTED_WATCH_TARGET_BYTES {
            return Err(ApplicationError::Initialization(
                "persisted watch target exceeded its protocol bound".to_owned(),
            ));
        }
        encoded.push((target.alias.clone(), payload));
    }
    let database_instance_id =
        code_system_graph_store_sqlite::SqliteStore::open(database_path)?.database_instance_id()?;
    super::work_state::WorkState::open(database_path, &database_instance_id)
        .and_then(|mut state| state.replace_watch_scope(workspace, &encoded))
        .map_err(ApplicationError::Initialization)
}

#[doc(hidden)]
pub fn load_persisted_watch_targets(
    database_path: &Path,
    workspace: &str,
) -> Result<Vec<SyncTarget>, ApplicationError> {
    let database_instance_id = work_database_instance_id(database_path)?;
    let state = super::work_state::WorkState::open(database_path, &database_instance_id)
        .map_err(ApplicationError::Initialization)?;
    state
        .load_watch_scope(workspace, MAX_PERSISTED_WATCH_TARGET_BYTES)
        .map_err(ApplicationError::Initialization)?
        .into_iter()
        .map(|encoded| {
            serde_json::from_slice::<PersistedWatchTarget>(&encoded)
                .map_err(|error| ApplicationError::Initialization(error.to_string()))?
                .try_into()
        })
        .collect()
}

fn codegraph_binary(overrides: &ScanOverrides) -> OsString {
    overrides
        .codegraph_binary
        .as_ref()
        .map(|path| path.as_os_str().to_owned())
        .or_else(|| {
            std::env::var_os("CODE_SYSTEM_GRAPH_CODEGRAPH_BINARY").filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| OsString::from("codegraph"))
}

fn run_codegraph_sync(
    binary: &OsStr,
    project_path: &Path,
    deadline: Instant,
) -> Result<(), String> {
    if Instant::now() >= deadline {
        return Err("codegraph sync exceeded the repository synchronization deadline".to_owned());
    }
    let mut command = Command::new(binary);
    command
        .arg("sync")
        .arg("--quiet")
        .arg(project_path)
        .current_dir(project_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    configure_codegraph_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("failed to start codegraph sync: {error}"))?;
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("failed to wait for codegraph sync: {error}"))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            super::worker::terminate_process_tree(child.id());
            terminate_codegraph_process_group(&mut child);
            return Err(
                "codegraph sync exceeded the repository synchronization deadline and was terminated"
                    .to_owned(),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    cleanup_exited_codegraph_process_group(child.id());
    if status.success() {
        return Ok(());
    }
    Err(format!("codegraph sync exited with {status}"))
}

#[cfg(unix)]
fn configure_codegraph_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_codegraph_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn terminate_codegraph_process_group(child: &mut std::process::Child) {
    if let Ok(process_group_id) = i32::try_from(child.id()) {
        let process_group_id = nix::unistd::Pid::from_raw(process_group_id);
        let _ = nix::sys::signal::killpg(process_group_id, nix::sys::signal::Signal::SIGTERM);
        std::thread::sleep(Duration::from_millis(100));
        let _ = nix::sys::signal::killpg(process_group_id, nix::sys::signal::Signal::SIGKILL);
    }
    let _ = child.wait();
}

#[cfg(not(unix))]
fn terminate_codegraph_process_group(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(unix)]
fn cleanup_exited_codegraph_process_group(process_id: u32) {
    if let Ok(process_group_id) = i32::try_from(process_id) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(process_group_id),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

#[cfg(not(unix))]
fn cleanup_exited_codegraph_process_group(process_id: u32) {
    super::worker::terminate_process_tree(process_id);
}

#[derive(Debug)]
struct CodeGraphSyncObservation {
    state: CodeGraphSyncState,
    detail: Option<String>,
    index_changed: bool,
}

fn synchronize_codegraph_targets<I, S>(
    targets: &[SyncTarget],
    enabled: bool,
    timeout: Duration,
    mut inspect: I,
    mut synchronize: S,
) -> CodeGraphSyncSummary
where
    I: FnMut(&SyncTarget, Instant) -> Result<ProviderStatus, String>,
    S: FnMut(&Path, Instant) -> Result<(), String>,
{
    let mut observations = Vec::with_capacity(targets.len());
    let mut repositories = Vec::with_capacity(targets.len());
    if enabled {
        for target in targets {
            let deadline = Instant::now() + timeout;
            let observation = if target.path.join(".codegraph").is_dir() {
                match inspect(target, deadline) {
                    Ok(ProviderStatus::Available) => CodeGraphSyncObservation {
                        state: CodeGraphSyncState::Synchronized,
                        detail: None,
                        index_changed: false,
                    },
                    Ok(ProviderStatus::Stale) => match synchronize(&target.path, deadline) {
                        Ok(()) => CodeGraphSyncObservation {
                            state: CodeGraphSyncState::Synchronized,
                            detail: None,
                            index_changed: true,
                        },
                        Err(detail) => failed_sync_observation(&detail),
                    },
                    Ok(ProviderStatus::IndexMissing) => CodeGraphSyncObservation {
                        state: CodeGraphSyncState::SkippedNotInitialized,
                        detail: Some("local CodeGraph index is not initialized".to_owned()),
                        index_changed: false,
                    },
                    Ok(status) => failed_sync_observation(&format!(
                        "CodeGraph status is not usable for synchronization: {status:?}"
                    )),
                    Err(detail) => failed_sync_observation(&detail),
                }
            } else {
                CodeGraphSyncObservation {
                    state: CodeGraphSyncState::SkippedNotInitialized,
                    detail: Some("local CodeGraph index is not initialized".to_owned()),
                    index_changed: false,
                }
            };
            repositories.push(CodeGraphRepositorySync {
                repository: target.alias.clone(),
                state: observation.state,
                detail: observation.detail.clone(),
            });
            observations.push(observation);
            super::worker::report_progress(code_system_graph_core::JobPhase::CodeGraphSync, 1);
        }
    }
    repositories.sort_by(|left, right| left.repository.cmp(&right.repository));
    let synchronized_count = repositories
        .iter()
        .filter(|item| item.state == CodeGraphSyncState::Synchronized)
        .count();
    let changed_count = observations
        .iter()
        .filter(|item| item.state == CodeGraphSyncState::Synchronized && item.index_changed)
        .count();
    let unchanged_count = observations
        .iter()
        .filter(|item| item.state == CodeGraphSyncState::Synchronized && !item.index_changed)
        .count();
    let skipped_count = repositories
        .iter()
        .filter(|item| item.state == CodeGraphSyncState::SkippedNotInitialized)
        .count();
    let failed_count = repositories
        .iter()
        .filter(|item| item.state == CodeGraphSyncState::Failed)
        .count();
    CodeGraphSyncSummary {
        enabled,
        repository_count: targets.len(),
        synchronized_count,
        changed_count,
        unchanged_count,
        skipped_count,
        failed_count,
        repositories,
    }
}

fn failed_sync_observation(detail: &str) -> CodeGraphSyncObservation {
    CodeGraphSyncObservation {
        state: CodeGraphSyncState::Failed,
        detail: Some(bounded_detail(detail)),
        index_changed: false,
    }
}

fn bounded_detail(detail: &str) -> String {
    const MAX_CHARS: usize = 512;
    let normalized = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= MAX_CHARS {
        return normalized;
    }
    let mut bounded = normalized.chars().take(MAX_CHARS).collect::<String>();
    bounded.push_str("...");
    bounded
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    fn target(alias: &str, path: PathBuf) -> SyncTarget {
        SyncTarget {
            alias: alias.to_owned(),
            path,
            ignore_policy: IgnorePolicy::new(
                Vec::new(),
                code_system_graph_core::ConfigSource::Default,
                Vec::new(),
                code_system_graph_core::ConfigSource::Default,
            )
            .expect("built-in ignore policy"),
            explicit_paths: Vec::new(),
        }
    }

    #[test]
    fn persisted_watch_scope_should_preserve_every_alias_policy() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let database = temporary.path().join("graph.db");
        let shared = temporary.path().join("shared");
        std::fs::create_dir(&shared)?;
        let mut restrictive = target("a-restrictive", shared.clone());
        restrictive.ignore_policy = IgnorePolicy::new(
            vec!["generated/**".to_owned()],
            ConfigSource::WorkspaceManifest,
            Vec::new(),
            ConfigSource::Default,
        )?;
        let permissive = target("b-permissive", shared);
        let expected = vec![restrictive, permissive];

        persist_watch_targets(&database, "workspace", &expected)?;
        let actual = load_persisted_watch_targets(&database, "workspace")?;

        assert_eq!(actual, expected);
        Ok(())
    }

    #[test]
    fn synchronization_should_skip_uninitialized_indexes_and_bound_failures() -> anyhow::Result<()>
    {
        let temporary = tempfile::tempdir()?;
        let initialized = temporary.path().join("initialized");
        let absent = temporary.path().join("absent");
        std::fs::create_dir_all(initialized.join(".codegraph"))?;
        std::fs::create_dir(&absent)?;
        let called = RefCell::new(Vec::new());
        let targets = vec![target("zeta", initialized.clone()), target("alpha", absent)];

        let report = synchronize_codegraph_targets(
            &targets,
            true,
            Duration::from_secs(1),
            |target, _| {
                called.borrow_mut().push(target.path.clone());
                Err("failure ".repeat(600))
            },
            |_, _| panic!("failed status inspection must not synchronize"),
        );

        assert_eq!(called.into_inner(), vec![initialized]);
        assert_eq!(report.repository_count, 2);
        assert_eq!(report.synchronized_count, 0);
        assert_eq!(report.skipped_count, 1);
        assert_eq!(report.failed_count, 1);
        assert_eq!(report.repositories[0].repository, "alpha");
        assert_eq!(
            report.repositories[0].state,
            CodeGraphSyncState::SkippedNotInitialized
        );
        assert!(
            report.repositories[1]
                .detail
                .as_deref()
                .is_some_and(|detail| detail.chars().count() <= 515)
        );
        Ok(())
    }

    #[test]
    fn disabled_codegraph_sync_should_not_invoke_runner() {
        let target = target("repo", PathBuf::from("repo"));
        let report = synchronize_codegraph_targets(
            &[target],
            false,
            Duration::from_secs(1),
            |_, _| panic!("disabled synchronization must not inspect CodeGraph"),
            |_, _| panic!("disabled synchronization must not invoke CodeGraph"),
        );
        assert!(!report.enabled);
        assert_eq!(report.repository_count, 1);
        assert!(report.repositories.is_empty());
    }

    #[test]
    fn synchronization_should_distinguish_changed_and_current_indexes() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let current = temporary.path().join("current");
        let stale = temporary.path().join("stale");
        std::fs::create_dir_all(current.join(".codegraph"))?;
        std::fs::create_dir_all(stale.join(".codegraph"))?;
        let synchronized = RefCell::new(Vec::new());
        let targets = vec![target("current", current), target("stale", stale.clone())];

        let report = synchronize_codegraph_targets(
            &targets,
            true,
            Duration::from_secs(1),
            |target, _| {
                if target.alias == "stale" {
                    Ok(ProviderStatus::Stale)
                } else {
                    Ok(ProviderStatus::Available)
                }
            },
            |path, _| {
                synchronized.borrow_mut().push(path.to_path_buf());
                Ok(())
            },
        );

        assert_eq!(synchronized.into_inner(), vec![stale]);
        assert_eq!(report.synchronized_count, 2);
        assert_eq!(report.changed_count, 1);
        assert_eq!(report.unchanged_count, 1);
        Ok(())
    }

    #[test]
    fn synchronization_should_share_one_deadline_between_status_and_sync() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let repository = temporary.path().join("stale");
        std::fs::create_dir_all(repository.join(".codegraph"))?;
        let inspected_deadline = RefCell::new(None);
        let synchronized_deadline = RefCell::new(None);

        let report = synchronize_codegraph_targets(
            &[target("stale", repository)],
            true,
            Duration::from_secs(1),
            |_, deadline| {
                inspected_deadline.replace(Some(deadline));
                Ok(ProviderStatus::Stale)
            },
            |_, deadline| {
                synchronized_deadline.replace(Some(deadline));
                Ok(())
            },
        );

        assert_eq!(report.changed_count, 1);
        assert_eq!(
            inspected_deadline.into_inner(),
            synchronized_deadline.into_inner()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn codegraph_timeout_should_terminate_its_descendant_process() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir()?;
        let script = temporary.path().join("codegraph-test");
        let descendant_pid = temporary.path().join("descendant.pid");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nsleep 30 &\nprintf '%s' \"$!\" > '{}'\nwait\n",
                descendant_pid.display()
            ),
        )?;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;

        let error = run_codegraph_sync(
            script.as_os_str(),
            temporary.path(),
            Instant::now() + Duration::from_secs(2),
        )
        .expect_err("test process must time out");
        assert!(
            error.contains("was terminated"),
            "unexpected error: {error}"
        );
        let pid = std::fs::read_to_string(&descendant_pid)?.parse::<i32>()?;
        for _ in 0..20 {
            if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        anyhow::bail!("CodeGraph descendant {pid} survived timeout")
    }

    #[cfg(unix)]
    #[test]
    fn successful_codegraph_sync_should_terminate_surviving_descendants() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir()?;
        let script = temporary.path().join("codegraph-test");
        let descendant_pid = temporary.path().join("descendant.pid");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nsh -c 'trap \"\" TERM; sleep 30' &\nprintf '%s' \"$!\" > '{}'\nexit 0\n",
                descendant_pid.display()
            ),
        )?;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;

        run_codegraph_sync(
            script.as_os_str(),
            temporary.path(),
            Instant::now() + Duration::from_secs(2),
        )
        .map_err(anyhow::Error::msg)?;
        let pid = std::fs::read_to_string(&descendant_pid)?.parse::<i32>()?;
        for _ in 0..20 {
            if nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_err() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        anyhow::bail!("CodeGraph descendant {pid} survived successful sync")
    }
}

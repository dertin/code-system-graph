//! Incremental synchronization orchestration for local workspace indexes.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use code_system_graph_core::{EffectiveRepositoryConfig, IgnorePolicy};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ApplicationError, ScanOverrides, ScanSummary, load_workspace_context};

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
    let targets = workspace_sync_targets(config_path, overrides)?;
    let binary = codegraph_binary(overrides);
    let codegraph = synchronize_codegraph_targets(&targets, synchronize_codegraph, |path| {
        run_codegraph_sync(&binary, path)
    });
    let mut scan_overrides = overrides.clone();
    scan_overrides.codegraph = codegraph.synchronized_count > 0;
    let scan = super::scan_workspace_with_overrides(config_path, database_path, &scan_overrides)?;
    Ok(SyncSummary {
        schema_version: 1,
        scan,
        codegraph,
    })
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

fn run_codegraph_sync(binary: &OsStr, project_path: &Path) -> Result<(), String> {
    let status = Command::new(binary)
        .arg("sync")
        .arg("--quiet")
        .arg(project_path)
        .current_dir(project_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("failed to start codegraph sync: {error}"))?;
    if status.success() {
        return Ok(());
    }
    Err(format!("codegraph sync exited with {status}"))
}

fn synchronize_codegraph_targets<F>(
    targets: &[SyncTarget],
    enabled: bool,
    mut synchronize: F,
) -> CodeGraphSyncSummary
where
    F: FnMut(&Path) -> Result<(), String>,
{
    let mut repositories = Vec::with_capacity(targets.len());
    if enabled {
        for target in targets {
            let (state, detail) = if target.path.join(".codegraph").is_dir() {
                match synchronize(&target.path) {
                    Ok(()) => (CodeGraphSyncState::Synchronized, None),
                    Err(detail) => (CodeGraphSyncState::Failed, Some(bounded_detail(&detail))),
                }
            } else {
                (
                    CodeGraphSyncState::SkippedNotInitialized,
                    Some("local CodeGraph index is not initialized".to_owned()),
                )
            };
            repositories.push(CodeGraphRepositorySync {
                repository: target.alias.clone(),
                state,
                detail,
            });
        }
    }
    repositories.sort_by(|left, right| left.repository.cmp(&right.repository));
    let synchronized_count = repositories
        .iter()
        .filter(|item| item.state == CodeGraphSyncState::Synchronized)
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
        skipped_count,
        failed_count,
        repositories,
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
    fn synchronization_should_skip_uninitialized_indexes_and_bound_failures() -> anyhow::Result<()>
    {
        let temporary = tempfile::tempdir()?;
        let initialized = temporary.path().join("initialized");
        let absent = temporary.path().join("absent");
        std::fs::create_dir_all(initialized.join(".codegraph"))?;
        std::fs::create_dir(&absent)?;
        let called = RefCell::new(Vec::new());
        let targets = vec![target("zeta", initialized.clone()), target("alpha", absent)];

        let report = synchronize_codegraph_targets(&targets, true, |path| {
            called.borrow_mut().push(path.to_path_buf());
            Err("failure ".repeat(600))
        });

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
        let report = synchronize_codegraph_targets(&[target], false, |_| {
            panic!("disabled synchronization must not invoke CodeGraph")
        });
        assert!(!report.enabled);
        assert_eq!(report.repository_count, 1);
        assert!(report.repositories.is_empty());
    }
}

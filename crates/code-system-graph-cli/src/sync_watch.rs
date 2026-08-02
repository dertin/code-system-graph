//! Portable filesystem watching for the `sync --watch` command.

#[cfg(target_os = "linux")]
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use code_system_graph::{
    ApplicationError, IgnorePolicy, ScanOverrides, SyncSummary, finish_watcher_lease, heartbeat_watcher_lease, start_watcher_lease, sync_workspace_with_wall_time_cap, workspace_sync_targets
};
use notify::{Config, Event, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

#[derive(serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WatchOutput<'a> {
    SyncResult {
        #[serde(rename = "schemaVersion")]
        schema_version: u8,
        summary: &'a SyncSummary,
    },
    Termination {
        #[serde(rename = "schemaVersion")]
        schema_version: u8,
        state: &'a str,
        detail: &'a str,
    },
}

enum WaitOutcome {
    Change,
    Heartbeat,
    Shutdown,
    ExpiredIdle,
    ExpiredSession,
}

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);
const MAX_SYNC_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
struct WatchScope {
    config: PathBuf,
    database: PathBuf,
    repositories: Vec<WatchRepository>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WatchRepository {
    root: PathBuf,
    ignore_policy: IgnorePolicy,
    explicit_paths: Vec<PathBuf>,
}

impl WatchRepository {
    fn relevant(&self, path: &Path) -> bool {
        path.strip_prefix(&self.root).is_ok_and(|relative| {
            self.explicit_paths.iter().any(|explicit| {
                relative == explicit
                    || explicit.starts_with(relative)
                    || relative.starts_with(explicit)
            }) || !self.ignore_policy.excludes(relative, path.is_dir())
        })
    }

    fn should_watch_directory(&self, path: &Path) -> bool {
        path.strip_prefix(&self.root).is_ok_and(|relative| {
            self.explicit_paths
                .iter()
                .any(|explicit| explicit.starts_with(relative))
                || !self.ignore_policy.excludes(relative, true)
        })
    }
}

impl WatchScope {
    fn load(config: &Path, database: &Path, overrides: &ScanOverrides) -> anyhow::Result<Self> {
        let mut repositories = workspace_sync_targets(config, overrides)?
            .into_iter()
            .map(|target| WatchRepository {
                root: target.path,
                ignore_policy: target.ignore_policy,
                explicit_paths: target.explicit_paths,
            })
            .collect::<Vec<_>>();
        repositories.sort_by(|left, right| left.root.cmp(&right.root));
        Ok(Self {
            config: absolute_path(config)?,
            database: absolute_path(database)?,
            repositories,
        })
    }

    fn relevant_event(&self, event: &Event) -> bool {
        event.paths.is_empty() || event.paths.iter().any(|path| self.relevant_path(path))
    }

    fn relevant_path(&self, path: &Path) -> bool {
        if path == self.config {
            return true;
        }
        if database_artifact(path, &self.database) {
            return false;
        }
        self.repositories
            .iter()
            .any(|repository| repository.relevant(path))
    }

    fn should_watch_directory(&self, path: &Path) -> bool {
        self.repositories
            .iter()
            .any(|repository| repository.should_watch_directory(path))
    }

    fn watch_entries(&self) -> Vec<(PathBuf, RecursiveMode)> {
        let mut entries = Vec::new();
        if let Some(parent) = self.config.parent() {
            entries.push((parent.to_path_buf(), RecursiveMode::NonRecursive));
        }
        for repository in &self.repositories {
            add_watch_entry(
                &mut entries,
                repository.root.clone(),
                RecursiveMode::Recursive,
            );
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }
}

enum ActiveWatcher {
    Native(Box<RecommendedWatcher>),
    Polling(Box<PollWatcher>),
}

impl ActiveWatcher {
    #[cfg(target_os = "linux")]
    fn refresh_native_directories(&mut self, scope: &WatchScope) -> notify::Result<()> {
        if let Self::Native(watcher) = self {
            add_native_watch_entries(watcher.as_mut(), scope)?;
        } else if let Self::Polling(watcher) = self {
            let _keep_alive = watcher.as_ref();
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn refresh_native_directories(&mut self, _scope: &WatchScope) -> notify::Result<()> {
        match self {
            Self::Native(watcher) => {
                let _keep_alive = watcher.as_ref();
            }
            Self::Polling(watcher) => {
                let _keep_alive = watcher.as_ref();
            }
        }
        Ok(())
    }
}

/// Runs initial synchronization and then keeps both local index layers current.
#[expect(
    clippy::too_many_lines,
    reason = "the watcher lifecycle is kept together so every terminal path closes its lease"
)]
pub(crate) async fn watch_workspace(
    config: PathBuf,
    database: PathBuf,
    overrides: ScanOverrides,
    synchronize_codegraph: bool,
    debounce: Duration,
    poll_interval: Option<Duration>,
) -> anyhow::Result<()> {
    let (workspace, policy) = start_watcher_lease(&config, &database)?;
    let session_started = Instant::now();
    let mut idle_renewed = session_started;
    let mut last_pass_started = session_started;
    let (sender, mut receiver) = mpsc::channel(1);
    let mut scope = WatchScope::load(&config, &database, &overrides)?;
    let mut watcher = build_watcher(&scope, sender.clone(), poll_interval)?;

    if let Err(error) = retry_sync(
        &config,
        &database,
        &overrides,
        synchronize_codegraph,
        session_started,
        policy.max_watch_session_wall_time_ms,
    )
    .await
    {
        if session_expired(session_started, policy.max_watch_session_wall_time_ms) {
            finish_and_emit(
                &database,
                &workspace,
                "expired_session",
                "watcher reached its absolute session deadline",
            )?;
            return Ok(());
        }
        finish_after_error(&database, &workspace, &error)?;
        return Err(error);
    }
    loop {
        heartbeat_watcher_lease(&database, &workspace, false)?;
        let idle_deadline = idle_renewed + Duration::from_millis(policy.watch_idle_timeout_ms);
        let session_deadline =
            session_started + Duration::from_millis(policy.max_watch_session_wall_time_ms);
        match wait_for_debounced_change(&mut receiver, debounce, idle_deadline, session_deadline)
            .await?
        {
            WaitOutcome::Change => {}
            WaitOutcome::Heartbeat => continue,
            WaitOutcome::Shutdown => {
                finish_and_emit(
                    &database,
                    &workspace,
                    "stale",
                    "watcher stopped by operator signal",
                )?;
                drop(watcher);
                return Ok(());
            }
            WaitOutcome::ExpiredIdle => {
                finish_and_emit(
                    &database,
                    &workspace,
                    "expired_idle",
                    "watcher reached its inactivity deadline",
                )?;
                drop(watcher);
                return Ok(());
            }
            WaitOutcome::ExpiredSession => {
                finish_and_emit(
                    &database,
                    &workspace,
                    "expired_session",
                    "watcher reached its absolute session deadline",
                )?;
                drop(watcher);
                return Ok(());
            }
        }
        let minimum_start =
            last_pass_started + Duration::from_millis(policy.min_watch_rescan_interval_ms);
        if Instant::now() < minimum_start {
            tokio::time::sleep(minimum_start.saturating_duration_since(Instant::now())).await;
        }
        if session_started.elapsed() >= Duration::from_millis(policy.max_watch_session_wall_time_ms)
        {
            finish_and_emit(
                &database,
                &workspace,
                "expired_session",
                "watcher reached its absolute session deadline",
            )?;
            return Ok(());
        }
        last_pass_started = Instant::now();
        if let Err(error) = retry_sync(
            &config,
            &database,
            &overrides,
            synchronize_codegraph,
            session_started,
            policy.max_watch_session_wall_time_ms,
        )
        .await
        {
            if session_expired(session_started, policy.max_watch_session_wall_time_ms) {
                finish_and_emit(
                    &database,
                    &workspace,
                    "expired_session",
                    "watcher reached its absolute session deadline",
                )?;
                return Ok(());
            }
            finish_after_error(&database, &workspace, &error)?;
            return Err(error);
        }
        idle_renewed = Instant::now();
        heartbeat_watcher_lease(&database, &workspace, true)?;
        let refreshed = match WatchScope::load(&config, &database, &overrides) {
            Ok(refreshed) => refreshed,
            Err(error) => {
                eprintln!("csgraph sync could not refresh watch roots: {error:#}");
                continue;
            }
        };
        if refreshed == scope {
            if let Err(error) = watcher.refresh_native_directories(&scope) {
                eprintln!(
                    "csgraph sync could not extend the native watcher ({error}); falling back to polling"
                );
                watcher = build_poll_watcher(
                    &scope,
                    sender.clone(),
                    poll_interval.unwrap_or(DEFAULT_POLL_INTERVAL),
                )?;
            }
        } else {
            let replacement = build_watcher(&refreshed, sender.clone(), poll_interval)?;
            watcher = replacement;
            scope = refreshed;
        }
    }
}

fn emit_sync(
    config: &Path,
    database: &Path,
    overrides: &ScanOverrides,
    synchronize_codegraph: bool,
    wall_time_cap_ms: u64,
) -> anyhow::Result<SyncSummary> {
    let summary = sync_workspace_with_wall_time_cap(
        config,
        database,
        overrides,
        synchronize_codegraph,
        wall_time_cap_ms,
    )?;
    println!(
        "{}",
        serde_json::to_string(&WatchOutput::SyncResult {
            schema_version: 1,
            summary: &summary,
        })?
    );
    std::io::stdout()
        .flush()
        .context("failed to flush sync result")?;
    Ok(summary)
}

async fn retry_sync(
    config: &Path,
    database: &Path,
    overrides: &ScanOverrides,
    synchronize_codegraph: bool,
    session_started: Instant,
    max_session_wall_time_ms: u64,
) -> anyhow::Result<()> {
    let mut delay = Duration::from_millis(250);
    for attempt in 1..=MAX_SYNC_ATTEMPTS {
        match emit_sync(
            config,
            database,
            overrides,
            synchronize_codegraph,
            remaining_session_ms(session_started, max_session_wall_time_ms),
        ) {
            Ok(_) => return Ok(()),
            Err(error)
                if error
                    .downcast_ref::<ApplicationError>()
                    .is_some_and(|error| {
                        matches!(error, ApplicationError::TransientExecution(_))
                    })
                    && attempt < MAX_SYNC_ATTEMPTS =>
            {
                eprintln!(
                    "csgraph sync pass {attempt}/{MAX_SYNC_ATTEMPTS} failed: {error:#}; retrying"
                );
                tokio::time::sleep(delay).await;
                delay = delay.saturating_mul(2);
            }
            Err(error)
                if error
                    .downcast_ref::<ApplicationError>()
                    .is_some_and(|error| {
                        matches!(error, ApplicationError::TransientExecution(_))
                    }) =>
            {
                eprintln!(
                    "csgraph sync pass {attempt}/{MAX_SYNC_ATTEMPTS} failed: {error:#}; stopping watcher"
                );
                return Err(error);
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("bounded retry loop always returns")
}

fn session_expired(session_started: Instant, maximum_ms: u64) -> bool {
    session_started.elapsed().as_millis() >= u128::from(maximum_ms)
}

fn remaining_session_ms(session_started: Instant, maximum_ms: u64) -> u64 {
    let elapsed = u64::try_from(session_started.elapsed().as_millis()).unwrap_or(u64::MAX);
    maximum_ms.saturating_sub(elapsed).max(1)
}

async fn wait_for_debounced_change(
    receiver: &mut mpsc::Receiver<()>,
    debounce: Duration,
    idle_deadline: Instant,
    session_deadline: Instant,
) -> anyhow::Result<WaitOutcome> {
    let heartbeat_deadline = tokio::time::Instant::now() + Duration::from_mins(1);
    let idle_deadline = tokio::time::Instant::from_std(idle_deadline);
    let session_deadline = tokio::time::Instant::from_std(session_deadline);
    tokio::select! {
        signal = super::shutdown_signal() => {
            signal.context("failed to listen for sync shutdown")?;
            return Ok(WaitOutcome::Shutdown);
        }
        event = receiver.recv() => {
            anyhow::ensure!(event.is_some(), "filesystem watcher stopped unexpectedly");
        }
        () = tokio::time::sleep_until(heartbeat_deadline) => return Ok(WaitOutcome::Heartbeat),
        () = tokio::time::sleep_until(idle_deadline) => return Ok(WaitOutcome::ExpiredIdle),
        () = tokio::time::sleep_until(session_deadline) => return Ok(WaitOutcome::ExpiredSession),
    }

    let started = tokio::time::Instant::now();
    let maximum = started + debounce.saturating_mul(5).max(Duration::from_secs(10));
    let mut quiet_deadline = started + debounce;
    loop {
        let deadline = quiet_deadline.min(maximum);
        tokio::select! {
            signal = super::shutdown_signal() => {
                signal.context("failed to listen for sync shutdown")?;
                return Ok(WaitOutcome::Shutdown);
            }
            event = receiver.recv() => {
                anyhow::ensure!(event.is_some(), "filesystem watcher stopped unexpectedly");
                quiet_deadline = tokio::time::Instant::now() + debounce;
            }
            () = tokio::time::sleep_until(idle_deadline) => return Ok(WaitOutcome::ExpiredIdle),
            () = tokio::time::sleep_until(session_deadline) => return Ok(WaitOutcome::ExpiredSession),
            () = tokio::time::sleep_until(deadline) => return Ok(WaitOutcome::Change),
        }
    }
}

fn finish_after_error(
    database: &Path,
    workspace: &str,
    error: &anyhow::Error,
) -> anyhow::Result<()> {
    let is_limit = error
        .downcast_ref::<ApplicationError>()
        .is_some_and(|error| {
            matches!(
                error,
                ApplicationError::ExecutionLimit(_) | ApplicationError::ExtractionLimit(_)
            )
        });
    let state = if is_limit { "failed_limit" } else { "stale" };
    finish_and_emit(database, workspace, state, &format!("{error:#}"))
}

fn finish_and_emit(
    database: &Path,
    workspace: &str,
    state: &str,
    detail: &str,
) -> anyhow::Result<()> {
    let bounded_detail = detail.chars().take(512).collect::<String>();
    finish_watcher_lease(database, workspace, state, Some(&bounded_detail))?;
    println!(
        "{}",
        serde_json::to_string(&WatchOutput::Termination {
            schema_version: 1,
            state,
            detail: &bounded_detail,
        })?
    );
    std::io::stdout()
        .flush()
        .context("failed to flush watcher termination")?;
    Ok(())
}

fn build_watcher(
    scope: &WatchScope,
    sender: mpsc::Sender<()>,
    requested_poll_interval: Option<Duration>,
) -> anyhow::Result<ActiveWatcher> {
    let automatic_polling = wsl_windows_mount(scope);
    if requested_poll_interval.is_some() || automatic_polling {
        if automatic_polling && requested_poll_interval.is_none() {
            eprintln!(
                "csgraph sync selected polling because a repository is on a WSL Windows mount"
            );
        }
        return build_poll_watcher(
            scope,
            sender,
            requested_poll_interval.unwrap_or(DEFAULT_POLL_INTERVAL),
        );
    }

    match build_native_watcher(scope, sender.clone()) {
        Ok(watcher) => Ok(watcher),
        Err(error) => {
            eprintln!("csgraph sync native watcher unavailable ({error}); falling back to polling");
            build_poll_watcher(scope, sender, DEFAULT_POLL_INTERVAL)
        }
    }
}

fn build_native_watcher(
    scope: &WatchScope,
    sender: mpsc::Sender<()>,
) -> notify::Result<ActiveWatcher> {
    let callback_scope = scope.clone();
    let mut watcher = RecommendedWatcher::new(
        move |result| forward_event(result, &callback_scope, &sender),
        Config::default().with_follow_symlinks(false),
    )?;
    add_native_watch_entries(&mut watcher, scope)?;
    Ok(ActiveWatcher::Native(Box::new(watcher)))
}

fn build_poll_watcher(
    scope: &WatchScope,
    sender: mpsc::Sender<()>,
    interval: Duration,
) -> anyhow::Result<ActiveWatcher> {
    let callback_scope = scope.clone();
    let config = Config::default()
        .with_poll_interval(interval)
        .with_compare_contents(true)
        .with_follow_symlinks(false);
    let mut watcher = PollWatcher::new(
        move |result| forward_event(result, &callback_scope, &sender),
        config,
    )
    .context("failed to create polling filesystem watcher")?;
    add_watch_entries(&mut watcher, scope).context("failed to install polling watch roots")?;
    Ok(ActiveWatcher::Polling(Box::new(watcher)))
}

fn add_watch_entries<W: Watcher>(watcher: &mut W, scope: &WatchScope) -> notify::Result<()> {
    for (path, mode) in scope.watch_entries() {
        watcher.watch(&path, mode)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn add_native_watch_entries<W: Watcher>(watcher: &mut W, scope: &WatchScope) -> notify::Result<()> {
    let mut installed = BTreeSet::new();
    if let Some(parent) = scope.config.parent() {
        watcher.watch(parent, RecursiveMode::NonRecursive)?;
        installed.insert(parent.to_path_buf());
    }
    let mut pending = scope
        .repositories
        .iter()
        .map(|repository| repository.root.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    let mut visited = BTreeSet::new();
    while let Some(directory) = pending.pop() {
        if !visited.insert(directory.clone()) {
            continue;
        }
        if installed.insert(directory.clone()) {
            watcher.watch(&directory, RecursiveMode::NonRecursive)?;
        }
        let entries = std::fs::read_dir(&directory).map_err(notify::Error::io)?;
        for entry in entries {
            let entry = entry.map_err(notify::Error::io)?;
            let file_type = entry.file_type().map_err(notify::Error::io)?;
            if file_type.is_dir()
                && !file_type.is_symlink()
                && scope.should_watch_directory(&entry.path())
            {
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn add_native_watch_entries<W: Watcher>(watcher: &mut W, scope: &WatchScope) -> notify::Result<()> {
    add_watch_entries(watcher, scope)
}

fn forward_event(result: notify::Result<Event>, scope: &WatchScope, sender: &mpsc::Sender<()>) {
    match result {
        Ok(event) if scope.relevant_event(&event) => {
            let _ignored = sender.try_send(());
        }
        Ok(_) => {}
        Err(error)
            if !error.paths.is_empty()
                && error
                    .paths
                    .iter()
                    .all(|path| database_artifact(path, &scope.database)) => {}
        Err(error) => {
            eprintln!("csgraph sync filesystem watcher reported: {error}");
            let _ignored = sender.try_send(());
        }
    }
}

fn add_watch_entry(
    entries: &mut Vec<(PathBuf, RecursiveMode)>,
    path: PathBuf,
    mode: RecursiveMode,
) {
    if entries.iter().any(|(existing, existing_mode)| {
        *existing_mode == RecursiveMode::Recursive && path.starts_with(existing)
    }) {
        return;
    }
    if let Some(existing) = entries.iter_mut().find(|(existing, _)| existing == &path) {
        if mode == RecursiveMode::Recursive {
            existing.1 = mode;
        }
        return;
    }
    if mode == RecursiveMode::Recursive {
        entries.retain(|(existing, _)| !existing.starts_with(&path));
    }
    entries.push((path, mode));
}

fn database_artifact(path: &Path, database: &Path) -> bool {
    if path == database {
        return true;
    }
    let work_database = {
        let mut value = database.as_os_str().to_os_string();
        value.push(".work-v1.db");
        PathBuf::from(value)
    };
    if path == work_database {
        return true;
    }
    let Some(database_name) = database.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    path.parent() == database.parent()
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                [
                    "-wal",
                    "-shm",
                    "-journal",
                    ".work-v1.db",
                    ".work-v1.db-wal",
                    ".work-v1.db-shm",
                    ".work-v1.db-journal",
                ]
                .iter()
                .any(|suffix| name == format!("{database_name}{suffix}"))
            })
}

fn absolute_path(path: &Path) -> anyhow::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory")?
            .join(path)
    };
    if absolute.exists() {
        return std::fs::canonicalize(&absolute)
            .with_context(|| format!("failed to resolve `{}`", absolute.display()));
    }
    Ok(absolute)
}

#[cfg(target_os = "linux")]
fn wsl_windows_mount(scope: &WatchScope) -> bool {
    let is_wsl = std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .is_ok_and(|release| release.to_ascii_lowercase().contains("microsoft"));
    is_wsl
        && scope.repositories.iter().any(|repository| {
            let mut components = repository.root.components();
            matches!(components.next(), Some(Component::RootDir))
                && components
                    .next()
                    .is_some_and(|component| component.as_os_str() == "mnt")
                && components.next().is_some_and(|component| {
                    let drive = component.as_os_str().to_string_lossy();
                    drive.len() == 1 && drive.as_bytes()[0].is_ascii_alphabetic()
                })
        })
}

#[cfg(not(target_os = "linux"))]
const fn wsl_windows_mount(_scope: &WatchScope) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(excludes: &[&str], includes: &[&str]) -> IgnorePolicy {
        IgnorePolicy::new(
            excludes.iter().map(ToString::to_string).collect(),
            code_system_graph::ConfigSource::Default,
            includes.iter().map(ToString::to_string).collect(),
            code_system_graph::ConfigSource::Default,
        )
        .expect("built-in ignore policy")
    }

    #[test]
    fn scope_should_ignore_generated_state_and_database_sidecars() {
        let scope = WatchScope {
            config: PathBuf::from("/workspace/code-system-graph.yaml"),
            database: PathBuf::from("/workspace/.state/graph.db"),
            repositories: vec![WatchRepository {
                root: PathBuf::from("/workspace/repo"),
                ignore_policy: policy(&[], &[]),
                explicit_paths: vec![PathBuf::from(".code-system-graph.yaml")],
            }],
        };
        assert!(scope.relevant_path(Path::new("/workspace/repo/src/lib.rs")));
        assert!(scope.relevant_path(Path::new("/workspace/code-system-graph.yaml")));
        assert!(!scope.relevant_path(Path::new("/workspace/repo/.codegraph/codegraph.db-wal")));
        assert!(!scope.relevant_path(Path::new("/workspace/.state/graph.db-wal")));
        assert!(!scope.relevant_path(Path::new("/workspace/.state/graph.db.work-v1.db-wal")));
        assert!(!scope.relevant_path(Path::new("/workspace/.state/graph.db.work-v1.db-shm")));
    }

    #[test]
    fn scope_should_apply_custom_excludes_and_default_includes() {
        let scope = WatchScope {
            config: PathBuf::from("/workspace/code-system-graph.yaml"),
            database: PathBuf::from("/workspace/.state/graph.db"),
            repositories: vec![WatchRepository {
                root: PathBuf::from("/workspace/repo"),
                ignore_policy: policy(&["./generated//./**"], &["./vendor//internal-sdk/./**"]),
                explicit_paths: vec![PathBuf::from("generated/explicit.yaml")],
            }],
        };

        assert_eq!(
            (
                scope.relevant_path(Path::new("/workspace/repo/generated/output.rs")),
                scope.relevant_path(Path::new("/workspace/repo/vendor/internal-sdk/src/lib.rs")),
                scope.relevant_path(Path::new("/workspace/repo/vendor/external/lib.rs")),
                scope.relevant_path(Path::new("/workspace/repo/generated/explicit.yaml")),
            ),
            (false, true, false, true)
        );
    }

    #[test]
    fn scope_should_preserve_policies_for_aliases_with_the_same_checkout() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let repository = temporary.path().join("repo/generated/output");
        std::fs::create_dir_all(&repository)?;
        let source = repository.join("lib.rs");
        std::fs::write(&source, "pub fn observed() {}\n")?;
        let config = temporary.path().join("code-system-graph.yaml");
        std::fs::write(
            &config,
            "version: 1\nname: shared-checkout\nrepos:\n  a-restrictive:\n    path: repo\n    excludes:\n      - generated/*\n  b-permissive:\n    path: repo\n",
        )?;

        let scope = WatchScope::load(
            &config,
            &temporary.path().join("graph.db"),
            &ScanOverrides::default(),
        )?;

        assert_eq!(scope.repositories.len(), 2);
        assert!(scope.relevant_path(&source));
        assert!(scope.should_watch_directory(&repository));
        Ok(())
    }

    #[test]
    fn recursive_parent_should_cover_nested_watch_roots() {
        let mut entries = vec![(PathBuf::from("/workspace"), RecursiveMode::NonRecursive)];
        add_watch_entry(
            &mut entries,
            PathBuf::from("/workspace/repo/nested"),
            RecursiveMode::Recursive,
        );
        add_watch_entry(
            &mut entries,
            PathBuf::from("/workspace/repo"),
            RecursiveMode::Recursive,
        );
        assert_eq!(
            entries,
            vec![
                (PathBuf::from("/workspace"), RecursiveMode::NonRecursive),
                (PathBuf::from("/workspace/repo"), RecursiveMode::Recursive),
            ]
        );
    }
}

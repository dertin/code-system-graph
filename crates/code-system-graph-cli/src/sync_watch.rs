//! Portable filesystem watching for the `sync --watch` command.

#[cfg(target_os = "linux")]
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(target_os = "linux")]
use std::path::Component;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Context;
use code_system_graph::{
    ApplicationError, IgnorePolicy, ScanOverrides, SyncSummary, finish_watcher_lease, heartbeat_watcher_lease, load_persisted_watch_targets, start_watcher_lease, sync_workspace_with_wall_time_cap
};
use code_system_graph_core::RepositoryPathMatcher;
use notify::{Config, Event, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use sysinfo::{Pid, ProcessesToUpdate, System};
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

#[derive(Debug, Clone, Copy)]
enum WatchSignal {
    Dirty,
    RefreshDirectories,
}

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);
const MAX_SYNC_ATTEMPTS: usize = 3;
const WATCH_EVENT_PROTOCOL_MAX_BYTES: usize = 128;

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WatchEventMessage {
    Ready {
        schema_version: u8,
    },
    Dirty {
        schema_version: u8,
        #[serde(default)]
        refresh_scope: bool,
    },
}

#[derive(Debug, Default)]
struct WatchEventState {
    initial_ready: bool,
    refresh_started: Option<Instant>,
    refresh_scope: bool,
    failed: bool,
}

struct WatchEventWorker {
    child: Child,
    group: code_system_graph::SupervisedProcessGroup,
    receiver: mpsc::Receiver<()>,
    state: Arc<Mutex<WatchEventState>>,
    system: System,
}

impl WatchEventWorker {
    fn spawn(
        config: &Path,
        database: &Path,
        workspace: &str,
        poll_interval: Option<Duration>,
        policy: &code_system_graph_core::ExecutionPolicy,
    ) -> anyhow::Result<Self> {
        let executable = std::env::current_exe().context("failed to locate watcher worker")?;
        let mut command = Command::new(executable);
        command
            .arg("__watch-events-v1")
            .arg("--config")
            .arg(config)
            .arg("--database")
            .arg(database)
            .arg("--workspace")
            .arg(workspace)
            .arg("--max-wall-time-ms")
            .arg(policy.max_scan_wall_time_ms.to_string())
            .arg("--max-no-progress-time-ms")
            .arg(policy.max_no_progress_time_ms.to_string())
            .arg("--max-memory-bytes")
            .arg(policy.max_worker_memory_bytes.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(interval) = poll_interval {
            command.arg("--poll-interval-ms").arg(
                u64::try_from(interval.as_millis())
                    .unwrap_or(u64::MAX)
                    .to_string(),
            );
        }
        code_system_graph::configure_supervised_process_group(&mut command);
        let mut child = command
            .spawn()
            .context("failed to start filesystem watcher worker")?;
        let group = match code_system_graph::SupervisedProcessGroup::attach(&child) {
            Ok(group) => group,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                anyhow::bail!("failed to isolate filesystem watcher worker: {error}");
            }
        };
        let stdout = child
            .stdout
            .take()
            .context("filesystem watcher worker stdout was unavailable")?;
        let (sender, receiver) = mpsc::channel(1);
        let state = Arc::new(Mutex::new(WatchEventState::default()));
        spawn_watch_event_reader(stdout, sender, Arc::clone(&state));
        let mut worker = Self {
            child,
            group,
            receiver,
            state,
            system: System::new(),
        };
        worker.wait_until_ready(policy)?;
        Ok(worker)
    }

    fn wait_until_ready(
        &mut self,
        policy: &code_system_graph_core::ExecutionPolicy,
    ) -> anyhow::Result<()> {
        let started = Instant::now();
        loop {
            self.ensure_running(policy)?;
            let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let maximum = policy
                .max_scan_wall_time_ms
                .min(policy.max_no_progress_time_ms);
            if elapsed > maximum {
                return Err(watcher_execution_limit(
                    code_system_graph_core::ExecutionResource::NoProgressTimeMs,
                    elapsed,
                    maximum,
                ));
            }
            if self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("watcher protocol state was poisoned"))?
                .initial_ready
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn ensure_running(
        &mut self,
        policy: &code_system_graph_core::ExecutionPolicy,
    ) -> anyhow::Result<()> {
        if self.child.try_wait()?.is_some() {
            anyhow::bail!("filesystem watcher worker stopped unexpectedly");
        }
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("watcher protocol state was poisoned"))?;
        if state.failed {
            anyhow::bail!("filesystem watcher worker protocol failed");
        }
        if let Some(started) = state.refresh_started {
            let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            if elapsed > policy.max_no_progress_time_ms {
                return Err(watcher_execution_limit(
                    code_system_graph_core::ExecutionResource::NoProgressTimeMs,
                    elapsed,
                    policy.max_no_progress_time_ms,
                ));
            }
        }
        drop(state);
        let pid = Pid::from_u32(self.child.id());
        self.system
            .refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        let observed = self.system.process(pid).map_or(0, sysinfo::Process::memory);
        if observed > policy.max_worker_memory_bytes {
            return Err(watcher_execution_limit(
                code_system_graph_core::ExecutionResource::WorkerMemoryBytes,
                observed,
                policy.max_worker_memory_bytes,
            ));
        }
        Ok(())
    }

    fn receiver(&mut self) -> &mut mpsc::Receiver<()> {
        &mut self.receiver
    }

    fn take_scope_refresh(&self) -> anyhow::Result<bool> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("watcher protocol state was poisoned"))?;
        Ok(std::mem::take(&mut state.refresh_scope))
    }
}

impl Drop for WatchEventWorker {
    fn drop(&mut self) {
        code_system_graph::terminate_supervised_process(&mut self.child, &self.group, 0);
    }
}

fn spawn_watch_event_reader(
    stdout: impl std::io::Read + Send + 'static,
    sender: mpsc::Sender<()>,
    state: Arc<Mutex<WatchEventState>>,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::with_capacity(WATCH_EVENT_PROTOCOL_MAX_BYTES, stdout);
        loop {
            let mut line = Vec::new();
            match Read::by_ref(&mut reader)
                .take(u64::try_from(WATCH_EVENT_PROTOCOL_MAX_BYTES + 1).unwrap_or(u64::MAX))
                .read_until(b'\n', &mut line)
            {
                Ok(0) | Err(_) => break,
                Ok(_) if line.len() > WATCH_EVENT_PROTOCOL_MAX_BYTES || !line.ends_with(b"\n") => {
                    break;
                }
                Ok(_) => {}
            }
            let Ok(message) = serde_json::from_slice::<WatchEventMessage>(&line) else {
                break;
            };
            let Ok(mut current) = state.lock() else {
                return;
            };
            match message {
                WatchEventMessage::Ready { schema_version: 1 } => {
                    current.initial_ready = true;
                    current.refresh_started = None;
                }
                WatchEventMessage::Dirty {
                    schema_version: 1,
                    refresh_scope,
                } => {
                    current.refresh_started.get_or_insert_with(Instant::now);
                    current.refresh_scope |= refresh_scope;
                    drop(current);
                    match sender.try_send(()) {
                        Ok(()) | Err(mpsc::error::TrySendError::Full(())) => {}
                        Err(mpsc::error::TrySendError::Closed(())) => return,
                    }
                }
                WatchEventMessage::Ready { .. } | WatchEventMessage::Dirty { .. } => break,
            }
        }
        if let Ok(mut current) = state.lock() {
            current.failed = true;
        }
    });
}

fn watcher_execution_limit(
    resource: code_system_graph_core::ExecutionResource,
    observed: u64,
    maximum: u64,
) -> anyhow::Error {
    anyhow::Error::new(ApplicationError::ExecutionLimit(
        code_system_graph_core::ExecutionLimitExceeded {
            run_id: "watcher-events".to_owned(),
            phase: code_system_graph_core::JobPhase::Discovery,
            resource,
            observed,
            maximum,
            completed_units: 0,
        },
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WatchScope {
    config: PathBuf,
    database: PathBuf,
    repositories: Vec<WatchRepository>,
}

#[derive(Debug, Clone)]
struct WatchRepository {
    root: PathBuf,
    ignore_policy: IgnorePolicy,
    ignore_matcher: Arc<Mutex<RepositoryPathMatcher>>,
    explicit_paths: Vec<PathBuf>,
}

impl WatchRepository {
    fn new(root: PathBuf, ignore_policy: IgnorePolicy, explicit_paths: Vec<PathBuf>) -> Self {
        let ignore_matcher = RepositoryPathMatcher::new(&root, ignore_policy.clone());
        Self {
            root,
            ignore_policy,
            ignore_matcher: Arc::new(Mutex::new(ignore_matcher)),
            explicit_paths,
        }
    }

    fn excluded(&self, relative: &Path, directory: bool) -> anyhow::Result<bool> {
        let mut matcher = self
            .ignore_matcher
            .lock()
            .map_err(|_| anyhow::anyhow!("repository ignore matcher was poisoned"))?;
        matcher
            .excludes(relative, directory)
            .map_err(anyhow::Error::new)
    }

    fn relevant(&self, path: &Path) -> anyhow::Result<bool> {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return Ok(false);
        };
        if (self.ignore_policy.use_gitignore()
            && relative
                .file_name()
                .is_some_and(|name| name == ".gitignore"))
            || self.explicit_paths.iter().any(|explicit| {
                relative == explicit
                    || explicit.starts_with(relative)
                    || relative.starts_with(explicit)
            })
        {
            return Ok(true);
        }
        Ok(!self.excluded(relative, path.is_dir())?)
    }

    #[cfg(any(target_os = "linux", test))]
    fn should_watch_directory(&self, path: &Path) -> anyhow::Result<bool> {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return Ok(false);
        };
        if self
            .explicit_paths
            .iter()
            .any(|explicit| explicit.starts_with(relative))
        {
            return Ok(true);
        }
        Ok(!self.excluded(relative, true)?)
    }
}

impl PartialEq for WatchRepository {
    fn eq(&self, other: &Self) -> bool {
        self.root == other.root
            && self.ignore_policy == other.ignore_policy
            && self.explicit_paths == other.explicit_paths
    }
}

impl Eq for WatchRepository {}

impl WatchScope {
    fn load(config: &Path, database: &Path, workspace: &str) -> anyhow::Result<Self> {
        let mut repositories = load_persisted_watch_targets(database, workspace)?
            .into_iter()
            .map(|target| {
                WatchRepository::new(target.path, target.ignore_policy, target.explicit_paths)
            })
            .collect::<Vec<_>>();
        repositories.sort_by(|left, right| left.root.cmp(&right.root));
        Ok(Self {
            config: absolute_path(config)?,
            database: absolute_path(database)?,
            repositories,
        })
    }

    fn relevant_event(&self, event: &Event) -> anyhow::Result<bool> {
        if event.paths.is_empty() {
            return Ok(true);
        }
        for path in &event.paths {
            if self.relevant_path(path)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn changes_enabled_gitignore(&self, event: &Event) -> bool {
        event.paths.iter().any(|path| {
            path.file_name().is_some_and(|name| name == ".gitignore")
                && self.repositories.iter().any(|repository| {
                    repository.ignore_policy.use_gitignore()
                        && path.strip_prefix(&repository.root).is_ok()
                })
        })
    }

    fn relevant_path(&self, path: &Path) -> anyhow::Result<bool> {
        if path == self.config {
            return Ok(true);
        }
        if database_artifact(path, &self.database) {
            return Ok(false);
        }
        for repository in &self.repositories {
            if repository.relevant(path)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    #[cfg(any(target_os = "linux", test))]
    fn should_watch_directory(&self, path: &Path) -> anyhow::Result<bool> {
        for repository in &self.repositories {
            if repository.should_watch_directory(path)? {
                return Ok(true);
            }
        }
        Ok(false)
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

struct NativeWatcherState {
    watcher: Box<RecommendedWatcher>,
    #[cfg(target_os = "linux")]
    installed: BTreeSet<PathBuf>,
}

enum ActiveWatcher {
    Native(NativeWatcherState),
    Polling(Box<PollWatcher>),
}

impl ActiveWatcher {
    #[cfg(target_os = "linux")]
    fn refresh_native_directories(
        &mut self,
        scope: &WatchScope,
        policy: &code_system_graph_core::ExecutionPolicy,
    ) -> anyhow::Result<()> {
        if let Self::Native(state) = self {
            let mut guard = WatchSetupGuard::new(policy);
            add_native_watch_entries(
                state.watcher.as_mut(),
                scope,
                &mut state.installed,
                &mut guard,
            )?;
        } else if let Self::Polling(watcher) = self {
            let _keep_alive = watcher.as_ref();
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    fn refresh_native_directories(
        &mut self,
        _scope: &WatchScope,
        _policy: &code_system_graph_core::ExecutionPolicy,
    ) -> anyhow::Result<()> {
        match self {
            Self::Native(state) => {
                let _keep_alive = state.watcher.as_ref();
            }
            Self::Polling(watcher) => {
                let _keep_alive = watcher.as_ref();
            }
        }
        Ok(())
    }
}

struct WatchSetupGuard {
    started: Instant,
    last_progress: Instant,
    max_wall_time_ms: u64,
    max_no_progress_time_ms: u64,
    max_memory_bytes: u64,
    inspected_units: u64,
    completed_units: u64,
    system: System,
}

impl WatchSetupGuard {
    fn new(policy: &code_system_graph_core::ExecutionPolicy) -> Self {
        let now = Instant::now();
        Self {
            started: now,
            last_progress: now,
            max_wall_time_ms: policy.max_scan_wall_time_ms,
            max_no_progress_time_ms: policy.max_no_progress_time_ms,
            max_memory_bytes: policy.max_worker_memory_bytes,
            inspected_units: 0,
            completed_units: 0,
            system: System::new(),
        }
    }

    fn inspect(&mut self) -> anyhow::Result<()> {
        self.inspected_units = self.inspected_units.saturating_add(1);
        self.check_time()?;
        if self.inspected_units == 1 || self.inspected_units.is_multiple_of(1_024) {
            let pid = Pid::from_u32(std::process::id());
            self.system
                .refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
            let observed = self.system.process(pid).map_or(0, sysinfo::Process::memory);
            if observed > self.max_memory_bytes {
                return Err(self.limit(
                    code_system_graph_core::ExecutionResource::WorkerMemoryBytes,
                    observed,
                    self.max_memory_bytes,
                ));
            }
        }
        Ok(())
    }

    fn completed_directory(&mut self) -> anyhow::Result<()> {
        self.completed_units = self.completed_units.saturating_add(1);
        self.last_progress = Instant::now();
        self.inspect()
    }

    fn check_time(&self) -> anyhow::Result<()> {
        let wall_time_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        if wall_time_ms > self.max_wall_time_ms {
            return Err(self.limit(
                code_system_graph_core::ExecutionResource::WallTimeMs,
                wall_time_ms,
                self.max_wall_time_ms,
            ));
        }
        let no_progress_ms =
            u64::try_from(self.last_progress.elapsed().as_millis()).unwrap_or(u64::MAX);
        if no_progress_ms > self.max_no_progress_time_ms {
            return Err(self.limit(
                code_system_graph_core::ExecutionResource::NoProgressTimeMs,
                no_progress_ms,
                self.max_no_progress_time_ms,
            ));
        }
        Ok(())
    }

    fn limit(
        &self,
        resource: code_system_graph_core::ExecutionResource,
        observed: u64,
        maximum: u64,
    ) -> anyhow::Error {
        anyhow::Error::new(ApplicationError::ExecutionLimit(
            code_system_graph_core::ExecutionLimitExceeded {
                run_id: "watcher-setup".to_owned(),
                phase: code_system_graph_core::JobPhase::Discovery,
                resource,
                observed,
                maximum,
                completed_units: self.completed_units,
            },
        ))
    }
}

/// Runs the private filesystem-event process used by the watch supervisor.
pub(crate) async fn run_watch_event_worker(
    config: PathBuf,
    database: PathBuf,
    workspace: String,
    poll_interval_ms: Option<u64>,
    max_wall_time_ms: u64,
    max_no_progress_time_ms: u64,
    max_memory_bytes: u64,
) -> anyhow::Result<()> {
    let policy = code_system_graph_core::ExecutionPolicy {
        max_scan_wall_time_ms: max_wall_time_ms,
        max_no_progress_time_ms,
        max_worker_memory_bytes: max_memory_bytes,
        ..code_system_graph_core::ExecutionPolicy::default()
    };
    let scope = WatchScope::load(&config, &database, &workspace)?;
    let (sender, mut receiver) = mpsc::channel(1);
    let refresh_required = Arc::new(AtomicBool::new(false));
    let scope_refresh_required = Arc::new(AtomicBool::new(false));
    let requested_poll = poll_interval_ms.map(Duration::from_millis);
    let mut watcher = build_watcher(
        &scope,
        sender.clone(),
        Arc::clone(&refresh_required),
        Arc::clone(&scope_refresh_required),
        requested_poll,
        &policy,
    )?;
    emit_watch_event(&WatchEventMessage::Ready { schema_version: 1 })?;
    while let Some(signal) = receiver.recv().await {
        emit_watch_event(&WatchEventMessage::Dirty {
            schema_version: 1,
            refresh_scope: scope_refresh_required.swap(false, Ordering::AcqRel),
        })?;
        let refresh = refresh_required.swap(false, Ordering::AcqRel)
            || matches!(signal, WatchSignal::RefreshDirectories);
        if refresh {
            match watcher.refresh_native_directories(&scope, &policy) {
                Ok(()) => {}
                Err(error) if watcher_limit_exceeded(&error) => return Err(error),
                Err(_) => {
                    watcher = build_poll_watcher(
                        &scope,
                        sender.clone(),
                        Arc::clone(&refresh_required),
                        Arc::clone(&scope_refresh_required),
                        requested_poll.unwrap_or(DEFAULT_POLL_INTERVAL),
                        &policy,
                    )?;
                }
            }
        }
        emit_watch_event(&WatchEventMessage::Ready { schema_version: 1 })?;
    }
    anyhow::bail!("filesystem event channel stopped")
}

fn emit_watch_event(message: &WatchEventMessage) -> anyhow::Result<()> {
    let encoded = serde_json::to_vec(message)?;
    anyhow::ensure!(
        encoded.len().saturating_add(1) <= WATCH_EVENT_PROTOCOL_MAX_BYTES,
        "filesystem watcher protocol message exceeded its bound"
    );
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(&encoded)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
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
    let (workspace, owner_token, policy) = start_watcher_lease(&config, &database)?;
    let session_started = Instant::now();
    let mut idle_renewed = session_started;
    let mut last_pass_started = session_started;

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
                &owner_token,
                "expired_session",
                "watcher reached its absolute session deadline",
            )?;
            return Ok(());
        }
        finish_after_error(&database, &workspace, &owner_token, &error)?;
        return Err(watcher_public_error(&error));
    }
    let mut scope = match WatchScope::load(&config, &database, &workspace) {
        Ok(scope) => scope,
        Err(error) => {
            finish_after_error(&database, &workspace, &owner_token, &error)?;
            return Err(watcher_public_error(&error));
        }
    };
    let mut watcher =
        match WatchEventWorker::spawn(&config, &database, &workspace, poll_interval, &policy) {
            Ok(watcher) => watcher,
            Err(error) => {
                finish_after_error(&database, &workspace, &owner_token, &error)?;
                return Err(watcher_public_error(&error));
            }
        };
    // The first supervised sync completed before watch installation. Run one incremental catch-up
    // after the event channel is live so changes made during installation cannot be missed.
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
                &owner_token,
                "expired_session",
                "watcher reached its absolute session deadline",
            )?;
            return Ok(());
        }
        finish_after_error(&database, &workspace, &owner_token, &error)?;
        return Err(watcher_public_error(&error));
    }
    loop {
        if let Err(error) = watcher.ensure_running(&policy) {
            finish_after_error(&database, &workspace, &owner_token, &error)?;
            return Err(watcher_public_error(&error));
        }
        if let Err(error) = heartbeat_watcher_lease(&database, &workspace, &owner_token, false) {
            let error = anyhow::Error::new(error);
            finish_after_error(&database, &workspace, &owner_token, &error)?;
            return Err(watcher_public_error(&error));
        }
        let idle_deadline = idle_renewed + Duration::from_millis(policy.watch_idle_timeout_ms);
        let session_deadline =
            session_started + Duration::from_millis(policy.max_watch_session_wall_time_ms);
        let wait_outcome = match wait_for_debounced_change(
            watcher.receiver(),
            debounce,
            idle_deadline,
            session_deadline,
        )
        .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                finish_after_error(&database, &workspace, &owner_token, &error)?;
                return Err(watcher_public_error(&error));
            }
        };
        match wait_outcome {
            WaitOutcome::Change => {}
            WaitOutcome::Heartbeat => continue,
            WaitOutcome::Shutdown => {
                finish_and_emit(
                    &database,
                    &workspace,
                    &owner_token,
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
                    &owner_token,
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
                    &owner_token,
                    "expired_session",
                    "watcher reached its absolute session deadline",
                )?;
                drop(watcher);
                return Ok(());
            }
        }
        let refresh_scope = match watcher.take_scope_refresh() {
            Ok(refresh_scope) => refresh_scope,
            Err(error) => {
                finish_after_error(&database, &workspace, &owner_token, &error)?;
                return Err(watcher_public_error(&error));
            }
        };
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
                &owner_token,
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
                    &owner_token,
                    "expired_session",
                    "watcher reached its absolute session deadline",
                )?;
                return Ok(());
            }
            finish_after_error(&database, &workspace, &owner_token, &error)?;
            return Err(watcher_public_error(&error));
        }
        idle_renewed = Instant::now();
        if let Err(error) = heartbeat_watcher_lease(&database, &workspace, &owner_token, true) {
            let error = anyhow::Error::new(error);
            finish_after_error(&database, &workspace, &owner_token, &error)?;
            return Err(watcher_public_error(&error));
        }
        let refreshed = match WatchScope::load(&config, &database, &workspace) {
            Ok(refreshed) => refreshed,
            Err(error) => {
                eprintln!(
                    "csgraph sync could not refresh watch roots: {}",
                    watcher_error_detail(&error)
                );
                continue;
            }
        };
        if refresh_scope || refreshed != scope {
            let replacement = match WatchEventWorker::spawn(
                &config,
                &database,
                &workspace,
                poll_interval,
                &policy,
            ) {
                Ok(watcher) => watcher,
                Err(error) => {
                    finish_after_error(&database, &workspace, &owner_token, &error)?;
                    return Err(watcher_public_error(&error));
                }
            };
            // Keep the old worker alive until the replacement is ready, then run a supervised
            // catch-up while both scopes are observed. A change during installation is therefore
            // either included by this pass or remains coalesced in the replacement channel.
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
                        &owner_token,
                        "expired_session",
                        "watcher reached its absolute session deadline",
                    )?;
                    return Ok(());
                }
                finish_after_error(&database, &workspace, &owner_token, &error)?;
                return Err(watcher_public_error(&error));
            }
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
                    "csgraph sync pass {attempt}/{MAX_SYNC_ATTEMPTS} failed: {}; retrying",
                    watcher_error_detail(&error)
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
                    "csgraph sync pass {attempt}/{MAX_SYNC_ATTEMPTS} failed: {}; stopping watcher",
                    watcher_error_detail(&error)
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
    owner_token: &str,
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
    let detail = watcher_error_detail(error);
    finish_and_emit(database, workspace, owner_token, state, &detail)
}

fn watcher_error_detail(error: &anyhow::Error) -> String {
    let Some(application) = error.downcast_ref::<ApplicationError>() else {
        return "watcher operation failed".to_owned();
    };
    match application {
        ApplicationError::ExecutionLimit(limit) => format!(
            "execution limit exceeded: phase={:?}, resource={:?}, observed={}, maximum={}",
            limit.phase, limit.resource, limit.observed, limit.maximum
        ),
        ApplicationError::ExtractionLimit(limit) => format!(
            "extraction limit exceeded: extractor={}, resource={:?}, observed={}, maximum={}",
            limit.extractor, limit.resource, limit.observed, limit.maximum
        ),
        _ => format!(
            "supervised sync failed with {:?}",
            super::application_exit_code(application)
        ),
    }
}

fn watcher_public_error(error: &anyhow::Error) -> anyhow::Error {
    let exit_code = error
        .downcast_ref::<ApplicationError>()
        .map_or(code_system_graph_core::ExitCode::Internal, |application| {
            code_system_graph::application_exit_code(application)
        });
    anyhow::Error::new(ApplicationError::SupervisedApplication {
        exit_code,
        message: watcher_error_detail(error),
    })
}

fn finish_and_emit(
    database: &Path,
    workspace: &str,
    owner_token: &str,
    state: &str,
    detail: &str,
) -> anyhow::Result<()> {
    let bounded_detail = detail.chars().take(512).collect::<String>();
    finish_watcher_lease(
        database,
        workspace,
        owner_token,
        state,
        Some(&bounded_detail),
    )?;
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
    sender: mpsc::Sender<WatchSignal>,
    refresh_required: Arc<AtomicBool>,
    scope_refresh_required: Arc<AtomicBool>,
    requested_poll_interval: Option<Duration>,
    policy: &code_system_graph_core::ExecutionPolicy,
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
            refresh_required,
            scope_refresh_required,
            requested_poll_interval.unwrap_or(DEFAULT_POLL_INTERVAL),
            policy,
        );
    }

    match build_native_watcher(
        scope,
        sender.clone(),
        Arc::clone(&refresh_required),
        Arc::clone(&scope_refresh_required),
        policy,
    ) {
        Ok(watcher) => Ok(watcher),
        Err(error) if watcher_limit_exceeded(&error) => Err(error),
        Err(error) => {
            eprintln!("csgraph sync native watcher unavailable ({error}); falling back to polling");
            build_poll_watcher(
                scope,
                sender,
                refresh_required,
                scope_refresh_required,
                DEFAULT_POLL_INTERVAL,
                policy,
            )
        }
    }
}

fn watcher_limit_exceeded(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ApplicationError>()
        .is_some_and(|error| matches!(error, ApplicationError::ExecutionLimit(_)))
}

fn build_native_watcher(
    scope: &WatchScope,
    sender: mpsc::Sender<WatchSignal>,
    refresh_required: Arc<AtomicBool>,
    scope_refresh_required: Arc<AtomicBool>,
    policy: &code_system_graph_core::ExecutionPolicy,
) -> anyhow::Result<ActiveWatcher> {
    let callback_scope = scope.clone();
    let mut watcher = RecommendedWatcher::new(
        move |result| {
            forward_event(
                result,
                &callback_scope,
                &sender,
                &refresh_required,
                &scope_refresh_required,
            );
        },
        Config::default().with_follow_symlinks(false),
    )
    .context("failed to create native filesystem watcher")?;
    let mut guard = WatchSetupGuard::new(policy);
    #[cfg(target_os = "linux")]
    let mut installed = BTreeSet::new();
    #[cfg(target_os = "linux")]
    add_native_watch_entries(&mut watcher, scope, &mut installed, &mut guard)?;
    #[cfg(not(target_os = "linux"))]
    add_native_watch_entries(&mut watcher, scope, &mut guard)?;
    Ok(ActiveWatcher::Native(NativeWatcherState {
        watcher: Box::new(watcher),
        #[cfg(target_os = "linux")]
        installed,
    }))
}

fn build_poll_watcher(
    scope: &WatchScope,
    sender: mpsc::Sender<WatchSignal>,
    refresh_required: Arc<AtomicBool>,
    scope_refresh_required: Arc<AtomicBool>,
    interval: Duration,
    policy: &code_system_graph_core::ExecutionPolicy,
) -> anyhow::Result<ActiveWatcher> {
    let callback_scope = scope.clone();
    let config = Config::default()
        .with_poll_interval(interval)
        .with_compare_contents(true)
        .with_follow_symlinks(false);
    let mut watcher = PollWatcher::new(
        move |result| {
            forward_event(
                result,
                &callback_scope,
                &sender,
                &refresh_required,
                &scope_refresh_required,
            );
        },
        config,
    )
    .context("failed to create polling filesystem watcher")?;
    let mut guard = WatchSetupGuard::new(policy);
    add_watch_entries(&mut watcher, scope, &mut guard)
        .context("failed to install polling watch roots")?;
    Ok(ActiveWatcher::Polling(Box::new(watcher)))
}

fn add_watch_entries<W: Watcher>(
    watcher: &mut W,
    scope: &WatchScope,
    guard: &mut WatchSetupGuard,
) -> anyhow::Result<()> {
    for (path, mode) in scope.watch_entries() {
        guard.inspect()?;
        watcher.watch(&path, mode)?;
        guard.completed_directory()?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn add_native_watch_entries<W: Watcher>(
    watcher: &mut W,
    scope: &WatchScope,
    installed: &mut BTreeSet<PathBuf>,
    guard: &mut WatchSetupGuard,
) -> anyhow::Result<()> {
    if let Some(parent) = scope.config.parent() {
        guard.inspect()?;
        if installed.insert(parent.to_path_buf()) {
            watcher.watch(parent, RecursiveMode::NonRecursive)?;
        }
        guard.completed_directory()?;
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
        guard.inspect()?;
        if !visited.insert(directory.clone()) {
            continue;
        }
        if installed.insert(directory.clone()) {
            watcher.watch(&directory, RecursiveMode::NonRecursive)?;
        }
        let entries = std::fs::read_dir(&directory)?;
        for entry in entries {
            guard.inspect()?;
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir()
                && !file_type.is_symlink()
                && scope.should_watch_directory(&entry.path())?
            {
                pending.push(entry.path());
            }
        }
        guard.completed_directory()?;
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn add_native_watch_entries<W: Watcher>(
    watcher: &mut W,
    scope: &WatchScope,
    guard: &mut WatchSetupGuard,
) -> anyhow::Result<()> {
    add_watch_entries(watcher, scope, guard)
}

fn forward_event(
    result: notify::Result<Event>,
    scope: &WatchScope,
    sender: &mpsc::Sender<WatchSignal>,
    refresh_required: &AtomicBool,
    scope_refresh_required: &AtomicBool,
) {
    match result {
        Ok(event) => match scope.relevant_event(&event) {
            Ok(true) => {
                if scope.changes_enabled_gitignore(&event) {
                    scope_refresh_required.store(true, Ordering::Release);
                }
                let signal =
                    if event.kind.is_create() && event.paths.iter().any(|path| path.is_dir()) {
                        refresh_required.store(true, Ordering::Release);
                        WatchSignal::RefreshDirectories
                    } else {
                        WatchSignal::Dirty
                    };
                let _ignored = sender.try_send(signal);
            }
            Ok(false) => {}
            Err(error) => {
                eprintln!("csgraph sync could not apply repository ignore policy: {error}");
                let _ignored = sender.try_send(WatchSignal::Dirty);
            }
        },
        Err(error)
            if !error.paths.is_empty()
                && error
                    .paths
                    .iter()
                    .all(|path| database_artifact(path, &scope.database)) => {}
        Err(error) => {
            eprintln!("csgraph sync filesystem watcher reported: {error}");
            let _ignored = sender.try_send(WatchSignal::Dirty);
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

    #[test]
    fn watch_event_reader_should_coalesce_without_blocking_ready() {
        let (sender, mut receiver) = mpsc::channel(1);
        sender.try_send(()).expect("channel should accept prefill");
        let state = Arc::new(Mutex::new(WatchEventState {
            initial_ready: true,
            refresh_started: Some(Instant::now()),
            refresh_scope: false,
            failed: false,
        }));
        let input = concat!(
            "{\"type\":\"dirty\",\"schema_version\":1}\n",
            "{\"type\":\"ready\",\"schema_version\":1}\n"
        );
        spawn_watch_event_reader(
            std::io::Cursor::new(input.as_bytes().to_vec()),
            sender,
            Arc::clone(&state),
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let completed = state.lock().expect("watch event state").failed;
            if completed || Instant::now() >= deadline {
                break;
            }
            std::thread::yield_now();
        }

        let state = state.lock().expect("watch event state");
        assert!(state.failed, "reader should reach bounded EOF");
        assert!(state.refresh_started.is_none());
        assert!(receiver.try_recv().is_ok());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn repeated_dirty_messages_should_not_renew_no_progress_deadline() {
        let (sender, _receiver) = mpsc::channel(1);
        let original = Instant::now()
            .checked_sub(Duration::from_secs(10))
            .expect("ten seconds should be representable");
        let state = Arc::new(Mutex::new(WatchEventState {
            initial_ready: true,
            refresh_started: Some(original),
            refresh_scope: false,
            failed: false,
        }));
        let input = concat!(
            "{\"type\":\"dirty\",\"schema_version\":1}\n",
            "{\"type\":\"dirty\",\"schema_version\":1}\n"
        );
        spawn_watch_event_reader(
            std::io::Cursor::new(input.as_bytes().to_vec()),
            sender,
            Arc::clone(&state),
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        while !state.lock().expect("watch event state").failed && Instant::now() < deadline {
            std::thread::yield_now();
        }

        assert_eq!(
            state.lock().expect("watch event state").refresh_started,
            Some(original)
        );
    }

    #[test]
    fn directory_refresh_should_survive_a_full_dirty_channel() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let directory = temporary.path().to_path_buf();
        let scope = WatchScope {
            config: directory.clone(),
            database: directory.join("graph.db"),
            repositories: Vec::new(),
        };
        let (sender, mut receiver) = mpsc::channel(1);
        sender
            .try_send(WatchSignal::Dirty)
            .expect("channel should accept prefill");
        let refresh_required = AtomicBool::new(false);
        let event = Event::new(notify::EventKind::Create(notify::event::CreateKind::Folder))
            .add_path(directory);

        let scope_refresh_required = AtomicBool::new(false);
        forward_event(
            Ok(event),
            &scope,
            &sender,
            &refresh_required,
            &scope_refresh_required,
        );

        assert!(matches!(receiver.try_recv(), Ok(WatchSignal::Dirty)));
        assert!(refresh_required.swap(false, Ordering::AcqRel));
    }

    #[test]
    fn gitignore_change_should_request_scope_rebuild_even_when_channel_is_full() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().join("repo");
        std::fs::create_dir(&root).expect("repository root");
        let scope = WatchScope {
            config: temporary.path().join("code-system-graph.yaml"),
            database: temporary.path().join("graph.db"),
            repositories: vec![WatchRepository::new(
                root.clone(),
                gitignore_policy(),
                Vec::new(),
            )],
        };
        let (sender, mut receiver) = mpsc::channel(1);
        sender
            .try_send(WatchSignal::Dirty)
            .expect("channel should accept prefill");
        let refresh_required = AtomicBool::new(false);
        let scope_refresh_required = AtomicBool::new(false);
        let event = Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Any,
        )))
        .add_path(root.join(".gitignore"));

        forward_event(
            Ok(event),
            &scope,
            &sender,
            &refresh_required,
            &scope_refresh_required,
        );

        assert!(matches!(receiver.try_recv(), Ok(WatchSignal::Dirty)));
        assert!(scope_refresh_required.swap(false, Ordering::AcqRel));
    }

    #[test]
    fn watcher_error_detail_should_not_expose_worker_parser_messages() {
        let secret = "private-literal-7f2381";
        let error = anyhow::Error::new(ApplicationError::SupervisedApplication {
            exit_code: code_system_graph_core::ExitCode::InvalidInput,
            message: format!("parser rejected `{secret}`"),
        });

        let detail = watcher_error_detail(&error);
        assert!(!detail.contains(secret));
        assert_eq!(detail, "supervised sync failed with InvalidInput");
    }

    #[test]
    fn watch_setup_should_apply_memory_policy_before_traversal() {
        let policy = code_system_graph_core::ExecutionPolicy {
            max_worker_memory_bytes: 1,
            ..code_system_graph_core::ExecutionPolicy::default()
        };
        let mut guard = WatchSetupGuard::new(&policy);

        let error = guard
            .inspect()
            .expect_err("the current process uses more than one byte");
        assert!(matches!(
            error.downcast_ref::<ApplicationError>(),
            Some(ApplicationError::ExecutionLimit(limit))
                if limit.resource
                    == code_system_graph_core::ExecutionResource::WorkerMemoryBytes
                    && limit.maximum == 1
        ));
    }

    fn policy(excludes: &[&str], includes: &[&str]) -> IgnorePolicy {
        IgnorePolicy::new(
            excludes.iter().map(ToString::to_string).collect(),
            code_system_graph::ConfigSource::Default,
            includes.iter().map(ToString::to_string).collect(),
            code_system_graph::ConfigSource::Default,
        )
        .expect("built-in ignore policy")
    }

    fn gitignore_policy() -> IgnorePolicy {
        IgnorePolicy::with_gitignore(
            Vec::new(),
            code_system_graph::ConfigSource::WorkspaceManifest,
            Vec::new(),
            code_system_graph::ConfigSource::WorkspaceManifest,
            true,
            code_system_graph::ConfigSource::WorkspaceManifest,
        )
        .expect("Git ignore policy")
    }

    #[test]
    fn scope_should_apply_gitignore_and_observe_rule_changes() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let root = temporary.path().join("repo");
        std::fs::create_dir_all(root.join("nested"))?;
        std::fs::write(root.join(".gitignore"), "ignored.rs\n")?;
        std::fs::write(root.join("nested/.gitignore"), "*.rs\n!keep.rs\n")?;
        let scope = WatchScope {
            config: temporary.path().join("code-system-graph.yaml"),
            database: temporary.path().join("graph.db"),
            repositories: vec![WatchRepository::new(
                root.clone(),
                gitignore_policy(),
                Vec::new(),
            )],
        };

        assert!(!scope.relevant_path(&root.join("ignored.rs"))?);
        assert!(!scope.relevant_path(&root.join("nested/drop.rs"))?);
        assert!(scope.relevant_path(&root.join("nested/keep.rs"))?);
        assert!(scope.relevant_path(&root.join("nested/.gitignore"))?);

        std::fs::write(root.join(".gitignore"), "")?;
        std::fs::write(root.join("nested/.gitignore"), "")?;
        let refreshed = WatchScope {
            config: temporary.path().join("code-system-graph.yaml"),
            database: temporary.path().join("graph.db"),
            repositories: vec![WatchRepository::new(
                root.clone(),
                gitignore_policy(),
                Vec::new(),
            )],
        };
        assert!(refreshed.relevant_path(&root.join("ignored.rs"))?);
        assert!(refreshed.relevant_path(&root.join("nested/drop.rs"))?);
        Ok(())
    }

    #[test]
    fn scope_should_ignore_generated_state_and_database_sidecars() -> anyhow::Result<()> {
        let scope = WatchScope {
            config: PathBuf::from("/workspace/code-system-graph.yaml"),
            database: PathBuf::from("/workspace/.state/graph.db"),
            repositories: vec![WatchRepository::new(
                PathBuf::from("/workspace/repo"),
                policy(&[], &[]),
                vec![PathBuf::from(".code-system-graph.yaml")],
            )],
        };
        assert!(scope.relevant_path(Path::new("/workspace/repo/src/lib.rs"))?);
        assert!(scope.relevant_path(Path::new("/workspace/code-system-graph.yaml"))?);
        assert!(!scope.relevant_path(Path::new("/workspace/repo/.codegraph/codegraph.db-wal"))?);
        assert!(!scope.relevant_path(Path::new("/workspace/.state/graph.db-wal"))?);
        assert!(!scope.relevant_path(Path::new("/workspace/.state/graph.db.work-v1.db-wal"))?);
        assert!(!scope.relevant_path(Path::new("/workspace/.state/graph.db.work-v1.db-shm"))?);
        Ok(())
    }

    #[test]
    fn scope_should_apply_custom_excludes_and_default_includes() -> anyhow::Result<()> {
        let scope = WatchScope {
            config: PathBuf::from("/workspace/code-system-graph.yaml"),
            database: PathBuf::from("/workspace/.state/graph.db"),
            repositories: vec![WatchRepository::new(
                PathBuf::from("/workspace/repo"),
                policy(&["./generated//./**"], &["./vendor//internal-sdk/./**"]),
                vec![PathBuf::from("generated/explicit.yaml")],
            )],
        };

        assert_eq!(
            (
                scope.relevant_path(Path::new("/workspace/repo/generated/output.rs"))?,
                scope.relevant_path(Path::new("/workspace/repo/vendor/internal-sdk/src/lib.rs"))?,
                scope.relevant_path(Path::new("/workspace/repo/vendor/external/lib.rs"))?,
                scope.relevant_path(Path::new("/workspace/repo/generated/explicit.yaml"))?,
            ),
            (false, true, false, true)
        );
        Ok(())
    }

    #[test]
    fn scope_should_preserve_policies_for_aliases_with_the_same_checkout() -> anyhow::Result<()> {
        let temporary = tempfile::tempdir()?;
        let repository = temporary.path().join("repo/generated/output");
        std::fs::create_dir_all(&repository)?;
        let source = repository.join("lib.rs");
        std::fs::write(&source, "pub fn observed() {}\n")?;
        let root = temporary.path().join("repo");
        let scope = WatchScope {
            config: temporary.path().join("code-system-graph.yaml"),
            database: temporary.path().join("graph.db"),
            repositories: vec![
                WatchRepository::new(root.clone(), policy(&["generated/*"], &[]), Vec::new()),
                WatchRepository::new(root, policy(&[], &[]), Vec::new()),
            ],
        };

        assert_eq!(scope.repositories.len(), 2);
        assert!(scope.relevant_path(&source)?);
        assert!(scope.should_watch_directory(&repository)?);
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

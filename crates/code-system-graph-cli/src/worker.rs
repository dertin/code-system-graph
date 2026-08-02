//! Supervised process boundary for mutating workspace passes.

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use code_system_graph_core::{
    ExecutionLimitExceeded, ExecutionPolicy, ExecutionResource, ExecutionSummary, ExtractionLimitExceeded, GraphqlExtractionError, JobPhase, ScanJobTracker
};
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessesToUpdate, System};

use super::{
    ApplicationError, ScanOverrides, ScanSummary, application_exit_code, load_execution_policy
};
use crate::sync::{SyncSummary, sync_workspace_direct};

const PROTOCOL_VERSION: u8 = 1;
const MAX_REQUEST_BYTES: u64 = 1_048_576;
const MAX_PROTOCOL_LINE_BYTES: usize = 1_048_576;
const PROTOCOL_QUEUE_CAPACITY: usize = 256;
const MAX_PROTOCOL_MESSAGES_PER_POLL: usize = 1_024;
const POLL_INTERVAL: Duration = Duration::from_millis(100);

static WORKER_PROTOCOL_ACTIVE: AtomicBool = AtomicBool::new(false);
static WORKER_LIMIT_REPORTED: AtomicBool = AtomicBool::new(false);
static COMPLETED_UNITS: AtomicU64 = AtomicU64::new(0);
static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);
static WORKER_TRACKER: OnceLock<Mutex<ScanJobTracker>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkerEnvelope {
    schema_version: u8,
    run_id: String,
    request: WorkerRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum WorkerRequest {
    Scan {
        config: PathBuf,
        database: PathBuf,
        overrides: ScanOverrides,
    },
    Sync {
        config: PathBuf,
        database: PathBuf,
        overrides: ScanOverrides,
        synchronize_codegraph: bool,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WorkerMessage {
    Policy {
        schema_version: u8,
        policy: ExecutionPolicy,
    },
    Progress {
        schema_version: u8,
        phase: JobPhase,
        completed_units: u64,
    },
    ScanResult {
        schema_version: u8,
        summary: ScanSummary,
    },
    SyncResult {
        schema_version: u8,
        summary: SyncSummary,
    },
    Failure {
        schema_version: u8,
        failure: WorkerFailure,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WorkerFailure {
    ExtractionLimit {
        error: ExtractionLimitExceeded,
    },
    ExecutionLimit {
        error: ExecutionLimitExceeded,
    },
    PartialScanBudgetChanged,
    Transient {
        message: String,
    },
    Other {
        exit_code: code_system_graph_core::ExitCode,
        message: String,
    },
}

#[derive(Clone, Copy)]
enum ExpectedResult {
    Scan,
    Sync,
}

#[derive(Debug)]
enum SupervisedResult {
    Scan(ScanSummary),
    Sync(SyncSummary),
}

struct ProtocolReader {
    messages: Receiver<Result<WorkerMessage, ProtocolReadError>>,
}

struct SignalCancellation {
    requested: Arc<AtomicBool>,
    registrations: Vec<signal_hook::SigId>,
}

#[derive(Debug)]
enum ProtocolReadError {
    Io(String),
    LineTooLong(usize),
    Invalid(String),
}

/// Executes the hidden worker protocol on stdin/stdout.
///
/// This entry point is public only so the binary target can dispatch the hidden subcommand.
#[doc(hidden)]
pub fn run_worker_from_stdio() -> Result<(), String> {
    let mut request_bytes = Vec::new();
    std::io::stdin()
        .take(MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut request_bytes)
        .map_err(|error| format!("failed to read worker request: {error}"))?;
    if request_bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err("worker request exceeded protocol limit".to_owned());
    }
    let envelope: WorkerEnvelope = serde_json::from_slice(&request_bytes)
        .map_err(|error| format!("invalid worker request: {error}"))?;
    if envelope.schema_version != PROTOCOL_VERSION {
        return Err("unsupported worker request protocol version".to_owned());
    }

    WORKER_PROTOCOL_ACTIVE.store(true, Ordering::Release);
    WORKER_LIMIT_REPORTED.store(false, Ordering::Release);
    COMPLETED_UNITS.store(0, Ordering::Release);
    let config = match &envelope.request {
        WorkerRequest::Scan { config, .. } | WorkerRequest::Sync { config, .. } => config,
    };
    let policy = match load_execution_policy(config) {
        Ok(policy) => policy,
        Err(error) => return write_message(&failure_message(error)),
    };
    write_message(&WorkerMessage::Policy {
        schema_version: PROTOCOL_VERSION,
        policy: policy.clone(),
    })?;
    WORKER_TRACKER
        .set(Mutex::new(ScanJobTracker::new(envelope.run_id, policy)))
        .map_err(|_| "worker execution tracker was already initialized".to_owned())?;
    report_progress(JobPhase::Configuration, 1);
    let message = match envelope.request {
        WorkerRequest::Scan {
            config,
            database,
            overrides,
        } => match super::scan_workspace_direct(&config, &database, &overrides) {
            Ok(summary) => WorkerMessage::ScanResult {
                schema_version: PROTOCOL_VERSION,
                summary,
            },
            Err(error) => failure_message(error),
        },
        WorkerRequest::Sync {
            config,
            database,
            overrides,
            synchronize_codegraph,
        } => match sync_workspace_direct(&config, &database, &overrides, synchronize_codegraph) {
            Ok(summary) => WorkerMessage::SyncResult {
                schema_version: PROTOCOL_VERSION,
                summary,
            },
            Err(error) => failure_message(error),
        },
    };
    write_message(&message)
}

fn failure_message(error: ApplicationError) -> WorkerMessage {
    let retryable = retryable_application_error(&error);
    let failure = match error {
        ApplicationError::ExtractionLimit(error)
        | ApplicationError::HttpExtraction(
            code_system_graph_core::HttpExtractionError::LimitExceeded(error),
        )
        | ApplicationError::PackageManifest(
            code_system_graph_core::PackageManifestError::LimitExceeded(error),
        )
        | ApplicationError::GeneratedClient(
            code_system_graph_core::GeneratedClientError::LimitExceeded(error),
        )
        | ApplicationError::Graphql(GraphqlExtractionError::LimitExceeded(error))
        | ApplicationError::Protobuf(
            code_system_graph_core::ProtobufExtractionError::LimitExceeded(error),
        )
        | ApplicationError::SourceSyntax(
            code_system_graph_core::SourceSyntaxError::LimitExceeded(error),
        ) => WorkerFailure::ExtractionLimit { error },
        ApplicationError::ExecutionLimit(error) => WorkerFailure::ExecutionLimit { error },
        ApplicationError::PartialScanBudgetChanged => WorkerFailure::PartialScanBudgetChanged,
        error if retryable => WorkerFailure::Transient {
            message: error.to_string(),
        },
        error => {
            let exit_code = application_exit_code(&error);
            WorkerFailure::Other {
                exit_code,
                message: format!("supervised worker failed with {exit_code:?}"),
            }
        }
    };
    WorkerMessage::Failure {
        schema_version: PROTOCOL_VERSION,
        failure,
    }
}

fn retryable_application_error(error: &ApplicationError) -> bool {
    match error {
        ApplicationError::ReadFile { source, .. }
        | ApplicationError::Store(code_system_graph_store_sqlite::StoreError::Io {
            source, ..
        }) => retryable_io_kind(source.kind()),
        ApplicationError::Store(code_system_graph_store_sqlite::StoreError::LockHeld(_)) => true,
        ApplicationError::Store(code_system_graph_store_sqlite::StoreError::Sqlite(error)) => {
            matches!(
                error.sqlite_error_code(),
                Some(
                    rusqlite::ErrorCode::DatabaseBusy
                        | rusqlite::ErrorCode::DatabaseLocked
                        | rusqlite::ErrorCode::SystemIoFailure
                )
            )
        }
        _ => false,
    }
}

fn retryable_io_kind(kind: std::io::ErrorKind) -> bool {
    matches!(
        kind,
        std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
    )
}

fn write_message(message: &WorkerMessage) -> Result<(), String> {
    let mut encoded = serde_json::to_vec(message)
        .map_err(|error| format!("failed to encode worker protocol: {error}"))?;
    if encoded.len() > MAX_PROTOCOL_LINE_BYTES {
        return Err("worker response exceeded protocol limit".to_owned());
    }
    encoded.push(b'\n');
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&encoded)
        .and_then(|()| stdout.flush())
        .map_err(|error| format!("failed to write worker protocol: {error}"))
}

pub(crate) fn report_progress(phase: JobPhase, completed: u64) {
    if !WORKER_PROTOCOL_ACTIVE.load(Ordering::Acquire)
        || WORKER_LIMIT_REPORTED.load(Ordering::Acquire)
        || completed == 0
    {
        return;
    }
    if let Some(tracker) = WORKER_TRACKER.get() {
        let tracker_result = tracker.lock().map_or_else(
            |_| {
                Err(ExecutionLimitExceeded {
                    run_id: "worker-tracker".to_owned(),
                    phase,
                    resource: ExecutionResource::Cancellation,
                    observed: 1,
                    maximum: 0,
                    completed_units: COMPLETED_UNITS.load(Ordering::Acquire),
                })
            },
            |mut tracker| {
                tracker.enter_phase(phase)?;
                tracker.progress(completed)
            },
        );
        if let Err(error) = tracker_result {
            exit_for_execution_limit(error);
        }
    }
    let total = COMPLETED_UNITS
        .try_update(Ordering::AcqRel, Ordering::Acquire, |current| {
            current.checked_add(completed)
        })
        .map_or(u64::MAX, |previous| previous.saturating_add(completed));
    let _ = write_message(&WorkerMessage::Progress {
        schema_version: PROTOCOL_VERSION,
        phase,
        completed_units: total,
    });
}

pub(crate) fn check_time(phase: JobPhase) {
    if !WORKER_PROTOCOL_ACTIVE.load(Ordering::Acquire)
        || WORKER_LIMIT_REPORTED.load(Ordering::Acquire)
    {
        return;
    }
    if let Some(tracker) = WORKER_TRACKER.get() {
        let result = tracker.lock().map_or_else(
            |_| {
                Err(ExecutionLimitExceeded {
                    run_id: "worker-tracker".to_owned(),
                    phase,
                    resource: ExecutionResource::Cancellation,
                    observed: 1,
                    maximum: 0,
                    completed_units: COMPLETED_UNITS.load(Ordering::Acquire),
                })
            },
            |mut tracker| tracker.enter_phase(phase),
        );
        if let Err(error) = result {
            exit_for_execution_limit(error);
        }
    }
}

fn exit_for_execution_limit(error: ExecutionLimitExceeded) -> ! {
    WORKER_LIMIT_REPORTED.store(true, Ordering::Release);
    let _ = write_message(&WorkerMessage::Failure {
        schema_version: PROTOCOL_VERSION,
        failure: WorkerFailure::ExecutionLimit { error },
    });
    // This code runs only in the isolated hidden worker. Continuing after the
    // cooperative tracker expires could reach publication before the supervisor
    // observes the failure, so terminate immediately; SQLite rolls back an open
    // publication transaction and the parent reaps the whole process group.
    std::process::exit(70);
}

pub(crate) fn supervise_scan(
    config: &Path,
    database: &Path,
    overrides: &ScanOverrides,
) -> Result<ScanSummary, ApplicationError> {
    let request = WorkerRequest::Scan {
        config: config.to_path_buf(),
        database: database.to_path_buf(),
        overrides: overrides.clone(),
    };
    match supervise(config, &request, ExpectedResult::Scan, None, None)? {
        SupervisedResult::Scan(summary) => Ok(summary),
        SupervisedResult::Sync(_) => unreachable!("worker result kind was validated"),
    }
}

pub(crate) fn supervise_scan_with_executable(
    config: &Path,
    database: &Path,
    overrides: &ScanOverrides,
    executable: &Path,
) -> Result<ScanSummary, ApplicationError> {
    let request = WorkerRequest::Scan {
        config: config.to_path_buf(),
        database: database.to_path_buf(),
        overrides: overrides.clone(),
    };
    match supervise(
        config,
        &request,
        ExpectedResult::Scan,
        None,
        Some(executable),
    )? {
        SupervisedResult::Scan(summary) => Ok(summary),
        SupervisedResult::Sync(_) => unreachable!("worker result kind was validated"),
    }
}

pub(crate) fn supervise_sync(
    config: &Path,
    database: &Path,
    overrides: &ScanOverrides,
    synchronize_codegraph: bool,
) -> Result<SyncSummary, ApplicationError> {
    let request = WorkerRequest::Sync {
        config: config.to_path_buf(),
        database: database.to_path_buf(),
        overrides: overrides.clone(),
        synchronize_codegraph,
    };
    match supervise(config, &request, ExpectedResult::Sync, None, None)? {
        SupervisedResult::Sync(summary) => Ok(summary),
        SupervisedResult::Scan(_) => unreachable!("worker result kind was validated"),
    }
}

pub(crate) fn supervise_sync_with_executable(
    config: &Path,
    database: &Path,
    overrides: &ScanOverrides,
    synchronize_codegraph: bool,
    executable: &Path,
) -> Result<SyncSummary, ApplicationError> {
    let request = WorkerRequest::Sync {
        config: config.to_path_buf(),
        database: database.to_path_buf(),
        overrides: overrides.clone(),
        synchronize_codegraph,
    };
    match supervise(
        config,
        &request,
        ExpectedResult::Sync,
        None,
        Some(executable),
    )? {
        SupervisedResult::Sync(summary) => Ok(summary),
        SupervisedResult::Scan(_) => unreachable!("worker result kind was validated"),
    }
}

pub(crate) fn supervise_sync_with_wall_time_cap(
    config: &Path,
    database: &Path,
    overrides: &ScanOverrides,
    synchronize_codegraph: bool,
    wall_time_cap_ms: u64,
) -> Result<SyncSummary, ApplicationError> {
    let request = WorkerRequest::Sync {
        config: config.to_path_buf(),
        database: database.to_path_buf(),
        overrides: overrides.clone(),
        synchronize_codegraph,
    };
    match supervise(
        config,
        &request,
        ExpectedResult::Sync,
        Some(wall_time_cap_ms),
        None,
    )? {
        SupervisedResult::Sync(summary) => Ok(summary),
        SupervisedResult::Scan(_) => unreachable!("worker result kind was validated"),
    }
}

fn supervise(
    _config: &Path,
    request: &WorkerRequest,
    expected: ExpectedResult,
    wall_time_cap_ms: Option<u64>,
    explicit_executable: Option<&Path>,
) -> Result<SupervisedResult, ApplicationError> {
    let mut supervisory_policy = ExecutionPolicy::default();
    if let Some(cap) = wall_time_cap_ms {
        supervisory_policy.max_scan_wall_time_ms =
            supervisory_policy.max_scan_wall_time_ms.min(cap.max(1));
    }
    let run_id = next_run_id();
    let envelope = WorkerEnvelope {
        schema_version: PROTOCOL_VERSION,
        run_id: run_id.clone(),
        request: request.clone(),
    };
    let request_bytes = serde_json::to_vec(&envelope)
        .map_err(|error| ApplicationError::Initialization(error.to_string()))?;
    if request_bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err(limit_error(
            &run_id,
            JobPhase::Configuration,
            ExecutionResource::WorkerProtocolBytes,
            request_bytes.len() as u64,
            MAX_REQUEST_BYTES,
            0,
        ));
    }

    let executable =
        explicit_executable.map_or_else(worker_executable, validate_worker_executable)?;
    let mut command = Command::new(executable);
    command
        .arg("__worker-v1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    configure_supervised_process_group(&mut command);
    let mut child = command.spawn().map_err(|error| {
        ApplicationError::Initialization(format!("failed to start scan worker: {error}"))
    })?;
    let group = SupervisedProcessGroup::attach(&child).map_err(ApplicationError::Initialization)?;
    let mut stdin = child.stdin.take().ok_or_else(|| {
        ApplicationError::Initialization("scan worker stdin was unavailable".to_owned())
    })?;
    stdin.write_all(&request_bytes).map_err(|error| {
        ApplicationError::Initialization(format!("failed to send scan worker request: {error}"))
    })?;
    drop(stdin);
    let stdout = child.stdout.take().ok_or_else(|| {
        ApplicationError::Initialization("scan worker stdout was unavailable".to_owned())
    })?;
    let reader = ProtocolReader::spawn(stdout);
    let cancellation = SignalCancellation::register().map_err(|error| {
        terminate_supervised_process(
            &mut child,
            &group,
            supervisory_policy.graceful_termination_ms,
        );
        ApplicationError::Initialization(format!("failed to register worker cancellation: {error}"))
    })?;
    monitor_worker(
        &mut child,
        &group,
        &reader,
        &run_id,
        &mut supervisory_policy,
        wall_time_cap_ms,
        expected,
        &cancellation.requested,
    )
}

#[expect(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "Supervisor keeps protocol, deadlines, memory, termination, and final accounting together"
)]
fn monitor_worker(
    child: &mut Child,
    group: &SupervisedProcessGroup,
    reader: &ProtocolReader,
    run_id: &str,
    policy: &mut ExecutionPolicy,
    wall_time_cap_ms: Option<u64>,
    expected: ExpectedResult,
    cancellation: &AtomicBool,
) -> Result<SupervisedResult, ApplicationError> {
    let started = Instant::now();
    let mut last_progress = started;
    let mut completed_units = 0;
    let mut phase = JobPhase::Configuration;
    let mut peak_memory = 0;
    let mut final_result = None;
    let mut successful_exit_observed = None;
    let mut policy_received = false;
    let mut system = System::new();
    let root_pid = Pid::from_u32(child.id());

    loop {
        if cancellation.load(Ordering::Acquire) {
            terminate_supervised_process(child, group, policy.graceful_termination_ms);
            return Err(limit_error(
                run_id,
                phase,
                ExecutionResource::Cancellation,
                1,
                0,
                completed_units,
            ));
        }
        for _ in 0..MAX_PROTOCOL_MESSAGES_PER_POLL {
            let Ok(message) = reader.messages.try_recv() else {
                break;
            };
            match message {
                Ok(WorkerMessage::Policy {
                    schema_version,
                    policy: mut effective,
                }) if schema_version == PROTOCOL_VERSION && !policy_received => {
                    if let Some(cap) = wall_time_cap_ms {
                        effective.max_scan_wall_time_ms =
                            effective.max_scan_wall_time_ms.min(cap.max(1));
                    }
                    *policy = effective;
                    policy_received = true;
                }
                Ok(WorkerMessage::Progress {
                    schema_version,
                    phase: next_phase,
                    completed_units: next_completed,
                }) if schema_version == PROTOCOL_VERSION
                    && policy_received
                    && next_completed > completed_units =>
                {
                    completed_units = next_completed;
                    phase = next_phase;
                    last_progress = Instant::now();
                }
                Ok(WorkerMessage::Progress { .. }) => {}
                Ok(WorkerMessage::ScanResult {
                    schema_version,
                    summary,
                }) if schema_version == PROTOCOL_VERSION
                    && policy_received
                    && matches!(expected, ExpectedResult::Scan) =>
                {
                    final_result = Some(SupervisedResult::Scan(summary));
                }
                Ok(WorkerMessage::SyncResult {
                    schema_version,
                    summary,
                }) if schema_version == PROTOCOL_VERSION
                    && policy_received
                    && matches!(expected, ExpectedResult::Sync) =>
                {
                    final_result = Some(SupervisedResult::Sync(summary));
                }
                Ok(WorkerMessage::Failure {
                    schema_version,
                    failure,
                }) if schema_version == PROTOCOL_VERSION => {
                    let error = application_failure(failure);
                    for _ in 0..25 {
                        if child.try_wait().ok().flatten().is_some() {
                            group.force_termination();
                            return Err(error);
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    terminate_supervised_process(child, group, policy.graceful_termination_ms);
                    return Err(error);
                }
                Ok(_) => {
                    terminate_supervised_process(child, group, policy.graceful_termination_ms);
                    return Err(limit_error(
                        run_id,
                        phase,
                        ExecutionResource::WorkerProtocolBytes,
                        1,
                        0,
                        completed_units,
                    ));
                }
                Err(error) => {
                    terminate_supervised_process(child, group, policy.graceful_termination_ms);
                    let observed = match error {
                        ProtocolReadError::LineTooLong(bytes) => bytes as u64,
                        ProtocolReadError::Io(message) | ProtocolReadError::Invalid(message) => {
                            message.len() as u64
                        }
                    };
                    return Err(limit_error(
                        run_id,
                        phase,
                        ExecutionResource::WorkerProtocolBytes,
                        observed,
                        MAX_PROTOCOL_LINE_BYTES as u64,
                        completed_units,
                    ));
                }
            }
        }

        let elapsed = started.elapsed();
        let idle = last_progress.elapsed();
        let exceeded = if elapsed.as_millis() > u128::from(policy.max_scan_wall_time_ms) {
            Some((
                ExecutionResource::WallTimeMs,
                duration_millis(elapsed),
                policy.max_scan_wall_time_ms,
            ))
        } else if idle.as_millis() > u128::from(policy.max_no_progress_time_ms) {
            Some((
                ExecutionResource::NoProgressTimeMs,
                duration_millis(idle),
                policy.max_no_progress_time_ms,
            ))
        } else {
            None
        };
        if let Some((resource, observed, maximum)) = exceeded {
            terminate_supervised_process(child, group, policy.graceful_termination_ms);
            return Err(limit_error(
                run_id,
                phase,
                resource,
                observed,
                maximum,
                completed_units,
            ));
        }

        system.refresh_processes(ProcessesToUpdate::All, true);
        let memory = process_tree_memory(&system, root_pid);
        peak_memory = peak_memory.max(memory);
        if memory > policy.max_worker_memory_bytes {
            terminate_supervised_process(child, group, policy.graceful_termination_ms);
            return Err(limit_error(
                run_id,
                phase,
                ExecutionResource::WorkerMemoryBytes,
                memory,
                policy.max_worker_memory_bytes,
                completed_units,
            ));
        }

        let status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                terminate_supervised_process(child, group, policy.graceful_termination_ms);
                return Err(ApplicationError::Initialization(format!(
                    "failed to wait for scan worker: {error}"
                )));
            }
        };
        if let Some(status) = status {
            group.force_termination();
            if status.success() {
                if let Some(mut result) = final_result {
                    let execution = ExecutionSummary {
                        run_id: run_id.to_owned(),
                        duration_ms: duration_millis(started.elapsed()),
                        peak_worker_memory_bytes: peak_memory,
                        completed_work_units: completed_units,
                        checkpoint_hits: 0,
                        checkpoints_written: 0,
                        ..ExecutionSummary::default()
                    };
                    attach_execution(&mut result, execution);
                    return Ok(result);
                }
                let exited_at = successful_exit_observed.get_or_insert_with(Instant::now);
                if exited_at.elapsed() <= Duration::from_secs(1) {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
            }
            return Err(limit_error(
                run_id,
                phase,
                ExecutionResource::WorkerProcess,
                status
                    .code()
                    .map_or(u64::MAX, |code| code.unsigned_abs().into()),
                0,
                completed_units,
            ));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn attach_execution(result: &mut SupervisedResult, execution: ExecutionSummary) {
    match result {
        SupervisedResult::Scan(summary) => {
            let mut execution = execution;
            execution.checkpoint_hits = summary.execution.checkpoint_hits;
            execution.checkpoints_written = summary.execution.checkpoints_written;
            copy_artifact_timings(&summary.execution, &mut execution);
            summary.execution = execution;
        }
        SupervisedResult::Sync(summary) => {
            let mut execution = execution;
            execution.checkpoint_hits = summary.scan.execution.checkpoint_hits;
            execution.checkpoints_written = summary.scan.execution.checkpoints_written;
            copy_artifact_timings(&summary.scan.execution, &mut execution);
            summary.execution = execution.clone();
            summary.scan.execution = execution;
        }
    }
}

fn copy_artifact_timings(source: &ExecutionSummary, target: &mut ExecutionSummary) {
    target.measured_artifacts = source.measured_artifacts;
    target.artifact_duration_p50_ms = source.artifact_duration_p50_ms;
    target.artifact_duration_p95_ms = source.artifact_duration_p95_ms;
    target.artifact_duration_p99_ms = source.artifact_duration_p99_ms;
}

fn application_failure(failure: WorkerFailure) -> ApplicationError {
    match failure {
        WorkerFailure::ExtractionLimit { error } => ApplicationError::ExtractionLimit(error),
        WorkerFailure::ExecutionLimit { error } => ApplicationError::ExecutionLimit(error),
        WorkerFailure::PartialScanBudgetChanged => ApplicationError::PartialScanBudgetChanged,
        WorkerFailure::Transient { message } => ApplicationError::TransientExecution(message),
        WorkerFailure::Other { exit_code, message } => {
            ApplicationError::SupervisedApplication { exit_code, message }
        }
    }
}

fn limit_error(
    run_id: &str,
    phase: JobPhase,
    resource: ExecutionResource,
    observed: u64,
    maximum: u64,
    completed_units: u64,
) -> ApplicationError {
    ApplicationError::ExecutionLimit(ExecutionLimitExceeded {
        run_id: run_id.to_owned(),
        phase,
        resource,
        observed,
        maximum,
        completed_units,
    })
}

fn worker_executable() -> Result<PathBuf, ApplicationError> {
    let current = std::env::current_exe().map_err(|error| {
        ApplicationError::Initialization(format!("failed to resolve scan worker: {error}"))
    })?;
    worker_executable_for(&current)
}

fn worker_executable_for(current: &Path) -> Result<PathBuf, ApplicationError> {
    if current
        .file_stem()
        .is_some_and(|name| name == "csgraph" || name == "csgraph.exe")
    {
        return Ok(current.to_path_buf());
    }
    let worker_name = if cfg!(windows) {
        "csgraph.exe"
    } else {
        "csgraph"
    };
    if current.parent().and_then(Path::file_name) != Some(OsStr::new("out"))
        && let Some(candidate) = current
            .parent()
            .map(|directory| directory.join(worker_name))
        && candidate.is_file()
    {
        return Ok(candidate);
    }
    if current
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|name| name == "deps")
        && let Some(candidate) = current
            .parent()
            .and_then(Path::parent)
            .map(|directory| directory.join(worker_name))
        && candidate.is_file()
    {
        return Ok(candidate);
    }
    let cargo_build_layout = current
        .ancestors()
        .any(|path| path.file_name() == Some(OsStr::new("code-system-graph")))
        && current
            .ancestors()
            .any(|path| path.file_name() == Some(OsStr::new("build")));
    if cargo_build_layout
        && let Some(candidate) = current.ancestors().find_map(|directory| {
            matches!(
                directory.file_name().and_then(OsStr::to_str),
                Some("debug" | "release")
            )
            .then(|| directory.join(worker_name))
        })
        && candidate.is_file()
    {
        return Ok(candidate);
    }
    Err(ApplicationError::Initialization(format!(
        "could not locate a trusted csgraph worker for `{}`; embedding applications must use the explicit worker-executable library API",
        current.display()
    )))
}

fn validate_worker_executable(path: &Path) -> Result<PathBuf, ApplicationError> {
    if path.is_file() {
        return Ok(path.to_path_buf());
    }
    Err(ApplicationError::Initialization(format!(
        "configured scan worker `{}` is not a file",
        path.display()
    )))
}

fn next_run_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let counter = RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("run-{millis}-{}-{counter}", std::process::id())
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl ProtocolReader {
    fn spawn(stdout: impl Read + Send + 'static) -> Self {
        let (sender, messages) = mpsc::sync_channel(PROTOCOL_QUEUE_CAPACITY);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                match read_bounded_line(&mut reader, &mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        let message = serde_json::from_slice(&line)
                            .map_err(|error| ProtocolReadError::Invalid(error.to_string()));
                        if sender.send(message).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        });
        Self { messages }
    }
}

impl SignalCancellation {
    fn register() -> std::io::Result<Self> {
        let requested = Arc::new(AtomicBool::new(false));
        #[cfg(unix)]
        let signals = [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM];
        #[cfg(not(unix))]
        let signals = [signal_hook::consts::SIGINT];
        let registrations = signals
            .into_iter()
            .map(|signal| signal_hook::flag::register(signal, Arc::clone(&requested)))
            .collect::<std::io::Result<Vec<_>>>()?;
        Ok(Self {
            requested,
            registrations,
        })
    }
}

impl Drop for SignalCancellation {
    fn drop(&mut self) {
        for registration in self.registrations.drain(..) {
            signal_hook::low_level::unregister(registration);
        }
    }
}

fn read_bounded_line(
    reader: &mut impl BufRead,
    line: &mut Vec<u8>,
) -> Result<usize, ProtocolReadError> {
    loop {
        let buffer = reader
            .fill_buf()
            .map_err(|error| ProtocolReadError::Io(error.to_string()))?;
        if buffer.is_empty() {
            return Ok(line.len());
        }
        let consumed = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |position| position + 1);
        if line.len().saturating_add(consumed) > MAX_PROTOCOL_LINE_BYTES + 1 {
            return Err(ProtocolReadError::LineTooLong(
                line.len().saturating_add(consumed),
            ));
        }
        line.extend_from_slice(&buffer[..consumed]);
        reader.consume(consumed);
        if line.last() == Some(&b'\n') {
            line.pop();
            return Ok(line.len());
        }
    }
}

fn process_tree_memory(system: &System, root: Pid) -> u64 {
    system
        .processes()
        .iter()
        .filter(|(pid, _)| **pid == root || is_descendant(system, **pid, root))
        .map(|(_, process)| process.memory())
        .fold(0_u64, u64::saturating_add)
}

fn is_descendant(system: &System, mut candidate: Pid, root: Pid) -> bool {
    let mut remaining = system.processes().len();
    while remaining > 0 {
        let Some(parent) = system.process(candidate).and_then(sysinfo::Process::parent) else {
            return false;
        };
        if parent == root {
            return true;
        }
        if parent == candidate {
            return false;
        }
        candidate = parent;
        remaining -= 1;
    }
    false
}

#[doc(hidden)]
pub fn terminate_process_tree(root_process_id: u32) {
    let root = Pid::from_u32(root_process_id);
    let mut system = System::new();
    for _ in 0..3 {
        system.refresh_processes(ProcessesToUpdate::All, true);
        let descendants = system
            .processes()
            .iter()
            .filter(|(pid, _)| **pid != root && is_descendant(&system, **pid, root))
            .map(|(_, process)| process)
            .collect::<Vec<_>>();
        if descendants.is_empty() {
            break;
        }
        for process in descendants {
            let _ = process.kill();
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub(crate) fn process_identity(process_id: u32) -> Option<String> {
    let mut system = System::new();
    let pid = Pid::from_u32(process_id);
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system
        .process(pid)
        .map(|process| format!("{process_id}:{}", process.start_time()))
}

#[cfg(unix)]
#[doc(hidden)]
pub fn configure_supervised_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
#[doc(hidden)]
pub fn configure_supervised_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[doc(hidden)]
/// Isolated operating-system process group owned by one supervisor.
pub struct SupervisedProcessGroup {
    #[cfg(unix)]
    process_group_id: i32,
    #[cfg(windows)]
    job: windows_sys::Win32::Foundation::HANDLE,
}

impl SupervisedProcessGroup {
    /// Attaches a newly spawned child to an isolated process group or Windows Job Object.
    ///
    /// # Errors
    ///
    /// Returns a source-free diagnostic when the platform isolation primitive cannot be created.
    #[cfg(unix)]
    pub fn attach(child: &Child) -> Result<Self, String> {
        Ok(Self {
            process_group_id: i32::try_from(child.id())
                .map_err(|_| "worker PID was not representable".to_owned())?,
        })
    }

    /// Attaches a newly spawned child to an isolated process group or Windows Job Object.
    ///
    /// # Errors
    ///
    /// Returns a source-free diagnostic when the platform isolation primitive cannot be created.
    #[cfg(windows)]
    #[allow(
        unsafe_code,
        reason = "Windows Job Objects require FFI to bind the worker process tree"
    )]
    pub fn attach(child: &Child) -> Result<Self, String> {
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject
        };
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err("failed to create worker Job Object".to_owned());
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            ) == 0
                || AssignProcessToJobObject(job, child.as_raw_handle().cast()) == 0
            {
                windows_sys::Win32::Foundation::CloseHandle(job);
                return Err("failed to configure worker Job Object".to_owned());
            }
            Ok(Self { job })
        }
    }

    #[allow(
        unsafe_code,
        reason = "Windows Job Object termination is exposed through FFI"
    )]
    fn request_termination(&self) {
        #[cfg(unix)]
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.process_group_id),
            nix::sys::signal::Signal::SIGTERM,
        );
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.job, 1);
        }
    }

    #[allow(
        unsafe_code,
        reason = "Windows Job Object termination is exposed through FFI"
    )]
    fn force_termination(&self) {
        #[cfg(unix)]
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.process_group_id),
            nix::sys::signal::Signal::SIGKILL,
        );
        #[cfg(windows)]
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.job, 1);
        }
    }
}

#[cfg(unix)]
impl Drop for SupervisedProcessGroup {
    fn drop(&mut self) {
        self.force_termination();
    }
}

#[cfg(windows)]
impl Drop for SupervisedProcessGroup {
    #[allow(
        unsafe_code,
        reason = "Windows Job Object handles must be closed through FFI"
    )]
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.job);
        }
    }
}

#[doc(hidden)]
pub fn terminate_supervised_process(
    child: &mut Child,
    group: &SupervisedProcessGroup,
    grace_ms: u64,
) {
    terminate_process_tree(child.id());
    group.request_termination();
    let deadline = Instant::now() + Duration::from_millis(grace_ms);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    terminate_process_tree(child.id());
    group.force_termination();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_host_should_not_resolve_worker_from_external_search_paths() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let host = temporary.path().join("embedding-host");
        let error = worker_executable_for(&host).expect_err("untrusted lookup must fail closed");

        assert!(matches!(error, ApplicationError::Initialization(_)));
        assert!(error.to_string().contains("explicit worker-executable"));
    }

    #[test]
    fn bounded_reader_should_reject_oversized_line() {
        let bytes = vec![b'x'; MAX_PROTOCOL_LINE_BYTES + 2];
        let mut reader = BufReader::new(bytes.as_slice());
        let error = read_bounded_line(&mut reader, &mut Vec::new()).expect_err("must reject");
        assert!(matches!(error, ProtocolReadError::LineTooLong(_)));
    }

    #[test]
    fn arbitrary_duplicate_progress_should_not_advance_watchdog_units() {
        let message = WorkerMessage::Progress {
            schema_version: PROTOCOL_VERSION,
            phase: JobPhase::Discovery,
            completed_units: 1,
        };
        let encoded = serde_json::to_vec(&message).expect("encode");
        let decoded: WorkerMessage = serde_json::from_slice(&encoded).expect("decode");
        assert!(matches!(
            decoded,
            WorkerMessage::Progress {
                completed_units: 1,
                ..
            }
        ));
    }

    #[test]
    fn only_transient_storage_or_io_failures_should_be_retryable() {
        let lock = ApplicationError::Store(code_system_graph_store_sqlite::StoreError::LockHeld(
            PathBuf::from("graph.db.lock"),
        ));
        let deterministic = ApplicationError::Initialization("invalid manifest".to_owned());

        assert!(retryable_application_error(&lock));
        assert!(!retryable_application_error(&deterministic));
    }

    #[test]
    fn worker_failure_should_preserve_public_application_classification() {
        for original in [
            ApplicationError::UnknownOverrideRepository("missing".to_owned()),
            ApplicationError::WorkspaceNameMismatch {
                requested: "wrong".to_owned(),
                manifest: "expected".to_owned(),
            },
        ] {
            let expected = application_exit_code(&original);
            let WorkerMessage::Failure { failure, .. } = failure_message(original) else {
                panic!("failure message expected");
            };
            let restored = application_failure(failure);
            assert_eq!(application_exit_code(&restored), expected);
            assert!(!restored.to_string().is_empty());
        }
    }

    #[test]
    fn worker_failure_protocol_should_not_include_parser_input() {
        let secret = "private-parser-token-cf0391";
        let message = failure_message(ApplicationError::Graphql(
            GraphqlExtractionError::InvalidGraphql {
                source_path: "schema.graphql".to_owned(),
                message: format!("unexpected token `{secret}`"),
            },
        ));
        let encoded = serde_json::to_vec(&message).expect("worker failure protocol");

        assert!(
            !encoded
                .windows(secret.len())
                .any(|bytes| bytes == secret.as_bytes())
        );
        assert!(matches!(
            message,
            WorkerMessage::Failure {
                failure: WorkerFailure::Other {
                    exit_code: code_system_graph_core::ExitCode::InvalidInput,
                    ..
                },
                ..
            }
        ));
    }

    #[cfg(unix)]
    fn supervise_script(script: &str, policy: &ExecutionPolicy) -> ApplicationError {
        monitor_script(script, policy).expect_err("script must exceed a limit")
    }

    #[cfg(unix)]
    fn monitor_script(
        script: &str,
        policy: &ExecutionPolicy,
    ) -> Result<SupervisedResult, ApplicationError> {
        let mut policy = policy.clone();
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        configure_supervised_process_group(&mut command);
        let mut child = command.spawn().expect("test worker");
        let group = SupervisedProcessGroup::attach(&child).expect("test process group");
        let stdout = child.stdout.take().expect("test stdout");
        monitor_worker(
            &mut child,
            &group,
            &ProtocolReader::spawn(stdout),
            "test-run",
            &mut policy,
            None,
            ExpectedResult::Scan,
            &AtomicBool::new(false),
        )
    }

    #[cfg(unix)]
    #[test]
    fn supervisor_should_enforce_absolute_deadline() {
        let policy = ExecutionPolicy {
            max_scan_wall_time_ms: 75,
            max_no_progress_time_ms: 500,
            graceful_termination_ms: 10,
            ..ExecutionPolicy::default()
        };
        let error = supervise_script("sleep 30", &policy);
        assert!(matches!(
            error,
            ApplicationError::ExecutionLimit(ExecutionLimitExceeded {
                resource: ExecutionResource::WallTimeMs,
                ..
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn worker_reported_policy_should_cover_the_running_child() {
        let effective = ExecutionPolicy {
            max_scan_wall_time_ms: 75,
            max_no_progress_time_ms: 500,
            graceful_termination_ms: 10,
            ..ExecutionPolicy::default()
        };
        let message = serde_json::to_string(&WorkerMessage::Policy {
            schema_version: PROTOCOL_VERSION,
            policy: effective,
        })
        .expect("policy protocol");
        let outer = ExecutionPolicy {
            max_scan_wall_time_ms: 5_000,
            max_no_progress_time_ms: 5_000,
            graceful_termination_ms: 10,
            ..ExecutionPolicy::default()
        };
        let error = supervise_script(&format!("printf '%s\\n' '{message}'; sleep 30"), &outer);
        assert!(matches!(
            error,
            ApplicationError::ExecutionLimit(ExecutionLimitExceeded {
                resource: ExecutionResource::WallTimeMs,
                maximum: 75,
                ..
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn repeated_activity_without_completed_work_should_not_renew_watchdog() {
        let policy = ExecutionPolicy {
            max_scan_wall_time_ms: 1_000,
            max_no_progress_time_ms: 250,
            graceful_termination_ms: 10,
            ..ExecutionPolicy::default()
        };
        let policy_message = serde_json::to_string(&WorkerMessage::Policy {
            schema_version: PROTOCOL_VERSION,
            policy: policy.clone(),
        })
        .expect("policy protocol");
        let script = format!(
            "printf '%s\\n' '{policy_message}'; while true; do printf '%s\\n' '{{\"type\":\"progress\",\"schema_version\":1,\"phase\":\"discovery\",\"completed_units\":1}}'; sleep 0.02; done"
        );
        let error = supervise_script(&script, &policy);
        assert!(
            matches!(
                error,
                ApplicationError::ExecutionLimit(ExecutionLimitExceeded {
                    resource: ExecutionResource::NoProgressTimeMs,
                    completed_units: 1,
                    ..
                })
            ),
            "unexpected duplicate-progress result: {error:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn supervisor_should_enforce_process_tree_memory() {
        let policy = ExecutionPolicy {
            max_scan_wall_time_ms: 1_000,
            max_no_progress_time_ms: 1_000,
            max_worker_memory_bytes: 1,
            graceful_termination_ms: 10,
            ..ExecutionPolicy::default()
        };
        let error = supervise_script("sleep 30", &policy);
        assert!(matches!(
            error,
            ApplicationError::ExecutionLimit(ExecutionLimitExceeded {
                resource: ExecutionResource::WorkerMemoryBytes,
                ..
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn supervisor_should_terminate_descendants_that_ignore_parent_lifetime() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let pid_file = temporary.path().join("grandchild.pid");
        let script = format!(
            "sleep 30 & child=$!; printf '%s' \"$child\" > '{}'; wait",
            pid_file.display()
        );
        let policy = ExecutionPolicy {
            max_scan_wall_time_ms: 1_000,
            max_no_progress_time_ms: 75,
            graceful_termination_ms: 10,
            ..ExecutionPolicy::default()
        };
        let error = supervise_script(&script, &policy);
        assert!(matches!(
            error,
            ApplicationError::ExecutionLimit(ExecutionLimitExceeded {
                resource: ExecutionResource::NoProgressTimeMs,
                ..
            })
        ));
        let pid = std::fs::read_to_string(pid_file)
            .expect("grandchild PID")
            .parse::<u32>()
            .expect("numeric PID");
        std::thread::sleep(Duration::from_millis(50));
        assert!(process_identity(pid).is_none(), "grandchild survived limit");
    }

    #[cfg(unix)]
    #[test]
    fn supervisor_should_terminate_descendants_after_successful_worker_exit() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let pid_file = temporary.path().join("grandchild.pid");
        let policy = ExecutionPolicy::default();
        let policy_message = serde_json::to_string(&WorkerMessage::Policy {
            schema_version: PROTOCOL_VERSION,
            policy: policy.clone(),
        })
        .expect("policy message");
        let result_message = serde_json::to_string(&WorkerMessage::ScanResult {
            schema_version: PROTOCOL_VERSION,
            summary: ScanSummary {
                execution: ExecutionSummary::default(),
                workspace: "test".to_owned(),
                snapshot_id: "snapshot".to_owned(),
                node_count: 0,
                edge_count: 0,
                evidence_count: 0,
                community_count: 0,
                community_delta_count: 0,
                discovered_input_count: 0,
                changed_input_count: 0,
                reused_snapshot: false,
                corroborated_symbol_count: 0,
                affected_test_count: 0,
                degradation_count: 0,
                degradations: Vec::new(),
            },
        })
        .expect("result message");
        let script = format!(
            "sh -c 'trap \"\" TERM; sleep 30' & child=$!; printf '%s' \"$child\" > '{}'; printf '%s\\n' '{policy_message}'; printf '%s\\n' '{result_message}'",
            pid_file.display()
        );

        assert!(matches!(
            monitor_script(&script, &policy),
            Ok(SupervisedResult::Scan(_))
        ));
        let pid = std::fs::read_to_string(pid_file)
            .expect("grandchild PID")
            .parse::<u32>()
            .expect("numeric PID");
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            process_identity(pid).is_none(),
            "grandchild survived successful worker exit"
        );
    }
}

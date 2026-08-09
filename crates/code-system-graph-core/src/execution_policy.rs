use std::fmt::Write as _;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use code_system_graph_model::stable_id;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default maximum wall time for one supervised scan or sync pass.
pub const DEFAULT_MAX_SCAN_WALL_TIME_MS: u64 = 21_600_000;
/// Default maximum time without verified forward progress.
pub const DEFAULT_MAX_NO_PROGRESS_TIME_MS: u64 = 300_000;
/// Default maximum wall time for one repository-local `CodeGraph` synchronization.
pub const DEFAULT_MAX_CODEGRAPH_SYNC_WALL_TIME_MS_PER_REPO: u64 = 3_600_000;
/// Default maximum source-symbol anchors corroborated through `CodeGraph` per repository.
pub const DEFAULT_MAX_CODEGRAPH_CORROBORATION_ANCHORS_PER_REPO: i64 = 50;
/// Default maximum resident memory accepted for one worker process.
pub const DEFAULT_MAX_WORKER_MEMORY_BYTES: u64 = 17_179_869_184;
/// Default cooperative shutdown grace period before forced termination.
pub const DEFAULT_GRACEFUL_TERMINATION_MS: u64 = 5_000;
/// Default inactivity lease for a foreground watch session.
pub const DEFAULT_WATCH_IDLE_TIMEOUT_MS: u64 = 28_800_000;
/// Default absolute lifetime for one foreground watch session.
pub const DEFAULT_MAX_WATCH_SESSION_WALL_TIME_MS: u64 = 86_400_000;
/// Default minimum delay between watched sync pass starts.
pub const DEFAULT_MIN_WATCH_RESCAN_INTERVAL_MS: u64 = 10_000;
/// Default maximum retained historical checkpoint-cache bytes.
pub const DEFAULT_MAX_CHECKPOINT_CACHE_BYTES: u64 = 10_737_418_240;

/// Effective per-repository limit for source-symbol corroboration through `CodeGraph`.
///
/// The workspace manifest keeps `-1` as its portable unlimited sentinel, but resolved policy and
/// consumers use this type so that sentinel handling does not leak beyond the serde boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema)]
pub enum CodeGraphCorroborationAnchorLimit {
    /// Corroborate at most this many source-symbol anchors per repository.
    Bounded(NonZeroUsize),
    /// Do not apply a count limit.
    Unlimited,
}

impl CodeGraphCorroborationAnchorLimit {
    /// Returns the bounded limit, or `None` when corroboration is unlimited.
    #[must_use]
    pub const fn bounded(self) -> Option<NonZeroUsize> {
        match self {
            Self::Bounded(limit) => Some(limit),
            Self::Unlimited => None,
        }
    }
}

impl TryFrom<i64> for CodeGraphCorroborationAnchorLimit {
    type Error = InvalidExecutionPolicy;

    fn try_from(value: i64) -> Result<Self, Self::Error> {
        if value == -1 {
            return Ok(Self::Unlimited);
        }
        let limit = usize::try_from(value)
            .ok()
            .and_then(NonZeroUsize::new)
            .ok_or(InvalidExecutionPolicy::InvalidSignedValue {
                field: "maxCodeGraphCorroborationAnchorsPerRepo",
                value,
            })?;
        Ok(Self::Bounded(limit))
    }
}

impl Serialize for CodeGraphCorroborationAnchorLimit {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = match self {
            Self::Bounded(limit) => i64::try_from(limit.get()).map_err(|_| {
                serde::ser::Error::custom("CodeGraph corroboration anchor limit exceeds i64")
            })?,
            Self::Unlimited => -1,
        };
        serializer.serialize_i64(value)
    }
}

impl<'de> Deserialize<'de> for CodeGraphCorroborationAnchorLimit {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        i64::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for CodeGraphCorroborationAnchorLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bounded(limit) => limit.fmt(formatter),
            Self::Unlimited => formatter.write_str("-1"),
        }
    }
}

/// Optional operator-owned execution-policy overrides from the workspace manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionPolicyOverrides {
    /// Optional maximum wall time for one scan or sync pass.
    pub max_scan_wall_time_ms: Option<u64>,
    /// Optional maximum time without verified forward progress.
    pub max_no_progress_time_ms: Option<u64>,
    /// Optional per-repository `CodeGraph` synchronization wall time.
    #[serde(rename = "maxCodeGraphSyncWallTimeMsPerRepo")]
    pub max_codegraph_sync_wall_time_ms_per_repo: Option<u64>,
    /// Optional maximum source-symbol anchors corroborated through `CodeGraph` per repository.
    /// A value of `-1` disables this count limit.
    #[serde(rename = "maxCodeGraphCorroborationAnchorsPerRepo")]
    pub max_codegraph_corroboration_anchors_per_repo: Option<i64>,
    /// Optional maximum worker resident memory.
    pub max_worker_memory_bytes: Option<u64>,
    /// Optional cooperative shutdown grace period.
    pub graceful_termination_ms: Option<u64>,
    /// Optional watcher inactivity lease.
    pub watch_idle_timeout_ms: Option<u64>,
    /// Optional absolute watcher-session lifetime.
    pub max_watch_session_wall_time_ms: Option<u64>,
    /// Optional minimum delay between watched sync pass starts.
    pub min_watch_rescan_interval_ms: Option<u64>,
    /// Optional maximum retained checkpoint-cache bytes.
    pub max_checkpoint_cache_bytes: Option<u64>,
}

/// Effective global execution policy for one workspace operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionPolicy {
    /// Maximum wall time for one scan or sync pass.
    pub max_scan_wall_time_ms: u64,
    /// Maximum time without verified forward progress.
    pub max_no_progress_time_ms: u64,
    /// Maximum wall time for one repository-local `CodeGraph` synchronization.
    #[serde(rename = "maxCodeGraphSyncWallTimeMsPerRepo")]
    pub max_codegraph_sync_wall_time_ms_per_repo: u64,
    /// Maximum source-symbol anchors corroborated through `CodeGraph` per repository.
    /// Serialized as `-1` when this count limit is disabled.
    #[serde(rename = "maxCodeGraphCorroborationAnchorsPerRepo")]
    #[schemars(with = "i64")]
    pub max_codegraph_corroboration_anchors_per_repo: CodeGraphCorroborationAnchorLimit,
    /// Maximum resident memory accepted for one worker process.
    pub max_worker_memory_bytes: u64,
    /// Cooperative shutdown grace period before forced termination.
    pub graceful_termination_ms: u64,
    /// Inactivity lease for a foreground watch session.
    pub watch_idle_timeout_ms: u64,
    /// Absolute lifetime for one foreground watch session.
    pub max_watch_session_wall_time_ms: u64,
    /// Minimum delay between watched sync pass starts.
    pub min_watch_rescan_interval_ms: u64,
    /// Maximum retained historical checkpoint-cache bytes.
    pub max_checkpoint_cache_bytes: u64,
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self {
            max_scan_wall_time_ms: DEFAULT_MAX_SCAN_WALL_TIME_MS,
            max_no_progress_time_ms: DEFAULT_MAX_NO_PROGRESS_TIME_MS,
            max_codegraph_sync_wall_time_ms_per_repo:
                DEFAULT_MAX_CODEGRAPH_SYNC_WALL_TIME_MS_PER_REPO,
            max_codegraph_corroboration_anchors_per_repo:
                CodeGraphCorroborationAnchorLimit::try_from(
                    DEFAULT_MAX_CODEGRAPH_CORROBORATION_ANCHORS_PER_REPO,
                )
                .expect("the default CodeGraph corroboration anchor limit is valid"),
            max_worker_memory_bytes: DEFAULT_MAX_WORKER_MEMORY_BYTES,
            graceful_termination_ms: DEFAULT_GRACEFUL_TERMINATION_MS,
            watch_idle_timeout_ms: DEFAULT_WATCH_IDLE_TIMEOUT_MS,
            max_watch_session_wall_time_ms: DEFAULT_MAX_WATCH_SESSION_WALL_TIME_MS,
            min_watch_rescan_interval_ms: DEFAULT_MIN_WATCH_RESCAN_INTERVAL_MS,
            max_checkpoint_cache_bytes: DEFAULT_MAX_CHECKPOINT_CACHE_BYTES,
        }
    }
}

impl ExecutionPolicy {
    /// Resolves a partial global override and validates the effective relationships.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidExecutionPolicy`] for zero, unrepresentable, or inconsistent values.
    pub fn resolve(
        overrides: Option<&ExecutionPolicyOverrides>,
    ) -> Result<Self, InvalidExecutionPolicy> {
        let mut policy = Self::default();
        if let Some(values) = overrides {
            macro_rules! apply {
                ($field:ident) => {
                    if let Some(value) = values.$field {
                        policy.$field = value;
                    }
                };
            }
            apply!(max_scan_wall_time_ms);
            apply!(max_no_progress_time_ms);
            apply!(max_codegraph_sync_wall_time_ms_per_repo);
            if let Some(value) = values.max_codegraph_corroboration_anchors_per_repo {
                policy.max_codegraph_corroboration_anchors_per_repo = value.try_into()?;
            }
            apply!(max_worker_memory_bytes);
            apply!(graceful_termination_ms);
            apply!(watch_idle_timeout_ms);
            apply!(max_watch_session_wall_time_ms);
            apply!(min_watch_rescan_interval_ms);
            apply!(max_checkpoint_cache_bytes);
        }
        policy.validate()?;
        Ok(policy)
    }

    fn validate(&self) -> Result<(), InvalidExecutionPolicy> {
        for (field, value) in self.canonical_values() {
            let invalid_bytes = field.ends_with("Bytes") && usize::try_from(value).is_err();
            let invalid_sqlite_quota =
                field == "maxCheckpointCacheBytes" && i64::try_from(value).is_err();
            let invalid_deadline = field.ends_with("Ms")
                && Instant::now()
                    .checked_add(Duration::from_millis(value))
                    .is_none();
            if value == 0 || invalid_bytes || invalid_sqlite_quota || invalid_deadline {
                return Err(InvalidExecutionPolicy::InvalidValue { field, value });
            }
        }
        Self::require_not_greater(
            "maxNoProgressTimeMs",
            self.max_no_progress_time_ms,
            "maxScanWallTimeMs",
            self.max_scan_wall_time_ms,
        )?;
        Self::require_not_greater(
            "maxCodeGraphSyncWallTimeMsPerRepo",
            self.max_codegraph_sync_wall_time_ms_per_repo,
            "maxScanWallTimeMs",
            self.max_scan_wall_time_ms,
        )?;
        Self::require_not_greater(
            "gracefulTerminationMs",
            self.graceful_termination_ms,
            "maxNoProgressTimeMs",
            self.max_no_progress_time_ms,
        )?;
        Self::require_not_greater(
            "watchIdleTimeoutMs",
            self.watch_idle_timeout_ms,
            "maxWatchSessionWallTimeMs",
            self.max_watch_session_wall_time_ms,
        )?;
        Self::require_not_greater(
            "minWatchRescanIntervalMs",
            self.min_watch_rescan_interval_ms,
            "watchIdleTimeoutMs",
            self.watch_idle_timeout_ms,
        )
    }

    fn require_not_greater(
        field: &'static str,
        value: u64,
        maximum_field: &'static str,
        maximum: u64,
    ) -> Result<(), InvalidExecutionPolicy> {
        if value > maximum {
            return Err(InvalidExecutionPolicy::InvalidRelationship {
                field,
                value,
                maximum_field,
                maximum,
            });
        }
        Ok(())
    }

    fn canonical_values(&self) -> [(&'static str, u64); 9] {
        [
            ("maxScanWallTimeMs", self.max_scan_wall_time_ms),
            ("maxNoProgressTimeMs", self.max_no_progress_time_ms),
            (
                "maxCodeGraphSyncWallTimeMsPerRepo",
                self.max_codegraph_sync_wall_time_ms_per_repo,
            ),
            ("maxWorkerMemoryBytes", self.max_worker_memory_bytes),
            ("gracefulTerminationMs", self.graceful_termination_ms),
            ("watchIdleTimeoutMs", self.watch_idle_timeout_ms),
            (
                "maxWatchSessionWallTimeMs",
                self.max_watch_session_wall_time_ms,
            ),
            (
                "minWatchRescanIntervalMs",
                self.min_watch_rescan_interval_ms,
            ),
            ("maxCheckpointCacheBytes", self.max_checkpoint_cache_bytes),
        ]
    }

    /// Returns the stable canonical fingerprint of the effective operational policy.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut canonical = self
            .canonical_values()
            .into_iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(";");
        write!(
            canonical,
            ";maxCodeGraphCorroborationAnchorsPerRepo={}",
            self.max_codegraph_corroboration_anchors_per_repo
        )
        .expect("writing to a String cannot fail");
        stable_id("execution-policy", &canonical)
    }
}

/// Invalid operator-owned execution policy.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum InvalidExecutionPolicy {
    /// One effective value is zero or cannot be represented internally.
    #[error("execution policy `{field}` must be positive and representable; received {value}")]
    InvalidValue {
        /// Manifest field containing the invalid value.
        field: &'static str,
        /// Rejected numeric value.
        value: u64,
    },
    /// A signed policy value is neither a positive representable limit nor the supported sentinel.
    #[error(
        "execution policy `{field}` must be positive and representable, or -1 for unlimited; received {value}"
    )]
    InvalidSignedValue {
        /// Manifest field containing the invalid value.
        field: &'static str,
        /// Rejected signed value.
        value: i64,
    },
    /// One subordinate deadline exceeds its containing deadline.
    #[error("execution policy `{field}` ({value}) must not exceed `{maximum_field}` ({maximum})")]
    InvalidRelationship {
        /// Subordinate manifest field.
        field: &'static str,
        /// Supplied subordinate value.
        value: u64,
        /// Containing manifest field.
        maximum_field: &'static str,
        /// Supplied containing value.
        maximum: u64,
    },
}

/// Observable phase of one supervised scan or synchronization pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    /// Workspace configuration and checkout validation.
    Configuration,
    /// Filesystem artifact discovery.
    Discovery,
    /// Bounded content fingerprinting.
    Fingerprinting,
    /// Source-owned extraction and batch encoding.
    Extraction,
    /// Candidate graph assembly and linking.
    GraphAssembly,
    /// Deterministic community analysis.
    Communities,
    /// Atomic snapshot persistence.
    Publication,
    /// External repository-local `CodeGraph` synchronization.
    CodeGraphSync,
}

/// Resource exhausted by one supervised workspace operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionResource {
    /// Absolute wall time.
    WallTimeMs,
    /// Time without verified forward progress.
    NoProgressTimeMs,
    /// Resident worker memory.
    WorkerMemoryBytes,
    /// Deterministic work accounting overflow.
    WorkUnits,
    /// The worker terminated without a valid final protocol message.
    WorkerProcess,
    /// The worker protocol exceeded its byte limit or was malformed.
    WorkerProtocolBytes,
    /// The operator or hosting agent requested cancellation.
    Cancellation,
}

/// Observable resource accounting attached to a completed scan or sync pass.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionSummary {
    /// Unique identifier assigned by the supervisor to this pass.
    pub run_id: String,
    /// Monotonic wall-clock duration observed by the supervisor.
    pub duration_ms: u64,
    /// Largest combined resident set observed for the worker process tree.
    pub peak_worker_memory_bytes: u64,
    /// Number of completed units reported through the progress protocol.
    pub completed_work_units: u64,
    /// Number of deterministic checkpoint records reused by this pass.
    pub checkpoint_hits: u64,
    /// Number of complete deterministic checkpoint records written by this pass.
    pub checkpoints_written: u64,
    /// Number of artifact-extractor invocations measured by the worker.
    pub measured_artifacts: u64,
    /// Median artifact-extractor duration in milliseconds.
    pub artifact_duration_p50_ms: u64,
    /// 95th-percentile artifact-extractor duration in milliseconds.
    pub artifact_duration_p95_ms: u64,
    /// 99th-percentile artifact-extractor duration in milliseconds.
    pub artifact_duration_p99_ms: u64,
}

/// Typed failure produced when one supervised execution resource is exhausted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Error)]
#[error(
    "execution `{run_id}` exceeded {resource:?} during {phase:?}: observed {observed}, maximum {maximum}, completed {completed_units} units"
)]
pub struct ExecutionLimitExceeded {
    /// Stable identifier of the supervised operation.
    pub run_id: String,
    /// Phase active when the resource was exhausted.
    pub phase: JobPhase,
    /// Exhausted resource.
    pub resource: ExecutionResource,
    /// Observed resource value.
    pub observed: u64,
    /// Effective maximum.
    pub maximum: u64,
    /// Verified work units completed before rejection.
    pub completed_units: u64,
}

/// Injectable monotonic time source used by execution watchdogs.
pub trait MonotonicClock: std::fmt::Debug + Send + Sync {
    /// Duration since an arbitrary stable origin.
    fn now(&self) -> Duration;
}

#[derive(Debug)]
struct SystemMonotonicClock {
    origin: Instant,
}

impl SystemMonotonicClock {
    fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl MonotonicClock for SystemMonotonicClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// Monotonic progress tracker used inside one worker process.
#[derive(Debug)]
pub struct ScanJobTracker {
    run_id: String,
    policy: ExecutionPolicy,
    clock: Arc<dyn MonotonicClock>,
    started: Duration,
    last_progress: Duration,
    phase: JobPhase,
    completed_units: u64,
}

impl ScanJobTracker {
    /// Starts a tracker for one run at configuration validation.
    #[must_use]
    pub fn new(run_id: impl Into<String>, policy: ExecutionPolicy) -> Self {
        Self::with_clock(run_id, policy, Arc::new(SystemMonotonicClock::new()))
    }

    /// Starts a tracker with an injected monotonic clock for deterministic execution tests.
    #[must_use]
    pub fn with_clock(
        run_id: impl Into<String>,
        policy: ExecutionPolicy,
        clock: Arc<dyn MonotonicClock>,
    ) -> Self {
        let now = clock.now();
        Self {
            run_id: run_id.into(),
            policy,
            clock,
            started: now,
            last_progress: now,
            phase: JobPhase::Configuration,
            completed_units: 0,
        }
    }

    /// Changes phase after checking the active deadlines.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionLimitExceeded`] when wall time or no-progress time is exhausted.
    pub fn enter_phase(&mut self, phase: JobPhase) -> Result<(), ExecutionLimitExceeded> {
        self.check_time()?;
        self.phase = phase;
        Ok(())
    }

    /// Charges verified completed work using checked arithmetic.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionLimitExceeded`] on arithmetic overflow or an exhausted deadline.
    pub fn progress(&mut self, amount: u64) -> Result<(), ExecutionLimitExceeded> {
        let completed_units = self
            .completed_units
            .checked_add(amount)
            .ok_or_else(|| self.exceeded(ExecutionResource::WorkUnits, u64::MAX, u64::MAX - 1))?;
        self.check_time()?;
        self.completed_units = completed_units;
        self.last_progress = self.clock.now();
        Ok(())
    }

    /// Checks monotonic wall time and time since the last verified progress.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionLimitExceeded`] when either effective duration is exhausted.
    pub fn check_time(&self) -> Result<(), ExecutionLimitExceeded> {
        let now = self.clock.now();
        self.check_duration(
            ExecutionResource::WallTimeMs,
            now.saturating_sub(self.started),
            self.policy.max_scan_wall_time_ms,
        )?;
        self.check_duration(
            ExecutionResource::NoProgressTimeMs,
            now.saturating_sub(self.last_progress),
            self.policy.max_no_progress_time_ms,
        )
    }

    fn check_duration(
        &self,
        resource: ExecutionResource,
        observed: Duration,
        maximum: u64,
    ) -> Result<(), ExecutionLimitExceeded> {
        let observed = u64::try_from(observed.as_millis()).unwrap_or(u64::MAX);
        if observed > maximum {
            return Err(self.exceeded(resource, observed, maximum));
        }
        Ok(())
    }

    fn exceeded(
        &self,
        resource: ExecutionResource,
        observed: u64,
        maximum: u64,
    ) -> ExecutionLimitExceeded {
        ExecutionLimitExceeded {
            run_id: self.run_id.clone(),
            phase: self.phase,
            resource,
            observed,
            maximum,
            completed_units: self.completed_units,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    #[derive(Debug, Default)]
    struct FakeClock {
        milliseconds: AtomicU64,
    }

    impl FakeClock {
        fn advance(&self, milliseconds: u64) {
            self.milliseconds.fetch_add(milliseconds, Ordering::Relaxed);
        }
    }

    impl MonotonicClock for FakeClock {
        fn now(&self) -> Duration {
            Duration::from_millis(self.milliseconds.load(Ordering::Relaxed))
        }
    }

    #[test]
    fn defaults_should_be_generous_and_finite() {
        let policy = ExecutionPolicy::default();

        assert_eq!(policy.max_scan_wall_time_ms, 21_600_000);
        assert_eq!(
            policy
                .max_codegraph_corroboration_anchors_per_repo
                .bounded()
                .map(NonZeroUsize::get),
            Some(50)
        );
        assert_eq!(policy.max_worker_memory_bytes, 17_179_869_184);
        assert_eq!(policy.max_checkpoint_cache_bytes, 10_737_418_240);
    }

    #[test]
    fn partial_override_should_preserve_other_defaults() {
        let policy = ExecutionPolicy::resolve(Some(&ExecutionPolicyOverrides {
            max_scan_wall_time_ms: Some(28_800_000),
            ..ExecutionPolicyOverrides::default()
        }))
        .expect("valid override");

        assert_eq!(policy.max_scan_wall_time_ms, 28_800_000);
        assert_eq!(policy.max_no_progress_time_ms, 300_000);
    }

    #[test]
    fn zero_should_be_rejected() {
        let error = ExecutionPolicy::resolve(Some(&ExecutionPolicyOverrides {
            max_worker_memory_bytes: Some(0),
            ..ExecutionPolicyOverrides::default()
        }))
        .expect_err("zero must fail");

        assert!(matches!(
            error,
            InvalidExecutionPolicy::InvalidValue {
                field: "maxWorkerMemoryBytes",
                value: 0
            }
        ));
    }

    #[test]
    fn corroboration_anchor_limit_should_accept_positive_or_unlimited() {
        let bounded = ExecutionPolicy::resolve(Some(&ExecutionPolicyOverrides {
            max_codegraph_corroboration_anchors_per_repo: Some(12),
            ..ExecutionPolicyOverrides::default()
        }))
        .expect("positive anchor limit");
        let unlimited = ExecutionPolicy::resolve(Some(&ExecutionPolicyOverrides {
            max_codegraph_corroboration_anchors_per_repo: Some(-1),
            ..ExecutionPolicyOverrides::default()
        }))
        .expect("unlimited anchor limit");

        assert_eq!(
            bounded
                .max_codegraph_corroboration_anchors_per_repo
                .bounded()
                .map(NonZeroUsize::get),
            Some(12)
        );
        assert_eq!(
            unlimited
                .max_codegraph_corroboration_anchors_per_repo
                .bounded(),
            None
        );
        assert_ne!(bounded.fingerprint(), unlimited.fingerprint());
        assert_eq!(
            serde_json::to_value(bounded.max_codegraph_corroboration_anchors_per_repo)
                .expect("bounded limit serializes"),
            serde_json::json!(12)
        );
        assert_eq!(
            serde_json::to_value(unlimited.max_codegraph_corroboration_anchors_per_repo)
                .expect("unlimited limit serializes"),
            serde_json::json!(-1)
        );
    }

    #[test]
    fn corroboration_anchor_limit_should_reject_zero_and_values_below_sentinel() {
        for value in [0, -2] {
            let error = ExecutionPolicy::resolve(Some(&ExecutionPolicyOverrides {
                max_codegraph_corroboration_anchors_per_repo: Some(value),
                ..ExecutionPolicyOverrides::default()
            }))
            .expect_err("invalid anchor limit");

            assert!(matches!(
                error,
                InvalidExecutionPolicy::InvalidSignedValue {
                    field: "maxCodeGraphCorroborationAnchorsPerRepo",
                    value: observed
                } if observed == value
            ));
        }
    }

    #[test]
    fn technically_unrepresentable_values_should_be_rejected() {
        let quota = ExecutionPolicy::resolve(Some(&ExecutionPolicyOverrides {
            max_checkpoint_cache_bytes: Some(u64::MAX),
            ..ExecutionPolicyOverrides::default()
        }))
        .expect_err("SQLite quota overflow must fail");

        assert!(matches!(quota, InvalidExecutionPolicy::InvalidValue { .. }));
    }

    #[test]
    fn subordinate_deadline_should_not_exceed_scan_deadline() {
        let error = ExecutionPolicy::resolve(Some(&ExecutionPolicyOverrides {
            max_scan_wall_time_ms: Some(1_000),
            max_no_progress_time_ms: Some(1_001),
            max_codegraph_sync_wall_time_ms_per_repo: Some(1_000),
            graceful_termination_ms: Some(500),
            ..ExecutionPolicyOverrides::default()
        }))
        .expect_err("relationship must fail");

        assert!(matches!(
            error,
            InvalidExecutionPolicy::InvalidRelationship {
                field: "maxNoProgressTimeMs",
                ..
            }
        ));
    }

    #[test]
    fn fingerprint_should_ignore_yaml_field_order() {
        let first = ExecutionPolicy::default();
        let second = ExecutionPolicy::resolve(Some(&ExecutionPolicyOverrides::default()))
            .expect("defaults valid");

        assert_eq!(first.fingerprint(), second.fingerprint());
    }

    #[test]
    fn injected_clock_should_accept_exact_deadline_and_reject_one_unit_over() {
        let clock = Arc::new(FakeClock::default());
        let policy = ExecutionPolicy {
            max_scan_wall_time_ms: 10,
            max_no_progress_time_ms: 10,
            ..ExecutionPolicy::default()
        };
        let tracker = ScanJobTracker::with_clock("run", policy, clock.clone());

        clock.advance(10);
        tracker.check_time().expect("exact deadline is inclusive");
        clock.advance(1);
        let error = tracker.check_time().expect_err("one over must fail");

        assert_eq!(error.resource, ExecutionResource::WallTimeMs);
        assert_eq!(error.observed, 11);
        assert_eq!(error.maximum, 10);
    }

    #[test]
    fn phase_changes_should_not_fake_progress() {
        let clock = Arc::new(FakeClock::default());
        let policy = ExecutionPolicy {
            max_scan_wall_time_ms: 100,
            max_no_progress_time_ms: 5,
            ..ExecutionPolicy::default()
        };
        let mut tracker = ScanJobTracker::with_clock("run", policy, clock.clone());

        clock.advance(5);
        tracker
            .enter_phase(JobPhase::Discovery)
            .expect("exact idle deadline is inclusive");
        clock.advance(1);
        let error = tracker
            .enter_phase(JobPhase::Fingerprinting)
            .expect_err("phase churn must not renew watchdog");

        assert_eq!(error.resource, ExecutionResource::NoProgressTimeMs);
    }
}

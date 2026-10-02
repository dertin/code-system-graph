use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{ExecutionLimitExceeded, ExecutionPolicy, ExecutionResource, JobPhase};

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
    use std::sync::Arc;
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

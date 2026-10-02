//! Per-phase wall time and resident memory for one scan pass.

use std::time::Instant;

use code_system_graph_core::{JobPhase, PhaseTelemetry};
use sysinfo::{Pid, ProcessesToUpdate, System};

/// Records consecutive phases; each phase lasts from the previous mark to its own completion.
pub(crate) struct PhaseRecorder {
    last_mark: Instant,
    system: System,
    phases: Vec<PhaseTelemetry>,
}

impl PhaseRecorder {
    pub(crate) fn start() -> Self {
        Self {
            last_mark: Instant::now(),
            system: System::new(),
            phases: Vec::new(),
        }
    }

    pub(crate) fn complete(&mut self, phase: JobPhase) {
        let now = Instant::now();
        let duration_ms =
            u64::try_from(now.duration_since(self.last_mark).as_millis()).unwrap_or(u64::MAX);
        self.last_mark = now;
        let resident_memory_bytes = self.resident_memory_bytes();
        self.phases.push(PhaseTelemetry {
            phase,
            duration_ms,
            resident_memory_bytes,
        });
    }

    pub(crate) fn into_phases(self) -> Vec<PhaseTelemetry> {
        self.phases
    }

    fn resident_memory_bytes(&mut self) -> u64 {
        let pid = Pid::from_u32(std::process::id());
        self.system
            .refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
        self.system.process(pid).map_or(0, sysinfo::Process::memory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_recorder_should_keep_completion_order() {
        let mut recorder = PhaseRecorder::start();
        recorder.complete(JobPhase::Discovery);
        recorder.complete(JobPhase::Extraction);

        let phases = recorder.into_phases();

        assert_eq!(
            phases.iter().map(|phase| phase.phase).collect::<Vec<_>>(),
            vec![JobPhase::Discovery, JobPhase::Extraction]
        );
        assert!(phases.iter().any(|phase| phase.resident_memory_bytes > 0));
    }
}

use std::collections::HashMap;
use sysinfo::{Pid, ProcessStatus, Signal, System};
use tracing::{info, warn};

pub struct ProcessGovernor {
    sys: System,
    paused_processes: HashMap<Pid, u64>,
    pub is_emergency: bool,
    governed_miners: Vec<String>,
}

impl ProcessGovernor {
    pub fn new(miners: Vec<String>) -> Self {
        Self {
            sys: System::new_all(),
            paused_processes: HashMap::new(),
            is_emergency: false,
            governed_miners: miners.into_iter().map(|m| m.to_lowercase()).collect(),
        }
    }

    fn is_miner_process(&self, name: &str) -> bool {
        let lower_name = name.to_lowercase();
        self.governed_miners
            .iter()
            .any(|miner| lower_name.contains(miner))
    }

    /// On startup, unconditionally resume all known miners to clear any stale SIGSTOPs.
    /// Returns `true` when every known miner accepted SIGCONT (or none were present).
    /// Failed resumes are tracked in `paused_processes` so the main loop can retry via
    /// [`Self::resume_miners`].
    pub fn resume_all_known_miners(&mut self) -> bool {
        self.sys.refresh_processes();
        let mut all_ok = true;
        for process in self.sys.processes().values() {
            if self.is_miner_process(process.name()) {
                let pid = process.pid();
                let start_time = process.start_time();
                if process.kill_with(Signal::Continue).unwrap_or(false) {
                    info!(
                        pid = pid.as_u32(),
                        name = process.name(),
                        "Resumed miner process on startup"
                    );
                    self.paused_processes.remove(&pid);
                } else {
                    warn!(
                        pid = pid.as_u32(),
                        name = process.name(),
                        "Failed to resume miner process on startup"
                    );
                    self.paused_processes.insert(pid, start_time);
                    all_ok = false;
                }
            }
        }
        all_ok && self.paused_processes.is_empty()
    }

    /// Suspend known miner processes that are currently running.
    ///
    /// Already-stopped processes that we do **not** track are left alone so an operator's
    /// intentional `SIGSTOP` is not claimed and later resumed when the emergency clears.
    /// Tracked miners that were externally resumed (`!is_stopped`) are re-stopped.
    pub fn suspend_miners(&mut self) {
        self.sys.refresh_processes();
        for (&pid, process) in self.sys.processes() {
            if self.is_miner_process(process.name()) {
                let start_time = process.start_time();
                let is_tracked = self.paused_processes.get(&pid) == Some(&start_time);
                let is_stopped = matches!(process.status(), ProcessStatus::Stop);

                // Do not claim ownership of untracked processes that are already stopped.
                if is_stopped && !is_tracked {
                    continue;
                }

                if !is_stopped {
                    if process.kill_with(Signal::Stop).unwrap_or(false) {
                        info!(
                            pid = pid.as_u32(),
                            name = process.name(),
                            "Suspended miner process"
                        );
                        self.paused_processes.insert(pid, start_time);
                    } else {
                        warn!(
                            pid = pid.as_u32(),
                            name = process.name(),
                            "Failed to suspend miner process"
                        );
                    }
                }
            }
        }

        self.paused_processes.retain(|&pid, &mut start_time| {
            if let Some(process) = self.sys.process(pid) {
                process.start_time() == start_time
            } else {
                false
            }
        });
    }

    /// True when we still own at least one process that needs SIGCONT (or re-stop tracking).
    pub fn has_pending_resumes(&self) -> bool {
        !self.paused_processes.is_empty()
    }

    /// Resume only the miner processes that we successfully suspended.
    pub fn resume_miners(&mut self) -> bool {
        self.sys.refresh_processes();
        let mut all_success = true;

        self.paused_processes.retain(|&pid, &mut start_time| {
            if let Some(process) = self.sys.process(pid) {
                if process.start_time() != start_time {
                    return false; // Process died and PID reused
                }
                if process.kill_with(Signal::Continue).unwrap_or(false) {
                    info!(
                        pid = pid.as_u32(),
                        name = process.name(),
                        "Resumed miner process"
                    );
                    false // Remove from tracked
                } else {
                    warn!(
                        pid = pid.as_u32(),
                        name = process.name(),
                        "Failed to resume miner process"
                    );
                    all_success = false;
                    true // Keep tracking
                }
            } else {
                false // Process died
            }
        });

        all_success && self.paused_processes.is_empty()
    }
}

impl Drop for ProcessGovernor {
    fn drop(&mut self) {
        if !self.paused_processes.is_empty() {
            if self.is_emergency {
                warn!("Shutting down during active emergency: preserving SIGSTOP on miners.");
            } else {
                info!("Shutting down: resuming tracked miner processes...");
                self.resume_miners();
            }
        }
    }
}

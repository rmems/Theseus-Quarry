use std::collections::HashMap;
use sysinfo::{Pid, ProcessStatus, Signal, System};
use tracing::{info, warn};

const MINER_PROCESS_NAMES: &[&str] = &[
    "onezerominer",
    "bzminer",
    "SRBMiner-MULTI",
    "xmrig",
    "rigel",
    "qli-Client",
    "hellminer",
];

fn is_miner_process(name: &str) -> bool {
    let lower_name = name.to_lowercase();
    MINER_PROCESS_NAMES
        .iter()
        .any(|&miner| lower_name.contains(&miner.to_lowercase()))
}

pub struct ProcessGovernor {
    sys: System,
    paused_processes: HashMap<Pid, u64>,
}

impl ProcessGovernor {
    pub fn new() -> Self {
        Self {
            sys: System::new_all(),
            paused_processes: HashMap::new(),
        }
    }

    /// On startup, unconditionally resume all known miners to clear any stale SIGSTOPs
    pub fn resume_all_known_miners(&mut self) {
        self.sys.refresh_processes();
        for process in self.sys.processes().values() {
            if is_miner_process(process.name()) {
                if process.kill_with(Signal::Continue).unwrap_or(false) {
                    info!(
                        pid = process.pid().as_u32(),
                        name = process.name(),
                        "Resumed miner process on startup"
                    );
                } else {
                    warn!(
                        pid = process.pid().as_u32(),
                        name = process.name(),
                        "Failed to resume miner process on startup"
                    );
                }
            }
        }
    }

    /// Suspend any known miner processes that aren't already suspended by us.
    pub fn suspend_miners(&mut self) {
        self.sys.refresh_processes();
        for process in self.sys.processes().values() {
            if is_miner_process(process.name()) {
                let pid = process.pid();
                let start_time = process.start_time();

                let is_tracked = self.paused_processes.get(&pid) == Some(&start_time);
                let is_stopped = matches!(process.status(), ProcessStatus::Stop);

                if !is_tracked || !is_stopped {
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
            info!("Shutting down: resuming tracked miner processes...");
            self.resume_miners();
        }
    }
}

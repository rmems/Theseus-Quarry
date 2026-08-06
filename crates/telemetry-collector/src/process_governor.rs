use std::collections::HashSet;
use sysinfo::{Pid, Signal, System};
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

pub struct ProcessGovernor {
    sys: System,
    paused_pids: HashSet<Pid>,
}

impl ProcessGovernor {
    pub fn new() -> Self {
        Self {
            sys: System::new_all(),
            paused_pids: HashSet::new(),
        }
    }

    /// On startup, unconditionally resume all known miners to clear any stale SIGSTOPs
    pub fn resume_all_known_miners(&mut self) {
        self.sys.refresh_processes();
        for process in self.sys.processes().values() {
            if MINER_PROCESS_NAMES.contains(&process.name())
                && process.kill_with(Signal::Continue).unwrap_or(false)
            {
                info!(
                    pid = process.pid().as_u32(),
                    name = process.name(),
                    "Resumed miner process on startup"
                );
            }
        }
    }

    /// Suspend any known miner processes that aren't already suspended by us.
    pub fn suspend_miners(&mut self) {
        self.sys.refresh_processes();
        for process in self.sys.processes().values() {
            if MINER_PROCESS_NAMES.contains(&process.name()) {
                let pid = process.pid();
                if !self.paused_pids.contains(&pid) {
                    if process.kill_with(Signal::Stop).unwrap_or(false) {
                        info!(
                            pid = pid.as_u32(),
                            name = process.name(),
                            "Suspended miner process"
                        );
                        self.paused_pids.insert(pid);
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
    }

    /// Resume only the miner processes that we successfully suspended.
    pub fn resume_miners(&mut self) {
        self.sys.refresh_processes();
        for pid in self.paused_pids.drain() {
            if let Some(process) = self.sys.process(pid) {
                if process.kill_with(Signal::Continue).unwrap_or(false) {
                    info!(
                        pid = pid.as_u32(),
                        name = process.name(),
                        "Resumed miner process"
                    );
                } else {
                    warn!(
                        pid = pid.as_u32(),
                        name = process.name(),
                        "Failed to resume miner process"
                    );
                }
            }
        }
    }
}

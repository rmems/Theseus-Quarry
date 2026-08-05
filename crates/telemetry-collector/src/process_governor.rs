use sysinfo::{Signal, System};
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
}

impl ProcessGovernor {
    pub fn new() -> Self {
        Self {
            sys: System::new_all(),
        }
    }

    /// Suspend all known miner processes using SIGSTOP.
    pub fn suspend_miners(&mut self) {
        self.sys.refresh_processes();
        for process in self.sys.processes().values() {
            if MINER_PROCESS_NAMES.contains(&process.name()) {
                if process.kill_with(Signal::Stop).unwrap_or(false) {
                    info!(
                        pid = process.pid().as_u32(),
                        name = process.name(),
                        "Suspended miner process"
                    );
                } else {
                    warn!(
                        pid = process.pid().as_u32(),
                        name = process.name(),
                        "Failed to suspend miner process"
                    );
                }
            }
        }
    }

    /// Resume all known miner processes using SIGCONT.
    pub fn resume_miners(&mut self) {
        self.sys.refresh_processes();
        for process in self.sys.processes().values() {
            if MINER_PROCESS_NAMES.contains(&process.name()) {
                if process.kill_with(Signal::Continue).unwrap_or(false) {
                    info!(
                        pid = process.pid().as_u32(),
                        name = process.name(),
                        "Resumed miner process"
                    );
                } else {
                    warn!(
                        pid = process.pid().as_u32(),
                        name = process.name(),
                        "Failed to resume miner process"
                    );
                }
            }
        }
    }
}

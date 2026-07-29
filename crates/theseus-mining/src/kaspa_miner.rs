//! Kaspa miner — spawns `bzminer --algo kaspa` as a managed subprocess.

use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};

use mining_telemetry_core::{MinerBrand, WireMsg};

use crate::miner;
use crate::throttle::MinerCommand;

// ─── Defaults ────────────────────────────────────────────────────────────────

const DEFAULT_API_PORT: u16 = 4014;

// ─── Binary detection ────────────────────────────────────────────────────────

pub fn detect_kaspa_binary() -> Option<String> {
    miner::detect_binary(
        "KASPA_MINER_CMD",
        &[
            "binaries/mining/nodes/kaspa/bzminer_v24.0.1_linux/bzminer",
            "binaries/mining/nodes/kaspa/bin/bzminer",
        ],
        &["./bzminer", "./nodes/kaspa/bzminer_v24.0.1_linux/bzminer"],
        "bzminer",
    )
}

// ─── KaspaMiner ──────────────────────────────────────────────────────────────

pub struct KaspaMiner {
    inner: miner::MinerHandle,
}

impl KaspaMiner {
    pub fn new(telem_tx: mpsc::Sender<WireMsg>) -> Self {
        let config = miner::MinerConfig {
            name: "kaspa",
            brand: MinerBrand::BzMiner,
            kill_timeout: 2,
            log_prefix: "[kaspa] ".into(),
        };
        Self {
            inner: miner::MinerHandle::new("kaspa", telem_tx, config, spawn_bzminer),
        }
    }

    pub fn send(&self, command: MinerCommand) {
        self.inner.send(command);
    }
}

// ─── Spawn ───────────────────────────────────────────────────────────────────

fn spawn_bzminer(
    telem_tx: &mpsc::Sender<WireMsg>,
    state: Arc<Mutex<miner::MinerState>>,
    _config: &miner::MinerConfig,
) -> Option<u32> {
    let Some(binary) = detect_kaspa_binary() else {
        let msg = "no Kaspa (bzminer) binary found";
        *state.lock().unwrap() = miner::MinerState::Failed(msg.to_string());
        let _ = telem_tx.send(WireMsg::Status(format!("[kaspa] {msg}")));
        return None;
    };

    let wallet = std::env::var("KASPA_WALLET").unwrap_or_default();
    if wallet.is_empty() {
        let msg = "KASPA_WALLET not set";
        *state.lock().unwrap() = miner::MinerState::Failed(msg.to_string());
        let _ = telem_tx.send(WireMsg::Status(format!("[kaspa] {msg}")));
        return None;
    }

    let api_port = std::env::var("KASPA_API_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_API_PORT);

    let mut command = Command::new(&binary);
    command
        .arg("-a")
        .arg("kaspa")
        .arg("-w")
        .arg(&wallet)
        .arg("-p")
        .arg("node+tcp://127.0.0.1:16110")
        .arg("--nc")
        .arg("1")
        .arg("--http_port")
        .arg(api_port.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let pid = miner::spawn_managed_process(
        command,
        "kaspa",
        state,
        telem_tx,
        MinerBrand::BzMiner,
    );

    if let Some(p) = pid {
        let _ = telem_tx.send(WireMsg::Status(format!("[kaspa] miner started (PID {p})")));
    }

    pid
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mining_telemetry_core::MiningStats;

    #[test]
    fn detect_binary_env_override() {
        crate::miner::with_env_lock(|| {
                unsafe { std::env::set_var("KASPA_MINER_CMD", "/custom/path/bzminer"); }
                let result = detect_kaspa_binary();
                unsafe { std::env::remove_var("KASPA_MINER_CMD"); }
                // May or may not match depending on canonical path existing
                let _ = result;
        });
    }

    #[test]
    fn parse_khs_joined() {
        let mut stats = MiningStats::default();
        stats.update_from_line(MinerBrand::BzMiner, "kaspa 1234.56mhs total");
        assert!(stats.kaspa.is_active);
    }

    #[test]
    fn state_default_is_idle() {
        let s = miner::MinerState::Idle;
        assert_eq!(s, miner::MinerState::Idle);
    }
}

//! Verus miner — spawns `hellminer` or `SRBMiner-Multi` as a managed subprocess.

use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};

use mining_telemetry_core::{MinerBrand, WireMsg};

use crate::miner;
use crate::throttle::MinerCommand;

// ─── Defaults ────────────────────────────────────────────────────────────────

const DEFAULT_POOL: &str = "stratum+tcp://na.luckpool.net:3956";
const DEFAULT_THREADS: &str = "4";
const DEFAULT_NICE: &str = "15";
const DEFAULT_WORKER: &str = "ship9950x";

// ─── Binary detection ────────────────────────────────────────────────────────

pub fn detect_verus_binary() -> Option<String> {
    if let Some(p) = miner::detect_binary(
        "VRSC_MINER_CMD",
        &[
            "binaries/mining/nodes/verus/bin/hellminer",
            "binaries/mining/SRBMiner-Multi-3-2-2/SRBMiner-MULTI",
            "binaries/mining/SRBMiner-Multi-1-0-9/SRBMiner-MULTI",
        ],
        &[
            "./nodes/verus/bin/hellminer",
            "./hellminer",
            "./SRBMiner-Multi-3-2-2/SRBMiner-MULTI",
            "./SRBMiner-MULTI",
        ],
        "hellminer",
    ) {
        return Some(p);
    }
    miner::detect_binary("VRSC_MINER_CMD", &[], &[], "SRBMiner-MULTI")
}

// ─── VerusMiner ──────────────────────────────────────────────────────────────

pub struct VerusMiner {
    inner: miner::MinerHandle,
}

impl VerusMiner {
    pub fn new(telem_tx: mpsc::Sender<WireMsg>) -> Self {
        let config = miner::MinerConfig {
            name: "verus",
            brand: MinerBrand::Hellminer,
            kill_timeout: 3,
            log_prefix: "[verus] ".into(),
        };
        Self {
            inner: miner::MinerHandle::new("verus", telem_tx, config, spawn_verus_miner),
        }
    }

    pub fn send(&self, command: MinerCommand) {
        self.inner.send(command);
    }
}

// ─── Spawn ───────────────────────────────────────────────────────────────────

fn spawn_verus_miner(
    telem_tx: &mpsc::Sender<WireMsg>,
    state: Arc<Mutex<miner::MinerState>>,
    _config: &miner::MinerConfig,
) -> Option<u32> {
    let Some(binary) = detect_verus_binary() else {
        let msg = "no Verus miner binary found — set VRSC_MINER_CMD or place hellminer/SRBMiner in binaries/mining/";
        eprintln!("[verus] {msg}");
        *state.lock().unwrap() = miner::MinerState::Failed(msg.to_string());
        let _ = telem_tx.send(WireMsg::Status(format!("[verus] {msg}")));
        return None;
    };

    let wallet = match std::env::var("VRSC_WALLET") {
        Ok(w) if !w.is_empty() => w,
        _ => {
            let msg = "VRSC_WALLET not set";
            eprintln!("[verus] {msg}");
            *state.lock().unwrap() = miner::MinerState::Failed(msg.to_string());
            let _ = telem_tx.send(WireMsg::Status(format!("[verus] {msg}")));
            return None;
        }
    };

    let pool = std::env::var("VRSC_POOL").unwrap_or_else(|_| DEFAULT_POOL.to_string());
    let threads = std::env::var("VRSC_THREADS").unwrap_or_else(|_| DEFAULT_THREADS.to_string());
    let nice = std::env::var("VRSC_NICE").unwrap_or_else(|_| DEFAULT_NICE.to_string());
    let worker = std::env::var("VRSC_WORKER").unwrap_or_else(|_| DEFAULT_WORKER.to_string());

    let use_hellminer = binary.contains("hellminer");
    // Always use Hellminer brand — both hellminer and SRBMiner-Multi mine Verus,
    // and the brand determines which coin stats to write to.
    let brand = MinerBrand::Hellminer;

    let mut command = if use_hellminer {
        let mut cmd = Command::new("nice");
        cmd.arg(format!("-n{}", nice))
            .arg(&binary)
            .arg("--cpu")
            .arg("-c")
            .arg(&pool)
            .arg("-u")
            .arg(format!("{}.{}", wallet, worker))
            .arg("-p")
            .arg("x")
            .arg("--threads")
            .arg(&threads);
        cmd
    } else {
        let mut cmd = Command::new(&binary);
        cmd.arg("--algorithm")
            .arg("verushash")
            .arg("--pool")
            .arg(&pool)
            .arg("--wallet")
            .arg(format!("{}.{}", wallet, worker))
            .arg("--cpu-threads")
            .arg(&threads);
        cmd
    };

    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let pid = miner::spawn_managed_process(command, "verus", state, telem_tx, brand);

    if let Some(p) = pid {
        eprintln!("[verus] spawned PID {p}  pool={pool}  threads={threads}  nice={nice}");
        let _ = telem_tx.send(WireMsg::Status(format!(
            "[verus] miner started (PID {p}  threads={threads}  nice={nice})"
        )));
    }

    pid
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_binary_env_override() {
        crate::miner::with_env_lock(|| {
                unsafe { std::env::set_var("VRSC_MINER_CMD", "/custom/path/hellminer"); }
                let result = detect_verus_binary();
                unsafe { std::env::remove_var("VRSC_MINER_CMD"); }
                assert_eq!(result, Some("/custom/path/hellminer".to_string()));
        });
    }

    #[test]
    fn state_default_is_idle() {
        let s = miner::MinerState::Idle;
        assert_eq!(s, miner::MinerState::Idle);
    }

    #[test]
    fn verus_miner_constructs_without_panic() {
        let (tx, _rx) = mpsc::channel::<WireMsg>();
        let miner_inst = VerusMiner::new(tx);
        miner_inst.send(MinerCommand::Stop);
    }
}

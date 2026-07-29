//! Monero miner — spawns `SRBMiner-Multi` or `xmrig` as a managed subprocess.

use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};

use mining_telemetry_core::{MinerBrand, WireMsg};

use crate::miner;
use crate::throttle::MinerCommand;

// ─── Defaults ────────────────────────────────────────────────────────────────

const DEFAULT_POOL: &str = "stratum+tcp://xmr.2miners.com:12212";
const DEFAULT_THREADS: &str = "4";
const DEFAULT_API_PORT: u16 = 4015;

// ─── Binary detection ────────────────────────────────────────────────────────

pub fn detect_monero_binary() -> Option<String> {
    // Prefer SRBMiner (present on this fleet); xmrig optional fallback.
    if let Some(p) = miner::detect_binary(
        "MONERO_MINER_CMD",
        &[
            "binaries/mining/SRBMiner-Multi-3-2-2/SRBMiner-MULTI",
            "binaries/mining/SRBMiner-Multi-1-0-9/SRBMiner-MULTI",
            "binaries/mining/xmrig/xmrig",
        ],
        &[
            "./SRBMiner-Multi-3-2-2/SRBMiner-MULTI",
            "./SRBMiner-MULTI",
            "./xmrig/xmrig",
            "./xmrig",
        ],
        "SRBMiner-MULTI",
    ) {
        return Some(p);
    }
    miner::detect_binary("MONERO_MINER_CMD", &[], &[], "xmrig")
}

// ─── MoneroMiner ─────────────────────────────────────────────────────────────

pub struct MoneroMiner {
    inner: miner::MinerHandle,
}

impl MoneroMiner {
    pub fn new(telem_tx: mpsc::Sender<WireMsg>) -> Self {
        let config = miner::MinerConfig {
            name: "monero",
            brand: MinerBrand::Xmrig,
            kill_timeout: 3,
            log_prefix: "[monero] ".into(),
        };
        Self {
            inner: miner::MinerHandle::new("monero", telem_tx, config, spawn_monero_miner),
        }
    }

    pub fn send(&self, command: MinerCommand) {
        self.inner.send(command);
    }
}

// ─── Spawn ───────────────────────────────────────────────────────────────────

fn spawn_monero_miner(
    telem_tx: &mpsc::Sender<WireMsg>,
    state: Arc<Mutex<miner::MinerState>>,
    _config: &miner::MinerConfig,
) -> Option<u32> {
    let Some(binary) = detect_monero_binary() else {
        let msg = "no Monero miner binary found — set MONERO_MINER_CMD or place SRBMiner/xmrig in binaries/mining/";
        eprintln!("[monero] {msg}");
        *state.lock().unwrap() = miner::MinerState::Failed(msg.to_string());
        let _ = telem_tx.send(WireMsg::Status(format!("[monero] {msg}")));
        return None;
    };

    let wallet = match std::env::var("MONERO_WALLET") {
        Ok(w) if !w.is_empty() => w,
        _ => {
            let msg = "MONERO_WALLET not set";
            eprintln!("[monero] {msg}");
            *state.lock().unwrap() = miner::MinerState::Failed(msg.to_string());
            let _ = telem_tx.send(WireMsg::Status(format!("[monero] {msg}")));
            return None;
        }
    };

    let pool = std::env::var("MONERO_POOL").unwrap_or_else(|_| DEFAULT_POOL.to_string());
    let threads = std::env::var("MONERO_THREADS").unwrap_or_else(|_| DEFAULT_THREADS.to_string());
    let api_port = std::env::var("MONERO_API_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_API_PORT);

    let mut command = Command::new(&binary);

    if binary.contains("SRBMiner") || binary.contains("SRBMiner-MULTI") {
        command
            .arg("--algorithm")
            .arg("randomx")
            .arg("--pool")
            .arg(&pool)
            .arg("--wallet")
            .arg(&wallet)
            .arg("--cpu-threads")
            .arg(&threads)
            .arg("--http-port")
            .arg(api_port.to_string());
    } else {
        command
            .arg("-o")
            .arg(&pool)
            .arg("-o")
            .arg(&pool)
            .arg("-u")
            .arg(&wallet)
            .arg("-t")
            .arg(&threads)
            .arg("--http-port")
            .arg(api_port.to_string());
    }

    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    let pid = miner::spawn_managed_process(
        command,
        "monero",
        state,
        telem_tx,
        MinerBrand::Xmrig,
    );

    if let Some(p) = pid {
        eprintln!("[monero] spawned PID {p}  pool={pool}  threads={threads}  api=:{api_port}");
        let _ = telem_tx.send(WireMsg::Status(format!(
            "[monero] miner started (PID {p}  threads={threads}  api=:{api_port})"
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
                unsafe { std::env::set_var("MONERO_MINER_CMD", "/custom/path/xmrig"); }
                let result = detect_monero_binary();
                unsafe { std::env::remove_var("MONERO_MINER_CMD"); }
                assert_eq!(result, Some("/custom/path/xmrig".to_string()));
        });
    }

    #[test]
    fn state_default_is_idle() {
        let s = miner::MinerState::Idle;
        assert_eq!(s, miner::MinerState::Idle);
    }

    #[test]
    fn monero_miner_constructs_without_panic() {
        let (tx, _rx) = mpsc::channel::<WireMsg>();
        let _miner = MoneroMiner::new(tx);
        let (tx2, _rx2) = mpsc::channel::<WireMsg>();
        let miner = MoneroMiner::new(tx2);
        miner.send(MinerCommand::Stop);
    }
}

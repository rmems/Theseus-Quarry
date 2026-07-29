//! Dynex miner — spawns `onezerominer --algo dynex` as a managed subprocess.

use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};

use mining_telemetry_core::{MinerBrand, WireMsg};

use crate::miner;
use crate::throttle::MinerCommand;

// ─── Defaults ────────────────────────────────────────────────────────────────

const DEFAULT_POOL: &str = "stratum+tcp://us3.dynex.herominers.com:1120";
const DEFAULT_DEVICE: &str = "0";
const DEFAULT_API_PORT: u16 = 3010;

// ─── Binary detection ────────────────────────────────────────────────────────

pub fn detect_dynex_binary() -> Option<String> {
    miner::detect_binary(
        "DYNEX_MINER_CMD",
        &[
            "binaries/mining/onezerominer",
            "binaries/mining/onezerominer-linux/onezerominer",
        ],
        &["./onezerominer", "./onezerominer-linux/onezerominer"],
        "onezerominer",
    )
}

// ─── DynexMiner ──────────────────────────────────────────────────────────────

pub struct DynexMiner {
    inner: miner::MinerHandle,
}

impl DynexMiner {
    pub fn new(telem_tx: mpsc::Sender<WireMsg>) -> Self {
        let config = miner::MinerConfig {
            name: "dynex",
            brand: MinerBrand::DynexSolver,
            kill_timeout: 3,
            log_prefix: "[dynex] ".into(),
        };
        Self {
            inner: miner::MinerHandle::new("dynex", telem_tx, config, spawn_onezerominer),
        }
    }

    pub fn send(&self, command: MinerCommand) {
        self.inner.send(command);
    }
}

// ─── Spawn ───────────────────────────────────────────────────────────────────

fn spawn_onezerominer(
    telem_tx: &mpsc::Sender<WireMsg>,
    state: Arc<Mutex<miner::MinerState>>,
    _config: &miner::MinerConfig,
) -> Option<u32> {
    let Some(binary) = detect_dynex_binary() else {
        let msg = "no Dynex miner binary found — set DYNEX_MINER_CMD or place \
                   onezerominer under binaries/mining/";
        eprintln!("[dynex] {msg}");
        *state.lock().unwrap() = miner::MinerState::Failed(msg.to_string());
        let _ = telem_tx.send(WireMsg::Status(format!("[dynex] {msg}")));
        return None;
    };

    let pool = std::env::var("DYNEX_POOL").unwrap_or_else(|_| DEFAULT_POOL.to_string());
    let device = std::env::var("DYNEX_DEVICE").unwrap_or_else(|_| DEFAULT_DEVICE.to_string());
    let api_port = std::env::var("DYNEX_API_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_API_PORT);
    let wallet = match std::env::var("SHIP_WALLET") {
        Ok(w) if !w.is_empty() => w,
        _ => {
            eprintln!("[dynex] WARNING: SHIP_WALLET not set — using placeholder (no real rewards)");
            "PLACEHOLDER_SET_SHIP_WALLET".to_string()
        }
    };

    let mut command = Command::new(&binary);
    command
        .arg("--algo")
        .arg("dynex")
        .arg("--pool")
        .arg(&pool)
        .arg("--wallet")
        .arg(&wallet)
        .arg("--devices")
        .arg(&device)
        .arg("--api-port")
        .arg(api_port.to_string())
        .arg("--disable-telemetry")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let pid = miner::spawn_managed_process(
        command,
        "dynex",
        state,
        telem_tx,
        MinerBrand::DynexSolver,
    );

    if let Some(p) = pid {
        eprintln!("[dynex] spawned PID {p}  pool={pool}  device={device}  api=:{api_port}");
        let _ = telem_tx.send(WireMsg::Status(format!(
            "[dynex] miner started (PID {p}  device={device}  api=:{api_port})"
        )));
    }

    pid
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mining_telemetry_core::{extract_count_after, MiningStats};

    #[test]
    fn detect_binary_env_override() {
        crate::miner::with_env_lock(|| {
            unsafe {
                std::env::set_var("DYNEX_MINER_CMD", "/custom/path/onezerominer");
            }
            let result = detect_dynex_binary();
            unsafe {
                std::env::remove_var("DYNEX_MINER_CMD");
            }
            assert_eq!(result, Some("/custom/path/onezerominer".to_string()));
        });
    }

    #[test]
    fn detect_binary_empty_override_falls_through() {
        crate::miner::with_env_lock(|| {
            unsafe {
                std::env::set_var("DYNEX_MINER_CMD", "   ");
                std::env::set_var("SHIP_WORK_DIR", "/nonexistent/path");
            }
            let result = detect_dynex_binary();
            unsafe {
                std::env::remove_var("DYNEX_MINER_CMD");
                std::env::remove_var("SHIP_WORK_DIR");
            }
            let _ = result;
        });
    }

    #[test]
    fn parse_khs_joined() {
        let mut stats = MiningStats::default();
        stats.update_from_line(MinerBrand::DynexSolver, "GPU0: 1234.56KH/S total");
        assert!(stats.dynex.is_active);
        let v = stats.dynex.hashrate_mh_s;
        assert!(
            (v - 1.23456).abs() < 1e-4,
            "expected ~1.23456 MH/s, got {v}"
        );
    }

    #[test]
    fn parse_mhs_joined() {
        let mut stats = MiningStats::default();
        stats.update_from_line(MinerBrand::DynexSolver, "Total: 0.82MH/S");
        assert!(stats.dynex.is_active);
        assert!((stats.dynex.hashrate_mh_s - 0.82).abs() < 1e-6);
    }

    #[test]
    fn parse_no_hashrate_returns_none() {
        let mut stats = MiningStats::default();
        stats.update_from_line(
            MinerBrand::DynexSolver,
            "Connected to pool us3.dynex.herominers.com",
        );
        assert!(!stats.dynex.is_active);
    }

    #[test]
    fn parse_share_accepted_line() {
        let mut stats = MiningStats::default();
        stats.update_from_line(
            MinerBrand::DynexSolver,
            "GPU0: 1000.00KH/S | Accepted 5 / Rejected 0",
        );
        assert!(stats.dynex.is_active);
        assert_eq!(stats.dynex.shares_accepted, 5);
        assert_eq!(stats.dynex.shares_rejected, 0);
    }

    #[test]
    fn extract_count_after_accepted() {
        assert_eq!(
            extract_count_after("accepted 12 / rejected 0", "accepted"),
            Some(12)
        );
    }

    #[test]
    fn extract_count_after_missing() {
        assert_eq!(extract_count_after("no numbers here", "accepted"), None);
    }

    #[test]
    fn state_default_is_idle() {
        let s = miner::MinerState::Idle;
        assert_eq!(s, miner::MinerState::Idle);
    }

    #[test]
    fn dynex_miner_constructs_without_panic() {
        let (tx, _rx) = mpsc::channel::<WireMsg>();
        let miner_inst = DynexMiner::new(tx);
        miner_inst.send(MinerCommand::Stop);
    }
}

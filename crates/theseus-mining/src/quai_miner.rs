//! Quai Network miner wrapper.
//!
//! Spawns the external Quai stratum miner process in its own process group,
//! streams its stdout, and provides start/stop control.
//!
//! Quai is special: YieldForChat switches GPU→CPU mode instead of stopping.

use std::io::BufRead;
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use mining_telemetry_core::{MinerBrand, WireMsg};

use crate::miner;
use crate::throttle::MinerCommand;

// ─── Config ──────────────────────────────────────────────────────────────────

const DEFAULT_POOL: &str = "stratum.quai.network:3333";

fn resolve_wallet() -> Option<String> {
    std::env::var("QUAI_WALLET_ADDRESS")
        .ok()
        .filter(|w| !w.is_empty() && w != "0xYOUR_QUAI_WALLET" && w.starts_with("0x"))
}

fn detect_miner_cmd(mode: QuaiMiningMode) -> Option<Vec<String>> {
    if let Ok(cmd) = std::env::var("QUAI_MINER_CMD") {
        let trimmed = cmd.trim();
        if !trimmed.is_empty() {
            let parts: Vec<String> = trimmed
                .split_whitespace()
                .filter(|p| !p.is_empty())
                .map(|p| p.to_string())
                .collect();
            if !parts.is_empty() {
                return Some(parts);
            }
        }
    }

    if mode != QuaiMiningMode::Cpu {
        if let Some(rigel) = detect_rigel_binary() {
            return Some(vec![rigel]);
        }
    }

    let base = miner::ship_work_dir();
    let go_quai = format!("{base}/binaries/mining/nodes/Quai/go-quai/go-quai");
    if miner::is_usable_binary(&go_quai) {
        return Some(vec![go_quai]);
    }

    let home = std::env::var("HOME").unwrap_or_default();
    let legacy = format!("{home}/quai-tools/go-quai-stratum/go-quai-stratum");
    if miner::is_usable_binary(&legacy) {
        return Some(vec![legacy]);
    }

    miner::detect_binary("QUAI_MINER_CMD", &[], &[], "xmrig").map(|p| vec![p])
}

/// Locate Rigel under `binaries/mining/` (symlink or versioned dir), then PATH.
/// Full command override is `QUAI_MINER_CMD` (handled in `detect_miner_cmd`).
fn detect_rigel_binary() -> Option<String> {
    miner::detect_binary(
        "QUAI_RIGEL_BIN",
        &[
            "binaries/mining/rigel",
            "binaries/mining/rigel-1.23.1-linux/rigel",
        ],
        &["./rigel", "./rigel-1.23.1-linux/rigel"],
        "rigel",
    )
}

fn normalize_rigel_pool(pool: &str) -> String {
    if pool.contains("://") {
        pool.to_string()
    } else {
        format!("stratum+tcp://{pool}")
    }
}

fn resolve_worker_name() -> String {
    if let Ok(name) = std::env::var("QUAI_WORKER_NAME") {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Ok(host) = std::env::var("HOSTNAME") {
        let trimmed = host.trim();
        if !trimmed.is_empty() {
            return trimmed
                .split('.')
                .next()
                .unwrap_or("ship_of_theseus")
                .to_string();
        }
    }
    "ship_of_theseus".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuaiMiningMode {
    Cpu,
    Gpu,
}

impl QuaiMiningMode {
    pub fn from_env() -> Self {
        match std::env::var("QUAI_MINING_MODE").ok().as_deref() {
            Some("CPU") | Some("cpu") => QuaiMiningMode::Cpu,
            Some("GPU") | Some("gpu") => QuaiMiningMode::Gpu,
            _ => {
                if detect_rigel_binary().is_some() {
                    QuaiMiningMode::Gpu
                } else {
                    QuaiMiningMode::Cpu
                }
            }
        }
    }
}

// ─── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum QuaiMinerState {
    Idle,
    Running,
    Failed(String),
}

pub struct QuaiMiner {
    cmd_tx: mpsc::SyncSender<MinerCommand>,
}

impl Default for QuaiMiner {
    fn default() -> Self {
        Self::new(std::sync::mpsc::channel().0)
    }
}

impl QuaiMiner {
    pub fn new(telem_tx: mpsc::Sender<WireMsg>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::sync_channel::<MinerCommand>(32);
        thread::Builder::new()
            .name("quai-supervisor".into())
            .spawn(move || supervisor_loop(cmd_rx, telem_tx))
            .expect("quai supervisor thread");
        Self { cmd_tx }
    }

    pub fn send(&self, command: MinerCommand) {
        let _ = self.cmd_tx.try_send(command);
    }

    pub fn stop(&self) {
        self.send(MinerCommand::Stop);
    }
}

impl Drop for QuaiMiner {
    fn drop(&mut self) {
        self.stop();
    }
}

// ─── Supervisor loop (custom: GPU↔CPU mode switching) ────────────────────────

fn supervisor_loop(cmd_rx: mpsc::Receiver<MinerCommand>, telem_tx: mpsc::Sender<WireMsg>) {
    let state = Arc::new(Mutex::new(QuaiMinerState::Idle));
    let mut child_pid: Option<u32> = None;
    let current_mode = QuaiMiningMode::from_env();

    for cmd in cmd_rx {
        match cmd {
            MinerCommand::Start => {
                if matches!(*state.lock().unwrap(), QuaiMinerState::Running) {
                    continue;
                }
                child_pid = spawn_process(&telem_tx, Arc::clone(&state), current_mode);
            }
            MinerCommand::Stop => {
                miner::kill_process_group(child_pid.take(), 3);
                let _ = telem_tx.send(WireMsg::Status("[quai] miner stopped".into()));
                *state.lock().unwrap() = QuaiMinerState::Idle;
            }
            MinerCommand::YieldForChat(model) => {
                if current_mode == QuaiMiningMode::Gpu {
                    eprintln!("[quai] yielding GPU for chat ({model}) — switching to CPU mode");
                    miner::kill_process_group(child_pid.take(), 3);
                    child_pid = spawn_process(&telem_tx, Arc::clone(&state), QuaiMiningMode::Cpu);
                }
            }
            MinerCommand::ResumeAfterChat => {
                let target_mode = QuaiMiningMode::from_env();
                if target_mode == QuaiMiningMode::Gpu && child_pid.is_some() {
                    eprintln!("[quai] resuming GPU mode after chat");
                    miner::kill_process_group(child_pid.take(), 3);
                    child_pid = spawn_process(&telem_tx, Arc::clone(&state), QuaiMiningMode::Gpu);
                }
            }
            MinerCommand::ResetDebounce => {}
        }
    }
}

// ─── Spawn ───────────────────────────────────────────────────────────────────

fn spawn_process(
    telem_tx: &mpsc::Sender<WireMsg>,
    state: Arc<Mutex<QuaiMinerState>>,
    mode: QuaiMiningMode,
) -> Option<u32> {
    let wallet = resolve_wallet();
    let threads_default = if mode == QuaiMiningMode::Cpu { 16 } else { 4 };
    let threads = std::env::var("QUAI_MINING_THREADS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(threads_default);
    let pool = std::env::var("QUAI_POOL").unwrap_or_else(|_| DEFAULT_POOL.to_string());

    let Some(cmd_parts) = detect_miner_cmd(mode) else {
        let msg = if mode == QuaiMiningMode::Gpu {
            "no Rigel miner found for GPU mode. Try QUAI_MINING_MODE=CPU."
        } else {
            "no Quai miner binary found. \
             Set QUAI_MINER_CMD or install a miner to ~/quai-tools/go-quai-stratum/go-quai-stratum."
        };
        eprintln!("[quai-miner] {msg}");
        *state.lock().unwrap() = QuaiMinerState::Failed(msg.to_string());
        let _ = telem_tx.send(WireMsg::Status(format!("[quai-miner] {msg}")));
        return None;
    };

    let binary = &cmd_parts[0];
    let mut command = Command::new(binary);
    if cmd_parts.len() > 1 {
        command.args(&cmd_parts[1..]);
    }

    let binary_lc = binary.to_ascii_lowercase();
    if binary_lc.contains("rigel") {
        let Some(wallet_addr) = wallet.as_deref() else {
            return None;
        };
        let rigel_pool = normalize_rigel_pool(&pool);
        let worker = resolve_worker_name();
        let wallet_worker = format!("{}.{}", wallet_addr, worker);
        command
            .arg("-a")
            .arg("quai")
            .arg("-o")
            .arg(&rigel_pool)
            .arg("-u")
            .arg(&wallet_worker)
            .arg("--no-tui")
            .arg("--stats-interval")
            .arg("10")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    } else {
        let home = std::env::var("HOME").unwrap_or_default();
        let config_path = format!("{home}/quai-tools/go-quai-stratum/config/config.json");
        command
            .arg("-config")
            .arg(&config_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
    }

    miner::setsid_command(&mut command);

    match command.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            eprintln!(
                "[quai-miner] spawned PID {pid}  mode={:?}  threads={threads}",
                mode
            );
            *state.lock().unwrap() = QuaiMinerState::Running;
            let _ = telem_tx.send(WireMsg::Status(format!(
                "[quai-miner] started (PID {pid}  threads={threads})"
            )));

            if let Some(stdout) = child.stdout.take() {
                let tx = telem_tx.clone();
                let brand = if binary_lc.contains("rigel") {
                    MinerBrand::Rigel
                } else {
                    MinerBrand::Xmrig
                };
                thread::Builder::new()
                    .name("quai-stdout".into())
                    .spawn(move || miner::generic_stdout_reader(stdout, tx, brand, "quai"))
                    .ok();
            }
            if let Some(stderr) = child.stderr.take() {
                thread::Builder::new()
                    .name("quai-stderr".into())
                    .spawn(move || {
                        for line in std::io::BufReader::new(stderr).lines().flatten() {
                            eprintln!("[quai-stderr] {line}");
                        }
                    })
                    .ok();
            }

            let state_reap = Arc::clone(&state);
            thread::Builder::new()
                .name("quai-reap".into())
                .spawn(move || {
                    let _ = child.wait();
                    *state_reap.lock().unwrap() = QuaiMinerState::Idle;
                })
                .ok();
            Some(pid)
        }
        Err(e) => {
            eprintln!("[quai-miner] spawn failed: {e}");
            *state.lock().unwrap() = QuaiMinerState::Failed(e.to_string());
            None
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mining_telemetry_core::MiningStats;

    #[test]
    fn parse_khs_joined() {
        let mut stats = MiningStats::default();
        stats.update_from_line(MinerBrand::Rigel, "QUAI 1234.56KH/S total");
        assert!(stats.quai.is_active);
    }

    #[test]
    fn parse_no_hashrate_returns_none() {
        let mut stats = MiningStats::default();
        stats.update_from_line(MinerBrand::Rigel, "Connecting to pool...");
        assert!(!stats.quai.is_active);
    }

    #[test]
    fn state_default_is_idle() {
        let s = QuaiMinerState::Idle;
        assert_eq!(s, QuaiMinerState::Idle);
    }

    #[test]
    fn quai_miner_constructs_without_panic() {
        let (tx, _rx) = mpsc::channel::<WireMsg>();
        let miner_inst = QuaiMiner::new(tx);
        miner_inst.send(MinerCommand::Stop);
    }
}

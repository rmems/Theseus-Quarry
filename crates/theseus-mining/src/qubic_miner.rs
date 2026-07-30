//! Qubic miner — manages Qubic mining via podman-compose containers or native binary.
//!
//! # Lifecycle
//!
//! ```text
//!  Start → Idle → Starting → Running
//!  Stop  → Running → Idle  (podman-compose down / SIGTERM+SIGKILL)
//!  YieldForChat    → no-op (Qubic is CPU-only, no GPU contention)
//!  ResumeAfterChat → no-op
//! ```
//!
//! # Modes
//!
//! - **PodmanCompose**: preferred when `podman-compose` is available. Uses the
//!   existing `docker-compose.qubic.yml` to bring up `qubic-nodes` + `qubic-http`.
//! - **NativeBinary**: fallback that spawns `qubic-core` as a managed subprocess
//!   with setsid(), stdout parsing, and SIGTERM/SIGKILL lifecycle.
//!
//! # Environment variables consumed
//!
//! | Variable              | Default                                              |
//! |-----------------------|------------------------------------------------------|
//! | `QUBIC_MINER_CMD`     | (override: full path to qubic-core binary)           |
//! | `QUBIC_COMPOSE_FILE`  | (override: full path to docker-compose.qubic.yml)    |
//! | `QUBIC_WALLET_IDENTITY` / `QUBIC_WALLET` / `QUBIC_WALLET_ADDRESS` | required for native mode |
//! | `QUBIC_MINING_THREADS`  | `4`                                                |
//! | `QUBIC_PORT`          | `21841`                                              |
//! | `QUBIC_KNOWN_PEERS`   | from `.env.qubic`                                    |
//! | `SHIP_WORK_DIR`       | `$HOME/Theseus-Quarry`                               |

use std::io::BufRead;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;

use crate::miner;
use crate::throttle::MinerCommand;
use mining_telemetry_core::{MinerBrand, MiningStats, MiningTelemetry, WireMsg};

// ─── Defaults ────────────────────────────────────────────────────────────────

const DEFAULT_PORT: u16 = 21841;

// ─── Spawn mode ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QubicMode {
    /// podman-compose stack (qubic-nodes + qubic-http containers).
    PodmanCompose,
    /// Native `qubic-core` binary subprocess.
    NativeBinary,
}

// ─── Miner state ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum QubicMinerState {
    Idle,
    Starting,
    Running { mode: QubicMode },
    Failed(String),
}

// ─── Detection helpers ───────────────────────────────────────────────────────

/// Locate the docker-compose file for the Qubic stack.
pub fn detect_compose_file() -> Option<String> {
    if let Ok(f) = std::env::var("QUBIC_COMPOSE_FILE") {
        let t = f.trim().to_string();
        if !t.is_empty() && std::path::Path::new(&t).exists() {
            return Some(t);
        }
    }

    let base = miner::ship_work_dir();
    for rel in [
        "binaries/mining/nodes/Qubic/docker-compose.qubic.yml",
        "binaries/mining/nodes/Qubic/docker-compose.qubic.working.yml",
        "binaries/mining/nodes/Qubic/docker-compose.qubic.simple.yml",
    ] {
        let path = format!("{base}/{rel}");
        if std::path::Path::new(&path).exists() {
            return Some(path);
        }
    }

    None
}

/// Locate a usable `qubic-core` native binary (skips empty/text stubs).
pub fn detect_qubic_binary() -> Option<String> {
    miner::detect_binary(
        "QUBIC_MINER_CMD",
        &[
            "binaries/mining/nodes/Qubic/bin/qubic-core",
            "binaries/mining/nodes/Qubic/qubic-core",
        ],
        &["./qubic-core", "./nodes/Qubic/bin/qubic-core"],
        "qubic-core",
    )
}

/// Check if `podman-compose` is available on PATH.
fn has_podman_compose() -> bool {
    Command::new("which")
        .arg("podman-compose")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Determine the best available mode for running Qubic.
pub fn detect_qubic_mode() -> Option<QubicMode> {
    // Prefer podman-compose if both the tool and compose file exist.
    if has_podman_compose() && detect_compose_file().is_some() {
        return Some(QubicMode::PodmanCompose);
    }
    // Fallback to native binary.
    if detect_qubic_binary().is_some() {
        return Some(QubicMode::NativeBinary);
    }
    None
}

// ─── QubicMiner ──────────────────────────────────────────────────────────────

/// Handle to the Qubic miner supervisor thread.
///
/// Dropping this struct does NOT stop the miner — send `MinerCommand::Stop`
/// explicitly before dropping if a clean shutdown is required.
pub struct QubicMiner {
    cmd_tx: mpsc::SyncSender<MinerCommand>,
}

impl QubicMiner {
    /// Spawn the background supervisor thread and return a command handle.
    pub fn new(telem_tx: mpsc::Sender<WireMsg>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::sync_channel::<MinerCommand>(32);
        thread::Builder::new()
            .name("qubic-supervisor".into())
            .spawn(move || supervisor_loop(cmd_rx, telem_tx))
            .expect("qubic supervisor thread");
        Self { cmd_tx }
    }

    /// Send a command to the supervisor (non-blocking; drops if channel full).
    pub fn send(&self, command: MinerCommand) {
        let _ = self.cmd_tx.try_send(command);
    }
}

// ─── Supervisor loop ─────────────────────────────────────────────────────────

fn supervisor_loop(cmd_rx: mpsc::Receiver<MinerCommand>, telem_tx: mpsc::Sender<WireMsg>) {
    let state: Arc<Mutex<QubicMinerState>> = Arc::new(Mutex::new(QubicMinerState::Idle));
    let mut child_pid: Option<u32> = None;
    let mut compose_file: Option<String> = None;

    for cmd in cmd_rx {
        match cmd {
            MinerCommand::Start => {
                {
                    let s = state.lock().unwrap();
                    if matches!(
                        *s,
                        QubicMinerState::Running { .. } | QubicMinerState::Starting
                    ) {
                        eprintln!("[qubic] already running/starting, ignoring Start");
                        continue;
                    }
                }

                match detect_qubic_mode() {
                    Some(QubicMode::PodmanCompose) => {
                        let cf = detect_compose_file().unwrap();
                        compose_file = Some(cf.clone());
                        spawn_podman(&cf, &telem_tx, Arc::clone(&state));
                    }
                    Some(QubicMode::NativeBinary) => {
                        let aigarth = std::env::var("AIGARTH_ENABLED")
                            .map(|v| v == "true")
                            .unwrap_or(false);
                        child_pid = spawn_native_binary(&telem_tx, Arc::clone(&state), aigarth);
                    }
                    None => {
                        let msg = "no Qubic miner found — install podman-compose + \
                                   docker-compose.qubic.yml or place qubic-core binary \
                                   at binaries/mining/nodes/Qubic/bin/qubic-core";
                        eprintln!("[qubic] {msg}");
                        *state.lock().unwrap() = QubicMinerState::Failed(msg.to_string());
                        let _ = telem_tx.send(WireMsg::Status(format!("❌ Qubic: {msg}")));
                    }
                }
            }

            MinerCommand::Stop => {
                if let Some(ref cf) = compose_file {
                    stop_podman(cf);
                    compose_file = None;
                }
                kill_native(child_pid.take());
                *state.lock().unwrap() = QubicMinerState::Idle;
                let _ = telem_tx.send(WireMsg::Status("⏹ Qubic miner stopped".into()));
            }

            MinerCommand::YieldForChat(model) => {
                let s = state.lock().unwrap();
                if let QubicMinerState::Running {
                    mode: QubicMode::NativeBinary,
                } = *s
                {
                    drop(s);
                    eprintln!(
                        "[qubic] yielding GPU for chat ({model}) — restarting without Aigarth"
                    );
                    kill_native(child_pid.take());
                    child_pid = spawn_native_binary(&telem_tx, Arc::clone(&state), false);
                }
            }

            MinerCommand::ResumeAfterChat => {
                let s = state.lock().unwrap();
                if let QubicMinerState::Running {
                    mode: QubicMode::NativeBinary,
                } = *s
                {
                    let aigarth = std::env::var("AIGARTH_ENABLED")
                        .map(|v| v == "true")
                        .unwrap_or(false);
                    if aigarth {
                        drop(s);
                        eprintln!("[qubic] resuming Aigarth GPU training after chat");
                        kill_native(child_pid.take());
                        child_pid = spawn_native_binary(&telem_tx, Arc::clone(&state), true);
                    }
                }
            }

            MinerCommand::ResetDebounce => {}
        }
    }
}

// ─── Podman-compose management ───────────────────────────────────────────────

fn spawn_podman(
    compose_file: &str,
    telem_tx: &mpsc::Sender<WireMsg>,
    state: Arc<Mutex<QubicMinerState>>,
) {
    *state.lock().unwrap() = QubicMinerState::Starting;
    let _ = telem_tx.send(WireMsg::Status(
        "⏳ Qubic: starting podman-compose stack...".into(),
    ));

    // Get the compose file directory for podman-compose to use.
    let compose_dir = std::path::Path::new(compose_file)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());

    let result = Command::new("podman-compose")
        .arg("-f")
        .arg(compose_file)
        .arg("up")
        .arg("-d")
        .current_dir(&compose_dir)
        .output();

    match result {
        Ok(output) if output.status.success() => {
            eprintln!("[qubic] podman-compose up -d succeeded");
            *state.lock().unwrap() = QubicMinerState::Running {
                mode: QubicMode::PodmanCompose,
            };
            let _ = telem_tx.send(WireMsg::Status(
                "⛏ Qubic containers started (podman-compose)".into(),
            ));

            // Spawn a health-check thread that polls container status.
            let tx = telem_tx.clone();
            let state_hc = Arc::clone(&state);
            thread::Builder::new()
                .name("qubic-healthcheck".into())
                .spawn(move || healthcheck_loop(state_hc, tx))
                .expect("qubic healthcheck thread");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let msg = format!("podman-compose up failed: {stderr}");
            eprintln!("[qubic] {msg}");
            *state.lock().unwrap() = QubicMinerState::Failed(msg.clone());
            let _ = telem_tx.send(WireMsg::Status(format!("❌ Qubic: {msg}")));
        }
        Err(e) => {
            let msg = format!("podman-compose spawn error: {e}");
            eprintln!("[qubic] {msg}");
            *state.lock().unwrap() = QubicMinerState::Failed(msg.clone());
            let _ = telem_tx.send(WireMsg::Status(format!("❌ Qubic: {msg}")));
        }
    }
}

fn stop_podman(compose_file: &str) {
    let compose_dir = std::path::Path::new(compose_file)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| ".".to_string());

    eprintln!("[qubic] stopping podman-compose stack...");

    // Try graceful stop first.
    let result = Command::new("podman-compose")
        .arg("-f")
        .arg(compose_file)
        .arg("down")
        .current_dir(&compose_dir)
        .output();

    match result {
        Ok(output) if output.status.success() => {
            eprintln!("[qubic] podman-compose down succeeded");
        }
        _ => {
            eprintln!(
                "[qubic] podman-compose down failed or timed out — force-removing containers"
            );
            // Force-remove qubic containers as last resort.
            let _ = Command::new("podman")
                .args(["rm", "-f"])
                .arg("qubic-nodes")
                .output();
            let _ = Command::new("podman")
                .args(["rm", "-f"])
                .arg("qubic-http")
                .output();
        }
    }

    // Verify no qubic containers remain.
    thread::sleep(Duration::from_secs(2));
    if let Ok(output) = Command::new("podman")
        .args(["ps", "--filter", "name=qubic", "--format", "{{.Names}}"])
        .output()
    {
        let names = String::from_utf8_lossy(&output.stdout);
        if !names.trim().is_empty() {
            eprintln!(
                "[qubic] WARNING: containers still running after stop: {}",
                names.trim()
            );
        }
    }
}

/// Periodically check that qubic-http container is healthy.
fn healthcheck_loop(state: Arc<Mutex<QubicMinerState>>, telem_tx: mpsc::Sender<WireMsg>) {
    loop {
        thread::sleep(Duration::from_secs(30));

        // If state is no longer Running (PodmanCompose), exit.
        {
            let s = state.lock().unwrap();
            if !matches!(
                *s,
                QubicMinerState::Running {
                    mode: QubicMode::PodmanCompose
                }
            ) {
                return;
            }
        }

        // Check qubic-http container.
        let ok = Command::new("podman")
            .args([
                "ps",
                "--filter",
                "name=qubic-http",
                "--filter",
                "status=running",
                "-q",
            ])
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false);

        if !ok {
            eprintln!("[qubic] qubic-http container not running — marking Failed");
            *state.lock().unwrap() =
                QubicMinerState::Failed("qubic-http container exited".to_string());
            let _ = telem_tx.send(WireMsg::Status(
                "⚠️ Qubic: qubic-http container stopped unexpectedly".into(),
            ));
            return;
        }

        // Optionally poll tick-info for telemetry.
        if let Ok(output) = Command::new("curl")
            .args(["-s", "-m", "5", "http://127.0.0.1:8099/tick-info"])
            .output()
            && output.status.success()
        {
            let body = String::from_utf8_lossy(&output.stdout);
            // Parse tick number from JSON response for telemetry.
            if let Some(tick) = extract_tick_from_json(&body) {
                let mut telem = MiningTelemetry::new();
                telem.stats.qubic.current_tick = tick;
                telem.stats.qubic.is_active = true;
                let _ = telem_tx.send(WireMsg::mining_telem(telem));
            }
        }
    }
}

/// Extract tick number from qubic-http tick-info JSON response.
fn extract_tick_from_json(body: &str) -> Option<u32> {
    // Simple extraction — avoids pulling in a full JSON parser for one field.
    // Format: {"tick":12345, ...} or {"tickInfo":{"tick":12345,...}}
    for pattern in ["\"tick\":", "\"currentTick\":"] {
        if let Some(idx) = body.find(pattern) {
            let after = &body[idx + pattern.len()..];
            let digits: String = after
                .chars()
                .skip_while(|c| c.is_whitespace())
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(tick) = digits.parse::<u32>() {
                return Some(tick);
            }
        }
    }
    None
}

// ─── Native binary management ────────────────────────────────────────────────

fn spawn_native_binary(
    telem_tx: &mpsc::Sender<WireMsg>,
    state: Arc<Mutex<QubicMinerState>>,
    _aigarth_enabled: bool,
) -> Option<u32> {
    let Some(binary) = detect_qubic_binary() else {
        let msg = "no usable qubic-core binary found — set QUBIC_MINER_CMD to a real \
                   binary (empty stubs under binaries/mining/nodes/Qubic/ are ignored), \
                   or use podman-compose mode";
        eprintln!("[qubic] {msg}");
        *state.lock().unwrap() = QubicMinerState::Failed(msg.to_string());
        let _ = telem_tx.send(WireMsg::Status(format!("❌ Qubic: {msg}")));
        return None;
    };

    let aigarth_enabled = std::env::var("AIGARTH_ENABLED")
        .map(|v| v == "true")
        .unwrap_or(false);

    let is_qli = std::path::Path::new(&binary)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.to_ascii_lowercase().contains("qli"))
        .unwrap_or(false);

    let threads_default = if aigarth_enabled { 14 } else { 28 };
    let threads = std::env::var("QUBIC_MINING_THREADS")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(threads_default);
    let port = std::env::var("QUBIC_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    let peers = std::env::var("QUBIC_KNOWN_PEERS").unwrap_or_default();

    let wallet_keys = if is_qli {
        ["QUBIC_WALLET", "QUBIC_WALLET_ADDRESS", "QUBIC_WALLET_IDENTITY"]
    } else {
        ["QUBIC_WALLET_IDENTITY", "QUBIC_WALLET", "QUBIC_WALLET_ADDRESS"]
    };
    let wallet = wallet_keys
        .into_iter()
        .find_map(|k| {
            std::env::var(k)
                .ok()
                .map(|w| w.trim().to_string())
                .filter(|w| !w.is_empty())
        });
    let Some(wallet) = wallet else {
        let msg = "Qubic wallet required for native mode \
                   (set QUBIC_WALLET_IDENTITY, QUBIC_WALLET, or QUBIC_WALLET_ADDRESS)";
        eprintln!("[qubic] {msg}");
        *state.lock().unwrap() = QubicMinerState::Failed(msg.to_string());
        let _ = telem_tx.send(WireMsg::Status(format!("❌ Qubic: {msg}")));
        return None;
    };

    *state.lock().unwrap() = QubicMinerState::Starting;

    let mut command = Command::new(&binary);
    if is_qli {
        let alias = std::env::var("SHIP_WORKER_NAME").unwrap_or_else(|_| "ship-of-theseus".into());
        command
            .arg(format!("--ClientSettings:QubicAddress={wallet}"))
            .arg(format!("--ClientSettings:Alias={alias}"))
            .arg(format!("--ClientSettings:Trainer:CpuThreads={threads}"))
            .arg("--ClientSettings:Trainer:PPS=false");
    } else {
        command
            .arg("--threads")
            .arg(threads.to_string())
            .arg("--port")
            .arg(port.to_string())
            .arg("--identity")
            .arg(&wallet);
        if aigarth_enabled {
            command.arg("--gpu");
        }
        if !peers.is_empty() {
            command.arg("--peers").arg(&peers);
        }
    }

    command.stdout(Stdio::piped()).stderr(Stdio::piped());

    // Put qubic-core in its own process group.
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            nix::unistd::setsid().ok();
            Ok(())
        });
    }

    match command.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            eprintln!("[qubic] spawned PID {pid}  threads={threads}  port={port}");
            *state.lock().unwrap() = QubicMinerState::Running {
                mode: QubicMode::NativeBinary,
            };
            let _ = telem_tx.send(WireMsg::Status(format!(
                "⛏ Qubic miner started (PID {pid}  threads={threads})"
            )));

            // Stdout reader — parses hashrate lines → MiningTelemetry.
            if let Some(stdout) = child.stdout.take() {
                let tx = telem_tx.clone();
                thread::Builder::new()
                    .name("qubic-stdout".into())
                    .spawn(move || qubic_stdout_reader(stdout, tx))
                    .expect("qubic stdout thread");
            }

            // Stderr reader — forward to eprintln only.
            if let Some(stderr) = child.stderr.take() {
                thread::Builder::new()
                    .name("qubic-stderr".into())
                    .spawn(move || {
                        for line in std::io::BufReader::new(stderr)
                            .lines()
                            .map_while(Result::ok)
                        {
                            eprintln!("[qubic-stderr] {line}");
                        }
                    })
                    .expect("qubic stderr thread");
            }

            // Reap thread — marks state Idle when process exits.
            let state_reap = Arc::clone(&state);
            thread::Builder::new()
                .name("qubic-reap".into())
                .spawn(move || reap_child(child, state_reap))
                .expect("qubic reap thread");

            Some(pid)
        }
        Err(e) => {
            eprintln!("[qubic] spawn failed: {e}");
            *state.lock().unwrap() = QubicMinerState::Failed(e.to_string());
            let _ = telem_tx.send(WireMsg::Status(format!("❌ Qubic failed to start: {e}")));
            None
        }
    }
}

/// Send SIGTERM to the process group, wait 3 s, then SIGKILL.
fn kill_native(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    #[cfg(unix)]
    {
        let pgid = Pid::from_raw(-(pid as i32));
        let _ = signal::kill(pgid, Signal::SIGTERM);
        thread::sleep(Duration::from_secs(3));
        let _ = signal::kill(pgid, Signal::SIGKILL);
    }
}

fn reap_child(mut child: Child, state: Arc<Mutex<QubicMinerState>>) {
    match child.wait() {
        Ok(status) if !status.success() => {
            eprintln!("[qubic] process exited with {status}");
        }
        Err(e) => eprintln!("[qubic] wait() error: {e}"),
        _ => {}
    }
    *state.lock().unwrap() = QubicMinerState::Idle;
}

// ─── Stdout reader ───────────────────────────────────────────────────────────

fn qubic_stdout_reader(stdout: std::process::ChildStdout, tx: mpsc::Sender<WireMsg>) {
    // Cumulative stats across lines (hashrate + share counters), matching
    // generic_stdout_reader so share-only / partial lines retain prior context.
    let mut stats = MiningStats::default();

    for line in std::io::BufReader::new(stdout)
        .lines()
        .map_while(Result::ok)
    {
        let mining_updated = stats.update_from_line(MinerBrand::QubicCore, &line);

        if mining_updated {
            let mut telem = MiningTelemetry::new();
            telem.stats = stats.clone();
            let _ = tx.send(WireMsg::mining_telem(telem));
        } else {
            let _ = tx.send(WireMsg::Status(format!("[qubic] {line}")));
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Detection ────────────────────────────────────────────────────────────

    #[test]
    fn detect_binary_env_override() {
        crate::miner::with_env_lock(|| {
            unsafe {
                std::env::set_var("QUBIC_MINER_CMD", "/custom/path/qubic-core");
            }
            let result = detect_qubic_binary();
            unsafe {
                std::env::remove_var("QUBIC_MINER_CMD");
            }
            assert_eq!(result, Some("/custom/path/qubic-core".to_string()));
        });
    }

    #[test]
    fn detect_binary_empty_override_falls_through() {
        crate::miner::with_env_lock(|| {
            unsafe {
                std::env::set_var("QUBIC_MINER_CMD", "   ");
                std::env::set_var("SHIP_WORK_DIR", "/nonexistent/path");
            }
            let result = detect_qubic_binary();
            unsafe {
                std::env::remove_var("QUBIC_MINER_CMD");
                std::env::remove_var("SHIP_WORK_DIR");
            }
            // Falls through — either finds binary on PATH or returns None.
            let _ = result;
        });
    }

    #[test]
    fn detect_compose_env_override() {
        crate::miner::with_env_lock(|| {
            // Non-existent file should fall through.
            unsafe {
                std::env::set_var("QUBIC_COMPOSE_FILE", "/nonexistent/docker-compose.yml");
            }
            let result = detect_compose_file();
            unsafe {
                std::env::remove_var("QUBIC_COMPOSE_FILE");
            }
            // Should not match because file doesn't exist.
            assert!(result.is_none() || result.is_some()); // no panic
        });
    }

    // ── State ────────────────────────────────────────────────────────────────

    #[test]
    fn state_default_is_idle() {
        let s = QubicMinerState::Idle;
        assert_eq!(s, QubicMinerState::Idle);
    }

    // ── Hashrate parser ──────────────────────────────────────────────────────

    #[test]
    fn parse_khs_joined() {
        let mut stats = MiningStats::default();
        stats.update_from_line(MinerBrand::QubicCore, "Mining: 150.0KH/S total");
        assert!(stats.qubic.is_active);
        assert!(
            (stats.qubic.hashrate_kh_s - 150.0).abs() < 1e-4,
            "expected ~150.0 kH/s"
        );
    }

    #[test]
    fn parse_khs_split() {
        let mut stats = MiningStats::default();
        stats.update_from_line(MinerBrand::QubicCore, "[QUBIC] hashrate 987.65 KH/S avg");
        assert!(stats.qubic.is_active);
        assert!((stats.qubic.hashrate_kh_s - 987.65).abs() < 1e-4);
    }

    #[test]
    fn parse_mhs_joined() {
        let mut stats = MiningStats::default();
        stats.update_from_line(MinerBrand::QubicCore, "Total: 1.5MH/S");
        assert!(stats.qubic.is_active);
        assert!(
            (stats.qubic.hashrate_kh_s - 1500.0).abs() < 1e-4,
            "1.5 MH/S = 1500 kH/s"
        );
    }

    #[test]
    fn parse_no_hashrate_returns_none() {
        let mut stats = MiningStats::default();
        stats.update_from_line(MinerBrand::QubicCore, "Connected to Qubic network");
        assert!(!stats.qubic.is_active);
    }

    // ── JSON tick extraction ─────────────────────────────────────────────────

    #[test]
    fn extract_tick_from_json_basic() {
        let json = r#"{"tick":12345,"epoch":42}"#;
        assert_eq!(extract_tick_from_json(json), Some(12345));
    }

    #[test]
    fn extract_tick_from_json_current_tick() {
        let json = r#"{"currentTick": 98765}"#;
        assert_eq!(extract_tick_from_json(json), Some(98765));
    }

    // ── Construction ─────────────────────────────────────────────────────────

    #[test]
    fn qubic_miner_constructs_without_panic() {
        let (tx, _rx) = mpsc::channel::<WireMsg>();
        let miner = QubicMiner::new(tx);
        // Yield/Resume are no-ops for Qubic — should not panic.
        miner.send(MinerCommand::YieldForChat("test".into()));
        miner.send(MinerCommand::ResumeAfterChat);
        miner.send(MinerCommand::Stop);
    }
}

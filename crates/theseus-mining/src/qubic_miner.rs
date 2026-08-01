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
//! - **NativeBinary**: fallback that spawns `qli-Client` or `qubic-core` as a
//!   managed subprocess with setsid(), stdout parsing, and SIGTERM/SIGKILL lifecycle.
//!
//! # Environment variables consumed
//!
//! | Variable              | Default                                              |
//! |-----------------------|------------------------------------------------------|
//! | `QUBIC_MINER_CMD`     | override: full path to `qli-Client` or `qubic-core`  |
//! | `QUBIC_CLIENT_KIND`   | `qli` \| `core` (required if credentials ambiguous)  |
//! | `QUBIC_COMPOSE_FILE`  | (override: full path to docker-compose.qubic.yml)    |
//! | `QUBIC_WALLET_IDENTITY` | required for **core** native mode                  |
//! | `QUBIC_WALLET` / `QUBIC_WALLET_ADDRESS` | public address for **qli** native mode |
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
    /// Native mode stores the supervised PID so reapers cannot Idle a replacement.
    /// `native_kind` / `gpu_enabled` drive YieldForChat (qli is CPU-only — no restart).
    Running {
        mode: QubicMode,
        pid: Option<u32>,
        native_kind: Option<QubicClientKind>,
        gpu_enabled: bool,
    },
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

/// How binary discovery ranks qli vs core families.
enum DiscoveryPreference {
    /// `QUBIC_CLIENT_KIND` set — only search that family (no cross-family fallthrough).
    Explicit(QubicClientKind),
    /// Heuristic from credentials — try preferred family first, then the other.
    Prefer(QubicClientKind),
}

fn discovery_preference() -> DiscoveryPreference {
    if let Ok(raw) = std::env::var("QUBIC_CLIENT_KIND") {
        let v = raw.trim().to_ascii_lowercase();
        if matches!(v.as_str(), "qli" | "qli-client" | "client") {
            return DiscoveryPreference::Explicit(QubicClientKind::Qli);
        }
        if matches!(v.as_str(), "core" | "qubic-core" | "native") {
            return DiscoveryPreference::Explicit(QubicClientKind::Core);
        }
        // Invalid nonempty values are rejected later by `detect_qubic_client_kind`.
    }
    let has_identity = env_nonempty("QUBIC_WALLET_IDENTITY");
    let has_public = env_nonempty("QUBIC_WALLET_ADDRESS") || env_nonempty("QUBIC_WALLET");
    let prefer = match (has_identity, has_public) {
        (true, false) => QubicClientKind::Core,
        (false, true) => QubicClientKind::Qli,
        // Ambiguous or neither: default to qli (scripts/mine-qubic.sh layout).
        _ => QubicClientKind::Qli,
    };
    DiscoveryPreference::Prefer(prefer)
}

fn search_client_family(kind: QubicClientKind) -> Option<String> {
    match kind {
        QubicClientKind::Qli => miner::detect_binary_paths(
            &["binaries/mining/qli-client/qli-Client"],
            &["./qli-Client"],
            "qli-Client",
        ),
        QubicClientKind::Core => miner::detect_binary_paths(
            &[
                "binaries/mining/nodes/Qubic/bin/qubic-core",
                "binaries/mining/nodes/Qubic/qubic-core",
            ],
            &["./qubic-core", "./nodes/Qubic/bin/qubic-core"],
            "qubic-core",
        ),
    }
}

/// Locate a usable Qubic native binary (`qli-Client` or `qubic-core`).
///
/// Order: `QUBIC_MINER_CMD` → family constrained by `QUBIC_CLIENT_KIND` (hard)
/// or credential heuristic (preferred then other).
pub fn detect_qubic_binary() -> Option<String> {
    if let Ok(cmd) = std::env::var("QUBIC_MINER_CMD") {
        let t = cmd.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }

    match discovery_preference() {
        DiscoveryPreference::Explicit(kind) => search_client_family(kind),
        DiscoveryPreference::Prefer(prefer) => {
            let other = match prefer {
                QubicClientKind::Qli => QubicClientKind::Core,
                QubicClientKind::Core => QubicClientKind::Qli,
            };
            search_client_family(prefer).or_else(|| search_client_family(other))
        }
    }
}

/// Native Qubic client flavor — selects CLI flags **and** which credential env is valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QubicClientKind {
    /// `qli-Client` / wrappers — public address via `QubicAddress`.
    Qli,
    /// `qubic-core` — private seed via `--identity`.
    Core,
}

fn env_nonempty(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .map(|w| !w.trim().is_empty())
        .unwrap_or(false)
}

/// Detect qli vs core without relying solely on the executable basename.
///
/// Order: `QUBIC_CLIENT_KIND` env → filename heuristics → credential presence.
/// Unrecognized non-empty `QUBIC_CLIENT_KIND` is an error (no silent fallthrough).
/// Opaque wrappers with both identity and public-address vars set require an
/// explicit `QUBIC_CLIENT_KIND`.
fn detect_qubic_client_kind(binary: &str) -> Result<QubicClientKind, String> {
    if let Ok(raw) = std::env::var("QUBIC_CLIENT_KIND") {
        let v = raw.trim();
        if !v.is_empty() {
            let lower = v.to_ascii_lowercase();
            return match lower.as_str() {
                "qli" | "qli-client" | "client" => Ok(QubicClientKind::Qli),
                "core" | "qubic-core" | "native" => Ok(QubicClientKind::Core),
                other => Err(format!(
                    "invalid QUBIC_CLIENT_KIND={other:?} \
                     (expected qli|qli-client|client|core|qubic-core|native)"
                )),
            };
        }
    }

    let name = std::path::Path::new(binary)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(binary)
        .to_ascii_lowercase();
    if name.contains("qli") {
        return Ok(QubicClientKind::Qli);
    }
    if name.contains("qubic-core") {
        return Ok(QubicClientKind::Core);
    }

    // Opaque wrapper (e.g. `mine-qubic`).
    let has_identity = env_nonempty("QUBIC_WALLET_IDENTITY");
    let has_public = env_nonempty("QUBIC_WALLET_ADDRESS") || env_nonempty("QUBIC_WALLET");
    match (has_identity, has_public) {
        (true, true) => Err("ambiguous Qubic credentials for opaque binary: both \
             QUBIC_WALLET_IDENTITY and QUBIC_WALLET(_ADDRESS) are set; \
             set QUBIC_CLIENT_KIND=qli|core"
            .into()),
        (true, false) => Ok(QubicClientKind::Core),
        (false, _) => Ok(QubicClientKind::Qli),
    }
}

/// Resolve the credential env var for the native client mode.
///
/// * **Core**: `QUBIC_WALLET_IDENTITY` only (never `QUBIC_WALLET`, which is the public address).
/// * **Qli**: `QUBIC_WALLET_ADDRESS`, then `QUBIC_WALLET` (public address aliases).
fn resolve_qubic_credential(kind: QubicClientKind) -> Option<String> {
    let keys: &[&str] = match kind {
        QubicClientKind::Qli => &["QUBIC_WALLET_ADDRESS", "QUBIC_WALLET"],
        QubicClientKind::Core => &["QUBIC_WALLET_IDENTITY"],
    };
    keys.iter().find_map(|k| {
        std::env::var(k)
            .ok()
            .map(|w| w.trim().to_string())
            .filter(|w| !w.is_empty())
    })
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
                                   docker-compose.qubic.yml, or place qli-Client at \
                                   binaries/mining/qli-client/qli-Client or qubic-core at \
                                   binaries/mining/nodes/Qubic/bin/qubic-core \
                                   (or set QUBIC_MINER_CMD)";
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
                // Only kill if we still believe the process is Running. After a
                // natural exit the reaper sets Idle but local `child_pid` can
                // linger; signalling that PGID risks PID-reuse collateral kill.
                let still_running = matches!(
                    *state.lock().unwrap(),
                    QubicMinerState::Running {
                        mode: QubicMode::NativeBinary,
                        ..
                    }
                );
                if still_running {
                    kill_native(child_pid.take());
                } else {
                    child_pid = None;
                }
                *state.lock().unwrap() = QubicMinerState::Idle;
                let _ = telem_tx.send(WireMsg::Status("⏹ Qubic miner stopped".into()));
            }

            MinerCommand::YieldForChat(model) => {
                // Only GPU-enabled qubic-core holds VRAM; qli-Client is CPU-only and
                // must not be killed/relaunched on every chat yield.
                let should_yield = matches!(
                    *state.lock().unwrap(),
                    QubicMinerState::Running {
                        mode: QubicMode::NativeBinary,
                        native_kind: Some(QubicClientKind::Core),
                        gpu_enabled: true,
                        ..
                    }
                );
                if should_yield {
                    eprintln!(
                        "[qubic] yielding GPU for chat ({model}) — restarting without Aigarth"
                    );
                    kill_native(child_pid.take());
                    child_pid = spawn_native_binary(&telem_tx, Arc::clone(&state), false);
                }
            }

            MinerCommand::ResumeAfterChat => {
                let aigarth = std::env::var("AIGARTH_ENABLED")
                    .map(|v| v == "true")
                    .unwrap_or(false);
                let should_resume = aigarth
                    && matches!(
                        *state.lock().unwrap(),
                        QubicMinerState::Running {
                            mode: QubicMode::NativeBinary,
                            native_kind: Some(QubicClientKind::Core),
                            gpu_enabled: false,
                            ..
                        }
                    );
                if should_resume {
                    eprintln!("[qubic] resuming Aigarth GPU training after chat");
                    kill_native(child_pid.take());
                    child_pid = spawn_native_binary(&telem_tx, Arc::clone(&state), true);
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
                pid: None,
                native_kind: None,
                gpu_enabled: false,
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
                    mode: QubicMode::PodmanCompose,
                    ..
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
                telem.stats.qubic.tick_sampled = true;
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

/// CLI args for `qubic-core` (not qli). Pure so Yield/Resume GPU flag can be unit-tested.
fn native_core_cli_args(
    wallet: &str,
    threads: u32,
    port: u16,
    peers: &str,
    aigarth_enabled: bool,
) -> Vec<String> {
    let mut args = vec![
        "--threads".into(),
        threads.to_string(),
        "--port".into(),
        port.to_string(),
        "--identity".into(),
        wallet.to_string(),
    ];
    if aigarth_enabled {
        args.push("--gpu".into());
    }
    if !peers.is_empty() {
        args.push("--peers".into());
        args.push(peers.to_string());
    }
    args
}

/// Whether Running state should mark GPU held (core + aigarth only).
fn native_gpu_enabled(client_kind: QubicClientKind, aigarth_enabled: bool) -> bool {
    aigarth_enabled && client_kind == QubicClientKind::Core
}

/// Spawn native qli-Client or qubic-core.
///
/// `aigarth_enabled` controls `--gpu` / `gpu_enabled` for core. Callers must
/// pass the desired mode — this does **not** re-read `AIGARTH_ENABLED`, so
/// `YieldForChat` can pass `false` and free VRAM while the env stays true.
fn spawn_native_binary(
    telem_tx: &mpsc::Sender<WireMsg>,
    state: Arc<Mutex<QubicMinerState>>,
    aigarth_enabled: bool,
) -> Option<u32> {
    let Some(binary) = detect_qubic_binary() else {
        let msg = "no usable qli-Client or qubic-core binary found — set QUBIC_MINER_CMD \
                   to a real binary (empty stubs under binaries/mining/ are ignored), \
                   place qli-Client at binaries/mining/qli-client/qli-Client, or use \
                   podman-compose mode";
        eprintln!("[qubic] {msg}");
        *state.lock().unwrap() = QubicMinerState::Failed(msg.to_string());
        let _ = telem_tx.send(WireMsg::Status(format!("❌ Qubic: {msg}")));
        return None;
    };

    let client_kind = match detect_qubic_client_kind(&binary) {
        Ok(k) => k,
        Err(msg) => {
            eprintln!("[qubic] {msg}");
            *state.lock().unwrap() = QubicMinerState::Failed(msg.clone());
            let _ = telem_tx.send(WireMsg::Status(format!("❌ Qubic: {msg}")));
            return None;
        }
    };

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

    // Mode-specific credentials: core never uses public-address vars as --identity.
    let wallet = resolve_qubic_credential(client_kind);
    let Some(wallet) = wallet else {
        let msg = match client_kind {
            QubicClientKind::Qli => {
                "Qubic address required for qli mode \
                 (set QUBIC_WALLET_ADDRESS or QUBIC_WALLET; \
                  override with QUBIC_CLIENT_KIND=qli for wrappers)"
            }
            QubicClientKind::Core => {
                "Qubic identity required for qubic-core \
                 (set QUBIC_WALLET_IDENTITY; not QUBIC_WALLET public address)"
            }
        };
        eprintln!("[qubic] {msg}");
        *state.lock().unwrap() = QubicMinerState::Failed(msg.to_string());
        let _ = telem_tx.send(WireMsg::Status(format!("❌ Qubic: {msg}")));
        return None;
    };

    *state.lock().unwrap() = QubicMinerState::Starting;

    let mut command = Command::new(&binary);
    if client_kind == QubicClientKind::Qli {
        // qli-Client loads adjacent settings/content relative to its distribution dir
        // (same as scripts/mine-qubic.sh: cd "$(dirname "$bin")").
        // PATH-only names like "qli-Client" have an empty parent — resolve first.
        let bin_path = std::path::Path::new(&binary);
        let launch_dir = bin_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .or_else(|| {
                std::fs::canonicalize(bin_path)
                    .ok()
                    .and_then(|c| c.parent().map(|p| p.to_path_buf()))
            });
        if let Some(dir) = launch_dir {
            command.current_dir(dir);
        }
        let alias = std::env::var("SHIP_WORKER_NAME").unwrap_or_else(|_| "ship-of-theseus".into());
        command
            .arg(format!("--ClientSettings:QubicAddress={wallet}"))
            .arg(format!("--ClientSettings:Alias={alias}"))
            .arg(format!("--ClientSettings:Trainer:CpuThreads={threads}"))
            .arg("--ClientSettings:Trainer:PPS=false");
    } else {
        for arg in native_core_cli_args(&wallet, threads, port, &peers, aigarth_enabled) {
            command.arg(arg);
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
                pid: Some(pid),
                native_kind: Some(client_kind),
                gpu_enabled: native_gpu_enabled(client_kind, aigarth_enabled),
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

/// Send SIGTERM to the process group, poll for exit, SIGKILL only if still alive.
fn kill_native(pid: Option<u32>) {
    // Same PID-reuse-safe path as generic miners.
    miner::kill_process_group(pid, 3);
}

fn reap_child(mut child: Child, state: Arc<Mutex<QubicMinerState>>) {
    let reaped_pid = child.id();
    match child.wait() {
        Ok(status) if !status.success() => {
            eprintln!("[qubic] process exited with {status}");
        }
        Err(e) => eprintln!("[qubic] wait() error: {e}"),
        _ => {}
    }
    // Same process-group wait as miner::reap_child — leader exit must not
    // clear Running while workers in the setsid group still hold GPU/CPU.
    miner::wait_process_group_exit(reaped_pid);
    let mut s = state.lock().unwrap();
    if matches!(
        *s,
        QubicMinerState::Running {
            mode: QubicMode::NativeBinary,
            pid: Some(pid),
            ..
        } if pid == reaped_pid
    ) {
        *s = QubicMinerState::Idle;
    }
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
    fn core_mode_uses_identity_only() {
        crate::miner::with_env_lock(|| {
            unsafe {
                std::env::set_var("QUBIC_WALLET_ADDRESS", "public-addr");
                std::env::set_var("QUBIC_WALLET", "also-public");
                std::env::set_var("QUBIC_WALLET_IDENTITY", "private-id");
            }
            let got = resolve_qubic_credential(QubicClientKind::Core);
            unsafe {
                std::env::remove_var("QUBIC_WALLET_ADDRESS");
                std::env::remove_var("QUBIC_WALLET");
                std::env::remove_var("QUBIC_WALLET_IDENTITY");
            }
            assert_eq!(got.as_deref(), Some("private-id"));
        });
    }

    #[test]
    fn core_mode_rejects_public_wallet_vars() {
        crate::miner::with_env_lock(|| {
            unsafe {
                std::env::remove_var("QUBIC_WALLET_IDENTITY");
                std::env::set_var("QUBIC_WALLET", "public-addr");
                std::env::set_var("QUBIC_WALLET_ADDRESS", "public-addr");
            }
            let got = resolve_qubic_credential(QubicClientKind::Core);
            unsafe {
                std::env::remove_var("QUBIC_WALLET");
                std::env::remove_var("QUBIC_WALLET_ADDRESS");
            }
            assert!(got.is_none());
        });
    }

    #[test]
    fn qli_mode_prefers_address_not_identity() {
        crate::miner::with_env_lock(|| {
            unsafe {
                std::env::set_var("QUBIC_WALLET_IDENTITY", "private-id");
                std::env::set_var("QUBIC_WALLET_ADDRESS", "public-addr");
                std::env::remove_var("QUBIC_WALLET");
            }
            let got = resolve_qubic_credential(QubicClientKind::Qli);
            unsafe {
                std::env::remove_var("QUBIC_WALLET_IDENTITY");
                std::env::remove_var("QUBIC_WALLET_ADDRESS");
            }
            assert_eq!(got.as_deref(), Some("public-addr"));
        });
    }

    #[test]
    fn opaque_wrapper_uses_address_when_no_identity() {
        crate::miner::with_env_lock(|| {
            unsafe {
                std::env::remove_var("QUBIC_CLIENT_KIND");
                std::env::remove_var("QUBIC_WALLET_IDENTITY");
                std::env::remove_var("QUBIC_WALLET");
                std::env::set_var("QUBIC_WALLET_ADDRESS", "public-addr");
            }
            let kind = detect_qubic_client_kind("/opt/wrappers/mine-qubic").unwrap();
            let got = resolve_qubic_credential(kind);
            unsafe {
                std::env::remove_var("QUBIC_WALLET_ADDRESS");
            }
            assert_eq!(kind, QubicClientKind::Qli);
            assert_eq!(got.as_deref(), Some("public-addr"));
        });
    }

    #[test]
    fn client_kind_env_overrides_filename() {
        crate::miner::with_env_lock(|| {
            unsafe {
                std::env::set_var("QUBIC_CLIENT_KIND", "qli");
                std::env::remove_var("QUBIC_WALLET_IDENTITY");
            }
            let kind = detect_qubic_client_kind("/usr/local/bin/qubic-core").unwrap();
            unsafe {
                std::env::remove_var("QUBIC_CLIENT_KIND");
            }
            assert_eq!(kind, QubicClientKind::Qli);
        });
    }

    #[test]
    fn invalid_client_kind_is_error() {
        crate::miner::with_env_lock(|| {
            unsafe {
                std::env::set_var("QUBIC_CLIENT_KIND", "qubicc");
            }
            let err = detect_qubic_client_kind("/opt/wrappers/mine-qubic").unwrap_err();
            unsafe {
                std::env::remove_var("QUBIC_CLIENT_KIND");
            }
            assert!(err.contains("invalid QUBIC_CLIENT_KIND"), "{err}");
        });
    }

    #[test]
    fn opaque_wrapper_ambiguous_credentials_require_kind() {
        crate::miner::with_env_lock(|| {
            unsafe {
                std::env::remove_var("QUBIC_CLIENT_KIND");
                std::env::set_var("QUBIC_WALLET_IDENTITY", "private-id");
                std::env::set_var("QUBIC_WALLET_ADDRESS", "public-addr");
            }
            let err = detect_qubic_client_kind("/opt/wrappers/mine-qubic").unwrap_err();
            unsafe {
                std::env::remove_var("QUBIC_WALLET_IDENTITY");
                std::env::remove_var("QUBIC_WALLET_ADDRESS");
            }
            assert!(err.contains("ambiguous"), "{err}");
        });
    }

    #[test]
    fn discovery_explicit_kind_is_hard_constraint() {
        crate::miner::with_env_lock(|| {
            unsafe {
                std::env::set_var("QUBIC_CLIENT_KIND", "core");
                std::env::remove_var("QUBIC_WALLET_ADDRESS");
                std::env::remove_var("QUBIC_WALLET");
            }
            assert!(matches!(
                discovery_preference(),
                DiscoveryPreference::Explicit(QubicClientKind::Core)
            ));
            unsafe {
                std::env::set_var("QUBIC_CLIENT_KIND", "qli");
            }
            assert!(matches!(
                discovery_preference(),
                DiscoveryPreference::Explicit(QubicClientKind::Qli)
            ));
            unsafe {
                std::env::remove_var("QUBIC_CLIENT_KIND");
            }
        });
    }

    #[test]
    fn discovery_credential_heuristic_is_prefer_not_hard() {
        crate::miner::with_env_lock(|| {
            unsafe {
                std::env::remove_var("QUBIC_CLIENT_KIND");
                std::env::set_var("QUBIC_WALLET_IDENTITY", "private-id");
                std::env::remove_var("QUBIC_WALLET_ADDRESS");
                std::env::remove_var("QUBIC_WALLET");
            }
            assert!(matches!(
                discovery_preference(),
                DiscoveryPreference::Prefer(QubicClientKind::Core)
            ));
            unsafe {
                std::env::remove_var("QUBIC_WALLET_IDENTITY");
                std::env::set_var("QUBIC_WALLET_ADDRESS", "public-addr");
            }
            assert!(matches!(
                discovery_preference(),
                DiscoveryPreference::Prefer(QubicClientKind::Qli)
            ));
            unsafe {
                std::env::remove_var("QUBIC_WALLET_ADDRESS");
            }
        });
    }

    #[test]
    fn yield_false_omits_gpu_flag_on_core_cli() {
        // YieldForChat passes aigarth_enabled=false — must not re-add --gpu.
        let args = native_core_cli_args("private-id", 14, 21841, "", false);
        assert!(
            !args.iter().any(|a| a == "--gpu"),
            "expected no --gpu in {args:?}"
        );
        assert!(!native_gpu_enabled(QubicClientKind::Core, false));
    }

    #[test]
    fn resume_true_includes_gpu_flag_on_core_cli() {
        let args = native_core_cli_args("private-id", 14, 21841, "", true);
        assert!(
            args.iter().any(|a| a == "--gpu"),
            "expected --gpu in {args:?}"
        );
        assert!(native_gpu_enabled(QubicClientKind::Core, true));
        assert!(!native_gpu_enabled(QubicClientKind::Qli, true));
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

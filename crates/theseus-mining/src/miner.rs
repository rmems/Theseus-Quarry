//! Shared miner infrastructure — process management, binary detection, supervisor loop.
//!
//! Each coin module provides a `spawn_fn` with coin-specific args and uses this
//! module for everything else: kill, reap, setsid, stdout parsing, state management.

use std::io::BufRead;
use std::process::{Child, Command};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use nix::sys::signal::{self, Signal};
use nix::unistd::Pid;

use mining_telemetry_core::{MinerBrand, MiningStats, MiningTelemetry, WireMsg};

use crate::throttle::MinerCommand;

// ─── Generic MinerState ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum MinerState {
    Idle,
    Running { pid: u32 },
    Failed(String),
}

// ─── Coin-specific config ────────────────────────────────────────────────────

pub struct MinerConfig {
    pub name: &'static str,
    pub brand: MinerBrand,
    pub kill_timeout: u64,
    pub log_prefix: String,
}

// ─── Process management ──────────────────────────────────────────────────────

pub fn kill_process_group(pid: Option<u32>, timeout_secs: u64) {
    let Some(pid) = pid else { return };
    #[cfg(unix)]
    {
        let pgid = Pid::from_raw(-(pid as i32));
        let _ = signal::kill(pgid, Signal::SIGTERM);
        thread::sleep(Duration::from_secs(timeout_secs));
        let _ = signal::kill(pgid, Signal::SIGKILL);
    }
}

pub fn reap_child(mut child: Child, state: Arc<Mutex<MinerState>>, log_prefix: &str) {
    match child.wait() {
        Ok(status) if !status.success() => {
            eprintln!("{log_prefix} process exited with {status}");
        }
        Err(e) => eprintln!("{log_prefix} wait() error: {e}"),
        _ => {}
    }
    *state.lock().unwrap() = MinerState::Idle;
}

pub fn setsid_command(command: &mut Command) {
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            nix::unistd::setsid().ok();
            Ok(())
        });
    }
}

/// Repo root for mining binaries (`binaries/mining/...`).
///
/// Order: `SHIP_WORK_DIR` → cwd if it looks like this repo → `$HOME/Theseus-Quarry`.
pub fn ship_work_dir() -> String {
    if let Ok(dir) = std::env::var("SHIP_WORK_DIR") {
        let t = dir.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join("binaries/mining").is_dir() || cwd.join("crates/theseus-mining").is_dir() {
            return cwd.to_string_lossy().into_owned();
        }
    }
    let home = std::env::var("HOME").unwrap_or_default();
    format!("{home}/Theseus-Quarry")
}

/// True if `path` is a non-empty regular file (skips 0-byte placeholders / text stubs).
pub fn is_usable_binary(path: &str) -> bool {
    let p = std::path::Path::new(path);
    match std::fs::metadata(p) {
        Ok(meta) if meta.is_file() && meta.len() > 1024 => true,
        _ => false,
    }
}

/// Detect a binary: env override → canonical paths under repo → local paths → `which`.
pub fn detect_binary(
    env_var: &str,
    canonical_paths: &[&str],
    local_paths: &[&str],
    which_name: &str,
) -> Option<String> {
    if let Ok(cmd) = std::env::var(env_var) {
        let t = cmd.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }

    let base = ship_work_dir();
    for tmpl in canonical_paths {
        let path = format!("{base}/{tmpl}");
        if is_usable_binary(&path) {
            return Some(path);
        }
    }

    for p in local_paths {
        if is_usable_binary(p) {
            return Some(p.to_string());
        }
    }

    if Command::new("which")
        .arg(which_name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return Some(which_name.to_string());
    }

    None
}

/// Serialize tests that mutate process environment (std env is global).
#[cfg(test)]
pub fn with_env_lock<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    use std::sync::{Mutex, OnceLock};
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let lock = LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn is_usable_binary_rejects_missing_and_tiny() {
        assert!(!is_usable_binary("/nonexistent/miner-binary-xyz"));
        let dir = std::env::temp_dir().join("theseus_miner_detect_test");
        let _ = std::fs::create_dir_all(&dir);
        let tiny = dir.join("stub");
        std::fs::write(&tiny, b"x").unwrap();
        assert!(!is_usable_binary(tiny.to_str().unwrap()));
        let fat = dir.join("realish");
        let mut f = std::fs::File::create(&fat).unwrap();
        f.write_all(&vec![0u8; 2048]).unwrap();
        assert!(is_usable_binary(fat.to_str().unwrap()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detect_binary_env_override_wins() {
        with_env_lock(|| {
            unsafe {
                std::env::set_var("THESEUS_TEST_MINER_CMD", "/opt/custom-miner");
            }
            let got = detect_binary("THESEUS_TEST_MINER_CMD", &[], &[], "nope");
            unsafe {
                std::env::remove_var("THESEUS_TEST_MINER_CMD");
            }
            assert_eq!(got.as_deref(), Some("/opt/custom-miner"));
        });
    }
}

// ─── Generic MinerHandle ─────────────────────────────────────────────────────

pub struct MinerHandle {
    cmd_tx: mpsc::SyncSender<MinerCommand>,
}

impl MinerHandle {
    pub fn new(
        name: &'static str,
        telem_tx: mpsc::Sender<WireMsg>,
        config: MinerConfig,
        spawn_fn: fn(&mpsc::Sender<WireMsg>, Arc<Mutex<MinerState>>, &MinerConfig) -> Option<u32>,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::sync_channel::<MinerCommand>(32);
        thread::Builder::new()
            .name(format!("{name}-supervisor"))
            .spawn(move || supervisor_loop(cmd_rx, telem_tx, config, spawn_fn))
            .expect("supervisor thread");
        Self { cmd_tx }
    }

    pub fn send(&self, command: MinerCommand) {
        let _ = self.cmd_tx.try_send(command);
    }
}

// ─── Generic supervisor loop ─────────────────────────────────────────────────

fn supervisor_loop(
    cmd_rx: mpsc::Receiver<MinerCommand>,
    telem_tx: mpsc::Sender<WireMsg>,
    config: MinerConfig,
    spawn_fn: fn(&mpsc::Sender<WireMsg>, Arc<Mutex<MinerState>>, &MinerConfig) -> Option<u32>,
) {
    let state: Arc<Mutex<MinerState>> = Arc::new(Mutex::new(MinerState::Idle));
    let mut child_pid: Option<u32> = None;
    let mut paused_for_chat = false;

    for cmd in cmd_rx {
        match cmd {
            MinerCommand::Start => {
                if matches!(*state.lock().unwrap(), MinerState::Running { .. }) {
                    eprintln!("{}already running, ignoring Start", config.log_prefix);
                    continue;
                }
                paused_for_chat = false;
                child_pid = spawn_fn(&telem_tx, Arc::clone(&state), &config);
            }

            MinerCommand::Stop => {
                paused_for_chat = false;
                kill_process_group(child_pid.take(), config.kill_timeout);
                *state.lock().unwrap() = MinerState::Idle;
                let _ = telem_tx.send(WireMsg::Status(format!(
                    "{}miner stopped",
                    config.log_prefix
                )));
            }

            MinerCommand::YieldForChat(ref model) => {
                eprintln!("{}yielding for {model}", config.log_prefix);
                kill_process_group(child_pid.take(), config.kill_timeout);
                *state.lock().unwrap() = MinerState::Idle;
                paused_for_chat = true;
                let _ = telem_tx.send(WireMsg::Status(format!(
                    "{}paused for {model}",
                    config.log_prefix
                )));
            }

            MinerCommand::ResumeAfterChat => {
                if paused_for_chat && child_pid.is_none() {
                    eprintln!("{}resuming after chat", config.log_prefix);
                    paused_for_chat = false;
                    child_pid = spawn_fn(&telem_tx, Arc::clone(&state), &config);
                }
            }

            MinerCommand::ResetDebounce => {}
        }
    }
}

// ─── Generic spawn helpers ───────────────────────────────────────────────────

pub fn spawn_managed_process(
    mut command: Command,
    name: &str,
    state: Arc<Mutex<MinerState>>,
    telem_tx: &mpsc::Sender<WireMsg>,
    brand: MinerBrand,
) -> Option<u32> {
    setsid_command(&mut command);

    match command.spawn() {
        Ok(mut child) => {
            let pid = child.id();
            *state.lock().unwrap() = MinerState::Running { pid };

            if let Some(stdout) = child.stdout.take() {
                let tx = telem_tx.clone();
                let name_str = name.to_string();
                thread::Builder::new()
                    .name(format!("{name_str}-stdout"))
                    .spawn(move || generic_stdout_reader(stdout, tx, brand, &name_str))
                    .expect("stdout thread");
            }

            if let Some(stderr) = child.stderr.take() {
                let prefix = name.to_string();
                thread::Builder::new()
                    .name(format!("{name}-stderr"))
                    .spawn(move || {
                        for line in std::io::BufReader::new(stderr).lines().flatten() {
                            eprintln!("[{prefix}-stderr] {line}");
                        }
                    })
                    .expect("stderr thread");
            }

            let state_reap = Arc::clone(&state);
            let prefix = name.to_string();
            thread::Builder::new()
                .name(format!("{name}-reap"))
                .spawn(move || reap_child(child, state_reap, &prefix))
                .expect("reap thread");

            Some(pid)
        }
        Err(e) => {
            *state.lock().unwrap() = MinerState::Failed(e.to_string());
            let _ = telem_tx.send(WireMsg::Status(format!("[{name}] spawn failed: {e}")));
            None
        }
    }
}

pub fn generic_stdout_reader(
    stdout: std::process::ChildStdout,
    tx: mpsc::Sender<WireMsg>,
    brand: MinerBrand,
    name: &str,
) {
    for line in std::io::BufReader::new(stdout).lines().flatten() {
        let mut stats = MiningStats::default();
        stats.update_from_line(brand, &line);

        let is_active = match brand {
            MinerBrand::DynexSolver => stats.dynex.is_active,
            MinerBrand::BzMiner => stats.kaspa.is_active,
            MinerBrand::Xmrig => stats.monero.is_active,
            MinerBrand::Rigel => stats.quai.is_active,
            MinerBrand::QubicCore => stats.qubic.is_active,
            MinerBrand::SRBMiner => stats.monero.is_active,
            MinerBrand::Hellminer => stats.verus.is_active,
            MinerBrand::Unknown => false,
        };

        if is_active {
            let mut telem = MiningTelemetry::new();
            telem.stats = stats;
            let _ = tx.send(WireMsg::MiningTelem(telem));
        } else {
            let _ = tx.send(WireMsg::Status(format!("[{name}] {line}")));
        }
    }
}

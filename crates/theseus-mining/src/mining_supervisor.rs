//! Unified mining supervisor — coordinates Dynex and/or Quai miners.
//!
//! # Usage
//!
//! ```no_run
//! use theseus_mining::mining_supervisor::{MiningAlgo, UnifiedMining};
//! use mining_telemetry_core::WireMsg;
//! use std::sync::mpsc;
//!
//! let (tx, _rx) = mpsc::channel::<WireMsg>();
//! let mut um = UnifiedMining::new(MiningAlgo::Both, tx);
//! um.start();
//! // ... run until Ctrl+C ...
//! um.stop();
//! ```

use std::str::FromStr;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use mining_telemetry_core::WireMsg;

use crate::dynex_miner::DynexMiner;
use crate::gpu_scheduler::{GpuScheduler, GpuSchedulerConfig};
use crate::kaspa_miner::KaspaMiner;
use crate::quai_miner::QuaiMiner;
use crate::qubic_miner::QubicMiner;
use crate::rotation::{AlgoRotator, MarketSnapshot, RotationConfig};
use crate::state::{self, MiningPersistentState, RotationRecord};
use crate::throttle::MinerCommand;

// ─── Algorithm selection ─────────────────────────────────────────────────────

/// Which miners to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiningAlgo {
    Dynex,
    Quai,
    Qubic,
    /// Dynex + Quai (legacy alias).
    Both,
    /// Kaspa miner.
    Kaspa,
    /// All miners.
    All,
}

impl MiningAlgo {
    pub fn runs_dynex(self) -> bool {
        matches!(self, MiningAlgo::Dynex | MiningAlgo::Both | MiningAlgo::All)
    }

    pub fn runs_quai(self) -> bool {
        matches!(self, MiningAlgo::Quai | MiningAlgo::Both | MiningAlgo::All)
    }

    pub fn runs_qubic(self) -> bool {
        matches!(self, MiningAlgo::Qubic | MiningAlgo::All)
    }

    pub fn runs_kaspa(self) -> bool {
        matches!(self, MiningAlgo::Kaspa | MiningAlgo::All)
    }
}

impl FromStr for MiningAlgo {
    type Err = String;

    /// Parse from the `--algo` flag value: "dynex", "quai", "qubic", "dynex,quai", "both", "all".
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_lowercase();
        let parts: Vec<&str> = lower.split(',').map(str::trim).collect();

        // Validate all parts are recognized names.
        let valid_names = ["all", "both", "dynex", "quai", "qubic", "kaspa"];
        for part in &parts {
            if !valid_names.contains(part) {
                return Err(format!("unknown algorithm: {part}"));
            }
        }

        let has_all = parts.contains(&"all");
        let has_dynex = parts.contains(&"dynex");
        let has_quai = parts.contains(&"quai");
        let has_qubic = parts.contains(&"qubic");
        let has_kaspa = parts.contains(&"kaspa");
        let has_both = parts.contains(&"both");

        if has_all || (has_dynex && has_quai && has_qubic && has_kaspa) {
            return Ok(MiningAlgo::All);
        }

        match (
            has_dynex || has_both,
            has_quai || has_both,
            has_qubic,
            has_kaspa,
        ) {
            (true, true, false, false) => Ok(MiningAlgo::Both),
            (true, false, false, false) => Ok(MiningAlgo::Dynex),
            (false, true, false, false) => Ok(MiningAlgo::Quai),
            (false, false, true, false) => Ok(MiningAlgo::Qubic),
            (false, false, false, true) => Ok(MiningAlgo::Kaspa),
            // Any other valid multi-coin combo (e.g. "dynex,qubic,kaspa") → All
            _ => Ok(MiningAlgo::All),
        }
    }
}

impl std::fmt::Display for MiningAlgo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MiningAlgo::Dynex => write!(f, "dynex"),
            MiningAlgo::Quai => write!(f, "quai"),
            MiningAlgo::Qubic => write!(f, "qubic"),
            MiningAlgo::Kaspa => write!(f, "kaspa"),
            MiningAlgo::Both => write!(f, "dynex+quai"),
            MiningAlgo::All => write!(f, "dynex+quai+qubic+kaspa"),
        }
    }
}

// ─── Unified mining handle ───────────────────────────────────────────────────

/// Coordinates Dynex and/or Quai mining with a single start/stop API.
///
/// Both miners run independently in their own threads/processes.
/// They share the same telemetry bus (`telem_tx`) so the SNN supervisor
/// receives a unified stream of `WireMsg::Mining` events.
///
/// When a GPU algo is active, the embedded `GpuScheduler` polls NVML
/// every 2 seconds and translates decisions into `MinerCommand`s.
/// Rollback state for mid-rotation failure recovery.
struct RollbackState {
    old_algo: MiningAlgo,
    deadline: Instant,
}

pub struct UnifiedMining {
    algo: MiningAlgo,
    dynex: Option<DynexMiner>,
    quai: Option<QuaiMiner>,
    qubic: Option<QubicMiner>,
    kaspa: Option<KaspaMiner>,
    scheduler: Option<GpuScheduler>,
    rotator: Option<AlgoRotator>,
    telem_tx: mpsc::Sender<WireMsg>,
    /// Track whether GPU mining was paused by the scheduler so we can
    /// send Start/Stop only on transitions.
    gpu_mining_paused: bool,
    /// Pending rollback check after a rotation.
    pending_rollback: Option<RollbackState>,
    /// Saved scheduler config for reconstructing after rotation.
    sched_config: GpuSchedulerConfig,
}

impl UnifiedMining {
    /// Create the supervisor.  Call `.start()` when ready to mine.
    pub fn new(algo: MiningAlgo, telem_tx: mpsc::Sender<WireMsg>) -> Self {
        Self::with_scheduler_config(algo, telem_tx, GpuSchedulerConfig::default())
    }

    /// Create the supervisor with custom GPU scheduler config.
    pub fn with_scheduler_config(
        algo: MiningAlgo,
        telem_tx: mpsc::Sender<WireMsg>,
        sched_config: GpuSchedulerConfig,
    ) -> Self {
        let dynex = if algo.runs_dynex() {
            Some(DynexMiner::new(telem_tx.clone()))
        } else {
            None
        };
        let quai = if algo.runs_quai() {
            Some(QuaiMiner::new(telem_tx.clone()))
        } else {
            None
        };
        let qubic = if algo.runs_qubic() {
            Some(QubicMiner::new(telem_tx.clone()))
        } else {
            None
        };
        let kaspa = if algo.runs_kaspa() {
            Some(KaspaMiner::new(telem_tx.clone()))
        } else {
            None
        };

        // Create GPU scheduler only when a GPU algo is enabled.
        // Qubic is included because Aigarth uses the RTX 5080 for training.
        let scheduler = if algo.runs_dynex() || algo.runs_kaspa() || algo.runs_qubic() {
            Some(GpuScheduler::new(sched_config.clone()))
        } else {
            None
        };

        Self {
            algo,
            dynex,
            quai,
            qubic,
            kaspa,
            scheduler,
            rotator: None,
            telem_tx,
            gpu_mining_paused: false,
            pending_rollback: None,
            sched_config,
        }
    }

    /// Enable auto-rotation with the given config.
    pub fn enable_rotation(&mut self, config: RotationConfig) {
        self.rotator = Some(AlgoRotator::new(self.algo, config));
    }

    /// Start all configured miners.
    pub fn start(&mut self) {
        eprintln!("[unified-mining] starting algo={}", self.algo);

        if let Some(ref dynex) = self.dynex {
            dynex.send(MinerCommand::Start);
        }
        if let Some(ref mut kaspa) = self.kaspa {
            kaspa.send(MinerCommand::Start);
        }
        if let Some(ref quai) = self.quai {
            quai.send(MinerCommand::Start);
        }
        if let Some(ref qubic) = self.qubic {
            qubic.send(MinerCommand::Start);
        }
    }

    /// Stop all miners cleanly.
    pub fn stop(&mut self) {
        eprintln!("[unified-mining] stopping algo={}", self.algo);

        if let Some(ref dynex) = self.dynex {
            dynex.send(MinerCommand::Stop);
        }
        if let Some(ref mut kaspa) = self.kaspa {
            kaspa.send(MinerCommand::Stop);
        }
        if let Some(ref mut quai) = self.quai {
            quai.send(MinerCommand::Stop);
        }
        if let Some(ref qubic) = self.qubic {
            qubic.send(MinerCommand::Stop);
        }
    }

    /// Poll the GPU scheduler and translate its decision into miner commands.
    ///
    /// Call this every ~2 seconds from the main loop.  Only affects GPU miners
    /// (Dynex); Quai and Qubic are CPU-only and always keep running.
    pub fn poll_gpu(&mut self) {
        let scheduler = match self.scheduler.as_mut() {
            Some(s) => s,
            None => return,
        };

        let (decision, event) = scheduler.poll();

        // Emit telemetry event if present.
        if let Some(ev) = event {
            let _ = self.telem_tx.send(WireMsg::GpuSchedulerEvent(ev));
        }

        // Translate decision to miner commands on transitions only.
        let should_pause = decision.is_paused();
        if should_pause && !self.gpu_mining_paused {
            eprintln!(
                "[unified-mining] gpu-sched: pausing/yielding GPU miners ({})",
                decision.label()
            );
            if let Some(ref dynex) = self.dynex {
                dynex.send(MinerCommand::Stop);
            }
            if let Some(ref quai) = self.quai {
                quai.send(MinerCommand::YieldForChat("LLM activity".into()));
            }
            if let Some(ref qubic) = self.qubic {
                qubic.send(MinerCommand::YieldForChat("LLM activity".into()));
            }
            self.gpu_mining_paused = true;
        } else if !should_pause && self.gpu_mining_paused {
            eprintln!(
                "[unified-mining] gpu-sched: resuming GPU miners ({})",
                decision.label()
            );
            if let Some(ref dynex) = self.dynex {
                dynex.send(MinerCommand::Start);
            }
            if let Some(ref quai) = self.quai {
                quai.send(MinerCommand::ResumeAfterChat);
            }
            if let Some(ref qubic) = self.qubic {
                qubic.send(MinerCommand::ResumeAfterChat);
            }
            self.gpu_mining_paused = false;
        }
    }

    /// Yield GPU VRAM for LLM chat — delegates to the GPU scheduler.
    pub fn yield_for_chat(&mut self, model_name: &str) {
        if let Some(ref scheduler) = self.scheduler {
            eprintln!("[unified-mining] yielding GPU for chat ({model_name})");
            scheduler.set_llm_active(true);
        } else if let Some(ref dynex) = self.dynex {
            // Fallback: no scheduler, direct command.
            eprintln!("[unified-mining] yielding Dynex VRAM for chat ({model_name})");
            dynex.send(MinerCommand::YieldForChat(model_name.to_string()));
        }
        // Quai and Qubic keep running — they're CPU-only.
        // The scheduler's next poll() will translate this to a Stop command.
    }

    /// Resume GPU mining after chat finishes — clears the scheduler's LLM flag.
    pub fn resume_after_chat(&mut self) {
        if let Some(ref scheduler) = self.scheduler {
            eprintln!("[unified-mining] resuming GPU after chat");
            scheduler.set_llm_active(false);
        } else if let Some(ref dynex) = self.dynex {
            eprintln!("[unified-mining] resuming Dynex after chat");
            dynex.send(MinerCommand::ResumeAfterChat);
        }
        // The scheduler's next poll() will translate this to a Start command.
    }

    /// Reset the auto-resume debounce timer (user sent another message).
    pub fn reset_chat_debounce(&self) {
        if let Some(ref dynex) = self.dynex {
            dynex.send(MinerCommand::ResetDebounce);
        }
    }

    pub fn algo(&self) -> MiningAlgo {
        self.algo
    }

    /// Access the GPU scheduler (if present).
    pub fn scheduler(&self) -> Option<&GpuScheduler> {
        self.scheduler.as_ref()
    }

    /// Access the GPU scheduler mutably (if present).
    pub fn scheduler_mut(&mut self) -> Option<&mut GpuScheduler> {
        self.scheduler.as_mut()
    }

    /// Access the telemetry bus.
    pub fn telem_tx(&self) -> &mpsc::Sender<WireMsg> {
        &self.telem_tx
    }

    /// Check if a rotation is warranted given current market data.
    ///
    /// If the rotator recommends a switch, performs `switch_algo()` and
    /// emits telemetry.  Call this every poll cycle when `--auto-rotate` is set.
    pub fn check_rotation(&mut self, market: &MarketSnapshot) {
        let rotator = match self.rotator.as_mut() {
            Some(r) => r,
            None => return,
        };

        let (recommendation, event) = rotator.evaluate(market);
        let _ = self.telem_tx.send(WireMsg::RotationEvent(event));

        if let Some(new_algo) = recommendation {
            eprintln!("[unified-mining] rotation: {} → {}", self.algo, new_algo);
            if self.switch_algo(new_algo) {
                if let Some(ref mut r) = self.rotator {
                    r.record_rotation(new_algo);
                }
                // Save state.
                let mut persist = state::load_state().unwrap_or(MiningPersistentState {
                    last_algo: self.algo.to_string(),
                    last_rotation_ts: None,
                    thermal_throttle_c: None,
                    thermal_emergency_c: None,
                    rotation_stats: Default::default(),
                });
                persist.last_algo = new_algo.to_string();
                persist.last_rotation_ts = Some(state::now_iso8601());
                persist.rotation_stats.push(RotationRecord {
                    timestamp: state::now_iso8601(),
                    from_algo: self.algo.to_string(),
                    to_algo: new_algo.to_string(),
                    reason: "profitability".into(),
                    revenue_delta_pct: 0.0, // could be computed, but telemetry has it
                });
                let _ = state::save_state(&persist);

                let _ = self.telem_tx.send(WireMsg::Status(format!(
                    "Rotated mining algorithm: {} → {}",
                    self.algo, new_algo
                )));
            }
        }

        // Check pending rollback.
        self.check_rollback();
    }

    /// Switch to a new mining algorithm.  Stops old miners, constructs new ones,
    /// starts them, and sets up a rollback check.
    ///
    /// Returns `true` if the switch was initiated.
    pub fn switch_algo(&mut self, new_algo: MiningAlgo) -> bool {
        let old_algo = self.algo;
        eprintln!("[unified-mining] switch_algo: {} → {}", old_algo, new_algo);

        // 1. Stop old miners.
        self.stop();

        // 2. Wait for VRAM to free if switching away from GPU algo.
        if old_algo.runs_dynex()
            && !new_algo.runs_dynex()
            && let Some(ref scheduler) = self.scheduler
        {
            scheduler.verify_vram_freed(Duration::from_secs(5));
        }

        // 3. Reconstruct miners for new algo.
        self.algo = new_algo;
        self.dynex = if new_algo.runs_dynex() {
            Some(DynexMiner::new(self.telem_tx.clone()))
        } else {
            None
        };
        self.quai = if new_algo.runs_quai() {
            Some(QuaiMiner::new(self.telem_tx.clone()))
        } else {
            None
        };
        self.qubic = if new_algo.runs_qubic() {
            Some(QubicMiner::new(self.telem_tx.clone()))
        } else {
            None
        };
        self.kaspa = if new_algo.runs_kaspa() {
            Some(KaspaMiner::new(self.telem_tx.clone()))
        } else {
            None
        };

        // Update scheduler presence.
        if new_algo.runs_dynex() && self.scheduler.is_none() {
            self.scheduler = Some(GpuScheduler::new(self.sched_config.clone()));
        } else if !new_algo.runs_dynex() {
            self.scheduler = None;
        }

        // 4. Start new miners.
        self.gpu_mining_paused = false;
        self.start();

        // 5. Set up rollback check (10s grace period).
        self.pending_rollback = Some(RollbackState {
            old_algo,
            deadline: Instant::now() + Duration::from_secs(10),
        });

        true
    }

    /// Check if a pending rollback should be executed.
    fn check_rollback(&mut self) {
        let rollback = match self.pending_rollback.take() {
            Some(r) => r,
            None => return,
        };

        if Instant::now() < rollback.deadline {
            // Not yet time to check — put it back.
            self.pending_rollback = Some(rollback);
            return;
        }

        // Rollback check: all miners should have started successfully.
        // In the current architecture, miner threads are spawned and run
        // independently — a Failed state would only be detectable via
        // telemetry.  For now, we simply clear the rollback after the
        // grace period.  A full all_miners_failed() check would require
        // exposing state from each miner, which can be added incrementally.
        eprintln!(
            "[unified-mining] rollback window elapsed — rotation from {} committed",
            rollback.old_algo
        );
    }

    /// Save current state to disk (call before shutdown).
    pub fn save_state(&self) {
        let mut persist = state::load_state().unwrap_or(MiningPersistentState {
            last_algo: self.algo.to_string(),
            last_rotation_ts: None,
            thermal_throttle_c: None,
            thermal_emergency_c: None,
            rotation_stats: Default::default(),
        });
        persist.last_algo = self.algo.to_string();
        if let Err(e) = state::save_state(&persist) {
            eprintln!("[unified-mining] failed to save state: {e}");
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_algo_variants() {
        assert_eq!("dynex".parse::<MiningAlgo>().unwrap(), MiningAlgo::Dynex);
        assert_eq!("quai".parse::<MiningAlgo>().unwrap(), MiningAlgo::Quai);
        assert_eq!("qubic".parse::<MiningAlgo>().unwrap(), MiningAlgo::Qubic);
        assert_eq!(
            "dynex,quai".parse::<MiningAlgo>().unwrap(),
            MiningAlgo::Both
        );
        assert_eq!(
            "quai,dynex".parse::<MiningAlgo>().unwrap(),
            MiningAlgo::Both
        );
        assert_eq!("both".parse::<MiningAlgo>().unwrap(), MiningAlgo::Both);
        assert_eq!("kaspa".parse::<MiningAlgo>().unwrap(), MiningAlgo::Kaspa);
        assert_eq!("all".parse::<MiningAlgo>().unwrap(), MiningAlgo::All);
        assert_eq!(
            "dynex,quai,qubic,kaspa".parse::<MiningAlgo>().unwrap(),
            MiningAlgo::All
        );
        assert_eq!(
            "dynex,qubic,kaspa".parse::<MiningAlgo>().unwrap(),
            MiningAlgo::All
        );
        assert!("unknown".parse::<MiningAlgo>().is_err());
    }

    #[test]
    fn algo_membership() {
        assert!(MiningAlgo::Dynex.runs_dynex());
        assert!(!MiningAlgo::Dynex.runs_quai());
        assert!(!MiningAlgo::Dynex.runs_qubic());

        assert!(!MiningAlgo::Quai.runs_dynex());
        assert!(MiningAlgo::Quai.runs_quai());
        assert!(!MiningAlgo::Quai.runs_qubic());

        assert!(!MiningAlgo::Qubic.runs_dynex());
        assert!(!MiningAlgo::Qubic.runs_quai());
        assert!(MiningAlgo::Qubic.runs_qubic());

        assert!(MiningAlgo::Both.runs_dynex());
        assert!(MiningAlgo::Both.runs_quai());
        assert!(!MiningAlgo::Both.runs_qubic());

        assert!(MiningAlgo::All.runs_dynex());
        assert!(MiningAlgo::All.runs_quai());
        assert!(MiningAlgo::All.runs_qubic());
        assert!(MiningAlgo::All.runs_kaspa());

        assert!(MiningAlgo::Kaspa.runs_kaspa());
        assert!(!MiningAlgo::Kaspa.runs_dynex());
    }
}

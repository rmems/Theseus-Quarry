//! GPU resource scheduler — VRAM-aware, temperature-governed mining arbitration.
//!
//! Replaces the ad-hoc yield/resume pattern with a priority-based decision
//! engine that polls NVML every 2 seconds and emits `GpuSchedulerEvent`
//! telemetry on state transitions.
//!
//! # Priority ladder (highest → lowest)
//!
//! 1. LLM inference active → pause mining
//! 2. Thermal emergency (≥ 90°C) → pause mining
//! 3. Thermal throttle (≥ 80°C) → throttle mining
//! 4. VRAM pressure (used > ceiling) → pause mining
//! 5. Otherwise → mining allowed

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::Nvml;

use mining_telemetry_core::GpuSchedulerEvent;

// ─── Types ───────────────────────────────────────────────────────────────────

/// Snapshot of GPU state as seen by the scheduler.
#[derive(Debug, Clone)]
pub struct GpuSnapshot {
    pub vram_used_mb: u64,
    pub vram_total_mb: u64,
    pub gpu_temp_c: f32,
    pub power_w: f32,
}

/// Scheduler decision.
#[derive(Debug, Clone, PartialEq)]
pub enum GpuDecision {
    /// Mining may proceed (or resume).
    MiningAllowed,
    /// Mining must pause.
    MiningPaused(PauseReason),
    /// Mining should throttle (thermal pressure but not emergency).
    MiningThrottled { temp_c: f32 },
}

impl GpuDecision {
    pub fn label(&self) -> &'static str {
        match self {
            GpuDecision::MiningAllowed => "allowed",
            GpuDecision::MiningPaused(PauseReason::LlmInference) => "paused:llm",
            GpuDecision::MiningPaused(PauseReason::VramPressure) => "paused:vram",
            GpuDecision::MiningPaused(PauseReason::ThermalEmergency) => "paused:thermal",
            GpuDecision::MiningThrottled { .. } => "throttled",
        }
    }

    pub fn is_paused(&self) -> bool {
        matches!(self, GpuDecision::MiningPaused(_))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PauseReason {
    LlmInference,
    VramPressure,
    ThermalEmergency,
}

// ─── Config ──────────────────────────────────────────────────────────────────

/// Configuration thresholds for the GPU scheduler.
#[derive(Debug, Clone)]
pub struct GpuSchedulerConfig {
    /// VRAM to reserve for SNN brain (MB).
    pub vram_reserved_mb: u64,
    /// VRAM usage above which mining is paused (MB).
    /// Computed as: total - vram_reserved_mb
    pub vram_mining_ceiling_mb: u64,
    /// VRAM usage below which mining can resume after pause (MB).
    /// Default: 2048 (matches supervisor VRAM_CLEAR_THRESHOLD).
    pub vram_clear_threshold_mb: u64,
    /// GPU temp above which mining is throttled (°C).
    pub thermal_throttle_c: f32,
    /// GPU temp above which mining is killed (emergency) (°C).
    pub thermal_emergency_c: f32,
    /// Minimum time between state transitions (prevent thrashing).
    pub transition_cooldown: Duration,
}

impl Default for GpuSchedulerConfig {
    fn default() -> Self {
        // Default to 4GB reservation, will be overridden by CLI/env
        let reserved_mb: u64 = 4096;
        let total_mb: u64 = 16384; // Default assumption, will be computed at runtime
        Self {
            vram_reserved_mb: reserved_mb,
            vram_mining_ceiling_mb: total_mb.saturating_sub(reserved_mb),
            vram_clear_threshold_mb: 2048,
            thermal_throttle_c: 80.0,
            thermal_emergency_c: 90.0,
            transition_cooldown: Duration::from_secs(10),
        }
    }
}

// ─── Snapshot provider trait (for testability) ───────────────────────────────

/// Abstraction over GPU hardware reads. Allows mock injection in tests.
pub trait GpuSnapshotProvider: Send {
    fn snapshot(&self) -> Option<GpuSnapshot>;
}

/// Real NVML-backed provider.
struct NvmlProvider {
    nvml: Nvml,
}

impl GpuSnapshotProvider for NvmlProvider {
    fn snapshot(&self) -> Option<GpuSnapshot> {
        let device = self.nvml.device_by_index(0).ok()?;
        let mem = device.memory_info().ok()?;
        let temp = device.temperature(TemperatureSensor::Gpu).ok().unwrap_or(0) as f32;
        let power = device.power_usage().ok().unwrap_or(0) as f32 / 1000.0; // mW → W

        Some(GpuSnapshot {
            vram_used_mb: mem.used / (1024 * 1024),
            vram_total_mb: mem.total / (1024 * 1024),
            gpu_temp_c: temp,
            power_w: power,
        })
    }
}

/// Mock provider for tests.
pub struct MockProvider {
    snapshot: GpuSnapshot,
}

impl MockProvider {
    pub fn new(snapshot: GpuSnapshot) -> Self {
        Self { snapshot }
    }
}

impl GpuSnapshotProvider for MockProvider {
    fn snapshot(&self) -> Option<GpuSnapshot> {
        Some(self.snapshot.clone())
    }
}

// ─── GpuScheduler ────────────────────────────────────────────────────────────

pub struct GpuScheduler {
    config: GpuSchedulerConfig,
    provider: Box<dyn GpuSnapshotProvider>,
    current_decision: GpuDecision,
    last_transition: Instant,
    last_heartbeat: Instant,
    transition_count: u64,
    /// External signal: an LLM request is pending or active.
    llm_active: Arc<AtomicBool>,
    last_snapshot: Option<GpuSnapshot>,
}

impl GpuScheduler {
    /// Create a scheduler backed by real NVML hardware.
    pub fn new(mut config: GpuSchedulerConfig) -> Self {
        let provider: Box<dyn GpuSnapshotProvider> = match Nvml::init() {
            Ok(nvml) => {
                // Auto-compute ceiling from actual total VRAM.
                if let Ok(device) = nvml.device_by_index(0) {
                    if let Ok(mem) = device.memory_info() {
                        let total_mb = mem.total / (1024 * 1024);
                        config.vram_mining_ceiling_mb =
                            total_mb.saturating_sub(config.vram_reserved_mb);
                        eprintln!(
                            "[gpu-sched] NVML init OK — total={total_mb}MB reserved={}MB ceiling={}MB",
                            config.vram_reserved_mb, config.vram_mining_ceiling_mb
                        );
                    }
                }
                Box::new(NvmlProvider { nvml })
            }
            Err(e) => {
                eprintln!("[gpu-sched] NVML init failed ({e}) — scheduler will use defaults");
                Box::new(FallbackProvider)
            }
        };

        Self {
            config,
            provider,
            current_decision: GpuDecision::MiningAllowed,
            last_transition: Instant::now(),
            last_heartbeat: Instant::now(),
            transition_count: 0,
            llm_active: Arc::new(AtomicBool::new(false)),
            last_snapshot: None,
        }
    }

    /// Create a scheduler with a mock snapshot provider (for tests).
    pub fn new_mock(config: GpuSchedulerConfig, provider: Box<dyn GpuSnapshotProvider>) -> Self {
        Self {
            config,
            provider,
            current_decision: GpuDecision::MiningAllowed,
            last_transition: Instant::now(),
            last_heartbeat: Instant::now(),
            transition_count: 0,
            llm_active: Arc::new(AtomicBool::new(false)),
            last_snapshot: None,
        }
    }

    /// Get a handle to the `llm_active` flag for external callers.
    pub fn llm_active_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.llm_active)
    }

    /// Set the LLM active flag.
    pub fn set_llm_active(&self, active: bool) {
        self.llm_active.store(active, Ordering::SeqCst);
    }

    /// Poll GPU state and return the current scheduling decision.
    ///
    /// Returns `(decision, Option<GpuSchedulerEvent>)`. The event is `Some`
    /// on state transitions or periodic heartbeats (every 30s).
    pub fn poll(&mut self) -> (GpuDecision, Option<GpuSchedulerEvent>) {
        let snapshot = self.provider.snapshot().unwrap_or(GpuSnapshot {
            vram_used_mb: 0,
            vram_total_mb: self.config.vram_mining_ceiling_mb + 4096,
            gpu_temp_c: 0.0,
            power_w: 0.0,
        });
        self.last_snapshot = Some(snapshot.clone());

        let llm = self.llm_active.load(Ordering::SeqCst);

        // Evaluate priority ladder.
        let new_decision = if llm {
            GpuDecision::MiningPaused(PauseReason::LlmInference)
        } else if snapshot.gpu_temp_c >= self.config.thermal_emergency_c {
            GpuDecision::MiningPaused(PauseReason::ThermalEmergency)
        } else if snapshot.gpu_temp_c >= self.config.thermal_throttle_c {
            GpuDecision::MiningThrottled {
                temp_c: snapshot.gpu_temp_c,
            }
        } else if snapshot.vram_used_mb > self.config.vram_mining_ceiling_mb {
            GpuDecision::MiningPaused(PauseReason::VramPressure)
        } else {
            GpuDecision::MiningAllowed
        };

        // Enforce cooldown: if cooldown hasn't elapsed, hold current decision.
        let changed = new_decision != self.current_decision;
        if changed && self.last_transition.elapsed() >= self.config.transition_cooldown {
            self.current_decision = new_decision;
            self.last_transition = Instant::now();
            self.transition_count += 1;
            self.last_heartbeat = Instant::now();

            let event = self.make_event(&snapshot);
            return (self.current_decision.clone(), Some(event));
        }

        // Heartbeat every 30s regardless of transition.
        let event = if self.last_heartbeat.elapsed() >= Duration::from_secs(30) {
            self.last_heartbeat = Instant::now();
            Some(self.make_event(&snapshot))
        } else {
            None
        };

        (self.current_decision.clone(), event)
    }

    /// Get the latest snapshot.
    pub fn snapshot(&self) -> Option<&GpuSnapshot> {
        self.last_snapshot.as_ref()
    }

    /// VRAM free in MB.
    pub fn vram_free_mb(&self) -> u64 {
        self.last_snapshot
            .as_ref()
            .map(|s| s.vram_total_mb.saturating_sub(s.vram_used_mb))
            .unwrap_or(0)
    }

    /// Current decision (without polling).
    pub fn current_decision(&self) -> &GpuDecision {
        &self.current_decision
    }

    /// Poll NVML every 100ms until VRAM < threshold or timeout.
    /// Ported from supervisor/src/mining/supervisor.rs:388-444.
    pub fn verify_vram_freed(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(snapshot) = self.provider.snapshot() {
                if snapshot.vram_used_mb < self.config.vram_clear_threshold_mb {
                    return true;
                }
                eprintln!(
                    "[gpu-sched] vram-verify: used={}MB / threshold={}MB",
                    snapshot.vram_used_mb, self.config.vram_clear_threshold_mb
                );
            }
            if Instant::now() > deadline {
                eprintln!("[gpu-sched] vram-verify timeout");
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn make_event(&self, snapshot: &GpuSnapshot) -> GpuSchedulerEvent {
        GpuSchedulerEvent {
            decision: self.current_decision.label().to_string(),
            vram_used_mb: snapshot.vram_used_mb,
            vram_total_mb: snapshot.vram_total_mb,
            gpu_temp_c: snapshot.gpu_temp_c,
            power_w: snapshot.power_w,
            transition_count: self.transition_count,
        }
    }
}

/// Fallback provider when NVML is unavailable.
struct FallbackProvider;

impl GpuSnapshotProvider for FallbackProvider {
    fn snapshot(&self) -> Option<GpuSnapshot> {
        // Return safe defaults — mining always allowed.
        Some(GpuSnapshot {
            vram_used_mb: 0,
            vram_total_mb: 16384,
            gpu_temp_c: 50.0,
            power_w: 100.0,
        })
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> GpuSchedulerConfig {
        GpuSchedulerConfig {
            vram_mining_ceiling_mb: 12000,
            vram_clear_threshold_mb: 2000,
            vram_reserved_mb: 4096,
            thermal_throttle_c: 80.0,
            thermal_emergency_c: 90.0,
            transition_cooldown: Duration::from_millis(0), // no cooldown in tests
        }
    }

    fn mock_scheduler(snapshot: GpuSnapshot) -> GpuScheduler {
        GpuScheduler::new_mock(test_config(), Box::new(MockProvider::new(snapshot)))
    }

    #[test]
    fn mining_allowed_when_cool_and_low_vram() {
        let mut sched = mock_scheduler(GpuSnapshot {
            vram_used_mb: 2000,
            vram_total_mb: 16384,
            gpu_temp_c: 60.0,
            power_w: 200.0,
        });
        let (decision, _) = sched.poll();
        assert_eq!(decision, GpuDecision::MiningAllowed);
    }

    #[test]
    fn llm_active_pauses_mining() {
        let mut sched = mock_scheduler(GpuSnapshot {
            vram_used_mb: 2000,
            vram_total_mb: 16384,
            gpu_temp_c: 60.0,
            power_w: 200.0,
        });
        sched.set_llm_active(true);
        let (decision, _) = sched.poll();
        assert_eq!(
            decision,
            GpuDecision::MiningPaused(PauseReason::LlmInference)
        );
    }

    #[test]
    fn thermal_emergency_pauses_mining() {
        let mut sched = mock_scheduler(GpuSnapshot {
            vram_used_mb: 2000,
            vram_total_mb: 16384,
            gpu_temp_c: 92.0,
            power_w: 350.0,
        });
        let (decision, _) = sched.poll();
        assert_eq!(
            decision,
            GpuDecision::MiningPaused(PauseReason::ThermalEmergency)
        );
    }

    #[test]
    fn thermal_throttle_detected() {
        let mut sched = mock_scheduler(GpuSnapshot {
            vram_used_mb: 2000,
            vram_total_mb: 16384,
            gpu_temp_c: 85.0,
            power_w: 300.0,
        });
        let (decision, _) = sched.poll();
        assert!(matches!(decision, GpuDecision::MiningThrottled { temp_c } if temp_c == 85.0));
    }

    #[test]
    fn vram_pressure_pauses_mining() {
        let mut sched = mock_scheduler(GpuSnapshot {
            vram_used_mb: 13000, // > 12000 ceiling
            vram_total_mb: 16384,
            gpu_temp_c: 60.0,
            power_w: 200.0,
        });
        let (decision, _) = sched.poll();
        assert_eq!(
            decision,
            GpuDecision::MiningPaused(PauseReason::VramPressure)
        );
    }

    #[test]
    fn llm_takes_priority_over_thermal() {
        let mut sched = mock_scheduler(GpuSnapshot {
            vram_used_mb: 2000,
            vram_total_mb: 16384,
            gpu_temp_c: 95.0, // emergency temp
            power_w: 350.0,
        });
        sched.set_llm_active(true);
        let (decision, _) = sched.poll();
        // LLM is highest priority
        assert_eq!(
            decision,
            GpuDecision::MiningPaused(PauseReason::LlmInference)
        );
    }

    #[test]
    fn cooldown_prevents_rapid_transitions() {
        let config = GpuSchedulerConfig {
            transition_cooldown: Duration::from_secs(60), // 60s cooldown
            ..test_config()
        };
        let mut sched = GpuScheduler::new_mock(
            config,
            Box::new(MockProvider::new(GpuSnapshot {
                vram_used_mb: 2000,
                vram_total_mb: 16384,
                gpu_temp_c: 60.0,
                power_w: 200.0,
            })),
        );

        // First poll: MiningAllowed (initial state).
        let (d1, _) = sched.poll();
        assert_eq!(d1, GpuDecision::MiningAllowed);

        // Pretend LLM goes active — but cooldown prevents transition.
        sched.set_llm_active(true);
        let (d2, _) = sched.poll();
        // Cooldown holds the previous decision.
        assert_eq!(d2, GpuDecision::MiningAllowed);
    }

    #[test]
    fn transition_emits_event() {
        let mut sched = mock_scheduler(GpuSnapshot {
            vram_used_mb: 2000,
            vram_total_mb: 16384,
            gpu_temp_c: 60.0,
            power_w: 200.0,
        });

        // First poll — initial state is MiningAllowed, snapshot matches → no change.
        let (_, event1) = sched.poll();
        // No transition because initial state already matches.
        assert!(event1.is_none());

        // Now change to paused.
        sched.set_llm_active(true);
        let (_, event2) = sched.poll();
        assert!(event2.is_some());
        let e = event2.unwrap();
        assert_eq!(e.decision, "paused:llm");
        assert_eq!(e.transition_count, 1);
    }

    #[test]
    fn decision_labels_correct() {
        assert_eq!(GpuDecision::MiningAllowed.label(), "allowed");
        assert_eq!(
            GpuDecision::MiningPaused(PauseReason::LlmInference).label(),
            "paused:llm"
        );
        assert_eq!(
            GpuDecision::MiningPaused(PauseReason::VramPressure).label(),
            "paused:vram"
        );
        assert_eq!(
            GpuDecision::MiningPaused(PauseReason::ThermalEmergency).label(),
            "paused:thermal"
        );
        assert_eq!(
            GpuDecision::MiningThrottled { temp_c: 85.0 }.label(),
            "throttled"
        );
    }
}

//! GPU resource scheduler — VRAM-aware, temperature-governed mining arbitration.
//!
//! Priority ladder (highest → lowest):
//! 1. Thermal emergency (≥ 90°C) → pause mining
//! 2. Thermal throttle (≥ 80°C) → throttle mining
//! 3. VRAM pressure (used > ceiling) → pause mining
//! 4. Otherwise → mining allowed

use std::time::{Duration, Instant};

use nvml_wrapper::Nvml;
use nvml_wrapper::enum_wrappers::device::TemperatureSensor;

use mining_telemetry_core::GpuSchedulerEvent;

// ─── Types ───────────────────────────────────────────────────────────────────

/// Snapshot of GPU state as seen by the scheduler.
#[derive(Debug, Clone)]
pub struct GpuSnapshot {
    pub vram_used_mb: u64,
    pub vram_total_mb: u64,
    /// GPU die temperature (°C). `None` = sensor not read this tick. Never
    /// zero-filled or fabricated (RM-629 audit, issue #24).
    pub gpu_temp_c: Option<f32>,
    /// Board power (W, from NVML milliwatts). `None` = not read this tick.
    pub power_w: Option<f32>,
}

/// NVML reports board power in **milliwatts**; the contract column is watts.
pub fn nvml_milliwatts_to_watts(mw: u32) -> f32 {
    mw as f32 / 1000.0
}

/// Scheduler decision.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq)]
pub enum GpuDecision {
    /// Mining may proceed.
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
    VramPressure,
    ThermalEmergency,
}

// ─── Config ──────────────────────────────────────────────────────────────────

/// Configuration thresholds for the GPU scheduler.
#[derive(Debug, Clone)]
pub struct GpuSchedulerConfig {
    /// VRAM reserved (MB).
    pub vram_reserved_mb: u64,
    /// VRAM usage above which mining is paused (MB).
    pub vram_mining_ceiling_mb: u64,
    /// GPU temp above which mining is throttled (°C).
    pub thermal_throttle_c: f32,
    /// GPU temp above which mining is killed (emergency) (°C).
    pub thermal_emergency_c: f32,
    /// Minimum time between state transitions (prevent thrashing).
    pub transition_cooldown: Duration,
}

impl Default for GpuSchedulerConfig {
    fn default() -> Self {
        let reserved_mb: u64 = 4096;
        let total_mb: u64 = 16384;
        Self {
            vram_reserved_mb: reserved_mb,
            vram_mining_ceiling_mb: total_mb.saturating_sub(reserved_mb),
            thermal_throttle_c: 80.0,
            thermal_emergency_c: 90.0,
            transition_cooldown: Duration::from_secs(10),
        }
    }
}

// ─── Snapshot provider trait ─────────────────────────────────────────────────

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
        // NVML temperature is whole °C; power is mW. A failed read stays
        // `None` — a 0 °C / 0 W reading would be fabricated physiology.
        let temp = device
            .temperature(TemperatureSensor::Gpu)
            .ok()
            .map(|c| c as f32);
        let power = device.power_usage().ok().map(nvml_milliwatts_to_watts);

        Some(GpuSnapshot {
            vram_used_mb: mem.used / (1024 * 1024),
            vram_total_mb: mem.total / (1024 * 1024),
            gpu_temp_c: temp,
            power_w: power,
        })
    }
}

/// Mock provider for tests.
#[cfg(test)]
pub struct MockProvider {
    snapshot: GpuSnapshot,
}

#[cfg(test)]
impl MockProvider {
    pub fn new(snapshot: GpuSnapshot) -> Self {
        Self { snapshot }
    }
}

#[cfg(test)]
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
    last_snapshot: Option<GpuSnapshot>,
}

impl GpuScheduler {
    /// Create a scheduler backed by real NVML hardware.
    pub fn new(mut config: GpuSchedulerConfig) -> Self {
        let provider: Box<dyn GpuSnapshotProvider> = match Nvml::init() {
            Ok(nvml) => {
                if let Ok(device) = nvml.device_by_index(0)
                    && let Ok(mem) = device.memory_info()
                {
                    let total_mb = mem.total / (1024 * 1024);
                    config.vram_mining_ceiling_mb =
                        total_mb.saturating_sub(config.vram_reserved_mb);
                    eprintln!(
                        "[gpu-sched] NVML init OK — total={total_mb}MB reserved={}MB ceiling={}MB",
                        config.vram_reserved_mb, config.vram_mining_ceiling_mb
                    );
                }
                Box::new(NvmlProvider { nvml })
            }
            Err(e) => {
                eprintln!("[gpu-sched] NVML init failed ({e}) — scheduler will use defaults");
                Box::new(FallbackProvider)
            }
        };

        let last_transition = Instant::now()
            .checked_sub(config.transition_cooldown)
            .unwrap_or_else(Instant::now);

        Self {
            config,
            provider,
            current_decision: GpuDecision::MiningAllowed,
            last_transition,
            last_heartbeat: Instant::now(),
            transition_count: 0,
            last_snapshot: None,
        }
    }

    /// Create a scheduler with a mock snapshot provider (for tests).
    #[cfg(test)]
    pub fn new_mock(config: GpuSchedulerConfig, provider: Box<dyn GpuSnapshotProvider>) -> Self {
        let last_transition = Instant::now()
            .checked_sub(config.transition_cooldown)
            .unwrap_or_else(Instant::now);

        Self {
            config,
            provider,
            current_decision: GpuDecision::MiningAllowed,
            last_transition,
            last_heartbeat: Instant::now(),
            transition_count: 0,
            last_snapshot: None,
        }
    }

    /// Poll GPU state and return the current scheduling decision.
    pub fn poll(&mut self) -> (GpuDecision, Option<GpuSchedulerEvent>) {
        let snapshot = self.provider.snapshot().unwrap_or(GpuSnapshot {
            vram_used_mb: 0,
            vram_total_mb: self.config.vram_mining_ceiling_mb + 4096,
            gpu_temp_c: None,
            power_w: None,
        });
        self.last_snapshot = Some(snapshot.clone());

        // Priority ladder. An unread temperature skips the thermal rungs —
        // identical to the previous 0 °C behaviour for the safety decision,
        // but no longer recorded as a real reading. Whether unknown GPU
        // temperature should fail closed is an open safety decision
        // (docs/contract-audit-rm-629.md, U-6).
        let temp = snapshot.gpu_temp_c.filter(|t| t.is_finite());
        let new_decision = if temp.is_some_and(|t| t >= self.config.thermal_emergency_c) {
            GpuDecision::MiningPaused(PauseReason::ThermalEmergency)
        } else if let Some(temp_c) = temp.filter(|t| *t >= self.config.thermal_throttle_c) {
            GpuDecision::MiningThrottled { temp_c }
        } else if snapshot.vram_used_mb > self.config.vram_mining_ceiling_mb {
            GpuDecision::MiningPaused(PauseReason::VramPressure)
        } else {
            GpuDecision::MiningAllowed
        };

        let changed = new_decision != self.current_decision;
        let cooldown_met = self.last_transition.elapsed() >= self.config.transition_cooldown;

        if changed && (cooldown_met || new_decision.is_paused()) {
            self.current_decision = new_decision;
            self.last_transition = Instant::now();
            self.transition_count += 1;
            self.last_heartbeat = Instant::now();

            let event = self.make_event(&snapshot);
            return (self.current_decision.clone(), Some(event));
        }

        let event = if self.last_heartbeat.elapsed() >= Duration::from_secs(30) {
            self.last_heartbeat = Instant::now();
            Some(self.make_event(&snapshot))
        } else {
            None
        };

        (self.current_decision.clone(), event)
    }

    fn make_event(&self, snapshot: &GpuSnapshot) -> GpuSchedulerEvent {
        GpuSchedulerEvent {
            decision: self.current_decision.label().to_string(),
            vram_used_mb: snapshot.vram_used_mb,
            vram_total_mb: snapshot.vram_total_mb,
            // Non-finite readings are dropped, not serialized (serde_json
            // would otherwise write `null` and hide the fault).
            gpu_temp_c: snapshot.gpu_temp_c.filter(|t| t.is_finite()),
            power_w: snapshot.power_w.filter(|p| p.is_finite()),
            transition_count: self.transition_count,
        }
    }
}

/// Fallback provider when NVML is unavailable.
///
/// Reports no sensor values: it previously fabricated 50 °C / 100 W, which
/// would have entered the JSONL corpus as real GPU physiology.
struct FallbackProvider;

impl GpuSnapshotProvider for FallbackProvider {
    fn snapshot(&self) -> Option<GpuSnapshot> {
        Some(GpuSnapshot {
            vram_used_mb: 0,
            vram_total_mb: 16384,
            gpu_temp_c: None,
            power_w: None,
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
            vram_reserved_mb: 4096,
            thermal_throttle_c: 80.0,
            thermal_emergency_c: 90.0,
            transition_cooldown: Duration::from_millis(0),
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
            gpu_temp_c: Some(60.0),
            power_w: Some(200.0),
        });
        let (decision, _) = sched.poll();
        assert_eq!(decision, GpuDecision::MiningAllowed);
    }

    #[test]
    fn thermal_emergency_pauses_mining() {
        let mut sched = mock_scheduler(GpuSnapshot {
            vram_used_mb: 2000,
            vram_total_mb: 16384,
            gpu_temp_c: Some(92.0),
            power_w: Some(350.0),
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
            gpu_temp_c: Some(85.0),
            power_w: Some(300.0),
        });
        let (decision, _) = sched.poll();
        assert!(matches!(decision, GpuDecision::MiningThrottled { temp_c } if temp_c == 85.0));
    }

    #[test]
    fn vram_pressure_pauses_mining() {
        let mut sched = mock_scheduler(GpuSnapshot {
            vram_used_mb: 13000,
            vram_total_mb: 16384,
            gpu_temp_c: Some(60.0),
            power_w: Some(200.0),
        });
        let (decision, _) = sched.poll();
        assert_eq!(
            decision,
            GpuDecision::MiningPaused(PauseReason::VramPressure)
        );
    }

    #[test]
    fn decision_labels_correct() {
        assert_eq!(GpuDecision::MiningAllowed.label(), "allowed");
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

    #[test]
    fn nvml_power_is_converted_from_milliwatts() {
        assert_eq!(nvml_milliwatts_to_watts(252_500), 252.5);
        assert_eq!(nvml_milliwatts_to_watts(0), 0.0);
    }

    #[test]
    fn unread_sensors_are_not_fabricated_in_events() {
        let mut sched = GpuScheduler::new_mock(test_config(), Box::new(FallbackProvider));
        // Force a heartbeat event.
        sched.last_heartbeat = Instant::now() - Duration::from_secs(31);
        let (decision, event) = sched.poll();
        assert_eq!(decision, GpuDecision::MiningAllowed);
        let event = event.expect("heartbeat event");
        assert_eq!(event.gpu_temp_c, None);
        assert_eq!(event.power_w, None);
        let env = mining_telemetry_core::envelope_from_gpu_sched("collector", &event);
        let line = env.to_json_line().unwrap();
        assert!(!line.contains("gpu_temp_c"), "{line}");
        assert!(!line.contains("power_w"), "{line}");
    }

    #[test]
    fn non_finite_temperature_never_counts_as_cool_reading() {
        let mut sched = mock_scheduler(GpuSnapshot {
            vram_used_mb: 2000,
            vram_total_mb: 16384,
            gpu_temp_c: Some(f32::NAN),
            power_w: Some(200.0),
        });
        let (decision, _) = sched.poll();
        // NaN skips the thermal rungs (same as unread); VRAM rung still applies.
        assert_eq!(decision, GpuDecision::MiningAllowed);
    }

    #[test]
    fn thermal_ladder_still_fires_on_real_reading() {
        let mut sched = mock_scheduler(GpuSnapshot {
            vram_used_mb: 2000,
            vram_total_mb: 16384,
            gpu_temp_c: Some(90.0),
            power_w: None,
        });
        let (decision, _) = sched.poll();
        assert_eq!(
            decision,
            GpuDecision::MiningPaused(PauseReason::ThermalEmergency)
        );
    }
}

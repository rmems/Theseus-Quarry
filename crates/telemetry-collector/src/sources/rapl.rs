//! Powercap RAPL package energy counter → `HostHw.cpu_package_power_w`.
//!
//! `energy_uj` is a cumulative **microjoule** counter that wraps at
//! `max_energy_range_uj` (not `u64::MAX`). Power is the delta between two
//! samples divided by elapsed time, so:
//!
//! - the first sample after start has no delta and emits **nothing** (it used
//!   to emit `0.0 W`, a fabricated reading);
//! - a counter that goes backwards is unwrapped with `max_energy_range_uj`
//!   when that file is readable, otherwise treated as a reset and dropped;
//! - a non-positive `dt` or an implausible result is dropped, never zeroed.
//!
//! On AMD Zen the kernel exposes package energy through the same
//! `intel-rapl:0` powercap zone. The zone `name` is checked to be a package
//! domain; whether this is the right zone on the workstation is unverified
//! without the live host (audit U-4).

use super::TelemetryRecord;
use mining_telemetry_core::host_hw;
use std::sync::Mutex;
use std::time::Instant;

const RAPL_ZONE: &str = "/sys/class/powercap/intel-rapl:0";

/// Upper bound for a plausible desktop CPU package (W). Above this the delta
/// is a counter fault (missed wrap, reset), not physiology.
const MAX_PLAUSIBLE_W: f64 = 2000.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct EnergySample {
    pub energy_uj: u64,
    pub at: Instant,
}

/// Energy consumed between two counter values (µJ). A backwards counter is a
/// wrap only if we know the range and the previous value was inside it;
/// otherwise it is a reset → `None`.
fn energy_delta_uj(prev_uj: u64, now_uj: u64, max_energy_range_uj: Option<u64>) -> Option<u64> {
    if now_uj >= prev_uj {
        return Some(now_uj - prev_uj);
    }
    let range = max_energy_range_uj.filter(|&r| prev_uj <= r)?;
    (range - prev_uj).checked_add(now_uj)
}

/// Pure delta → watts. `None` means "no trustworthy reading this tick".
pub(crate) fn power_from_delta(
    prev: Option<EnergySample>,
    now: EnergySample,
    max_energy_range_uj: Option<u64>,
) -> Option<f64> {
    let prev = prev?;
    let dt = now.at.checked_duration_since(prev.at)?.as_secs_f64();
    if dt <= 0.0 {
        return None;
    }
    let delta_uj = energy_delta_uj(prev.energy_uj, now.energy_uj, max_energy_range_uj)?;
    let watts = (delta_uj as f64 / 1_000_000.0) / dt;
    (watts.is_finite() && (0.0..=MAX_PLAUSIBLE_W).contains(&watts)).then_some(watts)
}

fn read_u64(path: &str) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

pub struct RaplState {
    last: Mutex<Option<EnergySample>>,
}

impl RaplState {
    pub fn new() -> Self {
        Self {
            last: Mutex::new(None),
        }
    }

    pub fn poll(&self) -> TelemetryRecord {
        TelemetryRecord {
            source: "rapl",
            envelope: self.try_poll(),
        }
    }

    fn try_poll(&self) -> Option<mining_telemetry_core::TelemetryEnvelope> {
        if let Ok(name) = std::fs::read_to_string(format!("{RAPL_ZONE}/name"))
            && !name.trim().starts_with("package")
        {
            return None;
        }
        let energy_uj = read_u64(&format!("{RAPL_ZONE}/energy_uj"))?;
        let max_range = read_u64(&format!("{RAPL_ZONE}/max_energy_range_uj"));
        let now = EnergySample {
            energy_uj,
            at: Instant::now(),
        };
        let prev = {
            let mut last = self.last.lock().unwrap_or_else(|p| p.into_inner());
            last.replace(now)
        };
        let power_w = power_from_delta(prev, now, max_range)?;
        Some(host_hw(
            "collector",
            "rapl_telemetry",
            None,
            None,
            None,
            Some(power_w),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn pair(e0: u64, e1: u64, secs: u64) -> (EnergySample, EnergySample) {
        let t0 = Instant::now();
        (
            EnergySample {
                energy_uj: e0,
                at: t0,
            },
            EnergySample {
                energy_uj: e1,
                at: t0 + Duration::from_secs(secs),
            },
        )
    }

    #[test]
    fn first_sample_emits_nothing_not_zero() {
        let (_, now) = pair(0, 1_000_000, 1);
        assert_eq!(power_from_delta(None, now, Some(u64::MAX)), None);
    }

    #[test]
    fn microjoules_per_second_to_watts() {
        let (a, b) = pair(10_000_000, 135_000_000, 1);
        assert_eq!(power_from_delta(Some(a), b, None), Some(125.0));
    }

    #[test]
    fn wrap_uses_max_energy_range_not_u64_max() {
        // range 262_143_328_850 uJ (typical Zen value); wrap by 100 J total.
        let range = 262_143_328_850;
        let (a, b) = pair(range - 40_000_000, 60_000_000, 1);
        assert_eq!(power_from_delta(Some(a), b, Some(range)), Some(100.0));
    }

    #[test]
    fn backwards_counter_without_range_is_reset_dropped() {
        let (a, b) = pair(500_000_000, 1_000, 1);
        assert_eq!(power_from_delta(Some(a), b, None), None);
    }

    #[test]
    fn zero_dt_dropped() {
        let (a, b) = pair(1, 2, 0);
        assert_eq!(power_from_delta(Some(a), b, None), None);
    }

    #[test]
    fn implausible_power_dropped() {
        // 10 kJ in 1 s = 10 kW: counter fault.
        let (a, b) = pair(0, 10_000_000_000, 1);
        assert_eq!(power_from_delta(Some(a), b, None), None);
    }
}

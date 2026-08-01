use super::TelemetryRecord;
use mining_telemetry_core::host_hw;
use std::sync::Mutex;
use std::time::Instant;

const RAPL_PATH: &str = "/sys/class/powercap/intel-rapl:0/energy_uj";

pub struct RaplState {
    last_energy_uj: Mutex<Option<u64>>,
    last_time: Mutex<Option<Instant>>,
}

impl RaplState {
    pub fn new() -> Self {
        Self {
            last_energy_uj: Mutex::new(None),
            last_time: Mutex::new(None),
        }
    }

    pub fn poll(&self) -> TelemetryRecord {
        TelemetryRecord {
            source: "rapl",
            envelope: self.try_poll(),
        }
    }

    fn try_poll(&self) -> Option<mining_telemetry_core::TelemetryEnvelope> {
        let raw = std::fs::read_to_string(RAPL_PATH).ok()?;
        let energy: u64 = raw.trim().parse().ok()?;
        let now = Instant::now();

        let mut last_e = self.last_energy_uj.lock().unwrap();
        let mut last_t = self.last_time.lock().unwrap();

        let power_w = if let (Some(prev_e), Some(prev_t)) = (*last_e, *last_t) {
            let dt = now.duration_since(prev_t).as_secs_f64();
            if dt > 0.0 {
                let delta = if energy >= prev_e {
                    energy - prev_e
                } else {
                    u64::MAX - prev_e + energy
                };
                (delta as f64 / 1_000_000.0) / dt
            } else {
                0.0
            }
        } else {
            0.0
        };

        *last_e = Some(energy);
        *last_t = Some(now);

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

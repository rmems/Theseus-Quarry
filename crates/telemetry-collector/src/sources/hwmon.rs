use mining_telemetry_core::host_hw;
use std::path::PathBuf;
use super::TelemetryRecord;

pub fn poll() -> TelemetryRecord {
    TelemetryRecord {
        source: "hwmon",
        envelope: try_poll(),
    }
}

fn find_hwmon(name: &str) -> Option<PathBuf> {
    let base = std::path::Path::new("/sys/class/hwmon");
    let entries = std::fs::read_dir(base).ok()?;
    for entry in entries.flatten() {
        let name_path = entry.path().join("name");
        if let Ok(n) = std::fs::read_to_string(&name_path) {
            if n.trim() == name {
                return Some(entry.path());
            }
        }
    }
    None
}

fn read_temp(hwmon: &std::path::Path, sensor: &str) -> Option<f64> {
    std::fs::read_to_string(hwmon.join(sensor))
        .ok()
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|v| v / 1000.0)
}

fn try_poll() -> Option<mining_telemetry_core::TelemetryEnvelope> {
    let k10 = find_hwmon("k10temp")?;
    Some(host_hw(
        "collector",
        "hwmon_telemetry",
        read_temp(&k10, "temp1_input"),
        read_temp(&k10, "temp3_input"),
        read_temp(&k10, "temp4_input"),
        None,
    ))
}

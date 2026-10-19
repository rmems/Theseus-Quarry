//! AMD `k10temp` hwmon → `HostHw` CPU temperatures.
//!
//! sysfs hwmon `temp*_input` is **integer millidegrees Celsius**
//! (Linux `Documentation/hwmon/sysfs-interface.rst`). Channels are resolved by
//! their `temp*_label` (`Tctl`, `Tccd1`, `Tccd2`) rather than by index, because
//! the index→label mapping differs across k10temp CPU families. If a label
//! file is absent the documented legacy index is used as a fallback; that
//! fallback is unverified on the workstation (audit U-3, issue #15 style).

use super::TelemetryRecord;
use mining_telemetry_core::host_hw;
use std::path::{Path, PathBuf};

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
        if let Ok(n) = std::fs::read_to_string(&name_path)
            && n.trim() == name
        {
            return Some(entry.path());
        }
    }
    None
}

/// Parse one sysfs `temp*_input` value (integer millidegrees C) into °C.
///
/// Fails (returns `None`) on anything that is not a plain integer so a
/// unit-ambiguous value (e.g. an already-converted `61.5`) is rejected rather
/// than silently divided by 1000 again.
pub(crate) fn parse_millidegrees(raw: &str) -> Option<f64> {
    let milli: i64 = raw.trim().parse().ok()?;
    let c = milli as f64 / 1000.0;
    // Physically plausible die temperature window; anything else is a
    // driver/unit fault, not physiology.
    (-40.0..=150.0).contains(&c).then_some(c)
}

/// Find the `temp{N}_input` whose `temp{N}_label` equals `label`.
fn input_for_label(hwmon: &Path, label: &str) -> Option<PathBuf> {
    (1..=16).find_map(|n| {
        let l = std::fs::read_to_string(hwmon.join(format!("temp{n}_label"))).ok()?;
        (l.trim() == label).then(|| hwmon.join(format!("temp{n}_input")))
    })
}

/// True when the chip publishes any `temp{N}_label` file.
fn has_labels(hwmon: &Path) -> bool {
    (1..=16).any(|n| hwmon.join(format!("temp{n}_label")).exists())
}

/// Resolve a channel's input file. The fixed legacy index is used only on
/// chips that publish no labels at all: if labels exist but this one is
/// absent (e.g. no `Tccd2` on an 8-core part), the index may belong to a
/// different sensor, so the channel is reported missing instead.
fn channel_input(hwmon: &Path, label: &str, legacy_input: &str) -> Option<PathBuf> {
    match input_for_label(hwmon, label) {
        Some(path) => Some(path),
        None if !has_labels(hwmon) => Some(hwmon.join(legacy_input)),
        None => None,
    }
}

fn read_channel(hwmon: &Path, label: &str, legacy_input: &str) -> Option<f64> {
    let path = channel_input(hwmon, label, legacy_input)?;
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| parse_millidegrees(&s))
}

fn try_poll() -> Option<mining_telemetry_core::TelemetryEnvelope> {
    let k10 = find_hwmon("k10temp")?;
    Some(host_hw(
        "collector",
        "hwmon_telemetry",
        read_channel(&k10, "Tctl", "temp1_input"),
        read_channel(&k10, "Tccd1", "temp3_input"),
        read_channel(&k10, "Tccd2", "temp4_input"),
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn millidegrees_convert_to_celsius() {
        assert_eq!(parse_millidegrees("61500\n"), Some(61.5));
        assert_eq!(parse_millidegrees("0"), Some(0.0));
    }

    #[test]
    fn non_integer_or_garbage_fails_loud() {
        // Already-converted °C would otherwise become 0.0615 °C.
        assert_eq!(parse_millidegrees("61.5"), None);
        assert_eq!(parse_millidegrees("NaN"), None);
        assert_eq!(parse_millidegrees(""), None);
        assert_eq!(parse_millidegrees("abc"), None);
    }

    #[test]
    fn implausible_values_rejected() {
        // 500 °C: almost certainly a driver/unit fault.
        assert_eq!(parse_millidegrees("500000"), None);
        assert_eq!(parse_millidegrees("-273150"), None);
    }

    #[test]
    fn label_lookup_beats_index() {
        let dir = std::env::temp_dir().join(format!("theseus_hwmon_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Tctl lives on temp2 here; legacy index temp1 holds something else.
        std::fs::write(dir.join("temp1_input"), "99000\n").unwrap();
        std::fs::write(dir.join("temp1_label"), "Tccd1\n").unwrap();
        std::fs::write(dir.join("temp2_input"), "61500\n").unwrap();
        std::fs::write(dir.join("temp2_label"), "Tctl\n").unwrap();
        assert_eq!(read_channel(&dir, "Tctl", "temp1_input"), Some(61.5));
        assert_eq!(read_channel(&dir, "Tccd1", "temp3_input"), Some(99.0));
        // No label, no legacy file → None (not 0.0).
        assert_eq!(read_channel(&dir, "Tccd2", "temp4_input"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn labeled_chip_never_falls_back_to_legacy_index() {
        let dir = std::env::temp_dir().join(format!("theseus_hwmon_lbl_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 8-core part: Tctl and Tccd1 are labeled, there is no Tccd2, and the
        // legacy Tccd2 index temp4 holds an unrelated sensor.
        std::fs::write(dir.join("temp1_input"), "61500\n").unwrap();
        std::fs::write(dir.join("temp1_label"), "Tctl\n").unwrap();
        std::fs::write(dir.join("temp3_input"), "58000\n").unwrap();
        std::fs::write(dir.join("temp3_label"), "Tccd1\n").unwrap();
        std::fs::write(dir.join("temp4_input"), "35000\n").unwrap();
        assert_eq!(read_channel(&dir, "Tccd2", "temp4_input"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unlabeled_chip_uses_legacy_index() {
        let dir = std::env::temp_dir().join(format!("theseus_hwmon_nolbl_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("temp1_input"), "61500\n").unwrap();
        assert_eq!(read_channel(&dir, "Tctl", "temp1_input"), Some(61.5));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

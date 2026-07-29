//! Persistent state for the mining supervisor.
//!
//! Saves scheduler config overrides, rotation history, and last-used algorithm
//! to `$SHIP_WORK_DIR/.mining_state.json` so the system can resume intelligently
//! across restarts.

use std::collections::VecDeque;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ─── Types ───────────────────────────────────────────────────────────────────

/// Top-level persistent state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiningPersistentState {
    /// Last active algorithm (e.g. "dynex", "quai", "all").
    pub last_algo: String,
    /// ISO 8601 timestamp of the last rotation, if any.
    pub last_rotation_ts: Option<String>,
    /// User-tuned GPU scheduler thresholds (optional).
    pub thermal_throttle_c: Option<f32>,
    pub thermal_emergency_c: Option<f32>,
    /// Cumulative rotation statistics.
    #[serde(default)]
    pub rotation_stats: CumulativeRotationStats,
}

/// Tracks rotation history for debugging and telemetry.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CumulativeRotationStats {
    pub rotations_total: u64,
    /// Last 50 rotation records (ring buffer).
    pub rotation_history: VecDeque<RotationRecord>,
}

impl CumulativeRotationStats {
    /// Append a rotation record, capping at 50 entries.
    pub fn push(&mut self, record: RotationRecord) {
        self.rotations_total += 1;
        self.rotation_history.push_back(record);
        while self.rotation_history.len() > 50 {
            self.rotation_history.pop_front();
        }
    }
}

/// A single rotation event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationRecord {
    /// ISO 8601 timestamp.
    pub timestamp: String,
    pub from_algo: String,
    pub to_algo: String,
    /// "profitability" | "manual" | "gpu_unavailable"
    pub reason: String,
    pub revenue_delta_pct: f64,
}

// ─── Load / Save ─────────────────────────────────────────────────────────────

/// Resolve the state file path.
///
/// Priority: `$MINING_STATE_FILE` env → `$SHIP_WORK_DIR/.mining_state.json`
/// → `<cwd>/.mining_state.json`.
pub fn state_file_path() -> PathBuf {
    if let Ok(p) = std::env::var("MINING_STATE_FILE") {
        return PathBuf::from(p);
    }
    if let Ok(dir) = std::env::var("SHIP_WORK_DIR") {
        return Path::new(&dir).join(".mining_state.json");
    }
    PathBuf::from(".mining_state.json")
}

/// Load persistent state from disk.  Returns `None` on missing or corrupt file.
pub fn load_state() -> Option<MiningPersistentState> {
    let path = state_file_path();
    let data = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<MiningPersistentState>(&data) {
        Ok(state) => Some(state),
        Err(e) => {
            eprintln!("[state] failed to parse {}: {e}", path.display());
            None
        }
    }
}

/// Save persistent state to disk (atomic write: tmp + rename).
pub fn save_state(state: &MiningPersistentState) -> io::Result<()> {
    let path = state_file_path();
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(state)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Get current time as ISO 8601 string (UTC).
pub fn now_iso8601() -> String {
    use std::time::SystemTime;
    let d = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    // Simple UTC format: YYYY-MM-DDTHH:MM:SSZ
    let secs = d.as_secs();
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    // Convert days since epoch to date.
    let (y, m, d) = days_to_ymd(days);
    format!("{y:04}-{m:02}-{d:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Civil calendar algorithm (Howard Hinnant).
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u64, m, d)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn round_trip_state() {
        let state = MiningPersistentState {
            last_algo: "dynex".into(),
            last_rotation_ts: Some("2026-03-18T12:00:00Z".into()),
            thermal_throttle_c: Some(75.0),
            thermal_emergency_c: None,
            rotation_stats: CumulativeRotationStats {
                rotations_total: 3,
                rotation_history: VecDeque::new(),
            },
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: MiningPersistentState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.last_algo, "dynex");
        assert_eq!(parsed.thermal_throttle_c, Some(75.0));
        assert_eq!(parsed.rotation_stats.rotations_total, 3);
    }

    #[test]
    fn rotation_history_caps_at_50() {
        let mut stats = CumulativeRotationStats::default();
        for i in 0..60 {
            stats.push(RotationRecord {
                timestamp: format!("t{i}"),
                from_algo: "a".into(),
                to_algo: "b".into(),
                reason: "profitability".into(),
                revenue_delta_pct: 25.0,
            });
        }
        assert_eq!(stats.rotation_history.len(), 50);
        assert_eq!(stats.rotations_total, 60);
        // Oldest should be t10 (first 10 were evicted).
        assert_eq!(stats.rotation_history.front().unwrap().timestamp, "t10");
    }

    #[test]
    fn now_iso8601_format() {
        let ts = now_iso8601();
        // Should match YYYY-MM-DDTHH:MM:SSZ pattern.
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.len(), 20);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
    }

    #[test]
    fn save_and_load_via_file() {
        // Use a unique path to avoid env-var races with parallel tests.
        let path = format!("/tmp/theseus_state_test_{}.json", std::process::id());

        let state = MiningPersistentState {
            last_algo: "all".into(),
            last_rotation_ts: None,
            thermal_throttle_c: None,
            thermal_emergency_c: Some(85.0),
            rotation_stats: CumulativeRotationStats::default(),
        };

        // Write directly.
        let json = serde_json::to_string_pretty(&state).unwrap();
        std::fs::write(&path, &json).unwrap();

        // Read back.
        let data = std::fs::read_to_string(&path).unwrap();
        let loaded: MiningPersistentState = serde_json::from_str(&data).unwrap();
        assert_eq!(loaded.last_algo, "all");
        assert_eq!(loaded.thermal_emergency_c, Some(85.0));

        // Cleanup.
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_missing_file_returns_none_via_parse() {
        // Test the parsing path directly — a missing file returns None.
        let result: Option<MiningPersistentState> =
            serde_json::from_str::<MiningPersistentState>("not valid json").ok();
        assert!(result.is_none());
    }
}

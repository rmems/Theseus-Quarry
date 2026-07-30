mod schema;

pub use schema::{
    JsonlSink, NodeHealthInput, RecordKind, SCHEMA_VERSION, TelemetryEnvelope, TelemetryPayload,
    detect_host, envelope_from_gpu_sched, envelope_from_rotation, envelopes_from_mining_stats,
    envelopes_from_mining_telemetry, envelopes_from_wire, host_hw, node_health,
};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum MinerBrand {
    BzMiner,
    DynexSolver,
    Xmrig,
    Rigel,
    QubicCore,
    SRBMiner,
    Hellminer,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum CoinType {
    Dynex,
    Quai,
    Qubic,
    Kaspa,
    Monero,
    Verus,
    Ocean,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct GpuSchedulerEvent {
    pub decision: String,
    pub vram_used_mb: u64,
    pub vram_total_mb: u64,
    pub gpu_temp_c: f32,
    pub power_w: f32,
    pub transition_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct KaspaStats {
    pub hashrate_mh_s: f64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub uptime_seconds: u64,
    pub is_active: bool,
}

impl Default for KaspaStats {
    fn default() -> Self {
        Self {
            hashrate_mh_s: 0.0,
            shares_accepted: 0,
            shares_rejected: 0,
            uptime_seconds: 0,
            is_active: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RotationEvent {
    pub kind: String,
    pub from_algo: Option<String>,
    pub to_algo: Option<String>,
    pub revenues: Vec<(String, f64, f64)>,
    pub market_age_secs: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DynexStats {
    pub hashrate_mh_s: f64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub uptime_seconds: u64,
    pub is_active: bool,
    pub solver_steps: Option<u64>,
    pub solver_chips: Option<u32>,
    pub complexity: Option<u64>,
    pub joules_per_step: Option<f32>,
}

impl Default for DynexStats {
    fn default() -> Self {
        Self {
            hashrate_mh_s: 0.0,
            shares_accepted: 0,
            shares_rejected: 0,
            uptime_seconds: 0,
            is_active: false,
            solver_steps: None,
            solver_chips: None,
            complexity: None,
            joules_per_step: None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MiningStats {
    pub dynex: DynexStats,
    pub quai: QuaiStats,
    pub qubic: QubicStats,
    pub monero: MoneroStats,
    pub verus: VerusStats,
    pub kaspa: KaspaStats,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl Default for MiningStats {
    fn default() -> Self {
        Self {
            dynex: DynexStats::default(),
            quai: QuaiStats::default(),
            qubic: QubicStats::default(),
            monero: MoneroStats::default(),
            verus: VerusStats::default(),
            kaspa: KaspaStats::default(),
            timestamp: chrono::Utc::now(),
        }
    }
}

impl MiningStats {
    /// Parse one stdout line into cumulative stats.
    ///
    /// Returns `true` when a mining field (hashrate or share counter) changed.
    /// Callers should emit MinerPerf only on `true` so chatty banners do not
    /// re-sample a sticky `is_active` snapshot with a fresh timestamp.
    pub fn update_from_line(&mut self, _brand: MinerBrand, line: &str) -> bool {
        let mut updated = false;
        let lower = line.to_lowercase();

        match _brand {
            MinerBrand::DynexSolver => {
                if let Some(hr) = extract_hashrate(&lower, &["kh/s", "mh/s", "gh/s"]) {
                    self.dynex.hashrate_mh_s = hr;
                    self.dynex.is_active = true;
                    updated = true;
                }
                if let Some(n) = extract_count_after(line, "accepted") {
                    self.dynex.shares_accepted = n;
                    updated = true;
                }
                if let Some(n) = extract_count_after(line, "rejected") {
                    self.dynex.shares_rejected = n;
                    updated = true;
                }
            }
            MinerBrand::BzMiner => {
                if let Some(hr) = extract_hashrate(&lower, &["mhs", "ghs", "ths"]) {
                    self.kaspa.hashrate_mh_s = hr;
                    self.kaspa.is_active = true;
                    updated = true;
                }
                // Word-boundary match: plain "acc" would false-hit "acceleration".
                if let Some(n) = extract_delimited_share(&lower, "acc") {
                    self.kaspa.shares_accepted = n;
                    updated = true;
                }
                if let Some(n) = extract_delimited_share(&lower, "rej") {
                    self.kaspa.shares_rejected = n;
                    updated = true;
                }
            }
            MinerBrand::Xmrig | MinerBrand::SRBMiner => {
                if let Some(hr) = extract_hashrate(&lower, &["h/s", "kh/s", "mh/s"]) {
                    // extract_hashrate normalizes to MH/s; Monero fields are H/s.
                    self.monero.hashrate_h_s = hr * 1e6;
                    self.monero.is_active = true;
                    updated = true;
                }
                if let Some(n) = extract_count_after(line, "accepted") {
                    self.monero.shares_accepted = n;
                    updated = true;
                }
                if let Some(n) = extract_count_after(line, "rejected") {
                    self.monero.shares_rejected = n;
                    updated = true;
                }
            }
            MinerBrand::Hellminer => {
                if let Some(hr) = extract_hashrate(&lower, &["h/s", "kh/s", "mh/s"]) {
                    // extract_hashrate normalizes to MH/s; Verus fields are H/s.
                    self.verus.hashrate_h_s = hr * 1e6;
                    self.verus.is_active = true;
                    updated = true;
                }
                if let Some(n) = extract_count_after(line, "accepted") {
                    self.verus.shares_accepted = n;
                    updated = true;
                }
                if let Some(n) = extract_count_after(line, "rejected") {
                    self.verus.shares_rejected = n;
                    updated = true;
                }
            }
            MinerBrand::Rigel => {
                if let Some(hr) = extract_hashrate(&lower, &["kh/s", "mh/s", "gh/s"]) {
                    self.quai.hashrate_mh_s = hr;
                    self.quai.is_active = true;
                    updated = true;
                }
                if let Some(n) = extract_count_after(line, "accepted") {
                    self.quai.shares_accepted = n;
                    updated = true;
                }
                if let Some(n) = extract_count_after(line, "rejected") {
                    self.quai.shares_rejected = n;
                    updated = true;
                }
            }
            MinerBrand::QubicCore => {
                if let Some(hr) = extract_hashrate(&lower, &["kh/s", "mh/s", "gh/s"]) {
                    // Qubic uses kH/s; extract_hashrate normalizes to MH/s, convert back
                    self.qubic.hashrate_kh_s = hr * 1000.0;
                    self.qubic.hashrate_sampled = true;
                    self.qubic.is_active = true;
                    updated = true;
                }
            }
            _ => {}
        }

        if updated {
            // Only advance wall-clock when a real mining field changed.
            self.timestamp = chrono::Utc::now();
        }
        updated
    }
}

/// Parse a hashrate token and normalize to **MH/s**.
///
/// Callers that store H/s or kH/s must convert back (`* 1e6` or `* 1e3`).
fn extract_hashrate(line: &str, suffixes: &[&str]) -> Option<f64> {
    for suffix in suffixes {
        let pattern = format!("([0-9.]+)\\s*{}", regex::escape(suffix));
        if let Ok(re) = regex::Regex::new(&pattern)
            && let Some(caps) = re.captures(line)
            && let Ok(val) = caps.get(1)?.as_str().parse::<f64>()
        {
            // Normalize all rates to MH/s.
            let multiplier = match *suffix {
                "h/s" => 1e-6,
                "kh/s" => 1e-3,
                "mh/s" | "mhs" => 1.0,
                "gh/s" | "ghs" => 1e3,
                "ths" => 1e6, // 1 TH/s = 1_000_000 MH/s
                _ => 1.0,
            };
            return Some(val * multiplier);
        }
    }
    None
}

#[cfg(test)]
mod hashrate_unit_tests {
    use super::*;

    #[test]
    fn xmrig_h_s_stored_as_hashes_per_second() {
        let mut stats = MiningStats::default();
        stats.update_from_line(
            MinerBrand::Xmrig,
            "speed 10s/60s/15m 1234.5 h/s max 2000.0 h/s",
        );
        assert!(stats.monero.is_active);
        assert!((stats.monero.hashrate_h_s - 1234.5).abs() < 1e-6);
    }

    #[test]
    fn hellminer_kh_s_converted_to_h_s() {
        let mut stats = MiningStats::default();
        stats.update_from_line(MinerBrand::Hellminer, "hashrate: 2.5 kh/s");
        assert!(stats.verus.is_active);
        assert!((stats.verus.hashrate_h_s - 2500.0).abs() < 1e-6);
    }

    #[test]
    fn ths_token_normalizes_to_mhs() {
        // BzMiner path accepts "ths" and stores MH/s (1 TH/s = 1e6 MH/s).
        let mut stats = MiningStats::default();
        stats.update_from_line(MinerBrand::BzMiner, "hashrate 1.5 ths");
        assert!(stats.kaspa.is_active);
        assert!((stats.kaspa.hashrate_mh_s - 1.5e6).abs() < 1.0);
    }

    #[test]
    fn update_from_line_refreshes_timestamp_on_mining_update() {
        let mut stats = MiningStats::default();
        let old_time = chrono::Utc::now() - chrono::Duration::seconds(10);
        stats.timestamp = old_time;
        assert!(stats.update_from_line(MinerBrand::Xmrig, "speed 10s 100.0 h/s"));
        assert!(stats.timestamp > old_time);
    }

    #[test]
    fn banner_line_does_not_count_as_update() {
        let mut stats = MiningStats::default();
        assert!(stats.update_from_line(MinerBrand::Xmrig, "speed 10s 100.0 h/s"));
        let t1 = stats.timestamp;
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(!stats.update_from_line(MinerBrand::Xmrig, "Connected to pool"));
        assert_eq!(stats.timestamp, t1);
        assert!(stats.monero.is_active); // sticky, but no new sample signal
    }
}

pub fn extract_count_after(line: &str, keyword: &str) -> Option<u64> {
    let lower = line.to_lowercase();
    let idx = lower.find(keyword)?;
    let after = &line[idx..];
    let re = regex::Regex::new(r"(\d+)").ok()?;
    re.captures(after)?.get(1)?.as_str().parse().ok()
}

/// Extract a share counter next to a short keyword (`acc`/`rej`) without matching
/// substrings of longer words (`acceleration`). Caller may pass a lowercased line.
fn extract_delimited_share(line: &str, keyword: &str) -> Option<u64> {
    let pattern = format!(r"\b{}\b\s*[:=]?\s*(\d+)", regex::escape(keyword));
    let re = regex::Regex::new(&pattern).ok()?;
    re.captures(line)?.get(1)?.as_str().parse().ok()
}

#[cfg(test)]
mod bzminer_share_tests {
    use super::*;

    #[test]
    fn bzminer_acc_rej_counters() {
        let mut stats = MiningStats::default();
        assert!(stats.update_from_line(MinerBrand::BzMiner, "Shares: acc: 12 rej: 1"));
        assert_eq!(stats.kaspa.shares_accepted, 12);
        assert_eq!(stats.kaspa.shares_rejected, 1);
    }

    #[test]
    fn bzminer_acceleration_is_not_acc_share() {
        let mut stats = MiningStats::default();
        // Must not treat "acceleration" as the short "acc" share keyword.
        assert!(
            !stats.update_from_line(MinerBrand::BzMiner, "GPU0 acceleration enabled profile=3")
        );
        assert_eq!(stats.kaspa.shares_accepted, 0);
        assert_eq!(stats.kaspa.shares_rejected, 0);
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct QuaiStats {
    pub hashrate_mh_s: f64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub difficulty: f64,
    pub pool_difficulty: f64,
    pub uptime_seconds: u64,
    pub is_active: bool,
}

impl Default for QuaiStats {
    fn default() -> Self {
        Self {
            hashrate_mh_s: 0.0,
            shares_accepted: 0,
            shares_rejected: 0,
            difficulty: 0.0,
            pool_difficulty: 0.0,
            uptime_seconds: 0,
            is_active: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct QubicStats {
    pub hashrate_kh_s: f64,
    pub solutions_found: u64,
    pub current_tick: u32,
    pub peers_connected: u16,
    pub uptime_seconds: u64,
    pub is_active: bool,
    pub epoch_progress: f32,
    pub aigarth_active: bool,
    /// True after a stdout hashrate token was parsed (including explicit 0 kH/s).
    pub hashrate_sampled: bool,
    /// True after a health/tick poll set `current_tick` (including tick 0).
    pub tick_sampled: bool,
}

impl Default for QubicStats {
    fn default() -> Self {
        Self {
            hashrate_kh_s: 0.0,
            solutions_found: 0,
            current_tick: 0,
            peers_connected: 0,
            uptime_seconds: 0,
            is_active: false,
            epoch_progress: 0.0,
            aigarth_active: false,
            hashrate_sampled: false,
            tick_sampled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MoneroStats {
    pub hashrate_h_s: f64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub uptime_seconds: u64,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerusStats {
    pub hashrate_h_s: f64,
    pub shares_accepted: u64,
    pub shares_rejected: u64,
    pub uptime_seconds: u64,
    pub is_active: bool,
}

impl Default for VerusStats {
    fn default() -> Self {
        Self {
            hashrate_h_s: 0.0,
            shares_accepted: 0,
            shares_rejected: 0,
            uptime_seconds: 0,
            is_active: false,
        }
    }
}

impl Default for MoneroStats {
    fn default() -> Self {
        Self {
            hashrate_h_s: 0.0,
            shares_accepted: 0,
            shares_rejected: 0,
            uptime_seconds: 0,
            is_active: false,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MiningTelemetry {
    pub stats: MiningStats,
}

impl MiningTelemetry {
    pub fn new() -> Self {
        Self {
            stats: MiningStats {
                dynex: DynexStats::default(),
                quai: QuaiStats {
                    hashrate_mh_s: 0.0,
                    shares_accepted: 0,
                    shares_rejected: 0,
                    difficulty: 0.0,
                    pool_difficulty: 0.0,
                    uptime_seconds: 0,
                    is_active: false,
                },
                qubic: QubicStats::default(),
                monero: MoneroStats::default(),
                verus: VerusStats::default(),
                kaspa: KaspaStats::default(),
                timestamp: chrono::Utc::now(),
            },
        }
    }

    pub fn update(&mut self) -> anyhow::Result<()> {
        // TODO: Implement telemetry gathering
        Ok(())
    }

    pub fn get_stats(&self) -> &MiningStats {
        &self.stats
    }
}

impl Default for MiningTelemetry {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub enum WireMsg {
    /// Boxed to keep `WireMsg` small (clippy `large_enum_variant`).
    MiningTelem(Box<MiningTelemetry>),
    Status(String),
    GpuSchedulerEvent(GpuSchedulerEvent),
    RotationEvent(RotationEvent),
}

impl WireMsg {
    pub fn mining_telem(t: MiningTelemetry) -> Self {
        Self::MiningTelem(Box::new(t))
    }
}

//! Telemetry Schema module — durable JSONL envelope + payloads (schema_version = 1).
//!
//! Both supervisor and collector are adapters that serialize these records.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    CoinType, DynexStats, GpuSchedulerEvent, KaspaStats, MiningStats, MiningTelemetry, MoneroStats,
    QuaiStats, QubicStats, RotationEvent, VerusStats, WireMsg,
};

/// Current on-disk schema. Break-free: no dual-compat with pre-schema free-form JSON.
pub const SCHEMA_VERSION: u32 = 1;

/// Record class on every envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    MinerPerf,
    NodeHealth,
    HostHw,
    Status,
    /// GPU resource scheduler events (collector/supervisor governance)
    GpuSched,
    Rotation,
}

/// Full durable line: envelope fields + nested payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelemetryEnvelope {
    pub schema_version: u32,
    pub timestamp: DateTime<Utc>,
    /// Producer: `supervisor`, `collector`, etc.
    pub source: String,
    pub kind: RecordKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    /// Multi-stem file basename (without `.jsonl`).
    pub stem: String,
    pub payload: TelemetryPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TelemetryPayload {
    MinerPerf {
        coin: String,
        hashrate: f64,
        /// Unit string matching `hashrate` (e.g. `MH/s`, `H/s`, `kH/s`).
        hashrate_unit: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        shares_accepted: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        shares_rejected: Option<u64>,
        is_active: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        uptime_seconds: Option<u64>,
    },
    NodeHealth {
        coin: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        height: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        target_height: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tick: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        epoch: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        active: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        speed_hs: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        threads: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        hashrate_mh: Option<f64>,
    },
    HostHw {
        #[serde(skip_serializing_if = "Option::is_none")]
        cpu_tctl_c: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cpu_ccd1_c: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cpu_ccd2_c: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        cpu_package_power_w: Option<f64>,
    },
    Status {
        message: String,
    },
    GpuSched {
        decision: String,
        vram_used_mb: u64,
        vram_total_mb: u64,
        gpu_temp_c: f32,
        power_w: f32,
        transition_count: u64,
    },
    Rotation {
        kind: String,
        from_algo: Option<String>,
        to_algo: Option<String>,
        market_age_secs: f64,
    },
}

impl TelemetryEnvelope {
    pub fn new(
        source: impl Into<String>,
        kind: RecordKind,
        stem: impl Into<String>,
        payload: TelemetryPayload,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            timestamp: Utc::now(),
            source: source.into(),
            kind,
            host: detect_host(),
            run_id: std::env::var("TELEMETRY_RUN_ID")
                .ok()
                .filter(|s| !s.is_empty()),
            stem: stem.into(),
            payload,
        }
    }

    /// Serialize as one JSONL line (no trailing newline).
    pub fn to_json_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn to_json_value(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }
}

/// Best-effort hostname for the envelope.
pub fn detect_host() -> Option<String> {
    if let Ok(h) = std::env::var("HOSTNAME") {
        let t = h.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    if let Ok(h) = std::env::var("HOST") {
        let t = h.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn coin_stem(coin: &str) -> String {
    format!("{coin}_telemetry")
}

fn coin_name(c: CoinType) -> &'static str {
    match c {
        CoinType::Dynex => "dynex",
        CoinType::Quai => "quai",
        CoinType::Qubic => "qubic",
        CoinType::Kaspa => "kaspa",
        CoinType::Monero => "monero",
        CoinType::Verus => "verus",
        CoinType::Ocean => "ocean",
        CoinType::Unknown => "unknown",
    }
}

/// Project mining stats into one envelope per **active** coin (miner_perf).
pub fn envelopes_from_mining_stats(source: &str, stats: &MiningStats) -> Vec<TelemetryEnvelope> {
    let mut out = Vec::new();
    let ts = stats.timestamp;

    let push = |out: &mut Vec<TelemetryEnvelope>,
                coin: CoinType,
                hashrate: f64,
                unit: &str,
                acc: u64,
                rej: u64,
                active: bool,
                uptime: u64| {
        if !active && hashrate == 0.0 {
            return;
        }
        let name = coin_name(coin);
        let mut env = TelemetryEnvelope::new(
            source,
            RecordKind::MinerPerf,
            coin_stem(name),
            TelemetryPayload::MinerPerf {
                coin: name.to_string(),
                hashrate,
                hashrate_unit: unit.to_string(),
                shares_accepted: Some(acc),
                shares_rejected: Some(rej),
                is_active: active,
                uptime_seconds: Some(uptime),
            },
        );
        env.timestamp = ts;
        out.push(env);
    };

    let d: &DynexStats = &stats.dynex;
    if d.is_active {
        push(
            &mut out,
            CoinType::Dynex,
            d.hashrate_mh_s,
            "MH/s",
            d.shares_accepted,
            d.shares_rejected,
            d.is_active,
            d.uptime_seconds,
        );
    }
    let q: &QuaiStats = &stats.quai;
    if q.is_active {
        push(
            &mut out,
            CoinType::Quai,
            q.hashrate_mh_s,
            "MH/s",
            q.shares_accepted,
            q.shares_rejected,
            q.is_active,
            q.uptime_seconds,
        );
    }
    let qb: &QubicStats = &stats.qubic;
    // Prefer explicit sample flags: stdout hashrate (incl. 0 kH/s) vs health tick (incl. 0).
    if qb.hashrate_sampled || qb.solutions_found > 0 {
        push(
            &mut out,
            CoinType::Qubic,
            qb.hashrate_kh_s,
            "kH/s",
            qb.solutions_found,
            0,
            qb.is_active,
            qb.uptime_seconds,
        );
    } else if qb.tick_sampled || qb.current_tick > 0 {
        let mut env = node_health(
            source,
            "qubic_telemetry",
            NodeHealthInput {
                coin: "qubic".into(),
                tick: Some(u64::from(qb.current_tick)),
                ..Default::default()
            },
        );
        env.timestamp = ts;
        out.push(env);
    }
    let k: &KaspaStats = &stats.kaspa;
    if k.is_active {
        push(
            &mut out,
            CoinType::Kaspa,
            k.hashrate_mh_s,
            "MH/s",
            k.shares_accepted,
            k.shares_rejected,
            k.is_active,
            k.uptime_seconds,
        );
    }
    let m: &MoneroStats = &stats.monero;
    if m.is_active {
        push(
            &mut out,
            CoinType::Monero,
            m.hashrate_h_s,
            "H/s",
            m.shares_accepted,
            m.shares_rejected,
            m.is_active,
            m.uptime_seconds,
        );
    }
    let v: &VerusStats = &stats.verus;
    if v.is_active {
        push(
            &mut out,
            CoinType::Verus,
            v.hashrate_h_s,
            "H/s",
            v.shares_accepted,
            v.shares_rejected,
            v.is_active,
            v.uptime_seconds,
        );
    }

    out
}

pub fn envelopes_from_mining_telemetry(
    source: &str,
    t: &MiningTelemetry,
) -> Vec<TelemetryEnvelope> {
    envelopes_from_mining_stats(source, &t.stats)
}

/// Project a `WireMsg` into zero or more envelopes for durable write.
///
/// `WireMsg::Status` is **not** written to JSONL (high volume / log-only).
/// Use tracing for status strings; disk holds structured miner/hw/control events.
pub fn envelopes_from_wire(source: &str, msg: &WireMsg) -> Vec<TelemetryEnvelope> {
    match msg {
        WireMsg::MiningTelem(t) => envelopes_from_mining_telemetry(source, t.as_ref()),
        WireMsg::Status(_) => Vec::new(),
        WireMsg::GpuSchedulerEvent(e) => vec![envelope_from_gpu_sched(source, e)],
        WireMsg::RotationEvent(e) => vec![envelope_from_rotation(source, e)],
    }
}

pub fn envelope_from_gpu_sched(source: &str, e: &GpuSchedulerEvent) -> TelemetryEnvelope {
    TelemetryEnvelope::new(
        source,
        RecordKind::GpuSched,
        "gpu_sched_telemetry",
        TelemetryPayload::GpuSched {
            decision: e.decision.clone(),
            vram_used_mb: e.vram_used_mb,
            vram_total_mb: e.vram_total_mb,
            gpu_temp_c: e.gpu_temp_c,
            power_w: e.power_w,
            transition_count: e.transition_count,
        },
    )
}

pub fn envelope_from_rotation(source: &str, e: &RotationEvent) -> TelemetryEnvelope {
    TelemetryEnvelope::new(
        source,
        RecordKind::Rotation,
        "rotation_telemetry",
        TelemetryPayload::Rotation {
            kind: e.kind.clone(),
            from_algo: e.from_algo.clone(),
            to_algo: e.to_algo.clone(),
            market_age_secs: e.market_age_secs,
        },
    )
}

// ── Collector helpers ────────────────────────────────────────────────────────

/// Fields for a `node_health` envelope (avoids a long positional arg list).
#[derive(Debug, Clone, Default)]
pub struct NodeHealthInput {
    pub coin: String,
    pub height: Option<u64>,
    pub target_height: Option<u64>,
    pub tick: Option<u64>,
    pub epoch: Option<u64>,
    pub active: Option<bool>,
    pub speed_hs: Option<u64>,
    pub threads: Option<u64>,
    pub hashrate_mh: Option<f64>,
}

pub fn node_health(source: &str, stem: &str, input: NodeHealthInput) -> TelemetryEnvelope {
    TelemetryEnvelope::new(
        source,
        RecordKind::NodeHealth,
        stem,
        TelemetryPayload::NodeHealth {
            coin: input.coin,
            height: input.height,
            target_height: input.target_height,
            tick: input.tick,
            epoch: input.epoch,
            active: input.active,
            speed_hs: input.speed_hs,
            threads: input.threads,
            hashrate_mh: input.hashrate_mh,
        },
    )
}

pub fn host_hw(
    source: &str,
    stem: &str,
    cpu_tctl_c: Option<f64>,
    cpu_ccd1_c: Option<f64>,
    cpu_ccd2_c: Option<f64>,
    cpu_package_power_w: Option<f64>,
) -> TelemetryEnvelope {
    TelemetryEnvelope::new(
        source,
        RecordKind::HostHw,
        stem,
        TelemetryPayload::HostHw {
            cpu_tctl_c,
            cpu_ccd1_c,
            cpu_ccd2_c,
            cpu_package_power_w,
        },
    )
}

/// Synchronous multi-stem JSONL sink (supervisor + tests).
pub struct JsonlSink {
    data_dir: std::path::PathBuf,
}

impl JsonlSink {
    pub fn new(data_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    pub fn write_envelope(&self, env: &TelemetryEnvelope) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        let path = self.data_dir.join(format!("{}.jsonl", env.stem));
        let mut line = env
            .to_json_line()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        f.write_all(line.as_bytes())?;
        f.flush()
    }

    pub fn write_all(&self, envs: &[TelemetryEnvelope]) -> std::io::Result<()> {
        for e in envs {
            self.write_envelope(e)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_version_is_one() {
        assert_eq!(SCHEMA_VERSION, 1);
    }

    #[test]
    fn envelope_round_trip_miner_perf() {
        let env = TelemetryEnvelope::new(
            "supervisor",
            RecordKind::MinerPerf,
            "dynex_telemetry",
            TelemetryPayload::MinerPerf {
                coin: "dynex".into(),
                hashrate: 1.5,
                hashrate_unit: "MH/s".into(),
                shares_accepted: Some(3),
                shares_rejected: Some(0),
                is_active: true,
                uptime_seconds: Some(60),
            },
        );
        let line = env.to_json_line().unwrap();
        assert!(line.contains("\"schema_version\":1"));
        assert!(line.contains("miner_perf") || line.contains("\"kind\":\"miner_perf\""));
        let back: TelemetryEnvelope = serde_json::from_str(&line).unwrap();
        assert_eq!(back.schema_version, 1);
        assert_eq!(back.stem, "dynex_telemetry");
        assert_eq!(back.kind, RecordKind::MinerPerf);
    }

    #[test]
    fn envelope_round_trip_host_hw() {
        let env = host_hw(
            "collector",
            "hwmon_telemetry",
            Some(61.0),
            Some(55.0),
            None,
            None,
        );
        let line = env.to_json_line().unwrap();
        let back: TelemetryEnvelope = serde_json::from_str(&line).unwrap();
        assert_eq!(back.kind, RecordKind::HostHw);
        match back.payload {
            TelemetryPayload::HostHw { cpu_tctl_c, .. } => {
                assert_eq!(cpu_tctl_c, Some(61.0));
            }
            _ => panic!("wrong payload"),
        }
    }

    #[test]
    fn mining_stats_active_coin_emits_envelope() {
        let mut stats = MiningStats::default();
        stats.dynex.is_active = true;
        stats.dynex.hashrate_mh_s = 2.0;
        let envs = envelopes_from_mining_stats("supervisor", &stats);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].stem, "dynex_telemetry");
        assert_eq!(envs[0].kind, RecordKind::MinerPerf);
    }

    #[test]
    fn inactive_coins_emit_nothing() {
        let stats = MiningStats::default();
        assert!(envelopes_from_mining_stats("supervisor", &stats).is_empty());
    }

    #[test]
    fn qubic_tick_only_emits_node_health() {
        let mut stats = MiningStats::default();
        stats.qubic.is_active = true;
        stats.qubic.current_tick = 42_000;
        stats.qubic.tick_sampled = true;
        let envs = envelopes_from_mining_stats("supervisor", &stats);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].kind, RecordKind::NodeHealth);
        assert_eq!(envs[0].stem, "qubic_telemetry");
    }

    #[test]
    fn qubic_explicit_zero_hashrate_emits_miner_perf() {
        // Stdout "0 kH/s" marks hashrate_sampled — must not drop or become NodeHealth.
        let mut stats = MiningStats::default();
        stats.qubic.is_active = true;
        stats.qubic.hashrate_kh_s = 0.0;
        stats.qubic.hashrate_sampled = true;
        stats.qubic.current_tick = 0;
        let envs = envelopes_from_mining_stats("supervisor", &stats);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].kind, RecordKind::MinerPerf);
        assert_eq!(envs[0].stem, "qubic_telemetry");
    }

    #[test]
    fn qubic_zero_tick_health_emits_node_health() {
        // healthcheck can parse {"tick":0} at startup — must not become MinerPerf.
        let mut stats = MiningStats::default();
        stats.qubic.is_active = true;
        stats.qubic.current_tick = 0;
        stats.qubic.tick_sampled = true;
        let envs = envelopes_from_mining_stats("supervisor", &stats);
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].kind, RecordKind::NodeHealth);
    }

    #[test]
    fn jsonl_sink_writes_stem_file() {
        let dir = std::env::temp_dir().join(format!("theseus_schema_sink_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sink = JsonlSink::new(&dir);
        let env = node_health(
            "collector",
            "quai_telemetry",
            NodeHealthInput {
                coin: "quai".into(),
                height: Some(42),
                ..Default::default()
            },
        );
        sink.write_envelope(&env).unwrap();
        let text = std::fs::read_to_string(dir.join("quai_telemetry.jsonl")).unwrap();
        assert!(text.contains("\"schema_version\":1"));
        assert!(text.contains("\"height\":42") || text.contains("\"height\": 42"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

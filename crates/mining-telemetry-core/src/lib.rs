mod schema;

pub use schema::{
    JsonlSink, MinerPerfInput, NodeHealthInput, RecordKind, SCHEMA_VERSION, TelemetryEnvelope,
    TelemetryPayload, detect_host, envelope_from_gpu_sched, envelope_from_rotation, host_hw,
    miner_perf, node_health,
};

use serde::Serialize;

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
pub struct RotationEvent {
    pub kind: String,
    pub from_algo: Option<String>,
    pub to_algo: Option<String>,
    pub revenues: Vec<(String, f64, f64)>,
    pub market_age_secs: f64,
}

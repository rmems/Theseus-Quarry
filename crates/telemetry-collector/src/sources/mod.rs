use mining_telemetry_core::TelemetryEnvelope;

pub mod bzminer;
pub mod dynex;
pub mod hwmon;
pub mod monero;
pub mod onezerominer;
pub mod quai;
pub mod qubic;
pub mod rapl;
pub mod xmrig;

/// Poll result from a telemetry source (schema v1 envelope).
pub struct TelemetryRecord {
    /// Source name for logs (e.g. "monero", "hwmon").
    pub source: &'static str,
    /// Durable record. `None` means the source failed this cycle.
    pub envelope: Option<TelemetryEnvelope>,
}

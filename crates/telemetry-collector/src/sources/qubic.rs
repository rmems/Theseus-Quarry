use mining_telemetry_core::node_health;
use super::TelemetryRecord;

const QUBIC_TICK_URL: &str = "http://127.0.0.1:8099/tick-info";

pub async fn poll(client: &reqwest::Client) -> TelemetryRecord {
    TelemetryRecord {
        source: "qubic",
        envelope: try_poll(client).await,
    }
}

async fn try_poll(client: &reqwest::Client) -> Option<mining_telemetry_core::TelemetryEnvelope> {
    let resp: serde_json::Value = client
        .get(QUBIC_TICK_URL)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let tick_info = resp.get("tick_info").unwrap_or(&resp);
    let tick = tick_info.get("tick").and_then(|v| v.as_u64());
    let epoch = tick_info.get("epoch").and_then(|v| v.as_u64());
    Some(node_health(
        "collector",
        "qubic_telemetry",
        "qubic",
        None,
        None,
        tick,
        epoch,
        None,
        None,
        None,
        None,
    ))
}

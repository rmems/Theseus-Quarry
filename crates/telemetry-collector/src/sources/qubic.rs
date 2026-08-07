use super::TelemetryRecord;
use mining_telemetry_core::{NodeHealthInput, node_health};

pub async fn poll(client: &reqwest::Client, endpoint: &str) -> TelemetryRecord {
    TelemetryRecord {
        source: "qubic",
        envelope: try_poll(client, endpoint).await,
    }
}

async fn try_poll(
    client: &reqwest::Client,
    endpoint: &str,
) -> Option<mining_telemetry_core::TelemetryEnvelope> {
    let resp: serde_json::Value = client.get(endpoint).send().await.ok()?.json().await.ok()?;
    let tick_info = resp.get("tick_info").unwrap_or(&resp);
    let tick = tick_info.get("tick").and_then(|v| v.as_u64());
    let epoch = tick_info.get("epoch").and_then(|v| v.as_u64());
    Some(node_health(
        "collector",
        "qubic_telemetry",
        NodeHealthInput {
            coin: "qubic".into(),
            tick,
            epoch,
            ..Default::default()
        },
    ))
}

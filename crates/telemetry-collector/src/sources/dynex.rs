use super::TelemetryRecord;
use mining_telemetry_core::{NodeHealthInput, node_health};

pub async fn poll(client: &reqwest::Client, endpoint: &str) -> TelemetryRecord {
    TelemetryRecord {
        source: "dynex",
        envelope: try_poll(client, endpoint).await,
    }
}

async fn try_poll(
    client: &reqwest::Client,
    endpoint: &str,
) -> Option<mining_telemetry_core::TelemetryEnvelope> {
    let resp: serde_json::Value = client.get(endpoint).send().await.ok()?.json().await.ok()?;
    let height = resp.get("height").and_then(|v| v.as_u64());
    Some(node_health(
        "collector",
        "dynex_telemetry",
        NodeHealthInput {
            coin: "dynex".into(),
            height,
            ..Default::default()
        },
    ))
}

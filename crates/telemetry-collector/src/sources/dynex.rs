use mining_telemetry_core::node_health;
use super::TelemetryRecord;

const DYNEX_HEIGHT_URL: &str = "http://127.0.0.1:17336/getheight";

pub async fn poll(client: &reqwest::Client) -> TelemetryRecord {
    TelemetryRecord {
        source: "dynex",
        envelope: try_poll(client).await,
    }
}

async fn try_poll(client: &reqwest::Client) -> Option<mining_telemetry_core::TelemetryEnvelope> {
    let resp: serde_json::Value = client
        .get(DYNEX_HEIGHT_URL)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let height = resp.get("height").and_then(|v| v.as_u64());
    Some(node_health(
        "collector",
        "dynex_telemetry",
        "dynex",
        height,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    ))
}

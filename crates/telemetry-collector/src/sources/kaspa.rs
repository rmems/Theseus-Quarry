use super::TelemetryRecord;
use mining_telemetry_core::{NodeHealthInput, node_health};

pub async fn poll(client: &reqwest::Client, endpoint: &str) -> TelemetryRecord {
    TelemetryRecord {
        source: "kaspa",
        envelope: try_poll(client, endpoint).await,
    }
}

async fn try_poll(
    client: &reqwest::Client,
    endpoint: &str,
) -> Option<mining_telemetry_core::TelemetryEnvelope> {
    let resp: serde_json::Value = client.get(endpoint).send().await.ok()?.json().await.ok()?;
    let mut hashrate_mh = None;
    for key in &["hashrate", "total_hashrate", "hashrate_mh", "hashrate_mhs"] {
        if let Some(val) = resp.get(key) {
            hashrate_mh = val.as_f64();
            break;
        }
    }
    Some(node_health(
        "collector",
        "kaspa_telemetry",
        NodeHealthInput {
            coin: "kaspa".into(),
            hashrate_mh,
            ..Default::default()
        },
    ))
}

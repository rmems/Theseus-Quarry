use mining_telemetry_core::node_health;
use super::TelemetryRecord;

const KASPA_BZ_API: &str = "http://127.0.0.1:4014/";

pub async fn poll(client: &reqwest::Client) -> TelemetryRecord {
    TelemetryRecord {
        source: "kaspa",
        envelope: try_poll(client).await,
    }
}

async fn try_poll(client: &reqwest::Client) -> Option<mining_telemetry_core::TelemetryEnvelope> {
    let resp: serde_json::Value = client
        .get(KASPA_BZ_API)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
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
        "kaspa",
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        hashrate_mh.or(Some(0.0)),
    ))
}

use super::TelemetryRecord;
use mining_telemetry_core::{NodeHealthInput, node_health};
use serde_json::json;

const MONERO_RPC: &str = "http://127.0.0.1:18081/json_rpc";

pub async fn poll(client: &reqwest::Client) -> TelemetryRecord {
    let envelope = try_poll(client).await;
    TelemetryRecord {
        source: "monero",
        envelope,
    }
}

async fn try_poll(client: &reqwest::Client) -> Option<mining_telemetry_core::TelemetryEnvelope> {
    let mining_body = json!({"jsonrpc": "2.0", "id": "0", "method": "mining_status"});
    let info_body = json!({"jsonrpc": "2.0", "id": "0", "method": "get_info"});

    let mining_resp: serde_json::Value = client
        .post(MONERO_RPC)
        .json(&mining_body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let info_resp: serde_json::Value = client
        .post(MONERO_RPC)
        .json(&info_body)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    let mining = mining_resp.get("result")?;
    let info = info_resp.get("result")?;

    Some(node_health(
        "collector",
        "monero_telemetry",
        NodeHealthInput {
            coin: "monero".into(),
            height: info.get("height").and_then(|v| v.as_u64()),
            target_height: info.get("target_height").and_then(|v| v.as_u64()),
            active: mining.get("active").and_then(|v| v.as_bool()),
            // monerod may return integer or fractional speed (H/s).
            speed_hs: mining.get("speed").and_then(|v| {
                v.as_u64()
                    .or_else(|| v.as_f64().map(|f| f.round().max(0.0) as u64))
            }),
            threads: mining.get("threads_count").and_then(|v| v.as_u64()),
            ..Default::default()
        },
    ))
}

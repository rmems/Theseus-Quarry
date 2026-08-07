use super::TelemetryRecord;
use mining_telemetry_core::{NodeHealthInput, node_health};
use serde_json::json;

pub async fn poll(client: &reqwest::Client, endpoint: &str) -> TelemetryRecord {
    TelemetryRecord {
        source: "quai",
        envelope: try_poll(client, endpoint).await,
    }
}

async fn try_poll(
    client: &reqwest::Client,
    endpoint: &str,
) -> Option<mining_telemetry_core::TelemetryEnvelope> {
    let payloads = [
        json!({"jsonrpc": "2.0", "method": "eth_blockNumber", "params": [], "id": 1}),
        json!({"jsonrpc": "2.0", "method": "quai_blockNumber", "params": [], "id": 1}),
    ];
    for payload in &payloads {
        if let Ok(resp) = client.post(endpoint).json(payload).send().await
            && let Ok(val) = resp.json::<serde_json::Value>().await
            && let Some(res) = val.get("result").and_then(|v| v.as_str())
            && let Some(stripped) = res.strip_prefix("0x")
            && let Ok(h) = u64::from_str_radix(stripped, 16)
        {
            return Some(node_health(
                "collector",
                "quai_telemetry",
                NodeHealthInput {
                    coin: "quai".into(),
                    height: Some(h),
                    ..Default::default()
                },
            ));
        }
    }
    None
}

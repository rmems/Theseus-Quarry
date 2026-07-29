use super::TelemetryRecord;
use mining_telemetry_core::{NodeHealthInput, node_health};
use serde_json::json;

const QUAI_RPC: &str = "http://127.0.0.1:9001";

pub async fn poll(client: &reqwest::Client) -> TelemetryRecord {
    TelemetryRecord {
        source: "quai",
        envelope: try_poll(client).await,
    }
}

async fn try_poll(client: &reqwest::Client) -> Option<mining_telemetry_core::TelemetryEnvelope> {
    let payloads = [
        json!({"jsonrpc": "2.0", "method": "eth_blockNumber", "params": [], "id": 1}),
        json!({"jsonrpc": "2.0", "method": "quai_blockNumber", "params": [], "id": 1}),
    ];
    for payload in &payloads {
        if let Ok(resp) = client.post(QUAI_RPC).json(payload).send().await
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

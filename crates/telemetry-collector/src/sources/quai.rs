use mining_telemetry_core::node_health;
use serde_json::json;
use super::TelemetryRecord;

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
        if let Ok(resp) = client.post(QUAI_RPC).json(payload).send().await {
            if let Ok(val) = resp.json::<serde_json::Value>().await {
                if let Some(res) = val.get("result").and_then(|v| v.as_str()) {
                    if let Some(stripped) = res.strip_prefix("0x") {
                        if let Ok(h) = u64::from_str_radix(stripped, 16) {
                            return Some(node_health(
                                "collector",
                                "quai_telemetry",
                                "quai",
                                Some(h),
                                None,
                                None,
                                None,
                                None,
                                None,
                                None,
                                None,
                            ));
                        }
                    }
                }
            }
        }
    }
    None
}

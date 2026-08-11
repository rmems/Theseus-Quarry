//! XMRig local HTTP API → MinerPerf.
//!
//! Typical endpoint root: `http://127.0.0.1:4015/` (MONERO_API_PORT).
//! Summary path: `/1/summary`. Hashrate is H/s.
//!
//! Stem: `{coin}_miner_telemetry` (distinct from node `{coin}_telemetry`).
//!
//! SRBMiner-Multi is **not** parsed here (different JSON shape). Use a dedicated
//! source if the deployment runs SRBMiner on this port.

use super::TelemetryRecord;
use mining_telemetry_core::{MinerPerfInput, miner_perf};

pub async fn poll(client: &reqwest::Client, endpoint: &str, coin: &str) -> TelemetryRecord {
    TelemetryRecord {
        source: "xmrig",
        envelope: try_poll(client, endpoint, coin).await,
    }
}

async fn try_poll(
    client: &reqwest::Client,
    endpoint: &str,
    coin: &str,
) -> Option<mining_telemetry_core::TelemetryEnvelope> {
    let base = endpoint.trim_end_matches('/');
    let url = format!("{base}/1/summary");
    let response = client.get(&url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let resp: serde_json::Value = response.json().await.ok()?;
    let parsed = parse_summary(&resp)?;
    let stem = format!("{coin}_miner_telemetry");
    Some(miner_perf(
        "collector",
        &stem,
        MinerPerfInput {
            coin: coin.to_string(),
            hashrate: parsed.hashrate_hs,
            hashrate_unit: "H/s".into(),
            shares_accepted: parsed.shares_accepted,
            shares_rejected: parsed.shares_rejected,
            is_active: parsed.hashrate_hs > 0.0,
            uptime_seconds: parsed.uptime_seconds,
        },
    ))
}

#[derive(Debug, PartialEq)]
struct ParsedSummary {
    hashrate_hs: f64,
    shares_accepted: Option<u64>,
    shares_rejected: Option<u64>,
    uptime_seconds: Option<u64>,
}

/// XMRig `/1/summary`: `hashrate.total[i]` may be null before the window fills.
fn parse_summary(resp: &serde_json::Value) -> Option<ParsedSummary> {
    let hashrate_hs = first_numeric_in_array(resp.pointer("/hashrate/total"))
        .or_else(|| resp.pointer("/hashrate/highest").and_then(|v| v.as_f64()))?;

    let shares_good = resp
        .pointer("/results/shares_good")
        .and_then(|v| v.as_u64());
    let shares_total = resp
        .pointer("/results/shares_total")
        .and_then(|v| v.as_u64());
    let shares_rejected = match (shares_total, shares_good) {
        (Some(total), Some(good)) => Some(total.saturating_sub(good)),
        _ => None,
    };
    let uptime = resp.get("uptime").and_then(|v| v.as_u64());

    Some(ParsedSummary {
        hashrate_hs,
        shares_accepted: shares_good,
        shares_rejected,
        uptime_seconds: uptime,
    })
}

fn first_numeric_in_array(v: Option<&serde_json::Value>) -> Option<f64> {
    let arr = v?.as_array()?;
    for item in arr {
        if let Some(n) = item.as_f64() {
            return Some(n);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_total_skipping_nulls() {
        let v = json!({
            "hashrate": {"total": [null, 1234.5, 1000.0], "highest": 2000.0},
            "results": {"shares_good": 10, "shares_total": 12},
            "uptime": 99
        });
        let p = parse_summary(&v).unwrap();
        assert!((p.hashrate_hs - 1234.5).abs() < 1e-6);
        assert_eq!(p.shares_accepted, Some(10));
        assert_eq!(p.shares_rejected, Some(2));
        assert_eq!(p.uptime_seconds, Some(99));
    }

    #[test]
    fn falls_back_to_highest() {
        let v = json!({
            "hashrate": {"total": [null, null], "highest": 500.0},
            "results": {}
        });
        let p = parse_summary(&v).unwrap();
        assert!((p.hashrate_hs - 500.0).abs() < 1e-6);
    }

    #[test]
    fn rejects_missing_hashrate() {
        let v = json!({"results": {"shares_good": 1}});
        assert!(parse_summary(&v).is_none());
    }
}

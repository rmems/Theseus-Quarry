//! XMRig (and compatible) local HTTP API → MinerPerf.
//!
//! Typical endpoint root: `http://127.0.0.1:4015/` (MONERO_API_PORT).
//! Summary path: `/1/summary`.
//!
//! Note: SRBMiner-Multi can share this port but uses a different JSON layout
//! (algorithm/device arrays). Confirm against a live SRBMiner instance before
//! adding a fallback branch.

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
    let resp: serde_json::Value = client.get(&url).send().await.ok()?.json().await.ok()?;

    // XMRig: hashrate.total[0] is the short-window rate in H/s.
    let hashrate = resp
        .pointer("/hashrate/total/0")
        .and_then(|v| v.as_f64())
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

    let stem = format!("{coin}_telemetry");
    Some(miner_perf(
        "collector",
        &stem,
        MinerPerfInput {
            coin: coin.to_string(),
            hashrate,
            hashrate_unit: "H/s".into(),
            shares_accepted: shares_good,
            shares_rejected,
            is_active: hashrate > 0.0,
            uptime_seconds: uptime,
        },
    ))
}

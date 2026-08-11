//! BzMiner local HTTP API → MinerPerf.
//!
//! Typical endpoint: `http://127.0.0.1:4014/status` (KASPA_API_PORT).

use super::TelemetryRecord;
use mining_telemetry_core::{MinerPerfInput, miner_perf};

pub async fn poll(client: &reqwest::Client, endpoint: &str, coin: &str) -> TelemetryRecord {
    TelemetryRecord {
        source: "bzminer",
        envelope: try_poll(client, endpoint, coin).await,
    }
}

async fn try_poll(
    client: &reqwest::Client,
    endpoint: &str,
    coin: &str,
) -> Option<mining_telemetry_core::TelemetryEnvelope> {
    let base = endpoint.trim_end_matches('/');
    let url = format!("{base}/status");
    let resp: serde_json::Value = client.get(&url).send().await.ok()?.json().await.ok()?;

    // Sum pool hashrates when present; fall back to top-level keys.
    let mut hashrate = 0.0_f64;
    if let Some(pools) = resp.get("pools").and_then(|p| p.as_array()) {
        for pool in pools {
            if let Some(v) = pool
                .get("hashrate")
                .and_then(|v| v.as_f64())
                .or_else(|| pool.get("hash_rate").and_then(|v| v.as_f64()))
            {
                hashrate += v;
            }
        }
    }
    if hashrate == 0.0 {
        for key in &["hashrate", "total_hashrate", "hashrate_mh", "hashrate_mhs"] {
            if let Some(v) = resp.get(*key).and_then(|v| v.as_f64()) {
                hashrate = v;
                break;
            }
        }
    }

    let accepted = resp
        .get("valid_solutions")
        .and_then(|v| v.as_u64())
        .or_else(|| resp.get("accepted_shares").and_then(|v| v.as_u64()));
    let rejected = {
        let mut n = 0_u64;
        let mut any = false;
        for key in &["rejected_solutions", "invalid_solutions", "stale_solutions"] {
            if let Some(v) = resp.get(*key).and_then(|v| v.as_u64()) {
                n = n.saturating_add(v);
                any = true;
            }
        }
        if any {
            Some(n)
        } else {
            resp.get("rejected_shares").and_then(|v| v.as_u64())
        }
    };
    let uptime = resp
        .get("uptime_s")
        .and_then(|v| v.as_u64())
        .or_else(|| resp.get("uptime").and_then(|v| v.as_u64()));

    // BzMiner reports native H/s on the local API.
    let stem = format!("{coin}_telemetry");
    Some(miner_perf(
        "collector",
        &stem,
        MinerPerfInput {
            coin: coin.to_string(),
            hashrate,
            hashrate_unit: "H/s".into(),
            shares_accepted: accepted,
            shares_rejected: rejected,
            is_active: hashrate > 0.0,
            uptime_seconds: uptime,
        },
    ))
}

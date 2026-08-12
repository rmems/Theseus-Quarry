//! BzMiner local HTTP API → MinerPerf.
//!
//! Typical endpoint: `http://127.0.0.1:4014/status` (KASPA_API_PORT).
//! Stem: `{coin}_miner_telemetry` (distinct from node `{coin}_telemetry`).

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
    let response = client.get(&url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let resp: serde_json::Value = response.json().await.ok()?;
    let parsed = parse_status(&resp)?;
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
struct ParsedStatus {
    hashrate_hs: f64,
    shares_accepted: Option<u64>,
    shares_rejected: Option<u64>,
    uptime_seconds: Option<u64>,
}

/// Parse a BzMiner `/status` JSON body into native H/s.
///
/// Returns `None` when the body does not look like a BzMiner status document
/// (avoids treating error JSON as an idle miner).
fn parse_status(resp: &serde_json::Value) -> Option<ParsedStatus> {
    if !resp.is_object() {
        return None;
    }

    let mut hashrate_hs = 0.0_f64;
    let mut saw_hashrate = false;

    if let Some(pools) = resp.get("pools").and_then(|p| p.as_array()) {
        for pool in pools {
            if let Some(v) = pool
                .get("hashrate")
                .and_then(|v| v.as_f64())
                .or_else(|| pool.get("hash_rate").and_then(|v| v.as_f64()))
            {
                hashrate_hs += v;
                saw_hashrate = true;
            }
        }
    }

    if !saw_hashrate {
        // Prefer H/s keys first; convert MH/s keys when that is all we have.
        for key in &["hashrate", "total_hashrate", "hash_rate"] {
            if let Some(v) = resp.get(*key).and_then(|v| v.as_f64()) {
                hashrate_hs = v;
                saw_hashrate = true;
                break;
            }
        }
        if !saw_hashrate {
            for key in &["hashrate_mh", "hashrate_mhs"] {
                if let Some(v) = resp.get(*key).and_then(|v| v.as_f64()) {
                    hashrate_hs = v * 1e6;
                    saw_hashrate = true;
                    break;
                }
            }
        }
    }

    // Require at least one miner-shaped signal so random JSON is rejected.
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

    let looks_like_status = saw_hashrate
        || resp.get("pools").is_some()
        || accepted.is_some()
        || uptime.is_some()
        || resp.get("devices").is_some()
        || resp.get("gpus").is_some();
    if !looks_like_status {
        return None;
    }

    Some(ParsedStatus {
        hashrate_hs,
        shares_accepted: accepted,
        shares_rejected: rejected,
        uptime_seconds: uptime,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_pool_hashrate_as_hs() {
        let v = json!({
            "pools": [{"hashrate": 1.5e9}],
            "valid_solutions": 10,
            "rejected_solutions": 1,
            "uptime_s": 60
        });
        let p = parse_status(&v).unwrap();
        assert!((p.hashrate_hs - 1.5e9).abs() < 1.0);
        assert_eq!(p.shares_accepted, Some(10));
        assert_eq!(p.shares_rejected, Some(1));
        assert_eq!(p.uptime_seconds, Some(60));
    }

    #[test]
    fn converts_hashrate_mh_to_hs() {
        let v = json!({"hashrate_mh": 12.5, "uptime_s": 1});
        let p = parse_status(&v).unwrap();
        assert!((p.hashrate_hs - 12.5e6).abs() < 1.0);
    }

    #[test]
    fn rejects_unrelated_json() {
        let v = json!({"error": "not found", "code": 404});
        assert!(parse_status(&v).is_none());
    }
}

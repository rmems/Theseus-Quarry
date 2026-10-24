//! OneZeroMiner local HTTP API → MinerPerf.
//!
//! Typical endpoint: `http://127.0.0.1:3010/` (DYNEX_API_PORT).
//! Stem: `{coin}_miner_telemetry` (distinct from node `{coin}_telemetry`).
//!
//! Field names are not fully documented; parse defensively and return `None`
//! when a usable hashrate cannot be found. Confirm exact paths against a live
//! OneZeroMiner instance when available.
//!
//! Hashrate is labeled **H/s** (conservative default until a live unit field
//! is confirmed).

use super::TelemetryRecord;
use super::http_poll::{self, ProbeBudget};
use mining_telemetry_core::{MinerPerfInput, miner_perf};

pub async fn poll(
    client: &reqwest::Client,
    endpoint: &str,
    coin: &str,
    budget: ProbeBudget,
) -> TelemetryRecord {
    TelemetryRecord {
        source: "onezerominer",
        envelope: try_poll(client, endpoint, coin, budget).await,
    }
}

async fn try_poll(
    client: &reqwest::Client,
    endpoint: &str,
    coin: &str,
    budget: ProbeBudget,
) -> Option<mining_telemetry_core::TelemetryEnvelope> {
    let base = endpoint.trim_end_matches('/');
    let mut resp: Option<serde_json::Value> = None;
    for path in ["/status", "/"] {
        let Some(timeout) = budget.request_timeout() else {
            break;
        };
        let url = format!("{base}{path}");
        let Some(v) = http_poll::get_json_object(
            client,
            http_poll::json_request("onezerominer", &url, timeout),
        )
        .await
        else {
            continue;
        };
        if extract_hashrate(&v).is_some() {
            resp = Some(v);
            break;
        }
    }
    let resp = resp?;

    let hashrate = extract_hashrate(&resp)?;
    let accepted = first_u64(
        &resp,
        &[
            "accepted",
            "accepted_shares",
            "shares_accepted",
            "valid_shares",
            "valid_solutions",
        ],
    );
    let rejected = first_u64(
        &resp,
        &[
            "rejected",
            "rejected_shares",
            "shares_rejected",
            "invalid_shares",
            "stale_shares",
        ],
    );
    let uptime = first_u64(&resp, &["uptime", "uptime_s", "uptime_seconds"]);

    let stem = format!("{coin}_miner_telemetry");
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

fn extract_hashrate(v: &serde_json::Value) -> Option<f64> {
    for key in &[
        "hashrate",
        "total_hashrate",
        "hash_rate",
        "hashrate_total",
        "hr",
    ] {
        if let Some(n) = v.get(*key).and_then(|x| x.as_f64()) {
            return Some(n);
        }
    }
    // Sum per-GPU arrays when present.
    for key in &["gpus", "devices", "workers"] {
        if let Some(arr) = v.get(*key).and_then(|x| x.as_array()) {
            let mut sum = 0.0_f64;
            let mut any = false;
            for item in arr {
                if let Some(n) = item
                    .get("hashrate")
                    .and_then(|x| x.as_f64())
                    .or_else(|| item.get("hr").and_then(|x| x.as_f64()))
                {
                    sum += n;
                    any = true;
                }
            }
            if any {
                return Some(sum);
            }
        }
    }
    None
}

fn first_u64(v: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    for key in keys {
        if let Some(n) = v.get(*key).and_then(|x| x.as_u64()) {
            return Some(n);
        }
        if let Some(n) = v
            .pointer(&format!("/results/{key}"))
            .and_then(|x| x.as_u64())
            .or_else(|| v.pointer(&format!("/stats/{key}")).and_then(|x| x.as_u64()))
        {
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
    fn extract_hashrate_top_level() {
        let v = json!({"hashrate": 42.0, "accepted": 3});
        assert_eq!(extract_hashrate(&v), Some(42.0));
        assert_eq!(first_u64(&v, &["accepted", "accepted_shares"]), Some(3));
    }

    #[test]
    fn extract_hashrate_sums_gpus() {
        let v = json!({"gpus": [{"hashrate": 10.0}, {"hr": 5.5}]});
        assert_eq!(extract_hashrate(&v), Some(15.5));
    }

    #[test]
    fn extract_hashrate_missing() {
        assert!(extract_hashrate(&json!({"error": "nope"})).is_none());
    }

    #[test]
    fn first_u64_nested_results() {
        let v = json!({"results": {"shares_accepted": 7}});
        assert_eq!(first_u64(&v, &["shares_accepted", "accepted"]), Some(7));
    }
}

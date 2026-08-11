//! OneZeroMiner local HTTP API → MinerPerf.
//!
//! Typical endpoint: `http://127.0.0.1:3010/` (DYNEX_API_PORT).
//!
//! Field names are not fully documented; parse defensively and return `None`
//! when a usable hashrate cannot be found. Confirm exact paths against a live
//! OneZeroMiner instance when available.

use super::TelemetryRecord;
use mining_telemetry_core::{MinerPerfInput, miner_perf};

pub async fn poll(client: &reqwest::Client, endpoint: &str, coin: &str) -> TelemetryRecord {
    TelemetryRecord {
        source: "onezerominer",
        envelope: try_poll(client, endpoint, coin).await,
    }
}

async fn try_poll(
    client: &reqwest::Client,
    endpoint: &str,
    coin: &str,
) -> Option<mining_telemetry_core::TelemetryEnvelope> {
    let base = endpoint.trim_end_matches('/');
    // Prefer `/` then common status paths.
    let mut resp: Option<serde_json::Value> = None;
    for path in ["", "/status", "/api", "/stats"] {
        let url = if path.is_empty() {
            format!("{base}/")
        } else {
            format!("{base}{path}")
        };
        if let Ok(r) = client.get(&url).send().await
            && let Ok(v) = r.json::<serde_json::Value>().await
        {
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

    // Prefer MH/s if the value looks already scaled; otherwise treat as H/s.
    // Without live docs we emit H/s and let operators reconfigure if needed.
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
        // Nested under results/stats.
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

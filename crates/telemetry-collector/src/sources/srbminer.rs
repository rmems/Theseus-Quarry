//! SRBMiner-Multi local HTTP API → MinerPerf.
//!
//! JSON root: `http://127.0.0.1:{MONERO_API_PORT}/` (`--api-enable --api-port`).
//! GUI at `/stats` is ignored. Hashrate is native H/s.
//!
//! Stem: `{coin}_miner_telemetry` (distinct from node `{coin}_telemetry`).
//! Soft-fails (`None`) on XMRig `/1/summary` or other non-SRB JSON.

use super::TelemetryRecord;
use mining_telemetry_core::{MinerPerfInput, miner_perf};

pub async fn poll(client: &reqwest::Client, endpoint: &str, coin: &str) -> TelemetryRecord {
    TelemetryRecord {
        source: "srbminer",
        envelope: try_poll(client, endpoint, coin).await,
    }
}

async fn try_poll(
    client: &reqwest::Client,
    endpoint: &str,
    coin: &str,
) -> Option<mining_telemetry_core::TelemetryEnvelope> {
    let base = endpoint.trim_end_matches('/');
    let url = format!("{base}/");
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

/// Parse SRBMiner-Multi JSON (`GET /`). Returns `None` when the body is not SRB-shaped
/// or no usable hashrate can be found.
fn parse_status(resp: &serde_json::Value) -> Option<ParsedStatus> {
    if !looks_like_srb(resp) {
        return None;
    }

    let algo = pick_algorithm(resp);
    let hashrate_hs = algo
        .and_then(algo_hashrate)
        .or_else(|| finite_f64(resp.get("hashrate_total_now")))?;

    let (shares_accepted, shares_rejected) =
        algo.map(algo_shares).unwrap_or_else(|| root_shares(resp));

    let uptime_seconds = as_u64(resp.get("mining_time")).or_else(|| as_u64(resp.get("uptime")));

    Some(ParsedStatus {
        hashrate_hs,
        shares_accepted,
        shares_rejected,
        uptime_seconds,
    })
}

fn looks_like_srb(resp: &serde_json::Value) -> bool {
    if !resp.is_object() {
        return false;
    }
    if resp.get("algorithms").and_then(|a| a.as_array()).is_some() {
        return true;
    }
    // Older builds expose hashrate_total_now; require another SRB marker so
    // unrelated JSON with that field is not treated as MinerPerf.
    finite_f64(resp.get("hashrate_total_now")).is_some()
        && (resp.get("gpu_devices").is_some()
            || resp.get("rig_name").is_some()
            || resp.get("total_algorithms").is_some())
}

fn pick_algorithm(resp: &serde_json::Value) -> Option<&serde_json::Value> {
    let arr = resp.get("algorithms")?.as_array()?;
    arr.iter()
        .find(|a| is_randomx(a.get("name").and_then(|n| n.as_str())))
        .or_else(|| arr.first())
}

fn is_randomx(name: Option<&str>) -> bool {
    let Some(name) = name else {
        return false;
    };
    let n = name.to_ascii_lowercase();
    n == "randomx" || n == "rx/0" || n == "rx" || n.contains("randomx") || n.contains("monero")
}

/// Prefer windowed numeric fields, then explicit cpu/gpu `total` keys.
/// Never sum a `gpu`/`cpu` object (that double-counts `total` + per-device).
fn algo_hashrate(algo: &serde_json::Value) -> Option<f64> {
    let hr = algo.get("hashrate")?;
    if let Some(n) = finite_f64(hr.get("1min")).or_else(|| finite_f64(hr.get("now"))) {
        return Some(n);
    }
    let cpu = hr.pointer("/cpu/total").and_then(finite_f64_value);
    let gpu = hr.pointer("/gpu/total").and_then(finite_f64_value);
    match (cpu, gpu) {
        (Some(c), Some(g)) => Some(c + g),
        (Some(c), None) => Some(c),
        (None, Some(g)) => Some(g),
        (None, None) => None,
    }
}

fn algo_shares(algo: &serde_json::Value) -> (Option<u64>, Option<u64>) {
    let accepted = as_u64(algo.pointer("/shares/accepted")).or_else(|| {
        algo.get("gpu_accepted_shares")
            .and_then(|g| g.as_object())
            .map(|gpus| gpus.values().filter_map(as_u64_value).sum())
    });
    let rejected = as_u64(algo.pointer("/shares/rejected")).or_else(|| {
        algo.get("gpu_rejected_shares")
            .and_then(|g| g.as_object())
            .map(|gpus| gpus.values().filter_map(as_u64_value).sum())
    });
    (accepted, rejected)
}

fn root_shares(resp: &serde_json::Value) -> (Option<u64>, Option<u64>) {
    (
        as_u64(resp.pointer("/shares/accepted")),
        as_u64(resp.pointer("/shares/rejected")),
    )
}

fn finite_f64(v: Option<&serde_json::Value>) -> Option<f64> {
    v.and_then(finite_f64_value)
}

fn finite_f64_value(v: &serde_json::Value) -> Option<f64> {
    let n = v.as_f64()?;
    n.is_finite().then_some(n)
}

fn as_u64(v: Option<&serde_json::Value>) -> Option<u64> {
    v.and_then(as_u64_value)
}

fn as_u64_value(v: &serde_json::Value) -> Option<u64> {
    v.as_u64().or_else(|| {
        let n = v.as_f64()?;
        if !n.is_finite() || n < 0.0 || n.fract() != 0.0 {
            return None;
        }
        let c = n as u64;
        // Reject values that do not convert losslessly (e.g. 1e30 → u64::MAX).
        (c as f64 == n).then_some(c)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_v3_gpu_total_without_double_count() {
        let v = json!({
            "algorithms": [{
                "name": "autolykos2",
                "hashrate": {
                    "gpu": {"gpu0": 10.0, "total": 42.0}
                },
                "shares": {"accepted": 8, "rejected": 1}
            }],
            "mining_time": 60
        });
        let p = parse_status(&v).unwrap();
        assert!((p.hashrate_hs - 42.0).abs() < 1e-6);
        assert_eq!(p.shares_accepted, Some(8));
        assert_eq!(p.shares_rejected, Some(1));
        assert_eq!(p.uptime_seconds, Some(60));
    }

    #[test]
    fn parses_randomx_cpu_1min() {
        let v = json!({
            "algorithms": [{
                "name": "randomx",
                "hashrate": {
                    "1min": 1234.5,
                    "now": 1200.0,
                    "cpu": {"total": 1234.5},
                    "gpu": {"total": 0.0}
                },
                "shares": {"accepted": 10, "rejected": 2}
            }],
            "uptime": 99
        });
        let p = parse_status(&v).unwrap();
        assert!((p.hashrate_hs - 1234.5).abs() < 1e-6);
        assert_eq!(p.shares_accepted, Some(10));
        assert_eq!(p.shares_rejected, Some(2));
        assert_eq!(p.uptime_seconds, Some(99));
    }

    #[test]
    fn prefers_randomx_among_algorithms() {
        let v = json!({
            "algorithms": [
                {
                    "name": "autolykos2",
                    "hashrate": {"1min": 9_000.0},
                    "shares": {"accepted": 1, "rejected": 0}
                },
                {
                    "name": "randomx",
                    "hashrate": {"1min": 111.0},
                    "shares": {"accepted": 4, "rejected": 1}
                }
            ]
        });
        let p = parse_status(&v).unwrap();
        assert!((p.hashrate_hs - 111.0).abs() < 1e-6);
        assert_eq!(p.shares_accepted, Some(4));
        assert_eq!(p.shares_rejected, Some(1));
    }

    #[test]
    fn sums_cpu_and_gpu_totals_when_windows_absent() {
        let v = json!({
            "algorithms": [{
                "name": "randomx",
                "hashrate": {
                    "cpu": {"thread0": 10.0, "total": 40.0},
                    "gpu": {"gpu0": 5.0, "total": 5.0}
                }
            }]
        });
        let p = parse_status(&v).unwrap();
        assert!((p.hashrate_hs - 45.0).abs() < 1e-6);
    }

    #[test]
    fn falls_back_to_hashrate_total_now() {
        let v = json!({
            "hashrate_total_now": 55.5,
            "gpu_devices": [],
            "mining_time": 3
        });
        let p = parse_status(&v).unwrap();
        assert!((p.hashrate_hs - 55.5).abs() < 1e-6);
        assert_eq!(p.uptime_seconds, Some(3));
    }

    #[test]
    fn rejects_hashrate_total_now_without_srb_marker() {
        assert!(parse_status(&json!({"hashrate_total_now": 55.5, "ok": true})).is_none());
    }

    #[test]
    fn rejects_out_of_range_share_counts() {
        let v = json!({
            "algorithms": [{
                "name": "randomx",
                "hashrate": {"1min": 10.0},
                "shares": {"accepted": 1e30, "rejected": 0.0}
            }]
        });
        let p = parse_status(&v).unwrap();
        assert_eq!(p.shares_accepted, None);
        assert_eq!(p.shares_rejected, Some(0));
    }

    #[test]
    fn rejects_fractional_share_counts() {
        let v = json!({
            "algorithms": [{
                "name": "randomx",
                "hashrate": {"1min": 10.0},
                "shares": {"accepted": 10.7, "rejected": 1.0}
            }]
        });
        let p = parse_status(&v).unwrap();
        assert_eq!(p.shares_accepted, None);
        assert_eq!(p.shares_rejected, Some(1));
    }

    #[test]
    fn rejects_xmrig_summary() {
        let v = json!({
            "hashrate": {"total": [null, 1234.5, 1000.0], "highest": 2000.0},
            "results": {"shares_good": 10, "shares_total": 12},
            "uptime": 99
        });
        assert!(parse_status(&v).is_none());
    }

    #[test]
    fn rejects_unrelated_json() {
        assert!(parse_status(&json!({"error": "not found", "code": 404})).is_none());
    }
}

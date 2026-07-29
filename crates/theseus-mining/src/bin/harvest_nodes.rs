//! harvest_nodes — Sync-Data → NeuromorphicSnapshot JSONL
//!
//! Parses Kaspa and Monero node logs into the NeuromorphicSnapshot format
//! that train_snn already understands.  Outputs one JSON record per log line
//! that carries a meaningful signal (block ingestion rate, sync progress).
//!
//! Usage:
//!   cargo run -p neuro-spike-core --bin harvest_nodes -- \
//!       --kaspa  mining/nodes/kaspa/logs/rusty-kaspa.log \
//!       --monero mining/nodes/xmr/chain/bitmonero.log \
//!       --out    research/node_sync_harvest.jsonl

use clap::Parser;
use serde::Serialize;
use std::{
    fs::File,
    io::{BufRead, BufReader, BufWriter, Write},
    path::PathBuf,
};

#[derive(Parser)]
struct Args {
    #[arg(
        long,
        default_value = "binaries/mining/nodes/kaspa/logs/rusty-kaspa.log"
    )]
    kaspa: PathBuf,
    #[arg(long, default_value = "binaries/mining/nodes/xmr/chain/bitmonero.log")]
    monero: PathBuf,
    #[arg(long, default_value = "data/telemetry/node_sync_harvest.jsonl")]
    out: PathBuf,
}

/// Expand a leading `~/` using `$HOME` (std::fs does not shell-expand tildes).
fn expand_tilde(path: PathBuf) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    if s == "~"
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home);
    }
    path
}

// Mirrors the fields train_snn actually reads from NeuromorphicSnapshot.
// We only populate what we can derive from log lines; everything else is 0.
#[derive(Serialize)]
struct Snapshot {
    timestamp: String,
    telemetry: Telemetry,
}

#[derive(Serialize, Default)]
struct Telemetry {
    // ch7  — hashrate proxy: block ingestion rate (blocks/s), normalised to [0,1]
    hashrate_mh: f32,
    // ch13 — power proxy: sync progress [0,1] (higher = more work done)
    power_w: f32,
    // ch14 — temp proxy: blocks remaining normalised [0,1] (stress signal)
    gpu_temp_c: f32,
    // ch10 — qubic tick trace: reused for "new block arrived" pulse
    qubic_tick_trace: f32,
    // ch11 — epoch progress: overall chain sync fraction [0,1]
    qubic_epoch_progress: f32,
    // reward hint for E-prop: rises as sync completes
    #[serde(skip_serializing_if = "Option::is_none")]
    reward_hint: Option<f32>,
    // passthrough defaults so the engine doesn't choke
    vddcr_gfx_v: f32,
    vram_temp_c: f32,
    gpu_clock_mhz: f32,
    mem_clock_mhz: f32,
    fan_speed_pct: f32,
    rejected_shares: u32,
    mem_util_pct: f32,
    ocean_intel: f32,
    power_z_score: f32,
    temp_z_score: f32,
    clock_z_score: f32,
    clock_mhz: f32,
    qubic_tick_rate: f32,
    qu_price_usd: f32,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let out_path = expand_tilde(args.out);
    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let out_file = File::create(&out_path)?;
    let mut writer = BufWriter::new(out_file);
    let mut total = 0usize;

    // ── Monero ────────────────────────────────────────────────────────
    // Log line: "Synced 3198616/3634345 (88%, 435729 left)"
    if args.monero.exists() {
        let reader = BufReader::new(File::open(&args.monero)?);
        let mut prev_height = 0u64;
        let mut prev_ts_secs = 0f64;

        for line_result in reader.lines() {
            let line = match line_result {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[harvest] monero line read error: {e}");
                    continue;
                }
            };
            if !line.contains("Synced ") {
                continue;
            }

            let (current, total_blocks, ts_secs) = match parse_monero_line(&line) {
                Some(v) => v,
                None => continue,
            };

            let sync_frac = current as f32 / total_blocks.max(1) as f32;
            let remaining_frac = 1.0 - sync_frac;

            // blocks/s between consecutive log lines
            let dt = (ts_secs - prev_ts_secs).max(0.001);
            let blk_delta = current.saturating_sub(prev_height) as f32;
            // normalise: ~20 blocks/s is fast IBD for Monero
            let ingestion_rate = (blk_delta / dt as f32 / 20.0).clamp(0.0, 1.0);

            let snap = Snapshot {
                timestamp: line[..23].trim().to_string(),
                telemetry: Telemetry {
                    hashrate_mh: ingestion_rate,
                    power_w: sync_frac * 400.0, // map to watts range engine expects
                    gpu_temp_c: 40.0 + remaining_frac * 40.0, // stress rises when far from done
                    qubic_tick_trace: if blk_delta > 0.0 { 1.0 } else { 0.0 },
                    qubic_epoch_progress: sync_frac,
                    reward_hint: Some(sync_frac),
                    vddcr_gfx_v: 0.85,
                    fan_speed_pct: 30.0,
                    gpu_clock_mhz: 210.0,
                    mem_clock_mhz: 405.0,
                    clock_mhz: 210.0,
                    ..Default::default()
                },
            };

            writeln!(writer, "{}", serde_json::to_string(&snap)?)?;
            total += 1;
            prev_height = current;
            prev_ts_secs = ts_secs;
        }
        eprintln!("[harvest] Monero: {} records", total);
    } else {
        eprintln!("[harvest] Monero log not found: {}", args.monero.display());
    }

    // ── Kaspa ─────────────────────────────────────────────────────────
    // Log line: "Processed 0 blocks and 0 headers in the last 10.00s
    //            (0 transactions; 0 UTXO-validated blocks; 0.00 parents;
    //             0.00 mergeset; 0.00 TPB; 0.0 mass)"
    let kaspa_start = total;
    if args.kaspa.exists() {
        let reader = BufReader::new(File::open(&args.kaspa)?);

        for line_result in reader.lines() {
            let line = match line_result {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[harvest] kaspa line read error: {e}");
                    continue;
                }
            };
            if !line.contains("Processed") || !line.contains("blocks") {
                continue;
            }

            let (blocks, headers, interval_s) = match parse_kaspa_line(&line) {
                Some(v) => v,
                None => continue,
            };

            // blocks/s normalised: Kaspa DAG can do ~1 block/s sustained
            let blk_rate = (blocks as f32 / interval_s.max(0.001) / 1.0).clamp(0.0, 1.0);
            let hdr_rate = (headers as f32 / interval_s.max(0.001) / 10.0).clamp(0.0, 1.0);
            // Use header rate as sync-progress proxy (headers arrive before blocks in IBD)
            let sync_proxy = hdr_rate.max(blk_rate);

            let snap = Snapshot {
                timestamp: line[..29].trim().to_string(),
                telemetry: Telemetry {
                    hashrate_mh: blk_rate,
                    power_w: sync_proxy * 400.0,
                    gpu_temp_c: 40.0 + (1.0 - sync_proxy) * 40.0,
                    qubic_tick_trace: if blocks > 0 { 1.0 } else { 0.0 },
                    qubic_epoch_progress: sync_proxy,
                    reward_hint: Some(sync_proxy),
                    vddcr_gfx_v: 0.85,
                    fan_speed_pct: 30.0,
                    gpu_clock_mhz: 210.0,
                    mem_clock_mhz: 405.0,
                    clock_mhz: 210.0,
                    ..Default::default()
                },
            };

            writeln!(writer, "{}", serde_json::to_string(&snap)?)?;
            total += 1;
        }
        eprintln!("[harvest] Kaspa: {} records", total - kaspa_start);
    } else {
        eprintln!("[harvest] Kaspa log not found: {}", args.kaspa.display());
    }

    writer.flush()?;
    println!("Harvested {} total records → {}", total, out_path.display());
    Ok(())
}

// ── Parsers ───────────────────────────────────────────────────────────

/// Returns (current_height, total_height, timestamp_as_secs_f64)
fn parse_monero_line(line: &str) -> Option<(u64, u64, f64)> {
    // "2026-03-20 12:51:46.179\t...\tSynced 3198616/3634345 ..."
    let synced_pos = line.find("Synced ")?;
    let rest = &line[synced_pos + 7..];
    let slash = rest.find('/')?;
    let space = rest[slash..].find(' ')?;
    let current: u64 = rest[..slash].trim().parse().ok()?;
    let total: u64 = rest[slash + 1..slash + space].trim().parse().ok()?;

    // Parse timestamp from start of line: "2026-03-20 12:51:46.179"
    let ts = parse_timestamp_secs(&line[..23]);
    Some((current, total, ts))
}

/// Returns (blocks_processed, headers_processed, interval_seconds)
fn parse_kaspa_line(line: &str) -> Option<(u64, u64, f32)> {
    // "Processed 0 blocks and 0 headers in the last 10.00s ..."
    let proc_pos = line.find("Processed ")?;
    let rest = &line[proc_pos + 10..];

    let blk_end = rest.find(" blocks")?;
    let blocks: u64 = rest[..blk_end].trim().parse().ok()?;

    let hdr_start = rest.find("and ")? + 4;
    let hdr_end = rest[hdr_start..].find(" headers")? + hdr_start;
    let headers: u64 = rest[hdr_start..hdr_end].trim().parse().ok()?;

    let last_pos = rest.find("last ")? + 5;
    let s_pos = rest[last_pos..].find('s')? + last_pos;
    let interval: f32 = rest[last_pos..s_pos].trim().parse().ok()?;

    Some((blocks, headers, interval))
}

/// Very lightweight ISO-ish timestamp → seconds-since-midnight (good enough for delta).
fn parse_timestamp_secs(s: &str) -> f64 {
    // "2026-03-20 12:51:46.179"
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() < 2 {
        return 0.0;
    }
    let time_parts: Vec<&str> = parts[1].split(':').collect();
    if time_parts.len() < 3 {
        return 0.0;
    }
    let h: f64 = time_parts[0].parse().unwrap_or(0.0);
    let m: f64 = time_parts[1].parse().unwrap_or(0.0);
    let s: f64 = time_parts[2].parse().unwrap_or(0.0);
    h * 3600.0 + m * 60.0 + s
}

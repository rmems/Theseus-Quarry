//! Eagle-Lander — Multi-Algorithm Mining Supervisor
//!
//! Controls Dynex, Quai, Qubic, Kaspa, Monero, and Verus mining with independent lifecycle management.
//!
//! # Examples
//!
//! ```bash
//! # Dynex only (default)
//! cargo run -p theseus-mining -- --algo dynex --wallet DNX123...
//!
//! # Quai only (CPU mining)
//! cargo run -p theseus-mining -- --algo quai
//!
//! # All miners simultaneously
//! cargo run -p theseus-mining -- --algo all
//! ```

use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use tracing::{Level, info};

use std::sync::{Arc, Mutex};
use std::time::Instant;

use mining_telemetry_core::{JsonlSink, SCHEMA_VERSION, WireMsg, envelopes_from_wire};
use theseus_mining::gpu_scheduler::GpuSchedulerConfig;
use theseus_mining::mining_supervisor::{MiningAlgo, UnifiedMining};
use theseus_mining::rotation::{MarketSnapshot, RotationConfig};

// ─── CLI ─────────────────────────────────────────────────────────────────────

/// Eagle-Lander — Multi-Algorithm Mining Supervisor
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Which algorithm(s) to mine.
    ///
    /// Accepted values:
    ///   dynex    — Dynex GPU mining via onezerominer
    ///   quai     — Quai Network mining via rigel/go-quai-stratum
    ///   qubic    — Qubic mining via qubic-core/podman-compose
    ///   kaspa    — Kaspa GPU mining via bzminer
    ///   all      — All miners
    ///   both     — Dynex + Quai
    ///   Comma-separated combos (e.g. "dynex,qubic") are also accepted.
    #[arg(long, default_value = "dynex")]
    algo: String,

    /// Dynex wallet address (falls back to SHIP_WALLET env var).
    #[arg(long, env = "SHIP_WALLET")]
    wallet: Option<String>,

    /// Dynex stratum pool address.
    #[arg(long, default_value = "stratum+tcp://us3.dynex.herominers.com:1120")]
    dynex_pool: String,

    /// Quai wallet address (falls back to QUAI_WALLET_ADDRESS env var).
    #[arg(long, env = "QUAI_WALLET_ADDRESS")]
    quai_wallet: Option<String>,

    /// Quai stratum pool address (falls back to QUAI_POOL env var).
    #[arg(long, env = "QUAI_POOL", default_value = "stratum.quai.network:3333")]
    quai_pool: String,

    /// CPU threads for Quai mining (falls back to QUAI_MINING_THREADS).
    #[arg(long, env = "QUAI_MINING_THREADS", default_value_t = 4)]
    quai_threads: u32,

    /// CPU threads for Qubic mining.
    #[arg(long, env = "QUBIC_MINING_THREADS", default_value_t = 4)]
    qubic_threads: u32,

    /// GPU device ordinal for Dynex (default: 0).
    #[arg(short, long, default_value_t = 0)]
    device: u32,

    /// GPU temperature (°C) at which mining is throttled.
    #[arg(long, default_value_t = 80.0)]
    thermal_throttle: f32,

    /// GPU temperature (°C) at which mining is killed (emergency).
    #[arg(long, default_value_t = 90.0)]
    thermal_emergency: f32,

    /// GPU scheduler poll interval in seconds.
    #[arg(long, default_value_t = 2)]
    gpu_poll_secs: u64,

    /// Enable automatic algorithm rotation based on profitability.
    #[arg(long, default_value_t = false)]
    auto_rotate: bool,

    /// Minimum seconds between algorithm rotations.
    #[arg(long, default_value_t = 300)]
    rotation_cooldown: u64,

    /// Percentage improvement required to trigger a rotation.
    #[arg(long, default_value_t = 20.0)]
    rotation_threshold: f64,

    /// VRAM (MB) to reserve for SNN brain, reducing mining ceiling.
    #[arg(long, env = "VRAM_RESERVED_FOR_SNN", default_value_t = 4096)]
    vram_reserved_for_snn: u64,

    /// Directory for schema v1 multi-stem JSONL (dual-write with collector).
    /// Empty / unset disables disk telemetry from the supervisor.
    #[arg(long, env = "TELEMETRY_DATA_DIR", default_value = "./data/telemetry")]
    telemetry_data_dir: String,

    /// When true (default), append WireMsg envelopes to TELEMETRY_DATA_DIR.
    #[arg(long, env = "TELEMETRY_JSONL", default_value_t = true)]
    telemetry_jsonl: bool,
}

fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    let args = Args::parse();

    let algo: MiningAlgo = args
        .algo
        .parse()
        .map_err(|e: String| anyhow::anyhow!("{e}"))?;

    if let Some(ref w) = args.wallet {
        // SAFETY: called before spawning threads
        unsafe {
            std::env::set_var("SHIP_WALLET", w);
        }
    }
    if let Some(ref w) = args.quai_wallet {
        unsafe {
            std::env::set_var("QUAI_WALLET_ADDRESS", w);
        }
    }
    unsafe {
        std::env::set_var("QUAI_POOL", &args.quai_pool);
        std::env::set_var("QUAI_MINING_THREADS", args.quai_threads.to_string());
        std::env::set_var("QUBIC_MINING_THREADS", args.qubic_threads.to_string());
    }

    println!("╔══════════════════════════════════════════════════╗");
    println!("║   Eagle-Lander — Mining Supervisor v3          ║");
    println!("╠══════════════════════════════════════════════════╣");
    println!("║  Algo      : {:<36} ║", algo);
    if algo.runs_dynex() {
        let w = std::env::var("SHIP_WALLET").unwrap_or_else(|_| "NOT SET".into());
        println!("║  Dynex wallet : {:<32} ║", truncate(&w, 32));
        println!("║  Dynex pool   : {:<32} ║", truncate(&args.dynex_pool, 32));
        println!("║  GPU device   : {:<32} ║", args.device);
    }
    if algo.runs_quai() {
        let w = std::env::var("QUAI_WALLET_ADDRESS").unwrap_or_else(|_| "NOT SET".into());
        println!("║  Quai wallet  : {:<32} ║", truncate(&w, 32));
        println!("║  Quai pool    : {:<32} ║", truncate(&args.quai_pool, 32));
        println!("║  Quai threads : {:<32} ║", args.quai_threads);
    }
    if algo.runs_qubic() {
        println!("║  Qubic wallet : {:<32} ║", truncate("...", 32));
        println!("║  Qubic threads: {:<32} ║", args.qubic_threads);
    }
    if algo.runs_kaspa() {
        println!("║  Kaspa        : enabled                      ║");
    }
    println!("╚══════════════════════════════════════════════════╝");

    if algo.runs_dynex() && std::env::var("SHIP_WALLET").is_err() {
        eprintln!(
            "WARNING: SHIP_WALLET not set — Dynex will run in mock mode.\n\
             Pass --wallet <addr> or set SHIP_WALLET in .env."
        );
    }

    let (telem_tx, telem_rx) = mpsc::channel::<WireMsg>();

    let telem_dir = args.telemetry_data_dir.trim().to_string();
    let telem_jsonl = args.telemetry_jsonl && !telem_dir.is_empty();
    if telem_jsonl {
        info!(
            data_dir = %telem_dir,
            schema_version = SCHEMA_VERSION,
            "supervisor JSONL telemetry enabled"
        );
    }

    std::thread::Builder::new()
        .name("telem-logger".into())
        .spawn(move || {
            let sink = if telem_jsonl {
                Some(JsonlSink::new(&telem_dir))
            } else {
                None
            };
            for msg in telem_rx {
                if let Some(ref sink) = sink {
                    let envs = envelopes_from_wire("supervisor", &msg);
                    if let Err(e) = sink.write_all(&envs) {
                        tracing::warn!(error = %e, "telemetry JSONL write failed");
                    }
                }
                match msg {
                    WireMsg::Status(s) => info!("[status] {s}"),
                    WireMsg::MiningTelem(t) => {
                        let d = &t.stats.dynex;
                        let q = &t.stats.quai;
                        let qb = &t.stats.qubic;
                        let k = &t.stats.kaspa;
                        let m = &t.stats.monero;
                        let v = &t.stats.verus;
                        info!(
                            "[mining] dynex={:.3}MH/s quai={:.3}MH/s qubic={:.0}kH/s kaspa={:.3}MH/s monero={:.0}h/s verus={:.0}h/s",
                            d.hashrate_mh_s,
                            q.hashrate_mh_s,
                            qb.hashrate_kh_s,
                            k.hashrate_mh_s,
                            m.hashrate_h_s,
                            v.hashrate_h_s
                        );
                    }
                    WireMsg::GpuSchedulerEvent(e) => {
                        info!("[gpu-sched] decision={} vram={}/{}MB temp={:.1}°C",
                            e.decision, e.vram_used_mb, e.vram_total_mb, e.gpu_temp_c);
                    }
                    WireMsg::RotationEvent(e) => {
                        info!("[rotation] kind={} from={:?} to={:?} market_age={:.0}s",
                            e.kind, e.from_algo, e.to_algo, e.market_age_secs);
                    }
                }
            }
        })
        .expect("telem logger thread");

    let sched_config = GpuSchedulerConfig {
        thermal_throttle_c: args.thermal_throttle,
        thermal_emergency_c: args.thermal_emergency,
        vram_reserved_mb: args.vram_reserved_for_snn,
        ..GpuSchedulerConfig::default()
    };

    let mut um = UnifiedMining::with_scheduler_config(algo, telem_tx, sched_config);
    um.start();

    let auto_rotate = args.auto_rotate;
    if auto_rotate {
        let rot_config = RotationConfig {
            cooldown: Duration::from_secs(args.rotation_cooldown),
            improvement_threshold_pct: args.rotation_threshold,
            ..RotationConfig::default()
        };
        um.enable_rotation(rot_config);
        info!(
            "Auto-rotation enabled (cooldown={}s, threshold={:.0}%)",
            args.rotation_cooldown, args.rotation_threshold
        );
    }

    let market = Arc::new(Mutex::new(MarketSnapshot::default()));
    let market_last_fetch = Arc::new(Mutex::new(Instant::now()));

    if auto_rotate {
        let market_clone = Arc::clone(&market);
        let market_fetch_clone = Arc::clone(&market_last_fetch);
        std::thread::Builder::new()
            .name("market-poller".into())
            .spawn(move || {
                market_poller_loop(market_clone, market_fetch_clone);
            })
            .expect("market poller thread");
    }

    info!("Miners running. Press Ctrl+C to stop.");

    let poll_interval = Duration::from_secs(args.gpu_poll_secs);

    setup_signal_flag();
    loop {
        if signal_received() {
            break;
        }
        um.poll_gpu();

        if auto_rotate {
            let mut snap = market.lock().unwrap_or_else(|p| p.into_inner()).clone();
            let age = market_last_fetch
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .elapsed();
            snap.price_age = age;
            snap.gpu_available = um
                .scheduler()
                .map(|s| !s.current_decision().is_paused())
                .unwrap_or(true);
            um.check_rotation(&snap);
        }
        std::thread::sleep(poll_interval);
    }

    info!("Shutdown signal — stopping miners...");
    um.save_state();
    um.stop();
    info!("All miners stopped. Goodbye.");

    Ok(())
}

fn market_poller_loop(market: Arc<Mutex<MarketSnapshot>>, last_fetch: Arc<Mutex<Instant>>) {
    loop {
        match fetch_market_prices() {
            Ok((dnx, qu)) => {
                if let Ok(mut snap) = market.lock() {
                    snap.dnx_price_usd = dnx;
                    snap.qu_price_usd = qu;
                }
                if let Ok(mut t) = last_fetch.lock() {
                    *t = Instant::now();
                }
            }
            Err(e) => {
                eprintln!("[market-poller] fetch failed: {e}");
            }
        }
        std::thread::sleep(Duration::from_secs(60));
    }
}

fn fetch_market_prices() -> Result<(f64, f64)> {
    let url =
        "https://api.coingecko.com/api/v3/simple/price?ids=dynex,qubic-network&vs_currencies=usd";
    let body = reqwest::blocking::get(url)?.text()?;

    let dnx = extract_price(&body, "dynex").unwrap_or(0.0);
    let qu = extract_price(&body, "qubic-network").unwrap_or(0.0);

    Ok((dnx, qu))
}

fn extract_price(json: &str, coin: &str) -> Option<f64> {
    let key = format!("\"{}\"", coin);
    let start = json.find(&key)?;
    let rest = &json[start..];
    let usd_key = "\"usd\":";
    let usd_pos = rest.find(usd_key)?;
    let num_start = usd_pos + usd_key.len();
    let num_rest = &rest[num_start..];
    let end = num_rest
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .unwrap_or(num_rest.len());
    num_rest[..end].parse::<f64>().ok()
}

use std::sync::atomic::{AtomicBool, Ordering};

static SHUTDOWN: AtomicBool = AtomicBool::new(false);

fn setup_signal_flag() {
    #[cfg(unix)]
    {
        use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

        extern "C" fn handler(_: nix::libc::c_int) {
            SHUTDOWN.store(true, Ordering::SeqCst);
        }

        let action = SigAction::new(
            SigHandler::Handler(handler),
            SaFlags::empty(),
            SigSet::empty(),
        );
        unsafe {
            let _ = sigaction(Signal::SIGINT, &action);
            let _ = sigaction(Signal::SIGTERM, &action);
        }
    }
}

fn signal_received() -> bool {
    SHUTDOWN.load(Ordering::SeqCst)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max.saturating_sub(1)])
    }
}

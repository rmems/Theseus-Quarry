mod gpu_scheduler;
mod process_governor;
mod sources;
mod writer;

use clap::Parser;
use std::time::Duration;
use tracing::{debug, info, warn};

#[derive(Parser)]
#[command(
    name = "telemetry-collector",
    about = "Polls mining nodes and system sensors, writes schema v1 JSONL telemetry"
)]
struct Args {
    /// Output directory for multi-stem JSONL files.
    #[arg(long, env = "TELEMETRY_DATA_DIR", default_value = "./data/telemetry")]
    data_dir: String,

    /// Poll interval in seconds.
    #[arg(long, env = "TELEMETRY_INTERVAL", default_value_t = 5)]
    interval: u64,

    /// GPU temperature (°C) at which mining is throttled.
    #[arg(long, env = "THERMAL_THROTTLE_C", default_value_t = 80.0)]
    thermal_throttle: f32,

    /// GPU temperature (°C) at which the scheduler reports an emergency pause.
    #[arg(long, env = "THERMAL_EMERGENCY_C", default_value_t = 90.0)]
    thermal_emergency: f32,

    /// Kaspa Miner local API port
    #[arg(long, env = "KASPA_API_PORT", default_value_t = 4014)]
    kaspa_api_port: u16,

    /// Dynex Node RPC URL
    #[arg(
        long,
        env = "DYNEX_NODE_RPC_URL",
        default_value = "http://127.0.0.1:17336"
    )]
    dynex_node_rpc_url: String,

    /// Monero Node RPC URL
    #[arg(
        long,
        env = "MONERO_NODE_RPC_URL",
        default_value = "http://127.0.0.1:18081/json_rpc"
    )]
    monero_node_rpc_url: String,

    /// Quai Node RPC URL
    #[arg(
        long,
        env = "QUAI_NODE_RPC_URL",
        default_value = "http://127.0.0.1:9001"
    )]
    quai_node_rpc_url: String,

    /// Qubic Node API URL
    #[arg(
        long,
        env = "QUBIC_NODE_API_URL",
        default_value = "http://127.0.0.1:8099"
    )]
    qubic_node_api_url: String,

    /// Comma-separated list of miner executable basenames to govern.
    #[arg(
        long,
        env = "GOVERNED_MINERS",
        value_delimiter = ',',
        default_value = "bzminer,xmrig,onezerominer,rigel,qli-Client,hellminer,SRBMiner-MULTI"
    )]
    governed_miners: Vec<String>,
}

async fn flush_record(w: &mut writer::JsonlWriter, rec: sources::TelemetryRecord) {
    if let Some(env) = rec.envelope {
        let stem = env.stem.clone();
        if let Err(e) = w.write_envelope(&env).await {
            warn!(source = rec.source, error = %e, "write failed");
        } else {
            debug!(source = rec.source, %stem, "wrote envelope");
        }
    } else {
        debug!(source = rec.source, "source unavailable, skipping");
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    anyhow::ensure!(
        args.thermal_throttle.is_finite() && args.thermal_emergency.is_finite(),
        "Thermal thresholds must be finite numbers"
    );
    anyhow::ensure!(
        args.thermal_throttle >= 0.0 && args.thermal_throttle < args.thermal_emergency,
        "Require 0.0 <= thermal_throttle < thermal_emergency, got {} and {}",
        args.thermal_throttle,
        args.thermal_emergency
    );

    let data_dir = shellexpand::tilde(&args.data_dir).into_owned();
    info!(
        data_dir = %data_dir,
        interval_secs = args.interval,
        schema_version = mining_telemetry_core::SCHEMA_VERSION,
        "telemetry-collector starting"
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(2000))
        .build()?;
    let mut w = writer::JsonlWriter::new(&data_dir);
    let rapl = sources::rapl::RaplState::new();
    let mut gpu_sched = gpu_scheduler::GpuScheduler::new(gpu_scheduler::GpuSchedulerConfig {
        thermal_throttle_c: args.thermal_throttle,
        thermal_emergency_c: args.thermal_emergency,
        ..Default::default()
    });
    let tick = Duration::from_secs(args.interval);

    let kaspa_endpoint = format!("http://127.0.0.1:{}/", args.kaspa_api_port);
    let dynex_endpoint = format!(
        "{}/getheight",
        args.dynex_node_rpc_url.trim_end_matches('/')
    );
    let qubic_endpoint = format!(
        "{}/tick-info",
        args.qubic_node_api_url.trim_end_matches('/')
    );

    let mut governor = process_governor::ProcessGovernor::new(args.governed_miners);

    // Initial GPU safety check before startup recovery
    let (decision, event) = gpu_sched.poll();
    let mut currently_paused = decision.is_paused();

    if currently_paused {
        warn!("Initial state is thermal emergency / VRAM pressure. Suspending known miners...");
        governor.is_emergency = true;
        governor.suspend_miners();
    } else {
        info!("Initial state is below the emergency pause threshold. Resuming all known miners to clear stale SIGSTOPs...");
        governor.resume_all_known_miners();
    }

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("Failed to create SIGTERM listener");

    if let Some(event) = event {
        let env = mining_telemetry_core::envelope_from_gpu_sched("collector", &event);
        if let Err(e) = w.write_envelope(&env).await {
            warn!(source = "collector", error = %e, "gpu_sched write failed");
        } else {
            debug!(source = "collector", stem = %env.stem, "wrote gpu_sched envelope");
        }
    }

    loop {
        let (decision, gpu_event) = gpu_sched.poll();
        match decision {
            gpu_scheduler::GpuDecision::MiningPaused(ref reason) => {
                warn!(?reason, "GPU mining safety limit exceeded");
            }
            gpu_scheduler::GpuDecision::MiningThrottled { temp_c } => {
                warn!(temp_c, "GPU mining thermal throttle active");
            }
            gpu_scheduler::GpuDecision::MiningAllowed => {}
        }

        let should_pause = decision.is_paused();
        governor.is_emergency = should_pause;
        if should_pause {
            if !currently_paused {
                warn!("Thermal emergency / VRAM pressure breached: Suspending miners...");
                currently_paused = true;
            }
            governor.suspend_miners();
        } else if !should_pause && currently_paused {
            info!("Thermal levels returned to normal: Resuming miners...");
            if governor.resume_miners() {
                currently_paused = false;
            } else {
                warn!("Some miners failed to resume; will retry next tick.");
            }
        }

        if let Some(event) = gpu_event {
            let env = mining_telemetry_core::envelope_from_gpu_sched("collector", &event);
            if let Err(e) = w.write_envelope(&env).await {
                warn!(source = "collector", error = %e, "gpu_sched write failed");
            } else {
                debug!(source = "collector", stem = %env.stem, "wrote gpu_sched envelope");
            }
        }

        let (monero_rec, dynex_rec, quai_rec, qubic_rec, kaspa_rec) = tokio::join!(
            sources::monero::poll(&client, &args.monero_node_rpc_url),
            sources::dynex::poll(&client, &dynex_endpoint),
            sources::quai::poll(&client, &args.quai_node_rpc_url),
            sources::qubic::poll(&client, &qubic_endpoint),
            sources::kaspa::poll(&client, &kaspa_endpoint),
        );
        flush_record(&mut w, monero_rec).await;
        flush_record(&mut w, dynex_rec).await;
        flush_record(&mut w, quai_rec).await;
        flush_record(&mut w, qubic_rec).await;
        flush_record(&mut w, kaspa_rec).await;
        flush_record(&mut w, sources::hwmon::poll()).await;
        flush_record(&mut w, rapl.poll()).await;

        #[cfg(not(unix))]
        let sigterm_fut = std::future::pending::<()>();
        
        #[cfg(unix)]
        let sigterm_fut = sigterm.recv();

        tokio::select! {
            _ = tokio::time::sleep(tick) => {}
            _ = tokio::signal::ctrl_c() => {
                info!("Received SIGINT, shutting down...");
                break;
            }
            _ = sigterm_fut => {
                info!("Received SIGTERM, shutting down...");
                break;
            }
        }
    }
    Ok(())
}

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
    ///
    /// Sequential HTTP probes (OneZeroMiner, Monero node RPC pair, Quai RPC fallbacks)
    /// share a per-tick budget of `interval - 500ms` slack; each request is capped by
    /// the client timeout (2s). See `sources::http_poll`.
    #[arg(long, env = "TELEMETRY_INTERVAL", default_value_t = 5)]
    interval: u64,

    /// GPU temperature (°C) at which mining is throttled.
    #[arg(long, env = "THERMAL_THROTTLE_C", default_value_t = 80.0)]
    thermal_throttle: f32,

    /// GPU temperature (°C) at which the scheduler reports an emergency pause.
    #[arg(long, env = "THERMAL_EMERGENCY_C", default_value_t = 90.0)]
    thermal_emergency: f32,

    /// BzMiner local HTTP API port (MinerPerf for Kaspa / multi-algo).
    #[arg(long, env = "KASPA_API_PORT", default_value_t = 4014)]
    kaspa_api_port: u16,

    /// OneZeroMiner local HTTP API port (MinerPerf for Dynex).
    #[arg(long, env = "DYNEX_API_PORT", default_value_t = 3010)]
    dynex_api_port: u16,

    /// XMRig / SRBMiner-Multi local HTTP API port (MinerPerf for Monero).
    #[arg(long, env = "MONERO_API_PORT", default_value_t = 4015)]
    monero_api_port: u16,

    /// Which Monero miner HTTP adapter to use on MONERO_API_PORT.
    #[arg(long, env = "MONERO_MINER", value_enum, default_value_t = sources::monero_miner::MoneroMinerKind::Auto)]
    monero_miner: sources::monero_miner::MoneroMinerKind,

    /// Dynex Node RPC URL (loopback recommended; see `NODE_RPC_LOOPBACK_ONLY`).
    #[arg(
        long,
        env = "DYNEX_NODE_RPC_URL",
        default_value = "http://127.0.0.1:17336"
    )]
    dynex_node_rpc_url: String,

    /// Monero Node RPC URL (loopback recommended; see `NODE_RPC_LOOPBACK_ONLY`).
    #[arg(
        long,
        env = "MONERO_NODE_RPC_URL",
        default_value = "http://127.0.0.1:18081/json_rpc"
    )]
    monero_node_rpc_url: String,

    /// Quai Node RPC URL (loopback recommended; see `NODE_RPC_LOOPBACK_ONLY`).
    #[arg(
        long,
        env = "QUAI_NODE_RPC_URL",
        default_value = "http://127.0.0.1:9001"
    )]
    quai_node_rpc_url: String,

    /// Qubic Node API URL (loopback recommended; see `NODE_RPC_LOOPBACK_ONLY`).
    #[arg(
        long,
        env = "QUBIC_NODE_API_URL",
        default_value = "http://127.0.0.1:8099"
    )]
    qubic_node_api_url: String,

    /// When true (default), node RPC/API URLs must target loopback (`127.0.0.1`, `localhost`, or `::1`).
    #[arg(long, env = "NODE_RPC_LOOPBACK_ONLY", default_value_t = true)]
    node_rpc_loopback_only: bool,

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

fn validate_thermal_args(args: &Args) -> anyhow::Result<()> {
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
    Ok(())
}

struct HttpEndpoints {
    bzminer: String,
    onezerominer: String,
    xmrig: String,
    dynex: String,
    qubic: String,
}

impl HttpEndpoints {
    fn from_args(args: &Args) -> Self {
        Self {
            bzminer: format!("http://127.0.0.1:{}/", args.kaspa_api_port),
            onezerominer: format!("http://127.0.0.1:{}/", args.dynex_api_port),
            xmrig: format!("http://127.0.0.1:{}/", args.monero_api_port),
            dynex: format!(
                "{}/getheight",
                args.dynex_node_rpc_url.trim_end_matches('/')
            ),
            qubic: format!(
                "{}/tick-info",
                args.qubic_node_api_url.trim_end_matches('/')
            ),
        }
    }
}

async fn write_gpu_sched_event(
    w: &mut writer::JsonlWriter,
    event: &mining_telemetry_core::GpuSchedulerEvent,
) {
    let env = mining_telemetry_core::envelope_from_gpu_sched("collector", event);
    if let Err(e) = w.write_envelope(&env).await {
        warn!(source = "collector", error = %e, "gpu_sched write failed");
    } else {
        debug!(source = "collector", stem = %env.stem, "wrote gpu_sched envelope");
    }
}

fn log_gpu_decision(decision: &gpu_scheduler::GpuDecision) {
    match decision {
        gpu_scheduler::GpuDecision::MiningPaused(reason) => {
            warn!(?reason, "GPU mining safety limit exceeded");
        }
        gpu_scheduler::GpuDecision::MiningThrottled { temp_c } => {
            warn!(temp_c, "GPU mining thermal throttle active");
        }
        gpu_scheduler::GpuDecision::MiningAllowed => {}
    }
}

fn sync_miner_governor(
    governor: &mut process_governor::ProcessGovernor,
    should_pause: bool,
    currently_paused: &mut bool,
) {
    governor.is_emergency = should_pause;
    if should_pause {
        if !*currently_paused {
            warn!("Thermal emergency / VRAM pressure breached: Suspending miners...");
            *currently_paused = true;
        }
        governor.suspend_miners();
        return;
    }
    if !*currently_paused && !governor.has_pending_resumes() {
        return;
    }
    if *currently_paused {
        info!("GPU safety limits cleared (thermal/VRAM): Resuming miners...");
    }
    if governor.resume_miners() {
        *currently_paused = false;
    } else {
        warn!("Some miners failed to resume; will retry next tick.");
    }
}

fn apply_startup_gpu_state(
    governor: &mut process_governor::ProcessGovernor,
    decision: &gpu_scheduler::GpuDecision,
) {
    if decision.is_paused() {
        warn!("Initial state is thermal emergency / VRAM pressure. Suspending known miners...");
        governor.is_emergency = true;
        governor.suspend_miners();
        return;
    }
    info!(
        "Initial state is below the emergency pause threshold. Resuming all known miners to clear stale SIGSTOPs..."
    );
    if !governor.resume_all_known_miners() {
        warn!("Some miners failed to resume on startup; will retry next tick.");
    }
}

struct PollTickContext<'a> {
    writer: &'a mut writer::JsonlWriter,
    client: &'a reqwest::Client,
    args: &'a Args,
    endpoints: &'a HttpEndpoints,
    rapl: &'a sources::rapl::RaplState,
}

async fn poll_and_flush_sources(ctx: &mut PollTickContext<'_>) {
    let probe_budget = sources::http_poll::ProbeBudget::from_poll_interval_secs(ctx.args.interval);
    let (
        monero_node_rec,
        dynex_node_rec,
        quai_rec,
        qubic_rec,
        bzminer_rec,
        monero_miner_rec,
        onezerominer_rec,
    ) = tokio::join!(
        sources::monero::poll(ctx.client, &ctx.args.monero_node_rpc_url, probe_budget),
        sources::dynex::poll(ctx.client, &ctx.endpoints.dynex),
        sources::quai::poll(ctx.client, &ctx.args.quai_node_rpc_url, probe_budget),
        sources::qubic::poll(ctx.client, &ctx.endpoints.qubic),
        sources::bzminer::poll(ctx.client, &ctx.endpoints.bzminer, "kaspa"),
        sources::monero_miner::poll(
            ctx.client,
            &ctx.endpoints.xmrig,
            "monero",
            ctx.args.monero_miner,
        ),
        sources::onezerominer::poll(
            ctx.client,
            &ctx.endpoints.onezerominer,
            "dynex",
            probe_budget
        ),
    );
    flush_record(ctx.writer, monero_node_rec).await;
    flush_record(ctx.writer, dynex_node_rec).await;
    flush_record(ctx.writer, quai_rec).await;
    flush_record(ctx.writer, qubic_rec).await;
    flush_record(ctx.writer, bzminer_rec).await;
    flush_record(ctx.writer, monero_miner_rec).await;
    flush_record(ctx.writer, onezerominer_rec).await;
    flush_record(ctx.writer, sources::hwmon::poll()).await;
    flush_record(ctx.writer, ctx.rapl.poll()).await;
}

async fn run_collector(mut args: Args) -> anyhow::Result<()> {
    let data_dir = shellexpand::tilde(&args.data_dir).into_owned();
    info!(
        data_dir = %data_dir,
        interval_secs = args.interval,
        schema_version = mining_telemetry_core::SCHEMA_VERSION,
        "telemetry-collector starting"
    );

    sources::http_poll::validate_node_rpc_urls(
        args.node_rpc_loopback_only,
        &[
            ("dynex", args.dynex_node_rpc_url.as_str()),
            ("monero", args.monero_node_rpc_url.as_str()),
            ("quai", args.quai_node_rpc_url.as_str()),
            ("qubic", args.qubic_node_api_url.as_str()),
        ],
    )?;

    let client = reqwest::Client::builder()
        .timeout(sources::http_poll::CLIENT_REQUEST_TIMEOUT)
        .build()?;
    let mut w = writer::JsonlWriter::new(&data_dir);
    let rapl = sources::rapl::RaplState::new();
    let mut gpu_sched = gpu_scheduler::GpuScheduler::new(gpu_scheduler::GpuSchedulerConfig {
        thermal_throttle_c: args.thermal_throttle,
        thermal_emergency_c: args.thermal_emergency,
        ..Default::default()
    });
    let tick = Duration::from_secs(args.interval);
    let endpoints = HttpEndpoints::from_args(&args);
    let mut governor =
        process_governor::ProcessGovernor::new(std::mem::take(&mut args.governed_miners));

    let (startup_decision, startup_event) = gpu_sched.poll();
    let mut currently_paused = startup_decision.is_paused();
    apply_startup_gpu_state(&mut governor, &startup_decision);

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("Failed to create SIGTERM listener");

    if let Some(event) = startup_event {
        write_gpu_sched_event(&mut w, &event).await;
    }

    loop {
        let (decision, gpu_event) = gpu_sched.poll();
        log_gpu_decision(&decision);
        sync_miner_governor(&mut governor, decision.is_paused(), &mut currently_paused);

        if let Some(event) = gpu_event {
            write_gpu_sched_event(&mut w, &event).await;
        }

        let mut tick_ctx = PollTickContext {
            writer: &mut w,
            client: &client,
            args: &args,
            endpoints: &endpoints,
            rapl: &rapl,
        };
        poll_and_flush_sources(&mut tick_ctx).await;

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();
    validate_thermal_args(&args)?;
    run_collector(args).await
}

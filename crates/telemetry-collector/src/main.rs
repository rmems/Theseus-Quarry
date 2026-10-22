mod gpu_scheduler;
mod process_governor;
mod sources;
mod writer;

use clap::Parser;
use clap::builder::BoolishValueParser;
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
    #[arg(
        long,
        env = "NODE_RPC_LOOPBACK_ONLY",
        default_value_t = true,
        action = clap::ArgAction::Set,
        value_parser = BoolishValueParser::new()
    )]
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
}

impl HttpEndpoints {
    fn from_args(args: &Args) -> Self {
        Self {
            bzminer: format!("http://127.0.0.1:{}/", args.kaspa_api_port),
            onezerominer: format!("http://127.0.0.1:{}/", args.dynex_api_port),
            xmrig: format!("http://127.0.0.1:{}/", args.monero_api_port),
        }
    }
}

/// Node RPC/API URLs after loopback policy; `None` disables that source only.
struct NodePollEndpoints {
    monero_rpc: Option<String>,
    quai_rpc: Option<String>,
    dynex_getheight: Option<String>,
    qubic_tick_info: Option<String>,
}

impl NodePollEndpoints {
    fn from_args(args: &Args) -> Self {
        let loopback_only = args.node_rpc_loopback_only;
        let dynex_base = sources::http_poll::resolve_node_rpc_url(
            "dynex",
            args.dynex_node_rpc_url.clone(),
            loopback_only,
        );
        let qubic_base = sources::http_poll::resolve_node_rpc_url(
            "qubic",
            args.qubic_node_api_url.clone(),
            loopback_only,
        );
        Self {
            monero_rpc: sources::http_poll::resolve_node_rpc_url(
                "monero",
                args.monero_node_rpc_url.clone(),
                loopback_only,
            ),
            quai_rpc: sources::http_poll::resolve_node_rpc_url(
                "quai",
                args.quai_node_rpc_url.clone(),
                loopback_only,
            ),
            dynex_getheight: dynex_base.map(|u| format!("{}/getheight", u.trim_end_matches('/'))),
            qubic_tick_info: qubic_base.map(|u| format!("{}/tick-info", u.trim_end_matches('/'))),
        }
    }
}

fn disabled_node_record(source: &'static str) -> sources::TelemetryRecord {
    sources::TelemetryRecord {
        source,
        envelope: None,
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

fn pause_miner_governor(
    governor: &mut process_governor::ProcessGovernor,
    currently_paused: &mut bool,
) {
    if !*currently_paused {
        warn!("Thermal emergency / VRAM pressure breached: Suspending miners...");
        *currently_paused = true;
    }
    governor.suspend_miners();
}

fn resume_miner_governor_if_needed(
    governor: &mut process_governor::ProcessGovernor,
    currently_paused: &mut bool,
) {
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

fn sync_miner_governor(
    governor: &mut process_governor::ProcessGovernor,
    should_pause: bool,
    currently_paused: &mut bool,
) {
    governor.is_emergency = should_pause;
    if should_pause {
        pause_miner_governor(governor, currently_paused);
    } else {
        resume_miner_governor_if_needed(governor, currently_paused);
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
    node_endpoints: &'a NodePollEndpoints,
    rapl: &'a sources::rapl::RaplState,
}

type MinerPollBatch = (
    sources::TelemetryRecord,
    sources::TelemetryRecord,
    sources::TelemetryRecord,
    sources::TelemetryRecord,
    sources::TelemetryRecord,
    sources::TelemetryRecord,
    sources::TelemetryRecord,
);

async fn poll_mining_sources(ctx: &PollTickContext<'_>) -> MinerPollBatch {
    let probe_budget = sources::http_poll::ProbeBudget::from_poll_interval_secs(ctx.args.interval);
    let monero_node = async {
        match ctx.node_endpoints.monero_rpc.as_deref() {
            Some(url) => sources::monero::poll(ctx.client, url, probe_budget).await,
            None => disabled_node_record("monero"),
        }
    };
    let dynex_node = async {
        match ctx.node_endpoints.dynex_getheight.as_deref() {
            Some(url) => sources::dynex::poll(ctx.client, url).await,
            None => disabled_node_record("dynex"),
        }
    };
    let quai_node = async {
        match ctx.node_endpoints.quai_rpc.as_deref() {
            Some(url) => sources::quai::poll(ctx.client, url, probe_budget).await,
            None => disabled_node_record("quai"),
        }
    };
    let qubic_node = async {
        match ctx.node_endpoints.qubic_tick_info.as_deref() {
            Some(url) => sources::qubic::poll(ctx.client, url).await,
            None => disabled_node_record("qubic"),
        }
    };
    tokio::join!(
        monero_node,
        dynex_node,
        quai_node,
        qubic_node,
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
    )
}

async fn flush_mining_batch(w: &mut writer::JsonlWriter, batch: MinerPollBatch) {
    let (
        monero_node_rec,
        dynex_node_rec,
        quai_rec,
        qubic_rec,
        bzminer_rec,
        monero_miner_rec,
        onezerominer_rec,
    ) = batch;
    flush_record(w, monero_node_rec).await;
    flush_record(w, dynex_node_rec).await;
    flush_record(w, quai_rec).await;
    flush_record(w, qubic_rec).await;
    flush_record(w, bzminer_rec).await;
    flush_record(w, monero_miner_rec).await;
    flush_record(w, onezerominer_rec).await;
}

async fn poll_and_flush_sources(ctx: &mut PollTickContext<'_>) {
    let batch = poll_mining_sources(ctx).await;
    flush_mining_batch(ctx.writer, batch).await;
    flush_record(ctx.writer, sources::hwmon::poll()).await;
    flush_record(ctx.writer, ctx.rapl.poll()).await;
}

struct CollectorState {
    args: Args,
    client: reqwest::Client,
    writer: writer::JsonlWriter,
    rapl: sources::rapl::RaplState,
    gpu_sched: gpu_scheduler::GpuScheduler,
    governor: process_governor::ProcessGovernor,
    endpoints: HttpEndpoints,
    node_endpoints: NodePollEndpoints,
    tick: Duration,
    currently_paused: bool,
    #[cfg(unix)]
    sigterm: tokio::signal::unix::Signal,
}

fn prepare_data_dir(args: &Args) -> anyhow::Result<String> {
    Ok(shellexpand::tilde(&args.data_dir).into_owned())
}

async fn init_collector(mut args: Args) -> anyhow::Result<CollectorState> {
    let data_dir = prepare_data_dir(&args)?;
    info!(
        data_dir = %data_dir,
        interval_secs = args.interval,
        schema_version = mining_telemetry_core::SCHEMA_VERSION,
        "telemetry-collector starting"
    );

    let client = reqwest::Client::builder()
        .timeout(sources::http_poll::CLIENT_REQUEST_TIMEOUT)
        .build()?;
    let mut writer = writer::JsonlWriter::new(&data_dir);
    let rapl = sources::rapl::RaplState::new();
    let mut gpu_sched = gpu_scheduler::GpuScheduler::new(gpu_scheduler::GpuSchedulerConfig {
        thermal_throttle_c: args.thermal_throttle,
        thermal_emergency_c: args.thermal_emergency,
        ..Default::default()
    });
    let tick = Duration::from_secs(args.interval);
    let endpoints = HttpEndpoints::from_args(&args);
    let node_endpoints = NodePollEndpoints::from_args(&args);
    let mut governor =
        process_governor::ProcessGovernor::new(std::mem::take(&mut args.governed_miners));

    let (startup_decision, startup_event) = gpu_sched.poll();
    let currently_paused = startup_decision.is_paused();
    apply_startup_gpu_state(&mut governor, &startup_decision);

    if let Some(event) = startup_event {
        write_gpu_sched_event(&mut writer, &event).await;
    }

    #[cfg(unix)]
    let sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("Failed to create SIGTERM listener");

    Ok(CollectorState {
        args,
        client,
        writer,
        rapl,
        gpu_sched,
        governor,
        endpoints,
        node_endpoints,
        tick,
        currently_paused,
        #[cfg(unix)]
        sigterm,
    })
}

async fn run_poll_loop(state: &mut CollectorState) -> anyhow::Result<()> {
    loop {
        let (decision, gpu_event) = state.gpu_sched.poll();
        log_gpu_decision(&decision);
        sync_miner_governor(
            &mut state.governor,
            decision.is_paused(),
            &mut state.currently_paused,
        );

        if let Some(event) = gpu_event {
            write_gpu_sched_event(&mut state.writer, &event).await;
        }

        let mut tick_ctx = PollTickContext {
            writer: &mut state.writer,
            client: &state.client,
            args: &state.args,
            endpoints: &state.endpoints,
            node_endpoints: &state.node_endpoints,
            rapl: &state.rapl,
        };
        poll_and_flush_sources(&mut tick_ctx).await;

        #[cfg(not(unix))]
        let sigterm_fut = std::future::pending::<()>();

        #[cfg(unix)]
        let sigterm_fut = state.sigterm.recv();

        tokio::select! {
            _ = tokio::time::sleep(state.tick) => {}
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

async fn run_collector(args: Args) -> anyhow::Result<()> {
    let mut state = init_collector(args).await?;
    run_poll_loop(&mut state).await
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

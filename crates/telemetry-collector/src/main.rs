mod gpu_scheduler;
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

    loop {
        flush_record(&mut w, sources::monero::poll(&client).await).await;
        flush_record(&mut w, sources::dynex::poll(&client).await).await;
        flush_record(&mut w, sources::quai::poll(&client).await).await;
        flush_record(&mut w, sources::qubic::poll(&client).await).await;
        flush_record(&mut w, sources::kaspa::poll(&client).await).await;
        flush_record(&mut w, sources::hwmon::poll()).await;
        flush_record(&mut w, rapl.poll()).await;

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

        if let Some(event) = gpu_event {
            let env = mining_telemetry_core::envelope_from_gpu_sched("collector", &event);
            if let Err(e) = w.write_envelope(&env).await {
                warn!(source = "collector", error = %e, "gpu_sched write failed");
            } else {
                debug!(source = "collector", stem = %env.stem, "wrote gpu_sched envelope");
            }
        }

        tokio::time::sleep(tick).await;
    }
}

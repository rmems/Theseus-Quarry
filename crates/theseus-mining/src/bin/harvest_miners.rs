use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use clap::Parser;
use mining_telemetry_core::{CoinType, MinerBrand, MiningStats};
use walkdir::WalkDir;

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

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, default_value = "binaries/mining")]
    scan_dir: PathBuf,

    #[arg(long, default_value = "~/Spikenaut-Vault/telemetry/miners")]
    out_dir: PathBuf,

    #[arg(long, default_value = "all")]
    coins: String,

    #[arg(long, default_value_t = false)]
    verbose: bool,

    #[arg(long, default_value_t = 52428800)]
    max_size: u64,
}

fn detect_coin_type(path: &Path) -> CoinType {
    let path_str = path.to_string_lossy().to_lowercase();
    if path_str.contains("dynex") {
        CoinType::Dynex
    } else if path_str.contains("quai") {
        CoinType::Quai
    } else if path_str.contains("qubic") {
        CoinType::Qubic
    } else if path_str.contains("kaspa") || path_str.contains("bzminer") {
        CoinType::Kaspa
    } else if path_str.contains("monero") || path_str.contains("xmrig") {
        CoinType::Monero
    } else if path_str.contains("verus") || path_str.contains("hellminer") {
        CoinType::Verus
    } else if path_str.contains("ocean") {
        CoinType::Ocean
    } else {
        CoinType::Unknown
    }
}

fn detect_brand(line: &str) -> MinerBrand {
    let lower = line.to_lowercase();
    if lower.contains("bzminer") {
        MinerBrand::BzMiner
    } else if lower.contains("dynex") {
        MinerBrand::DynexSolver
    } else if lower.contains("xmrig") {
        MinerBrand::Xmrig
    } else if lower.contains("rigel") {
        MinerBrand::Rigel
    } else if lower.contains("qubic-core") {
        MinerBrand::QubicCore
    } else if lower.contains("srbminer") {
        MinerBrand::SRBMiner
    } else if lower.contains("hellminer") {
        MinerBrand::Hellminer
    } else {
        MinerBrand::Unknown
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let out_dir = expand_tilde(args.out_dir);
    fs::create_dir_all(&out_dir)?;

    let mut total_records = 0usize;

    eprintln!("[harvest] Scanning: {}", args.scan_dir.display());

    for entry in WalkDir::new(&args.scan_dir)
        .max_depth(5)
        .into_iter()
        .flatten()
    {
        let path = entry.path();
        if !path.is_file() || path.extension().map(|s| s != "log").unwrap_or(true) {
            continue;
        }

        let coin = detect_coin_type(path);
        if coin == CoinType::Unknown {
            continue;
        }

        let reader = BufReader::new(File::open(path)?);
        let mut brand = MinerBrand::Unknown;

        for line_result in reader.lines() {
            let line = match line_result {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("[harvest] {} line read error: {e}", path.display());
                    continue;
                }
            };
            if brand == MinerBrand::Unknown {
                brand = detect_brand(&line);
            }
            if brand == MinerBrand::Unknown {
                continue;
            }

            let mut stats = MiningStats::default();
            stats.update_from_line(brand, &line);

            if stats.dynex.is_active
                || stats.quai.is_active
                || stats.qubic.is_active
                || stats.monero.is_active
                || stats.verus.is_active
                || stats.kaspa.is_active
            {
                let coin_name = match coin {
                    CoinType::Dynex => "dynex",
                    CoinType::Quai => "quai",
                    CoinType::Qubic => "qubic",
                    CoinType::Kaspa => "kaspa",
                    CoinType::Monero => "monero",
                    CoinType::Verus => "verus",
                    CoinType::Ocean => "ocean",
                    CoinType::Unknown => "unknown",
                };

                if args.verbose {
                    eprintln!(
                        "[{}] {}",
                        coin_name,
                        serde_json::to_string(&stats).unwrap_or_default()
                    );
                }
                total_records += 1;
            }
        }
    }

    eprintln!(
        "[harvest] Parsed {} telemetry lines from {}",
        total_records,
        args.scan_dir.display()
    );
    Ok(())
}

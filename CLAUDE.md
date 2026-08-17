# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Theseus-Quarry: a mining-strict Rust workspace — multi-coin miner orchestration and **ops telemetry only**. Multi-coin mining here is not a profit operation; it's a hardware/mining data-capture instrument for the owner's **Theseus Machine Physiology** project. The JSONL it produces is a raw data source consumed by separate personal-research repos (in the `rmems` org and `Limen-Neural`) that train an SNN model. This repo does not know about that pipeline by name and must not depend on it — it stays a strict producer. It is not an SNN/training product and must not become one; see Ownership boundaries below before adding any dependency or crate.

## Commands

Toolchain needs **rustup** (`rust-toolchain.toml` pins latest stable + rustfmt/clippy/rust-src), not just a distro `rustc`:

```bash
./scripts/setup-rust.sh
source "$HOME/.cargo/env"
```

Full local verify — must pass before merge/tag (mirrors CI on the self-hosted `ryzen` runner):

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build -p telemetry-collector --release --locked
```

Single test (module path is from the crate root, e.g. `mod gpu_scheduler;` in `main.rs`):

```bash
cargo test -p telemetry-collector gpu_scheduler::tests::thermal_emergency_pauses_mining
cargo test -p mining-telemetry-core schema::tests::envelope_round_trip_miner_perf
```

Run the collector locally:

```bash
export TELEMETRY_DATA_DIR=./data/telemetry
cargo run -p telemetry-collector
```

Source-only Docker image (no `binaries/`, wallets, or `.env`):

```bash
docker build -f .devcontainer/Dockerfile -t theseus-quarry:dev .
```

## Architecture

Exactly two workspace crates (`Cargo.toml`), deliberately — see Ownership boundaries:

- **`crates/mining-telemetry-core`** — `schema.rs` is the single source of truth for the JSONL wire format: `TelemetryEnvelope` / `TelemetryPayload` / `RecordKind`, `SCHEMA_VERSION = 1`. The collector depends on this crate rather than redefining payload shapes locally.
- **`crates/telemetry-collector`** — the sole JSONL writer. `main.rs` runs a poll loop that fires every source concurrently with one `tokio::join!` per tick:
  - `sources/*.rs` — one adapter per producer. Miner local HTTP APIs (`bzminer`, `xmrig`, `srbminer`, `onezerominer`) produce `MinerPerf`; node RPC (`monero`, `dynex`, `quai`, `qubic`) produces `NodeHealth`; `hwmon`/`rapl` produce `HostHw`. Each `poll()` returns `TelemetryRecord { source, envelope: Option<TelemetryEnvelope> }` — `None` means a soft-fail for that tick, not a panic.
  - `sources/monero_miner.rs` — dispatch adapter for "one coin, multiple possible miner binaries": `MONERO_MINER=auto` races `xmrig` and `srbminer` polls in parallel and prefers `xmrig` if both succeed. Follow this pattern for future multi-miner coins rather than hardcoding one binary.
  - `gpu_scheduler.rs` — `GpuScheduler`: NVML-backed thermal/VRAM arbitration with a fixed priority ladder (thermal emergency → thermal throttle → VRAM pressure → allowed) and a transition cooldown to prevent thrashing. This is mining safety, not a learned/adaptive policy — keep it that way.
  - `process_governor.rs` — `ProcessGovernor`: SIGSTOP/SIGCONT on governed miner binary names, tracked by `(PID, start_time)` so it never touches a process it didn't stop itself (an operator's own `SIGSTOP`'d process is left alone); resumes tracked miners on `Drop` unless an emergency is still active.
  - `writer.rs` — `JsonlWriter`: one rolling file handle per envelope `stem`, daily rotation, 6 files kept.

Data model (`mining_telemetry_core::schema`): every JSONL line is an envelope — `schema_version`, `timestamp`, `source`, `kind`, optional `host`/`run_id`, `stem`, `payload`. `kind` ∈ `{miner_perf, node_health, host_hw, status, gpu_sched, rotation}`. MinerPerf and NodeHealth for the same coin write to **different stems** (`{coin}_miner_telemetry` vs `{coin}_telemetry`) — never mix kinds into one stem. Schema breaks free at v1: no dual-compat shim for pre-schema free-form JSON.

Config is entirely env-var/CLI via `clap` (`Args` in `main.rs`, `env = "..."` on every field) — no config file. Canonical secrets live outside the repo at `~/.config/theseus-quarry/*.env` (from `.env.example` / `binaries.env.example`), loaded by `scripts/load-env.sh` for the shell miner scripts (`scripts/mine-*.sh`); the collector binary reads env vars directly or a gitignored root `.env` override.

## Ownership boundaries (hard rule)

Theseus-Quarry is the body, not the nervous system. Do not Cargo-depend on `neuromod`, `axon-encoder`, `hybrid-fusion`, `thalamic-relay`, `limbic-critic`, `corpus-ipc`, or any `limen-return` package, and do not add a third workspace crate (e.g. `crates/learned-supervisor`) for research adapters — two crates only. Full in/out table: `CONTEXT.md` → Ownership boundaries. The dual-process Theseus-producer / Thalamic-supervisor contract (GH#17) is coordinated in another repo/issue — do not implement it here.

Never commit `binaries/`, wallets, seeds, real `.env`, or chain data (see `.gitignore`).

## Release / versioning

Workspace version lives in root `Cargo.toml` → `[workspace.package].version`. Minor bump for feature/schema changes at a milestone cut; patch optional for docs/CI/profile-only changes. Full policy and tag/release commands: `REVIEW.md` → Release.

## Further reading

- `CONTEXT.md` — glossary (envelope/stem/kind/adapter), architecture decision log, Ownership boundaries in/out table
- `AGENTS.md` — agent hard rules, Cursor Cloud environment specifics
- `REVIEW.md` — pre-merge/pre-tag checklist (Cargo profiles, domain invariants, release policy)

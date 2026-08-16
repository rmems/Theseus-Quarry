# Theseus-Quarry — domain context

Mining-strict workspace: multi-coin miner orchestration and **ops telemetry**.
Not an SNN/training product. Downstream consumers may read JSONL; they are not this repo’s identity.

## Glossary

| Term | Meaning |
|------|---------|
| **Telemetry Schema module** | Owned by `mining-telemetry-core`. Defines the durable record shape (envelope + payload). The single place field names and units are decided. |
| **Envelope** | Common JSONL keys on every line: `schema_version`, `timestamp`, `source`, `kind`, optional `host` / `run_id`. |
| **Payload** | Kind-specific body: miner performance, node health, host hardware, or control events. |
| **Kind** | Record class: `miner_perf`, `node_health`, `host_hw`, `status`, `gpu_sched`, `rotation`. |
| **Source** | Producer identity for JSONL records: `collector`. |
| **Stem** | JSONL file basename without extension. Node health uses `{coin}_telemetry`; miner HTTP MinerPerf uses `{coin}_miner_telemetry`; host sensors use stems like `hwmon_telemetry`. Multi-stem layout under `TELEMETRY_DATA_DIR`. |
| **Adapter (collector)** | `telemetry-collector` path: miner HTTP APIs, node RPC, and sysfs poll → envelope → JSONL. |
| **Miner performance** | Hashrate, shares, miner uptime — from miner process or miner HTTP API. |
| **Node health** | Chain height, tick, peer/sync — from full-node RPC. Distinct stem/kind from miner performance. |
| **Host hardware** | RAPL, hwmon, NVML-class signals. |
| **schema_version** | Integer on every envelope. Current target: **1** (break free; no alias layer for pre-schema free-form JSON). |

## Decisions (architecture)

### Telemetry Schema deepen (2026-07-28)

- **Layout:** multi-stem JSONL (not single `events.jsonl`).
- **Writers:** single writer — `telemetry-collector` under `TELEMETRY_DATA_DIR`.
- **Compat:** break free at `schema_version = 1` (honest names; no dual-compat with old free-form lines).
- **Owner crate:** `mining-telemetry-core`; collector must depend on it.
- **Implemented (2026-07-28):** `crates/mining-telemetry-core/src/schema.rs`; collector dual-path node + host hardware; status lines stay log-only (not JSONL).
- **Implemented (2026-08-11):** MinerPerf from miner local HTTP APIs (BzMiner / XMRig / OneZeroMiner); removed supervisor stdout `extract_hashrate` path. Node RPC sources remain for `NodeHealth`.
- **Implemented (2026-08-12):** SRBMiner-Multi HTTP MinerPerf on `MONERO_API_PORT` (`MONERO_MINER=auto|xmrig|srbminer`); launch script uses `--api-enable --api-port`.

## Ownership boundaries

Theseus-Quarry is the body, not the nervous system: multi-coin miner
orchestration and ops telemetry only. Theseus does not know Thalamic,
`neuromod`, or any SNN/training crate by name.

### In scope (this repo)

- Multi-coin miner launch scripts and local HTTP / node RPC polling
- `telemetry-collector` writing schema v1 JSONL under `TELEMETRY_DATA_DIR`
- `mining-telemetry-core` envelope / kinds / stems (MinerPerf vs NodeHealth vs host vs `gpu_sched`)
- `GpuScheduler` + `ProcessGovernor` as mining safety (thermal / VRAM / SIGSTOP), not a learned policy
- Two crates only — no new crate for research adapters

### Out of scope (other repos)

| Concern | Lives in |
|---------|----------|
| Dataset archive / sanitized copies | `Spikenaut-Vault` |
| SNN training / proposals | `Spikenaut-SNN` |
| Analog → spikes | `axon-encoder` (crates.io) |
| SNN primitives | `neuromod` |
| LLM hidden-state → SNN | `hybrid-fusion` |
| Deterministic supervisor *consumer* of the telemetry contract | `thalamic-relay` (GH#17) |

Theseus must **not** Cargo-depend on Thalamic, `neuromod`, `axon-encoder`,
`hybrid-fusion`, `limbic-critic`, or `corpus-ipc`. Downstream may read our
JSONL; they are not this repo's identity.

GH#17 (dual-process Theseus producer + Thalamic supervisor, contract only) is
the research/runtime coordination issue and stays in its own repo/issue; this
section is docs-only and does not implement it.

## Non-goals

- Shipping miner binaries or chain data in git
- Embedding neuromorphic training or SNN crates here
- One crate per coin as a modularization goal

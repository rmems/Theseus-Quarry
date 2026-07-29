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
| **Source** | Producer identity: e.g. `supervisor`, `collector`, or a finer id. |
| **Stem** | JSONL file basename without extension (`monero_telemetry`, `hwmon_telemetry`, …). Multi-stem layout: one file per stem under `TELEMETRY_DATA_DIR`. |
| **Adapter (supervisor)** | `theseus-mining` path: process stdout / `WireMsg` → envelope → JSONL (+ logs). |
| **Adapter (collector)** | `telemetry-collector` path: HTTP/sysfs poll → envelope → JSONL. |
| **WireMsg** | In-process bus message (`MiningTelem`, `Status`, scheduler/rotation events). Not the disk format; may be projected into envelopes. |
| **Miner performance** | Hashrate, shares, miner uptime — from miner process or miner HTTP API. |
| **Node health** | Chain height, tick, peer/sync — from full-node RPC. Distinct stem/kind from miner performance. |
| **Host hardware** | RAPL, hwmon, NVML-class signals. |
| **schema_version** | Integer on every envelope. Current target: **1** (break free; no alias layer for pre-schema free-form JSON). |

## Decisions (architecture)

### Telemetry Schema deepen (2026-07-28)

- **Layout:** multi-stem JSONL (not single `events.jsonl`).
- **Writers:** dual — supervisor and collector both write envelopes under `TELEMETRY_DATA_DIR`.
- **Compat:** break free at `schema_version = 1` (honest names; no dual-compat with old free-form lines).
- **Owner crate:** `mining-telemetry-core`; collector must depend on it.
- **Implemented (2026-07-28):** `crates/mining-telemetry-core/src/schema.rs`; collector + supervisor dual-write; status lines stay log-only (not JSONL).
- **Out of scope / follow-ons:** unit bug fix in `extract_hashrate`, miner-vs-node port alignment, file rotation, monero/verus wiring into `UnifiedMining`.

## Non-goals

- Shipping miner binaries or chain data in git
- Embedding neuromorphic training or SNN crates here
- One crate per coin as a modularization goal

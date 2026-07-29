# Theseus-Quarry

**Mining-strict** Rust workspace: multi-coin miner orchestration, process supervision, and **ops telemetry** (hashrate, shares, temperature, power, node health).

This is not a neuromorphic / SNN research library. JSONL metrics are plain mining/hardware signals. Downstream training projects may consume them; they live elsewhere.

## Workspace

| Crate | Role |
|-------|------|
| `theseus-mining` | Supervisor + per-coin miner launchers |
| `mining-telemetry-core` | Shared stats / types for miner brands and coins |
| `telemetry-collector` | Polls local nodes and sensors; writes JSONL |

Shell helpers under `scripts/mine-*.sh` start individual coin miners.

## What is not in git

Runtime stays on disk only:

- `binaries/` — miner binaries, full nodes, chain data (often 100GB+)
- Wallets, seeds, private keys, SSL keys
- Real `.env` / config secrets

See `.gitignore`. Do not force-add those paths.

## Configuration

Canonical secrets directory:

```text
~/.config/theseus-quarry/
  mining.env      # wallets, pools, thread counts (from .env.example)
  binaries.env    # RPC passwords, binary paths (from binaries.env.example)
  ocean.env       # optional Ocean stack secrets
```

```bash
mkdir -p ~/.config/theseus-quarry
cp .env.example ~/.config/theseus-quarry/mining.env
cp binaries.env.example ~/.config/theseus-quarry/binaries.env
chmod 600 ~/.config/theseus-quarry/*.env
# edit with real values
```

Optional: a gitignored `.env` in the repo root still works as a local override. Shell scripts load config first via `scripts/load-env.sh`. The Rust supervisor uses `dotenv` and reads a repo-local `.env` if present (copy or symlink from config if you run the binary from the repo).

### Telemetry (schema v1)

Both **telemetry-collector** and **theseus-mining** write multi-stem JSONL under
`TELEMETRY_DATA_DIR` (default `./data/telemetry`). Every line is a versioned envelope:

```json
{
  "schema_version": 1,
  "timestamp": "…",
  "source": "collector|supervisor",
  "kind": "miner_perf|node_health|host_hw|gpu_sched|rotation",
  "stem": "dynex_telemetry",
  "payload": { "type": "…", "…": "…" }
}
```

```bash
export TELEMETRY_DATA_DIR=./data/telemetry
cargo run -p telemetry-collector
# supervisor dual-write (default on):
cargo run -p theseus-mining -- --algo dynex
# disable supervisor disk writes:
cargo run -p theseus-mining -- --algo dynex --telemetry-jsonl false
```

See `CONTEXT.md` for domain terms (envelope, kind, stem, adapters).

## Build

```bash
cargo build --workspace
cargo test --workspace
```

Requires a recent Rust toolchain (edition 2024). GPU NVML features need NVIDIA drivers where used.

## Coins (orchestration targets)

Dynex, Quai, Qubic, Kaspa, Monero, Verus — via external miner binaries you place under `binaries/` (not shipped in this repository).

## License

MIT — see [LICENSE](LICENSE).

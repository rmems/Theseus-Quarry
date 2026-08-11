# Theseus-Quarry

**Mining-strict** Rust workspace: multi-coin miner orchestration, process supervision, and **ops telemetry** (hashrate, shares, temperature, power, node health).

This is not a neuromorphic / SNN research library. JSONL metrics are plain mining/hardware signals. Downstream training projects may consume them; they live elsewhere.

## Workspace

| Crate | Role |
|-------|------|
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

Optional: a gitignored `.env` in the repo root still works as a local override. Shell scripts load config first via `scripts/load-env.sh`. The Rust collector uses `dotenvy` / env vars (copy or symlink from config if you run the binary from the repo).

### Telemetry (schema v1)

**telemetry-collector** is the sole JSONL writer. It polls miner local HTTP APIs
(MinerPerf), node RPC (NodeHealth), and sysfs (host hardware), writing multi-stem
JSONL under `TELEMETRY_DATA_DIR` (default `./data/telemetry`). Every line is a
versioned envelope:

```json
{
  "schema_version": 1,
  "timestamp": "…",
  "source": "collector",
  "kind": "miner_perf|node_health|host_hw|gpu_sched|rotation",
  "stem": "dynex_telemetry",
  "payload": { "type": "…", "…": "…" }
}
```

```bash
export TELEMETRY_DATA_DIR=./data/telemetry
cargo run -p telemetry-collector
```

See `CONTEXT.md` for domain terms (envelope, kind, stem, adapters).

## Build

Requires **rustup** (not only a distro `rustc` package). `rust-toolchain.toml` pins the **latest stable** channel and installs `rustfmt`, `clippy`, `rust-src`, and `rust-analyzer`.

```bash
# one-time / update toolchain
./scripts/setup-rust.sh
# ensure ~/.cargo/bin is first on PATH
source "$HOME/.cargo/env"

cargo build --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Edition 2024. GPU NVML features need NVIDIA drivers on the host where used.

### Dev Container / Docker

Source-only image (no `binaries/`, wallets, or `.env`):

```bash
docker build -f .devcontainer/Dockerfile -t theseus-quarry:dev .
# Run as the host user so Cargo does not create root-owned target/ on the bind mount.
docker run --rm -it \
  --user "$(id -u):$(id -g)" \
  -e CARGO_HOME=/tmp/cargo \
  -e CARGO_TARGET_DIR=/tmp/target \
  -v "$PWD:/workspace" -w /workspace \
  theseus-quarry:dev bash
```

Or open the repo in VS Code / Cursor **Dev Containers** (`.devcontainer/`). Do not mount host wallet paths into the container by default.

### CI (self-hosted GPU runner)

GitHub Actions runs on a **self-hosted** runner with labels:

`self-hosted`, `Linux`, `X64`, `ryzen`

| Workflow | What it does |
|----------|----------------|
| `CI` | Latest stable Rust: `fmt`, `clippy -D warnings`, `test --workspace`; self-hosted **qodana · rust** |
| `Qodana` | Cloud scan via `JetBrains/qodana-action@v2026.1` (`.github/workflows/code_quality.yml`) — needs `QODANA_TOKEN` |
| `Docker` | Build `.devcontainer/Dockerfile`, smoke `cargo test` in the image |

Add the Qodana Cloud project token as a repo secret: **Settings → Secrets → `QODANA_TOKEN`**.

Local Qodana (optional):

```bash
qodana scan --config qodana.yaml --image jetbrains/qodana-rust:2026.1-eap --skip-pull --print-problems
```

On the runner host (e.g. ShipOfTheseus):

```bash
cd ~/actions-runner/Theseus-Quarry-runner
# preferred: install as a service (needs sudo once)
sudo ./svc.sh install
sudo ./svc.sh start
sudo ./svc.sh status
# or foreground: ./run.sh
```

Confirm online:

```bash
gh api repos/rmems/Theseus-Quarry/actions/runners \
  --jq '.runners[] | {name, status, labels: [.labels[].name]}'
```

CI never needs mining binaries or secrets checked into git.

## Coins (orchestration targets)

Dynex, Quai, Qubic, Kaspa, Monero, Verus — via external miner binaries you place under `binaries/` (not shipped in this repository).

## License

MIT — see [LICENSE](LICENSE).

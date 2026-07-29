# Theseus-Quarry — agent guide

Mining-strict Rust workspace: multi-coin miner orchestration and ops telemetry.
Not an SNN/training product. See `CONTEXT.md` for domain terms (envelope, stem, kind).

## Repo layout

| Path | Role |
|------|------|
| `crates/theseus-mining` | Supervisor + per-coin miner launchers |
| `crates/mining-telemetry-core` | Shared stats, wire messages, JSONL schema |
| `crates/telemetry-collector` | Polls nodes/sensors; writes JSONL |
| `.devcontainer/` | Local VS Code / Cursor Dev Container |
| `.cursor/` | Cursor **Cloud Agent** Dockerfile + `environment.json` |
| `scripts/` | Shell helpers (`setup-rust.sh`, mine-*.sh) |

## Hard rules

- Do **not** commit `binaries/`, wallets, seeds, real `.env`, or chain data.
- Prefer `--locked` on CI-style Cargo commands.
- Telemetry schema lives in `mining-telemetry-core`; keep MinerPerf vs NodeHealth distinct.
- Hashrate units: `extract_hashrate` returns **MH/s**; convert when storing H/s (`* 1e6`) or kH/s (`* 1e3`).

## Local verify

```bash
./scripts/setup-rust.sh   # if needed
source "$HOME/.cargo/env"
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Dev image (source-only):

```bash
docker build -f .devcontainer/Dockerfile -t theseus-quarry:dev .
```

## Cursor Cloud specific instructions

Cloud agents use `.cursor/environment.json` (Dockerfile mode). Paths are relative to `.cursor/`.

| Hook | Command |
|------|---------|
| Image | `.cursor/Dockerfile` — Ubuntu 24.04, stable Rust, nested Docker |
| `install` | `rustup show && cargo fetch --locked` |
| `start` | Start Docker daemon (`fuse-overlayfs` + `iptables-legacy`) |

After boot:

1. Confirm toolchain: `rustc --version && cargo --version`
2. Run the local verify commands above before declaring work done.
3. Docker smoke (optional): `docker build -f .devcontainer/Dockerfile -t theseus-quarry:dev .`
4. Never mount or invent wallet/binary secrets; use repo examples (`.env.example`, `binaries.env.example`) only as templates.
5. Self-hosted GitHub Actions (`ryzen`) are for CI on a trusted private host — do not broaden `pull_request` self-hosted exposure without gates.

If Docker is needed mid-task and `docker info` fails, run `sudo service docker start` (or check `/tmp/dockerd.log`).

# Theseus-Quarry — quality review checklist

Pre-merge and pre-tag gate for **humans and agents**. Domain terms: `CONTEXT.md`. Agent rules: `AGENTS.md`.

## Purpose

Confirm the workspace still builds cleanly, respects schema/domain invariants, and is safe to tag. This is not an APM/vendor observability checklist (New Relic / Sentry / Prometheus exporters are **out of scope** for now).

## Toolchain

| Item | Location |
|------|----------|
| Channel + components | `rust-toolchain.toml` (stable + rustfmt, clippy, rust-src, rust-analyzer) |
| Local install | `./scripts/setup-rust.sh` then `source "$HOME/.cargo/env"` |
| Lockfile | Prefer `cargo … --locked` for CI-style checks |

Confirm:

```bash
rustc --version && cargo --version
```

## Cargo profiles

Defined in the workspace root `Cargo.toml`:

| Profile | Command examples | Intent |
|---------|------------------|--------|
| **dev** | `cargo build -p telemetry-collector` | Fast iteration; `debug = 1` (line tables) |
| **test** | `cargo test --workspace --locked` | Unit/integration tests with debug info |
| **bench** | `cargo bench` (when benches exist) | Scaffold only; no LTO until real benches land |
| **release** | `cargo build -p telemetry-collector --release --locked` | Production collector: thin LTO, single CGU, strip symbols, `panic = "abort"` |

Do not force a workspace version bump solely for profile tweaks; see [Release](#release).

## Commands (must pass before merge / tag)

```bash
./scripts/setup-rust.sh   # if needed
source "$HOME/.cargo/env"

cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build -p telemetry-collector --release
```

Optional:

```bash
docker build -f .devcontainer/Dockerfile -t theseus-quarry:dev .
# cargo bench   # only after benches/ exist
```

CI (self-hosted `ryzen`) runs fmt, clippy, test, and a **release-profile** collector build smoke.

## Domain checks

- [ ] **MinerPerf vs NodeHealth** — miner HTTP sources write `{coin}_miner_telemetry`; node RPC stays `{coin}_telemetry`. Do not mix kinds in one stem.
- [ ] **Hashrate units** — store the miner API’s native `hashrate_unit` (do not force MH/s). XMRig/BzMiner document H/s; OneZero is conservative H/s until live confirm (#15).
- [ ] **ProcessGovernor** — only track processes we stop; untracked already-stopped miners stay unowned; PID + start_time identity; no SIGCONT to unrelated PIDs.
- [ ] **Schema** — `schema_version = 1` envelopes only; no free-form dual-compat.
- [ ] **SRBMiner** — not parsed on `MONERO_API_PORT` yet (#14); do not claim support in docs without the parser.

## Secrets / non-goals

**Never commit or invent:**

- `binaries/`, wallets, seeds, real `.env`, chain data
- Live New Relic / Sentry DSNs as hardcoded secrets

Templates only: `.env.example`, `binaries.env.example`.

**Non-goals of this checklist:** neuromorphic/SNN crates, one crate per coin, shipping miner binaries in git.

## Release

Workspace version lives in root `Cargo.toml` → `[workspace.package] version`.

| Change type | Version |
|-------------|---------|
| Feature / behavior (governor, new miner HTTP, schema) | **minor** (e.g. 0.3.0 → 0.4.0) when cutting a milestone release |
| Bugfix, docs, profiles, REVIEW-only, CI smoke | **patch** optional (e.g. 0.3.0 → 0.3.1); skip bump if only hygiene and no tag is planned |
| Breaking schema or public CLI | **minor** at 0.x (or major when 1.0 exists) |

Tag and GitHub release:

```bash
# after main is green and version bumped if needed
git tag -a vX.Y.Z -m "Theseus-Quarry vX.Y.Z — …"
git push origin vX.Y.Z
gh release create vX.Y.Z --title "vX.Y.Z — …" --notes "…"
```

Update Linear twin(s) on the **rmems** team / **Crypto mining telemetry extraction** project when GH issues close.

Current release line: **v0.3.0** (ProcessGovernor + HTTP MinerPerf). Active milestone: **v0.4 — Quality & miner API polish**.

## Out of scope (explicitly declined for now)

- New Relic infrastructure / custom events
- Sentry APM (optional later for hard collector faults only)
- Full Prometheus exporter
- Full criterion suite for every parser

## Quick agent path

1. Run [Commands](#commands).
2. Skim [Domain checks](#domain-checks).
3. Confirm no secrets in the diff.
4. Open PR with `Co-authored-by` when agent-authored; assign **rmems**, set milestone, link Linear twin.

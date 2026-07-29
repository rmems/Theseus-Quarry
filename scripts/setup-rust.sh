#!/usr/bin/env bash
# Install/update latest stable Rust via rustup for Theseus-Quarry.
# Idempotent. Prefer rustup over distro packages so rust-toolchain.toml works.
set -euo pipefail

export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"
export PATH="$CARGO_HOME/bin:$PATH"
export RUSTUP_INIT_SKIP_PATH_CHECK="${RUSTUP_INIT_SKIP_PATH_CHECK:-yes}"

if ! command -v rustup >/dev/null 2>&1; then
  echo "Installing rustup (stable)..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
    --default-toolchain stable \
    --profile default
fi

# shellcheck disable=SC1091
if [ -f "$CARGO_HOME/env" ]; then
  # shellcheck source=/dev/null
  . "$CARGO_HOME/env"
fi

rustup self update || true
rustup update stable
rustup default stable
rustup component add rustfmt clippy rust-src rust-analyzer

# Ensure repo toolchain is present (reads rust-toolchain.toml if in repo root)
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ -f "$ROOT/rust-toolchain.toml" ]; then
  (cd "$ROOT" && rustup show)
fi

echo ""
echo "Active toolchain:"
rustc --version
cargo --version
rustup component list --installed | rg 'rustfmt|clippy|rust-src|rust-analyzer' || true
echo ""
echo "PATH tip: ensure $CARGO_HOME/bin is before /usr/bin in your shell:"
echo "  echo 'source \"\$HOME/.cargo/env\"' >> ~/.bashrc"

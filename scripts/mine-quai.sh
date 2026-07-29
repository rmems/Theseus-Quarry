#!/usr/bin/env bash
# =============================================================================
# mine-quai.sh — Quai Network CPU/GPU Mining
# Algorithm: Kawpow
# Miner: rigel
# =============================================================================

set -euo pipefail

# ─── Directories ──────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LOG_DIR="$REPO_ROOT/data/logs"

# ─── Coin-specific defaults ───────────────────────────────────────────────────
COIN="quai"
BINARY_NAME="rigel"
BINARY_DEFAULT="binaries/mining/rigel-1.23.1-linux/rigel"
LOG_FILE="$LOG_DIR/${COIN}.log"

# ─── Pre-flight checks ────────────────────────────────────────────────────────
preflight_check() {
    mkdir -p "$LOG_DIR"

    # Load mining env (XDG config first, then repo .env)
    # shellcheck source=load-env.sh
    source "$REPO_ROOT/scripts/load-env.sh"

    # Check wallet (with backward compat for QUAI_WALLET_ADDRESS)
    local wallet="${QUAI_WALLET:-${QUAI_WALLET_ADDRESS:-}}"
    if [ -z "$wallet" ]; then
        echo "ERROR: QUAI_WALLET not set in mining env"
        echo "Set QUAI_WALLET=<your_quai_address> in ~/.config/theseus-quarry/mining.env"
        exit 1
    fi

    # Check binary
    local bin="${QUAI_BIN:-$REPO_ROOT/$BINARY_DEFAULT}"
    if [ ! -f "$bin" ]; then
        echo "ERROR: $bin not found"
        echo "Set QUAI_BIN in .env or place rigel at $BINARY_DEFAULT"
        exit 1
    fi
    if [ ! -x "$bin" ]; then
        chmod +x "$bin"
    fi
}

# ─── Start miner ─────────────────────────────────────────────────────────────
start_miner() {
    local bin="${QUAI_BIN:-$REPO_ROOT/$BINARY_DEFAULT}"
    local wallet="${QUAI_WALLET:-${QUAI_WALLET_ADDRESS:-}}"
    local pool="${QUAI_POOL:-stratum+tcp://127.0.0.1:7071}"
    local threads="${QUAI_THREADS:-8}"
    local worker="${SHIP_WORKER_NAME:-ship-of-theseus}"

    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Starting ${COIN} miner" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Wallet: ${wallet:0:24}..." | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Pool: $pool" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Threads: $threads" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Worker: $worker" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Log: $LOG_FILE" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] ───────────────────────────────────────" | tee -a "$LOG_FILE"

    # Launch miner
    cd "$(dirname "$bin")"
    exec "$bin" \
        -a kawpow \
        --coin quai \
        -o "$pool" \
        -u "$wallet" \
        -w "$worker" \
        --threads "$threads" \
        --no-tui \
        --stats-interval 10 \
        --log-file "$LOG_FILE"
}

# ─── Main ────────────────────────────────────────────────────────────────────
preflight_check
start_miner

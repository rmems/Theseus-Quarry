#!/usr/bin/env bash
# =============================================================================
# mine-qubic.sh — Qubic CPU Mining
# Algorithm: Qubic
# Miner: qli-Client
# =============================================================================

set -euo pipefail

# ─── Directories ──────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LOG_DIR="$REPO_ROOT/data/logs"

# ─── Coin-specific defaults ───────────────────────────────────────────────────
COIN="qubic"
BINARY_NAME="qli-Client"
BINARY_DEFAULT="binaries/mining/qli-client/qli-Client"
LOG_FILE="$LOG_DIR/${COIN}.log"

# ─── Pre-flight checks ────────────────────────────────────────────────────────
preflight_check() {
    mkdir -p "$LOG_DIR"

    # Load mining env (XDG config first, then repo .env)
    # shellcheck source=load-env.sh
    source "$REPO_ROOT/scripts/load-env.sh"

    # Check wallet (with backward compat for QUBIC_WALLET_ADDRESS)
    local wallet="${QUBIC_WALLET:-${QUBIC_WALLET_ADDRESS:-}}"
    if [ -z "$wallet" ]; then
        echo "ERROR: QUBIC_WALLET not set in mining env"
        echo "Set QUBIC_WALLET=<your_qubic_address> in ~/.config/theseus-quarry/mining.env"
        exit 1
    fi

    # Check binary
    local bin="${QUBIC_BIN:-$REPO_ROOT/$BINARY_DEFAULT}"
    if [ ! -f "$bin" ]; then
        echo "ERROR: $bin not found"
        echo "Set QUBIC_BIN in .env or place qli-Client at $BINARY_DEFAULT"
        exit 1
    fi
    if [ ! -x "$bin" ]; then
        chmod +x "$bin"
    fi

    # Check node API
    local api_url="${QUBIC_API_URL:-http://127.0.0.1:8099}"
    if ! curl -s --max-time 5 "$api_url/tick-info" >/dev/null 2>&1; then
        echo "WARNING: Qubic node API not responding at $api_url"
        echo "Start node first or set QUBIC_API_URL in .env"
    fi
}

# ─── Start miner ─────────────────────────────────────────────────────────────
start_miner() {
    local bin="${QUBIC_BIN:-$REPO_ROOT/$BINARY_DEFAULT}"
    local wallet="${QUBIC_WALLET:-${QUBIC_WALLET_ADDRESS:-}}"
    local pool="${QUBIC_POOL:-https://pool.qubic.li}"
    local threads="${QUBIC_THREADS:-16}"
    local alias="${SHIP_WORKER_NAME:-ship-of-theseus}"

    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Starting ${COIN} miner" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Wallet: ${wallet:0:24}..." | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Pool: $pool (solo)" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Threads: $threads" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Alias: $alias" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Log: $LOG_FILE" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] ───────────────────────────────────────" | tee -a "$LOG_FILE"

    # Launch miner (solo mining mode)
    cd "$(dirname "$bin")"
    exec "$bin" \
        --ClientSettings:QubicAddress="$wallet" \
        --ClientSettings:Alias="$alias" \
        --ClientSettings:Trainer:CpuThreads="$threads" \
        --ClientSettings:Trainer:PPS=false
}

# ─── Main ────────────────────────────────────────────────────────────────────
preflight_check
start_miner

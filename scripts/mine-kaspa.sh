#!/usr/bin/env bash
# =============================================================================
# mine-kaspa.sh — Kaspa GPU Mining
# Algorithm: k Heavy Hash
# Miner: bzminer
# =============================================================================

set -euo pipefail

# ─── Directories ──────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LOG_DIR="$REPO_ROOT/data/logs"

# ─── Coin-specific defaults ───────────────────────────────────────────────────
COIN="kaspa"
BINARY_NAME="bzminer"
BINARY_DEFAULT="binaries/mining/nodes/kaspa/bzminer_v24.0.1_linux/bzminer"
LOG_FILE="$LOG_DIR/${COIN}.log"
API_PORT="${KASPA_API_PORT:-4014}"

# ─── Pre-flight checks ────────────────────────────────────────────────────────
preflight_check() {
    mkdir -p "$LOG_DIR"

    # Load mining env (XDG config first, then repo .env)
    # shellcheck source=load-env.sh
    source "$SCRIPT_DIR/load-env.sh"

    # Check wallet (with backward compat for KASPA_WALLET_ADDRESS)
    local wallet="${KASPA_WALLET:-${KASPA_WALLET_ADDRESS:-}}"
    if [ -z "$wallet" ]; then
        echo "ERROR: KASPA_WALLET not set in mining env"
        echo "Set KASPA_WALLET=<your_kaspa_address> in ~/.config/theseus-quarry/mining.env"
        exit 1
    fi

    # Check binary
    local bin="${KASPA_BIN:-$REPO_ROOT/$BINARY_DEFAULT}"
    if [ ! -f "$bin" ]; then
        echo "ERROR: $bin not found"
        echo "Set KASPA_BIN in .env or place ${BINARY_NAME} at $BINARY_DEFAULT"
        exit 1
    fi
    if [ ! -x "$bin" ]; then
        chmod +x "$bin"
    fi

    check_port_free "$API_PORT" "Kaspa" "KASPA_API_PORT"
}

# ─── Start miner ─────────────────────────────────────────────────────────────
start_miner() {
    local bin="${KASPA_BIN:-$REPO_ROOT/$BINARY_DEFAULT}"
    local wallet="${KASPA_WALLET:-${KASPA_WALLET_ADDRESS:-}}"
    local pool="${KASPA_POOL:-stratum+tcp://127.0.0.1:16110}"

    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Starting ${COIN} miner" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Wallet: ${wallet:0:24}..." | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Pool: $pool" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Log: $LOG_FILE" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] ───────────────────────────────────────" | tee -a "$LOG_FILE"

    # Launch miner (solo mining via direct node connection)
    exec "$bin" \
        -a kaspa \
        -w "$wallet" \
        -p "node+tcp://${pool#*://}" \
        --nc 1 \
        --http_port "$API_PORT" \
        >> "$LOG_FILE" 2>&1
}

# ─── Main ────────────────────────────────────────────────────────────────────
preflight_check
start_miner

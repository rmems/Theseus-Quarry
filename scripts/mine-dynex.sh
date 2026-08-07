#!/usr/bin/env bash
# =============================================================================
# mine-dynex.sh — Dynex GPU Mining (PoUW)
# Algorithm: DynexSolve
# Miner: onezerominer
# =============================================================================

set -euo pipefail

# ─── Directories ──────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LOG_DIR="$REPO_ROOT/data/logs"

# ─── Coin-specific defaults ───────────────────────────────────────────────────
COIN="dynex"
BINARY_NAME="onezerominer"
BINARY_DEFAULT="binaries/mining/onezerominer"
BINARY_FALLBACK="binaries/mining/onezerominer-linux/onezerominer"
LOG_FILE="$LOG_DIR/${COIN}.log"
API_PORT="${DYNEX_API_PORT:-3010}"

# ─── Pre-flight checks ────────────────────────────────────────────────────────
preflight_check() {
    mkdir -p "$LOG_DIR"

    # Load mining env (XDG config first, then repo .env)
    # shellcheck source=load-env.sh
    source "$SCRIPT_DIR/load-env.sh"

    # Check wallet (with backward compat for SHIP_WALLET)
    local wallet="${DYNEX_WALLET:-${SHIP_WALLET:-}}"
    if [ -z "$wallet" ]; then
        echo "ERROR: DYNEX_WALLET not set in mining env"
        echo "Set DYNEX_WALLET=<your_dynx_address> in ~/.config/theseus-quarry/mining.env"
        exit 1
    fi

    # Check binary
    local bin
    bin="$(resolve_dynex_bin)" || exit 1
    if [ ! -x "$bin" ]; then
        chmod +x "$bin"
    fi

    check_port_free "${DYNEX_API_PORT:-$API_PORT}" "Dynex" "DYNEX_API_PORT"
}

resolve_dynex_bin() {
    if [ -n "${DYNEX_BIN:-}" ] && [ -f "$DYNEX_BIN" ]; then
        echo "$DYNEX_BIN"
        return 0
    fi
    if [ -f "$REPO_ROOT/$BINARY_DEFAULT" ]; then
        echo "$REPO_ROOT/$BINARY_DEFAULT"
        return 0
    fi
    if [ -f "$REPO_ROOT/$BINARY_FALLBACK" ]; then
        echo "$REPO_ROOT/$BINARY_FALLBACK"
        return 0
    fi
    echo "ERROR: ${BINARY_NAME} not found under binaries/mining/" >&2
    echo "Set DYNEX_BIN or place ${BINARY_NAME} at $BINARY_DEFAULT" >&2
    return 1
}

# ─── Start miner ─────────────────────────────────────────────────────────────
start_miner() {
    local bin
    bin="$(resolve_dynex_bin)" || exit 1
    local wallet="${DYNEX_WALLET:-${SHIP_WALLET:-}}"
    local pool="${DYNEX_POOL:-stratum+tcp://us3.dynex.herominers.com:1120}"
    local device="${DYNEX_DEVICE:-0}"
    local worker="${SHIP_WORKER_NAME:-ship-of-theseus}"

    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Starting ${COIN} miner" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Wallet: ${wallet:0:24}..." | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Pool: $pool" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Device: GPU $device" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Worker: $worker" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Log: $LOG_FILE" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] ───────────────────────────────────────" | tee -a "$LOG_FILE"

    # Launch miner
    exec "$bin" \
        --algo dynex \
        --pool "$pool" \
        --wallet "$wallet" \
        --devices "$device" \
        --worker "$worker" \
        --api-port "$API_PORT" \
        --disable-telemetry \
        --log-file "$LOG_FILE"
}

# ─── Main ────────────────────────────────────────────────────────────────────
preflight_check
start_miner

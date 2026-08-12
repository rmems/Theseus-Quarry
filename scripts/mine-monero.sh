#!/usr/bin/env bash
# =============================================================================
# mine-monero.sh — Monero CPU Mining
# Algorithm: RandomX
# Miner: SRBMiner-Multi or xmrig
# =============================================================================

set -euo pipefail

# ─── Directories ──────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LOG_DIR="$REPO_ROOT/data/logs"

# ─── Coin-specific defaults ───────────────────────────────────────────────────
COIN="monero"
BINARY_DEFAULT="binaries/mining/SRBMiner-Multi-3-2-2/SRBMiner-MULTI"
LOG_FILE="$LOG_DIR/${COIN}.log"
API_PORT="${MONERO_API_PORT:-4015}"

# ─── Pre-flight checks ────────────────────────────────────────────────────────
preflight_check() {
    mkdir -p "$LOG_DIR"

    # Load mining env (XDG config first, then repo .env)
    # shellcheck source=load-env.sh
    source "$SCRIPT_DIR/load-env.sh"

    # Check wallet (with backward compat for MONERO_WALLET_ADDRESS)
    local wallet="${MONERO_WALLET:-${MONERO_WALLET_ADDRESS:-}}"
    if [ -z "$wallet" ]; then
        echo "ERROR: MONERO_WALLET not set in mining env"
        echo "Set MONERO_WALLET=<your_xmr_address> in ~/.config/theseus-quarry/mining.env"
        exit 1
    fi

    # Check binary (SRBMiner first, then xmrig fallback, then PATH)
    local bin="${MONERO_BIN:-}"
    if [ -z "$bin" ]; then
        if [ -f "$REPO_ROOT/$BINARY_DEFAULT" ]; then
            bin="$REPO_ROOT/$BINARY_DEFAULT"
        elif [ -f "$REPO_ROOT/binaries/mining/xmrig/xmrig" ]; then
            bin="$REPO_ROOT/binaries/mining/xmrig/xmrig"
        elif bin="$(command -v xmrig 2>/dev/null)" && [ -n "$bin" ]; then
            :

        else
            echo "ERROR: No Monero miner found"
            echo "Set MONERO_BIN in .env, or place a binary at one of:"
            echo "  $REPO_ROOT/$BINARY_DEFAULT"
            echo "  $REPO_ROOT/binaries/mining/xmrig/xmrig"
            echo "  or ensure xmrig is on PATH"
            exit 1
        fi
    fi

    # Require a regular file that is executable (reject dirs/symlinks-to-dirs).
    if [ ! -f "$bin" ]; then
        echo "ERROR: $bin not found (need a regular file, not a directory)"
        exit 1
    fi
    if [ ! -x "$bin" ]; then
        chmod +x "$bin" || {
            echo "ERROR: $bin is not executable"
            exit 1
        }
    fi
    # Export resolved path so start_miner reuses the same binary (incl. PATH hits).
    if [ -z "${MONERO_BIN:-}" ]; then
        export MONERO_BIN="$bin"
    fi

    check_port_free "${MONERO_API_PORT:-$API_PORT}" "Monero" "MONERO_API_PORT"
}

# ─── Start miner ─────────────────────────────────────────────────────────────
start_miner() {
    local bin="${MONERO_BIN:-}"
    if [ -z "$bin" ]; then
        if [ -f "$REPO_ROOT/$BINARY_DEFAULT" ]; then
            bin="$REPO_ROOT/$BINARY_DEFAULT"
        else
            bin="$REPO_ROOT/binaries/mining/xmrig/xmrig"
        fi
    fi
    local wallet="${MONERO_WALLET:-${MONERO_WALLET_ADDRESS:-}}"
    local pool="${MONERO_POOL:-stratum+tcp://xmr.2miners.com:12212}"
    local threads="${MONERO_THREADS:-4}"
    local worker="${SHIP_WORKER_NAME:-ship-of-theseus}"

    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Starting ${COIN} miner" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Wallet: ${wallet:0:24}..." | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Pool: $pool" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Threads: $threads" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Worker: $worker" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Log: $LOG_FILE" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Binary: $bin" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] ───────────────────────────────────────" | tee -a "$LOG_FILE"

    # Detect miner type and launch accordingly
    if [[ "$bin" == *"SRBMiner"* ]]; then
        exec "$bin" \
            --algorithm randomx \
            --pool "$pool" \
            --wallet "$wallet" \
            --cpu-threads "$threads" \
            --api-enable \
            --api-port "$API_PORT" \
            >> "$LOG_FILE" 2>&1
    else
        exec "$bin" \
            -o "$pool" \
            -u "$wallet.$worker" \
            -t "$threads" \
            --http-port "$API_PORT" \
            >> "$LOG_FILE" 2>&1
    fi
}

# ─── Main ────────────────────────────────────────────────────────────────────
preflight_check
start_miner

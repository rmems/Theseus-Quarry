#!/usr/bin/env bash
# =============================================================================
# mine-verus.sh — Verus CPU Mining
# Algorithm: VerusHash
# Miner: hellminer (primary) or SRBMiner (fallback)
# =============================================================================

set -euo pipefail

# ─── Directories ──────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LOG_DIR="$REPO_ROOT/data/logs"

# ─── Coin-specific defaults ───────────────────────────────────────────────────
COIN="verus"
BINARY_NAME="hellminer"
BINARY_DEFAULT="binaries/mining/nodes/verus/bin/hellminer"
FALLBACK_BINARY="binaries/mining/SRBMiner-Multi-3-2-2/SRBMiner-MULTI"
LOG_FILE="$LOG_DIR/${COIN}.log"

# ─── Pre-flight checks ────────────────────────────────────────────────────────
preflight_check() {
    mkdir -p "$LOG_DIR"

    # Load mining env (XDG config first, then repo .env)
    # shellcheck source=load-env.sh
    source "$SCRIPT_DIR/load-env.sh"

    # Check wallet (with backward compat for VRSC_WALLET_ADDRESS)
    local wallet="${VRSC_WALLET:-${VRSC_WALLET_ADDRESS:-}}"
    if [ -z "$wallet" ]; then
        echo "ERROR: VRSC_WALLET not set in mining env"
        echo "Set VRSC_WALLET=<your_vrsc_address> in ~/.config/theseus-quarry/mining.env"
        exit 1
    fi

    # Check binary (hellminer first, then SRBMiner fallback)
    local bin="${VRSC_BIN:-}"
    if [ -z "$bin" ]; then
        if [ -f "$REPO_ROOT/$BINARY_DEFAULT" ]; then
            bin="$REPO_ROOT/$BINARY_DEFAULT"
        elif [ -f "$REPO_ROOT/$FALLBACK_BINARY" ]; then
            bin="$REPO_ROOT/$FALLBACK_BINARY"
        else
            echo "ERROR: No Verus miner found"
            echo "Set VRSC_BIN in .env or place ${BINARY_NAME}/SRBMiner in binaries/mining/"
            exit 1
        fi
    fi

    if [ ! -f "$bin" ]; then
        echo "ERROR: $bin not found"
        exit 1
    fi
    if [ ! -x "$bin" ]; then
        chmod +x "$bin"
    fi
}

# ─── Start miner ─────────────────────────────────────────────────────────────
start_miner() {
    local bin="${VRSC_BIN:-}"
    if [ -z "$bin" ]; then
        if [ -f "$REPO_ROOT/$BINARY_DEFAULT" ]; then
            bin="$REPO_ROOT/$BINARY_DEFAULT"
        else
            bin="$REPO_ROOT/$FALLBACK_BINARY"
        fi
    fi
    local wallet="${VRSC_WALLET:-${VRSC_WALLET_ADDRESS:-}}"
    local pool="${VRSC_POOL:-stratum+tcp://na.luckpool.net:3956}"
    local threads="${VRSC_THREADS:-4}"
    local nice="${VRSC_NICE:-15}"
    local worker="${SHIP_WORKER_NAME:-ship9950x}"

    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Starting ${COIN} miner" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Wallet: ${wallet:0:24}..." | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Pool: $pool" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Threads: $threads" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] nice: $nice" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Worker: $worker" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Log: $LOG_FILE" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] Binary: $bin" | tee -a "$LOG_FILE"
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] ───────────────────────────────────────" | tee -a "$LOG_FILE"

    # Detect miner type and launch accordingly
    if [[ "$bin" == *"hellminer"* ]]; then
        # hellminer
        exec nice -n "$nice" "$bin" \
            --cpu \
            -c "$pool" \
            -u "$wallet.$worker" \
            -p x \
            --threads "$threads" \
            >> "$LOG_FILE" 2>&1
    else
        # SRBMiner
        exec nice -n "$nice" "$bin" \
            --algorithm verushash \
            --pool "$pool" \
            --wallet "$wallet.$worker" \
            --cpu-threads "$threads" \
            >> "$LOG_FILE" 2>&1
    fi
}

# ─── Main ────────────────────────────────────────────────────────────────────
preflight_check
start_miner

#!/usr/bin/env bash
# Shared env loader for mine-*.sh scripts.
# Prefer XDG config (canonical secrets location); fall back to repo .env.
#
# Usage from a mine-*.sh after setting REPO_ROOT:
#   # shellcheck source=load-env.sh
#   source "$REPO_ROOT/scripts/load-env.sh"

_theseus_config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/theseus-quarry"
_theseus_mining_env="${THESEUS_MINING_ENV:-$_theseus_config_dir/mining.env}"
_theseus_binaries_env="${THESEUS_BINARIES_ENV:-$_theseus_config_dir/binaries.env}"
_theseus_repo_env="${REPO_ROOT:-.}/.env"

_loaded=
if [ -f "$_theseus_mining_env" ]; then
    set -a
    # shellcheck disable=SC1090
    source "$_theseus_mining_env"
    set +a
    _loaded=1
fi
if [ -f "$_theseus_binaries_env" ]; then
    set -a
    # shellcheck disable=SC1090
    source "$_theseus_binaries_env"
    set +a
    _loaded=1
fi
# Repo-local .env overrides config (still gitignored)
if [ -f "$_theseus_repo_env" ]; then
    set -a
    # shellcheck disable=SC1090
    source "$_theseus_repo_env"
    set +a
    _loaded=1
fi

if [ -z "$_loaded" ]; then
    # Non-fatal: allow env-only launches (MONERO_WALLET already exported, etc.).
    # Callers (mine-*.sh preflight) still require wallet/binary themselves.
    echo "WARNING: no mining env file found."
    echo "  Expected: $_theseus_mining_env"
    echo "  Or:       $_theseus_repo_env"
    echo "  Continuing with process environment only."
fi

unset _theseus_config_dir _theseus_mining_env _theseus_binaries_env _theseus_repo_env _loaded

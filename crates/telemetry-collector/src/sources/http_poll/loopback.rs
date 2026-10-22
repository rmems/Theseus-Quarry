use tracing::{error, warn};

/// Returns `true` when `host` is loopback (`127.0.0.1`, `localhost`, or `::1`).
pub fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

/// When `loopback_only` is true, require node RPC URLs to target loopback hosts.
pub fn validate_node_rpc_url(label: &str, url: &str, loopback_only: bool) -> anyhow::Result<()> {
    if !loopback_only {
        warn!(
            label,
            "NODE_RPC_LOOPBACK_ONLY=false; node RPC URL is not restricted to loopback — use only trusted local endpoints"
        );
        return Ok(());
    }
    let parsed = reqwest::Url::parse(url)
        .map_err(|e| anyhow::anyhow!("Invalid node RPC URL for {label}: {e}"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("Node RPC URL for {label} must include a host"))?;
    anyhow::ensure!(
        is_loopback_host(host),
        "Node RPC URL for {label} must be loopback (127.0.0.1, localhost, or ::1) when NODE_RPC_LOOPBACK_ONLY is true; got host {host}"
    );
    Ok(())
}

/// Accept a node RPC URL when it passes policy, or log and return `None` so the collector
/// can keep running (GPU governor and miner polls unaffected).
pub fn resolve_node_rpc_url(label: &str, url: String, loopback_only: bool) -> Option<String> {
    match validate_node_rpc_url(label, &url, loopback_only) {
        Ok(()) => Some(url),
        Err(e) => {
            error!(
                label,
                error = %e,
                "Disabling node RPC source; fix URL or set NODE_RPC_LOOPBACK_ONLY=false"
            );
            None
        }
    }
}

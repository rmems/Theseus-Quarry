use tracing::warn;

/// Returns `true` when `host` is loopback (`127.0.0.1`, `localhost`, or `::1`).
pub fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

/// When `loopback_only` is true, require node RPC URLs to target loopback hosts.
pub fn validate_node_rpc_url(label: &str, url: &str, loopback_only: bool) -> anyhow::Result<()> {
    if !loopback_only {
        warn!(
            label,
            url = url,
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

pub fn validate_node_rpc_urls(
    loopback_only: bool,
    endpoints: &[(&str, &str)],
) -> anyhow::Result<()> {
    for (label, url) in endpoints {
        validate_node_rpc_url(label, url, loopback_only)?;
    }
    Ok(())
}

//! Bounded HTTP reads for miner and node adapters.
//!
//! - Per-response body cap: [`MAX_RESPONSE_BODY_BYTES`] (1 MiB) before JSON parse.
//! - Client default timeout: [`CLIENT_REQUEST_TIMEOUT`] (2s); sequential probes share
//!   a per-tick budget of `poll_interval - PROBE_INTERVAL_SLACK` via [`ProbeBudget`].
//! - Non-JSON `Content-Type` (when present) is skipped; missing `Content-Type` is allowed
//!   if the body parses as a JSON object.

mod budget;
mod fetch;
mod loopback;
mod skip;

#[cfg(test)]
mod tests;

use std::time::Duration;

pub use budget::ProbeBudget;
pub use fetch::{HttpJsonRequest, get_json_object, post_json_object};

/// Build a bounded JSON HTTP request descriptor for miner/node adapters.
pub fn json_request<'a>(
    adapter: &'static str,
    url: &'a str,
    timeout: Duration,
) -> HttpJsonRequest<'a> {
    HttpJsonRequest {
        adapter,
        url,
        timeout,
    }
}
pub use loopback::{node_url_with_path, resolve_node_rpc_url};

/// Node RPC client. Redirects are disabled so a loopback endpoint cannot send the
/// collector to a non-loopback host.
pub fn node_http_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(CLIENT_REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

/// Maximum response body size read from miner/node HTTP before JSON parse.
pub const MAX_RESPONSE_BODY_BYTES: usize = 1024 * 1024;

/// Default per-request timeout on the shared `reqwest::Client` (see `main.rs`).
pub const CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_millis(2000);

/// Slack subtracted from the poll interval when budgeting sequential HTTP probes
/// within one collector tick (default 5s interval → 4.5s probe budget).
pub const PROBE_INTERVAL_SLACK: Duration = Duration::from_millis(500);

pub(crate) use skip::{SkipClass, log_skip};

pub(crate) fn content_type_allows_json(content_type: Option<&str>) -> bool {
    let Some(raw) = content_type else {
        return true;
    };
    let mime = raw
        .split(';')
        .next()
        .unwrap_or(raw)
        .trim()
        .to_ascii_lowercase();
    mime == "application/json" || mime.ends_with("+json")
}

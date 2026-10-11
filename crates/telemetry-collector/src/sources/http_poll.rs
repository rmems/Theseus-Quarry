//! Bounded HTTP reads for miner and node adapters.
//!
//! - Per-response body cap: [`MAX_RESPONSE_BODY_BYTES`] (1 MiB) before JSON parse.
//! - Client default timeout: [`CLIENT_REQUEST_TIMEOUT`] (2s); sequential probes share
//!   a per-tick budget of `poll_interval - PROBE_INTERVAL_SLACK` via [`ProbeBudget`].
//! - Non-JSON `Content-Type` (when present) is skipped; missing `Content-Type` is allowed
//!   if the body parses as a JSON object.

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;
use tracing::warn;

/// Maximum response body size read from miner/node HTTP before JSON parse.
pub const MAX_RESPONSE_BODY_BYTES: usize = 1024 * 1024;

/// Default per-request timeout on the shared `reqwest::Client` (see `main.rs`).
pub const CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_millis(2000);

/// Slack subtracted from the poll interval when budgeting sequential HTTP probes
/// within one collector tick (default 5s interval → 4.5s probe budget).
pub const PROBE_INTERVAL_SLACK: Duration = Duration::from_millis(500);

/// Remaining wall time for sequential probes in one poll tick.
#[derive(Clone, Copy, Debug)]
pub struct ProbeBudget {
    deadline: tokio::time::Instant,
}

impl ProbeBudget {
    pub fn from_poll_interval_secs(interval_secs: u64) -> Self {
        let total = Duration::from_secs(interval_secs).saturating_sub(PROBE_INTERVAL_SLACK);
        Self {
            deadline: tokio::time::Instant::now() + total,
        }
    }

    /// Timeout for the next HTTP call, or `None` when the tick budget is exhausted.
    pub fn request_timeout(&self) -> Option<Duration> {
        let remaining = self
            .deadline
            .saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        Some(remaining.min(CLIENT_REQUEST_TIMEOUT))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipClass {
    HttpSend,
    HttpStatus,
    WrongContentType,
    OversizeBody,
    InvalidJson,
    NotJsonObject,
}

impl SkipClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::HttpSend => "http_send",
            Self::HttpStatus => "http_status",
            Self::WrongContentType => "wrong_content_type",
            Self::OversizeBody => "oversize_body",
            Self::InvalidJson => "invalid_json",
            Self::NotJsonObject => "not_json_object",
        }
    }
}

fn status_class_from_u16(code: u16) -> &'static str {
    match code {
        200..=299 => "success",
        400..=499 => "client_error",
        500..=599 => "server_error",
        _ => "other",
    }
}

fn log_skip(adapter: &str, class: SkipClass, http_status: Option<u16>) {
    if let Some(code) = http_status {
        warn!(
            adapter,
            skip = class.as_str(),
            http_status = code,
            status_class = status_class_from_u16(code),
            "HTTP poll skipped"
        );
    } else {
        warn!(adapter, skip = class.as_str(), "HTTP poll skipped");
    }
}

pub fn content_type_allows_json(content_type: Option<&str>) -> bool {
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

async fn read_body_capped(response: reqwest::Response, max: usize) -> Result<Vec<u8>, SkipClass> {
    if let Some(len) = response.content_length()
        && len as usize > max
    {
        return Err(SkipClass::OversizeBody);
    }

    let mut buf = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| SkipClass::HttpSend)?;
        if buf.len().saturating_add(chunk.len()) > max {
            return Err(SkipClass::OversizeBody);
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

async fn json_object_from_response(
    adapter: &str,
    response: reqwest::Response,
) -> Result<Value, SkipClass> {
    let status = response.status();
    if !status.is_success() {
        log_skip(adapter, SkipClass::HttpStatus, Some(status.as_u16()));
        return Err(SkipClass::HttpStatus);
    }

    if !content_type_allows_json(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
    ) {
        log_skip(adapter, SkipClass::WrongContentType, Some(status.as_u16()));
        return Err(SkipClass::WrongContentType);
    }

    let body = match read_body_capped(response, MAX_RESPONSE_BODY_BYTES).await {
        Ok(b) => b,
        Err(SkipClass::OversizeBody) => {
            log_skip(adapter, SkipClass::OversizeBody, Some(status.as_u16()));
            return Err(SkipClass::OversizeBody);
        }
        Err(e) => return Err(e),
    };

    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => {
            log_skip(adapter, SkipClass::InvalidJson, Some(status.as_u16()));
            return Err(SkipClass::InvalidJson);
        }
    };

    if !value.is_object() {
        log_skip(adapter, SkipClass::NotJsonObject, Some(status.as_u16()));
        return Err(SkipClass::NotJsonObject);
    }

    Ok(value)
}

pub async fn get_json_object(
    adapter: &str,
    client: &Client,
    url: &str,
    timeout: Duration,
) -> Option<Value> {
    let response = match client.get(url).timeout(timeout).send().await {
        Ok(r) => r,
        Err(_) => {
            log_skip(adapter, SkipClass::HttpSend, None);
            return None;
        }
    };
    json_object_from_response(adapter, response).await.ok()
}

pub async fn post_json_object(
    adapter: &str,
    client: &Client,
    url: &str,
    body: &Value,
    timeout: Duration,
) -> Option<Value> {
    let response = match client.post(url).json(body).timeout(timeout).send().await {
        Ok(r) => r,
        Err(_) => {
            log_skip(adapter, SkipClass::HttpSend, None);
            return None;
        }
    };
    json_object_from_response(adapter, response).await.ok()
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn content_type_json_variants() {
        assert!(content_type_allows_json(None));
        assert!(content_type_allows_json(Some("application/json")));
        assert!(content_type_allows_json(Some(
            "application/json; charset=utf-8"
        )));
        assert!(content_type_allows_json(Some("application/vnd.api+json")));
        assert!(!content_type_allows_json(Some("text/html")));
        assert!(!content_type_allows_json(Some("text/plain")));
    }

    #[test]
    fn loopback_hosts() {
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("example.com"));
    }

    #[test]
    fn validate_node_rpc_rejects_remote_when_enabled() {
        let err = validate_node_rpc_url("quai", "http://203.0.113.1:9001", true).unwrap_err();
        assert!(err.to_string().contains("loopback"));
    }

    #[tokio::test]
    async fn rejects_oversized_body() {
        let server = MockServer::start().await;
        let huge = "x".repeat(MAX_RESPONSE_BODY_BYTES + 1);
        Mock::given(method("GET"))
            .and(path("/big"))
            .respond_with(ResponseTemplate::new(200).set_body_string(huge))
            .mount(&server)
            .await;

        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let url = format!("{}/big", server.uri());
        assert!(
            get_json_object("test_adapter", &client, &url, Duration::from_secs(5))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn rejects_non_json_content_type() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/html"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html")
                    .set_body_string(r#"{"ok":true}"#),
            )
            .mount(&server)
            .await;

        let client = Client::builder().build().unwrap();
        let url = format!("{}/html", server.uri());
        assert!(
            get_json_object("test_adapter", &client, &url, Duration::from_secs(2))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn allows_missing_content_type_when_json_object() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let body = br#"{"height":42}"#;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(header.as_bytes()).await;
            let _ = socket.write_all(body).await;
            let mut _buf = [0u8; 64];
            let _ = socket.read(&mut _buf).await;
        });

        let client = Client::builder().build().unwrap();
        let url = format!("http://{addr}/");
        let v = get_json_object("test_adapter", &client, &url, Duration::from_secs(2))
            .await
            .expect("expected JSON");
        assert_eq!(v.get("height").and_then(|h| h.as_u64()), Some(42));
    }

    #[tokio::test]
    async fn slow_body_times_out_without_unbounded_wait() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/slow"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_secs(30))
                    .set_body_string(r#"{"ok":true}"#),
            )
            .mount(&server)
            .await;

        let client = Client::builder().build().unwrap();
        let url = format!("{}/slow", server.uri());
        let started = std::time::Instant::now();
        assert!(
            get_json_object("test_adapter", &client, &url, Duration::from_millis(500))
                .await
                .is_none()
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn probe_budget_caps_request_timeout() {
        let budget = ProbeBudget::from_poll_interval_secs(5);
        let t = budget.request_timeout().expect("budget");
        assert!(t <= CLIENT_REQUEST_TIMEOUT);
        assert!(t <= Duration::from_secs(5));
    }
}

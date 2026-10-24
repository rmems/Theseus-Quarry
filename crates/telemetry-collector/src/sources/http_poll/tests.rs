use super::loopback::{
    append_url_path, is_loopback_host, resolve_node_rpc_url, validate_node_rpc_url,
};
use super::{
    CLIENT_REQUEST_TIMEOUT, MAX_RESPONSE_BODY_BYTES, ProbeBudget, content_type_allows_json,
    get_json_object, json_request, node_http_client,
};
use reqwest::Client;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn wiremock_get_url(route: &str, response: ResponseTemplate) -> String {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(response)
        .mount(&server)
        .await;
    format!("{}{}", server.uri(), route)
}

async fn assert_get_json_skipped(url: &str, timeout: Duration) {
    let client = Client::builder().build().unwrap();
    assert!(
        get_json_object(&client, json_request("test_adapter", url, timeout))
            .await
            .is_none()
    );
}

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

#[test]
fn resolve_node_rpc_disables_remote_when_loopback_required() {
    assert!(resolve_node_rpc_url("quai", "http://203.0.113.1:9001".into(), true).is_none());
    assert_eq!(
        resolve_node_rpc_url("quai", "http://127.0.0.1:9001".into(), true),
        Some("http://127.0.0.1:9001".into())
    );
}

#[test]
fn append_url_path_preserves_query() {
    let joined = append_url_path("http://127.0.0.1:17336?token=abc", "getheight").unwrap();
    let url = reqwest::Url::parse(&joined).unwrap();
    assert_eq!(url.path(), "/getheight");
    assert_eq!(url.query(), Some("token=abc"));

    let nested = append_url_path("http://127.0.0.1:8099/api/?x=1", "tick-info").unwrap();
    let nested_url = reqwest::Url::parse(&nested).unwrap();
    assert_eq!(nested_url.path(), "/api/tick-info");
    assert_eq!(nested_url.query(), Some("x=1"));
}

#[tokio::test]
async fn rejects_oversized_body() {
    let huge = "x".repeat(MAX_RESPONSE_BODY_BYTES + 1);
    let url = wiremock_get_url(
        "/big",
        ResponseTemplate::new(200).set_body_raw(huge, "application/json"),
    )
    .await;
    assert_get_json_skipped(&url, Duration::from_secs(5)).await;
}

#[tokio::test]
async fn node_client_does_not_follow_redirect() {
    let remote = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/secret"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(r#"{"leaked":true}"#, "application/json"),
        )
        .mount(&remote)
        .await;

    let local = MockServer::start().await;
    let location = format!("{}/secret", remote.uri());
    Mock::given(method("GET"))
        .and(path("/getheight"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", location))
        .mount(&local)
        .await;

    let client = node_http_client().unwrap();
    let url = format!("{}/getheight", local.uri());
    let got = get_json_object(&client, json_request("dynex", &url, Duration::from_secs(2))).await;
    assert!(got.is_none());
}

#[tokio::test]
async fn rejects_non_json_content_type() {
    let url = wiremock_get_url(
        "/html",
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/html")
            .set_body_string(r#"{"ok":true}"#),
    )
    .await;
    assert_get_json_skipped(&url, Duration::from_secs(2)).await;
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
    let v = get_json_object(
        &client,
        json_request("test_adapter", &url, Duration::from_secs(2)),
    )
    .await
    .expect("expected JSON");
    assert_eq!(v.get("height").and_then(|h| h.as_u64()), Some(42));
}

#[tokio::test]
async fn slow_body_times_out_without_unbounded_wait() {
    let url = wiremock_get_url(
        "/slow",
        ResponseTemplate::new(200)
            .set_delay(Duration::from_secs(30))
            .set_body_string(r#"{"ok":true}"#),
    )
    .await;
    let started = std::time::Instant::now();
    assert_get_json_skipped(&url, Duration::from_millis(500)).await;
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn probe_budget_caps_request_timeout() {
    let budget = ProbeBudget::from_poll_interval_secs(5);
    let t = budget.request_timeout().expect("budget");
    assert!(t <= CLIENT_REQUEST_TIMEOUT);
    assert!(t <= Duration::from_secs(5));
}

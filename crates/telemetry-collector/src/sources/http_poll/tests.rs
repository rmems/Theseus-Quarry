use super::loopback::{is_loopback_host, validate_node_rpc_url};
use super::{
    CLIENT_REQUEST_TIMEOUT, HttpJsonRequest, MAX_RESPONSE_BODY_BYTES, ProbeBudget,
    content_type_allows_json, get_json_object,
};
use reqwest::Client;
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
        get_json_object(
            &client,
            HttpJsonRequest {
                adapter: "test_adapter",
                url: &url,
                timeout: Duration::from_secs(5),
            },
        )
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
        get_json_object(
            &client,
            HttpJsonRequest {
                adapter: "test_adapter",
                url: &url,
                timeout: Duration::from_secs(2),
            },
        )
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
    let v = get_json_object(
        &client,
        HttpJsonRequest {
            adapter: "test_adapter",
            url: &url,
            timeout: Duration::from_secs(2),
        },
    )
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
        get_json_object(
            &client,
            HttpJsonRequest {
                adapter: "test_adapter",
                url: &url,
                timeout: Duration::from_millis(500),
            },
        )
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

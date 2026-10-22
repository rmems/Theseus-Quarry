use super::{MAX_RESPONSE_BODY_BYTES, SkipClass, content_type_allows_json, log_skip};
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::Value;
use std::time::Duration;

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

fn parse_json_object_body(
    adapter: &str,
    status_code: u16,
    body: &[u8],
) -> Result<Value, SkipClass> {
    let value: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => {
            log_skip(adapter, SkipClass::InvalidJson, Some(status_code));
            return Err(SkipClass::InvalidJson);
        }
    };

    if !value.is_object() {
        log_skip(adapter, SkipClass::NotJsonObject, Some(status_code));
        return Err(SkipClass::NotJsonObject);
    }

    Ok(value)
}

async fn json_object_from_response(
    adapter: &str,
    response: reqwest::Response,
) -> Result<Value, SkipClass> {
    let status = response.status();
    let status_code = status.as_u16();
    if !status.is_success() {
        log_skip(adapter, SkipClass::HttpStatus, Some(status_code));
        return Err(SkipClass::HttpStatus);
    }

    if !content_type_allows_json(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
    ) {
        log_skip(adapter, SkipClass::WrongContentType, Some(status_code));
        return Err(SkipClass::WrongContentType);
    }

    let body = match read_body_capped(response, MAX_RESPONSE_BODY_BYTES).await {
        Ok(b) => b,
        Err(SkipClass::OversizeBody) => {
            log_skip(adapter, SkipClass::OversizeBody, Some(status_code));
            return Err(SkipClass::OversizeBody);
        }
        Err(e) => return Err(e),
    };

    parse_json_object_body(adapter, status_code, &body)
}

async fn send_and_parse(
    adapter: &str,
    send: impl std::future::Future<Output = Result<reqwest::Response, reqwest::Error>>,
) -> Option<Value> {
    let response = match send.await {
        Ok(r) => r,
        Err(_) => {
            log_skip(adapter, SkipClass::HttpSend, None);
            return None;
        }
    };
    json_object_from_response(adapter, response).await.ok()
}

pub async fn get_json_object(
    adapter: &str,
    client: &Client,
    url: &str,
    timeout: Duration,
) -> Option<Value> {
    send_and_parse(adapter, client.get(url).timeout(timeout).send()).await
}

pub async fn post_json_object(
    adapter: &str,
    client: &Client,
    url: &str,
    body: &Value,
    timeout: Duration,
) -> Option<Value> {
    send_and_parse(adapter, client.post(url).json(body).timeout(timeout).send()).await
}

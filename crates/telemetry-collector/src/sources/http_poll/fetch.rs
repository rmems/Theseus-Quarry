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

fn response_status_code(adapter: &str, response: &reqwest::Response) -> Result<u16, SkipClass> {
    let status_code = response.status().as_u16();
    if response.status().is_success() {
        return Ok(status_code);
    }
    log_skip(adapter, SkipClass::HttpStatus, Some(status_code));
    Err(SkipClass::HttpStatus)
}

fn ensure_json_content_type(
    adapter: &str,
    status_code: u16,
    response: &reqwest::Response,
) -> Result<(), SkipClass> {
    if content_type_allows_json(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
    ) {
        return Ok(());
    }
    log_skip(adapter, SkipClass::WrongContentType, Some(status_code));
    Err(SkipClass::WrongContentType)
}

async fn response_json_bytes(
    adapter: &str,
    status_code: u16,
    response: reqwest::Response,
) -> Result<Vec<u8>, SkipClass> {
    match read_body_capped(response, MAX_RESPONSE_BODY_BYTES).await {
        Ok(b) => Ok(b),
        Err(SkipClass::OversizeBody) => {
            log_skip(adapter, SkipClass::OversizeBody, Some(status_code));
            Err(SkipClass::OversizeBody)
        }
        Err(e) => Err(e),
    }
}

async fn json_object_from_response(
    adapter: &str,
    response: reqwest::Response,
) -> Result<Value, SkipClass> {
    let status_code = response_status_code(adapter, &response)?;
    ensure_json_content_type(adapter, status_code, &response)?;
    let body = response_json_bytes(adapter, status_code, response).await?;
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

/// GET/POST target for bounded JSON object reads.
pub struct HttpJsonRequest<'a> {
    pub adapter: &'static str,
    pub url: &'a str,
    pub timeout: Duration,
}

pub async fn get_json_object(client: &Client, req: HttpJsonRequest<'_>) -> Option<Value> {
    send_and_parse(req.adapter, client.get(req.url).timeout(req.timeout).send()).await
}

pub async fn post_json_object(
    client: &Client,
    req: HttpJsonRequest<'_>,
    body: &Value,
) -> Option<Value> {
    send_and_parse(
        req.adapter,
        client.post(req.url).json(body).timeout(req.timeout).send(),
    )
    .await
}

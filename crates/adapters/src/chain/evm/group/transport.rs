//! Size-bounded HTTP adapter retaining metadata until typed classification completes.
use super::Failure;
use reqwest::{Client, StatusCode};
use serde_json::Value;
use std::time::{Duration, SystemTime};

/// Maximum decoded RPC body. Streaming prevents unbounded allocation even without Content-Length.
pub const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;
/// Raw response metadata, never exposed in logs or client errors.
pub struct HttpReply {
    /// Original HTTP status, including non-2xx JSON-RPC replies.
    pub status: u16,
    /// Parsed delta-seconds or HTTP-date retry delay.
    pub retry_after: Option<Duration>,
    /// Size-bounded parsed body; never formatted into errors.
    pub body: Value,
}
/// Creates a client with no redirects or hidden retries.
pub fn client() -> Result<Client, Failure> {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .no_proxy()
        .build()
        .map_err(|_| Failure::Transport)
}
/// Sends one admitted request and retains status even when its body is a JSON-RPC error.
pub async fn send(client: &Client, url: &url::Url, body: &Value) -> Result<HttpReply, Failure> {
    let mut response = client
        .post(url.clone())
        .json(body)
        .send()
        .await
        .map_err(|_| Failure::Transport)?;
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| {
            s.parse::<u64>().ok().map(Duration::from_secs).or_else(|| {
                httpdate::parse_http_date(s)
                    .ok()
                    .and_then(|d| d.duration_since(SystemTime::now()).ok())
            })
        });
    if status.is_redirection() || matches!(status.as_u16(), 401 | 403 | 408 | 413) {
        return Ok(HttpReply {
            status: status.as_u16(),
            retry_after,
            body: Value::Null,
        });
    }
    if response
        .content_length()
        .is_some_and(|size| size > u64::try_from(MAX_BODY_BYTES).unwrap_or(u64::MAX))
    {
        return bounded_reply(status, retry_after);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| Failure::Transport)? {
        if bytes
            .len()
            .checked_add(chunk.len())
            .is_none_or(|n| n > MAX_BODY_BYTES)
        {
            return bounded_reply(status, retry_after);
        }
        bytes.extend_from_slice(&chunk);
    }
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    if status == StatusCode::OK && body.is_null() {
        return Err(Failure::Malformed);
    }
    Ok(HttpReply {
        status: status.as_u16(),
        retry_after,
        body,
    })
}

fn bounded_reply(status: StatusCode, retry_after: Option<Duration>) -> Result<HttpReply, Failure> {
    if status.is_success() {
        return Err(Failure::Malformed);
    }
    Ok(HttpReply {
        status: status.as_u16(),
        retry_after,
        body: Value::Null,
    })
}

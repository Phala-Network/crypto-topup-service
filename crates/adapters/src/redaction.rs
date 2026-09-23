//! Provider URL and transport-error redaction shared by external adapters.

use std::error::Error;
use std::fmt::{self, Debug, Display, Formatter};

use alloy::transports::{RpcError, TransportError, TransportErrorKind};
use url::Url;

/// Maximum characters of a node-supplied JSON-RPC error message kept in an error.
const MAX_NODE_MESSAGE_CHARS: usize = 200;
/// Shortest credential or query value scrubbed from node messages; shorter values would
/// shred ordinary words without protecting a secret.
const MIN_SCRUBBED_VALUE_CHARS: usize = 4;
/// Shortest URL path segment treated as a possible embedded key (for example a project id).
const MIN_SCRUBBED_PATH_SEGMENT_CHARS: usize = 8;

/// A parsed provider URL whose formatting never reveals credentials, host, or path.
///
/// Formatting shows the configured provider label when one is attached, otherwise
/// `[REDACTED URL]`.
#[derive(Clone, Eq, PartialEq)]
pub struct Redacted {
    url: Url,
    provider: Option<String>,
}

impl Redacted {
    /// Parses a provider URL while retaining the value only for outbound client construction.
    pub fn parse(value: &str) -> Result<Self, url::ParseError> {
        Url::parse(value).map(|url| Self {
            url,
            provider: None,
        })
    }

    /// Labels the endpoint with its configured provider id, never derived from the URL.
    #[must_use]
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.provider = Some(provider.into());
        self
    }

    /// Returns the configured provider label, when one is attached.
    #[must_use]
    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    /// Returns the provider URL for an outbound client. Callers must not log this value.
    #[must_use]
    pub const fn expose(&self) -> &Url {
        &self.url
    }

    /// Maps a JSON-RPC client failure, keeping the node's answer but never the request URL.
    ///
    /// JSON-RPC error responses keep their code and a length-capped message, HTTP failures keep
    /// only the status code, and every other failure is reported as `transport`.
    #[must_use]
    pub fn rpc_error(
        &self,
        operation: &'static str,
        error: &TransportError,
    ) -> RedactedTransportError {
        let failure = match error {
            RpcError::ErrorResp(payload) => Failure::JsonRpc {
                code: payload.code,
                message: node_message(&self.scrub(&payload.message)),
            },
            RpcError::Transport(TransportErrorKind::HttpError(http)) => {
                Failure::HttpStatus(http.status)
            }
            _ => Failure::Transport("transport"),
        };
        RedactedTransportError::new(operation, self, failure)
    }

    /// Replaces this URL's credentials, query values, and long path segments in a
    /// provider-supplied message, in case a node or proxy echoes the request back.
    fn scrub(&self, message: &str) -> String {
        let url = &self.url;
        let mut secrets = vec![url.username().to_owned()];
        secrets.extend(url.password().map(str::to_owned));
        secrets.extend(url.query_pairs().map(|(_, value)| value.into_owned()));
        secrets.extend(
            url.query()
                .into_iter()
                .flat_map(|query| query.split('&'))
                .filter_map(|pair| pair.split_once('=').map(|(_, value)| value.to_owned())),
        );
        secrets.retain(|secret| secret.chars().count() >= MIN_SCRUBBED_VALUE_CHARS);
        secrets.extend(
            url.path_segments()
                .into_iter()
                .flatten()
                .filter(|segment| segment.chars().count() >= MIN_SCRUBBED_PATH_SEGMENT_CHARS)
                .map(str::to_owned),
        );
        // Longest first, so a value containing a shorter one is replaced whole.
        secrets.sort_unstable_by(|left, right| right.len().cmp(&left.len()).then(left.cmp(right)));
        secrets.dedup();
        secrets.iter().fold(message.to_owned(), |message, secret| {
            message.replace(secret.as_str(), "[REDACTED]")
        })
    }

    /// Maps a reqwest failure without retaining or formatting its request URL.
    #[must_use]
    pub fn request_error(
        &self,
        operation: &'static str,
        error: &reqwest::Error,
    ) -> RedactedTransportError {
        let failure = if let Some(status) = error.status() {
            Failure::HttpStatus(status.as_u16())
        } else if error.is_timeout() {
            Failure::Timeout
        } else if error.is_connect() {
            Failure::Transport("connect")
        } else if error.is_request() {
            Failure::Transport("request")
        } else if error.is_body() {
            Failure::Transport("body")
        } else if error.is_decode() {
            Failure::Transport("decode")
        } else {
            Failure::Transport("transport")
        };
        RedactedTransportError::new(operation, self, failure)
    }

    /// Maps an adapter timeout without retaining a potentially credentialed request.
    #[must_use]
    pub fn timeout_error(&self, operation: &'static str) -> RedactedTransportError {
        RedactedTransportError::new(operation, self, Failure::Timeout)
    }
}

impl Display for Redacted {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match &self.provider {
            Some(provider) => write!(formatter, "provider `{provider}`"),
            None => formatter.write_str("[REDACTED URL]"),
        }
    }
}

impl Debug for Redacted {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, formatter)
    }
}

/// The redacted reason a provider request failed.
#[derive(Clone, Eq, PartialEq)]
enum Failure {
    /// The node answered with a JSON-RPC error object.
    JsonRpc { code: i64, message: String },
    /// The provider answered with a non-success HTTP status.
    HttpStatus(u16),
    /// The request did not complete in time.
    Timeout,
    /// The request failed below HTTP; the category never includes the URL.
    Transport(&'static str),
}

impl Display for Failure {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::JsonRpc { code, message } => {
                write!(formatter, "JSON-RPC error {code}: {message}")
            }
            Self::HttpStatus(status) => write!(formatter, "HTTP {status}"),
            Self::Timeout => formatter.write_str("timeout"),
            Self::Transport(category) => formatter.write_str(category),
        }
    }
}

/// A provider failure safe to format in production logs.
///
/// It retains only the operation, the endpoint's redacted label, and the failure reason.
#[derive(Clone, Eq, PartialEq)]
pub struct RedactedTransportError {
    operation: &'static str,
    endpoint: String,
    failure: Failure,
}

impl RedactedTransportError {
    fn new(operation: &'static str, endpoint: &Redacted, failure: Failure) -> Self {
        Self {
            operation,
            endpoint: endpoint.to_string(),
            failure,
        }
    }
}

impl Display for RedactedTransportError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} failed for {} ({})",
            self.operation, self.endpoint, self.failure
        )
    }
}

impl Debug for RedactedTransportError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, formatter)
    }
}

impl Error for RedactedTransportError {}

/// Keeps a node-supplied message readable on one log line and bounded in length.
fn node_message(message: &str) -> String {
    let mut chars = message.chars().map(|character| {
        if character.is_control() {
            ' '
        } else {
            character
        }
    });
    let mut kept = chars
        .by_ref()
        .take(MAX_NODE_MESSAGE_CHARS)
        .collect::<String>();
    if chars.next().is_some() {
        kept.push('…');
    }
    kept
}

#[cfg(test)]
mod tests {
    use alloy::providers::{Provider, RootProvider};
    use axum::Router;
    use axum::http::{StatusCode, header};
    use tokio::net::TcpListener;

    use super::{MAX_NODE_MESSAGE_CHARS, Redacted, node_message};

    const SECRET: &str = "rpc-secret-token";
    const PROJECT_ID: &str = "0123456789abcdef";

    /// Serves one fixed HTTP response to every JSON-RPC request on a local port.
    async fn mock_node(status: StatusCode, body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local listener binds");
        let address = listener.local_addr().expect("listener address");
        let node = Router::new().fallback(move || async move {
            (status, [(header::CONTENT_TYPE, "application/json")], body)
        });
        tokio::spawn(async move { axum::serve(listener, node).await });
        format!("http://user:{SECRET}@{address}/rpc/{PROJECT_ID}?api_key={SECRET}")
    }

    async fn failed_block_number(url: &str, provider: &str) -> String {
        let endpoint = Redacted::parse(url)
            .expect("valid provider URL")
            .with_provider(provider);
        let provider: RootProvider = RootProvider::new_http(endpoint.expose().clone());
        let error = provider
            .get_block_number()
            .await
            .expect_err("mock node fails the request");
        let redacted = endpoint.rpc_error("block number fetch", &error);
        let display = redacted.to_string();
        let debug = format!("{redacted:?} {endpoint:?} {endpoint}");
        for rendered in [&display, &debug] {
            assert!(!rendered.contains(SECRET), "{rendered}");
            assert!(!rendered.contains("api_key"), "{rendered}");
            assert!(!rendered.contains("127.0.0.1"), "{rendered}");
            assert!(!rendered.contains("/rpc"), "{rendered}");
            assert!(!rendered.contains(PROJECT_ID), "{rendered}");
        }
        display
    }

    #[test]
    fn url_formatting_is_always_redacted() {
        let url = Redacted::parse(&format!(
            "https://user:{SECRET}@rpc.example/v1?api_key={SECRET}"
        ))
        .expect("valid provider URL");

        let message = format!("provider {url:?} failed: {url}");

        assert_eq!(message, "provider [REDACTED URL] failed: [REDACTED URL]");
        assert_eq!(
            url.with_provider("provider-a").to_string(),
            "provider `provider-a`"
        );
    }

    #[tokio::test]
    async fn json_rpc_error_keeps_the_node_code_and_message() {
        let url = mock_node(
            StatusCode::OK,
            r#"{"jsonrpc":"2.0","id":0,"error":{"code":-32000,"message":"nonce too low"}}"#,
        )
        .await;

        let display = failed_block_number(&url, "provider-a").await;

        assert_eq!(
            display,
            "block number fetch failed for provider `provider-a` (JSON-RPC error -32000: nonce too low)"
        );
    }

    #[tokio::test]
    async fn node_message_echoing_the_request_url_is_scrubbed() {
        let url = mock_node(
            StatusCode::OK,
            r#"{"jsonrpc":"2.0","id":0,"error":{"code":-32001,"message":"key rpc-secret-token is not enabled for project 0123456789abcdef"}}"#,
        )
        .await;

        let display = failed_block_number(&url, "provider-a").await;

        assert_eq!(
            display,
            "block number fetch failed for provider `provider-a` \
             (JSON-RPC error -32001: key [REDACTED] is not enabled for project [REDACTED])"
        );
    }

    #[tokio::test]
    async fn http_error_keeps_only_the_status_code() {
        let url = mock_node(StatusCode::TOO_MANY_REQUESTS, r#"{"error":"rate limited"}"#).await;

        let display = failed_block_number(&url, "provider-b").await;

        assert_eq!(
            display,
            "block number fetch failed for provider `provider-b` (HTTP 429)"
        );
    }

    #[test]
    fn node_messages_are_single_line_and_capped() {
        let message = node_message(&format!("line one\nline two {}", "x".repeat(500)));

        assert!(!message.contains('\n'));
        assert_eq!(message.chars().count(), MAX_NODE_MESSAGE_CHARS + 1);
        assert!(message.ends_with('…'));
        assert_eq!(node_message("nonce too low"), "nonce too low");
    }
}

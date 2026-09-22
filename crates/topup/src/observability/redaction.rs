use std::fmt::{self, Debug, Display, Formatter};

use url::Url;

/// A parsed provider URL whose formatting never reveals credentials or query parameters.
#[derive(Clone)]
pub struct Redacted(Url);

impl Redacted {
    /// Parses a provider URL while retaining the value only for outbound client construction.
    pub fn parse(value: &str) -> Result<Self, url::ParseError> {
        Url::parse(value).map(Self)
    }

    /// Returns the provider URL for an outbound client. Callers must not log this value.
    #[must_use]
    pub fn expose(&self) -> &Url {
        &self.0
    }

    /// Wraps an HTTP client error without formatting its potentially credentialed URL.
    #[must_use]
    pub fn request_error<'a>(&'a self, error: &'a reqwest::Error) -> RedactedRequestError<'a> {
        RedactedRequestError {
            provider: self,
            error,
        }
    }
}

impl Display for Redacted {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED URL]")
    }
}

impl Debug for Redacted {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, formatter)
    }
}

/// A provider request failure whose formatting cannot reveal the request URL.
pub struct RedactedRequestError<'a> {
    provider: &'a Redacted,
    error: &'a reqwest::Error,
}

impl Display for RedactedRequestError<'_> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let category = if self.error.is_timeout() {
            "timeout"
        } else if self.error.is_connect() {
            "connect"
        } else if self.error.is_status() {
            "status"
        } else if self.error.is_request() {
            "request"
        } else if self.error.is_body() {
            "body"
        } else if self.error.is_decode() {
            "decode"
        } else {
            "transport"
        };
        write!(
            formatter,
            "request to {} failed ({category})",
            self.provider
        )
    }
}

impl Debug for RedactedRequestError<'_> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, formatter)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tracing_test::traced_test;

    use super::Redacted;

    #[test]
    fn env_derived_provider_url_is_redacted_in_errors_and_logs() {
        let secret = "rpc-secret-token";
        let value = format!("https://user:{secret}@rpc.example/v1?api_key={secret}");
        let provider = Redacted::parse(&value).expect("valid provider URL");
        let message = format!("provider {provider:?} failed: {provider}");

        assert_eq!(message, "provider [REDACTED URL] failed: [REDACTED URL]");
        assert!(!message.contains(secret));
        assert!(!message.contains("user"));
    }

    #[traced_test]
    #[tokio::test]
    async fn failing_rpc_request_does_not_log_url_credentials() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local listener binds");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("request connects");
            drop(stream);
        });
        let secret = "rpc-secret-token";
        let value = format!("http://user:{secret}@{address}/rpc?api_key={secret}");
        let provider = Redacted::parse(&value).expect("valid credentialed URL");
        let error = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .expect("HTTP client builds")
            .get(provider.expose().clone())
            .send()
            .await
            .expect_err("closed listener fails the RPC request");
        server.await.expect("local listener task joins");

        tracing::error!(error = %provider.request_error(&error), "provider RPC request failed");

        assert!(logs_contain("provider RPC request failed"));
        assert!(logs_contain("[REDACTED URL]"));
        assert!(!logs_contain(secret));
        assert!(!logs_contain("api_key"));
    }
}

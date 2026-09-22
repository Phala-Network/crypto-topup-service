//! Provider URL and transport-error redaction shared by external adapters.

use std::error::Error;
use std::fmt::{self, Debug, Display, Formatter};

use url::Url;

/// A parsed provider URL whose formatting never reveals credentials or query parameters.
#[derive(Clone, Eq, PartialEq)]
pub struct Redacted(Url);

impl Redacted {
    /// Parses a provider URL while retaining the value only for outbound client construction.
    pub fn parse(value: &str) -> Result<Self, url::ParseError> {
        Url::parse(value).map(Self)
    }

    /// Returns the provider URL for an outbound client. Callers must not log this value.
    #[must_use]
    pub const fn expose(&self) -> &Url {
        &self.0
    }

    /// Maps a reqwest failure without retaining or formatting its request URL.
    #[must_use]
    pub fn request_error(
        &self,
        operation: &'static str,
        error: &reqwest::Error,
    ) -> RedactedTransportError {
        let category = if error.is_timeout() {
            "timeout"
        } else if error.is_connect() {
            "connect"
        } else if error.is_status() {
            "status"
        } else if error.is_request() {
            "request"
        } else if error.is_body() {
            "body"
        } else if error.is_decode() {
            "decode"
        } else {
            "transport"
        };
        RedactedTransportError::new(operation, self.clone(), category)
    }

    /// Maps a provider-library failure whose raw text must not reach logs.
    #[must_use]
    pub fn transport_error(&self, operation: &'static str) -> RedactedTransportError {
        RedactedTransportError::new(operation, self.clone(), "transport")
    }

    /// Maps an adapter timeout without retaining a potentially credentialed request.
    #[must_use]
    pub fn timeout_error(&self, operation: &'static str) -> RedactedTransportError {
        RedactedTransportError::new(operation, self.clone(), "timeout")
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

/// A provider failure safe to format in production logs.
#[derive(Clone, Eq, PartialEq)]
pub struct RedactedTransportError {
    operation: &'static str,
    endpoint: Redacted,
    category: &'static str,
}

impl RedactedTransportError {
    const fn new(operation: &'static str, endpoint: Redacted, category: &'static str) -> Self {
        Self {
            operation,
            endpoint,
            category,
        }
    }
}

impl Display for RedactedTransportError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} failed for {} ({})",
            self.operation, self.endpoint, self.category
        )
    }
}

impl Debug for RedactedTransportError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self, formatter)
    }
}

impl Error for RedactedTransportError {}

#[cfg(test)]
mod tests {
    use super::Redacted;

    #[test]
    fn url_formatting_is_always_redacted() {
        let secret = "rpc-secret-token";
        let url = Redacted::parse(&format!(
            "https://user:{secret}@rpc.example/v1?api_key={secret}"
        ))
        .expect("valid provider URL");

        let message = format!("provider {url:?} failed: {url}");

        assert_eq!(message, "provider [REDACTED URL] failed: [REDACTED URL]");
        assert!(!message.contains(secret));
        assert!(!message.contains("api_key"));
    }
}

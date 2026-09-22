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

#[cfg(test)]
mod tests {
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
}

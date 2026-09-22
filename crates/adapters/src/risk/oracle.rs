//! Direct sanctions-list checks through an EVM oracle contract.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use alloy::eips::BlockId;
use alloy::primitives::Address;
use alloy::providers::{Provider, RootProvider};
use alloy::rpc::types::{TransactionInput, TransactionRequest};
use alloy::sol;
use alloy::sol_types::SolCall;
use async_trait::async_trait;
use tokio::time::timeout;
use topup_core::screening::{SanctionsAnswer, SanctionsResult};

use crate::redaction::Redacted;

/// Default upper bound for one provider's sanctions RPC request.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

sol! {
    function isSanctioned(address account) external view returns (bool sanctioned);
}

/// Source of two-provider sanctions answers at a recorded block.
#[async_trait]
pub trait SanctionsSource: Send + Sync {
    /// Checks one address at the exact supplied block number.
    async fn sanctions(&self, address: Address, block_number: u64) -> SanctionsResult;
}

/// Invalid sanctions-oracle client configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SanctionsOracleConfigError {
    /// Provider A is not a valid HTTP URL.
    InvalidProviderAUrl,
    /// Provider B is not a valid HTTP URL.
    InvalidProviderBUrl,
    /// A zero timeout would make every provider unavailable.
    ZeroTimeout,
}

impl Display for SanctionsOracleConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProviderAUrl => formatter.write_str("provider A RPC URL is invalid"),
            Self::InvalidProviderBUrl => formatter.write_str("provider B RPC URL is invalid"),
            Self::ZeroTimeout => formatter.write_str("sanctions RPC timeout must be positive"),
        }
    }
}

impl Error for SanctionsOracleConfigError {}

/// Alloy HTTP client that checks the same oracle call through two providers.
pub struct SanctionsOracle {
    provider_a: RootProvider,
    provider_a_endpoint: Redacted,
    provider_b: RootProvider,
    provider_b_endpoint: Redacted,
    oracle: Address,
    request_timeout: Duration,
}

impl fmt::Debug for SanctionsOracle {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SanctionsOracle")
            .field("provider_a_endpoint", &self.provider_a_endpoint)
            .field("provider_b_endpoint", &self.provider_b_endpoint)
            .field("oracle", &self.oracle)
            .field("request_timeout", &self.request_timeout)
            .finish_non_exhaustive()
    }
}

impl SanctionsOracle {
    /// Creates a two-provider client for one configured sanctions oracle.
    pub fn new(
        provider_a_url: &str,
        provider_b_url: &str,
        oracle: Address,
        request_timeout: Duration,
    ) -> Result<Self, SanctionsOracleConfigError> {
        if request_timeout.is_zero() {
            return Err(SanctionsOracleConfigError::ZeroTimeout);
        }
        let provider_a_url = Redacted::parse(provider_a_url)
            .map_err(|_| SanctionsOracleConfigError::InvalidProviderAUrl)?
            .with_provider("provider-a");
        if !matches!(provider_a_url.expose().scheme(), "http" | "https") {
            return Err(SanctionsOracleConfigError::InvalidProviderAUrl);
        }
        let provider_b_url = Redacted::parse(provider_b_url)
            .map_err(|_| SanctionsOracleConfigError::InvalidProviderBUrl)?
            .with_provider("provider-b");
        if !matches!(provider_b_url.expose().scheme(), "http" | "https") {
            return Err(SanctionsOracleConfigError::InvalidProviderBUrl);
        }
        Ok(Self {
            provider_a: RootProvider::new_http(provider_a_url.expose().clone()),
            provider_a_endpoint: provider_a_url,
            provider_b: RootProvider::new_http(provider_b_url.expose().clone()),
            provider_b_endpoint: provider_b_url,
            oracle,
            request_timeout,
        })
    }

    /// Labels provider A and B errors with their configured provider ids instead of roles.
    #[must_use]
    pub fn with_provider_ids(
        mut self,
        provider_a: impl Into<String>,
        provider_b: impl Into<String>,
    ) -> Self {
        self.provider_a_endpoint = self.provider_a_endpoint.with_provider(provider_a);
        self.provider_b_endpoint = self.provider_b_endpoint.with_provider(provider_b);
        self
    }

    async fn answer(
        &self,
        provider: &RootProvider,
        endpoint: &Redacted,
        address: Address,
        block_number: u64,
    ) -> SanctionsAnswer {
        let call = isSanctionedCall { account: address };
        let request = TransactionRequest::default()
            .to(self.oracle)
            .input(TransactionInput::new(call.abi_encode().into()));
        let response = timeout(
            self.request_timeout,
            provider.call(request).block(BlockId::number(block_number)),
        )
        .await;
        let output = match response {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                let error = endpoint.rpc_error("sanctions oracle call", &error);
                tracing::warn!(%error, "sanctions provider request failed");
                return SanctionsAnswer::Unavailable;
            }
            Err(_) => {
                let error = endpoint.timeout_error("sanctions oracle call");
                tracing::warn!(%error, "sanctions provider request failed");
                return SanctionsAnswer::Unavailable;
            }
        };
        match isSanctionedCall::abi_decode_returns_validate(&output) {
            Ok(true) => SanctionsAnswer::Sanctioned,
            Ok(_) => SanctionsAnswer::Clear,
            Err(_) => SanctionsAnswer::Unavailable,
        }
    }
}

#[async_trait]
impl SanctionsSource for SanctionsOracle {
    async fn sanctions(&self, address: Address, block_number: u64) -> SanctionsResult {
        let (provider_a, provider_b) = tokio::join!(
            self.answer(
                &self.provider_a,
                &self.provider_a_endpoint,
                address,
                block_number
            ),
            self.answer(
                &self.provider_b,
                &self.provider_b_endpoint,
                address,
                block_number
            )
        );
        SanctionsResult {
            provider_a,
            provider_b,
            block_number,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use tokio::net::TcpListener;
    use tokio::time::sleep;

    use super::*;

    #[test]
    fn configuration_rejects_invalid_urls_without_echoing_them() {
        let secret = "not a url with api-key=secret";
        let error = SanctionsOracle::new(
            secret,
            "http://127.0.0.1:8545",
            Address::ZERO,
            DEFAULT_REQUEST_TIMEOUT,
        )
        .expect_err("invalid URL must fail");
        assert_eq!(error, SanctionsOracleConfigError::InvalidProviderAUrl);
        assert!(!error.to_string().contains(secret));
    }

    #[test]
    fn configuration_rejects_zero_timeout() {
        let error = SanctionsOracle::new(
            "http://127.0.0.1:8545",
            "http://127.0.0.1:8546",
            Address::ZERO,
            Duration::ZERO,
        )
        .expect_err("zero timeout must fail");
        assert_eq!(error, SanctionsOracleConfigError::ZeroTimeout);
    }

    #[test]
    fn configuration_rejects_non_http_urls() {
        let error = SanctionsOracle::new(
            "file:///tmp/provider",
            "http://127.0.0.1:8546",
            Address::ZERO,
            DEFAULT_REQUEST_TIMEOUT,
        )
        .expect_err("non-HTTP URL must fail");
        assert_eq!(error, SanctionsOracleConfigError::InvalidProviderAUrl);
    }

    #[tokio::test]
    async fn delayed_provider_responses_become_unavailable_at_the_deadline() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("test listener must bind");
        let address = listener.local_addr().expect("test listener has an address");
        let server = tokio::spawn(async move {
            let mut connections = Vec::with_capacity(2);
            for _ in 0..2 {
                let (connection, _) = listener.accept().await.expect("provider must connect");
                connections.push(connection);
            }
            sleep(Duration::from_secs(2)).await;
        });
        let timeout = Duration::from_millis(100);
        let oracle = SanctionsOracle::new(
            &format!("http://{address}"),
            &format!("http://{address}"),
            Address::ZERO,
            timeout,
        )
        .expect("test oracle must configure");

        let started = Instant::now();
        let result = oracle.sanctions(Address::repeat_byte(1), 1).await;
        let elapsed = started.elapsed();

        assert_eq!(result.provider_a, SanctionsAnswer::Unavailable);
        assert_eq!(result.provider_b, SanctionsAnswer::Unavailable);
        assert!(elapsed >= timeout);
        assert!(elapsed < Duration::from_secs(1));
        server.abort();
        let _ = server.await;
    }
}

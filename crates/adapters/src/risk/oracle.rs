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
use url::Url;

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
#[derive(Debug)]
pub struct SanctionsOracle {
    provider_a: RootProvider,
    provider_b: RootProvider,
    oracle: Address,
    request_timeout: Duration,
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
        let provider_a_url = Url::parse(provider_a_url)
            .map_err(|_| SanctionsOracleConfigError::InvalidProviderAUrl)?;
        if !matches!(provider_a_url.scheme(), "http" | "https") {
            return Err(SanctionsOracleConfigError::InvalidProviderAUrl);
        }
        let provider_b_url = Url::parse(provider_b_url)
            .map_err(|_| SanctionsOracleConfigError::InvalidProviderBUrl)?;
        if !matches!(provider_b_url.scheme(), "http" | "https") {
            return Err(SanctionsOracleConfigError::InvalidProviderBUrl);
        }
        Ok(Self {
            provider_a: RootProvider::new_http(provider_a_url),
            provider_b: RootProvider::new_http(provider_b_url),
            oracle,
            request_timeout,
        })
    }

    async fn answer(
        &self,
        provider: &RootProvider,
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
        let Ok(Ok(output)) = response else {
            return SanctionsAnswer::Unavailable;
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
            self.answer(&self.provider_a, address, block_number),
            self.answer(&self.provider_b, address, block_number)
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
}

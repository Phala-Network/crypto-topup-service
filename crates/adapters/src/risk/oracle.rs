//! Direct sanctions-list checks through an EVM oracle contract.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;

use alloy::eips::BlockId;
use alloy::primitives::Address;
use alloy::sol;
use alloy::sol_types::SolCall;
use async_trait::async_trait;
use topup_core::screening::{SanctionsAnswer, SanctionsResult};

use crate::chain::evm::EvmClient;

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
    /// Provider A is not an HTTP URL.
    InvalidProviderAUrl,
    /// Provider B is not an HTTP URL.
    InvalidProviderBUrl,
}

impl Display for SanctionsOracleConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProviderAUrl => formatter.write_str("provider A RPC URL is invalid"),
            Self::InvalidProviderBUrl => formatter.write_str("provider B RPC URL is invalid"),
        }
    }
}

impl Error for SanctionsOracleConfigError {}

/// Checks the same oracle call through a chain's first two providers.
#[derive(Debug)]
pub struct SanctionsOracle {
    provider_a: Arc<EvmClient>,
    provider_b: Arc<EvmClient>,
    oracle: Address,
}

impl SanctionsOracle {
    /// Creates a two-provider check of one configured sanctions oracle.
    pub fn new(
        provider_a: Arc<EvmClient>,
        provider_b: Arc<EvmClient>,
        oracle: Address,
    ) -> Result<Self, SanctionsOracleConfigError> {
        if !is_http(&provider_a) {
            return Err(SanctionsOracleConfigError::InvalidProviderAUrl);
        }
        if !is_http(&provider_b) {
            return Err(SanctionsOracleConfigError::InvalidProviderBUrl);
        }
        Ok(Self {
            provider_a,
            provider_b,
            oracle,
        })
    }

    async fn answer(
        &self,
        provider: &EvmClient,
        address: Address,
        block_number: u64,
    ) -> SanctionsAnswer {
        let call = isSanctionedCall { account: address };
        let output = match provider
            .call(
                "sanctions oracle call",
                self.oracle,
                call.abi_encode().into(),
                Some(BlockId::number(block_number)),
            )
            .await
        {
            Ok(output) => output,
            Err(error) => {
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

fn is_http(client: &EvmClient) -> bool {
    matches!(client.endpoint().expose().scheme(), "http" | "https")
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
    use std::time::{Duration, Instant};

    use tokio::net::TcpListener;
    use tokio::time::sleep;

    use super::*;

    fn client(url: &str, timeout: Duration) -> Arc<EvmClient> {
        Arc::new(EvmClient::with_policy(url, timeout, 1).expect("test URL parses"))
    }

    #[test]
    fn configuration_rejects_non_http_urls() {
        let error = SanctionsOracle::new(
            client("file:///tmp/provider", Duration::from_secs(1)),
            client("http://127.0.0.1:8546", Duration::from_secs(1)),
            Address::ZERO,
        )
        .expect_err("non-HTTP URL must fail");
        assert_eq!(error, SanctionsOracleConfigError::InvalidProviderAUrl);
        let error = SanctionsOracle::new(
            client("http://127.0.0.1:8545", Duration::from_secs(1)),
            client("ws://127.0.0.1:8546", Duration::from_secs(1)),
            Address::ZERO,
        )
        .expect_err("non-HTTP URL must fail");
        assert_eq!(error, SanctionsOracleConfigError::InvalidProviderBUrl);
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
        let url = format!("http://{address}");
        let oracle =
            SanctionsOracle::new(client(&url, timeout), client(&url, timeout), Address::ZERO)
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

use std::time::Duration;

use alloy_primitives::{Address, B256, U256};
use alloy_provider::{Provider, RootProvider};
use alloy_rpc_types_eth::Filter;
use async_trait::async_trait;
use tokio::time::timeout;
use topup_adapters::chain::evm::{ChainReader, EvmChain, TransferLog};
use topup_adapters::chain::flush::{decode_flushed, flushed_signature};
use topup_adapters::redaction::Redacted;

use crate::flusher::AlloyChainClient;
use crate::scanner::MAX_SCAN_WINDOW;

use super::ReconciliationError;

/// Bounded chain reads needed by reconciliation.
#[async_trait]
pub trait ReconciliationChain: Send + Sync {
    /// Returns the reviewed finalized block.
    async fn finalized_head(&self) -> Result<u64, ReconciliationError>;

    /// Returns finalized ERC-20 transfers to tracked recipients.
    async fn transfer_logs_to(
        &self,
        addresses: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<TransferLog>, ReconciliationError>;

    /// Returns token balances at one block in bounded JSON-RPC batches.
    async fn token_balances(
        &self,
        token: Address,
        addresses: &[Address],
        block: u64,
    ) -> Result<Vec<U256>, ReconciliationError>;

    /// Returns the sum of finalized on-chain `Flushed` events for one token.
    async fn flushed_total(
        &self,
        factory: Address,
        token: Address,
        from_block: u64,
        to_block: u64,
    ) -> Result<U256, ReconciliationError>;

    /// Returns factory-derived forwarder addresses in bounded batches.
    async fn factory_addresses(
        &self,
        factory: Address,
        salts: &[B256],
    ) -> Result<Vec<Address>, ReconciliationError>;
}

/// Production reconciliation client composed from the scanner and flusher RPC clients.
pub struct RpcReconciliationChain {
    scanner: EvmChain,
    flusher: AlloyChainClient,
    provider: RootProvider,
    endpoint: Redacted,
    request_timeout: Duration,
}

impl RpcReconciliationChain {
    /// Creates one bounded client for a configured RPC provider.
    pub fn connect(
        rpc_url: &str,
        request_timeout: Duration,
        balance_batch_size: usize,
    ) -> Result<Self, ReconciliationError> {
        let endpoint = Redacted::parse(rpc_url).map_err(|_| {
            ReconciliationError::Configuration("invalid reconciliation RPC URL".to_owned())
        })?;
        Ok(Self {
            scanner: EvmChain::new(rpc_url)?,
            flusher: AlloyChainClient::connect_http_with_policy(
                rpc_url,
                request_timeout,
                balance_batch_size,
            )?,
            provider: RootProvider::new_http(endpoint.expose().clone()),
            endpoint,
            request_timeout,
        })
    }

    /// Labels every provider error with the configured provider id instead of the URL.
    #[must_use]
    pub fn with_provider(self, provider: &str) -> Self {
        Self {
            scanner: self.scanner.with_provider(provider),
            flusher: self.flusher.with_provider(provider),
            endpoint: self.endpoint.with_provider(provider),
            ..self
        }
    }
}

#[async_trait]
impl ReconciliationChain for RpcReconciliationChain {
    async fn finalized_head(&self) -> Result<u64, ReconciliationError> {
        timeout(
            self.request_timeout,
            ChainReader::finalized_head(&self.scanner),
        )
        .await
        .map_err(|_| ReconciliationError::Chain("finalized-head request timed out".to_owned()))?
        .map_err(Into::into)
    }

    async fn transfer_logs_to(
        &self,
        addresses: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<TransferLog>, ReconciliationError> {
        timeout(
            self.request_timeout,
            ChainReader::transfer_logs_to(&self.scanner, addresses, from_block, to_block),
        )
        .await
        .map_err(|_| ReconciliationError::Chain("transfer-log request timed out".to_owned()))?
        .map_err(Into::into)
    }

    async fn token_balances(
        &self,
        token: Address,
        addresses: &[Address],
        block: u64,
    ) -> Result<Vec<U256>, ReconciliationError> {
        self.flusher
            .token_balances_at(token, addresses, block)
            .await
            .map_err(Into::into)
    }

    async fn flushed_total(
        &self,
        factory: Address,
        token: Address,
        from_block: u64,
        to_block: u64,
    ) -> Result<U256, ReconciliationError> {
        let mut total = U256::ZERO;
        let mut start = from_block;
        loop {
            let end = start
                .saturating_add(MAX_SCAN_WINDOW.saturating_sub(1))
                .min(to_block);
            let filter = Filter::new()
                .address(factory)
                .from_block(start)
                .to_block(end)
                .event_signature(flushed_signature())
                .topic3(token);
            let logs = timeout(self.request_timeout, self.provider.get_logs(&filter))
                .await
                .map_err(|_| {
                    ReconciliationError::Chain(
                        self.endpoint.timeout_error("Flushed log fetch").to_string(),
                    )
                })?
                .map_err(|error| {
                    ReconciliationError::Chain(
                        self.endpoint
                            .rpc_error("Flushed log fetch", &error)
                            .to_string(),
                    )
                })?;
            for log in logs {
                let decoded = decode_flushed(log.data())
                    .map_err(|error| ReconciliationError::Chain(error.to_string()))?;
                total = total
                    .checked_add(decoded.amount)
                    .ok_or(ReconciliationError::Invariant(
                        "Flushed event total overflowed U256",
                    ))?;
            }
            if end == to_block {
                break;
            }
            start = end.checked_add(1).ok_or(ReconciliationError::Invariant(
                "Flushed log range overflowed",
            ))?;
        }
        Ok(total)
    }

    async fn factory_addresses(
        &self,
        factory: Address,
        salts: &[B256],
    ) -> Result<Vec<Address>, ReconciliationError> {
        self.flusher
            .factory_addresses(factory, salts)
            .await
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn flushed_log_transport_failure_does_not_format_the_provider_url() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local listener binds");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                drop(stream);
            }
        });
        let secret = "rpc-secret-token";
        let chain = RpcReconciliationChain::connect(
            &format!("http://user:{secret}@{address}/rpc?api_key={secret}"),
            Duration::from_secs(5),
            10,
        )
        .expect("production adapter accepts URL");

        let error = chain
            .flushed_total(Address::ZERO, Address::ZERO, 1, 1)
            .await
            .expect_err("closed connections fail the log request");
        server.abort();

        let message = error.to_string();
        assert!(message.contains("[REDACTED URL]"), "{message}");
        assert!(!message.contains(secret) && !message.contains("api_key"));
    }
}

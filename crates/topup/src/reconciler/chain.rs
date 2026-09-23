use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{Address, B256, U256};
use async_trait::async_trait;
use tokio::time::timeout;
use topup_adapters::chain::evm::{ChainReader, FinalizedReader, TransferLog};

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

/// Production reconciliation reads: finalized logs through the reconciler's own reader, and
/// balances, `Flushed` events and derived addresses through the shared client.
#[async_trait]
impl ReconciliationChain for FinalizedReader {
    async fn finalized_head(&self) -> Result<u64, ReconciliationError> {
        timeout(
            self.client().request_timeout(),
            ChainReader::finalized_head(self),
        )
        .await
        .map_err(|_| ReconciliationError::Chain("finalized-head request timed out".to_owned()))?
        .map(|head| head.number)
        .map_err(Into::into)
    }

    async fn transfer_logs_to(
        &self,
        addresses: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<TransferLog>, ReconciliationError> {
        timeout(
            self.client().request_timeout(),
            ChainReader::transfer_logs_to(self, addresses, from_block, to_block),
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
        self.client()
            .token_balances(token, addresses, BlockNumberOrTag::Number(block))
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
            for event in self
                .client()
                .flushed_events(factory, token, start, end)
                .await?
            {
                total = total
                    .checked_add(event.amount)
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
        self.client()
            .factory_addresses(factory, salts)
            .await
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use topup_adapters::chain::evm::EvmClient;

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
        let chain = FinalizedReader::new(Arc::new(
            EvmClient::with_timeout(
                &format!("http://user:{secret}@{address}/rpc?api_key={secret}"),
                Duration::from_secs(5),
            )
            .expect("production adapter accepts URL"),
        ));

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

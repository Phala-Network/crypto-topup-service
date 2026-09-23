use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{Address, B256, U256};
use async_trait::async_trait;
use serde_json::Value;
use topup_adapters::chain::evm::{ChainError, EvmClient, FeeQuote};
use topup_adapters::chain::flush::{decode_has_role, encode_flush, encode_has_role, operator_role};

use super::{ChainClient, ChainLog, ChainReceipt, NonceReceiptSearch};

#[async_trait]
impl ChainClient for EvmClient {
    async fn token_balances(
        &self,
        token: Address,
        addresses: &[Address],
    ) -> Result<Vec<U256>, ChainError> {
        EvmClient::token_balances(self, token, addresses, BlockNumberOrTag::Latest).await
    }

    async fn native_balances(&self, addresses: &[Address]) -> Result<Vec<U256>, ChainError> {
        EvmClient::native_balances(self, addresses).await
    }

    async fn estimate_flush_gas(
        &self,
        factory: Address,
        operator: Address,
        salts: &[B256],
        token: Address,
    ) -> Result<u64, ChainError> {
        self.estimate_gas(operator, factory, encode_flush(salts.to_vec(), token))
            .await
    }

    async fn has_operator_role(
        &self,
        factory: Address,
        operator: Address,
    ) -> Result<bool, ChainError> {
        let output = self
            .call(
                "hasRole call",
                factory,
                encode_has_role(operator_role(), operator),
                None,
            )
            .await?;
        decode_has_role(&output)
            .map_err(|error| ChainError::InvalidResponse(format!("decode hasRole result: {error}")))
    }

    async fn pending_nonce(&self, operator: Address) -> Result<u64, ChainError> {
        EvmClient::pending_nonce(self, operator).await
    }

    async fn confirmed_nonce(&self, operator: Address) -> Result<u64, ChainError> {
        EvmClient::confirmed_nonce(self, operator).await
    }

    async fn latest_block(&self) -> Result<u64, ChainError> {
        EvmClient::latest_block(self).await
    }

    async fn finalized_block(&self) -> Result<u64, ChainError> {
        EvmClient::finalized_block(self)
            .await?
            .ok_or_else(|| ChainError::InvalidResponse("finalized block is unavailable".to_owned()))
    }

    async fn fee_quote(&self) -> Result<FeeQuote, ChainError> {
        EvmClient::fee_quote(self).await
    }

    async fn send_raw_transaction(&self, raw: &[u8]) -> Result<B256, ChainError> {
        EvmClient::send_raw_transaction(self, raw).await
    }

    async fn receipt(&self, hash: B256) -> Result<Option<ChainReceipt>, ChainError> {
        EvmClient::receipt(self, hash)
            .await?
            .map(convert_receipt)
            .transpose()
    }

    async fn receipt_by_sender_nonce(
        &self,
        operator: Address,
        nonce: u64,
        from_block: u64,
        max_blocks: u64,
    ) -> Result<NonceReceiptSearch, ChainError> {
        let latest = EvmClient::latest_block(self).await?;
        let first = from_block.min(latest);
        let last = first
            .saturating_add(max_blocks.saturating_sub(1))
            .min(latest);
        for block_number in first..=last {
            let block = self.block_with_transactions(block_number).await?;
            let Some(transactions) = block.get("transactions").and_then(Value::as_array) else {
                continue;
            };
            for transaction in transactions {
                let sender = transaction
                    .get("from")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<Address>().ok());
                let transaction_nonce = transaction
                    .get("nonce")
                    .and_then(Value::as_str)
                    .and_then(parse_quantity);
                if sender == Some(operator) && transaction_nonce == Some(nonce) {
                    let hash = transaction
                        .get("hash")
                        .and_then(Value::as_str)
                        .and_then(|value| value.parse::<B256>().ok())
                        .ok_or_else(|| {
                            ChainError::InvalidResponse(
                                "matching transaction omitted a valid hash".to_owned(),
                            )
                        })?;
                    return Ok(NonceReceiptSearch {
                        receipt: ChainClient::receipt(self, hash).await?,
                        next_block: None,
                    });
                }
            }
        }
        Ok(NonceReceiptSearch {
            receipt: None,
            next_block: (last < latest).then_some(last.saturating_add(1)),
        })
    }
}

fn convert_receipt(
    receipt: alloy_rpc_types_eth::TransactionReceipt,
) -> Result<ChainReceipt, ChainError> {
    let invalid = |message: &str| ChainError::InvalidResponse(message.to_owned());
    let block_number = receipt
        .block_number
        .ok_or_else(|| invalid("mined receipt omitted block number"))?;
    let success = receipt.status();
    let mut logs = Vec::new();
    for log in receipt.inner.logs() {
        logs.push(ChainLog {
            address: log.address(),
            topics: log.topics().to_vec(),
            data: log.data().data.clone(),
            block_number: log
                .block_number
                .ok_or_else(|| invalid("receipt log omitted block number"))?,
            log_index: log
                .log_index
                .ok_or_else(|| invalid("receipt log omitted log index"))?,
        });
    }
    Ok(ChainReceipt {
        transaction_hash: receipt.transaction_hash,
        block_number,
        success,
        logs,
    })
}

fn parse_quantity(value: &str) -> Option<u64> {
    value
        .strip_prefix("0x")
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
}

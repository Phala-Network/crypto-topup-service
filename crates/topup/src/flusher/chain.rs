use std::borrow::Cow;

use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_provider::{Provider, RootProvider};
use alloy_rpc_client::BatchRequest;
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use async_trait::async_trait;
use serde_json::Value;
use topup_adapters::chain::flush::{decode_balance_of, encode_balance_of, encode_flush};

use super::{ChainClient, ChainError, ChainLog, ChainReceipt, FeeQuote};

/// Alloy HTTP provider implementation used until the shared C3 EVM client is merged.
#[derive(Clone, Debug)]
pub struct AlloyChainClient {
    provider: RootProvider,
}

impl AlloyChainClient {
    /// Connects to one HTTP JSON-RPC endpoint.
    pub fn connect_http(url: &str) -> Result<Self, ChainError> {
        let url = url
            .parse()
            .map_err(|error| ChainError(format!("invalid RPC URL: {error}")))?;
        Ok(Self {
            provider: RootProvider::new_http(url),
        })
    }

    async fn batch_calls(
        &self,
        method: &'static str,
        params: Vec<Value>,
    ) -> Result<Vec<Value>, ChainError> {
        let mut batch = BatchRequest::new(self.provider.client());
        let mut waiters = Vec::with_capacity(params.len());
        for value in &params {
            waiters.push(
                batch
                    .add_call::<_, Value>(method, value)
                    .map_err(transport_error)?,
            );
        }
        batch.send().await.map_err(transport_error)?;
        let mut responses = Vec::with_capacity(waiters.len());
        for waiter in waiters {
            responses.push(waiter.await.map_err(transport_error)?);
        }
        Ok(responses)
    }

    fn convert_receipt(
        receipt: alloy_rpc_types_eth::TransactionReceipt,
    ) -> Result<ChainReceipt, ChainError> {
        let block_number = receipt
            .block_number
            .ok_or_else(|| ChainError("mined receipt omitted block number".to_owned()))?;
        let success = receipt.status();
        let mut logs = Vec::new();
        for log in receipt.inner.logs() {
            logs.push(ChainLog {
                address: log.address(),
                topics: log.topics().to_vec(),
                data: log.data().data.clone(),
                block_number: log
                    .block_number
                    .ok_or_else(|| ChainError("receipt log omitted block number".to_owned()))?,
                log_index: log
                    .log_index
                    .ok_or_else(|| ChainError("receipt log omitted log index".to_owned()))?,
            });
        }
        Ok(ChainReceipt {
            transaction_hash: receipt.transaction_hash,
            block_number,
            success,
            logs,
        })
    }
}

#[async_trait]
impl ChainClient for AlloyChainClient {
    async fn token_balances(
        &self,
        token: Address,
        addresses: &[Address],
    ) -> Result<Vec<U256>, ChainError> {
        let params = addresses
            .iter()
            .map(|address| {
                let tx = TransactionRequest::default()
                    .to(token)
                    .input(TransactionInput::new(encode_balance_of(*address)));
                serde_json::to_value((tx, BlockNumberOrTag::Latest))
                    .map_err(|error| ChainError(format!("serialize eth_call: {error}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.batch_calls("eth_call", params)
            .await?
            .into_iter()
            .map(|value| {
                let encoded: Bytes = serde_json::from_value(value)
                    .map_err(|error| ChainError(format!("decode eth_call bytes: {error}")))?;
                decode_balance_of(&encoded)
                    .map_err(|error| ChainError(format!("decode balanceOf result: {error}")))
            })
            .collect()
    }

    async fn native_balances(&self, addresses: &[Address]) -> Result<Vec<U256>, ChainError> {
        let params = addresses
            .iter()
            .map(|address| {
                serde_json::to_value((*address, BlockNumberOrTag::Latest))
                    .map_err(|error| ChainError(format!("serialize eth_getBalance: {error}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.batch_calls("eth_getBalance", params)
            .await?
            .into_iter()
            .map(|value| {
                serde_json::from_value(value)
                    .map_err(|error| ChainError(format!("decode native balance: {error}")))
            })
            .collect()
    }

    async fn estimate_flush_gas(
        &self,
        factory: Address,
        operator: Address,
        salts: &[B256],
        token: Address,
    ) -> Result<u64, ChainError> {
        let tx = TransactionRequest::default()
            .from(operator)
            .to(factory)
            .input(TransactionInput::new(encode_flush(salts.to_vec(), token)));
        self.provider
            .estimate_gas(tx)
            .await
            .map_err(transport_error)
    }

    async fn pending_nonce(&self, operator: Address) -> Result<u64, ChainError> {
        self.provider
            .get_transaction_count(operator)
            .pending()
            .await
            .map_err(transport_error)
    }

    async fn confirmed_nonce(&self, operator: Address) -> Result<u64, ChainError> {
        self.provider
            .get_transaction_count(operator)
            .latest()
            .await
            .map_err(transport_error)
    }

    async fn latest_block(&self) -> Result<u64, ChainError> {
        self.provider
            .get_block_number()
            .await
            .map_err(transport_error)
    }

    async fn finalized_block(&self) -> Result<u64, ChainError> {
        self.provider
            .get_block_number_by_id(BlockId::Number(BlockNumberOrTag::Finalized))
            .await
            .map_err(transport_error)?
            .ok_or_else(|| ChainError("finalized block is unavailable".to_owned()))
    }

    async fn fee_quote(&self) -> Result<FeeQuote, ChainError> {
        let estimate = self
            .provider
            .estimate_eip1559_fees()
            .await
            .map_err(transport_error)?;
        Ok(FeeQuote {
            max_fee_per_gas: estimate.max_fee_per_gas,
            max_priority_fee_per_gas: estimate.max_priority_fee_per_gas,
        })
    }

    async fn send_raw_transaction(&self, raw: &[u8]) -> Result<B256, ChainError> {
        match self.provider.send_raw_transaction(raw).await {
            Ok(pending) => Ok(*pending.tx_hash()),
            Err(error) if is_already_known(&error.to_string()) => {
                Ok(alloy_primitives::keccak256(raw))
            }
            Err(error) => Err(transport_error(error)),
        }
    }

    async fn receipt(&self, hash: B256) -> Result<Option<ChainReceipt>, ChainError> {
        self.provider
            .get_transaction_receipt(hash)
            .await
            .map_err(transport_error)?
            .map(Self::convert_receipt)
            .transpose()
    }

    async fn receipt_by_sender_nonce(
        &self,
        operator: Address,
        nonce: u64,
        from_block: u64,
    ) -> Result<Option<ChainReceipt>, ChainError> {
        let latest = self.latest_block().await?;
        let first = from_block.min(latest);
        for block_number in (first..=latest).rev() {
            let block: Value = self
                .provider
                .raw_request(
                    Cow::Borrowed("eth_getBlockByNumber"),
                    (format!("0x{block_number:x}"), true),
                )
                .await
                .map_err(transport_error)?;
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
                            ChainError("matching transaction omitted a valid hash".to_owned())
                        })?;
                    return self.receipt(hash).await;
                }
            }
        }
        Ok(None)
    }
}

fn parse_quantity(value: &str) -> Option<u64> {
    value
        .strip_prefix("0x")
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
}

fn transport_error(error: impl std::fmt::Display) -> ChainError {
    ChainError(error.to_string())
}

fn is_already_known(message: &str) -> bool {
    let lowercase = message.to_ascii_lowercase();
    lowercase.contains("already known") || lowercase.contains("known transaction")
}

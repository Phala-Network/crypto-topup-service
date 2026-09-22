use std::borrow::Cow;
use std::time::Duration;

use alloy_eips::{BlockId, BlockNumberOrTag};
use alloy_primitives::{Address, B256, Bytes, U256};
use alloy_provider::{Provider, RootProvider};
use alloy_rpc_client::BatchRequest;
use alloy_rpc_types_eth::{TransactionInput, TransactionRequest};
use async_trait::async_trait;
use serde_json::Value;
use tokio::time::timeout;
use topup_adapters::chain::flush::{
    decode_address_of, decode_balance_of, encode_address_of, encode_balance_of, encode_flush,
};

use super::{ChainClient, ChainError, ChainLog, ChainReceipt, FeeQuote, NonceReceiptSearch};

const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_BALANCE_BATCH_SIZE: usize = 500;

/// Alloy HTTP provider implementation used until the shared C3 EVM client is merged.
#[derive(Clone, Debug)]
pub struct AlloyChainClient {
    provider: RootProvider,
    request_timeout: Duration,
    balance_batch_size: usize,
}

impl AlloyChainClient {
    /// Connects to one HTTP JSON-RPC endpoint.
    pub fn connect_http(url: &str) -> Result<Self, ChainError> {
        Self::connect_http_with_policy(url, DEFAULT_RPC_TIMEOUT, DEFAULT_BALANCE_BATCH_SIZE)
    }

    /// Connects with explicit RPC timeout and maximum balance batch size.
    pub fn connect_http_with_policy(
        url: &str,
        request_timeout: Duration,
        balance_batch_size: usize,
    ) -> Result<Self, ChainError> {
        if request_timeout.is_zero() || balance_batch_size == 0 {
            return Err(ChainError::rpc(
                "RPC timeout and balance batch size must be positive",
            ));
        }
        let url = url
            .parse()
            .map_err(|error| ChainError::rpc(format!("invalid RPC URL: {error}")))?;
        Ok(Self {
            provider: RootProvider::new_http(url),
            request_timeout,
            balance_batch_size,
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
        timeout(self.request_timeout, batch.send())
            .await
            .map_err(|_| ChainError::rpc("RPC batch timed out"))?
            .map_err(transport_error)?;
        let mut responses = Vec::with_capacity(waiters.len());
        for waiter in waiters {
            responses.push(
                timeout(self.request_timeout, waiter)
                    .await
                    .map_err(|_| ChainError::rpc("RPC batch response timed out"))?
                    .map_err(transport_error)?,
            );
        }
        Ok(responses)
    }

    fn convert_receipt(
        receipt: alloy_rpc_types_eth::TransactionReceipt,
    ) -> Result<ChainReceipt, ChainError> {
        let block_number = receipt
            .block_number
            .ok_or_else(|| ChainError::rpc("mined receipt omitted block number"))?;
        let success = receipt.status();
        let mut logs = Vec::new();
        for log in receipt.inner.logs() {
            logs.push(ChainLog {
                address: log.address(),
                topics: log.topics().to_vec(),
                data: log.data().data.clone(),
                block_number: log
                    .block_number
                    .ok_or_else(|| ChainError::rpc("receipt log omitted block number"))?,
                log_index: log
                    .log_index
                    .ok_or_else(|| ChainError::rpc("receipt log omitted log index"))?,
            });
        }
        Ok(ChainReceipt {
            transaction_hash: receipt.transaction_hash,
            block_number,
            success,
            logs,
        })
    }

    /// Reads ERC-20 balances at one block in bounded JSON-RPC batches.
    pub async fn token_balances_at(
        &self,
        token: Address,
        addresses: &[Address],
        block: u64,
    ) -> Result<Vec<U256>, ChainError> {
        self.balance_of_at(token, addresses, BlockNumberOrTag::Number(block))
            .await
    }

    async fn balance_of_at(
        &self,
        token: Address,
        addresses: &[Address],
        block: BlockNumberOrTag,
    ) -> Result<Vec<U256>, ChainError> {
        let mut result = Vec::with_capacity(addresses.len());
        for chunk in addresses.chunks(self.balance_batch_size) {
            let params = chunk
                .iter()
                .map(|address| {
                    let tx = TransactionRequest::default()
                        .to(token)
                        .input(TransactionInput::new(encode_balance_of(*address)));
                    serde_json::to_value((tx, block))
                        .map_err(|error| ChainError::rpc(format!("serialize eth_call: {error}")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let decoded = self
                .batch_calls("eth_call", params)
                .await?
                .into_iter()
                .map(|value| {
                    let encoded: Bytes = serde_json::from_value(value).map_err(|error| {
                        ChainError::rpc(format!("decode eth_call bytes: {error}"))
                    })?;
                    decode_balance_of(&encoded).map_err(|error| {
                        ChainError::rpc(format!("decode balanceOf result: {error}"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            result.extend(decoded);
        }
        Ok(result)
    }

    /// Reads deterministic forwarder addresses in bounded JSON-RPC batches.
    pub async fn factory_addresses(
        &self,
        factory: Address,
        salts: &[B256],
    ) -> Result<Vec<Address>, ChainError> {
        let mut result = Vec::with_capacity(salts.len());
        for chunk in salts.chunks(self.balance_batch_size) {
            let params = chunk
                .iter()
                .map(|salt| {
                    let tx = TransactionRequest::default()
                        .to(factory)
                        .input(TransactionInput::new(encode_address_of(*salt)));
                    serde_json::to_value((tx, BlockNumberOrTag::Latest))
                        .map_err(|error| ChainError::rpc(format!("serialize eth_call: {error}")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let decoded = self
                .batch_calls("eth_call", params)
                .await?
                .into_iter()
                .map(|value| {
                    let encoded: Bytes = serde_json::from_value(value).map_err(|error| {
                        ChainError::rpc(format!("decode eth_call bytes: {error}"))
                    })?;
                    decode_address_of(&encoded).map_err(|error| {
                        ChainError::rpc(format!("decode addressOf result: {error}"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            result.extend(decoded);
        }
        Ok(result)
    }
}

#[async_trait]
impl ChainClient for AlloyChainClient {
    async fn token_balances(
        &self,
        token: Address,
        addresses: &[Address],
    ) -> Result<Vec<U256>, ChainError> {
        self.balance_of_at(token, addresses, BlockNumberOrTag::Latest)
            .await
    }

    async fn native_balances(&self, addresses: &[Address]) -> Result<Vec<U256>, ChainError> {
        let mut result = Vec::with_capacity(addresses.len());
        for chunk in addresses.chunks(self.balance_batch_size) {
            let params = chunk
                .iter()
                .map(|address| {
                    serde_json::to_value((*address, BlockNumberOrTag::Latest)).map_err(|error| {
                        ChainError::rpc(format!("serialize eth_getBalance: {error}"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let decoded: Vec<U256> = self
                .batch_calls("eth_getBalance", params)
                .await?
                .into_iter()
                .map(|value| {
                    serde_json::from_value(value)
                        .map_err(|error| ChainError::rpc(format!("decode native balance: {error}")))
                })
                .collect::<Result<Vec<_>, _>>()?;
            result.extend(decoded);
        }
        Ok(result)
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
        match timeout(self.request_timeout, self.provider.estimate_gas(tx)).await {
            Err(_) => Err(ChainError::rpc("eth_estimateGas timed out")),
            Ok(Ok(gas)) => Ok(gas),
            Ok(Err(error)) => {
                let message = error.to_string();
                if is_execution_revert(&message) {
                    Err(ChainError::estimation_revert(message))
                } else {
                    Err(ChainError::rpc(message))
                }
            }
        }
    }

    async fn pending_nonce(&self, operator: Address) -> Result<u64, ChainError> {
        timeout(
            self.request_timeout,
            self.provider.get_transaction_count(operator).pending(),
        )
        .await
        .map_err(|_| ChainError::rpc("pending nonce RPC timed out"))?
        .map_err(transport_error)
    }

    async fn confirmed_nonce(&self, operator: Address) -> Result<u64, ChainError> {
        timeout(
            self.request_timeout,
            self.provider.get_transaction_count(operator).latest(),
        )
        .await
        .map_err(|_| ChainError::rpc("confirmed nonce RPC timed out"))?
        .map_err(transport_error)
    }

    async fn latest_block(&self) -> Result<u64, ChainError> {
        timeout(self.request_timeout, self.provider.get_block_number())
            .await
            .map_err(|_| ChainError::rpc("block number RPC timed out"))?
            .map_err(transport_error)
    }

    async fn finalized_block(&self) -> Result<u64, ChainError> {
        timeout(
            self.request_timeout,
            self.provider
                .get_block_number_by_id(BlockId::Number(BlockNumberOrTag::Finalized)),
        )
        .await
        .map_err(|_| ChainError::rpc("finalized block RPC timed out"))?
        .map_err(transport_error)?
        .ok_or_else(|| ChainError::rpc("finalized block is unavailable"))
    }

    async fn fee_quote(&self) -> Result<FeeQuote, ChainError> {
        let estimate = timeout(self.request_timeout, self.provider.estimate_eip1559_fees())
            .await
            .map_err(|_| ChainError::rpc("fee estimate RPC timed out"))?
            .map_err(transport_error)?;
        Ok(FeeQuote {
            max_fee_per_gas: estimate.max_fee_per_gas,
            max_priority_fee_per_gas: estimate.max_priority_fee_per_gas,
        })
    }

    async fn send_raw_transaction(&self, raw: &[u8]) -> Result<B256, ChainError> {
        match timeout(
            self.request_timeout,
            self.provider.send_raw_transaction(raw),
        )
        .await
        {
            Err(_) => Err(ChainError::rpc("send raw transaction RPC timed out")),
            Ok(Ok(pending)) => Ok(*pending.tx_hash()),
            Ok(Err(error)) if is_already_known(&error.to_string()) => {
                Ok(alloy_primitives::keccak256(raw))
            }
            Ok(Err(error)) => Err(transport_error(error)),
        }
    }

    async fn receipt(&self, hash: B256) -> Result<Option<ChainReceipt>, ChainError> {
        timeout(
            self.request_timeout,
            self.provider.get_transaction_receipt(hash),
        )
        .await
        .map_err(|_| ChainError::rpc("receipt RPC timed out"))?
        .map_err(transport_error)?
        .map(Self::convert_receipt)
        .transpose()
    }

    async fn receipt_by_sender_nonce(
        &self,
        operator: Address,
        nonce: u64,
        from_block: u64,
        max_blocks: u64,
    ) -> Result<NonceReceiptSearch, ChainError> {
        let latest = self.latest_block().await?;
        let first = from_block.min(latest);
        let last = first
            .saturating_add(max_blocks.saturating_sub(1))
            .min(latest);
        for block_number in first..=last {
            let block: Value = timeout(
                self.request_timeout,
                self.provider.raw_request(
                    Cow::Borrowed("eth_getBlockByNumber"),
                    (format!("0x{block_number:x}"), true),
                ),
            )
            .await
            .map_err(|_| ChainError::rpc("block recovery RPC timed out"))?
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
                            ChainError::rpc("matching transaction omitted a valid hash")
                        })?;
                    return Ok(NonceReceiptSearch {
                        receipt: self.receipt(hash).await?,
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

fn parse_quantity(value: &str) -> Option<u64> {
    value
        .strip_prefix("0x")
        .and_then(|hex| u64::from_str_radix(hex, 16).ok())
}

fn transport_error(error: impl std::fmt::Display) -> ChainError {
    ChainError::rpc(error.to_string())
}

fn is_already_known(message: &str) -> bool {
    let lowercase = message.to_ascii_lowercase();
    lowercase.contains("already known")
        || lowercase.contains("known transaction")
        || lowercase.contains("transaction already imported")
}

fn is_execution_revert(message: &str) -> bool {
    let lowercase = message.to_ascii_lowercase();
    lowercase.contains("execution reverted") || lowercase.contains("revert")
}

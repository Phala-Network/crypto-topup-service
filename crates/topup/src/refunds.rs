//! Treasury refund transaction confirmation.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use alloy::eips::BlockNumberOrTag;
use alloy::providers::{Provider, RootProvider};
use alloy::sol;
use alloy_primitives::{Address, B256, U256};
use async_trait::async_trait;
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use tokio_util::sync::CancellationToken;
use topup_core::route::RouteFile;
use url::Url;
use uuid::Uuid;

sol! {
    event Transfer(address indexed from, address indexed to, uint256 amount);
}

/// One sent refund requiring finalized chain verification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefundCheck {
    /// Refund workflow identifier.
    pub refund_id: Uuid,
    /// Product receiving the webhook event.
    pub product_id: Uuid,
    /// Related deposit identifier.
    pub deposit_id: Uuid,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Refunded ERC-20 contract.
    pub asset_contract: Address,
    /// Configured treasury Safe.
    pub treasury: Address,
    /// Customer-controlled refund destination.
    pub to_address: Address,
    /// Minimum atomic token amount expected in the transaction.
    pub amount_atomic: U256,
    /// Recorded treasury transaction hash.
    pub tx_hash: B256,
}

/// Finality-aware observation of a recorded refund transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RefundObservation {
    /// The transaction is absent or not yet finalized.
    Pending,
    /// The transaction is finalized with the matching transfer total shown.
    Finalized {
        /// Finalized block containing the receipt.
        block_number: u64,
        /// Whether EVM execution succeeded.
        succeeded: bool,
        /// Sum of matching ERC-20 transfers in the transaction.
        transferred_atomic: U256,
    },
}

/// Chain-read failure while checking a refund transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RefundReadError {
    /// No RPC reader is configured for the chain.
    UnknownChain(u64),
    /// A configured provider URL is invalid.
    InvalidUrl(String),
    /// The RPC request failed during the named operation.
    Rpc(&'static str),
    /// The RPC response omitted a required field.
    MissingField(&'static str),
}

impl Display for RefundReadError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownChain(chain_id) => {
                write!(formatter, "no refund reader for chain {chain_id}")
            }
            Self::InvalidUrl(error) => write!(formatter, "invalid refund RPC URL: {error}"),
            Self::Rpc(operation) => write!(formatter, "refund RPC failed during {operation}"),
            Self::MissingField(field) => write!(formatter, "refund RPC omitted `{field}`"),
        }
    }
}

impl Error for RefundReadError {}

/// Finality-aware chain access used by the refund confirmation worker.
#[async_trait]
pub trait RefundChainReader: Send + Sync {
    /// Observes one recorded transaction against the expected refund transfer.
    async fn observe(&self, check: &RefundCheck) -> Result<RefundObservation, RefundReadError>;
}

/// Alloy HTTP implementation keyed by EVM chain id.
pub struct EvmRefundChainReader {
    providers: BTreeMap<u64, RootProvider>,
}

impl EvmRefundChainReader {
    /// Creates one provider per configured chain using provider A.
    pub fn from_routes(routes: &[RouteFile]) -> Result<Self, RefundReadError> {
        let mut providers = BTreeMap::new();
        for route in routes {
            if providers.contains_key(&route.chain.chain_id) {
                continue;
            }
            let provider_id = route
                .chain
                .rpc_providers
                .first()
                .ok_or(RefundReadError::MissingField("chain.rpc_providers[0]"))?;
            let environment = provider_environment_name(provider_id);
            let rpc_url = std::env::var(&environment)
                .map_err(|_| RefundReadError::MissingField("refund RPC environment"))?;
            let url = Url::parse(&rpc_url)
                .map_err(|error| RefundReadError::InvalidUrl(error.to_string()))?;
            providers.insert(route.chain.chain_id, RootProvider::new_http(url));
        }
        Ok(Self { providers })
    }
}

#[async_trait]
impl RefundChainReader for EvmRefundChainReader {
    async fn observe(&self, check: &RefundCheck) -> Result<RefundObservation, RefundReadError> {
        let provider = self
            .providers
            .get(&check.chain_id)
            .ok_or(RefundReadError::UnknownChain(check.chain_id))?;
        let receipt = provider
            .get_transaction_receipt(check.tx_hash)
            .await
            .map_err(|_| RefundReadError::Rpc("transaction receipt fetch"))?;
        let Some(receipt) = receipt else {
            return Ok(RefundObservation::Pending);
        };
        let block_number = receipt
            .block_number
            .ok_or(RefundReadError::MissingField("receipt.block_number"))?;
        let finalized = provider
            .get_block_by_number(BlockNumberOrTag::Finalized)
            .await
            .map_err(|_| RefundReadError::Rpc("finalized head fetch"))?
            .ok_or(RefundReadError::MissingField("finalized block"))?
            .header
            .inner
            .number;
        if block_number > finalized {
            return Ok(RefundObservation::Pending);
        }

        let mut transferred_atomic = U256::ZERO;
        for log in receipt.logs() {
            if log.address() != check.asset_contract {
                continue;
            }
            let Ok(transfer) = log.log_decode_validate::<Transfer>() else {
                continue;
            };
            if transfer.inner.data.from == check.treasury
                && transfer.inner.data.to == check.to_address
            {
                transferred_atomic = transferred_atomic
                    .checked_add(transfer.inner.data.amount)
                    .ok_or(RefundReadError::Rpc("matching transfer sum overflow"))?;
            }
        }
        Ok(RefundObservation::Finalized {
            block_number,
            succeeded: receipt.status(),
            transferred_atomic,
        })
    }
}

/// Runtime scheduling for refund confirmation checks.
#[derive(Clone, Copy, Debug)]
pub struct RefundConfirmationConfig {
    /// Delay between empty polls.
    pub poll_interval: Duration,
    /// Delay before a sent refund is eligible for another check.
    pub retry_interval: Duration,
}

impl Default for RefundConfirmationConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            retry_interval: Duration::from_secs(60),
        }
    }
}

/// PostgreSQL-backed refund confirmation loop.
pub struct RefundConfirmationWorker<R> {
    pool: PgPool,
    reader: R,
    treasuries: BTreeMap<u64, Address>,
    config: RefundConfirmationConfig,
}

impl<R> RefundConfirmationWorker<R>
where
    R: RefundChainReader,
{
    /// Creates a worker and validates that each chain has one treasury.
    pub fn new(
        pool: PgPool,
        reader: R,
        routes: &[RouteFile],
        config: RefundConfirmationConfig,
    ) -> Result<Self, RefundReadError> {
        let mut treasuries = BTreeMap::new();
        for route in routes {
            match treasuries.insert(route.chain.chain_id, route.chain.contracts.treasury) {
                Some(existing) if existing != route.chain.contracts.treasury => {
                    return Err(RefundReadError::MissingField(
                        "consistent chain treasury configuration",
                    ));
                }
                Some(_) | None => {}
            }
        }
        Ok(Self {
            pool,
            reader,
            treasuries,
            config,
        })
    }

    /// Checks at most one due sent refund.
    pub async fn check_once(&self) -> Result<bool, sqlx::Error> {
        let retry_seconds = i32::try_from(self.config.retry_interval.as_secs()).unwrap_or(i32::MAX);
        let Some(row) = claim_due_refund(&self.pool, retry_seconds).await? else {
            return Ok(false);
        };
        let chain_id = u64::try_from(row.chain_id).map_err(decode_error)?;
        let Some(treasury) = self.treasuries.get(&chain_id).copied() else {
            let evidence = json!({"result": "configuration_mismatch", "chain_id": chain_id});
            persist_evidence(&self.pool, row.refund_id, &evidence).await?;
            tracing::error!(refund_id = %row.refund_id, chain_id, "refund confirmation has no treasury configuration");
            return Ok(true);
        };
        let check = row.into_check(treasury)?;
        match self.reader.observe(&check).await {
            Ok(RefundObservation::Pending) => {
                persist_evidence(&self.pool, check.refund_id, &json!({"result": "pending"}))
                    .await?;
            }
            Ok(RefundObservation::Finalized {
                block_number,
                succeeded,
                transferred_atomic,
            }) => {
                let matched = succeeded && transferred_atomic >= check.amount_atomic;
                let evidence = json!({
                    "result": if matched { "matched" } else { "mismatch" },
                    "block_number": block_number,
                    "succeeded": succeeded,
                    "expected_amount_atomic": check.amount_atomic.to_string(),
                    "matching_transfer_atomic": transferred_atomic.to_string(),
                    "asset_contract": format!("{:#x}", check.asset_contract),
                    "treasury": format!("{:#x}", check.treasury),
                    "to_address": format!("{:#x}", check.to_address),
                    "tx_hash": format!("{:#x}", check.tx_hash),
                });
                if matched {
                    confirm_refund(&self.pool, &check, &evidence).await?;
                } else {
                    persist_evidence(&self.pool, check.refund_id, &evidence).await?;
                    tracing::error!(refund_id = %check.refund_id, tx_hash = %check.tx_hash, "finalized refund transaction does not match the approved transfer");
                }
            }
            Err(error) => {
                tracing::warn!(refund_id = %check.refund_id, %error, "refund confirmation chain read failed");
            }
        }
        Ok(true)
    }

    /// Runs until cancellation, retrying database and chain failures indefinitely.
    pub async fn run(&self, cancellation: CancellationToken) {
        loop {
            if cancellation.is_cancelled() {
                return;
            }
            let pause = match self.check_once().await {
                Ok(found) => !found,
                Err(error) => {
                    tracing::error!(%error, "refund confirmation database poll failed");
                    true
                }
            };
            if pause {
                tokio::select! {
                    () = cancellation.cancelled() => return,
                    () = tokio::time::sleep(self.config.poll_interval) => {}
                }
            }
        }
    }
}

#[derive(FromRow)]
struct RefundCheckRow {
    refund_id: Uuid,
    product_id: Uuid,
    deposit_id: Uuid,
    chain_id: i64,
    asset_contract: String,
    to_address: String,
    amount_atomic: String,
    tx_hash: String,
}

impl RefundCheckRow {
    fn into_check(self, treasury: Address) -> Result<RefundCheck, sqlx::Error> {
        Ok(RefundCheck {
            refund_id: self.refund_id,
            product_id: self.product_id,
            deposit_id: self.deposit_id,
            chain_id: u64::try_from(self.chain_id).map_err(decode_error)?,
            asset_contract: self.asset_contract.parse().map_err(decode_error)?,
            treasury,
            to_address: self.to_address.parse().map_err(decode_error)?,
            amount_atomic: self.amount_atomic.parse().map_err(decode_error)?,
            tx_hash: self.tx_hash.parse().map_err(decode_error)?,
        })
    }
}

async fn claim_due_refund(
    pool: &PgPool,
    retry_seconds: i32,
) -> Result<Option<RefundCheckRow>, sqlx::Error> {
    sqlx::query_as::<_, RefundCheckRow>(
        r#"
        WITH candidate AS (
            SELECT refund.id
            FROM refunds AS refund
            WHERE refund.status = 'sent' AND refund.next_check_at <= now()
            ORDER BY refund.next_check_at, refund.id
            FOR UPDATE SKIP LOCKED
            LIMIT 1
        )
        UPDATE refunds AS refund
        SET next_check_at = now() + make_interval(secs => $1), updated_at = now()
        FROM candidate, deposits AS deposit, accounts AS account
        WHERE refund.id = candidate.id
          AND deposit.id = refund.deposit_id
          AND account.id = deposit.account_id
        RETURNING refund.id AS refund_id, account.product_id, deposit.id AS deposit_id,
                  deposit.chain_id, deposit.asset_contract, refund.to_address,
                  refund.amount_atomic::text AS amount_atomic, refund.tx_hash
        "#,
    )
    .bind(retry_seconds)
    .fetch_optional(pool)
    .await
}

async fn persist_evidence(
    pool: &PgPool,
    refund_id: Uuid,
    evidence: &Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE refunds SET confirmation_evidence = $2, updated_at = now() WHERE id = $1 AND status = 'sent'",
    )
    .bind(refund_id)
    .bind(evidence)
    .execute(pool)
    .await?;
    Ok(())
}

async fn confirm_refund(
    pool: &PgPool,
    check: &RefundCheck,
    evidence: &Value,
) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let updated = sqlx::query(
        r#"
        UPDATE refunds
        SET status = 'confirmed', confirmation_evidence = $2,
            confirmed_at = now(), updated_at = now()
        WHERE id = $1 AND status = 'sent'
        "#,
    )
    .bind(check.refund_id)
    .bind(evidence)
    .execute(&mut *transaction)
    .await?;
    if updated.rows_affected() == 1 {
        sqlx::query(
            r#"
            INSERT INTO outbox (id, event_type, payload, next_attempt_at)
            VALUES ($1, 'deposit.refunded', $2, now())
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(json!({
            "product_id": check.product_id,
            "deposit_id": check.deposit_id,
            "refund_id": check.refund_id,
            "chain_id": check.chain_id,
            "asset_contract": format!("{:#x}", check.asset_contract),
            "amount_atomic": check.amount_atomic.to_string(),
            "to_address": format!("{:#x}", check.to_address),
            "tx_hash": format!("{:#x}", check.tx_hash),
        }))
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(())
}

fn provider_environment_name(provider_id: &str) -> String {
    let normalized = provider_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("TOPUP_RPC_{normalized}_URL")
}

fn decode_error(error: impl Display) -> sqlx::Error {
    sqlx::Error::Decode(error.to_string().into())
}

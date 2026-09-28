//! Merchant refunds (design D5): screening a refund's destination, and verifying the transaction
//! the merchant attached with `mark_paid` once it is final on both providers.
//!
//! The merchant pays a refund from the treasury of the deposit's own address; the service sends
//! nothing. At `finalized`, both providers must show the same receipt with a `Transfer` of the
//! deposit's token from that treasury to the destination for exactly the amount, in a log no other
//! refund uses. Then the refund is `succeeded` and `deposit.refunded` is sent; a finalized
//! transaction that does not pay it makes it `failed` with a `failure_reason`, which releases its
//! reservation of the deposit.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;
use std::sync::Arc;
use std::time::Duration;

use alloy::sol;
use alloy_primitives::{Address, B256, U256};
use async_trait::async_trait;
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use tokio_util::sync::CancellationToken;
use topup_adapters::chain::evm::EvmClient;
use topup_adapters::risk::oracle::{SanctionsOracle, SanctionsSource};
use topup_core::refund::{ExpectedRefund, RefundTransfer, match_refund_transfer};
use topup_core::route::RouteFile;
use topup_core::screening::SanctionsAnswer;
use uuid::Uuid;

use crate::routes::RouteSet;

sol! {
    event Transfer(address indexed from, address indexed to, uint256 amount);
}

/// One provider's view of an attached refund transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RefundReceipt {
    /// No receipt, or its block is above the provider's `finalized`.
    Pending,
    /// The receipt is at or below `finalized`.
    Finalized {
        /// Including block number.
        block_number: u64,
        /// Including block hash.
        block_hash: B256,
        /// Whether EVM execution succeeded.
        succeeded: bool,
        /// Every ERC-20 `Transfer` log of the receipt, in order.
        transfers: Vec<RefundTransfer>,
    },
}

/// Chain-read failure while reading a refund transaction.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RefundReadError {
    /// No RPC client is configured for the chain.
    #[error("no refund reader for chain {0}")]
    UnknownChain(u64),
    /// A provider is unusable.
    #[error("refund RPC configuration: {0}")]
    Configuration(String),
    /// The RPC request failed during the named operation.
    #[error("refund RPC failed during {0}")]
    Rpc(&'static str),
    /// The RPC response omitted a required field.
    #[error("refund RPC omitted `{0}`")]
    MissingField(&'static str),
}

/// One provider's finality-aware reads of refund transactions.
#[async_trait]
pub trait RefundChainReader: Send + Sync {
    /// Reads the transaction's receipt on `chain_id`.
    async fn receipt(&self, chain_id: u64, tx_hash: B256)
    -> Result<RefundReceipt, RefundReadError>;
}

/// Refund reads through one provider of each chain.
pub struct EvmRefundChainReader {
    clients: BTreeMap<u64, Arc<EvmClient>>,
}

impl EvmRefundChainReader {
    /// Reads every configured chain through its provider at `index` (0 is A, 1 is B).
    pub fn from_routes(routes: &RouteSet, index: usize) -> Result<Self, RefundReadError> {
        let mut clients = BTreeMap::new();
        for chain_id in routes.chain_ids() {
            let client = routes
                .provider(chain_id, index)
                .map_err(|error| RefundReadError::Configuration(error.to_string()))?;
            clients.insert(chain_id, Arc::clone(client));
        }
        Ok(Self::new(clients))
    }

    /// Reads through explicit per-chain clients.
    #[must_use]
    pub const fn new(clients: BTreeMap<u64, Arc<EvmClient>>) -> Self {
        Self { clients }
    }
}

#[async_trait]
impl RefundChainReader for EvmRefundChainReader {
    async fn receipt(
        &self,
        chain_id: u64,
        tx_hash: B256,
    ) -> Result<RefundReceipt, RefundReadError> {
        let client = self
            .clients
            .get(&chain_id)
            .ok_or(RefundReadError::UnknownChain(chain_id))?;
        let receipt = client
            .receipt(tx_hash)
            .await
            .map_err(|_| RefundReadError::Rpc("transaction receipt fetch"))?;
        let Some(receipt) = receipt else {
            return Ok(RefundReceipt::Pending);
        };
        let block_number = receipt
            .block_number
            .ok_or(RefundReadError::MissingField("receipt.block_number"))?;
        let block_hash = receipt
            .block_hash
            .ok_or(RefundReadError::MissingField("receipt.block_hash"))?;
        let finalized = client
            .finalized_block()
            .await
            .map_err(|_| RefundReadError::Rpc("finalized head fetch"))?
            .ok_or(RefundReadError::MissingField("finalized block"))?;
        if block_number > finalized {
            return Ok(RefundReceipt::Pending);
        }
        let mut transfers = Vec::new();
        for log in receipt.logs() {
            let Ok(transfer) = log.log_decode_validate::<Transfer>() else {
                continue;
            };
            transfers.push(RefundTransfer {
                log_index: log
                    .log_index
                    .ok_or(RefundReadError::MissingField("receipt.log.log_index"))?,
                token: log.address(),
                from: transfer.inner.data.from,
                to: transfer.inner.data.to,
                amount: transfer.inner.data.amount,
            });
        }
        Ok(RefundReceipt::Finalized {
            block_number,
            block_hash,
            succeeded: receipt.status(),
            transfers,
        })
    }
}

/// Sanctions screening of a refund destination on the route's chain and oracle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DestinationScreening {
    /// Neither provider lists the destination.
    Clear,
    /// A provider lists the destination.
    Sanctioned,
    /// A provider could not answer; the request can be retried.
    Unavailable,
}

/// Screens refund destinations (design §8: Phala's software does not help move funds to a
/// sanctioned address).
#[async_trait]
pub trait DestinationScreener: Send + Sync {
    /// Screens `destination` with `route`'s sanctions oracle on its chain.
    async fn screen(&self, route: &RouteFile, destination: Address) -> DestinationScreening;
}

/// Screens through the route's sanctions oracle on its chain's first two providers, at provider
/// A's `finalized` block, as the deposit screening step does at the deposit's block.
pub struct OracleDestinationScreener {
    routes: Arc<RouteSet>,
}

impl OracleDestinationScreener {
    /// Screens on the providers of `routes`.
    #[must_use]
    pub const fn new(routes: Arc<RouteSet>) -> Self {
        Self { routes }
    }

    async fn answers(
        &self,
        route: &RouteFile,
        destination: Address,
    ) -> Option<[SanctionsAnswer; 2]> {
        let chain_id = route.chain.chain_id;
        let primary = self.routes.provider(chain_id, 0).ok()?;
        let secondary = self.routes.provider(chain_id, 1).ok()?;
        let block = match primary.finalized_block().await {
            Ok(Some(block)) => block,
            Ok(None) | Err(_) => return None,
        };
        let oracle = SanctionsOracle::new(
            Arc::clone(primary),
            Arc::clone(secondary),
            route.screening.sanctions_oracle,
        )
        .ok()?;
        let result = oracle.sanctions(destination, block).await;
        Some([result.provider_a, result.provider_b])
    }
}

#[async_trait]
impl DestinationScreener for OracleDestinationScreener {
    async fn screen(&self, route: &RouteFile, destination: Address) -> DestinationScreening {
        match self.answers(route, destination).await {
            Some(answers) if answers.contains(&SanctionsAnswer::Sanctioned) => {
                DestinationScreening::Sanctioned
            }
            Some([SanctionsAnswer::Clear, SanctionsAnswer::Clear]) => DestinationScreening::Clear,
            Some(_) | None => DestinationScreening::Unavailable,
        }
    }
}

/// Screening that is never available, for an instance that creates no refunds.
pub struct UnavailableDestinationScreener;

#[async_trait]
impl DestinationScreener for UnavailableDestinationScreener {
    async fn screen(&self, _route: &RouteFile, _destination: Address) -> DestinationScreening {
        DestinationScreening::Unavailable
    }
}

/// Runtime scheduling for refund verification.
#[derive(Clone, Copy, Debug)]
pub struct RefundVerificationConfig {
    /// Delay between empty polls.
    pub poll_interval: Duration,
    /// Delay before an attached transaction is read again.
    pub retry_interval: Duration,
    /// Maximum duration of both providers' reads.
    pub observe_timeout: Duration,
}

impl Default for RefundVerificationConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            retry_interval: Duration::from_secs(60),
            observe_timeout: Duration::from_secs(20),
        }
    }
}

/// What one verification pass did with a refund.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Verification {
    /// No refund was due.
    Idle,
    /// The transaction is not final on both providers, the providers disagree, or a read failed.
    Waiting,
    /// The refund succeeded.
    Succeeded,
    /// The refund failed.
    Failed,
}

/// PostgreSQL-backed verification of attached refund transactions on two providers.
pub struct RefundVerificationWorker<A, B> {
    pool: PgPool,
    primary: A,
    secondary: B,
    config: RefundVerificationConfig,
}

impl<A, B> RefundVerificationWorker<A, B>
where
    A: RefundChainReader,
    B: RefundChainReader,
{
    /// Verifies on provider A (`primary`) and provider B (`secondary`).
    pub const fn new(
        pool: PgPool,
        primary: A,
        secondary: B,
        config: RefundVerificationConfig,
    ) -> Self {
        Self {
            pool,
            primary,
            secondary,
            config,
        }
    }

    /// Verifies at most one due refund.
    pub async fn check_once(&self) -> Result<Verification, sqlx::Error> {
        let retry_seconds = i32::try_from(self.config.retry_interval.as_secs()).unwrap_or(i32::MAX);
        let Some(row) = claim_due_refund(&self.pool, retry_seconds).await? else {
            return Ok(Verification::Idle);
        };
        let check = row.into_check()?;
        let reads = async {
            tokio::join!(
                self.primary.receipt(check.chain_id, check.tx_hash),
                self.secondary.receipt(check.chain_id, check.tx_hash)
            )
        };
        let (primary, secondary) = match tokio::time::timeout(self.config.observe_timeout, reads)
            .await
        {
            Ok((Ok(primary), Ok(secondary))) => (primary, secondary),
            Ok((Err(error), _) | (_, Err(error))) => {
                tracing::warn!(refund_id = %check.refund_id, %error, "refund verification chain read failed");
                return Ok(Verification::Waiting);
            }
            Err(_) => {
                persist_evidence(&self.pool, &check, &json!({"result": "observe_timeout"})).await?;
                tracing::warn!(refund_id = %check.refund_id, "refund verification timed out");
                return Ok(Verification::Waiting);
            }
        };
        let (block_number, block_hash, succeeded, transfers) = match (&primary, &secondary) {
            (
                RefundReceipt::Finalized {
                    block_number,
                    block_hash,
                    succeeded,
                    transfers,
                },
                _,
            ) if primary == secondary => (*block_number, *block_hash, *succeeded, transfers),
            (RefundReceipt::Pending, _) | (_, RefundReceipt::Pending) => {
                persist_evidence(&self.pool, &check, &json!({"result": "pending"})).await?;
                return Ok(Verification::Waiting);
            }
            _ => {
                persist_evidence(&self.pool, &check, &json!({"result": "providers_disagree"}))
                    .await?;
                tracing::warn!(refund_id = %check.refund_id, "providers disagree on a finalized refund transaction");
                return Ok(Verification::Waiting);
            }
        };
        let used = used_logs(&self.pool, &check).await?;
        let outcome = match_refund_transfer(
            &check.expected,
            succeeded,
            transfers,
            check.log_index,
            &used,
        );
        let evidence = json!({
            "result": match outcome {
                Ok(_) => "matched",
                Err(reason) => reason.code(),
            },
            "block_number": block_number,
            "block_hash": format!("{block_hash:#x}"),
            "succeeded": succeeded,
            "token": format!("{:#x}", check.expected.token),
            "treasury": format!("{:#x}", check.expected.treasury),
            "destination_address": format!("{:#x}", check.expected.destination),
            "amount_atomic": check.expected.amount.to_string(),
            "transfers": transfers.iter().map(|transfer| json!({
                "log_index": transfer.log_index,
                "token": format!("{:#x}", transfer.token),
                "from": format!("{:#x}", transfer.from),
                "to": format!("{:#x}", transfer.to),
                "amount_atomic": transfer.amount.to_string(),
            })).collect::<Vec<_>>(),
        });
        match outcome {
            Ok(log_index) => {
                if succeed(&self.pool, &check, log_index, &evidence).await? {
                    return Ok(Verification::Succeeded);
                }
                // Another refund took the log since it was read; the next pass sees it used.
                persist_evidence(&self.pool, &check, &json!({"result": "transfer_claimed"}))
                    .await?;
                Ok(Verification::Waiting)
            }
            Err(reason) => {
                fail(&self.pool, &check, reason.code(), &evidence).await?;
                tracing::warn!(refund_id = %check.refund_id, reason = reason.code(), "finalized refund transaction does not pay the refund");
                Ok(Verification::Failed)
            }
        }
    }

    /// Runs until cancellation, retrying database and chain failures indefinitely.
    pub async fn run(&self, cancellation: CancellationToken) {
        loop {
            if cancellation.is_cancelled() {
                return;
            }
            let pause = tokio::select! {
                biased;
                () = cancellation.cancelled() => return,
                result = self.check_once() => match result {
                    Ok(verification) => verification == Verification::Idle,
                    Err(error) => {
                        tracing::error!(%error, "refund verification database poll failed");
                        true
                    }
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

struct RefundCheck {
    refund_id: Uuid,
    account_id: Uuid,
    livemode: bool,
    deposit_id: Uuid,
    chain_id: u64,
    tx_hash: B256,
    log_index: Option<u64>,
    expected: ExpectedRefund,
}

#[derive(FromRow)]
struct RefundCheckRow {
    refund_id: Uuid,
    account_id: Uuid,
    livemode: bool,
    deposit_id: Uuid,
    chain_id: i64,
    tx_hash: String,
    log_index: Option<i64>,
    token: String,
    treasury: String,
    destination_address: String,
    amount_atomic: String,
}

impl RefundCheckRow {
    fn into_check(self) -> Result<RefundCheck, sqlx::Error> {
        Ok(RefundCheck {
            refund_id: self.refund_id,
            account_id: self.account_id,
            livemode: self.livemode,
            deposit_id: self.deposit_id,
            chain_id: u64::try_from(self.chain_id).map_err(decode_error)?,
            tx_hash: self.tx_hash.parse().map_err(decode_error)?,
            log_index: self
                .log_index
                .map(u64::try_from)
                .transpose()
                .map_err(decode_error)?,
            expected: ExpectedRefund {
                token: self.token.parse().map_err(decode_error)?,
                treasury: self.treasury.parse().map_err(decode_error)?,
                destination: self.destination_address.parse().map_err(decode_error)?,
                amount: self.amount_atomic.parse::<U256>().map_err(decode_error)?,
            },
        })
    }
}

/// Claims the refund due first and pushes its next check back by `retry_seconds`. The expected
/// sender is the treasury of the deposit's own address, fixed when the address was issued.
async fn claim_due_refund(
    pool: &PgPool,
    retry_seconds: i32,
) -> Result<Option<RefundCheckRow>, sqlx::Error> {
    sqlx::query_as::<_, RefundCheckRow>(
        r#"
        WITH candidate AS (
            SELECT refund.id
            FROM refunds AS refund
            WHERE refund.status = 'pending' AND refund.tx_hash IS NOT NULL
              AND refund.next_check_at <= now()
            ORDER BY refund.next_check_at, refund.id
            FOR UPDATE SKIP LOCKED
            LIMIT 1
        )
        UPDATE refunds AS refund
        SET next_check_at = now() + make_interval(secs => $1), updated_at = now()
        FROM candidate, deposits AS deposit, addresses AS address
        WHERE refund.id = candidate.id
          AND deposit.id = refund.deposit_id
          AND address.id = deposit.address_id
        RETURNING refund.id AS refund_id, refund.account_id, refund.livemode, refund.deposit_id,
                  refund.chain_id, refund.tx_hash, refund.log_index,
                  deposit.asset_contract AS token, address.treasury,
                  refund.destination_address, refund.amount_atomic::text AS amount_atomic
        "#,
    )
    .bind(retry_seconds)
    .fetch_optional(pool)
    .await
}

/// Logs of the transaction that pay or are named by another live refund.
async fn used_logs(pool: &PgPool, check: &RefundCheck) -> Result<BTreeSet<u64>, sqlx::Error> {
    let rows = sqlx::query_scalar::<_, i64>(
        r#"
        SELECT log_index
        FROM refunds
        WHERE chain_id = $1 AND tx_hash = $2 AND id <> $3 AND log_index IS NOT NULL
          AND status IN ('pending', 'succeeded')
        "#,
    )
    .bind(to_i64(check.chain_id)?)
    .bind(format!("{:#x}", check.tx_hash))
    .bind(check.refund_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|index| u64::try_from(index).map_err(decode_error))
        .collect()
}

async fn persist_evidence(
    pool: &PgPool,
    check: &RefundCheck,
    evidence: &Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE refunds
        SET confirmation_evidence = $3, updated_at = now()
        WHERE id = $1 AND status = 'pending' AND tx_hash = $2
        "#,
    )
    .bind(check.refund_id)
    .bind(format!("{:#x}", check.tx_hash))
    .bind(evidence)
    .execute(pool)
    .await?;
    Ok(())
}

/// Marks the refund `succeeded` with its log and sends `deposit.refunded`; `false` when the refund
/// is no longer pending or another refund took the log first.
async fn succeed(
    pool: &PgPool,
    check: &RefundCheck,
    log_index: u64,
    evidence: &Value,
) -> Result<bool, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let updated = sqlx::query(
        r#"
        UPDATE refunds
        SET status = 'succeeded', log_index = $3, confirmation_evidence = $4, updated_at = now()
        WHERE id = $1 AND status = 'pending' AND tx_hash = $2
        "#,
    )
    .bind(check.refund_id)
    .bind(format!("{:#x}", check.tx_hash))
    .bind(to_i64(log_index)?)
    .bind(evidence)
    .execute(&mut *transaction)
    .await;
    match updated {
        Ok(result) if result.rows_affected() == 1 => {}
        Ok(_) => return Ok(false),
        Err(sqlx::Error::Database(error))
            if error.constraint() == Some("refunds_transfer_unique") =>
        {
            return Ok(false);
        }
        Err(error) => return Err(error),
    }
    // The event's object is the deposit, with its refunded amount (as Stripe's `charge.refunded`
    // is the charge); its id is derived from the refund, one event per refund.
    crate::db::enqueue_in(
        &mut transaction,
        &crate::db::NewOutboxEvent {
            id: topup_core::identity::event_id("deposit.refunded", check.refund_id),
            event_type: "deposit.refunded".to_owned(),
            account_id: check.account_id,
            livemode: check.livemode,
            object: crate::db::EventObject::Deposit(check.deposit_id),
            next_attempt_at: chrono::Utc::now(),
            actor: crate::db::SYSTEM_ACTOR.to_owned(),
        },
    )
    .await?;
    transaction.commit().await?;
    Ok(true)
}

async fn fail(
    pool: &PgPool,
    check: &RefundCheck,
    reason: &str,
    evidence: &Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE refunds
        SET status = 'failed', failure_reason = $3, confirmation_evidence = $4, updated_at = now()
        WHERE id = $1 AND status = 'pending' AND tx_hash = $2
        "#,
    )
    .bind(check.refund_id)
    .bind(format!("{:#x}", check.tx_hash))
    .bind(reason)
    .bind(evidence)
    .execute(pool)
    .await?;
    Ok(())
}

fn to_i64(value: u64) -> Result<i64, sqlx::Error> {
    i64::try_from(value).map_err(decode_error)
}

fn decode_error(error: impl Display) -> sqlx::Error {
    sqlx::Error::Decode(error.to_string().into())
}

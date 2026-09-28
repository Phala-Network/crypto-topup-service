//! Finality watch (design D1, architecture §7): every deposit that is not final yet is re-read on
//! both providers by its transaction's receipt whenever provider A's `finalized` advances.
//!
//! - Receipt at or below `finalized` on both, with the same transfer at the deposit's receipt
//!   position: the deposit is final (`final_at`), and its evidence follows the block it is in.
//! - Receipt in a newer block that is not final: the transaction was re-included; its evidence is
//!   followed and nothing is reversed.
//! - Receipt at or below `finalized` without the transfer, or no receipt on both providers while
//!   the sender's nonce at `finalized` is past the transaction's (another transaction consumed
//!   it): the deposit is `reversed`, `deposit.reversed` is sent if the account was told of it,
//!   and a quote it consumed opens again (or expires).
//! - No receipt and the nonce not consumed: the transaction is pending again; the watch waits and
//!   alerts after an hour.
//!
//! Anything else (the providers disagree) waits for the next advance.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{Address, B256};
use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use tokio_util::sync::CancellationToken;
use topup_adapters::chain::evm::{
    ChainError, ChainReader, FinalizedReader, ReceiptLookup, TransferLog,
};
use topup_core::deposit::{DepositState, reverse};
use topup_core::identity::{event_id, reversed_event_id};
use topup_core::money::AtomicAmount;
use uuid::Uuid;

use crate::db::{self, EventObject, NewOutboxEvent};
use crate::routes::RouteSet;

/// How often provider A's `finalized` is polled; a pass runs only when it advanced.
pub const FINALITY_POLL_INTERVAL: Duration = Duration::from_secs(12);
/// Deposits re-read in one pass, oldest block first.
const WATCH_BATCH: i64 = 500;
/// A transaction that left the chain without a replacement is alerted on after this long.
const PENDING_AFTER_REORG_ALERT: TimeDelta = TimeDelta::hours(1);

/// Failure of one watch pass; the next pass retries.
#[derive(Debug, thiserror::Error)]
pub enum FinalityError {
    /// A chain read failed.
    #[error("{0}")]
    Chain(#[from] ChainError),
    /// A database operation failed.
    #[error("{0}")]
    Database(#[from] sqlx::Error),
    /// The chain has no configured providers.
    #[error("chain {0} is not configured")]
    UnknownChain(u64),
}

#[async_trait]
trait WatchReader: Send + Sync {
    async fn finalized(&self) -> Result<u64, ChainError>;
    async fn receipt_transfer(
        &self,
        tx_hash: B256,
        receipt_log_index: u64,
    ) -> Result<ReceiptLookup, ChainError>;
    async fn nonce_at(&self, account: Address, block: u64) -> Result<u64, ChainError>;
}

#[async_trait]
impl<R: ChainReader + Send + Sync> WatchReader for R {
    async fn finalized(&self) -> Result<u64, ChainError> {
        Ok(ChainReader::finalized_head(self).await?.number)
    }

    async fn receipt_transfer(
        &self,
        tx_hash: B256,
        receipt_log_index: u64,
    ) -> Result<ReceiptLookup, ChainError> {
        ChainReader::receipt_transfer(self, tx_hash, receipt_log_index).await
    }

    async fn nonce_at(&self, account: Address, block: u64) -> Result<u64, ChainError> {
        ChainReader::nonce_at(self, account, block).await
    }
}

struct WatchChain {
    primary: Arc<dyn WatchReader>,
    secondary: Arc<dyn WatchReader>,
}

/// Work done by one pass over a chain.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WatchStats {
    /// Deposits read.
    pub watched: u64,
    /// Deposits that became final.
    pub finalized: u64,
    /// Deposits whose re-included transaction was followed.
    pub followed: u64,
    /// Deposits reversed.
    pub reversed: u64,
}

/// The finality watch over every configured chain.
pub struct FinalityWatch {
    pool: PgPool,
    chains: BTreeMap<u64, WatchChain>,
}

impl FinalityWatch {
    /// Watches every configured chain on its first two providers.
    pub fn from_routes(pool: PgPool, routes: &RouteSet) -> Result<Self, String> {
        let mut chains = BTreeMap::new();
        for chain_id in routes.chain_ids() {
            let reader = |index| -> Result<Arc<dyn WatchReader>, String> {
                let client = routes
                    .provider(chain_id, index)
                    .map_err(|error| error.to_string())?;
                Ok(Arc::new(FinalizedReader::new(Arc::clone(client))))
            };
            chains.insert(
                chain_id,
                WatchChain {
                    primary: reader(0)?,
                    secondary: reader(1)?,
                },
            );
        }
        Ok(Self { pool, chains })
    }

    /// Watches one chain with injected readers, for tests.
    pub fn single<R1, R2>(pool: PgPool, chain_id: u64, primary: R1, secondary: R2) -> Self
    where
        R1: ChainReader + Send + Sync + 'static,
        R2: ChainReader + Send + Sync + 'static,
    {
        Self {
            pool,
            chains: BTreeMap::from([(
                chain_id,
                WatchChain {
                    primary: Arc::new(primary),
                    secondary: Arc::new(secondary),
                },
            )]),
        }
    }

    /// Re-reads every deposit of `chain_id` that is neither final nor reversed.
    pub async fn watch_once(&self, chain_id: u64) -> Result<WatchStats, FinalityError> {
        let chain = self
            .chains
            .get(&chain_id)
            .ok_or(FinalityError::UnknownChain(chain_id))?;
        let mut stats = WatchStats::default();
        if crate::reconciler::chain_is_blocked(&self.pool, chain_id).await? {
            return Ok(stats);
        }
        let deposits = unfinal_deposits(&self.pool, chain_id).await?;
        if deposits.is_empty() {
            return Ok(stats);
        }
        let (primary_finalized, secondary_finalized) =
            tokio::try_join!(chain.primary.finalized(), chain.secondary.finalized())?;
        for deposit in deposits {
            stats.watched = stats.watched.saturating_add(1);
            let (primary, secondary) = tokio::try_join!(
                chain
                    .primary
                    .receipt_transfer(deposit.tx_hash, deposit.receipt_log_index),
                chain
                    .secondary
                    .receipt_transfer(deposit.tx_hash, deposit.receipt_log_index),
            )?;
            let nonces = match (&primary, &secondary, deposit.origin) {
                (ReceiptLookup::Missing, ReceiptLookup::Missing, Some((from, _))) => {
                    Some(tokio::try_join!(
                        chain.primary.nonce_at(from, primary_finalized),
                        chain.secondary.nonce_at(from, secondary_finalized),
                    )?)
                }
                _ => None,
            };
            let verdict = decide(
                &deposit,
                Observed {
                    finalized: primary_finalized,
                    receipt: &primary,
                },
                Observed {
                    finalized: secondary_finalized,
                    receipt: &secondary,
                },
                nonces,
            );
            match self.apply(&deposit, verdict, chain_id).await? {
                Applied::Final => stats.finalized = stats.finalized.saturating_add(1),
                Applied::Followed => stats.followed = stats.followed.saturating_add(1),
                Applied::Reversed => stats.reversed = stats.reversed.saturating_add(1),
                Applied::Nothing => {}
            }
        }
        Ok(stats)
    }

    /// Runs a pass per chain whenever provider A's `finalized` advanced, until cancellation.
    /// Failures are logged and retried at the next poll.
    pub async fn run(&self, cancellation: CancellationToken) {
        let mut watched = BTreeMap::<u64, u64>::new();
        let monitor = crate::observability::CronMonitor::finality_watch();
        loop {
            monitor.check_in(true);
            for (&chain_id, chain) in &self.chains {
                let finalized = tokio::select! {
                    () = cancellation.cancelled() => return,
                    result = chain.primary.finalized() => result,
                };
                let finalized = match finalized {
                    Ok(finalized) => finalized,
                    Err(error) => {
                        tracing::warn!(chain_id, %error, "finality watch head read failed");
                        continue;
                    }
                };
                if watched.get(&chain_id) == Some(&finalized) {
                    continue;
                }
                let result = tokio::select! {
                    () = cancellation.cancelled() => return,
                    result = self.watch_once(chain_id) => result,
                };
                match result {
                    Ok(stats) => {
                        watched.insert(chain_id, finalized);
                        if stats.watched > 0 {
                            tracing::info!(
                                chain_id,
                                finalized,
                                watched = stats.watched,
                                became_final = stats.finalized,
                                followed = stats.followed,
                                reversed = stats.reversed,
                                "finality watch pass"
                            );
                        }
                    }
                    Err(error) => {
                        tracing::warn!(chain_id, %error, "finality watch pass failed; retrying");
                    }
                }
            }
            tokio::select! {
                () = cancellation.cancelled() => return,
                () = tokio::time::sleep(FINALITY_POLL_INTERVAL) => {}
            }
        }
    }

    async fn apply(
        &self,
        deposit: &WatchedDeposit,
        verdict: Verdict,
        chain_id: u64,
    ) -> Result<Applied, FinalityError> {
        match verdict {
            Verdict::Wait => Ok(Applied::Nothing),
            Verdict::Pending => {
                if Utc::now().signed_duration_since(deposit.block_time) > PENDING_AFTER_REORG_ALERT
                {
                    tracing::warn!(
                        tags.alert = "TopupDepositPendingAfterReorg",
                        tags.chain_id = chain_id,
                        tags.state = db::state_code(deposit.state),
                        deposit_id = %deposit.id,
                        tx_hash = %deposit.tx_hash,
                        "a deposit's transaction left the chain and is still pending"
                    );
                }
                Ok(Applied::Nothing)
            }
            Verdict::Follow(block) => {
                let applied = record_evidence(&self.pool, deposit, &block, false).await?;
                Ok(if applied {
                    Applied::Followed
                } else {
                    Applied::Nothing
                })
            }
            Verdict::Final(block) => {
                let applied = record_evidence(&self.pool, deposit, &block, true).await?;
                Ok(if applied {
                    Applied::Final
                } else {
                    Applied::Nothing
                })
            }
            Verdict::Reverse(evidence) => {
                let reversed = reverse_deposit(&self.pool, deposit, evidence).await?;
                if reversed {
                    tracing::warn!(
                        tags.alert = "TopupDepositReversed",
                        tags.chain_id = chain_id,
                        tags.state = db::state_code(deposit.state),
                        deposit_id = %deposit.id,
                        tx_hash = %deposit.tx_hash,
                        "a deposit's transfer is not in the final chain; the deposit is reversed"
                    );
                    Ok(Applied::Reversed)
                } else {
                    Ok(Applied::Nothing)
                }
            }
        }
    }
}

enum Applied {
    Final,
    Followed,
    Reversed,
    Nothing,
}

/// A deposit that is neither final nor reversed, with the stored evidence the watch compares.
#[derive(Clone, Debug)]
struct WatchedDeposit {
    id: Uuid,
    state: DepositState,
    attempt: i32,
    tx_hash: B256,
    receipt_log_index: u64,
    log_index: u64,
    block_hash: B256,
    block_time: DateTime<Utc>,
    address: Address,
    asset_contract: Address,
    from_address: Address,
    amount_atomic: AtomicAmount,
    /// The transaction's sender and nonce; absent on deposits recorded before fast credit.
    origin: Option<(Address, u64)>,
}

impl WatchedDeposit {
    fn is_same_transfer(&self, transfer: &TransferLog) -> bool {
        transfer.to == self.address
            && transfer.token == self.asset_contract
            && transfer.from == self.from_address
            && transfer.amount == self.amount_atomic
    }
}

/// The block a transfer is in now.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BlockEvidence {
    log_index: u64,
    block_number: u64,
    block_hash: B256,
    block_time: DateTime<Utc>,
}

impl From<&TransferLog> for BlockEvidence {
    fn from(transfer: &TransferLog) -> Self {
        Self {
            log_index: transfer.log_index,
            block_number: transfer.block_number,
            block_hash: transfer.block_hash,
            block_time: transfer.block_time,
        }
    }
}

#[derive(Clone, Copy)]
struct Observed<'a> {
    finalized: u64,
    receipt: &'a ReceiptLookup,
}

#[derive(Debug, PartialEq)]
enum Verdict {
    /// Both providers show the transfer at or below `finalized`.
    Final(BlockEvidence),
    /// Both providers show the transfer re-included in a newer block that is not final.
    Follow(BlockEvidence),
    /// The transfer is not part of the final chain.
    Reverse(Value),
    /// The transaction is in no block and its nonce is still unused.
    Pending,
    /// Nothing to record now.
    Wait,
}

/// Decides what the providers' receipts mean for a deposit; `nonces` are the sender's nonces at
/// each provider's `finalized`, read only when neither provider has a receipt.
fn decide(
    deposit: &WatchedDeposit,
    primary: Observed<'_>,
    secondary: Observed<'_>,
    nonces: Option<(u64, u64)>,
) -> Verdict {
    match (primary.receipt, secondary.receipt) {
        (
            ReceiptLookup::Included {
                block_number,
                block_hash,
                transfer: primary_transfer,
            },
            ReceiptLookup::Included {
                block_hash: secondary_hash,
                transfer: secondary_transfer,
                ..
            },
        ) if block_hash == secondary_hash && primary_transfer == secondary_transfer => {
            let is_final =
                *block_number <= primary.finalized && *block_number <= secondary.finalized;
            match primary_transfer.as_deref() {
                Some(transfer) if deposit.is_same_transfer(transfer) => {
                    let block = BlockEvidence::from(transfer);
                    if is_final {
                        Verdict::Final(block)
                    } else if block.block_hash != deposit.block_hash
                        || block.log_index != deposit.log_index
                    {
                        Verdict::Follow(block)
                    } else {
                        Verdict::Wait
                    }
                }
                // A detected deposit's evidence is provisional: its confirm step corrects a
                // transfer both providers agree on, still to this address.
                Some(transfer)
                    if deposit.state == DepositState::Detected
                        && transfer.to == deposit.address =>
                {
                    Verdict::Wait
                }
                _ if is_final => Verdict::Reverse(json!({
                    "stage": "finality",
                    "result": "transfer_absent_at_finality",
                    "block_number": block_number,
                    "block_hash": format!("{block_hash:#x}"),
                    "provider_a_finalized": primary.finalized,
                    "provider_b_finalized": secondary.finalized,
                })),
                _ => Verdict::Wait,
            }
        }
        (ReceiptLookup::Missing, ReceiptLookup::Missing) => match (deposit.origin, nonces) {
            (Some((from, nonce)), Some((primary_nonce, secondary_nonce)))
                if primary_nonce > nonce && secondary_nonce > nonce =>
            {
                Verdict::Reverse(json!({
                    "stage": "finality",
                    "result": "dropped_nonce_consumed",
                    "tx_from": format!("{from:#x}"),
                    "tx_nonce": nonce,
                    "provider_a_nonce": primary_nonce,
                    "provider_b_nonce": secondary_nonce,
                    "provider_a_finalized": primary.finalized,
                    "provider_b_finalized": secondary.finalized,
                }))
            }
            _ => Verdict::Pending,
        },
        _ => Verdict::Wait,
    }
}

async fn unfinal_deposits(
    pool: &PgPool,
    chain_id: u64,
) -> Result<Vec<WatchedDeposit>, FinalityError> {
    let rows = sqlx::query(
        r#"
        SELECT deposit.id, deposit.state, deposit.attempt, deposit.tx_hash,
               deposit.receipt_log_index, deposit.log_index, deposit.block_hash, deposit.block_time, address.address, deposit.asset_contract,
               deposit.from_address, deposit.amount_atomic::text AS amount_atomic,
               deposit.tx_from, deposit.tx_nonce::text AS tx_nonce
        FROM deposits AS deposit
        JOIN addresses AS address ON address.id = deposit.address_id
        WHERE deposit.chain_id = $1 AND deposit.final_at IS NULL AND deposit.state <> 'reversed'
        ORDER BY deposit.block_number, deposit.id
        LIMIT $2
        "#,
    )
    .bind(to_i64(chain_id)?)
    .bind(WATCH_BATCH)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let tx_from: Option<String> = row.try_get("tx_from")?;
            let tx_nonce: Option<String> = row.try_get("tx_nonce")?;
            let origin = match (tx_from, tx_nonce) {
                (Some(from), Some(nonce)) => {
                    Some((parse(&from)?, nonce.parse::<u64>().map_err(decode_error)?))
                }
                _ => None,
            };
            Ok(WatchedDeposit {
                id: row.try_get("id")?,
                state: db::parse_state(&row.try_get::<String, _>("state")?)?,
                attempt: row.try_get("attempt")?,
                tx_hash: parse(&row.try_get::<String, _>("tx_hash")?)?,
                receipt_log_index: to_u64(row.try_get("receipt_log_index")?)?,
                log_index: to_u64(row.try_get("log_index")?)?,
                block_hash: parse(&row.try_get::<String, _>("block_hash")?)?,
                block_time: row.try_get("block_time")?,
                address: parse(&row.try_get::<String, _>("address")?)?,
                asset_contract: parse(&row.try_get::<String, _>("asset_contract")?)?,
                from_address: parse(&row.try_get::<String, _>("from_address")?)?,
                amount_atomic: AtomicAmount::new(
                    row.try_get::<String, _>("amount_atomic")?
                        .parse()
                        .map_err(decode_error)?,
                ),
                origin,
            })
        })
        .collect()
}

/// Records where the transfer is now and, when `is_final`, that the deposit is final; a final
/// deposit is then linked to a confirmed flush after it. Returns whether the row changed.
async fn record_evidence(
    pool: &PgPool,
    deposit: &WatchedDeposit,
    block: &BlockEvidence,
    is_final: bool,
) -> Result<bool, FinalityError> {
    let mut transaction = pool.begin().await?;
    let updated = sqlx::query(
        r#"
        UPDATE deposits
        SET log_index = $2, block_number = $3, block_hash = $4, block_time = $5,
            final_at = CASE WHEN $6 THEN now() END,
            updated_at = now()
        WHERE id = $1 AND final_at IS NULL AND state <> 'reversed'
        RETURNING state
        "#,
    )
    .bind(deposit.id)
    .bind(to_i64(block.log_index)?)
    .bind(to_i64(block.block_number)?)
    .bind(format!("{:#x}", block.block_hash))
    .bind(block.block_time)
    .bind(is_final)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(row) = updated else {
        return Ok(false);
    };
    let state: String = row.try_get("state")?;
    let moved = block.block_hash != deposit.block_hash || block.log_index != deposit.log_index;
    insert_transition(
        &mut transaction,
        deposit.id,
        &state,
        &state,
        deposit.attempt,
        &json!({
            "stage": "finality",
            "result": if is_final { "final" } else { "followed" },
            "moved": moved,
            "log_index": block.log_index,
            "block_number": block.block_number,
            "block_hash": format!("{:#x}", block.block_hash),
        }),
    )
    .await?;
    if is_final {
        db::link_deposit_to_flush(&mut transaction, deposit.id).await?;
    }
    transaction.commit().await?;
    Ok(true)
}

/// Reverses a deposit that is still in the observed state and not final: the transition and, for
/// a deposit the account was told of (`credited` or `rejected`), `deposit.reversed`; a quote it
/// consumed opens again while its window lasts, or expires with `quote.expired`. A pending refund
/// cannot exist: refunds require a final deposit.
async fn reverse_deposit(
    pool: &PgPool,
    deposit: &WatchedDeposit,
    evidence: Value,
) -> Result<bool, FinalityError> {
    let Ok(transition) = reverse(deposit.state) else {
        return Ok(false);
    };
    let from = db::state_code(transition.from);
    let to = db::state_code(transition.to);
    let mut transaction = pool.begin().await?;
    let owner = sqlx::query_as::<_, (Uuid, bool)>(
        r#"
        UPDATE deposits
        SET state = $3, reason = NULL, lease_token = NULL, lease_until = NULL,
            next_attempt_at = now(), updated_at = now()
        WHERE id = $1 AND state = $2 AND final_at IS NULL
        RETURNING account_id, livemode
        "#,
    )
    .bind(deposit.id)
    .bind(from)
    .bind(to)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some((account_id, livemode)) = owner else {
        return Ok(false);
    };
    insert_transition(
        &mut transaction,
        deposit.id,
        from,
        to,
        deposit.attempt,
        &evidence,
    )
    .await?;
    if matches!(
        transition.from,
        DepositState::Credited | DepositState::Rejected
    ) {
        db::enqueue_in(
            &mut transaction,
            &NewOutboxEvent {
                id: reversed_event_id(deposit.id),
                event_type: "deposit.reversed".to_owned(),
                account_id,
                livemode,
                object: EventObject::Deposit(deposit.id),
                next_attempt_at: Utc::now(),
            },
        )
        .await?;
    }
    let reopened = sqlx::query(
        r#"
        UPDATE quotes
        SET consumed_by = NULL,
            status = CASE WHEN expires_at > now() THEN 'open' ELSE 'expired' END,
            exposure_reserved = expires_at > now(),
            closed_at = CASE WHEN expires_at > now() THEN NULL ELSE now() END
        WHERE consumed_by = $1
        RETURNING id, status
        "#,
    )
    .bind(deposit.id)
    .fetch_optional(&mut *transaction)
    .await?;
    if let Some(row) = reopened
        && row.try_get::<String, _>("status")? == "expired"
    {
        let quote_id: Uuid = row.try_get("id")?;
        db::enqueue_in(
            &mut transaction,
            &NewOutboxEvent {
                id: event_id("quote.expired", quote_id),
                event_type: "quote.expired".to_owned(),
                account_id,
                livemode,
                object: EventObject::Quote(quote_id),
                next_attempt_at: Utc::now(),
            },
        )
        .await?;
    }
    transaction.commit().await?;
    Ok(true)
}

async fn insert_transition(
    transaction: &mut Transaction<'_, Postgres>,
    deposit_id: Uuid,
    from: &str,
    to: &str,
    attempt: i32,
    evidence: &Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO transitions (id, deposit_id, from_state, to_state, attempt, evidence)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(deposit_id)
    .bind(from)
    .bind(to)
    .bind(attempt)
    .bind(evidence)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn to_i64(value: u64) -> Result<i64, sqlx::Error> {
    i64::try_from(value).map_err(|error| sqlx::Error::Encode(error.to_string().into()))
}

fn to_u64(value: i64) -> Result<u64, sqlx::Error> {
    u64::try_from(value).map_err(decode_error)
}

fn parse<T: std::str::FromStr>(value: &str) -> Result<T, sqlx::Error>
where
    T::Err: std::fmt::Display,
{
    value.parse().map_err(decode_error)
}

fn decode_error(error: impl std::fmt::Display) -> sqlx::Error {
    sqlx::Error::Decode(error.to_string().into())
}

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;

    use super::*;

    fn deposit(state: DepositState) -> WatchedDeposit {
        WatchedDeposit {
            id: Uuid::nil(),
            state,
            attempt: 0,
            tx_hash: B256::repeat_byte(1),
            receipt_log_index: 0,
            log_index: 5,
            block_hash: B256::repeat_byte(2),
            block_time: DateTime::UNIX_EPOCH,
            address: Address::repeat_byte(3),
            asset_contract: Address::repeat_byte(4),
            from_address: Address::repeat_byte(5),
            amount_atomic: AtomicAmount::new(U256::from(7)),
            origin: Some((Address::repeat_byte(6), 9)),
        }
    }

    fn transfer(deposit: &WatchedDeposit, block_number: u64, block_byte: u8) -> TransferLog {
        TransferLog {
            tx_hash: deposit.tx_hash,
            receipt_log_index: deposit.receipt_log_index,
            log_index: deposit.log_index,
            block_number,
            block_hash: B256::repeat_byte(block_byte),
            block_time: DateTime::UNIX_EPOCH,
            tx_from: Address::repeat_byte(6),
            tx_nonce: 9,
            token: deposit.asset_contract,
            from: deposit.from_address,
            to: deposit.address,
            amount: deposit.amount_atomic,
        }
    }

    fn included(transfer: Option<TransferLog>, block_number: u64, byte: u8) -> ReceiptLookup {
        ReceiptLookup::Included {
            block_number,
            block_hash: B256::repeat_byte(byte),
            transfer: transfer.map(Box::new),
        }
    }

    fn observed(finalized: u64, receipt: &ReceiptLookup) -> Observed<'_> {
        Observed { finalized, receipt }
    }

    #[test]
    fn the_same_transfer_at_or_below_finalized_is_final_and_follows_its_block() {
        let deposit = deposit(DepositState::Credited);
        let receipt = included(Some(transfer(&deposit, 100, 2)), 100, 2);
        assert!(matches!(
            decide(&deposit, observed(100, &receipt), observed(120, &receipt), None),
            Verdict::Final(block) if block.block_hash == deposit.block_hash
        ));
        let moved = included(Some(transfer(&deposit, 104, 8)), 104, 8);
        assert!(matches!(
            decide(&deposit, observed(110, &moved), observed(110, &moved), None),
            Verdict::Final(block) if block.block_number == 104
        ));
    }

    #[test]
    fn a_re_included_transaction_is_followed_not_reversed() {
        let deposit = deposit(DepositState::Credited);
        let mut later = transfer(&deposit, 103, 8);
        later.log_index = 11;
        let receipt = included(Some(later), 103, 8);
        assert!(matches!(
            decide(&deposit, observed(90, &receipt), observed(90, &receipt), None),
            Verdict::Follow(block) if block.block_number == 103 && block.log_index == 11
        ));
        // Unchanged evidence below finality: nothing to write.
        let same = included(Some(transfer(&deposit, 100, 2)), 100, 2);
        assert_eq!(
            decide(&deposit, observed(90, &same), observed(99, &same), None),
            Verdict::Wait
        );
    }

    #[test]
    fn a_dropped_transaction_is_reversed_only_when_both_providers_prove_its_nonce_consumed() {
        let deposit = deposit(DepositState::Credited);
        let missing = ReceiptLookup::Missing;
        assert!(matches!(
            decide(
                &deposit,
                observed(200, &missing),
                observed(200, &missing),
                Some((10, 10))
            ),
            Verdict::Reverse(evidence) if evidence["result"] == "dropped_nonce_consumed"
        ));
        assert_eq!(
            decide(
                &deposit,
                observed(200, &missing),
                observed(200, &missing),
                Some((10, 9))
            ),
            Verdict::Pending
        );
        let mut legacy = deposit.clone();
        legacy.origin = None;
        assert_eq!(
            decide(
                &legacy,
                observed(200, &missing),
                observed(200, &missing),
                Some((10, 10))
            ),
            Verdict::Pending
        );
    }

    #[test]
    fn a_final_receipt_without_the_transfer_reverses_and_disagreement_waits() {
        let deposit = deposit(DepositState::Rejected);
        let without = included(None, 100, 2);
        assert!(matches!(
            decide(&deposit, observed(100, &without), observed(100, &without), None),
            Verdict::Reverse(evidence) if evidence["result"] == "transfer_absent_at_finality"
        ));
        // Not final yet: the transaction may still change.
        assert_eq!(
            decide(
                &deposit,
                observed(99, &without),
                observed(100, &without),
                None
            ),
            Verdict::Wait
        );
        let with = included(Some(transfer(&deposit, 100, 2)), 100, 2);
        assert_eq!(
            decide(
                &deposit,
                observed(100, &with),
                observed(100, &without),
                None
            ),
            Verdict::Wait
        );
        let missing = ReceiptLookup::Missing;
        assert_eq!(
            decide(
                &deposit,
                observed(100, &with),
                observed(100, &missing),
                None
            ),
            Verdict::Wait
        );
    }

    #[test]
    fn a_detected_deposit_with_other_provisional_evidence_is_left_to_its_confirm_step() {
        let deposit = deposit(DepositState::Detected);
        let mut corrected = transfer(&deposit, 100, 2);
        corrected.amount = AtomicAmount::new(U256::from(8));
        let receipt = included(Some(corrected.clone()), 100, 2);
        assert_eq!(
            decide(
                &deposit,
                observed(100, &receipt),
                observed(100, &receipt),
                None
            ),
            Verdict::Wait
        );
        let credited = WatchedDeposit {
            state: DepositState::Credited,
            ..deposit
        };
        assert!(matches!(
            decide(
                &credited,
                observed(100, &receipt),
                observed(100, &receipt),
                None
            ),
            Verdict::Reverse(_)
        ));
    }
}

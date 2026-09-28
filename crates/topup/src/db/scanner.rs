use alloy_primitives::Address as EvmAddress;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use topup_core::deposit::{DepositState, RejectReason};
use topup_core::identity::{deposit_id, event_id};
use uuid::Uuid;

use super::deposits::{NewDeposit, insert_deposit_in};
use super::types::{parse_address, to_i64, to_u64};

/// Address metadata required by the finalized-log scanner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanAddress {
    /// Address row identifier.
    pub id: Uuid,
    /// Physical EVM address.
    pub address: EvmAddress,
    /// Earliest block requiring inspection.
    pub created_block: u64,
    /// Whether the one-time pre-cursor range has been scanned.
    pub backfilled: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct ScanAddressRecord {
    id: Uuid,
    address: String,
    created_block: i64,
    backfilled: bool,
}

impl TryFrom<ScanAddressRecord> for ScanAddress {
    type Error = sqlx::Error;

    fn try_from(record: ScanAddressRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.id,
            address: parse_address(&record.address)?,
            created_block: to_u64(record.created_block, "addresses.created_block")?,
            backfilled: record.backfilled,
        })
    }
}

/// Scanner writes committed atomically for one block range.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanCommit {
    /// Number of newly inserted deposits; duplicates are excluded.
    pub inserted: u64,
    /// Newly inserted deposits rejected as unsupported assets.
    pub unsupported_inserted: u64,
}

async fn insert_rejected_event(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    deposit: &NewDeposit,
) -> Result<(), sqlx::Error> {
    let id = deposit_id(deposit.chain_id, deposit.tx_hash, deposit.receipt_log_index);
    let (account_id, livemode): (Uuid, bool) =
        sqlx::query_as("SELECT account_id, livemode FROM deposits WHERE id = $1")
            .bind(id)
            .fetch_one(&mut **transaction)
            .await?;
    super::outbox::enqueue_in(
        transaction,
        &super::outbox::NewOutboxEvent {
            id: event_id("deposit.rejected", id),
            event_type: "deposit.rejected".to_owned(),
            account_id,
            livemode,
            object: super::outbox::EventObject::Deposit(id),
            next_attempt_at: Utc::now(),
            actor: crate::db::SYSTEM_ACTOR.to_owned(),
        },
    )
    .await
}

/// Returns the last completely committed block for a chain.
pub async fn get_cursor(pool: &PgPool, chain_id: u64) -> Result<Option<u64>, sqlx::Error> {
    let chain_id = to_i64(chain_id, "cursors.chain_id")?;
    let scanned_block =
        sqlx::query_scalar::<_, i64>("SELECT scanned_block FROM cursors WHERE chain_id = $1")
            .bind(chain_id)
            .fetch_optional(pool)
            .await?;
    scanned_block
        .map(|value| to_u64(value, "cursors.scanned_block"))
        .transpose()
}

/// Returns the last block the fast scanner committed at the route confirmation, if any.
pub async fn get_confirmed_cursor(
    pool: &PgPool,
    chain_id: u64,
) -> Result<Option<u64>, sqlx::Error> {
    let chain_id = to_i64(chain_id, "cursors.chain_id")?;
    let confirmed_block = sqlx::query_scalar::<_, Option<i64>>(
        "SELECT confirmed_block FROM cursors WHERE chain_id = $1",
    )
    .bind(chain_id)
    .fetch_optional(pool)
    .await?
    .flatten();
    confirmed_block
        .map(|value| to_u64(value, "cursors.confirmed_block"))
        .transpose()
}

/// Commits deposits the fast scanner found at the route confirmation and advances its cursor
/// to `confirmed_block`, atomically. The cursor never moves backwards, and it needs a finalized
/// cursor row: the fast scan starts above the finalized scanner's range.
pub async fn commit_confirmed_scan(
    pool: &PgPool,
    chain_id: u64,
    deposits: &[NewDeposit],
    confirmed_block: u64,
) -> Result<ScanCommit, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let commit = insert_deposits_in(&mut transaction, deposits).await?;
    sqlx::query(
        r#"
        UPDATE cursors
        SET confirmed_block = GREATEST(COALESCE(confirmed_block, 0), $2)
        WHERE chain_id = $1
        "#,
    )
    .bind(to_i64(chain_id, "cursors.chain_id")?)
    .bind(to_i64(confirmed_block, "cursors.confirmed_block")?)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(commit)
}

async fn insert_deposits_in(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    deposits: &[NewDeposit],
) -> Result<ScanCommit, sqlx::Error> {
    let mut inserted = 0_u64;
    let mut unsupported_inserted = 0_u64;
    for deposit in deposits {
        if insert_deposit_in(transaction, deposit).await? {
            inserted = inserted.checked_add(1).ok_or_else(|| {
                sqlx::Error::Protocol("inserted deposit count overflowed u64".to_owned())
            })?;
            if deposit.reason == Some(RejectReason::UnsupportedAsset) {
                unsupported_inserted = unsupported_inserted.checked_add(1).ok_or_else(|| {
                    sqlx::Error::Protocol("unsupported deposit count overflowed u64".to_owned())
                })?;
            }
            if deposit.state == DepositState::Rejected {
                insert_rejected_event(transaction, deposit).await?;
            }
        }
    }
    Ok(ScanCommit {
        inserted,
        unsupported_inserted,
    })
}

/// Loads every issued address of a chain: every quote's, whatever its status, and every deposit
/// address's network on the chain, active, retired, or superseded.
pub async fn list_scan_addresses(
    pool: &PgPool,
    chain_id: u64,
) -> Result<Vec<ScanAddress>, sqlx::Error> {
    let chain_id = to_i64(chain_id, "addresses.chain_id")?;
    let records = sqlx::query_as::<_, ScanAddressRecord>(
        r#"
        SELECT id, address, created_block, backfilled
        FROM addresses
        WHERE chain_id = $1
        ORDER BY created_block, id
        "#,
    )
    .bind(chain_id)
    .fetch_all(pool)
    .await?;
    records.into_iter().map(TryInto::try_into).collect()
}

/// Commits deposits, backfill markers, and an optional cursor advance atomically. A cursor
/// advance also deletes the display-only pending rows it now covers.
///
/// `scanned_block_time` is the block time of `scanned_block` when the advance reaches the
/// finalized head the scanner observed; it is ignored without a cursor advance. The stored time
/// never moves backwards and stays a lower bound on the cursor block's time, so quote expiry
/// (§9) can rely on every block up to that time being committed.
///
/// A deposit born `rejected` (no route for its asset) never passes through a pump step, so its
/// `deposit.rejected` event is written here, in the same transaction and only on first insert.
/// Deposits already recorded by the fast scanner are left as they are: the insert is keyed by the
/// receipt position, and the finality watch follows their evidence.
pub async fn commit_scan(
    pool: &PgPool,
    chain_id: u64,
    deposits: &[NewDeposit],
    backfilled_address_ids: &[Uuid],
    scanned_block: Option<u64>,
    scanned_block_time: Option<DateTime<Utc>>,
) -> Result<ScanCommit, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let commit = insert_deposits_in(&mut transaction, deposits).await?;

    if !backfilled_address_ids.is_empty() {
        sqlx::query("UPDATE addresses SET backfilled = true WHERE id = ANY($1)")
            .bind(backfilled_address_ids)
            .execute(&mut *transaction)
            .await?;
    }

    if let Some(scanned_block) = scanned_block {
        let chain_id = to_i64(chain_id, "cursors.chain_id")?;
        let scanned_block = to_i64(scanned_block, "cursors.scanned_block")?;
        sqlx::query(
            r#"
            INSERT INTO cursors (chain_id, scanned_block, scanned_block_time)
            VALUES ($1, $2, $3)
            ON CONFLICT (chain_id) DO UPDATE
            SET scanned_block = EXCLUDED.scanned_block,
                scanned_block_time = GREATEST(
                    cursors.scanned_block_time, EXCLUDED.scanned_block_time
                )
            WHERE cursors.scanned_block <= EXCLUDED.scanned_block
            "#,
        )
        .bind(chain_id)
        .bind(scanned_block)
        .bind(scanned_block_time)
        .execute(&mut *transaction)
        .await?;
        super::pending::delete_finalized_in(&mut transaction, chain_id, scanned_block).await?;
    }

    transaction.commit().await?;
    Ok(commit)
}

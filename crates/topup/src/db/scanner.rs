use alloy_primitives::Address as EvmAddress;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use topup_core::deposit::{DepositState, RejectReason};
use topup_core::identity::deposit_id;
use uuid::Uuid;

use super::deposits::{NewDeposit, insert_deposit_in};
use super::types::{parse_address, to_i64, to_u64};

/// Address metadata required by the finalized-log scanner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScanAddress {
    /// Address row identifier.
    pub id: Uuid,
    /// Owning account identifier.
    pub account_id: Uuid,
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
    account_id: Uuid,
    address: String,
    created_block: i64,
    backfilled: bool,
}

impl TryFrom<ScanAddressRecord> for ScanAddress {
    type Error = sqlx::Error;

    fn try_from(record: ScanAddressRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.id,
            account_id: record.account_id,
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
    let reason = deposit.reason.ok_or_else(|| {
        sqlx::Error::Protocol("rejected deposit is missing its reason".to_owned())
    })?;
    let id = deposit_id(deposit.chain_id, deposit.tx_hash, deposit.log_index);
    let inserted = sqlx::query(
        r#"
        INSERT INTO outbox (id, event_type, payload, next_attempt_at)
        SELECT $1, 'deposit.rejected',
               jsonb_build_object(
                   'product_id', account.product_id,
                   'deposit_id', $2::uuid,
                   'chain_id', $5::bigint,
                   'state', 'rejected',
                   'route', $6::text,
                   'reason', $3::text
               ),
               now()
        FROM accounts AS account
        WHERE account.id = $4
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(id)
    .bind(reason.code())
    .bind(deposit.account_id)
    .bind(to_i64(deposit.chain_id, "deposits.chain_id")?)
    .bind(deposit.route.as_deref())
    .execute(&mut **transaction)
    .await?;
    if inserted.rows_affected() == 1 {
        Ok(())
    } else {
        Err(sqlx::Error::Protocol(
            "rejected deposit account does not exist".to_owned(),
        ))
    }
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

/// Loads every address for a chain, including retired persistent and lock addresses.
pub async fn list_scan_addresses(
    pool: &PgPool,
    chain_id: u64,
) -> Result<Vec<ScanAddress>, sqlx::Error> {
    let chain_id = to_i64(chain_id, "addresses.chain_id")?;
    let records = sqlx::query_as::<_, ScanAddressRecord>(
        r#"
        SELECT id, account_id, address, created_block, backfilled
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

/// Commits deposits, backfill markers, and an optional cursor advance atomically.
///
/// `scanned_block_time` is the block time of `scanned_block` when the advance reaches the
/// finalized head the scanner observed; it is ignored without a cursor advance. The stored time
/// never moves backwards and stays a lower bound on the cursor block's time, so rate-lock expiry
/// (§9) can rely on every block up to that time being committed.
///
/// A deposit born `rejected` (no route for its asset) never passes through a pump step, so its
/// `deposit.rejected` event is written here, in the same transaction and only on first insert.
pub async fn commit_scan(
    pool: &PgPool,
    chain_id: u64,
    deposits: &[NewDeposit],
    backfilled_address_ids: &[Uuid],
    scanned_block: Option<u64>,
    scanned_block_time: Option<DateTime<Utc>>,
) -> Result<ScanCommit, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let mut inserted = 0_u64;
    let mut unsupported_inserted = 0_u64;
    for deposit in deposits {
        if insert_deposit_in(&mut transaction, deposit).await? {
            inserted = inserted.checked_add(1).ok_or_else(|| {
                sqlx::Error::Protocol("inserted deposit count overflowed u64".to_owned())
            })?;
            if deposit.reason == Some(RejectReason::UnsupportedAsset) {
                unsupported_inserted = unsupported_inserted.checked_add(1).ok_or_else(|| {
                    sqlx::Error::Protocol("unsupported deposit count overflowed u64".to_owned())
                })?;
            }
            if deposit.state == DepositState::Rejected {
                insert_rejected_event(&mut transaction, deposit).await?;
            }
        }
    }

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
    }

    transaction.commit().await?;
    Ok(ScanCommit {
        inserted,
        unsupported_inserted,
    })
}

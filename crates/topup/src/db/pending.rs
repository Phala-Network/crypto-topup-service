//! Display-only transfers seen above the finalized head (architecture §8, §12).
//!
//! Nothing here reads or writes deposits, transitions, rate locks, exposure, settlements, or
//! reconciliation state. Support, amount matching, and timeliness are computed by readers, so an
//! unfinalized transfer can never produce a stored rejection or credit.

use alloy_primitives::{Address as EvmAddress, B256};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Postgres, Transaction};
use topup_adapters::chain::evm::MAX_ADDRESSES_PER_REQUEST;
use topup_core::identity::deposit_id;
use topup_core::money::AtomicAmount;
use uuid::Uuid;

use super::scanner::ScanAddress;
use super::types::{
    address_hex, atomic_decimal, b256_hex, parse_address, parse_atomic_decimal, parse_b256, to_i64,
    to_u64,
};

/// One transfer to a watched address observed above `finalized` on provider A.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewPendingTransfer {
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Transaction hash.
    pub tx_hash: B256,
    /// Block-wide log index.
    pub log_index: u64,
    /// Block number at observation time.
    pub block_number: u64,
    /// Block hash at observation time.
    pub block_hash: B256,
    /// Block timestamp.
    pub block_time: DateTime<Utc>,
    /// Receiving address row.
    pub address_id: Uuid,
    /// Token contract that emitted the event.
    pub asset_contract: EvmAddress,
    /// Transfer sender.
    pub from_address: EvmAddress,
    /// Atomic token amount.
    pub amount_atomic: AtomicAmount,
}

/// A stored pending transfer as shown to products.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingTransfer {
    /// Identifier the deposit will have once final.
    pub deposit_id: Uuid,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Transaction hash.
    pub tx_hash: B256,
    /// Block-wide log index.
    pub log_index: u64,
    /// Block number at the last observation.
    pub block_number: u64,
    /// Block timestamp.
    pub block_time: DateTime<Utc>,
    /// Provider A latest block at the last observation.
    pub head_block: u64,
    /// Receiving address.
    pub address: EvmAddress,
    /// Token contract that emitted the event.
    pub asset_contract: EvmAddress,
    /// Transfer sender.
    pub from_address: EvmAddress,
    /// Atomic token amount.
    pub amount_atomic: AtomicAmount,
    /// First observation time.
    pub first_seen_at: DateTime<Utc>,
}

impl PendingTransfer {
    /// Blocks including the transfer's own block, as of the last head scan.
    #[must_use]
    pub fn confirmations(&self) -> u64 {
        self.head_block
            .saturating_sub(self.block_number)
            .saturating_add(1)
    }
}

/// Result of one committed head scan.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeadCommit {
    /// Transfers upserted.
    pub seen: u64,
    /// Rows removed as reorged, unwatched, or finalized.
    pub removed: u64,
    /// `deposit.pending` events written for the first time.
    pub announced: u64,
}

/// Addresses the head scan watches: open lock addresses until one hour after expiry, and
/// persistent addresses. When persistent addresses exceed one log request, only those issued or
/// fetched (`requested_at`) in the last 24 hours are watched, most recent first.
pub async fn list_watched_addresses(
    pool: &PgPool,
    chain_id: u64,
) -> Result<Vec<ScanAddress>, sqlx::Error> {
    let chain_id = to_i64(chain_id, "addresses.chain_id")?;
    let limit = i64::try_from(MAX_ADDRESSES_PER_REQUEST)
        .map_err(|error| sqlx::Error::Encode(error.to_string().into()))?;
    let rows = sqlx::query_as::<_, (Uuid, Uuid, String)>(
        r#"
        WITH persistent AS (
            SELECT id, account_id, address, requested_at
            FROM addresses
            WHERE chain_id = $1 AND kind = 'persistent'
        )
        (
            SELECT id, account_id, address
            FROM persistent
            WHERE (SELECT count(*) FROM persistent) <= $2
               OR requested_at > now() - interval '24 hours'
            ORDER BY requested_at DESC, id
            LIMIT $2
        )
        UNION ALL
        SELECT address.id, address.account_id, address.address
        FROM addresses AS address
        JOIN rate_locks AS rate_lock ON rate_lock.address_id = address.id
        WHERE address.chain_id = $1
          AND rate_lock.status IN ('open', 'expired')
          AND rate_lock.consumed_by IS NULL
          AND rate_lock.expires_at + interval '1 hour' > now()
        "#,
    )
    .bind(chain_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|(id, account_id, address)| {
            Ok(ScanAddress {
                id,
                account_id,
                address: parse_address(&address)?,
                created_block: 0,
                backfilled: true,
            })
        })
        .collect()
}

/// Replaces the pending view of blocks `from_block..=head_block` with `transfers`, in one
/// transaction: rows in that range not seen this time (reorged or no longer watched) and rows at
/// or below the finalized cursor are deleted; seen rows are upserted. Rows above `head_block`
/// (a provider briefly behind) are left for the next scan that covers them. The first sighting of a
/// transfer writes one `deposit.pending` event, at most once per chain event ever.
pub async fn commit_head_scan(
    pool: &PgPool,
    chain_id: u64,
    from_block: u64,
    head_block: u64,
    transfers: &[NewPendingTransfer],
) -> Result<HeadCommit, sqlx::Error> {
    let chain = to_i64(chain_id, "pending_transfers.chain_id")?;
    let from_block = to_i64(from_block, "pending_transfers.block_number")?;
    let head = to_i64(head_block, "pending_transfers.head_block")?;
    let mut transaction = pool.begin().await?;
    // `FOR SHARE` makes a concurrent cursor advance wait for this transaction (or this read wait
    // for it), so a row at or below the committed cursor is never inserted after the finalized
    // scanner deleted that range.
    let cursor = sqlx::query_scalar::<_, i64>(
        "SELECT scanned_block FROM cursors WHERE chain_id = $1 FOR SHARE",
    )
    .bind(chain)
    .fetch_optional(&mut *transaction)
    .await?
    .unwrap_or(-1);
    let tx_hashes = transfers
        .iter()
        .map(|transfer| b256_hex(transfer.tx_hash))
        .collect::<Vec<_>>();
    let log_indexes = transfers
        .iter()
        .map(|transfer| to_i64(transfer.log_index, "pending_transfers.log_index"))
        .collect::<Result<Vec<_>, _>>()?;
    let removed = sqlx::query(
        r#"
        DELETE FROM pending_transfers
        WHERE chain_id = $1
          AND (
              block_number <= $2
              OR (
                  block_number BETWEEN $3 AND $6
                  AND (tx_hash, log_index) NOT IN (
                      SELECT seen.tx_hash, seen.log_index
                      FROM unnest($4::text[], $5::bigint[]) AS seen (tx_hash, log_index)
                  )
              )
          )
        "#,
    )
    .bind(chain)
    .bind(cursor)
    .bind(from_block)
    .bind(&tx_hashes)
    .bind(&log_indexes)
    .bind(head)
    .execute(&mut *transaction)
    .await?
    .rows_affected();

    let mut commit = HeadCommit {
        removed,
        ..HeadCommit::default()
    };
    for transfer in transfers {
        let block_number = to_i64(transfer.block_number, "pending_transfers.block_number")?;
        if block_number <= cursor {
            continue;
        }
        upsert(&mut transaction, chain, head, transfer).await?;
        commit.seen = commit.seen.saturating_add(1);
        commit.announced = commit
            .announced
            .saturating_add(announce(&mut transaction, chain, transfer).await?);
    }
    transaction.commit().await?;
    Ok(commit)
}

async fn upsert(
    transaction: &mut Transaction<'_, Postgres>,
    chain: i64,
    head: i64,
    transfer: &NewPendingTransfer,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO pending_transfers (
            chain_id, tx_hash, log_index, block_number, block_hash, block_time, head_block,
            address_id, asset_contract, from_address, amount_atomic
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11::text::numeric)
        ON CONFLICT (chain_id, tx_hash, log_index) DO UPDATE
        SET block_number = EXCLUDED.block_number,
            block_hash = EXCLUDED.block_hash,
            block_time = EXCLUDED.block_time,
            head_block = EXCLUDED.head_block,
            address_id = EXCLUDED.address_id,
            asset_contract = EXCLUDED.asset_contract,
            from_address = EXCLUDED.from_address,
            amount_atomic = EXCLUDED.amount_atomic
        "#,
    )
    .bind(chain)
    .bind(b256_hex(transfer.tx_hash))
    .bind(to_i64(transfer.log_index, "pending_transfers.log_index")?)
    .bind(to_i64(
        transfer.block_number,
        "pending_transfers.block_number",
    )?)
    .bind(b256_hex(transfer.block_hash))
    .bind(transfer.block_time)
    .bind(head)
    .bind(transfer.address_id)
    .bind(address_hex(transfer.asset_contract))
    .bind(address_hex(transfer.from_address))
    .bind(atomic_decimal(transfer.amount_atomic))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

/// Writes `deposit.pending` under an identifier derived from the chain event, so a transfer that
/// is reorged out and seen again, or seen by a second process, is announced only once.
async fn announce(
    transaction: &mut Transaction<'_, Postgres>,
    chain: i64,
    transfer: &NewPendingTransfer,
) -> Result<u64, sqlx::Error> {
    let deposit_id = deposit_id(transfer.chain_id, transfer.tx_hash, transfer.log_index);
    let event_id = Uuid::new_v5(&deposit_id, b"deposit.pending");
    let result = sqlx::query(
        r#"
        INSERT INTO outbox (id, event_type, payload, next_attempt_at)
        SELECT $1, 'deposit.pending',
               jsonb_build_object(
                   'product_id', account.product_id,
                   'external_id', account.external_id,
                   'deposit_id', $2::uuid,
                   'chain_id', $3::bigint,
                   'tx_hash', $4::text,
                   'log_index', $5::bigint,
                   'block_number', $6::bigint,
                   'address', address.address,
                   'product_lock_ref', address.lock_ref,
                   'asset_contract', $7::text,
                   'from_address', $8::text,
                   'amount_atomic', $9::text,
                   'provisional', true
               ),
               now()
        FROM addresses AS address
        JOIN accounts AS account ON account.id = address.account_id
        WHERE address.id = $10
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(event_id)
    .bind(deposit_id)
    .bind(chain)
    .bind(b256_hex(transfer.tx_hash))
    .bind(to_i64(transfer.log_index, "pending_transfers.log_index")?)
    .bind(to_i64(
        transfer.block_number,
        "pending_transfers.block_number",
    )?)
    .bind(address_hex(transfer.asset_contract))
    .bind(address_hex(transfer.from_address))
    .bind(atomic_decimal(transfer.amount_atomic))
    .bind(transfer.address_id)
    .execute(&mut **transaction)
    .await?;
    Ok(result.rows_affected())
}

/// Deletes pending rows the finalized scanner now covers; runs in the cursor-advance transaction.
pub(crate) async fn delete_finalized_in(
    transaction: &mut Transaction<'_, Postgres>,
    chain_id: i64,
    scanned_block: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM pending_transfers WHERE chain_id = $1 AND block_number <= $2")
        .bind(chain_id)
        .bind(scanned_block)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct PendingRecord {
    chain_id: i64,
    tx_hash: String,
    log_index: i64,
    block_number: i64,
    block_time: DateTime<Utc>,
    head_block: i64,
    address: String,
    asset_contract: String,
    from_address: String,
    amount_atomic: String,
    first_seen_at: DateTime<Utc>,
}

impl TryFrom<PendingRecord> for PendingTransfer {
    type Error = sqlx::Error;

    fn try_from(record: PendingRecord) -> Result<Self, Self::Error> {
        let chain_id = to_u64(record.chain_id, "pending_transfers.chain_id")?;
        let tx_hash = parse_b256(&record.tx_hash)?;
        let log_index = to_u64(record.log_index, "pending_transfers.log_index")?;
        Ok(Self {
            deposit_id: deposit_id(chain_id, tx_hash, log_index),
            chain_id,
            tx_hash,
            log_index,
            block_number: to_u64(record.block_number, "pending_transfers.block_number")?,
            block_time: record.block_time,
            head_block: to_u64(record.head_block, "pending_transfers.head_block")?,
            address: parse_address(&record.address)?,
            asset_contract: parse_address(&record.asset_contract)?,
            from_address: parse_address(&record.from_address)?,
            amount_atomic: parse_atomic_decimal(&record.amount_atomic)?,
            first_seen_at: record.first_seen_at,
        })
    }
}

const PENDING_SELECT: &str = r#"
    SELECT pending.chain_id, pending.tx_hash, pending.log_index, pending.block_number,
           pending.block_time, pending.head_block, address.address, pending.asset_contract,
           pending.from_address, pending.amount_atomic::text AS amount_atomic,
           pending.first_seen_at
    FROM pending_transfers AS pending
    JOIN addresses AS address ON address.id = pending.address_id
    WHERE pending.block_number > COALESCE(
        (SELECT scan.scanned_block FROM cursors AS scan WHERE scan.chain_id = pending.chain_id),
        -1
    )
"#;

/// Pending transfers to an account's persistent addresses, oldest first.
pub async fn list_account_pending(
    pool: &PgPool,
    account_id: Uuid,
) -> Result<Vec<PendingTransfer>, sqlx::Error> {
    let query = format!(
        "{PENDING_SELECT} AND address.account_id = $1 AND address.kind = 'persistent' \
         ORDER BY pending.block_number, pending.log_index"
    );
    let records = sqlx::query_as::<_, PendingRecord>(&query)
        .bind(account_id)
        .fetch_all(pool)
        .await?;
    records.into_iter().map(TryInto::try_into).collect()
}

/// Pending transfers to one address, oldest first.
pub async fn list_address_pending(
    pool: &PgPool,
    address_id: Uuid,
) -> Result<Vec<PendingTransfer>, sqlx::Error> {
    let query = format!(
        "{PENDING_SELECT} AND pending.address_id = $1 \
         ORDER BY pending.block_number, pending.log_index"
    );
    let records = sqlx::query_as::<_, PendingRecord>(&query)
        .bind(address_id)
        .fetch_all(pool)
        .await?;
    records.into_iter().map(TryInto::try_into).collect()
}

/// Records that the product issued or fetched an account's persistent addresses, so the head scan
/// keeps watching them when persistent addresses exceed one log request. Writes at most once an
/// hour per address.
pub async fn touch_persistent_requested(
    pool: &PgPool,
    account_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE addresses
        SET requested_at = now()
        WHERE account_id = $1 AND kind = 'persistent'
          AND requested_at < now() - interval '1 hour'
        "#,
    )
    .bind(account_id)
    .execute(pool)
    .await?;
    Ok(())
}

use alloy_primitives::Address as EvmAddress;
use sqlx::{PgPool, Postgres, Transaction};
use topup_core::deposit::RejectReason;
use topup_core::identity::deposit_id;
use uuid::Uuid;

use super::deposits::NewDeposit;
use super::state_code;
use super::types::{address_hex, atomic_decimal, b256_hex, parse_address, to_i64, to_u64};

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
pub async fn commit_scan(
    pool: &PgPool,
    chain_id: u64,
    deposits: &[NewDeposit],
    backfilled_address_ids: &[Uuid],
    scanned_block: Option<u64>,
) -> Result<ScanCommit, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let mut inserted = 0_u64;
    for deposit in deposits {
        if insert_deposit(&mut transaction, deposit).await? {
            inserted = inserted.checked_add(1).ok_or_else(|| {
                sqlx::Error::Protocol("inserted deposit count overflowed u64".to_owned())
            })?;
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
            INSERT INTO cursors (chain_id, scanned_block)
            VALUES ($1, $2)
            ON CONFLICT (chain_id) DO UPDATE
            SET scanned_block = EXCLUDED.scanned_block
            WHERE cursors.scanned_block <= EXCLUDED.scanned_block
            "#,
        )
        .bind(chain_id)
        .bind(scanned_block)
        .execute(&mut *transaction)
        .await?;
    }

    transaction.commit().await?;
    Ok(ScanCommit { inserted })
}

async fn insert_deposit(
    transaction: &mut Transaction<'_, Postgres>,
    deposit: &NewDeposit,
) -> Result<bool, sqlx::Error> {
    let id = deposit_id(deposit.chain_id, deposit.tx_hash, deposit.log_index);
    let result = sqlx::query(
        r#"
        INSERT INTO deposits (
            id, chain_id, tx_hash, log_index, block_number, block_hash, block_time,
            address_id, account_id, route, route_version, asset_contract, from_address,
            amount_atomic, state, reason, next_attempt_at
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
            $14::text::numeric, $15, $16, $17
        )
        ON CONFLICT (chain_id, tx_hash, log_index) DO NOTHING
        "#,
    )
    .bind(id)
    .bind(to_i64(deposit.chain_id, "deposits.chain_id")?)
    .bind(b256_hex(deposit.tx_hash))
    .bind(to_i64(deposit.log_index, "deposits.log_index")?)
    .bind(to_i64(deposit.block_number, "deposits.block_number")?)
    .bind(b256_hex(deposit.block_hash))
    .bind(deposit.block_time)
    .bind(deposit.address_id)
    .bind(deposit.account_id)
    .bind(&deposit.route)
    .bind(
        deposit
            .route_version
            .map(|value| to_i64(value, "deposits.route_version"))
            .transpose()?,
    )
    .bind(address_hex(deposit.asset_contract))
    .bind(address_hex(deposit.from_address))
    .bind(atomic_decimal(deposit.amount_atomic))
    .bind(state_code(deposit.state))
    .bind(deposit.reason.map(RejectReason::code))
    .bind(deposit.next_attempt_at)
    .execute(&mut **transaction)
    .await?;
    Ok(result.rows_affected() == 1)
}

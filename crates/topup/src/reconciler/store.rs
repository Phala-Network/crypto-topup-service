use std::collections::BTreeSet;
use std::time::Duration;

use alloy_primitives::{Address, U256};
use serde_json::json;
use sqlx::{Connection as _, PgConnection, PgPool, Row};
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::routes::RouteSet;

use super::{Finding, ReconciliationError};

/// Persists a finding once and returns whether this call inserted it.
///
/// Every first insertion writes an audit row.
pub(crate) async fn persist_finding(
    pool: &PgPool,
    finding: &Finding,
) -> Result<bool, ReconciliationError> {
    let mut transaction = pool.begin().await?;
    let inserted = sqlx::query(
        r#"
        INSERT INTO reconciliation_findings
            (id, fingerprint, check_name, subjects, expected, observed, repair_applied, incomplete)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (fingerprint) DO NOTHING
        "#,
    )
    .bind(finding.id)
    .bind(&finding.fingerprint)
    .bind(finding.check.code())
    .bind(serde_json::to_value(&finding.subjects)?)
    .bind(&finding.expected)
    .bind(&finding.observed)
    .bind(finding.repair_applied)
    .bind(finding.incomplete)
    .execute(&mut *transaction)
    .await?
    .rows_affected()
        == 1;

    if inserted {
        let action = if finding.repair_applied {
            "reconciliation_repair"
        } else {
            "reconciliation_mismatch"
        };
        sqlx::query(
            r#"
            INSERT INTO audit (id, actor, action, subject, reason)
            VALUES ($1, 'reconciler', $2, $3, $4)
            "#,
        )
        .bind(Uuid::new_v5(&finding.id, b"audit"))
        .bind(action)
        .bind(serde_json::to_string(&finding.subjects)?)
        .bind(
            json!({
                "check": finding.check.code(),
                "expected": finding.expected,
                "observed": finding.observed,
            })
            .to_string(),
        )
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(inserted)
}

pub(crate) async fn block_address(
    pool: &PgPool,
    chain_id: u64,
    address_id: Uuid,
    check_name: &str,
    reason: &str,
) -> Result<(), ReconciliationError> {
    sqlx::query(
        r#"
        INSERT INTO reconciliation_blocks
            (block_key, scope, chain_id, address_id, check_name, reason)
        VALUES ($1, 'address', $2, $3, $4, $5)
        ON CONFLICT (block_key) DO NOTHING
        "#,
    )
    .bind(format!("address:{address_id}"))
    .bind(db_i64(chain_id)?)
    .bind(address_id)
    .bind(check_name)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn block_chain(
    pool: &PgPool,
    chain_id: u64,
    check_name: &str,
    reason: &str,
) -> Result<(), ReconciliationError> {
    sqlx::query(
        r#"
        INSERT INTO reconciliation_blocks
            (block_key, scope, chain_id, address_id, check_name, reason)
        VALUES ($1, 'chain', $2, NULL, $3, $4)
        ON CONFLICT (block_key) DO NOTHING
        "#,
    )
    .bind(format!("chain:{chain_id}"))
    .bind(db_i64(chain_id)?)
    .bind(check_name)
    .bind(reason)
    .execute(pool)
    .await?;
    Ok(())
}

/// Returns whether a chain has a persistent reconciliation freeze.
///
/// A frozen chain pauses its scanner, pumps, flusher, address issuance, and rate-lock creation
/// until the database owner deletes the block row.
pub async fn chain_is_blocked(pool: &PgPool, chain_id: u64) -> Result<bool, sqlx::Error> {
    let chain_id =
        i64::try_from(chain_id).map_err(|error| sqlx::Error::Encode(error.to_string().into()))?;
    sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM reconciliation_blocks WHERE scope = 'chain' AND chain_id = $1)",
    )
    .bind(chain_id)
    .fetch_one(pool)
    .await
}

/// Returns the configured chains which reconciliation has frozen.
pub async fn frozen_chains(pool: &PgPool, routes: &RouteSet) -> Result<BTreeSet<u64>, sqlx::Error> {
    let mut frozen = BTreeSet::new();
    for chain_id in routes.chain_ids() {
        if chain_is_blocked(pool, chain_id).await? {
            frozen.insert(chain_id);
        }
    }
    Ok(frozen)
}

/// Returns address ids excluded from flushing by persistent reconciliation blocks.
pub async fn blocked_addresses(pool: &PgPool, chain_id: u64) -> Result<Vec<Uuid>, sqlx::Error> {
    let chain_id =
        i64::try_from(chain_id).map_err(|error| sqlx::Error::Encode(error.to_string().into()))?;
    sqlx::query_scalar(
        "SELECT address_id FROM reconciliation_blocks WHERE scope = 'address' AND chain_id = $1 ORDER BY address_id",
    )
    .bind(chain_id)
    .fetch_all(pool)
    .await
}

/// Per-address ledger totals for chain positions at or below one finalized block.
pub(crate) async fn address_totals(
    pool: &PgPool,
    chain_id: u64,
    token: Address,
    finalized: u64,
) -> Result<Vec<(Uuid, U256, U256)>, ReconciliationError> {
    let rows = sqlx::query(
        r#"
        SELECT a.id,
               COALESCE((SELECT SUM(d.amount_atomic) FROM deposits d
                         WHERE d.address_id = a.id AND d.asset_contract = $2
                           AND d.block_number <= $3), 0)::text AS deposits,
               COALESCE((SELECT SUM(f.amount_atomic) FROM flushed f
                         JOIN flushes x ON x.id = f.flush_id
                         WHERE f.address_id = a.id AND x.token = $2
                           AND f.block_number <= $3), 0)::text AS flushed
        FROM addresses a
        WHERE a.chain_id = $1
        ORDER BY a.id
        "#,
    )
    .bind(db_i64(chain_id)?)
    .bind(format!("{token:#x}"))
    .bind(db_i64(finalized)?)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|row| {
            let deposits: String = row.try_get("deposits")?;
            let flushed: String = row.try_get("flushed")?;
            Ok((
                row.try_get("id")?,
                parse_u256(&deposits)?,
                parse_u256(&flushed)?,
            ))
        })
        .collect()
}

/// Returns addresses in planned or sent flushes whose on-chain effect is not yet recorded.
pub(crate) async fn in_flight_flush_addresses(
    pool: &PgPool,
    chain_id: u64,
    token: Address,
) -> Result<BTreeSet<Uuid>, ReconciliationError> {
    let ids = sqlx::query_scalar::<_, String>(
        r#"
        SELECT DISTINCT planned.item->>'address_id'
        FROM flushes x
        CROSS JOIN LATERAL jsonb_array_elements(COALESCE(x.receipt->'plan', '[]'::jsonb))
            AS planned(item)
        WHERE x.chain_id = $1 AND x.token = $2 AND x.status IN ('planned', 'sent')
        "#,
    )
    .bind(db_i64(chain_id)?)
    .bind(format!("{token:#x}"))
    .fetch_all(pool)
    .await?;
    ids.iter()
        .map(|id| {
            Uuid::parse_str(id)
                .map_err(|_| ReconciliationError::Invariant("flush plan address id is invalid"))
        })
        .collect()
}

/// Confirmed flushes that can link at least one unlinked deposit, in chain order.
pub(crate) async fn linkable_flushes(pool: &PgPool) -> Result<Vec<Uuid>, ReconciliationError> {
    Ok(sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT f.flush_id
        FROM flushed f
        JOIN flushes x ON x.id = f.flush_id
        WHERE x.status = 'confirmed'
          AND EXISTS (
              SELECT 1 FROM deposits d
              WHERE d.address_id = f.address_id
                AND d.asset_contract = x.token
                AND d.flush_id IS NULL
                AND (d.block_number, d.log_index) < (f.block_number, f.log_index)
          )
        GROUP BY f.flush_id
        ORDER BY min(f.block_number), min(f.log_index), f.flush_id
        "#,
    )
    .fetch_all(pool)
    .await?)
}

/// Session advisory lock key which separates lease-holding processes from the post-restore gate.
const LEASE_OWNER_LOCK: i64 = 0x746f_7075_705f_6c73;

/// Hold on the lease-owner lock, released explicitly or when its connection closes.
///
/// Every process that leases deposits holds it shared for its lifetime; the post-restore gate
/// takes it exclusively, so it cannot preempt the leases of a running process.
pub struct LeaseOwnerLock {
    connection: PgConnection,
    exclusive: bool,
}

impl LeaseOwnerLock {
    /// Pings the lock connection every `every` until `shutdown` is cancelled.
    ///
    /// The lock lives only as long as its connection. When a ping fails the lock may already be
    /// gone, so this cancels `shutdown` to stop the processes it guards and returns the error.
    /// On cancellation it returns the still-held lock, so the caller can release it once those
    /// processes have stopped.
    pub async fn watch(
        mut self,
        every: Duration,
        shutdown: CancellationToken,
    ) -> Result<Self, ReconciliationError> {
        let mut ticks = interval(every);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return Ok(self),
                _ = ticks.tick() => {
                    if let Err(error) = self.connection.ping().await {
                        shutdown.cancel();
                        return Err(error.into());
                    }
                }
            }
        }
    }

    /// Releases the lock and closes its connection.
    pub async fn release(mut self) -> Result<(), ReconciliationError> {
        let unlock = if self.exclusive {
            "SELECT pg_advisory_unlock($1)"
        } else {
            "SELECT pg_advisory_unlock_shared($1)"
        };
        sqlx::query_scalar::<_, bool>(unlock)
            .bind(LEASE_OWNER_LOCK)
            .fetch_one(&mut self.connection)
            .await?;
        self.connection.close().await?;
        Ok(())
    }
}

/// Takes the lease-owner lock shared on a dedicated connection.
///
/// Fails while the post-restore gate is running.
pub async fn hold_lease_owner_lock(pool: &PgPool) -> Result<LeaseOwnerLock, ReconciliationError> {
    try_lease_owner_lock(pool, false)
        .await?
        .ok_or(ReconciliationError::LeaseOwnerLock(
            "post-restore reconciliation is running",
        ))
}

/// Takes the lease-owner lock exclusively on a dedicated connection.
///
/// Fails while any lease-holding process is connected.
pub(crate) async fn exclusive_lease_owner_lock(
    pool: &PgPool,
) -> Result<LeaseOwnerLock, ReconciliationError> {
    try_lease_owner_lock(pool, true)
        .await?
        .ok_or(ReconciliationError::LeaseOwnerLock(
            "a process holding deposit leases is running; stop it before post-restore reconciliation",
        ))
}

async fn try_lease_owner_lock(
    pool: &PgPool,
    exclusive: bool,
) -> Result<Option<LeaseOwnerLock>, ReconciliationError> {
    let mut connection = pool.acquire().await?.detach();
    let lock = if exclusive {
        "SELECT pg_try_advisory_lock($1)"
    } else {
        "SELECT pg_try_advisory_lock_shared($1)"
    };
    let held: bool = sqlx::query_scalar(lock)
        .bind(LEASE_OWNER_LOCK)
        .fetch_one(&mut connection)
        .await?;
    Ok(held.then_some(LeaseOwnerLock {
        connection,
        exclusive,
    }))
}

/// Returns the next block of the missing-deposit scan, if one was recorded.
pub(crate) async fn deposit_cursor(
    pool: &PgPool,
    chain_id: u64,
) -> Result<Option<u64>, ReconciliationError> {
    let next: Option<i64> = sqlx::query_scalar(
        "SELECT next_block FROM reconciliation_deposit_cursors WHERE chain_id = $1",
    )
    .bind(db_i64(chain_id)?)
    .fetch_optional(pool)
    .await?;
    next.map(db_u64).transpose()
}

/// Advances the missing-deposit cursor from `from` to `next`; returns false if another pass did.
pub(crate) async fn advance_deposit_cursor(
    pool: &PgPool,
    chain_id: u64,
    from: Option<u64>,
    next: u64,
) -> Result<bool, ReconciliationError> {
    let chain_id = db_i64(chain_id)?;
    let next = db_i64(next)?;
    let rows = match from {
        None => sqlx::query(
            r#"
            INSERT INTO reconciliation_deposit_cursors (chain_id, next_block)
            VALUES ($1, $2)
            ON CONFLICT (chain_id) DO NOTHING
            "#,
        )
        .bind(chain_id)
        .bind(next)
        .execute(pool)
        .await?
        .rows_affected(),
        Some(from) => sqlx::query(
            r#"
            UPDATE reconciliation_deposit_cursors
            SET next_block = $3, updated_at = now()
            WHERE chain_id = $1 AND next_block = $2
            "#,
        )
        .bind(chain_id)
        .bind(db_i64(from)?)
        .bind(next)
        .execute(pool)
        .await?
        .rows_affected(),
    };
    Ok(rows == 1)
}

/// Accumulated treasury and `Flushed` totals through `next_block - 1`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CustodyCursor {
    pub(crate) next_block: u64,
    pub(crate) flushed_event_total: U256,
    pub(crate) treasury_inflow_total: U256,
}

pub(crate) async fn custody_cursor(
    pool: &PgPool,
    chain_id: u64,
    factory: Address,
    token: Address,
) -> Result<Option<CustodyCursor>, ReconciliationError> {
    let row = sqlx::query(
        r#"
        SELECT next_block, flushed_event_total::text AS flushed, treasury_inflow_total::text AS inflow
        FROM reconciliation_custody_cursors
        WHERE chain_id = $1 AND factory = $2 AND token = $3
        "#,
    )
    .bind(db_i64(chain_id)?)
    .bind(format!("{factory:#x}"))
    .bind(format!("{token:#x}"))
    .fetch_optional(pool)
    .await?;
    row.map(|row| {
        let flushed: String = row.try_get("flushed")?;
        let inflow: String = row.try_get("inflow")?;
        Ok(CustodyCursor {
            next_block: db_u64(row.try_get("next_block")?)?,
            flushed_event_total: parse_u256(&flushed)?,
            treasury_inflow_total: parse_u256(&inflow)?,
        })
    })
    .transpose()
}

/// Stores new custody totals if the cursor still has the value this pass started from.
pub(crate) async fn advance_custody_cursor(
    pool: &PgPool,
    chain_id: u64,
    factory: Address,
    token: Address,
    from: Option<CustodyCursor>,
    next: CustodyCursor,
) -> Result<bool, ReconciliationError> {
    let chain_id = db_i64(chain_id)?;
    let factory = format!("{factory:#x}");
    let token = format!("{token:#x}");
    let next_block = db_i64(next.next_block)?;
    let flushed = next.flushed_event_total.to_string();
    let inflow = next.treasury_inflow_total.to_string();
    let rows = match from {
        None => sqlx::query(
            r#"
            INSERT INTO reconciliation_custody_cursors
                (chain_id, factory, token, next_block, flushed_event_total, treasury_inflow_total)
            VALUES ($1, $2, $3, $4, $5::text::numeric, $6::text::numeric)
            ON CONFLICT (chain_id, factory, token) DO NOTHING
            "#,
        )
        .bind(chain_id)
        .bind(factory)
        .bind(token)
        .bind(next_block)
        .bind(flushed)
        .bind(inflow)
        .execute(pool)
        .await?
        .rows_affected(),
        Some(from) => sqlx::query(
            r#"
            UPDATE reconciliation_custody_cursors
            SET next_block = $4,
                flushed_event_total = $5::text::numeric,
                treasury_inflow_total = $6::text::numeric,
                updated_at = now()
            WHERE chain_id = $1 AND factory = $2 AND token = $3 AND next_block = $7
            "#,
        )
        .bind(chain_id)
        .bind(factory)
        .bind(token)
        .bind(next_block)
        .bind(flushed)
        .bind(inflow)
        .bind(db_i64(from.next_block)?)
        .execute(pool)
        .await?
        .rows_affected(),
    };
    Ok(rows == 1)
}

fn parse_u256(value: &str) -> Result<U256, ReconciliationError> {
    value
        .parse()
        .map_err(|_| ReconciliationError::Invariant("stored atomic total is invalid"))
}

fn db_i64(value: u64) -> Result<i64, ReconciliationError> {
    i64::try_from(value)
        .map_err(|_| ReconciliationError::Invariant("value exceeds PostgreSQL bigint"))
}

fn db_u64(value: i64) -> Result<u64, ReconciliationError> {
    u64::try_from(value).map_err(|_| ReconciliationError::Invariant("stored block is negative"))
}

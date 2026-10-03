//! Tenant-scoped treasury lookup and pagination.
use super::{CancellationReason, Kind, Status, Treasury, TreasuryError, parse_chain_address};
use crate::tenancy::Scope;
use alloy_primitives::Address;
use chrono::{DateTime, Duration, Utc};
use sqlx::{FromRow, PgConnection, PgPool};
use std::{collections::BTreeMap, str::FromStr};
use uuid::Uuid;

/// The scope's current treasury of every chain that has one.
pub async fn current(
    connection: &mut PgConnection,
    scope: Scope,
) -> Result<BTreeMap<u64, Address>, TreasuryError> {
    let rows = sqlx::query_as::<_, (i64, String)>(
        "SELECT chain_id, address FROM treasuries \
         WHERE account_id = $1 AND livemode = $2 AND applied_at IS NOT NULL AND replaced_at IS NULL",
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .fetch_all(connection)
    .await?;
    rows.into_iter()
        .map(|(chain_id, address)| {
            Ok((
                u64::try_from(chain_id).map_err(|_| TreasuryError::DatabaseInvariant)?,
                Address::from_str(&address).map_err(|_| TreasuryError::DatabaseInvariant)?,
            ))
        })
        .collect()
}

/// How far from a moment a treasury may have been in force for a restore's re-issue to derive an
/// address issued at that moment over it ([`in_force`]). A treasury's recorded application time
/// is when [`super::apply_due`] applied it under the scope's lock, or the event's `created` in whole
/// seconds for one applied again after a restore, and an issuer's time is read on another clock,
/// so they can differ by clock and rounding skew, seconds at most. Every
/// candidate is the merchant's own proven treasury, and a salt binds the address to its customer
/// and `qt_` id or version, so the window widens nothing else.
pub const IN_FORCE_TOLERANCE: Duration = Duration::minutes(5);

/// The scope's treasuries, by chain, that were in force at some time from `from` to `to`, in the
/// whole seconds the API renders times in: applied by the end of `to`'s second and not replaced
/// before `from`; without `from`, every one applied by then. What a forwarder issued in that time
/// pays: a deposit address's network or a quote's address is derived over one of them.
pub async fn in_force(
    connection: &mut PgConnection,
    scope: Scope,
    from: Option<DateTime<Utc>>,
    to: DateTime<Utc>,
) -> Result<Vec<(u64, Address)>, TreasuryError> {
    let rows = sqlx::query_as::<_, (i64, String)>(
        "SELECT chain_id, address FROM treasuries \
         WHERE account_id = $1 AND livemode = $2 AND applied_at IS NOT NULL \
           AND applied_at < $4 + interval '1 second' \
           AND ($3::timestamptz IS NULL OR replaced_at IS NULL OR replaced_at >= $3) \
         ORDER BY chain_id, applied_at",
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(from)
    .bind(to)
    .fetch_all(connection)
    .await?;
    rows.into_iter()
        .map(|(chain_id, address)| parse_chain_address(chain_id, &address))
        .collect()
}

/// The scope's pending change of its treasury of `chain_id`, if any: its id and address.
pub async fn pending_on(
    pool: &PgPool,
    scope: Scope,
    chain_id: u64,
) -> Result<Option<(Uuid, Address)>, TreasuryError> {
    let row: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, address FROM treasuries \
         WHERE account_id = $1 AND livemode = $2 AND chain_id = $3 \
           AND applied_at IS NULL AND canceled_at IS NULL",
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(i64::try_from(chain_id).map_err(|_| TreasuryError::DatabaseInvariant)?)
    .fetch_optional(pool)
    .await?;
    row.map(|(id, address)| {
        Address::from_str(&address)
            .map(|address| (id, address))
            .map_err(|_| TreasuryError::DatabaseInvariant)
    })
    .transpose()
}

/// The scope's current treasury of `chain_id`, if it has one.
pub async fn current_on(
    connection: &mut PgConnection,
    scope: Scope,
    chain_id: u64,
) -> Result<Option<Address>, TreasuryError> {
    Ok(current(connection, scope).await?.remove(&chain_id))
}

/// The columns of [`TreasuryRow`]; callers append the `WHERE` clause.
const SELECT: &str = r#"
    SELECT id, livemode, chain_id, address, kind, effective_at, created_at, applied_at,
           replaced_at, canceled_at, cancellation_reason, crediting_paused_by
    FROM treasuries
    WHERE account_id = $1 AND livemode = $2
"#;

/// Loads the scope's treasury `id`.
pub async fn get(pool: &PgPool, scope: Scope, id: Uuid) -> Result<Option<Treasury>, TreasuryError> {
    let mut connection = pool.acquire().await?;
    get_in(&mut connection, scope, id).await
}

/// The scope's treasury `id`, read on `connection`.
pub(crate) async fn get_in(
    connection: &mut PgConnection,
    scope: Scope,
    id: Uuid,
) -> Result<Option<Treasury>, TreasuryError> {
    sqlx::query_as::<_, TreasuryRow>(sqlx::AssertSqlSafe(format!("{SELECT} AND id = $3")))
        .bind(scope.account_id())
        .bind(scope.livemode())
        .bind(id)
        .fetch_optional(connection)
        .await?
        .map(TreasuryRow::into_treasury)
        .transpose()
}

/// Filters of [`list`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ListFilter {
    /// Only this chain's treasuries.
    pub chain_id: Option<u64>,
    /// Only treasuries in this status.
    pub status: Option<Status>,
}

/// A page of the scope's treasuries, newest first, at most `limit`, and whether more follow in its
/// direction (Stripe's cursor pagination): `cursor` is the treasury the page starts after (or, with
/// `before`, ends before); `NotFound` for a cursor outside the scope.
pub async fn list(
    pool: &PgPool,
    scope: Scope,
    filter: ListFilter,
    limit: i64,
    cursor: Option<(Uuid, bool)>,
) -> Result<(Vec<Treasury>, bool), TreasuryError> {
    let before = cursor.is_some_and(|(_, before)| before);
    let cursor = match cursor {
        Some((id, _)) => Some(
            sqlx::query_as::<_, (DateTime<Utc>, Uuid)>(
                "SELECT created_at, id FROM treasuries \
                 WHERE id = $1 AND account_id = $2 AND livemode = $3",
            )
            .bind(id)
            .bind(scope.account_id())
            .bind(scope.livemode())
            .fetch_optional(pool)
            .await?
            .ok_or(TreasuryError::NotFound)?,
        ),
        None => None,
    };
    let mut query = SELECT.to_owned();
    if let Some(status) = filter.status {
        query.push_str(" AND ");
        query.push_str(status.condition());
    }
    query.push_str(" AND ($3::bigint IS NULL OR chain_id = $3)");
    query.push_str(if before {
        " AND ($5::timestamptz IS NULL OR (created_at, id) > ($5, $6)) \
         ORDER BY created_at ASC, id ASC LIMIT $4"
    } else {
        " AND ($5::timestamptz IS NULL OR (created_at, id) < ($5, $6)) \
         ORDER BY created_at DESC, id DESC LIMIT $4"
    });
    let chain_id = filter
        .chain_id
        .map(i64::try_from)
        .transpose()
        .map_err(|_| TreasuryError::DatabaseInvariant)?;
    let mut rows = sqlx::query_as::<_, TreasuryRow>(sqlx::AssertSqlSafe(query))
        .bind(scope.account_id())
        .bind(scope.livemode())
        .bind(chain_id)
        .bind(limit.saturating_add(1))
        .bind(cursor.map(|(created_at, _)| created_at))
        .bind(cursor.map(|(_, id)| id))
        .fetch_all(pool)
        .await?;
    let limit = usize::try_from(limit).map_err(|_| TreasuryError::DatabaseInvariant)?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    if before {
        rows.reverse();
    }
    let treasuries = rows
        .into_iter()
        .map(TreasuryRow::into_treasury)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((treasuries, has_more))
}

#[derive(FromRow)]
struct TreasuryRow {
    id: Uuid,
    livemode: bool,
    chain_id: i64,
    address: String,
    kind: String,
    effective_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    applied_at: Option<DateTime<Utc>>,
    replaced_at: Option<DateTime<Utc>>,
    canceled_at: Option<DateTime<Utc>>,
    cancellation_reason: Option<String>,
    crediting_paused_by: Vec<String>,
}

impl TreasuryRow {
    fn into_treasury(self) -> Result<Treasury, TreasuryError> {
        let status = match (self.canceled_at, self.replaced_at, self.applied_at) {
            (Some(_), _, _) => Status::Canceled,
            (None, Some(_), _) => Status::Replaced,
            (None, None, Some(_)) => Status::Active,
            (None, None, None) => Status::Pending,
        };
        Ok(Treasury {
            id: self.id,
            livemode: self.livemode,
            chain_id: u64::try_from(self.chain_id).map_err(|_| TreasuryError::DatabaseInvariant)?,
            address: Address::from_str(&self.address)
                .map_err(|_| TreasuryError::DatabaseInvariant)?,
            kind: Kind::parse(&self.kind)?,
            status,
            effective_at: self.effective_at,
            created_at: self.created_at,
            replaced_at: self.replaced_at,
            canceled_at: self.canceled_at,
            cancellation_reason: self
                .cancellation_reason
                .as_deref()
                .map(CancellationReason::parse)
                .transpose()?,
            crediting_paused_by: self.crediting_paused_by,
        })
    }
}

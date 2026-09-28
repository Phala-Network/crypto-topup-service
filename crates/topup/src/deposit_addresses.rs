//! Deposit addresses (docs/design/multi-tenant.md "Deposit addresses"): a customer's persistent,
//! rotatable forwarder address per chain and asset, restored per the owner's 2026-09-21
//! requirement.
//!
//! A deposit address is the same forwarder a quote gets, `CREATE2` over the treasury and a salt,
//! but its salt is derived from the customer instead of a quote
//! ([`topup_core::address::deposit_address_salt`]), so the merchant recomputes it offline. It
//! carries no price: any transfer to it, active or retired, is credited at spot through the quote
//! pipeline (fast credit, reversal, events, sweeps, refunds, reconciliation), which reads the
//! forwarder's `addresses` row and finds no quote.
//!
//! Creation is idempotent per customer, chain, asset, and mode: it returns the active address.
//! Rotation retires it and issues the next version. Retired addresses keep being scanned and
//! credited, and keep paying the treasury they were issued for.

use std::collections::BTreeMap;
use std::str::FromStr;

use alloy_primitives::{Address as EvmAddress, B256};
use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder, Transaction};
use topup_core::address::{deposit_address_salt, forwarder_address};
use topup_core::route::RouteFile;
use uuid::Uuid;

use crate::api::error::ApiError;
use crate::api::metadata::MetadataUpdate;
use crate::audit::{self, Actor};
use crate::db::{Account, Customer};
use crate::tenancy::Scope;

/// Default cap on a live-mode account's active deposit addresses (design §12); the operator
/// raises it per account in `account_limits.max_active_deposit_addresses`.
pub const DEFAULT_MAX_ACTIVE_LIVE: i64 = 100_000;
/// Default cap on a test-mode account's active deposit addresses.
pub const DEFAULT_MAX_ACTIVE_TEST: i64 = 1_000;
/// Rotations one customer may make in a rolling hour, across chains and assets.
pub const MAX_ROTATIONS_PER_HOUR: i64 = 10;

/// The columns of [`DepositAddressRow`]; callers append the `WHERE` clause.
const SELECT: &str = r#"
    SELECT deposit_address.id, deposit_address.livemode, customer.client_reference_id,
           deposit_address.chain_id, deposit_address.asset, deposit_address.route,
           deposit_address.version, deposit_address.status, deposit_address.created_at,
           deposit_address.retired_at, address.id AS address_id, address.address,
           address.treasury, address.salt, deposit_address.metadata
    FROM deposit_addresses AS deposit_address
    JOIN addresses AS address ON address.deposit_address_id = deposit_address.id
    JOIN customers AS customer ON customer.id = deposit_address.customer_id
"#;

/// Lifecycle of a deposit address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    /// The address creation returns for the customer, chain, and asset.
    Active,
    /// Replaced by a newer version; payments to it are still credited.
    Retired,
}

impl Status {
    /// The stable API and database code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Retired => "retired",
        }
    }

    fn parse(value: &str) -> Result<Self, DepositAddressError> {
        match value {
            "active" => Ok(Self::Active),
            "retired" => Ok(Self::Retired),
            _ => Err(DepositAddressError::DatabaseInvariant),
        }
    }
}

/// A customer's deposit address.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DepositAddress {
    /// Deposit address identifier.
    pub id: Uuid,
    /// The forwarder's `addresses` row.
    pub address_id: Uuid,
    /// Mode.
    pub livemode: bool,
    /// The customer's `client_reference_id`.
    pub client_reference_id: String,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Route asset code.
    pub asset: String,
    /// Route the address was issued on.
    pub route: String,
    /// Version among the customer's addresses for the chain and asset, from 1.
    pub version: u64,
    /// Forwarder address.
    pub address: EvmAddress,
    /// The treasury the forwarder pays, its clone argument.
    pub treasury: EvmAddress,
    /// CREATE2 salt.
    pub salt: B256,
    /// Lifecycle status.
    pub status: Status,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Retirement time.
    pub retired_at: Option<DateTime<Utc>>,
    /// The merchant's metadata (`crate::api` validates it).
    pub metadata: BTreeMap<String, String>,
}

/// Deposit address failure mapped by the API boundary.
#[derive(Debug, thiserror::Error)]
pub enum DepositAddressError {
    /// The address does not exist in the scope.
    #[error("deposit address not found")]
    NotFound,
    /// The address was already rotated; rotate the customer's active address instead.
    #[error("deposit address is retired")]
    Retired,
    /// The account's cap on active deposit addresses in this mode is reached.
    #[error("the account has {0} active deposit addresses, its cap")]
    CapReached(i64),
    /// The customer rotated too often in the last hour.
    #[error("deposit address rotation limit exceeded")]
    RateLimited,
    /// Request input is invalid.
    #[error("{0}")]
    InvalidInput(&'static str),
    /// The merged metadata breaks a limit.
    #[error("invalid metadata")]
    Metadata(ApiError),
    /// Persisted data violated an internal invariant.
    #[error("deposit address database invariant failed")]
    DatabaseInvariant,
    /// PostgreSQL rejected or failed the operation.
    #[error("{0}")]
    Database(#[from] sqlx::Error),
}

/// The treasury a new address of `route` pays.
///
/// TODO(design PR 7): take the account's effective treasury for the chain instead of the route's.
/// When a treasury change takes effect, PR 7 must rotate every active deposit address of the
/// account on that chain ([`rotate`] with `Actor::system`), in the same transaction that applies
/// the change, and announce it with `account.treasury.updated`. [`create`] already replaces an
/// active address whose treasury is no longer the effective one, so a missed rotation is repaired
/// on the next creation; retired addresses keep paying their old treasury.
fn effective_treasury(route: &RouteFile) -> EvmAddress {
    route.chain.contracts.treasury
}

/// Returns `customer`'s active address for `route`'s chain and asset, issuing one when there is
/// none, or when the active one pays a treasury that is no longer the effective one (it is
/// retired as by a rotation, which this creation is not refused for).
///
/// `metadata`, the request's, is merged into the returned address's, as an update would: a new
/// address starts from the one it replaces, or from none.
///
/// Returns the address and whether it was issued by this call.
pub async fn create(
    pool: &PgPool,
    account: &Account,
    customer: &Customer,
    route: &RouteFile,
    metadata: Option<&MetadataUpdate>,
) -> Result<(DepositAddress, bool), DepositAddressError> {
    if customer.account_id != account.id {
        return Err(DepositAddressError::NotFound);
    }
    if route.livemode != customer.livemode {
        return Err(DepositAddressError::InvalidInput(
            "the route's mode differs from the customer's",
        ));
    }
    let scope = Scope::new(account.id, customer.livemode);
    let treasury = effective_treasury(route);
    let mut transaction = pool.begin().await?;
    lock_customer(&mut transaction, customer).await?;
    let active = find_active(
        &mut transaction,
        customer.id,
        route.chain.chain_id,
        &route.asset.symbol,
    )
    .await?;
    let merge = |current: BTreeMap<String, String>| match metadata {
        Some(update) => update.apply(current).map_err(DepositAddressError::Metadata),
        None => Ok(current),
    };
    let issued = match active {
        Some(mut active) if active.treasury == treasury => {
            let merged = merge(active.metadata.clone())?;
            if merged != active.metadata {
                sqlx::query("UPDATE deposit_addresses SET metadata = $2 WHERE id = $1")
                    .bind(active.id)
                    .bind(Json(&merged))
                    .execute(&mut *transaction)
                    .await?;
                active.metadata = merged;
            }
            transaction.commit().await?;
            return Ok((active, false));
        }
        Some(active) => {
            // The treasury changed: the stale address is retired, whatever the rotation limit.
            let merged = merge(active.metadata)?;
            retire(&mut transaction, active.id).await?;
            let new = NewAddress {
                account_public_id: &account.public_id,
                customer,
                route,
                treasury,
                metadata: &merged,
            };
            issue(&mut transaction, scope, &new).await?
        }
        None => {
            let merged = merge(BTreeMap::new())?;
            check_cap(&mut transaction, scope).await?;
            let new = NewAddress {
                account_public_id: &account.public_id,
                customer,
                route,
                treasury,
                metadata: &merged,
            };
            issue(&mut transaction, scope, &new).await?
        }
    };
    transaction.commit().await?;
    Ok((issued, true))
}

/// Retires the scope's active address `id` and issues the next version for the same customer,
/// chain, and asset on `route`, the current version of the address's route. The new version
/// carries the retired one's metadata.
pub async fn rotate(
    pool: &PgPool,
    account: &Account,
    scope: Scope,
    actor: &Actor,
    id: Uuid,
    route: &RouteFile,
) -> Result<DepositAddress, DepositAddressError> {
    let mut transaction = pool.begin().await?;
    let customer = sqlx::query_as::<_, (Uuid, String, Vec<String>)>(
        r#"
        SELECT customer.id, customer.client_reference_id, customer.paused_scopes
        FROM deposit_addresses AS deposit_address
        JOIN customers AS customer ON customer.id = deposit_address.customer_id
        WHERE deposit_address.id = $1 AND deposit_address.account_id = $2
          AND deposit_address.livemode = $3
        "#,
    )
    .bind(id)
    .bind(scope.account_id())
    .bind(scope.livemode())
    .fetch_optional(&mut *transaction)
    .await?
    .map(|(id, client_reference_id, paused_scopes)| Customer {
        id,
        account_id: scope.account_id(),
        livemode: scope.livemode(),
        client_reference_id,
        paused_scopes,
    })
    .ok_or(DepositAddressError::NotFound)?;
    // Creations and rotations of one customer are serialized by its row lock.
    lock_customer(&mut transaction, &customer).await?;
    let current = get_in(&mut transaction, scope, id)
        .await?
        .ok_or(DepositAddressError::NotFound)?;
    if current.status == Status::Retired {
        return Err(DepositAddressError::Retired);
    }
    if current.chain_id != route.chain.chain_id || current.asset != route.asset.symbol {
        return Err(DepositAddressError::DatabaseInvariant);
    }
    let recent: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM deposit_addresses
        WHERE customer_id = $1 AND status = 'retired' AND retired_at >= now() - interval '1 hour'
        "#,
    )
    .bind(customer.id)
    .fetch_one(&mut *transaction)
    .await?;
    if recent >= MAX_ROTATIONS_PER_HOUR {
        return Err(DepositAddressError::RateLimited);
    }
    retire(&mut transaction, current.id).await?;
    let new = NewAddress {
        account_public_id: &account.public_id,
        customer: &customer,
        route,
        treasury: effective_treasury(route),
        metadata: &current.metadata,
    };
    let issued = issue(&mut transaction, scope, &new).await?;
    audit::insert(
        &mut *transaction,
        &audit::Entry {
            account_id: Some(scope.account_id()),
            actor,
            action: "deposit_address.rotate",
            subject: &format!("deposit_address:{}", public_id(current.id)),
            reason: &format!("API request; replaced by {}", public_id(issued.id)),
        },
    )
    .await?;
    transaction.commit().await?;
    Ok(issued)
}

/// The public id of a deposit address, `da_` and the hex of its id.
#[must_use]
pub fn public_id(id: Uuid) -> String {
    crate::ids::format(crate::ids::DEPOSIT_ADDRESS, id)
}

/// Loads the scope's deposit address `id`.
pub async fn get(
    pool: &PgPool,
    scope: Scope,
    id: Uuid,
) -> Result<Option<DepositAddress>, DepositAddressError> {
    let mut builder = QueryBuilder::new(SELECT);
    push_scope(&mut builder, scope);
    builder.push(" AND deposit_address.id = ").push_bind(id);
    builder
        .build_query_as::<DepositAddressRow>()
        .fetch_optional(pool)
        .await?
        .map(TryInto::try_into)
        .transpose()
}

/// Filters and cursor of [`list`].
#[derive(Clone, Debug, Default)]
pub struct ListFilter {
    /// Only this customer's addresses.
    pub client_reference_id: Option<String>,
    /// Only addresses in this status.
    pub status: Option<Status>,
    /// Only this chain's addresses.
    pub chain_id: Option<u64>,
    /// The page after this address (older), or before it (newer) when `before`.
    pub cursor: Option<Uuid>,
    /// Whether `cursor` is `ending_before`.
    pub before: bool,
    /// Page size.
    pub limit: i64,
}

/// A page of the scope's deposit addresses, newest first, and whether more follow in the
/// direction of the page. A cursor outside the scope is `NotFound`.
pub async fn list(
    pool: &PgPool,
    scope: Scope,
    filter: &ListFilter,
) -> Result<(Vec<DepositAddress>, bool), DepositAddressError> {
    let mut builder = QueryBuilder::new(SELECT);
    push_scope(&mut builder, scope);
    if let Some(reference) = &filter.client_reference_id {
        builder
            .push(" AND customer.client_reference_id = ")
            .push_bind(reference.clone());
    }
    if let Some(status) = filter.status {
        builder
            .push(" AND deposit_address.status = ")
            .push_bind(status.code());
    }
    if let Some(chain_id) = filter.chain_id {
        let chain_id =
            i64::try_from(chain_id).map_err(|_| DepositAddressError::InvalidInput("chain_id"))?;
        builder
            .push(" AND deposit_address.chain_id = ")
            .push_bind(chain_id);
    }
    if let Some(cursor) = filter.cursor {
        let found: Option<(DateTime<Utc>, Uuid)> = sqlx::query_as(
            "SELECT created_at, id FROM deposit_addresses \
             WHERE id = $1 AND account_id = $2 AND livemode = $3",
        )
        .bind(cursor)
        .bind(scope.account_id())
        .bind(scope.livemode())
        .fetch_optional(pool)
        .await?;
        let (created_at, id) = found.ok_or(DepositAddressError::NotFound)?;
        builder
            .push(if filter.before {
                " AND (deposit_address.created_at, deposit_address.id) > ("
            } else {
                " AND (deposit_address.created_at, deposit_address.id) < ("
            })
            .push_bind(created_at)
            .push(", ")
            .push_bind(id)
            .push(")");
    }
    builder.push(if filter.before {
        " ORDER BY deposit_address.created_at ASC, deposit_address.id ASC LIMIT "
    } else {
        " ORDER BY deposit_address.created_at DESC, deposit_address.id DESC LIMIT "
    });
    builder.push_bind(filter.limit.saturating_add(1));
    let mut rows = builder
        .build_query_as::<DepositAddressRow>()
        .fetch_all(pool)
        .await?;
    let limit =
        usize::try_from(filter.limit).map_err(|_| DepositAddressError::DatabaseInvariant)?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    if filter.before {
        rows.reverse();
    }
    let addresses = rows
        .into_iter()
        .map(TryInto::try_into)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((addresses, has_more))
}

fn push_scope(builder: &mut QueryBuilder<Postgres>, scope: Scope) {
    builder
        .push(" WHERE deposit_address.account_id = ")
        .push_bind(scope.account_id())
        .push(" AND deposit_address.livemode = ")
        .push_bind(scope.livemode());
}

async fn get_in(
    transaction: &mut Transaction<'_, Postgres>,
    scope: Scope,
    id: Uuid,
) -> Result<Option<DepositAddress>, DepositAddressError> {
    let mut builder = QueryBuilder::new(SELECT);
    push_scope(&mut builder, scope);
    builder
        .push(" AND deposit_address.id = ")
        .push_bind(id)
        .push(" FOR UPDATE OF deposit_address");
    builder
        .build_query_as::<DepositAddressRow>()
        .fetch_optional(&mut **transaction)
        .await?
        .map(TryInto::try_into)
        .transpose()
}

async fn find_active(
    transaction: &mut Transaction<'_, Postgres>,
    customer_id: Uuid,
    chain_id: u64,
    asset: &str,
) -> Result<Option<DepositAddress>, DepositAddressError> {
    let chain_id =
        i64::try_from(chain_id).map_err(|_| DepositAddressError::InvalidInput("chain_id"))?;
    let mut builder = QueryBuilder::new(SELECT);
    builder
        .push(" WHERE deposit_address.customer_id = ")
        .push_bind(customer_id)
        .push(" AND deposit_address.chain_id = ")
        .push_bind(chain_id)
        .push(" AND deposit_address.asset = ")
        .push_bind(asset.to_owned())
        .push(" AND deposit_address.status = 'active' FOR UPDATE OF deposit_address");
    builder
        .build_query_as::<DepositAddressRow>()
        .fetch_optional(&mut **transaction)
        .await?
        .map(TryInto::try_into)
        .transpose()
}

async fn retire(
    transaction: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<(), DepositAddressError> {
    let updated = sqlx::query(
        "UPDATE deposit_addresses SET status = 'retired', retired_at = now() \
         WHERE id = $1 AND status = 'active'",
    )
    .bind(id)
    .execute(&mut **transaction)
    .await?
    .rows_affected();
    if updated != 1 {
        return Err(DepositAddressError::DatabaseInvariant);
    }
    Ok(())
}

/// Refuses a new address that would take the account's active addresses in `scope` past its cap.
/// The advisory lock serializes issuance per account and mode from this check to commit.
async fn check_cap(
    transaction: &mut Transaction<'_, Postgres>,
    scope: Scope,
) -> Result<(), DepositAddressError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('deposit-address-cap:' || $1, 0))")
        .bind(format!("{}:{}", scope.account_id(), scope.livemode()))
        .execute(&mut **transaction)
        .await?;
    let (active, cap): (i64, Option<i32>) = sqlx::query_as(
        r#"
        SELECT (SELECT count(*) FROM deposit_addresses
                WHERE account_id = $1 AND livemode = $2 AND status = 'active'),
               (SELECT max_active_deposit_addresses FROM account_limits
                WHERE account_id = $1 AND livemode = $2)
        "#,
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .fetch_one(&mut **transaction)
    .await?;
    let cap = cap.map_or(
        if scope.livemode() {
            DEFAULT_MAX_ACTIVE_LIVE
        } else {
            DEFAULT_MAX_ACTIVE_TEST
        },
        i64::from,
    );
    if active >= cap {
        return Err(DepositAddressError::CapReached(cap));
    }
    Ok(())
}

/// What [`issue`] derives and stores a new address from.
struct NewAddress<'a> {
    account_public_id: &'a str,
    customer: &'a Customer,
    route: &'a RouteFile,
    treasury: EvmAddress,
    metadata: &'a BTreeMap<String, String>,
}

async fn issue(
    transaction: &mut Transaction<'_, Postgres>,
    scope: Scope,
    new: &NewAddress<'_>,
) -> Result<DepositAddress, DepositAddressError> {
    let NewAddress {
        account_public_id,
        customer,
        route,
        treasury,
        metadata,
    } = *new;
    let chain_id = route.chain.chain_id;
    let chain_id_db =
        i64::try_from(chain_id).map_err(|_| DepositAddressError::InvalidInput("chain_id"))?;
    let asset = &route.asset.symbol;
    let latest: Option<i64> = sqlx::query_scalar(
        "SELECT max(version) FROM deposit_addresses \
         WHERE customer_id = $1 AND chain_id = $2 AND asset = $3",
    )
    .bind(customer.id)
    .bind(chain_id_db)
    .bind(asset)
    .fetch_one(&mut **transaction)
    .await?;
    let version = latest
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(DepositAddressError::DatabaseInvariant)?;
    let version_u64 = u64::try_from(version).map_err(|_| DepositAddressError::DatabaseInvariant)?;
    let salt = deposit_address_salt(
        account_public_id,
        scope.livemode(),
        &customer.client_reference_id,
        chain_id,
        asset,
        version_u64,
    );
    let address = forwarder_address(
        route.chain.contracts.forwarder_factory,
        route.chain.contracts.implementation,
        treasury,
        salt,
    );
    let id = Uuid::new_v4();
    let address_id = Uuid::new_v4();
    let created_at: DateTime<Utc> = sqlx::query_scalar(
        r#"
        INSERT INTO deposit_addresses
            (id, account_id, livemode, customer_id, chain_id, asset, route, version, metadata)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        RETURNING created_at
        "#,
    )
    .bind(id)
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(customer.id)
    .bind(chain_id_db)
    .bind(asset)
    .bind(&route.route)
    .bind(version)
    .bind(Json(metadata))
    .fetch_one(&mut **transaction)
    .await?;
    // A newly derived address cannot hold earlier payments to this salt and treasury unless
    // someone precomputed it, and such a payment would only ever reach the treasury; as for quote
    // addresses, the scanner covers it from the chain's committed cursor.
    sqlx::query(
        r#"
        INSERT INTO addresses (
            id, account_id, livemode, chain_id, deposit_address_id, salt, treasury, address,
            created_block
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8,
            COALESCE((SELECT scanned_block FROM cursors WHERE chain_id = $4), 0)
        )
        "#,
    )
    .bind(address_id)
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(chain_id_db)
    .bind(id)
    .bind(format!("{salt:#x}"))
    .bind(format!("{treasury:#x}"))
    .bind(format!("{address:#x}"))
    .execute(&mut **transaction)
    .await?;
    Ok(DepositAddress {
        id,
        address_id,
        livemode: scope.livemode(),
        client_reference_id: customer.client_reference_id.clone(),
        chain_id,
        asset: asset.clone(),
        route: route.route.clone(),
        version: version_u64,
        address,
        treasury,
        salt,
        status: Status::Active,
        created_at,
        retired_at: None,
        metadata: metadata.clone(),
    })
}

async fn lock_customer(
    transaction: &mut Transaction<'_, Postgres>,
    customer: &Customer,
) -> Result<(), DepositAddressError> {
    // `NO KEY UPDATE`, as quote creation takes: it serializes this customer's issuance without
    // blocking the `KEY SHARE` locks of foreign-key checks on scanner deposit inserts.
    let found = sqlx::query(
        "SELECT id FROM customers WHERE id = $1 AND account_id = $2 AND livemode = $3 \
         FOR NO KEY UPDATE",
    )
    .bind(customer.id)
    .bind(customer.account_id)
    .bind(customer.livemode)
    .fetch_optional(&mut **transaction)
    .await?;
    if found.is_none() {
        return Err(DepositAddressError::NotFound);
    }
    Ok(())
}

#[derive(FromRow)]
struct DepositAddressRow {
    id: Uuid,
    livemode: bool,
    client_reference_id: String,
    chain_id: i64,
    asset: String,
    route: String,
    version: i64,
    status: String,
    created_at: DateTime<Utc>,
    retired_at: Option<DateTime<Utc>>,
    address_id: Uuid,
    address: String,
    treasury: String,
    salt: String,
    metadata: Json<BTreeMap<String, String>>,
}

impl TryFrom<DepositAddressRow> for DepositAddress {
    type Error = DepositAddressError;

    fn try_from(row: DepositAddressRow) -> Result<Self, Self::Error> {
        let invariant = |_| DepositAddressError::DatabaseInvariant;
        Ok(Self {
            id: row.id,
            address_id: row.address_id,
            livemode: row.livemode,
            client_reference_id: row.client_reference_id,
            chain_id: u64::try_from(row.chain_id).map_err(invariant)?,
            asset: row.asset,
            route: row.route,
            version: u64::try_from(row.version).map_err(invariant)?,
            address: EvmAddress::from_str(&row.address)
                .map_err(|_| DepositAddressError::DatabaseInvariant)?,
            treasury: EvmAddress::from_str(&row.treasury)
                .map_err(|_| DepositAddressError::DatabaseInvariant)?,
            salt: B256::from_str(&row.salt).map_err(|_| DepositAddressError::DatabaseInvariant)?,
            status: Status::parse(&row.status)?,
            created_at: row.created_at,
            retired_at: row.retired_at,
            metadata: row.metadata.0,
        })
    }
}

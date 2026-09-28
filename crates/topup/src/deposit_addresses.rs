//! Deposit addresses (docs/design/multi-tenant.md "Deposit addresses"): a customer's persistent,
//! rotatable forwarder address, one for every chain and every supported asset (D16, amended
//! 2026-09-28 by the owner's decision: one address per customer, as exchanges give).
//!
//! A deposit address is the same forwarder a quote gets, `CREATE2` over the treasury and a salt,
//! but its salt is derived from the customer and a version and names no chain or asset
//! ([`topup_core::address::deposit_address_salt`]), so the merchant recomputes it offline. The
//! factory and implementation have one address on every chain, so the forwarder is the same
//! address on every chain whose treasury is the same address; a chain whose treasury differs has
//! its own address, which the address's network for that chain shows.
//!
//! Each chain's forwarder is an `addresses` row owned by the deposit address (a network). It
//! carries no price: a transfer of any supported token of the chain to it, active or retired, is
//! credited at spot through the quote pipeline (fast credit, reversal, events, sweeps, refunds,
//! reconciliation), which reads the forwarder's row and finds no quote; an unsupported token is
//! rejected as for a quote.
//!
//! Creation is idempotent per customer and mode: it returns the active address, adding a network
//! for every issuable chain that has none. Rotation retires the address and issues the next
//! version on every issuable chain. Retired addresses, and networks superseded by a treasury
//! change, keep being scanned and credited, and keep paying the treasury they were issued for.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use alloy_primitives::{Address as EvmAddress, B256};
use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{FromRow, PgConnection, PgPool, Postgres, QueryBuilder, Transaction};
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
/// Rotations one customer may make in a rolling hour.
pub const MAX_ROTATIONS_PER_HOUR: i64 = 10;

/// The columns of [`DepositAddressRow`]; callers append the `WHERE` clause.
const SELECT: &str = r#"
    SELECT deposit_address.id, account.public_id AS account_public_id, deposit_address.livemode,
           customer.client_reference_id, deposit_address.version, deposit_address.status,
           deposit_address.created_at, deposit_address.retired_at, deposit_address.metadata
    FROM deposit_addresses AS deposit_address
    JOIN customers AS customer ON customer.id = deposit_address.customer_id
    JOIN accounts AS account ON account.id = deposit_address.account_id
"#;

/// Lifecycle of a deposit address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    /// The address creation returns for the customer.
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

/// A chain a deposit address gets a forwarder on: the `CREATE2` contracts and the treasury a new
/// forwarder there pays.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Chain {
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Forwarder factory, at the same address on every chain.
    pub factory: EvmAddress,
    /// Forwarder implementation, at the same address on every chain.
    pub implementation: EvmAddress,
    /// The treasury new forwarders on the chain pay.
    pub treasury: EvmAddress,
}

impl Chain {
    /// The chain of `route`, with the treasury a new forwarder there pays.
    ///
    /// TODO(design PR 7): take the account's effective treasury for the chain instead of the
    /// route's. When a treasury change takes effect on a chain, PR 7 must supersede that chain's
    /// network of every deposit address of the account ([`sync_networks`] over the new treasury,
    /// active and retired addresses alike, since both are still paid) in the same transaction
    /// that applies the change, and announce it with `account.treasury.updated`; the address on
    /// the other chains is unchanged. [`create`] already supersedes an active address's network
    /// whose treasury is no longer the effective one, so a missed update is repaired on the next
    /// creation; a superseded network keeps paying its old treasury and is still credited.
    #[must_use]
    pub fn of(route: &RouteFile) -> Self {
        Self {
            chain_id: route.chain.chain_id,
            factory: route.chain.contracts.forwarder_factory,
            implementation: route.chain.contracts.implementation,
            treasury: route.chain.contracts.treasury,
        }
    }
}

/// A deposit address's current forwarder on one chain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Network {
    /// The forwarder's `addresses` row.
    pub address_id: Uuid,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Forwarder address on the chain.
    pub address: EvmAddress,
    /// The treasury the forwarder pays, its clone argument.
    pub treasury: EvmAddress,
}

/// A customer's deposit address.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DepositAddress {
    /// Deposit address identifier.
    pub id: Uuid,
    /// Mode.
    pub livemode: bool,
    /// The customer's `client_reference_id`.
    pub client_reference_id: String,
    /// Version among the customer's addresses, from 1.
    pub version: u64,
    /// CREATE2 salt, the same on every chain.
    pub salt: B256,
    /// Lifecycle status.
    pub status: Status,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Retirement time.
    pub retired_at: Option<DateTime<Utc>>,
    /// The merchant's metadata (`crate::api` validates it).
    pub metadata: BTreeMap<String, String>,
    /// The current forwarder on each chain it was issued on, by chain id. Networks superseded by
    /// a treasury change are not listed; they are still credited.
    pub networks: Vec<Network>,
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
    /// No chain accepts new networks (every chain of the mode is frozen or paused), so no address
    /// can be issued.
    #[error("no chain accepts new deposit addresses")]
    NoChain,
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

/// Returns `customer`'s active address, issuing one on `chains` when there is none.
///
/// An existing address gets a network on each of `chains` it has none on, and a new network on
/// each chain whose treasury is no longer the one its network pays (the old network is
/// superseded, and still credited). `chains` are the issuable chains of the mode; an existing
/// address's networks on other chains are kept.
///
/// `metadata`, the request's, is merged into the returned address's, as an update would.
///
/// Returns the address and whether it was issued by this call.
pub async fn create(
    pool: &PgPool,
    account: &Account,
    customer: &Customer,
    chains: &[Chain],
    metadata: Option<&MetadataUpdate>,
) -> Result<(DepositAddress, bool), DepositAddressError> {
    if customer.account_id != account.id {
        return Err(DepositAddressError::NotFound);
    }
    let scope = Scope::new(account.id, customer.livemode);
    let mut transaction = pool.begin().await?;
    lock_customer(&mut transaction, customer).await?;
    let merge = |current: BTreeMap<String, String>| match metadata {
        Some(update) => update.apply(current).map_err(DepositAddressError::Metadata),
        None => Ok(current),
    };
    let active = sqlx::query_as::<_, (Uuid, i64, Json<BTreeMap<String, String>>)>(
        "SELECT id, version, metadata FROM deposit_addresses \
         WHERE customer_id = $1 AND status = 'active' FOR UPDATE",
    )
    .bind(customer.id)
    .fetch_optional(&mut *transaction)
    .await?;
    let (id, issued) = match active {
        Some((id, version, Json(current))) => {
            let merged = merge(current.clone())?;
            if merged != current {
                sqlx::query("UPDATE deposit_addresses SET metadata = $2 WHERE id = $1")
                    .bind(id)
                    .bind(Json(&merged))
                    .execute(&mut *transaction)
                    .await?;
            }
            let version =
                u64::try_from(version).map_err(|_| DepositAddressError::DatabaseInvariant)?;
            let salt = deposit_address_salt(
                &account.public_id,
                customer.livemode,
                &customer.client_reference_id,
                version,
            );
            sync_networks(&mut transaction, scope, id, salt, chains).await?;
            (id, false)
        }
        None => {
            let merged = merge(BTreeMap::new())?;
            check_cap(&mut transaction, scope).await?;
            let id = issue(
                &mut transaction,
                scope,
                &account.public_id,
                customer,
                chains,
                &merged,
            )
            .await?;
            (id, true)
        }
    };
    let address = get_in(&mut transaction, scope, id)
        .await?
        .ok_or(DepositAddressError::DatabaseInvariant)?;
    transaction.commit().await?;
    Ok((address, issued))
}

/// Retires the scope's active address `id` and issues the next version for the same customer on
/// `chains`, the issuable chains of the mode. The new version carries the retired one's metadata.
pub async fn rotate(
    pool: &PgPool,
    account: &Account,
    scope: Scope,
    actor: &Actor,
    id: Uuid,
    chains: &[Chain],
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
    let current = sqlx::query_as::<_, (String, Json<BTreeMap<String, String>>)>(
        "SELECT status, metadata FROM deposit_addresses WHERE id = $1 FOR UPDATE",
    )
    .bind(id)
    .fetch_one(&mut *transaction)
    .await?;
    let (status, Json(metadata)) = current;
    if Status::parse(&status)? == Status::Retired {
        return Err(DepositAddressError::Retired);
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
    retire(&mut transaction, id).await?;
    let issued = issue(
        &mut transaction,
        scope,
        &account.public_id,
        &customer,
        chains,
        &metadata,
    )
    .await?;
    audit::insert(
        &mut *transaction,
        &audit::Entry {
            account_id: Some(scope.account_id()),
            actor,
            action: "deposit_address.rotate",
            subject: &format!("deposit_address:{}", public_id(id)),
            reason: &format!("API request; replaced by {}", public_id(issued)),
        },
    )
    .await?;
    let address = get_in(&mut transaction, scope, issued)
        .await?
        .ok_or(DepositAddressError::DatabaseInvariant)?;
    transaction.commit().await?;
    Ok(address)
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
    let mut connection = pool.acquire().await?;
    get_in(&mut connection, scope, id).await
}

/// Filters and cursor of [`list`].
#[derive(Clone, Debug, Default)]
pub struct ListFilter {
    /// Only this customer's addresses.
    pub client_reference_id: Option<String>,
    /// Only addresses in this status.
    pub status: Option<Status>,
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
    let mut connection = pool.acquire().await?;
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
    if let Some(cursor) = filter.cursor {
        let found: Option<(DateTime<Utc>, Uuid)> = sqlx::query_as(
            "SELECT created_at, id FROM deposit_addresses \
             WHERE id = $1 AND account_id = $2 AND livemode = $3",
        )
        .bind(cursor)
        .bind(scope.account_id())
        .bind(scope.livemode())
        .fetch_optional(&mut *connection)
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
        .fetch_all(&mut *connection)
        .await?;
    let limit =
        usize::try_from(filter.limit).map_err(|_| DepositAddressError::DatabaseInvariant)?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    if filter.before {
        rows.reverse();
    }
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let mut networks = load_networks(&mut connection, &ids).await?;
    let addresses = rows
        .into_iter()
        .map(|row| {
            let own = networks.remove(&row.id).unwrap_or_default();
            row.into_address(own)
        })
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
    connection: &mut PgConnection,
    scope: Scope,
    id: Uuid,
) -> Result<Option<DepositAddress>, DepositAddressError> {
    let mut builder = QueryBuilder::new(SELECT);
    push_scope(&mut builder, scope);
    builder.push(" AND deposit_address.id = ").push_bind(id);
    let Some(row) = builder
        .build_query_as::<DepositAddressRow>()
        .fetch_optional(&mut *connection)
        .await?
    else {
        return Ok(None);
    };
    let networks = load_networks(connection, &[row.id])
        .await?
        .remove(&row.id)
        .unwrap_or_default();
    row.into_address(networks).map(Some)
}

/// The current networks of `ids`, by deposit address and then chain id.
async fn load_networks(
    connection: &mut PgConnection,
    ids: &[Uuid],
) -> Result<BTreeMap<Uuid, Vec<Network>>, DepositAddressError> {
    let rows = sqlx::query_as::<_, (Uuid, Uuid, i64, String, String)>(
        r#"
        SELECT deposit_address_id, id, chain_id, address, treasury
        FROM addresses
        WHERE deposit_address_id = ANY($1) AND superseded_at IS NULL
        ORDER BY deposit_address_id, chain_id
        "#,
    )
    .bind(ids)
    .fetch_all(connection)
    .await?;
    let mut networks = BTreeMap::<Uuid, Vec<Network>>::new();
    for (deposit_address_id, address_id, chain_id, address, treasury) in rows {
        networks
            .entry(deposit_address_id)
            .or_default()
            .push(Network {
                address_id,
                chain_id: u64::try_from(chain_id)
                    .map_err(|_| DepositAddressError::DatabaseInvariant)?,
                address: EvmAddress::from_str(&address)
                    .map_err(|_| DepositAddressError::DatabaseInvariant)?,
                treasury: EvmAddress::from_str(&treasury)
                    .map_err(|_| DepositAddressError::DatabaseInvariant)?,
            });
    }
    Ok(networks)
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

/// Inserts the customer's next version with a network on each of `chains`, and returns its id.
async fn issue(
    transaction: &mut Transaction<'_, Postgres>,
    scope: Scope,
    account_public_id: &str,
    customer: &Customer,
    chains: &[Chain],
    metadata: &BTreeMap<String, String>,
) -> Result<Uuid, DepositAddressError> {
    if chains.is_empty() {
        return Err(DepositAddressError::NoChain);
    }
    let latest: Option<i64> =
        sqlx::query_scalar("SELECT max(version) FROM deposit_addresses WHERE customer_id = $1")
            .bind(customer.id)
            .fetch_one(&mut **transaction)
            .await?;
    let version = latest
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(DepositAddressError::DatabaseInvariant)?;
    let version_u64 = u64::try_from(version).map_err(|_| DepositAddressError::DatabaseInvariant)?;
    let id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO deposit_addresses (id, account_id, livemode, customer_id, version, metadata)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
    )
    .bind(id)
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(customer.id)
    .bind(version)
    .bind(Json(metadata))
    .execute(&mut **transaction)
    .await?;
    let salt = deposit_address_salt(
        account_public_id,
        scope.livemode(),
        &customer.client_reference_id,
        version_u64,
    );
    sync_networks(transaction, scope, id, salt, chains).await?;
    Ok(id)
}

/// Gives deposit address `id` (of `salt`) a current network on each of `chains` over the chain's
/// treasury: a chain without one gets one, and a chain whose network pays another treasury has it
/// superseded (still watched and credited) by one over the new treasury. A network superseded
/// earlier is made current again when its treasury is the chain's again. Networks on chains not
/// in `chains` are left as they are.
pub async fn sync_networks(
    transaction: &mut Transaction<'_, Postgres>,
    scope: Scope,
    id: Uuid,
    salt: B256,
    chains: &[Chain],
) -> Result<(), DepositAddressError> {
    let mut seen = BTreeSet::new();
    for chain in chains {
        if !seen.insert(chain.chain_id) {
            return Err(DepositAddressError::DatabaseInvariant);
        }
        let chain_id = i64::try_from(chain.chain_id)
            .map_err(|_| DepositAddressError::InvalidInput("chain_id"))?;
        let address = forwarder_address(chain.factory, chain.implementation, chain.treasury, salt);
        let address_hex = format!("{address:#x}");
        let current: Option<String> = sqlx::query_scalar(
            "SELECT address FROM addresses \
             WHERE deposit_address_id = $1 AND chain_id = $2 AND superseded_at IS NULL \
             FOR UPDATE",
        )
        .bind(id)
        .bind(chain_id)
        .fetch_optional(&mut **transaction)
        .await?;
        match current {
            Some(current) if current == address_hex => continue,
            Some(_) => {
                sqlx::query(
                    "UPDATE addresses SET superseded_at = now() \
                     WHERE deposit_address_id = $1 AND chain_id = $2 AND superseded_at IS NULL",
                )
                .bind(id)
                .bind(chain_id)
                .execute(&mut **transaction)
                .await?;
            }
            None => {}
        }
        // The chain's treasury may be one an earlier network of this address paid: that forwarder
        // is the chain's current one again.
        let restored = sqlx::query(
            "UPDATE addresses SET superseded_at = NULL \
             WHERE deposit_address_id = $1 AND chain_id = $2 AND address = $3",
        )
        .bind(id)
        .bind(chain_id)
        .bind(&address_hex)
        .execute(&mut **transaction)
        .await?
        .rows_affected();
        if restored > 0 {
            continue;
        }
        // A newly derived forwarder cannot hold earlier payments to this salt and treasury unless
        // someone sent to the address before its network existed (on a chain added later, or
        // before a treasury change); as for quote addresses, the scanner covers it from the
        // chain's committed cursor.
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
        .bind(Uuid::new_v4())
        .bind(scope.account_id())
        .bind(scope.livemode())
        .bind(chain_id)
        .bind(id)
        .bind(format!("{salt:#x}"))
        .bind(format!("{:#x}", chain.treasury))
        .bind(&address_hex)
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
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
    account_public_id: String,
    livemode: bool,
    client_reference_id: String,
    version: i64,
    status: String,
    created_at: DateTime<Utc>,
    retired_at: Option<DateTime<Utc>>,
    metadata: Json<BTreeMap<String, String>>,
}

impl DepositAddressRow {
    fn into_address(self, networks: Vec<Network>) -> Result<DepositAddress, DepositAddressError> {
        let version =
            u64::try_from(self.version).map_err(|_| DepositAddressError::DatabaseInvariant)?;
        Ok(DepositAddress {
            id: self.id,
            salt: deposit_address_salt(
                &self.account_public_id,
                self.livemode,
                &self.client_reference_id,
                version,
            ),
            livemode: self.livemode,
            client_reference_id: self.client_reference_id,
            version,
            status: Status::parse(&self.status)?,
            created_at: self.created_at,
            retired_at: self.retired_at,
            metadata: self.metadata.0,
            networks,
        })
    }
}

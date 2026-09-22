//! API-specific PostgreSQL queries with tenant predicates at the database boundary.

use std::collections::BTreeSet;
use std::str::FromStr;

use alloy_primitives::{Address as EvmAddress, B256};
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use topup_core::address::{forwarder_address, persistent_salt};
use uuid::Uuid;

use crate::db::{Account, Address, AddressKind, Product};

use super::error::ApiError;
use super::models::{DepositListQuery, DepositLookupQuery, DepositResponse, DepositsResponse};

/// Finds the unique product selected by an external API slug.
pub async fn find_product_by_slug(pool: &PgPool, slug: &str) -> Result<Option<Product>, ApiError> {
    let row = sqlx::query_as::<_, ProductRow>(
        r#"
        SELECT id, slug, settlement_url, webhook_url, pubkey, kid, paused_scopes
        FROM products
        WHERE slug = $1
        "#,
    )
    .bind(slug)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

/// Registers an account or returns the existing account with the same product identifier.
pub async fn register_account(
    pool: &PgPool,
    product_id: Uuid,
    external_id: &str,
) -> Result<Account, ApiError> {
    let mut transaction = pool.begin().await?;
    sqlx::query(
        r#"
        INSERT INTO accounts (id, product_id, external_id, paused_scopes)
        VALUES ($1, $2, $3, '{}')
        ON CONFLICT (product_id, external_id) DO NOTHING
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(product_id)
    .bind(external_id)
    .execute(&mut *transaction)
    .await?;
    let account = sqlx::query_as::<_, AccountRow>(
        r#"
        SELECT id, product_id, external_id, paused_scopes
        FROM accounts
        WHERE product_id = $1 AND external_id = $2
        "#,
    )
    .bind(product_id)
    .bind(external_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(ApiError::internal)?;
    transaction.commit().await?;
    Ok(account.into())
}

/// Finds an account only when it belongs to the requested product.
pub async fn find_account(
    pool: &PgPool,
    product_id: Uuid,
    external_id: &str,
) -> Result<Option<Account>, ApiError> {
    let row = sqlx::query_as::<_, AccountRow>(
        r#"
        SELECT id, product_id, external_id, paused_scopes
        FROM accounts
        WHERE product_id = $1 AND external_id = $2
        "#,
    )
    .bind(product_id)
    .bind(external_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

/// Returns the active persistent address, creating version one atomically when absent.
#[allow(clippy::too_many_arguments)]
pub async fn get_or_create_persistent_address(
    pool: &PgPool,
    product_id: Uuid,
    account: &Account,
    product_slug: &str,
    chain_id: u64,
    factory: EvmAddress,
    implementation: EvmAddress,
) -> Result<Address, ApiError> {
    let mut transaction = pool.begin().await?;
    lock_account(&mut transaction, product_id, account.id).await?;
    if let Some(address) = find_active_address(&mut transaction, account.id, chain_id).await? {
        transaction.commit().await?;
        return Ok(address);
    }
    let address = insert_persistent_address(
        &mut transaction,
        account,
        product_slug,
        chain_id,
        factory,
        implementation,
        1,
    )
    .await?;
    transaction.commit().await?;
    Ok(address)
}

/// Retires the active address and creates the next persistent version atomically.
#[allow(clippy::too_many_arguments)]
pub async fn rotate_persistent_address(
    pool: &PgPool,
    product_id: Uuid,
    account: &Account,
    product_slug: &str,
    chain_id: u64,
    factory: EvmAddress,
    implementation: EvmAddress,
) -> Result<Address, ApiError> {
    let mut transaction = pool.begin().await?;
    lock_account(&mut transaction, product_id, account.id).await?;
    let current = find_active_address(&mut transaction, account.id, chain_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let version = current
        .version
        .checked_add(1)
        .ok_or_else(|| ApiError::conflict("address version is exhausted"))?;
    sqlx::query("UPDATE addresses SET retired_at = now() WHERE id = $1 AND retired_at IS NULL")
        .bind(current.id)
        .execute(&mut *transaction)
        .await?;
    let address = insert_persistent_address(
        &mut transaction,
        account,
        product_slug,
        chain_id,
        factory,
        implementation,
        version,
    )
    .await?;
    transaction.commit().await?;
    Ok(address)
}

/// Lists a tenant account's deposits using a stable UUID cursor.
pub async fn list_account_deposits(
    pool: &PgPool,
    product_id: Uuid,
    account_id: Uuid,
    filters: &DepositListQuery,
) -> Result<DepositsResponse, ApiError> {
    let mut query = deposit_query();
    query
        .push(" WHERE account.product_id = ")
        .push_bind(product_id);
    query
        .push(" AND deposit.account_id = ")
        .push_bind(account_id);
    if let Some(state) = &filters.state {
        query.push(" AND deposit.state = ").push_bind(state.clone());
    }
    if let Some(from) = filters.from {
        query.push(" AND deposit.created_at >= ").push_bind(from);
    }
    if let Some(to) = filters.to {
        query.push(" AND deposit.created_at < ").push_bind(to);
    }
    if let Some(cursor) = filters.cursor {
        query.push(
            " AND (deposit.created_at, deposit.id) < (SELECT created_at, id FROM deposits WHERE id = ",
        );
        query.push_bind(cursor);
        query
            .push(" AND account_id = ")
            .push_bind(account_id)
            .push(")");
    }
    fetch_page(pool, query).await
}

/// Fetches one deposit only when its account belongs to the requested product.
pub async fn get_product_deposit(
    pool: &PgPool,
    product_id: Uuid,
    deposit_id: Uuid,
) -> Result<Option<DepositResponse>, ApiError> {
    let mut query = deposit_query();
    query
        .push(" WHERE account.product_id = ")
        .push_bind(product_id);
    query.push(" AND deposit.id = ").push_bind(deposit_id);
    let row = query
        .build_query_as::<DepositViewRow>()
        .fetch_optional(pool)
        .await?;
    row.map(TryInto::try_into).transpose()
}

/// Looks up product deposits by exactly one support key.
pub async fn lookup_product_deposits(
    pool: &PgPool,
    product_id: Uuid,
    filters: &DepositLookupQuery,
) -> Result<DepositsResponse, ApiError> {
    let mut query = deposit_query();
    query
        .push(" WHERE account.product_id = ")
        .push_bind(product_id);
    match (&filters.tx_hash, &filters.address, &filters.lock_ref) {
        (Some(tx_hash), None, None) => {
            let value = B256::from_str(tx_hash).map_err(|_| {
                ApiError::bad_request("tx_hash must be a 32-byte hexadecimal value")
            })?;
            query
                .push(" AND deposit.tx_hash = ")
                .push_bind(format!("{value:#x}"));
        }
        (None, Some(address), None) => {
            let value = EvmAddress::from_str(address).map_err(|_| {
                ApiError::bad_request("address must be a 20-byte hexadecimal value")
            })?;
            query
                .push(" AND address.address = ")
                .push_bind(format!("{value:#x}"));
        }
        (None, None, Some(lock_ref)) if !lock_ref.is_empty() => {
            query
                .push(" AND address.lock_ref = ")
                .push_bind(lock_ref.clone());
        }
        _ => {
            return Err(ApiError::bad_request(
                "exactly one of tx_hash, address, or lock_ref is required",
            ));
        }
    }
    fetch_page(pool, query).await
}

/// Adds or removes account pause scopes and appends an audit row in the same transaction.
pub async fn mutate_account_scopes(
    pool: &PgPool,
    product_id: Uuid,
    account_id: Uuid,
    requested: &[String],
    pause: bool,
    actor: &str,
) -> Result<Vec<String>, ApiError> {
    let mut transaction = pool.begin().await?;
    let current: Option<Vec<String>> = sqlx::query_scalar(
        "SELECT paused_scopes FROM accounts WHERE id = $1 AND product_id = $2 FOR UPDATE",
    )
    .bind(account_id)
    .bind(product_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let current = current.ok_or_else(ApiError::not_found)?;
    let updated = updated_scopes(current, requested, pause);
    sqlx::query("UPDATE accounts SET paused_scopes = $2 WHERE id = $1")
        .bind(account_id)
        .bind(&updated)
        .execute(&mut *transaction)
        .await?;
    insert_audit_tx(
        &mut transaction,
        actor,
        if pause { "pause" } else { "resume" },
        &format!("account:{account_id}"),
    )
    .await?;
    transaction.commit().await?;
    Ok(updated)
}

/// Adds or removes route pause scopes and appends the required administrative audit row.
pub async fn mutate_route_scopes(
    pool: &PgPool,
    route: &str,
    requested: &[String],
    pause: bool,
    actor: &str,
) -> Result<Vec<String>, ApiError> {
    let mut transaction = pool.begin().await?;
    sqlx::query(
        "INSERT INTO route_pauses (route, paused_scopes) VALUES ($1, '{}') ON CONFLICT DO NOTHING",
    )
    .bind(route)
    .execute(&mut *transaction)
    .await?;
    let current: Vec<String> =
        sqlx::query_scalar("SELECT paused_scopes FROM route_pauses WHERE route = $1 FOR UPDATE")
            .bind(route)
            .fetch_one(&mut *transaction)
            .await?;
    let updated = updated_scopes(current, requested, pause);
    sqlx::query("UPDATE route_pauses SET paused_scopes = $2 WHERE route = $1")
        .bind(route)
        .bind(&updated)
        .execute(&mut *transaction)
        .await?;
    insert_audit_tx(
        &mut transaction,
        actor,
        if pause { "pause" } else { "resume" },
        &format!("route:{route}"),
    )
    .await?;
    transaction.commit().await?;
    Ok(updated)
}

#[derive(FromRow)]
struct ProductRow {
    id: Uuid,
    slug: String,
    settlement_url: String,
    webhook_url: String,
    pubkey: String,
    kid: String,
    paused_scopes: Vec<String>,
}

impl From<ProductRow> for Product {
    fn from(row: ProductRow) -> Self {
        Self {
            id: row.id,
            slug: row.slug,
            settlement_url: row.settlement_url,
            webhook_url: row.webhook_url,
            pubkey: row.pubkey,
            kid: row.kid,
            paused_scopes: row.paused_scopes,
        }
    }
}

#[derive(FromRow)]
struct AccountRow {
    id: Uuid,
    product_id: Uuid,
    external_id: String,
    paused_scopes: Vec<String>,
}

impl From<AccountRow> for Account {
    fn from(row: AccountRow) -> Self {
        Self {
            id: row.id,
            product_id: row.product_id,
            external_id: row.external_id,
            paused_scopes: row.paused_scopes,
        }
    }
}

#[derive(FromRow)]
struct AddressRow {
    id: Uuid,
    account_id: Uuid,
    chain_id: i64,
    kind: String,
    version: i64,
    lock_ref: Option<String>,
    salt: String,
    address: String,
    retired_at: Option<DateTime<Utc>>,
}

impl TryFrom<AddressRow> for Address {
    type Error = ApiError;

    fn try_from(row: AddressRow) -> Result<Self, Self::Error> {
        let kind = match row.kind.as_str() {
            "persistent" => AddressKind::Persistent,
            "lock" => AddressKind::Lock,
            _ => return Err(ApiError::internal()),
        };
        Ok(Self {
            id: row.id,
            account_id: row.account_id,
            chain_id: u64::try_from(row.chain_id).map_err(|_| ApiError::internal())?,
            kind,
            version: u64::try_from(row.version).map_err(|_| ApiError::internal())?,
            lock_ref: row.lock_ref,
            salt: B256::from_str(&row.salt).map_err(|_| ApiError::internal())?,
            address: EvmAddress::from_str(&row.address).map_err(|_| ApiError::internal())?,
            retired_at: row.retired_at,
        })
    }
}

#[derive(FromRow)]
struct DepositViewRow {
    id: Uuid,
    chain_id: i64,
    tx_hash: String,
    log_index: i64,
    block_number: i64,
    block_time: DateTime<Utc>,
    address: String,
    lock_ref: Option<String>,
    route: Option<String>,
    route_version: Option<i64>,
    asset_contract: String,
    from_address: String,
    amount_atomic: String,
    state: String,
    valuation_at: Option<DateTime<Utc>>,
    price_scaled: Option<String>,
    credit_minor: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<DepositViewRow> for DepositResponse {
    type Error = ApiError;

    fn try_from(row: DepositViewRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            chain_id: u64::try_from(row.chain_id).map_err(|_| ApiError::internal())?,
            tx_hash: row.tx_hash,
            log_index: u64::try_from(row.log_index).map_err(|_| ApiError::internal())?,
            block_number: u64::try_from(row.block_number).map_err(|_| ApiError::internal())?,
            block_time: row.block_time,
            address: row.address,
            lock_ref: row.lock_ref,
            route: row.route,
            route_version: row
                .route_version
                .map(u64::try_from)
                .transpose()
                .map_err(|_| ApiError::internal())?,
            asset_contract: row.asset_contract,
            from_address: row.from_address,
            amount_atomic: row.amount_atomic,
            state: row.state,
            valuation_at: row.valuation_at,
            price_scaled: row.price_scaled,
            credit_minor: row.credit_minor,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

fn deposit_query() -> QueryBuilder<'static, Postgres> {
    QueryBuilder::new(
        r#"
        SELECT deposit.id, deposit.chain_id, deposit.tx_hash, deposit.log_index,
               deposit.block_number, deposit.block_time, address.address, address.lock_ref,
               deposit.route, deposit.route_version, deposit.asset_contract,
               deposit.from_address, deposit.amount_atomic::text AS amount_atomic,
               deposit.state, deposit.valuation_at, deposit.price_scaled::text AS price_scaled,
               deposit.credit_minor::text AS credit_minor, deposit.created_at, deposit.updated_at
        FROM deposits AS deposit
        JOIN accounts AS account ON account.id = deposit.account_id
        JOIN addresses AS address ON address.id = deposit.address_id
        "#,
    )
}

async fn fetch_page(
    pool: &PgPool,
    mut query: QueryBuilder<'static, Postgres>,
) -> Result<DepositsResponse, ApiError> {
    query.push(" ORDER BY deposit.created_at DESC, deposit.id DESC LIMIT 51");
    let rows = query
        .build_query_as::<DepositViewRow>()
        .fetch_all(pool)
        .await?;
    let has_more = rows.len() > 50;
    let deposits: Vec<DepositResponse> = rows
        .into_iter()
        .take(50)
        .map(TryInto::try_into)
        .collect::<Result<Vec<_>, _>>()?;
    let next_cursor = has_more
        .then(|| deposits.last().map(|deposit| deposit.id))
        .flatten();
    Ok(DepositsResponse {
        deposits,
        next_cursor,
    })
}

async fn lock_account(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    product_id: Uuid,
    account_id: Uuid,
) -> Result<(), ApiError> {
    let found = sqlx::query("SELECT id FROM accounts WHERE id = $1 AND product_id = $2 FOR UPDATE")
        .bind(account_id)
        .bind(product_id)
        .fetch_optional(&mut **transaction)
        .await?;
    if found.is_none() {
        return Err(ApiError::not_found());
    }
    Ok(())
}

async fn find_active_address(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    account_id: Uuid,
    chain_id: u64,
) -> Result<Option<Address>, ApiError> {
    let chain_id = i64::try_from(chain_id).map_err(|_| ApiError::internal())?;
    let row = sqlx::query_as::<_, AddressRow>(
        r#"
        SELECT id, account_id, chain_id, kind, version, lock_ref, salt, address, retired_at
        FROM addresses
        WHERE account_id = $1 AND chain_id = $2 AND kind = 'persistent' AND retired_at IS NULL
        FOR UPDATE
        "#,
    )
    .bind(account_id)
    .bind(chain_id)
    .fetch_optional(&mut **transaction)
    .await?;
    row.map(TryInto::try_into).transpose()
}

#[allow(clippy::too_many_arguments)]
async fn insert_persistent_address(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    account: &Account,
    product_slug: &str,
    chain_id: u64,
    factory: EvmAddress,
    implementation: EvmAddress,
    version: u64,
) -> Result<Address, ApiError> {
    let salt = persistent_salt(product_slug, &account.external_id, version);
    let address = forwarder_address(factory, implementation, salt);
    let row = sqlx::query_as::<_, AddressRow>(
        r#"
        INSERT INTO addresses
            (id, account_id, chain_id, kind, version, lock_ref, salt, address, retired_at)
        VALUES ($1, $2, $3, 'persistent', $4, NULL, $5, $6, NULL)
        RETURNING id, account_id, chain_id, kind, version, lock_ref, salt, address, retired_at
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(account.id)
    .bind(i64::try_from(chain_id).map_err(|_| ApiError::internal())?)
    .bind(i64::try_from(version).map_err(|_| ApiError::conflict("address version is exhausted"))?)
    .bind(format!("{salt:#x}"))
    .bind(format!("{address:#x}"))
    .fetch_one(&mut **transaction)
    .await?;
    row.try_into()
}

fn updated_scopes(current: Vec<String>, requested: &[String], pause: bool) -> Vec<String> {
    let mut scopes = current.into_iter().collect::<BTreeSet<_>>();
    for scope in requested {
        if pause {
            scopes.insert(scope.clone());
        } else {
            scopes.remove(scope);
        }
    }
    scopes.into_iter().collect()
}

async fn insert_audit_tx(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    actor: &str,
    action: &str,
    subject: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO audit (id, actor, action, subject, reason) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(actor)
    .bind(action)
    .bind(subject)
    .bind("signed API request")
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

//! Quote-first rate-lock lifecycle, exposure reservations, and expiry processing.

pub mod pricing;

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{Address as EvmAddress, U256};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rand::TryRng as _;
use rand::rngs::SysRng;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::{FromRow, PgPool, Postgres, Row, Transaction};
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use topup_core::address::{forwarder_address, lock_salt};
use topup_core::money::{
    AtomicAmount, MinorAmount, PRICE_SCALE, ScaledPrice, lock_price, tokens_for_credit,
};
use topup_core::route::RouteFile;
use uuid::Uuid;

use crate::db::{Account, Product};
use pricing::{PricingRuntime, ValidatedQuote};

/// The columns of [`RateLockRow`]; callers append the `WHERE` clause with `concat!`.
macro_rules! select_lock {
    () => {
        r#"
    SELECT rate_lock.address_id, account.external_id, rate_lock.route, address.chain_id,
           address.address,
           rate_lock.amount_atomic::text AS amount_atomic,
           rate_lock.price_scaled::text AS price_scaled,
           rate_lock.credit_minor::text AS credit_minor,
           rate_lock.expires_at, rate_lock.status, rate_lock.created_at, rate_lock.consumed_by
    FROM rate_locks AS rate_lock
    JOIN addresses AS address ON address.id = rate_lock.address_id
    JOIN accounts AS account ON account.id = address.account_id"#
    };
}

const EXPIRY_BATCH_SIZE: i64 = 100;

/// Current persisted rate-lock status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RateLockStatus {
    /// Awaiting its first eligible payment.
    Open,
    /// Used by an eligible payment.
    Consumed,
    /// Passed its payment window without consumption.
    Expired,
    /// Cancelled before payment.
    Cancelled,
}

impl RateLockStatus {
    /// Returns the stable API and database status code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Consumed => "consumed",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> Result<Self, RateLockError> {
        match value {
            "open" => Ok(Self::Open),
            "consumed" => Ok(Self::Consumed),
            "expired" => Ok(Self::Expired),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(RateLockError::DatabaseInvariant),
        }
    }
}

/// Product-visible immutable rate-lock facts and lifecycle state: the API's quote.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RateLock {
    /// Address row identifier, also the quote id.
    pub address_id: Uuid,
    /// The account's product-owned identifier.
    pub account_external_id: String,
    /// Route name.
    pub route: String,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Single-use forwarder address.
    pub address: EvmAddress,
    /// Exact requested token amount.
    pub amount_atomic: AtomicAmount,
    /// Frozen eight-decimal price.
    pub price: ScaledPrice,
    /// Frozen product credit.
    pub credit_minor: MinorAmount,
    /// Payment deadline.
    pub expires_at: DateTime<Utc>,
    /// Persisted lifecycle status.
    pub status: RateLockStatus,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// The deposit that consumed the lock, once consumed.
    pub consumed_by: Option<Uuid>,
}

/// The public id of a quote: `qt_` and the hex of its address row id. New quotes derive their
/// address salt from it, so a product can recompute the address from the id alone.
#[must_use]
pub fn quote_id(address_id: Uuid) -> String {
    crate::ids::format(crate::ids::QUOTE, address_id)
}

/// A validated quote source used by rate-lock creation.
#[async_trait]
pub trait QuoteProvider: Send + Sync {
    /// Fetches and validates the current spot price for the selected route.
    async fn quote(&self, route: &RouteFile) -> Result<ValidatedQuote, Value>;
}

/// Production quote provider configured from attested route files.
pub struct ConfiguredQuoteProvider {
    runtimes: BTreeMap<(String, u64), PricingRuntime>,
}

impl ConfiguredQuoteProvider {
    /// Builds adapters for every route version.
    pub fn from_routes(routes: &[RouteFile]) -> Result<Self, String> {
        let mut runtimes = BTreeMap::new();
        for route in routes {
            let key = (route.route.clone(), route.version);
            if runtimes
                .insert(key.clone(), PricingRuntime::configured(route)?)
                .is_some()
            {
                return Err(format!(
                    "duplicate pricing runtime for route `{}` version {}",
                    key.0, key.1
                ));
            }
        }
        Ok(Self { runtimes })
    }
}

#[async_trait]
impl QuoteProvider for ConfiguredQuoteProvider {
    async fn quote(&self, route: &RouteFile) -> Result<ValidatedQuote, Value> {
        let runtime = self
            .runtimes
            .get(&(route.route.clone(), route.version))
            .ok_or_else(|| json!({"stage": "pricing", "error": "missing_route_runtime"}))?;
        runtime.fetch(route).await
    }
}

/// Quote provider used by API surfaces that do not exercise rate-lock creation.
pub struct UnavailableQuoteProvider;

#[async_trait]
impl QuoteProvider for UnavailableQuoteProvider {
    async fn quote(&self, _route: &RouteFile) -> Result<ValidatedQuote, Value> {
        Err(json!({"stage": "pricing", "error": "unavailable"}))
    }
}

/// Rate-lock lifecycle failure mapped by the API boundary.
#[derive(Debug, thiserror::Error)]
pub enum RateLockError {
    /// Request input is invalid.
    #[error("{0}")]
    InvalidInput(&'static str),
    /// The requested credit, or its token amount, is below the route's minimum.
    #[error("{0}")]
    AmountTooSmall(&'static str),
    /// The requested credit's token amount is above the route's maximum deposit.
    #[error("{0}")]
    AmountTooLarge(&'static str),
    /// Current validated pricing is unavailable.
    #[error("validated pricing is unavailable")]
    PricingUnavailable,
    /// The per-account rolling creation limit was reached.
    #[error("rate-lock creation limit exceeded")]
    RateLimited,
    /// An open exposure cap would be exceeded.
    #[error("the {scope} cap on open quotes leaves {remaining} cents")]
    ExposureCap {
        /// `account`, `product`, or `global`.
        scope: &'static str,
        /// Credit, in minor units, still available under the cap.
        remaining: u64,
    },
    /// The tenant-scoped lock does not exist.
    #[error("rate lock not found")]
    NotFound,
    /// The lock can no longer be cancelled.
    #[error("rate lock is {}", .0.code())]
    NotOpen(RateLockStatus),
    /// The lock is still open but its payment window has closed, so it cannot be cancelled.
    #[error("payment window has closed")]
    WindowClosed,
    /// The lock address already received a deposit, so it cannot be cancelled.
    #[error("rate lock address already received a payment")]
    PendingPayment,
    /// An idempotency key was reused with different parameters.
    #[error("idempotency key was reused with different parameters")]
    IdempotencyMismatch,
    /// Money arithmetic could not be represented.
    #[error("rate-lock arithmetic is out of range")]
    Arithmetic,
    /// The operating system's random number generator failed.
    #[error("operating system entropy is unavailable")]
    EntropyUnavailable,
    /// Persisted data violated an internal invariant.
    #[error("rate-lock database invariant failed")]
    DatabaseInvariant,
    /// PostgreSQL rejected or failed the operation.
    #[error("{0}")]
    Database(#[from] sqlx::Error),
}

/// Creates a lock for `credit` on `route`, or, for a repeated `idempotency_key`, returns the lock
/// created with it.
///
/// A repeat must name the same account, route, and credit; anything else is an idempotency
/// mismatch. Callers answer a repeat (with [`find_by_idempotency_key`]) before any other check, so
/// pausing quotes never hides a lock the product already showed.
pub async fn create(
    pool: &PgPool,
    quotes: &Arc<dyn QuoteProvider>,
    product: &Product,
    account: &Account,
    route: &RouteFile,
    idempotency_key: Option<&str>,
    credit_minor: MinorAmount,
) -> Result<RateLock, RateLockError> {
    if let Some(key) = idempotency_key
        && let Some(existing) = find_by_idempotency_key(pool, product.id, key).await?
    {
        return replay(existing, account, route, credit_minor);
    }
    // Cheap pre-check so a rate-limited caller never triggers an external price fetch; the
    // authoritative check repeats under the account row lock below.
    check_creation_rate(pool, account.id, route).await?;

    let quote = quotes
        .quote(route)
        .await
        .map_err(|evidence| {
            tracing::warn!(route = %route.route, quote = %evidence, "rate-lock price validation failed");
            RateLockError::PricingUnavailable
        })?;
    let locked_price = lock_price(quote.price, route.rate_lock.spread_bps)
        .map_err(|_| RateLockError::Arithmetic)?;
    let amount_atomic = amount_for_credit(route, credit_minor, locked_price)?;
    validate_bounds(route, amount_atomic, credit_minor)?;
    let window = i64::try_from(route.rate_lock.window_s).map_err(|_| RateLockError::Arithmetic)?;
    let now = Utc::now();
    let expires_at = now
        .checked_add_signed(chrono::Duration::seconds(window))
        .ok_or(RateLockError::Arithmetic)?;

    let mut transaction = pool.begin().await?;
    lock_account(&mut transaction, product.id, account.id).await?;
    check_creation_rate(&mut *transaction, account.id, route).await?;
    check_exposure(
        &mut transaction,
        product.id,
        account.id,
        credit_minor,
        route,
    )
    .await?;
    let address_id = Uuid::new_v4();
    let lock_ref = quote_id(address_id);
    let salt = lock_salt(&product.slug, &account.external_id, &lock_ref);
    let address = forwarder_address(
        route.chain.contracts.forwarder_factory,
        route.chain.contracts.implementation,
        salt,
    );
    // A freshly derived single-use address cannot hold earlier payments, so the scanner only
    // needs to cover it from the chain's committed cursor instead of backfilling from genesis.
    sqlx::query(
        r#"
        INSERT INTO addresses (
            id, account_id, chain_id, kind, version, lock_ref, salt, address, retired_at,
            created_block
        )
        VALUES (
            $1, $2, $3, 'lock', 0, $4, $5, $6, NULL,
            COALESCE((SELECT scanned_block FROM cursors WHERE chain_id = $3), 0)
        )
        "#,
    )
    .bind(address_id)
    .bind(account.id)
    .bind(i64::try_from(route.chain.chain_id).map_err(|_| RateLockError::Arithmetic)?)
    .bind(&lock_ref)
    .bind(format!("{salt:#x}"))
    .bind(format!("{address:#x}"))
    .execute(&mut *transaction)
    .await?;
    let inserted = sqlx::query(
        r#"
        INSERT INTO rate_locks (
            address_id, route, amount_atomic, price_scaled, credit_minor, expires_at,
            status, exposure_reserved, created_at, product_id, idempotency_key
        )
        VALUES ($1, $2, $3::text::numeric, $4::text::numeric, $5::text::numeric, $6,
                'open', true, $7, $8, $9)
        "#,
    )
    .bind(address_id)
    .bind(&route.route)
    .bind(amount_atomic.value().to_string())
    .bind(locked_price.value().to_string())
    .bind(credit_minor.value().to_string())
    .bind(expires_at)
    .bind(now)
    .bind(product.id)
    .bind(idempotency_key)
    .execute(&mut *transaction)
    .await;
    match inserted {
        Ok(_) => transaction.commit().await?,
        // A concurrent request with the same key committed first: answer as its repeat.
        Err(sqlx::Error::Database(error))
            if error.constraint() == Some("rate_locks_product_idempotency_key_unique") =>
        {
            drop(transaction);
            let key = idempotency_key.ok_or(RateLockError::DatabaseInvariant)?;
            let existing = find_by_idempotency_key(pool, product.id, key)
                .await?
                .ok_or(RateLockError::DatabaseInvariant)?;
            return replay(existing, account, route, credit_minor);
        }
        Err(error) => return Err(error.into()),
    }
    Ok(RateLock {
        address_id,
        account_external_id: account.external_id.clone(),
        route: route.route.clone(),
        chain_id: route.chain.chain_id,
        address,
        amount_atomic,
        price: locked_price,
        credit_minor,
        expires_at,
        status: RateLockStatus::Open,
        created_at: now,
        consumed_by: None,
    })
}

/// Returns the product's lock created with `idempotency_key`, if any.
pub async fn find_by_idempotency_key(
    pool: &PgPool,
    product_id: Uuid,
    idempotency_key: &str,
) -> Result<Option<RateLock>, RateLockError> {
    let row = sqlx::query_as::<_, RateLockRow>(concat!(
        select_lock!(),
        " WHERE rate_lock.product_id = $1 AND rate_lock.idempotency_key = $2"
    ))
    .bind(product_id)
    .bind(idempotency_key)
    .fetch_optional(pool)
    .await?;
    row.map(TryInto::try_into).transpose()
}

/// Checks that a repeated idempotency key names the stored lock's account, route, and credit.
///
/// # Errors
///
/// [`RateLockError::IdempotencyMismatch`] when any of them differs.
pub fn replay(
    existing: RateLock,
    account: &Account,
    route: &RouteFile,
    credit_minor: MinorAmount,
) -> Result<RateLock, RateLockError> {
    if existing.account_external_id == account.external_id
        && existing.route == route.route
        && existing.chain_id == route.chain.chain_id
        && existing.credit_minor == credit_minor
    {
        Ok(existing)
    } else {
        Err(RateLockError::IdempotencyMismatch)
    }
}

/// Random bytes after `_secret_` in a client secret.
const CLIENT_SECRET_BYTES: usize = 24;

/// Issues the quote's `client_secret`, `qt_…_secret_` followed by 48 random hex digits, and stores
/// only its SHA-256, replacing any earlier secret so that one stops working.
pub async fn issue_client_secret(pool: &PgPool, address_id: Uuid) -> Result<String, RateLockError> {
    let mut random = [0_u8; CLIENT_SECRET_BYTES];
    SysRng.try_fill_bytes(&mut random).map_err(|error| {
        tracing::error!(%error, "OS RNG failed; no client secret issued");
        RateLockError::EntropyUnavailable
    })?;
    let secret = format!("{}_secret_{}", quote_id(address_id), hex::encode(random));
    let updated =
        sqlx::query("UPDATE rate_locks SET client_secret_hash = $2 WHERE address_id = $1")
            .bind(address_id)
            .bind(Sha256::digest(secret.as_bytes()).as_slice())
            .execute(pool)
            .await?
            .rows_affected();
    if updated != 1 {
        return Err(RateLockError::NotFound);
    }
    Ok(secret)
}

/// Loads the quote a client secret belongs to; any secret that does not match a stored one, in
/// form or value, is `None`.
pub async fn get_by_client_secret(
    pool: &PgPool,
    client_secret: &str,
) -> Result<Option<RateLock>, RateLockError> {
    let Some(address_id) = client_secret
        .split_once("_secret_")
        .and_then(|(quote, _)| crate::ids::parse(crate::ids::QUOTE, quote))
    else {
        return Ok(None);
    };
    let row = sqlx::query_as::<_, RateLockRow>(concat!(
        select_lock!(),
        " WHERE rate_lock.address_id = $1 AND rate_lock.client_secret_hash = $2"
    ))
    .bind(address_id)
    .bind(Sha256::digest(client_secret.as_bytes()).as_slice())
    .fetch_optional(pool)
    .await?;
    row.map(TryInto::try_into).transpose()
}

/// Loads one lock when it belongs to the authenticated product.
pub async fn get(
    pool: &PgPool,
    product_id: Uuid,
    address_id: Uuid,
) -> Result<Option<RateLock>, RateLockError> {
    let row = sqlx::query_as::<_, RateLockRow>(concat!(
        select_lock!(),
        " WHERE account.product_id = $1 AND rate_lock.address_id = $2"
    ))
    .bind(product_id)
    .bind(address_id)
    .fetch_optional(pool)
    .await?;
    row.map(TryInto::try_into).transpose()
}

/// Cancels an unpaid open lock, releasing its exposure, and appends an audit row atomically.
///
/// Any deposit row for the lock address, including a rejected one, means funds already arrived at
/// the single-use address, so the lock is no longer unpaid and cancellation is refused; such a
/// lock stays open until it is consumed or expires.
pub async fn cancel(
    pool: &PgPool,
    product: &Product,
    address_id: Uuid,
) -> Result<RateLock, RateLockError> {
    let mut transaction = pool.begin().await?;
    let row = get_in(&mut transaction, product.id, address_id)
        .await?
        .ok_or(RateLockError::NotFound)?;
    if row.status == RateLockStatus::Cancelled {
        transaction.commit().await?;
        return Ok(row);
    }
    if row.status != RateLockStatus::Open {
        return Err(RateLockError::NotOpen(row.status));
    }
    // The lock stays `open` until chain-time expiry, but an in-window payment may still be
    // finalizing, so cancellation ends with the payment window.
    if row.expires_at <= Utc::now() {
        return Err(RateLockError::WindowClosed);
    }
    // `FOR UPDATE` conflicts with the `KEY SHARE` lock a scanner deposit insert takes on its
    // address row, so an uncommitted deposit either commits first and is seen below, or waits
    // until this cancellation commits.
    sqlx::query("SELECT 1 FROM addresses WHERE id = $1 FOR UPDATE")
        .bind(row.address_id)
        .execute(&mut *transaction)
        .await?;
    let paid: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM deposits WHERE address_id = $1)")
            .bind(row.address_id)
            .fetch_one(&mut *transaction)
            .await?;
    if paid {
        return Err(RateLockError::PendingPayment);
    }
    sqlx::query(
        r#"
        UPDATE rate_locks
        SET status = 'cancelled', exposure_reserved = false, closed_at = now()
        WHERE address_id = $1 AND status = 'open' AND consumed_by IS NULL
        "#,
    )
    .bind(row.address_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO audit (id, actor, action, subject, reason) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(format!("product:{}", product.id))
    .bind("cancel_rate_lock")
    .bind(format!("rate_lock:{}", row.address_id))
    .bind("signed API request")
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(RateLock {
        status: RateLockStatus::Cancelled,
        ..row
    })
}

/// Atomically consumes an open lock, releasing its exposure reservation.
pub(crate) async fn consume(
    transaction: &mut Transaction<'_, Postgres>,
    address_id: Uuid,
    deposit_id: Uuid,
    idempotent: bool,
) -> Result<bool, sqlx::Error> {
    let Some((status, consumed_by)) = sqlx::query_as::<_, (String, Option<Uuid>)>(
        "SELECT status, consumed_by FROM rate_locks WHERE address_id = $1 FOR UPDATE",
    )
    .bind(address_id)
    .fetch_optional(&mut **transaction)
    .await?
    else {
        return Ok(false);
    };
    let status = RateLockStatus::parse(&status).map_err(rate_lock_sqlx)?;
    if status == RateLockStatus::Consumed && idempotent && consumed_by == Some(deposit_id) {
        return Ok(true);
    }
    if !matches!(status, RateLockStatus::Open | RateLockStatus::Expired) || consumed_by.is_some() {
        return Ok(false);
    }
    sqlx::query(
        r#"
        UPDATE rate_locks
        SET consumed_by = $2, status = 'consumed', exposure_reserved = false, closed_at = now()
        WHERE address_id = $1 AND status IN ('open', 'expired') AND consumed_by IS NULL
        "#,
    )
    .bind(address_id)
    .bind(deposit_id)
    .execute(&mut **transaction)
    .await?;
    Ok(true)
}

/// Expires one bounded batch and returns the number of rows closed.
///
/// A lock expires by chain time, not wall-clock time: only once its chain's scanner has committed
/// through a finalized block whose time is past `expires_at`, so every payment mined inside the
/// window is already recorded, and only while no such payment still awaits the confirm step that
/// may consume the lock. A stalled scanner therefore holds locks and their exposure open.
pub async fn expire_once(pool: &PgPool) -> Result<u64, RateLockError> {
    let mut transaction = pool.begin().await?;
    let rows = sqlx::query_as::<_, ExpiringRow>(
        r#"
        SELECT rate_lock.address_id, rate_lock.credit_minor::text AS credit_minor,
               rate_lock.amount_atomic::text AS amount_atomic,
               account.product_id, account.external_id, address.chain_id, address.address,
               address.lock_ref, rate_lock.route, rate_lock.expires_at
        FROM rate_locks AS rate_lock
        JOIN addresses AS address ON address.id = rate_lock.address_id
        JOIN accounts AS account ON account.id = address.account_id
        JOIN cursors AS cursor ON cursor.chain_id = address.chain_id
        WHERE rate_lock.status = 'open'
          AND rate_lock.consumed_by IS NULL
          AND rate_lock.expires_at < cursor.scanned_block_time
          AND NOT EXISTS (
              SELECT 1
              FROM deposits AS deposit
              WHERE deposit.address_id = rate_lock.address_id
                AND deposit.state = 'detected'
                AND deposit.block_time <= rate_lock.expires_at
          )
        ORDER BY rate_lock.expires_at, rate_lock.address_id
        FOR UPDATE OF rate_lock SKIP LOCKED
        LIMIT $1
        "#,
    )
    .bind(EXPIRY_BATCH_SIZE)
    .fetch_all(&mut *transaction)
    .await?;
    if rows.is_empty() {
        transaction.commit().await?;
        return Ok(0);
    }
    let count = u64::try_from(rows.len()).map_err(|_| RateLockError::DatabaseInvariant)?;
    let address_ids = rows.iter().map(|row| row.address_id).collect::<Vec<_>>();
    let updated = sqlx::query(
        r#"
        UPDATE rate_locks
        SET status = 'expired', exposure_reserved = false, closed_at = now()
        WHERE address_id = ANY($1) AND status = 'open' AND consumed_by IS NULL
        "#,
    )
    .bind(&address_ids)
    .execute(&mut *transaction)
    .await?;
    if updated.rows_affected() != count {
        return Err(RateLockError::DatabaseInvariant);
    }

    for row in &rows {
        let credit_minor = parse_minor(&row.credit_minor)?;
        sqlx::query(
            r#"
            INSERT INTO outbox (id, event_type, payload, next_attempt_at)
            VALUES ($1, 'rate_lock.expired', $2, now())
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(json!({
            "product_id": row.product_id,
            "external_id": row.external_id,
            "product_lock_ref": row.lock_ref,
            "route": row.route,
            "chain_id": row.chain_id,
            "address": row.address,
            "amount_atomic": row.amount_atomic,
            "credit_minor": credit_minor.value().to_string(),
            "expires_at": row.expires_at,
        }))
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(count)
}

/// Periodically closes overdue locks and emits expiry events.
pub struct ExpiryWorker {
    pool: PgPool,
    scan_interval: Duration,
}

impl ExpiryWorker {
    /// Creates an expiry worker.
    #[must_use]
    pub const fn new(pool: PgPool, scan_interval: Duration) -> Self {
        Self {
            pool,
            scan_interval,
        }
    }

    /// Runs expiry scans until cancellation.
    pub async fn run(&self, cancellation: CancellationToken) {
        let monitor = crate::observability::CronMonitor::lock_expiry();
        let mut ticker = interval(self.scan_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return,
                _ = ticker.tick() => {
                    match expire_once(&self.pool).await {
                        Ok(_) => monitor.check_in(true),
                        Err(error) => {
                            tracing::error!(
                                tags.alert = "TopupLockExpiryFailing",
                                %error,
                                "rate-lock expiry scan failed"
                            );
                        }
                    }
                }
            }
        }
    }
}

fn parse_u64(value: &str) -> Result<u64, RateLockError> {
    value
        .parse::<u64>()
        .map_err(|_| RateLockError::DatabaseInvariant)
}

fn amount_for_credit(
    route: &RouteFile,
    credit_minor: MinorAmount,
    price: ScaledPrice,
) -> Result<AtomicAmount, RateLockError> {
    if credit_minor.value() == 0 {
        return Err(RateLockError::AmountTooSmall(
            "amount must be greater than zero",
        ));
    }
    tokens_for_credit(
        credit_minor,
        price,
        route.asset.decimals,
        route.destination.unit_decimals,
    )
    .map_err(|_| RateLockError::AmountTooLarge("amount is too large to quote"))
}

fn validate_bounds(
    route: &RouteFile,
    amount: AtomicAmount,
    credit: MinorAmount,
) -> Result<(), RateLockError> {
    if credit.value() < route.screening.min_credit_minor {
        return Err(RateLockError::AmountTooSmall(
            "amount is below the minimum credit",
        ));
    }
    if amount < route.screening.min_deposit_atomic {
        return Err(RateLockError::AmountTooSmall(
            "amount is below the minimum deposit",
        ));
    }
    if amount > route.screening.max_deposit_atomic {
        return Err(RateLockError::AmountTooLarge(
            "amount is above the maximum deposit",
        ));
    }
    Ok(())
}

async fn check_creation_rate<'e, E>(
    executor: E,
    account_id: Uuid,
    route: &RouteFile,
) -> Result<(), RateLockError>
where
    E: sqlx::PgExecutor<'e>,
{
    let recent: i64 = sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM rate_locks AS rate_lock
        JOIN addresses AS address ON address.id = rate_lock.address_id
        WHERE address.account_id = $1
          AND rate_lock.created_at >= now() - interval '1 minute'
        "#,
    )
    .bind(account_id)
    .fetch_one(executor)
    .await?;
    let recent = u64::try_from(recent).map_err(|_| RateLockError::DatabaseInvariant)?;
    if recent >= route.rate_lock.max_creations_per_minute {
        return Err(RateLockError::RateLimited);
    }
    Ok(())
}

async fn lock_account(
    transaction: &mut Transaction<'_, Postgres>,
    product_id: Uuid,
    account_id: Uuid,
) -> Result<(), RateLockError> {
    // `NO KEY UPDATE` serializes creations per account without blocking the `KEY SHARE` locks
    // that foreign-key checks take when the scanner inserts deposits for this account.
    let found =
        sqlx::query("SELECT id FROM accounts WHERE id = $1 AND product_id = $2 FOR NO KEY UPDATE")
            .bind(account_id)
            .bind(product_id)
            .fetch_optional(&mut **transaction)
            .await?;
    if found.is_none() {
        return Err(RateLockError::NotFound);
    }
    Ok(())
}

/// Rejects a creation that would take any scope's open reserved lock credit past its cap, and
/// raises `TopupLockExposureNearCap` when it takes the product or global credit to 90 percent.
///
/// The transaction-level advisory lock serialises creations from this check to commit, and under
/// `READ COMMITTED` the sum, a later statement, sees every creation committed before it. Closing a
/// lock only lowers the sums, so cancellation, consumption, and expiry need no lock.
async fn check_exposure(
    transaction: &mut Transaction<'_, Postgres>,
    product_id: Uuid,
    account_id: Uuid,
    amount: MinorAmount,
    route: &RouteFile,
) -> Result<(), RateLockError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('rate-lock-exposure', 0))")
        .execute(&mut **transaction)
        .await?;
    let open = sqlx::query(
        r#"
        SELECT coalesce(sum(rate_lock.credit_minor)
                   FILTER (WHERE address.account_id = $1), 0)::text AS account,
               coalesce(sum(rate_lock.credit_minor)
                   FILTER (WHERE account.product_id = $2), 0)::text AS product,
               coalesce(sum(rate_lock.credit_minor), 0)::text AS global
        FROM rate_locks AS rate_lock
        JOIN addresses AS address ON address.id = rate_lock.address_id
        JOIN accounts AS account ON account.id = address.account_id
        WHERE rate_lock.status = 'open' AND rate_lock.exposure_reserved
        "#,
    )
    .bind(account_id)
    .bind(product_id)
    .fetch_one(&mut **transaction)
    .await?;
    let caps = &route.rate_lock.max_open_minor;
    for (scope, cap) in [
        ("account", caps.account),
        ("global", caps.global),
        ("product", caps.product),
    ] {
        let current = parse_u64(open.try_get(scope)?)?;
        let exceeded = RateLockError::ExposureCap {
            scope,
            remaining: cap.saturating_sub(current),
        };
        let Some(next) = current.checked_add(amount.value()) else {
            return Err(exceeded);
        };
        if next > cap {
            return Err(exceeded);
        }
        if scope != "account" && near_cap(next, cap) {
            let id = if scope == "product" {
                route.destination.product.as_str()
            } else {
                "global"
            };
            tracing::warn!(
                tags.alert = "TopupLockExposureNearCap",
                tags.scope = scope,
                tags.id = id,
                open_minor = next,
                cap_minor = cap,
                "open rate-lock exposure is at least 90 percent of its cap"
            );
        }
    }
    Ok(())
}

/// Whether `open` reaches 90 percent of a nonzero `cap`.
fn near_cap(open: u64, cap: u64) -> bool {
    cap > 0 && u128::from(open) * 10 >= u128::from(cap) * 9
}

#[derive(FromRow)]
struct RateLockRow {
    address_id: Uuid,
    external_id: String,
    route: String,
    chain_id: i64,
    address: String,
    amount_atomic: String,
    price_scaled: String,
    credit_minor: String,
    expires_at: DateTime<Utc>,
    status: String,
    created_at: DateTime<Utc>,
    consumed_by: Option<Uuid>,
}

impl TryFrom<RateLockRow> for RateLock {
    type Error = RateLockError;

    fn try_from(row: RateLockRow) -> Result<Self, Self::Error> {
        Ok(Self {
            address_id: row.address_id,
            account_external_id: row.external_id,
            route: row.route,
            chain_id: u64::try_from(row.chain_id).map_err(|_| RateLockError::DatabaseInvariant)?,
            address: EvmAddress::from_str(&row.address)
                .map_err(|_| RateLockError::DatabaseInvariant)?,
            amount_atomic: AtomicAmount::new(
                U256::from_str(&row.amount_atomic).map_err(|_| RateLockError::DatabaseInvariant)?,
            ),
            price: ScaledPrice::new(
                row.price_scaled
                    .parse::<u64>()
                    .map_err(|_| RateLockError::DatabaseInvariant)?,
                PRICE_SCALE,
            )
            .map_err(|_| RateLockError::DatabaseInvariant)?,
            credit_minor: parse_minor(&row.credit_minor)?,
            expires_at: row.expires_at,
            status: RateLockStatus::parse(&row.status)?,
            created_at: row.created_at,
            consumed_by: row.consumed_by,
        })
    }
}

async fn get_in(
    transaction: &mut Transaction<'_, Postgres>,
    product_id: Uuid,
    address_id: Uuid,
) -> Result<Option<RateLock>, RateLockError> {
    let row = sqlx::query_as::<_, RateLockRow>(concat!(
        select_lock!(),
        " WHERE account.product_id = $1 AND rate_lock.address_id = $2 FOR UPDATE OF rate_lock"
    ))
    .bind(product_id)
    .bind(address_id)
    .fetch_optional(&mut **transaction)
    .await?;
    row.map(TryInto::try_into).transpose()
}

#[derive(FromRow)]
struct ExpiringRow {
    address_id: Uuid,
    credit_minor: String,
    amount_atomic: String,
    product_id: Uuid,
    external_id: String,
    chain_id: i64,
    address: String,
    lock_ref: String,
    route: String,
    expires_at: DateTime<Utc>,
}

fn parse_minor(value: &str) -> Result<MinorAmount, RateLockError> {
    value
        .parse::<u64>()
        .map(MinorAmount::new)
        .map_err(|_| RateLockError::DatabaseInvariant)
}

fn rate_lock_sqlx(error: RateLockError) -> sqlx::Error {
    match error {
        RateLockError::Database(error) => error,
        other => sqlx::Error::Protocol(other.to_string()),
    }
}

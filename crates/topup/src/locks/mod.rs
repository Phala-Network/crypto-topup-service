//! Quote-first rate-lock lifecycle, exposure reservations, and expiry processing.

pub mod pricing;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use alloy_primitives::{Address as EvmAddress, U256};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool, Postgres, Row, Transaction};
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use topup_core::address::{forwarder_address, lock_salt};
use topup_core::money::{
    AtomicAmount, MinorAmount, PRICE_SCALE, ScaledPrice, credit, lock_price, tokens_for_credit,
};
use topup_core::route::RouteFile;
use uuid::Uuid;

use crate::db::{Account, Product};
use pricing::{PricingRuntime, ValidatedQuote};

const EXPIRY_BATCH_SIZE: i64 = 100;

/// A caller-selected amount for a new rate lock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestedAmount {
    /// Destination credit in minor units.
    Minor(MinorAmount),
    /// Token amount in atomic units.
    Atomic(AtomicAmount),
}

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

/// Product-visible immutable rate-lock facts and lifecycle state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RateLock {
    /// Address row identifier.
    pub address_id: Uuid,
    /// Product checkout reference.
    pub lock_ref: String,
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
}

/// Current account exposure availability and the next scheduled release.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExposureAvailability {
    /// Remaining account exposure in minor units.
    pub remaining_minor: u64,
    /// Earliest expiry among open reserved locks for the account.
    pub reset_at: Option<DateTime<Utc>>,
}

impl RateLock {
    /// Returns the visible status, treating an overdue open row as expired.
    #[must_use]
    pub fn visible_status(&self, now: DateTime<Utc>) -> RateLockStatus {
        if self.status == RateLockStatus::Open && self.expires_at <= now {
            RateLockStatus::Expired
        } else {
            self.status
        }
    }

    /// Returns whole seconds remaining in the payment window.
    #[must_use]
    pub fn remaining_seconds(&self, now: DateTime<Utc>) -> u64 {
        if self.visible_status(now) != RateLockStatus::Open {
            return 0;
        }
        u64::try_from(self.expires_at.signed_duration_since(now).num_seconds()).unwrap_or_default()
    }
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
#[derive(Debug)]
pub enum RateLockError {
    /// Request amount or reference is invalid.
    InvalidInput(&'static str),
    /// Quote-first creation is disabled for the route.
    Disabled,
    /// Current validated pricing is unavailable.
    PricingUnavailable,
    /// The per-account rolling creation limit was reached.
    RateLimited,
    /// An open exposure cap would be exceeded.
    ExposureCap(&'static str),
    /// The tenant-scoped lock does not exist.
    NotFound,
    /// The lock can no longer be cancelled.
    NotOpen,
    /// The lock address already received a deposit, so it cannot be cancelled.
    PendingPayment,
    /// A replay of an existing reference stated a different amount.
    IdempotencyMismatch,
    /// Money arithmetic could not be represented.
    Arithmetic,
    /// Persisted data violated an internal invariant.
    DatabaseInvariant,
    /// PostgreSQL rejected or failed the operation.
    Database(sqlx::Error),
}

impl Display for RateLockError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) => formatter.write_str(message),
            Self::Disabled => formatter.write_str("rate locks are disabled"),
            Self::PricingUnavailable => formatter.write_str("validated pricing is unavailable"),
            Self::RateLimited => formatter.write_str("rate-lock creation limit exceeded"),
            Self::ExposureCap(scope) => write!(formatter, "{scope} exposure cap exceeded"),
            Self::NotFound => formatter.write_str("rate lock not found"),
            Self::NotOpen => formatter.write_str("rate lock is not open"),
            Self::PendingPayment => {
                formatter.write_str("rate lock address already received a payment")
            }
            Self::IdempotencyMismatch => {
                formatter.write_str("rate-lock reference was reused with a different amount")
            }
            Self::Arithmetic => formatter.write_str("rate-lock arithmetic is out of range"),
            Self::DatabaseInvariant => formatter.write_str("rate-lock database invariant failed"),
            Self::Database(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for RateLockError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for RateLockError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

/// Creates a lock or returns the existing row for the same account reference.
///
/// A replay of an existing reference must state the same amount in the same unit as the stored
/// lock (`amount_atomic` against the locked token amount, `amount_minor` against the locked
/// credit); any other amount is an idempotency mismatch.
pub async fn create(
    pool: &PgPool,
    quotes: &Arc<dyn QuoteProvider>,
    product: &Product,
    account: &Account,
    route: &RouteFile,
    lock_ref: &str,
    requested: RequestedAmount,
) -> Result<RateLock, RateLockError> {
    if !route.rate_lock.enabled {
        return Err(RateLockError::Disabled);
    }
    validate_lock_ref(lock_ref)?;
    if let Some(existing) = get(pool, product.id, account.id, lock_ref).await? {
        return replay(existing, requested);
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
    let (amount_atomic, credit_minor) = amounts(route, requested, locked_price)?;
    validate_bounds(route, amount_atomic, credit_minor)?;
    let window = i64::try_from(route.rate_lock.window_s).map_err(|_| RateLockError::Arithmetic)?;
    let now = Utc::now();
    let expires_at = now
        .checked_add_signed(chrono::Duration::seconds(window))
        .ok_or(RateLockError::Arithmetic)?;

    let mut transaction = pool.begin().await?;
    lock_account(&mut transaction, product.id, account.id).await?;
    if let Some(existing) = get_in(&mut transaction, product.id, account.id, lock_ref).await? {
        transaction.commit().await?;
        return replay(existing, requested);
    }
    check_creation_rate(&mut *transaction, account.id, route).await?;

    reserve_exposure(
        &mut transaction,
        product.id,
        account.id,
        credit_minor,
        route,
    )
    .await?;
    let salt = lock_salt(&product.slug, &account.external_id, lock_ref);
    let address = forwarder_address(
        route.chain.contracts.forwarder_factory,
        route.chain.contracts.implementation,
        salt,
    );
    let address_id = Uuid::new_v4();
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
    .bind(lock_ref)
    .bind(format!("{salt:#x}"))
    .bind(format!("{address:#x}"))
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO rate_locks (
            address_id, route, amount_atomic, price_scaled, credit_minor, expires_at,
            status, exposure_reserved, created_at
        )
        VALUES ($1, $2, $3::text::numeric, $4::text::numeric, $5::text::numeric, $6,
                'open', true, $7)
        "#,
    )
    .bind(address_id)
    .bind(&route.route)
    .bind(amount_atomic.value().to_string())
    .bind(locked_price.value().to_string())
    .bind(credit_minor.value().to_string())
    .bind(expires_at)
    .bind(now)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(RateLock {
        address_id,
        lock_ref: lock_ref.to_owned(),
        chain_id: route.chain.chain_id,
        address,
        amount_atomic,
        price: locked_price,
        credit_minor,
        expires_at,
        status: RateLockStatus::Open,
    })
}

/// Loads one lock only when the account belongs to the authenticated product.
pub async fn get(
    pool: &PgPool,
    product_id: Uuid,
    account_id: Uuid,
    lock_ref: &str,
) -> Result<Option<RateLock>, RateLockError> {
    let row = sqlx::query_as::<_, RateLockRow>(
        r#"
        SELECT rate_lock.address_id, address.lock_ref, address.chain_id, address.address,
               rate_lock.amount_atomic::text AS amount_atomic,
               rate_lock.price_scaled::text AS price_scaled,
               rate_lock.credit_minor::text AS credit_minor,
               rate_lock.expires_at, rate_lock.status
        FROM rate_locks AS rate_lock
        JOIN addresses AS address ON address.id = rate_lock.address_id
        JOIN accounts AS account ON account.id = address.account_id
        WHERE account.product_id = $1 AND account.id = $2 AND address.lock_ref = $3
        "#,
    )
    .bind(product_id)
    .bind(account_id)
    .bind(lock_ref)
    .fetch_optional(pool)
    .await?;
    row.map(TryInto::try_into).transpose()
}

/// Loads account-level remaining exposure from the atomic reservation counter.
pub async fn exposure_availability(
    pool: &PgPool,
    account_id: Uuid,
    cap: u64,
) -> Result<ExposureAvailability, RateLockError> {
    let key = format!("account:{account_id}");
    let open: Option<String> =
        sqlx::query_scalar("SELECT open_minor::text FROM lock_exposure WHERE scope_key = $1")
            .bind(key)
            .fetch_optional(pool)
            .await?;
    let open = open
        .as_deref()
        .map(str::parse::<u64>)
        .transpose()
        .map_err(|_| RateLockError::DatabaseInvariant)?
        .unwrap_or_default();
    let reset_at = sqlx::query_scalar(
        r#"
        SELECT min(rate_lock.expires_at)
        FROM rate_locks AS rate_lock
        JOIN addresses AS address ON address.id = rate_lock.address_id
        WHERE address.account_id = $1
          AND rate_lock.status = 'open'
          AND rate_lock.exposure_reserved
        "#,
    )
    .bind(account_id)
    .fetch_one(pool)
    .await?;
    Ok(ExposureAvailability {
        remaining_minor: cap.saturating_sub(open),
        reset_at,
    })
}

/// Cancels an unpaid open lock, releases exposure, and appends an audit row atomically.
///
/// Any deposit row for the lock address, including a rejected one, means funds already arrived at
/// the single-use address, so the lock is no longer unpaid and cancellation is refused; such a
/// lock stays open until it is consumed or expires.
pub async fn cancel(
    pool: &PgPool,
    product: &Product,
    account: &Account,
    lock_ref: &str,
) -> Result<RateLock, RateLockError> {
    validate_lock_ref(lock_ref)?;
    let mut transaction = pool.begin().await?;
    let row = get_in(&mut transaction, product.id, account.id, lock_ref)
        .await?
        .ok_or(RateLockError::NotFound)?;
    if row.status == RateLockStatus::Cancelled {
        transaction.commit().await?;
        return Ok(row);
    }
    if row.status != RateLockStatus::Open || row.expires_at <= Utc::now() {
        return Err(RateLockError::NotOpen);
    }
    let paid: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM deposits WHERE address_id = $1)")
            .bind(row.address_id)
            .fetch_one(&mut *transaction)
            .await?;
    if paid {
        return Err(RateLockError::PendingPayment);
    }
    let reservation = lock_reservation(&mut transaction, row.address_id).await?;
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
    if reservation.reserved {
        release_exposure(&mut transaction, &reservation).await?;
    }
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

/// Atomically consumes an open lock and releases any associated exposure reservation.
pub(crate) async fn consume(
    transaction: &mut Transaction<'_, Postgres>,
    address_id: Uuid,
    deposit_id: Uuid,
    idempotent: bool,
) -> Result<bool, sqlx::Error> {
    let reservation = match lock_reservation(transaction, address_id).await {
        Ok(reservation) => reservation,
        Err(RateLockError::NotFound) => return Ok(false),
        Err(RateLockError::Database(error)) => return Err(error),
        Err(_) => {
            return Err(sqlx::Error::Protocol(
                "invalid rate-lock reservation".to_owned(),
            ));
        }
    };
    let row =
        sqlx::query("SELECT status, consumed_by FROM rate_locks WHERE address_id = $1 FOR UPDATE")
            .bind(address_id)
            .fetch_optional(&mut **transaction)
            .await?;
    let Some(row) = row else {
        return Ok(false);
    };
    let status: String = row.try_get("status")?;
    let consumed_by: Option<Uuid> = row.try_get("consumed_by")?;
    if status == "consumed" && idempotent && consumed_by == Some(deposit_id) {
        return Ok(true);
    }
    if !matches!(status.as_str(), "open" | "expired") || consumed_by.is_some() {
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
    if reservation.reserved {
        release_exposure(transaction, &reservation)
            .await
            .map_err(rate_lock_sqlx)?;
    }
    Ok(true)
}

/// Expires one bounded batch and returns the number of rows closed.
///
/// Exposure releases are aggregated per scope key and applied in sorted key order, the same order
/// creation, cancellation, and consumption lock scope rows in, so a batch spanning several
/// accounts cannot deadlock with them.
pub async fn expire_once(pool: &PgPool) -> Result<u64, RateLockError> {
    let mut transaction = pool.begin().await?;
    let rows = sqlx::query_as::<_, ExpiringRow>(
        r#"
        SELECT rate_lock.address_id, rate_lock.credit_minor::text AS credit_minor,
               rate_lock.amount_atomic::text AS amount_atomic,
               rate_lock.exposure_reserved, address.account_id, account.product_id,
               account.external_id, address.chain_id, address.address, address.lock_ref,
               rate_lock.route, rate_lock.expires_at
        FROM rate_locks AS rate_lock
        JOIN addresses AS address ON address.id = rate_lock.address_id
        JOIN accounts AS account ON account.id = address.account_id
        WHERE rate_lock.status = 'open'
          AND rate_lock.consumed_by IS NULL
          AND rate_lock.expires_at <= now()
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

    let mut releases = BTreeMap::<String, u64>::new();
    for row in &rows {
        let credit_minor = parse_minor(&row.credit_minor)?;
        if row.exposure_reserved {
            for key in scope_keys(row.account_id, row.product_id) {
                let total = releases.entry(key).or_default();
                *total = total
                    .checked_add(credit_minor.value())
                    .ok_or(RateLockError::Arithmetic)?;
            }
        }
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
    for (key, amount) in releases {
        decrement_exposure(&mut transaction, &key, amount).await?;
    }
    transaction.commit().await?;
    Ok(count)
}

/// In-process counters for the expiry loop heartbeat and work performed.
#[derive(Debug, Default)]
pub struct ExpiryMetrics {
    heartbeats: AtomicU64,
    expired: AtomicU64,
}

impl ExpiryMetrics {
    /// Returns completed expiry scan attempts.
    #[must_use]
    pub fn heartbeats(&self) -> u64 {
        self.heartbeats.load(Ordering::Relaxed)
    }

    /// Returns locks expired by this process.
    #[must_use]
    pub fn expired(&self) -> u64 {
        self.expired.load(Ordering::Relaxed)
    }
}

/// Periodically closes overdue locks and emits expiry events.
pub struct ExpiryWorker {
    pool: PgPool,
    metrics: Arc<ExpiryMetrics>,
    scan_interval: Duration,
}

impl ExpiryWorker {
    /// Creates an expiry worker.
    #[must_use]
    pub const fn new(pool: PgPool, metrics: Arc<ExpiryMetrics>, scan_interval: Duration) -> Self {
        Self {
            pool,
            metrics,
            scan_interval,
        }
    }

    /// Runs expiry scans until cancellation.
    pub async fn run(&self, cancellation: CancellationToken) {
        const LOOP_NAME: &str = "lock_expiry";
        const LOOP_INSTANCE: &str = "0";
        crate::observability::register_loop(LOOP_NAME, LOOP_INSTANCE);
        let mut ticker = interval(self.scan_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return,
                _ = ticker.tick() => {
                    self.metrics.heartbeats.fetch_add(1, Ordering::Relaxed);
                    crate::observability::heartbeat(LOOP_NAME, LOOP_INSTANCE);
                    match expire_once(&self.pool).await {
                        Ok(expired) => {
                            self.metrics.expired.fetch_add(expired, Ordering::Relaxed);
                            crate::observability::progress(LOOP_NAME, LOOP_INSTANCE);
                        }
                        Err(error) => tracing::error!(%error, "rate-lock expiry scan failed"),
                    }
                    crate::observability::waiting(LOOP_NAME, LOOP_INSTANCE, self.scan_interval);
                }
            }
        }
    }
}

fn amounts(
    route: &RouteFile,
    requested: RequestedAmount,
    price: ScaledPrice,
) -> Result<(AtomicAmount, MinorAmount), RateLockError> {
    match requested {
        RequestedAmount::Minor(target) => {
            if target.value() == 0 {
                return Err(RateLockError::InvalidInput(
                    "amount_minor must be greater than zero",
                ));
            }
            let amount = tokens_for_credit(
                target,
                price,
                route.asset.decimals,
                route.destination.unit_decimals,
            )
            .map_err(|_| RateLockError::Arithmetic)?;
            Ok((amount, target))
        }
        RequestedAmount::Atomic(amount) => {
            if amount.value().is_zero() {
                return Err(RateLockError::InvalidInput(
                    "amount_atomic must be greater than zero",
                ));
            }
            let credit = credit(
                amount,
                price,
                route.asset.decimals,
                route.destination.unit_decimals,
            )
            .map_err(|_| RateLockError::Arithmetic)?;
            Ok((amount, credit))
        }
    }
}

fn validate_bounds(
    route: &RouteFile,
    amount: AtomicAmount,
    credit: MinorAmount,
) -> Result<(), RateLockError> {
    if amount < route.screening.min_deposit_atomic || amount > route.screening.max_deposit_atomic {
        return Err(RateLockError::InvalidInput(
            "requested amount is outside route deposit bounds",
        ));
    }
    if credit.value() < route.screening.min_credit_minor {
        return Err(RateLockError::InvalidInput(
            "requested amount is below the minimum credit",
        ));
    }
    Ok(())
}

fn validate_lock_ref(lock_ref: &str) -> Result<(), RateLockError> {
    if lock_ref.is_empty() || lock_ref.len() > 255 {
        return Err(RateLockError::InvalidInput(
            "product_lock_ref must contain 1 to 255 bytes",
        ));
    }
    Ok(())
}

fn replay(existing: RateLock, requested: RequestedAmount) -> Result<RateLock, RateLockError> {
    let matches = match requested {
        RequestedAmount::Minor(credit) => existing.credit_minor == credit,
        RequestedAmount::Atomic(amount) => existing.amount_atomic == amount,
    };
    if matches {
        Ok(existing)
    } else {
        Err(RateLockError::IdempotencyMismatch)
    }
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

async fn reserve_exposure(
    transaction: &mut Transaction<'_, Postgres>,
    product_id: Uuid,
    account_id: Uuid,
    amount: MinorAmount,
    route: &RouteFile,
) -> Result<(), RateLockError> {
    let mut scopes = vec![
        (
            format!("account:{account_id}"),
            route.rate_lock.max_open_minor.account,
            "account",
        ),
        (
            format!("product:{product_id}"),
            route.rate_lock.max_open_minor.product,
            "product",
        ),
        (
            "global".to_owned(),
            route.rate_lock.max_open_minor.global,
            "global",
        ),
    ];
    scopes.sort_by(|left, right| left.0.cmp(&right.0));
    for (key, _, _) in &scopes {
        sqlx::query(
            "INSERT INTO lock_exposure (scope_key, open_minor) VALUES ($1, 0) ON CONFLICT DO NOTHING",
        )
        .bind(key)
        .execute(&mut **transaction)
        .await?;
    }
    let keys = scopes
        .iter()
        .map(|scope| scope.0.clone())
        .collect::<Vec<_>>();
    let rows = sqlx::query(
        "SELECT scope_key, open_minor::text AS open_minor FROM lock_exposure WHERE scope_key = ANY($1) ORDER BY scope_key COLLATE \"C\" FOR UPDATE",
    )
    .bind(&keys)
    .fetch_all(&mut **transaction)
    .await?;
    let open = rows
        .into_iter()
        .map(|row| {
            let key: String = row.try_get("scope_key")?;
            let value: String = row.try_get("open_minor")?;
            let parsed = value.parse::<u64>().map_err(|_| {
                sqlx::Error::Decode("lock exposure is outside u64".to_owned().into())
            })?;
            Ok((key, parsed))
        })
        .collect::<Result<BTreeMap<_, _>, sqlx::Error>>()?;
    for (key, cap, label) in &scopes {
        let current = open
            .get(key)
            .copied()
            .ok_or(RateLockError::DatabaseInvariant)?;
        let next = current
            .checked_add(amount.value())
            .ok_or(RateLockError::ExposureCap(label))?;
        if next > *cap {
            return Err(RateLockError::ExposureCap(label));
        }
    }
    for (key, _, _) in scopes {
        sqlx::query(
            "UPDATE lock_exposure SET open_minor = open_minor + $2::text::numeric, updated_at = now() WHERE scope_key = $1",
        )
        .bind(key)
        .bind(amount.value().to_string())
        .execute(&mut **transaction)
        .await?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Reservation {
    account_id: Uuid,
    product_id: Uuid,
    credit_minor: MinorAmount,
    reserved: bool,
}

async fn lock_reservation(
    transaction: &mut Transaction<'_, Postgres>,
    address_id: Uuid,
) -> Result<Reservation, RateLockError> {
    let row = sqlx::query(
        r#"
        SELECT address.account_id, account.product_id,
               rate_lock.credit_minor::text AS credit_minor,
               rate_lock.exposure_reserved
        FROM rate_locks AS rate_lock
        JOIN addresses AS address ON address.id = rate_lock.address_id
        JOIN accounts AS account ON account.id = address.account_id
        WHERE rate_lock.address_id = $1
        FOR UPDATE OF rate_lock
        "#,
    )
    .bind(address_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or(RateLockError::NotFound)?;
    let credit: String = row.try_get("credit_minor")?;
    Ok(Reservation {
        account_id: row.try_get("account_id")?,
        product_id: row.try_get("product_id")?,
        credit_minor: parse_minor(&credit)?,
        reserved: row.try_get("exposure_reserved")?,
    })
}

async fn release_exposure(
    transaction: &mut Transaction<'_, Postgres>,
    reservation: &Reservation,
) -> Result<(), RateLockError> {
    for key in scope_keys(reservation.account_id, reservation.product_id) {
        decrement_exposure(transaction, &key, reservation.credit_minor.value()).await?;
    }
    Ok(())
}

/// Returns the exposure scope keys for one lock in the byte order every writer locks them in.
fn scope_keys(account_id: Uuid, product_id: Uuid) -> [String; 3] {
    let mut keys = [
        format!("account:{account_id}"),
        format!("product:{product_id}"),
        "global".to_owned(),
    ];
    keys.sort();
    keys
}

async fn decrement_exposure(
    transaction: &mut Transaction<'_, Postgres>,
    key: &str,
    amount: u64,
) -> Result<(), RateLockError> {
    let result = sqlx::query(
        r#"
        UPDATE lock_exposure
        SET open_minor = open_minor - $2::text::numeric, updated_at = now()
        WHERE scope_key = $1 AND open_minor >= $2::text::numeric
        "#,
    )
    .bind(key)
    .bind(amount.to_string())
    .execute(&mut **transaction)
    .await?;
    if result.rows_affected() != 1 {
        return Err(RateLockError::DatabaseInvariant);
    }
    Ok(())
}

#[derive(FromRow)]
struct RateLockRow {
    address_id: Uuid,
    lock_ref: String,
    chain_id: i64,
    address: String,
    amount_atomic: String,
    price_scaled: String,
    credit_minor: String,
    expires_at: DateTime<Utc>,
    status: String,
}

impl TryFrom<RateLockRow> for RateLock {
    type Error = RateLockError;

    fn try_from(row: RateLockRow) -> Result<Self, Self::Error> {
        Ok(Self {
            address_id: row.address_id,
            lock_ref: row.lock_ref,
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
        })
    }
}

async fn get_in(
    transaction: &mut Transaction<'_, Postgres>,
    product_id: Uuid,
    account_id: Uuid,
    lock_ref: &str,
) -> Result<Option<RateLock>, RateLockError> {
    let row = sqlx::query_as::<_, RateLockRow>(
        r#"
        SELECT rate_lock.address_id, address.lock_ref, address.chain_id, address.address,
               rate_lock.amount_atomic::text AS amount_atomic,
               rate_lock.price_scaled::text AS price_scaled,
               rate_lock.credit_minor::text AS credit_minor,
               rate_lock.expires_at, rate_lock.status
        FROM rate_locks AS rate_lock
        JOIN addresses AS address ON address.id = rate_lock.address_id
        JOIN accounts AS account ON account.id = address.account_id
        WHERE account.product_id = $1 AND account.id = $2 AND address.lock_ref = $3
        FOR UPDATE OF rate_lock
        "#,
    )
    .bind(product_id)
    .bind(account_id)
    .bind(lock_ref)
    .fetch_optional(&mut **transaction)
    .await?;
    row.map(TryInto::try_into).transpose()
}

#[derive(FromRow)]
struct ExpiringRow {
    address_id: Uuid,
    credit_minor: String,
    amount_atomic: String,
    exposure_reserved: bool,
    account_id: Uuid,
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

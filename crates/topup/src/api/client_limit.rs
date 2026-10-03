//! Limits of anonymous reads of a quote or deposit address by `client_secret`.
//!
//! A secret carries its own tag ([`crate::client_secret`]), checked in memory first: a forged or
//! malformed secret is `404` with no database work and no budget charged. A genuine one is then
//! charged to its object's budget of [`PER_OBJECT`] reads per minute, still before the database,
//! and takes one of a fixed number of slots, a share of the database pool, for its database work,
//! which [`READ_TIMEOUT`] bounds. The budget is a GCRA, like the API's rate limits
//! ([`super::rate_limit`]): a burst of up to all of it, refilled at its rate, so no window boundary
//! admits a second budget right after the first. State is held in this process; the service runs
//! one instance.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::sync::{Semaphore, SemaphorePermit};
use uuid::Uuid;

use super::error::ApiError;
use super::rate_limit::{Clock, PRUNE_ABOVE, trial_admission};
use crate::client_secret::ClientSecretKey;

const MINUTE: Duration = Duration::from_secs(60);
/// Reads of one quote or deposit address per minute: a page polling every second, twice over.
const PER_OBJECT: u32 = 120;
/// How long a read waits for a slot before it is refused with `Retry-After: 1`.
const SLOT_WAIT: Duration = Duration::from_millis(250);
/// The database work of one read, after which it is refused with `503`.
pub const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// Slots of a limiter built without a pool size ([`Default`]).
const DEFAULT_SLOTS: usize = 4;

/// The client-secret key, and the budgets of anonymous reads by `client_secret`.
pub struct ClientReadLimiter {
    key: ClientSecretKey,
    slots: Semaphore,
    clock: Clock,
    /// Theoretical arrival time of each object's next read.
    reads: Mutex<HashMap<Uuid, Instant>>,
}

impl std::fmt::Debug for ClientReadLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientReadLimiter")
            .field("key", &self.key)
            .field("slots", &self.slots)
            .field("reads", &self.reads)
            .finish_non_exhaustive()
    }
}

/// A limiter with an ephemeral key ([`ClientSecretKey::ephemeral`]), for tests.
impl Default for ClientReadLimiter {
    fn default() -> Self {
        Self::with_clock(Instant::now)
    }
}

impl ClientReadLimiter {
    /// A limiter tagging secrets with `key` whose reads hold at most half of a pool of
    /// `pool_connections`, so merchant requests and the workers keep the rest.
    #[must_use]
    pub fn new(key: ClientSecretKey, pool_connections: u32) -> Self {
        let slots = usize::try_from(pool_connections / 2).unwrap_or(1).max(1);
        Self::build(key, slots, Box::new(Instant::now))
    }

    /// A limiter with an ephemeral key whose budgets run on the instants `clock` reads, so a test
    /// decides how much time passes between its reads instead of the machine it runs on.
    #[must_use]
    pub fn with_clock(clock: impl Fn() -> Instant + Send + Sync + 'static) -> Self {
        Self::build(ClientSecretKey::ephemeral(), DEFAULT_SLOTS, Box::new(clock))
    }

    fn build(key: ClientSecretKey, slots: usize, clock: Clock) -> Self {
        Self {
            key,
            slots: Semaphore::new(slots),
            clock,
            reads: Mutex::default(),
        }
    }

    /// The key that issues and checks client secrets.
    #[must_use]
    pub const fn key(&self) -> &ClientSecretKey {
        &self.key
    }

    /// Admits a read of `object`, whose public id is `id`, by `secret`: `404` unless the secret
    /// carries its tag, `429` over the object's budget or when no slot frees within
    /// [`SLOT_WAIT`]. The returned permit holds the slot until dropped.
    pub async fn admit(
        &self,
        id: &str,
        object: Uuid,
        secret: &str,
    ) -> Result<SemaphorePermit<'_>, ApiError> {
        if !self.key.verify(id, secret) {
            return Err(ApiError::not_found());
        }
        self.charge_at((self.clock)(), object)
            .map_err(ApiError::client_reads_limited)?;
        match tokio::time::timeout(SLOT_WAIT, self.slots.acquire()).await {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) | Err(_) => Err(ApiError::client_reads_limited(1)),
        }
    }

    /// Counts one read of `object`; `Err` holds the seconds until a read would be admitted, for a
    /// read over its budget, which is not counted.
    fn charge_at(&self, now: Instant, object: Uuid) -> Result<(), u64> {
        let mut reads = self
            .reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match trial_admission(reads.get(&object).copied(), now, PER_OBJECT, MINUTE) {
            Ok(next) => {
                if reads.len() >= PRUNE_ABOVE {
                    reads.retain(|_, next| *next > now);
                }
                reads.insert(object, next);
                Ok(())
            }
            Err(wait) => Err(wait
                .as_secs()
                .saturating_add(u64::from(wait.subsec_nanos() > 0))),
        }
    }
}

/// Runs a read's database work within [`READ_TIMEOUT`], answering `503` past it.
pub async fn bounded<T>(work: impl Future<Output = Result<T, ApiError>>) -> Result<T, ApiError> {
    tokio::time::timeout(READ_TIMEOUT, work)
        .await
        .unwrap_or_else(|_| Err(ApiError::service_unavailable("the read timed out")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admitted(limiter: &ClientReadLimiter, now: Instant, object: Uuid, reads: u32) -> u32 {
        (0..reads)
            .filter(|_| limiter.charge_at(now, object).is_ok())
            .count()
            .try_into()
            .unwrap()
    }

    #[test]
    fn limits_each_object_per_minute() {
        let limiter = ClientReadLimiter::default();
        let start = Instant::now();
        let quote = Uuid::from_u128(1);
        assert_eq!(admitted(&limiter, start, quote, PER_OBJECT), PER_OBJECT);
        // The next read is half a second away, at the budget's rate of two a second.
        assert_eq!(limiter.charge_at(start, quote), Err(1));
        assert_eq!(admitted(&limiter, start, Uuid::from_u128(2), 1), 1);
        assert_eq!(
            admitted(&limiter, start + Duration::from_secs(1), quote, 10),
            2
        );
        assert_eq!(
            admitted(&limiter, start + MINUTE * 2, quote, PER_OBJECT * 2),
            PER_OBJECT
        );
    }

    #[test]
    fn a_minute_boundary_admits_no_second_burst() {
        let limiter = ClientReadLimiter::default();
        let start = Instant::now();
        let quote = Uuid::from_u128(1);
        let late = start + MINUTE - Duration::from_millis(1);
        assert_eq!(admitted(&limiter, late, quote, PER_OBJECT), PER_OBJECT);
        // A millisecond later, across where a fixed window would reset, the budget is spent.
        assert_eq!(admitted(&limiter, start + MINUTE, quote, PER_OBJECT), 0);
    }

    #[tokio::test]
    async fn forged_or_wrong_object_secrets_consume_no_read_budget() {
        use axum::response::IntoResponse as _;
        let now = Instant::now();
        let limiter = ClientReadLimiter::with_clock(move || now);
        let object = Uuid::from_u128(1);
        let id = format!("quo_{}", object.simple());
        let valid = limiter.key().issue("acct_test", &id).unwrap();
        let forged = ClientSecretKey::ephemeral()
            .issue("acct_test", &id)
            .unwrap();
        let other_object = limiter.key().issue("acct_test", "quo_other").unwrap();
        for _ in 0..PER_OBJECT {
            for secret in [&forged, &other_object] {
                let error = limiter.admit(&id, object, secret).await.unwrap_err();
                assert_eq!(
                    error.into_response().status(),
                    axum::http::StatusCode::NOT_FOUND
                );
            }
        }
        for _ in 0..PER_OBJECT {
            let permit = limiter.admit(&id, object, &valid).await.unwrap();
            drop(permit);
        }
        let error = limiter.admit(&id, object, &valid).await.unwrap_err();
        assert_eq!(
            error.into_response().status(),
            axum::http::StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[test]
    fn slots_are_half_the_pool() {
        let key = ClientSecretKey::ephemeral;
        assert_eq!(
            ClientReadLimiter::new(key(), 14).slots.available_permits(),
            7
        );
        assert_eq!(
            ClientReadLimiter::new(key(), 1).slots.available_permits(),
            1
        );
    }
}

//! Limits of anonymous reads of a quote or deposit address by `client_secret`.
//!
//! A secret carries its own tag ([`crate::client_secret`]), checked in memory first: a forged or
//! malformed secret is `404` with no database work and no budget charged. A genuine one is then
//! charged to its object's budget of [`PER_OBJECT`] reads per one-minute window, still before the
//! database, and takes one of a fixed number of slots, a share of the database pool, for its
//! database work, which [`READ_TIMEOUT`] bounds. State is held in this process; the service runs
//! one instance.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::sync::{Semaphore, SemaphorePermit};
use uuid::Uuid;

use super::error::ApiError;
use crate::client_secret::ClientSecretKey;

const WINDOW: Duration = Duration::from_secs(60);
/// Reads of one quote or deposit address per window: a page polling every second, twice over.
const PER_OBJECT: u32 = 120;
/// How long a read waits for a slot before it is refused with `Retry-After: 1`.
const SLOT_WAIT: Duration = Duration::from_millis(250);
/// The database work of one read, after which it is refused with `503`.
pub const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// Slots of a limiter built without a pool size ([`Default`]).
const DEFAULT_SLOTS: usize = 4;

/// The client-secret key, and the budgets of anonymous reads by `client_secret`.
#[derive(Debug)]
pub struct ClientReadLimiter {
    key: ClientSecretKey,
    slots: Semaphore,
    window: Mutex<Window>,
}

#[derive(Debug)]
struct Window {
    started: Instant,
    reads: HashMap<Uuid, u32>,
}

/// A limiter with an ephemeral key ([`ClientSecretKey::ephemeral`]), for tests.
impl Default for ClientReadLimiter {
    fn default() -> Self {
        Self::with_slots(ClientSecretKey::ephemeral(), DEFAULT_SLOTS)
    }
}

impl ClientReadLimiter {
    /// A limiter tagging secrets with `key` whose reads hold at most half of a pool of
    /// `pool_connections`, so merchant requests and the workers keep the rest.
    #[must_use]
    pub fn new(key: ClientSecretKey, pool_connections: u32) -> Self {
        let slots = usize::try_from(pool_connections / 2).unwrap_or(1).max(1);
        Self::with_slots(key, slots)
    }

    fn with_slots(key: ClientSecretKey, slots: usize) -> Self {
        Self {
            key,
            slots: Semaphore::new(slots),
            window: Mutex::new(Window {
                started: Instant::now(),
                reads: HashMap::new(),
            }),
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
        self.charge_at(Instant::now(), object)
            .map_err(ApiError::client_reads_limited)?;
        match tokio::time::timeout(SLOT_WAIT, self.slots.acquire()).await {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(_)) | Err(_) => Err(ApiError::client_reads_limited(1)),
        }
    }

    /// Counts one read of `object`; `Err` holds the seconds until the window ends, for a read over
    /// its budget, which is not counted.
    fn charge_at(&self, now: Instant, object: Uuid) -> Result<(), u64> {
        let mut window = self
            .window
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if now.saturating_duration_since(window.started) >= WINDOW {
            window.started = now;
            window.reads.clear();
        }
        let reads = window.reads.entry(object).or_default();
        if *reads >= PER_OBJECT {
            let left = WINDOW.saturating_sub(now.saturating_duration_since(window.started));
            return Err(left
                .as_secs()
                .saturating_add(u64::from(left.subsec_nanos() > 0)));
        }
        *reads += 1;
        Ok(())
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

    #[test]
    fn limits_each_object_per_window() {
        let limiter = ClientReadLimiter::default();
        let start = Instant::now();
        let quote = Uuid::from_u128(1);
        for _ in 0..PER_OBJECT {
            assert!(limiter.charge_at(start, quote).is_ok());
        }
        assert_eq!(limiter.charge_at(start, quote), Err(60));
        assert!(limiter.charge_at(start, Uuid::from_u128(2)).is_ok());
        assert!(limiter.charge_at(start + WINDOW, quote).is_ok());
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

//! Limits of anonymous reads of a quote or deposit address by `client_secret`.
//!
//! A secret is random and only its SHA-256 is stored, so verifying one is a single indexed lookup
//! and nothing cheaper exists. A read is charged to its object's and its account and mode's
//! budgets only once its secret is verified, so forged secrets never spend the budget of real
//! ones. Reads whose secret matches nothing have their own budget: once it is spent, a secret
//! this process has not issued or verified in this window or the one before is refused before
//! its lookup until the window ends, while known secrets keep being served. At most
//! [`IN_FLIGHT`] reads run at once, whatever their secrets, which bounds the database work of the
//! surface. Budgets are fixed one-minute windows held in this process; the service runs one
//! instance.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::{Semaphore, SemaphorePermit};
use uuid::Uuid;

use crate::tenancy::Scope;

const WINDOW: Duration = Duration::from_secs(60);
/// Verified reads of one quote or address per window: a page polling every second, twice over.
const PER_OBJECT: u32 = 120;
/// Verified reads of one account and mode's objects per window.
const PER_SCOPE: u32 = 6_000;
/// Reads per window whose secret matched nothing.
const FAILED: u32 = 1_200;
/// Reads served at once, a share of the API's connections.
const IN_FLIGHT: usize = 4;
/// Secrets remembered per window; one beyond it is looked up as an unknown secret.
const KNOWN: usize = 100_000;
/// Seconds a read refused for want of a slot waits.
const BUSY_RETRY_AFTER: u64 = 1;

/// The SHA-256 of a `client_secret`, as stored.
pub type SecretHash = [u8; 32];

/// Budgets of anonymous reads by `client_secret`.
#[derive(Debug)]
pub struct ClientReadLimiter {
    in_flight: Semaphore,
    state: Mutex<Window>,
}

#[derive(Debug)]
struct Window {
    started: Instant,
    failed: u32,
    per_object: HashMap<Uuid, u32>,
    per_scope: HashMap<Scope, u32>,
    /// Secrets issued or verified in this window.
    known: HashSet<SecretHash>,
    /// Secrets issued or verified in the window before.
    previous: HashSet<SecretHash>,
}

impl Default for ClientReadLimiter {
    fn default() -> Self {
        Self {
            in_flight: Semaphore::new(IN_FLIGHT),
            state: Mutex::new(Window {
                started: Instant::now(),
                failed: 0,
                per_object: HashMap::new(),
                per_scope: HashMap::new(),
                known: HashSet::new(),
                previous: HashSet::new(),
            }),
        }
    }
}

impl ClientReadLimiter {
    /// Admits a read by the secret whose hash is `secret`, before its lookup: the returned permit
    /// holds one of the [`IN_FLIGHT`] slots until dropped. `Err` holds the seconds to wait: no
    /// slot is free, or the secret is unknown and the window's failed reads are spent.
    pub fn begin(&self, secret: &SecretHash) -> Result<SemaphorePermit<'_>, u64> {
        let permit = self.in_flight.try_acquire().map_err(|_| BUSY_RETRY_AFTER)?;
        self.admit_at(Instant::now(), secret)?;
        Ok(permit)
    }

    /// Charges a read whose secret verified for `object` of `scope`; `Err` holds the seconds until
    /// the window ends, for a read over its object's or scope's budget. A refused read is not
    /// charged.
    pub fn verified(&self, secret: SecretHash, object: Uuid, scope: Scope) -> Result<(), u64> {
        self.verified_at(Instant::now(), secret, object, scope)
    }

    /// Charges a read whose secret matched nothing to the failed reads' budget, and forgets the
    /// secret, which may have been revoked.
    pub fn failed(&self, secret: &SecretHash) {
        let mut window = self.window(Instant::now());
        window.failed = window.failed.saturating_add(1);
        window.known.remove(secret);
        window.previous.remove(secret);
    }

    /// Remembers a newly issued secret, so its page's first read is known.
    pub fn issued(&self, secret: SecretHash) {
        self.window(Instant::now()).remember(secret);
    }

    fn admit_at(&self, now: Instant, secret: &SecretHash) -> Result<(), u64> {
        let window = self.window(now);
        if window.failed < FAILED
            || window.known.contains(secret)
            || window.previous.contains(secret)
        {
            return Ok(());
        }
        Err(window.seconds_left(now))
    }

    fn verified_at(
        &self,
        now: Instant,
        secret: SecretHash,
        object: Uuid,
        scope: Scope,
    ) -> Result<(), u64> {
        let mut window = self.window(now);
        window.remember(secret);
        let object_reads = window.per_object.get(&object).copied().unwrap_or(0);
        let scope_reads = window.per_scope.get(&scope).copied().unwrap_or(0);
        if object_reads >= PER_OBJECT || scope_reads >= PER_SCOPE {
            return Err(window.seconds_left(now));
        }
        window.per_object.insert(object, object_reads + 1);
        window.per_scope.insert(scope, scope_reads + 1);
        Ok(())
    }

    /// The current window, started anew once [`WINDOW`] has passed.
    fn window(&self, now: Instant) -> MutexGuard<'_, Window> {
        let mut window = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if now.saturating_duration_since(window.started) >= WINDOW {
            window.started = now;
            window.failed = 0;
            window.per_object.clear();
            window.per_scope.clear();
            window.previous = std::mem::take(&mut window.known);
        }
        window
    }
}

impl Window {
    fn remember(&mut self, secret: SecretHash) {
        if self.known.len() < KNOWN {
            self.known.insert(secret);
        }
    }

    /// Whole seconds until the window ends, rounded up.
    fn seconds_left(&self, now: Instant) -> u64 {
        let left = WINDOW.saturating_sub(now.saturating_duration_since(self.started));
        left.as_secs()
            .saturating_add(u64::from(left.subsec_nanos() > 0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(account: u128) -> Scope {
        Scope::new(Uuid::from_u128(account), true)
    }

    fn secret(value: u32) -> SecretHash {
        let mut secret = [0; 32];
        secret[..4].copy_from_slice(&value.to_be_bytes());
        secret
    }

    #[test]
    fn limits_each_object_and_scope_per_window() {
        let limiter = ClientReadLimiter::default();
        let start = Instant::now();
        let quote = Uuid::from_u128(1);
        for _ in 0..PER_OBJECT {
            assert!(
                limiter
                    .verified_at(start, secret(1), quote, scope(1))
                    .is_ok()
            );
        }
        assert_eq!(
            limiter.verified_at(start, secret(1), quote, scope(1)),
            Err(60)
        );
        assert!(
            limiter
                .verified_at(start, secret(2), Uuid::from_u128(2), scope(1))
                .is_ok()
        );
        assert!(
            limiter
                .verified_at(start + WINDOW, secret(1), quote, scope(1))
                .is_ok()
        );

        let later = start + WINDOW * 2;
        for index in 0..PER_SCOPE {
            let object = Uuid::from_u128(u128::from(index) + 10);
            assert!(
                limiter
                    .verified_at(later, secret(index), object, scope(1))
                    .is_ok()
            );
        }
        let object = Uuid::from_u128(3);
        assert!(
            limiter
                .verified_at(later, secret(3), object, scope(1))
                .is_err()
        );
        // Another account's budget is its own.
        assert!(
            limiter
                .verified_at(later, secret(3), object, scope(2))
                .is_ok()
        );
    }

    #[test]
    fn failed_reads_never_spend_verified_budgets_and_only_refuse_unknown_secrets() {
        let limiter = ClientReadLimiter::default();
        let start = Instant::now();
        let quote = Uuid::from_u128(1);
        // A page read before the forgeries, and a secret issued to a new page.
        assert!(
            limiter
                .verified_at(start, secret(1), quote, scope(1))
                .is_ok()
        );
        limiter.issued(secret(2));
        for index in 0..FAILED {
            assert!(limiter.admit_at(start, &secret(index + 100)).is_ok());
            limiter.failed(&secret(index + 100));
        }
        assert!(limiter.admit_at(start, &secret(99)).is_err());
        assert!(limiter.admit_at(start, &secret(1)).is_ok());
        assert!(limiter.admit_at(start, &secret(2)).is_ok());
        assert!(
            limiter
                .verified_at(start, secret(1), quote, scope(1))
                .is_ok()
        );

        // Known secrets carry over one window; the failed reads' budget starts anew.
        let next = start + WINDOW;
        assert!(limiter.admit_at(next, &secret(99)).is_ok());
        assert!(limiter.admit_at(next, &secret(1)).is_ok());
        // A secret that stops matching, as a pruned one, is forgotten.
        limiter.failed(&secret(1));
        assert!(!limiter.window(next).previous.contains(&secret(1)));
    }

    #[tokio::test]
    async fn bounds_reads_in_flight() {
        let limiter = ClientReadLimiter::default();
        let permits: Vec<_> = (0..IN_FLIGHT)
            .map(|_| limiter.begin(&secret(1)).expect("a free slot"))
            .collect();
        assert_eq!(limiter.begin(&secret(1)).err(), Some(BUSY_RETRY_AFTER));
        drop(permits);
        assert!(limiter.begin(&secret(1)).is_ok());
    }
}

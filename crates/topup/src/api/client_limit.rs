//! Rate limit of unsigned quote reads by `client_secret`.
//!
//! The public read has no product identity to charge, so it is limited per quote and in total,
//! in fixed one-minute windows held in this process. The total bounds the database load and the
//! size of the per-quote table; the per-quote limit keeps one checkout page from using it up.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use uuid::Uuid;

const WINDOW: Duration = Duration::from_secs(60);
/// Reads of one quote per window: a page polling every second, twice over.
const PER_QUOTE: u32 = 120;
/// Reads of all quotes per window.
const TOTAL: u32 = 6_000;

/// Fixed-window counters of unsigned quote reads.
#[derive(Debug)]
pub struct ClientReadLimiter {
    state: Mutex<Window>,
}

#[derive(Debug)]
struct Window {
    started: Instant,
    total: u32,
    per_quote: HashMap<Uuid, u32>,
}

impl Default for ClientReadLimiter {
    fn default() -> Self {
        Self {
            state: Mutex::new(Window {
                started: Instant::now(),
                total: 0,
                per_quote: HashMap::new(),
            }),
        }
    }
}

impl ClientReadLimiter {
    /// Counts one read of `quote`; `false` means the read is over a limit and must be refused.
    pub fn allow(&self, quote: Uuid) -> bool {
        self.allow_at(Instant::now(), quote)
    }

    fn allow_at(&self, now: Instant, quote: Uuid) -> bool {
        let mut window = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if now.saturating_duration_since(window.started) >= WINDOW {
            window.started = now;
            window.total = 0;
            window.per_quote.clear();
        }
        if window.total >= TOTAL {
            return false;
        }
        let count = window.per_quote.entry(quote).or_default();
        if *count >= PER_QUOTE {
            return false;
        }
        *count += 1;
        window.total += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_each_quote_and_the_total_per_window() {
        let limiter = ClientReadLimiter::default();
        let start = Instant::now();
        let quote = Uuid::from_u128(1);
        for _ in 0..PER_QUOTE {
            assert!(limiter.allow_at(start, quote));
        }
        assert!(!limiter.allow_at(start, quote));
        assert!(limiter.allow_at(start, Uuid::from_u128(2)));
        assert!(limiter.allow_at(start + WINDOW, quote));

        let later = start + WINDOW * 2;
        for index in 0..TOTAL {
            assert!(limiter.allow_at(later, Uuid::from_u128(u128::from(index) + 10)));
        }
        assert!(!limiter.allow_at(later, Uuid::from_u128(3)));
    }
}

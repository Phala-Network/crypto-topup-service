//! Entropy for full-jitter retry scheduling.

use rand::TryRngCore as _;
use rand::rngs::OsRng;

/// Entropy source used by full-jitter retry scheduling.
pub trait JitterSource: Send + Sync {
    /// Returns one value spanning the complete `u64` range.
    fn next_u64(&self) -> u64;
}

/// Operating-system entropy used by production retry loops.
pub struct OsJitter;

impl JitterSource for OsJitter {
    /// Falls back to zero, the unjittered and therefore longest delay, if the OS RNG fails.
    fn next_u64(&self) -> u64 {
        OsRng.try_next_u64().unwrap_or_else(|error| {
            tracing::warn!(%error, "OS RNG failed; using unjittered retry delay");
            0
        })
    }
}

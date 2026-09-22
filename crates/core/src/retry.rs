//! Pure retry scheduling.

use std::time::Duration;

const INITIAL_SECONDS: u64 = 30;
const MAX_SECONDS: u64 = 60 * 60;
const TWO_TO_64: u128 = 18_446_744_073_709_551_616;

/// Returns a full-jitter exponential backoff for a zero-based attempt number.
///
/// The unjittered ceiling starts at 30 seconds, doubles per attempt, and caps at
/// one hour. `jitter` is caller-provided entropy spanning the complete `u64`
/// range; the returned delay is uniformly mapped to the inclusive range from
/// zero through the current ceiling.
#[must_use]
pub fn backoff(attempt: u32, jitter: u64) -> Duration {
    let ceiling = INITIAL_SECONDS
        .saturating_mul(2_u64.saturating_pow(attempt))
        .min(MAX_SECONDS);
    let range = u128::from(ceiling.saturating_add(1));
    let scaled = u128::from(jitter)
        .saturating_mul(range)
        .div_euclid(TWO_TO_64);
    let seconds = match u64::try_from(scaled) {
        Ok(value) => value,
        Err(_) => ceiling,
    };

    Duration::from_secs(seconds)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn schedule_starts_at_thirty_seconds_and_caps_at_one_hour() {
        assert_eq!(backoff(0, u64::MAX), Duration::from_secs(30));
        assert_eq!(backoff(1, u64::MAX), Duration::from_secs(60));
        assert_eq!(backoff(6, u64::MAX), Duration::from_secs(1_920));
        assert_eq!(backoff(7, u64::MAX), Duration::from_secs(3_600));
        assert_eq!(backoff(u32::MAX, u64::MAX), Duration::from_secs(3_600));
    }

    proptest! {
        #[test]
        fn backoff_is_monotone_and_bounded(attempt in any::<u32>(), jitter in any::<u64>()) {
            let current = backoff(attempt, jitter);
            let next_attempt = attempt.saturating_add(1);
            let next = backoff(next_attempt, jitter);

            prop_assert!(current >= Duration::ZERO);
            prop_assert!(current <= Duration::from_secs(MAX_SECONDS));
            prop_assert!(next >= current);
            prop_assert!(next <= Duration::from_secs(MAX_SECONDS));
        }
    }
}

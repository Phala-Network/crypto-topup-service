//! Pure retry scheduling.

use std::time::Duration;

const INITIAL_SECONDS: u64 = 30;
const MAX_SECONDS: u64 = 60 * 60;
const TWO_TO_64: u128 = 18_446_744_073_709_551_616;

/// Returns a full-jitter exponential backoff for a zero-based retry attempt.
///
/// `attempt` is the zero-based index of failed retries within the current state;
/// zero schedules the first failed retry. The pump owns this counter: waiting
/// leaves it unchanged and advancing resets it to zero. The unjittered ceiling
/// starts at 30 seconds, doubles per attempt, and caps at one hour.
///
/// `jitter` is caller-provided entropy spanning the complete `u64` range. Zero
/// selects the current ceiling and `u64::MAX` selects zero; intermediate values
/// are uniformly mapped to the inclusive range between them.
#[must_use]
pub fn backoff(attempt: u32, jitter: u64) -> Duration {
    let ceiling = INITIAL_SECONDS
        .saturating_mul(2_u64.saturating_pow(attempt))
        .min(MAX_SECONDS);
    let range = u128::from(ceiling.saturating_add(1));
    let jitter_position = u64::MAX.saturating_sub(jitter);
    let scaled = u128::from(jitter_position)
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
    fn zero_entropy_uses_the_unjittered_exponential_sequence() {
        assert_eq!(backoff(0, 0), Duration::from_secs(30));
        assert_eq!(backoff(1, 0), Duration::from_secs(60));
        assert_eq!(backoff(2, 0), Duration::from_secs(120));
        assert_eq!(backoff(3, 0), Duration::from_secs(240));
        assert_eq!(backoff(4, 0), Duration::from_secs(480));
        assert_eq!(backoff(5, 0), Duration::from_secs(960));
        assert_eq!(backoff(6, 0), Duration::from_secs(1_920));
        assert_eq!(backoff(7, 0), Duration::from_secs(3_600));
        assert_eq!(backoff(u32::MAX, 0), Duration::from_secs(3_600));
    }

    #[test]
    fn maximum_entropy_uses_the_minimum_full_jitter_delay() {
        assert_eq!(backoff(0, u64::MAX), Duration::ZERO);
        assert_eq!(backoff(1, u64::MAX), Duration::ZERO);
        assert_eq!(backoff(2, u64::MAX), Duration::ZERO);
        assert_eq!(backoff(3, u64::MAX), Duration::ZERO);
        assert_eq!(backoff(4, u64::MAX), Duration::ZERO);
        assert_eq!(backoff(5, u64::MAX), Duration::ZERO);
        assert_eq!(backoff(6, u64::MAX), Duration::ZERO);
        assert_eq!(backoff(7, u64::MAX), Duration::ZERO);
        assert_eq!(backoff(u32::MAX, u64::MAX), Duration::ZERO);
    }

    #[test]
    fn midpoint_entropy_scales_each_full_jitter_range() {
        let midpoint = u64::MAX.div_euclid(2);

        assert_eq!(backoff(0, midpoint), Duration::from_secs(15));
        assert_eq!(backoff(1, midpoint), Duration::from_secs(30));
        assert_eq!(backoff(2, midpoint), Duration::from_secs(60));
        assert_eq!(backoff(3, midpoint), Duration::from_secs(120));
        assert_eq!(backoff(4, midpoint), Duration::from_secs(240));
        assert_eq!(backoff(5, midpoint), Duration::from_secs(480));
        assert_eq!(backoff(6, midpoint), Duration::from_secs(960));
        assert_eq!(backoff(7, midpoint), Duration::from_secs(1_800));
        assert_eq!(backoff(u32::MAX, midpoint), Duration::from_secs(1_800));
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

//! Backing off a provider read the provider refused for now.
//!
//! Every task of the service starts at once and shares each chain's provider A, so the first
//! finalized backstop pass and reconciliation round after a restart meet that startup burst, and a
//! public gateway answers the excess with HTTP 429 or JSON-RPC `-32005` (architecture §8, §13).

use std::future::Future;
use std::time::Duration;

use topup_adapters::chain::evm::ChainError;

use crate::jitter::{JitterSource as _, OsJitter};

/// Retries of one read that the provider refused for now.
const RATE_LIMIT_RETRIES: u32 = 6;
/// Ceiling of the first retry delay; it doubles per retry, so all retries wait at most 32 s.
const RATE_LIMIT_BACKOFF: Duration = Duration::from_millis(500);

/// Runs one provider read, retrying it while the provider refuses it for now.
///
/// Backing off within the pass or round (exponential, equal jitter) turns such a refusal into a
/// short delay instead of a failed pass. Every other failure, and a refusal that outlasts the
/// retries, fails the read.
pub(crate) async fn backing_off<T, Read, Attempt>(mut read: Read) -> Result<T, ChainError>
where
    Read: FnMut() -> Attempt,
    Attempt: Future<Output = Result<T, ChainError>>,
{
    let mut retry = 0;
    loop {
        match read().await {
            Err(error) if error.is_rate_limited() && retry < RATE_LIMIT_RETRIES => {
                let delay = retry_delay(retry, OsJitter.next_u64());
                tracing::warn!(
                    %error,
                    retry,
                    delay_ms = delay.as_millis(),
                    "provider refused a read for now; retrying"
                );
                tokio::time::sleep(delay).await;
                retry += 1;
            }
            result => return result,
        }
    }
}

/// Returns a delay between half and all of the retry's ceiling, `RATE_LIMIT_BACKOFF * 2^retry`.
fn retry_delay(retry: u32, jitter: u64) -> Duration {
    let ceiling = RATE_LIMIT_BACKOFF.saturating_mul(2_u32.saturating_pow(retry));
    let half = ceiling / 2;
    let spread = u64::try_from(half.as_millis()).unwrap_or(u64::MAX);
    half.saturating_add(Duration::from_millis(jitter % spread.saturating_add(1)))
}

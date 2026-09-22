//! Concurrent deposit state-machine pumps and state-age alerts.
//!
//! A step panic is not caught: release builds abort the process, the outstanding lease expires,
//! and another pump re-claims the deposit after the process restarts. Step timeouts remain durable
//! retry outcomes.

mod age;

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;
use topup_core::deposit::{
    DepositState, RejectReason, RetryError, StepOutcome, TransitionKind, WaitReason, next,
};
use topup_core::retry::backoff;
use uuid::Uuid;

use crate::db::{
    self, ApplyTransitionError, ApplyTransitionResult, Deposit, OutboxEvent, TransitionUpdate,
};

pub use age::{AgeAlertConfig, AgeAlertConfigError, AgeAlerter, PumpMetrics};

const LEASE_DURATION: Duration = Duration::from_secs(5 * 60);

/// One asynchronous operation for a non-terminal deposit state.
#[async_trait]
pub trait Step: Send + Sync {
    /// Runs the state-specific operation and returns its atomic persistence result.
    async fn run(&self, deposit: &Deposit) -> StepResult;
}

/// State-machine outcome and the evidence and events committed with it.
#[derive(Clone, Debug, PartialEq)]
pub struct StepResult {
    /// Domain outcome used by [`topup_core::deposit::next`].
    pub outcome: StepOutcome,
    /// Evidence written to the transition timeline.
    pub evidence: Value,
    /// Outbox events committed in the same transaction as the transition.
    pub events: Vec<OutboxEvent>,
    /// Additional database writes committed atomically with the transition.
    pub effects: db::TransitionEffects,
}

impl StepResult {
    /// Creates a result without outbox events.
    #[must_use]
    pub const fn new(outcome: StepOutcome, evidence: Value) -> Self {
        Self {
            outcome,
            evidence,
            events: Vec::new(),
            effects: db::TransitionEffects {
                canonical_evidence: None,
                valuation: None,
                settlement_adoption: None,
                lock_consumption: None,
            },
        }
    }
}

/// Registry containing exactly one step for every non-terminal deposit state.
pub struct StepSet {
    detected: Box<dyn Step>,
    confirmed: Box<dyn Step>,
    cleared: Box<dyn Step>,
    credited: Box<dyn Step>,
}

impl StepSet {
    /// Creates a complete state-to-step registry.
    #[must_use]
    pub fn new(
        detected: Box<dyn Step>,
        confirmed: Box<dyn Step>,
        cleared: Box<dyn Step>,
        credited: Box<dyn Step>,
    ) -> Self {
        Self {
            detected,
            confirmed,
            cleared,
            credited,
        }
    }

    /// Replaces the step registered for `detected` deposits.
    #[must_use]
    pub fn with_detected(mut self, detected: Box<dyn Step>) -> Self {
        self.detected = detected;
        self
    }

    /// Replaces the step registered for `confirmed` deposits.
    #[must_use]
    pub fn with_confirmed(mut self, confirmed: Box<dyn Step>) -> Self {
        self.confirmed = confirmed;
        self
    }

    /// Replaces the step registered for `cleared` deposits.
    #[must_use]
    pub fn with_cleared(mut self, cleared: Box<dyn Step>) -> Self {
        self.cleared = cleared;
        self
    }

    fn get(&self, state: DepositState) -> Option<&dyn Step> {
        match state {
            DepositState::Detected => Some(self.detected.as_ref()),
            DepositState::Confirmed => Some(self.confirmed.as_ref()),
            DepositState::Cleared => Some(self.cleared.as_ref()),
            DepositState::Credited => Some(self.credited.as_ref()),
            DepositState::Swept | DepositState::Rejected => None,
        }
    }
}

/// Placeholder step registry used until the state-specific work packages land.
pub struct NoopStepSet;

impl NoopStepSet {
    /// Builds a registry whose steps leave every deposit waiting.
    #[must_use]
    pub fn build() -> StepSet {
        StepSet::new(
            Box::new(NoopStep),
            Box::new(NoopStep),
            Box::new(NoopStep),
            Box::new(NoopStep),
        )
    }
}

/// Placeholder step that leaves a deposit waiting without external effects.
pub struct NoopStep;

#[async_trait]
impl Step for NoopStep {
    async fn run(&self, _deposit: &Deposit) -> StepResult {
        StepResult::new(
            StepOutcome::Wait {
                reason: WaitReason::Paused,
            },
            json!({"outcome": "wait", "reason": "noop_step_set"}),
        )
    }
}

/// Runtime timing policy for one pump worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PumpConfig {
    /// Maximum duration of one step, which must remain shorter than the lease.
    pub step_timeout: Duration,
    /// Delay used for an expected wait outcome.
    pub wait_interval: Duration,
    /// Delay before polling again when no due deposit is available.
    pub idle_poll_interval: Duration,
}

impl Default for PumpConfig {
    fn default() -> Self {
        Self {
            step_timeout: Duration::from_secs(4 * 60),
            wait_interval: Duration::from_secs(60),
            idle_poll_interval: Duration::from_millis(250),
        }
    }
}

impl PumpConfig {
    fn validate(self) -> Result<Self, PumpConfigError> {
        if self.step_timeout.is_zero() {
            return Err(PumpConfigError::ZeroStepTimeout);
        }
        if self.step_timeout >= LEASE_DURATION {
            return Err(PumpConfigError::TimeoutNotShorterThanLease);
        }
        if self.idle_poll_interval.is_zero() {
            return Err(PumpConfigError::ZeroIdlePollInterval);
        }
        Ok(self)
    }
}

/// Invalid pump timing configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PumpConfigError {
    /// A zero timeout would cancel every step immediately.
    ZeroStepTimeout,
    /// The step timeout must be strictly shorter than the five-minute lease.
    TimeoutNotShorterThanLease,
    /// A zero idle interval would create a busy claim loop.
    ZeroIdlePollInterval,
}

impl Display for PumpConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroStepTimeout => formatter.write_str("step timeout must be positive"),
            Self::TimeoutNotShorterThanLease => {
                formatter.write_str("step timeout must be shorter than the five-minute lease")
            }
            Self::ZeroIdlePollInterval => {
                formatter.write_str("idle poll interval must be positive")
            }
        }
    }
}

impl Error for PumpConfigError {}

/// Entropy source used by full-jitter retry scheduling.
pub trait JitterSource: Send + Sync {
    /// Returns one value spanning the complete `u64` range.
    fn next_u64(&self) -> u64;
}

/// UUID-v4-backed jitter source used by production pumps.
pub struct UuidJitter;

impl JitterSource for UuidJitter {
    fn next_u64(&self) -> u64 {
        Uuid::new_v4().as_u64_pair().0
    }
}

/// Result of one claim-and-process attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunOnceResult {
    /// No due deposit was claimable.
    Idle,
    /// The step result was persisted.
    Applied {
        /// Deposit whose transition was applied.
        deposit_id: Uuid,
    },
    /// A newer lease owner won and the late result was discarded.
    Stale {
        /// Deposit whose result was discarded.
        deposit_id: Uuid,
    },
    /// A selected single-use rate lock was consumed by another deposit.
    Contended {
        /// Deposit that must be re-run at spot pricing.
        deposit_id: Uuid,
    },
}

/// A worker that claims and advances one durable deposit at a time.
#[derive(Clone)]
pub struct Pump {
    pool: PgPool,
    steps: Arc<StepSet>,
    config: PumpConfig,
    jitter: Arc<dyn JitterSource>,
}

impl Pump {
    /// Creates a pump with UUID-v4 retry jitter.
    pub fn new(
        pool: PgPool,
        steps: Arc<StepSet>,
        config: PumpConfig,
    ) -> Result<Self, PumpConfigError> {
        Self::with_jitter(pool, steps, config, Arc::new(UuidJitter))
    }

    /// Creates a pump with an explicit jitter source.
    pub fn with_jitter(
        pool: PgPool,
        steps: Arc<StepSet>,
        config: PumpConfig,
        jitter: Arc<dyn JitterSource>,
    ) -> Result<Self, PumpConfigError> {
        Ok(Self {
            pool,
            steps,
            config: config.validate()?,
            jitter,
        })
    }

    /// Runs until cancellation, finishing an already claimed step before stopping.
    pub async fn run(&self, cancellation: CancellationToken) {
        loop {
            if cancellation.is_cancelled() {
                return;
            }

            match self.run_once().await {
                Ok(RunOnceResult::Idle) => {
                    tokio::select! {
                        () = cancellation.cancelled() => return,
                        () = sleep(self.config.idle_poll_interval) => {}
                    }
                }
                Ok(
                    RunOnceResult::Applied { .. }
                    | RunOnceResult::Stale { .. }
                    | RunOnceResult::Contended { .. },
                ) => {}
                Err(error) => {
                    tracing::error!(%error, "deposit pump iteration failed");
                    tokio::select! {
                        () = cancellation.cancelled() => return,
                        () = sleep(self.config.idle_poll_interval) => {}
                    }
                }
            }
        }
    }

    /// Claims at most one due deposit, runs one step, and persists one transition.
    pub async fn run_once(&self) -> Result<RunOnceResult, PumpError> {
        let lease_token = Uuid::new_v4();
        let Some(deposit) = db::claim_deposit(&self.pool, lease_token).await? else {
            return Ok(RunOnceResult::Idle);
        };
        let deposit_id = deposit.id;
        let result = self.run_step(&deposit).await;
        let result = match next(deposit.state, &result.outcome) {
            Ok(transition) => (result, transition),
            Err(error) => {
                tracing::error!(
                    deposit_id = %deposit.id,
                    state = ?deposit.state,
                    %error,
                    "step returned an invalid outcome"
                );
                let result = StepResult::new(
                    StepOutcome::Retry {
                        error: RetryError::InvariantViolation,
                    },
                    json!({
                        "outcome": "retry",
                        "error": "invalid_step_outcome",
                    }),
                );
                let transition = next(deposit.state, &result.outcome)
                    .map_err(|_| PumpError::MissingStep(deposit.state))?;
                (result, transition)
            }
        };
        let (result, transition) = result;
        let now = Utc::now();
        let attempt = match transition.kind {
            TransitionKind::Advanced => 0,
            TransitionKind::Retry => deposit.attempt.saturating_add(1),
            TransitionKind::Wait | TransitionKind::Rejected => deposit.attempt,
        };
        let delay = match transition.kind {
            TransitionKind::Retry => {
                let retry_attempt = u32::try_from(deposit.attempt).unwrap_or(u32::MAX);
                backoff(retry_attempt, self.jitter.next_u64())
            }
            TransitionKind::Wait => self.config.wait_interval,
            TransitionKind::Advanced | TransitionKind::Rejected => Duration::ZERO,
        };
        let chrono_delay =
            chrono::Duration::from_std(delay).map_err(|_| PumpError::ScheduleOutsideChronoRange)?;
        let next_attempt_at = now
            .checked_add_signed(chrono_delay)
            .ok_or(PumpError::ScheduleOutsideChronoRange)?;
        let update = TransitionUpdate {
            transition,
            rejection_reason: match result.outcome {
                StepOutcome::Reject(reason) => Some(reason),
                StepOutcome::AdoptProductAnswer { credited: false } => {
                    Some(RejectReason::ProductRefused)
                }
                _ => None,
            },
            attempt,
            next_attempt_at,
        };
        let mut transaction = self.pool.begin().await?;
        let applied = db::apply_transition(
            &mut transaction,
            deposit.id,
            deposit.state,
            lease_token,
            update,
            db::TransitionWrites {
                evidence: &result.evidence,
                effects: &result.effects,
                outbox_events: &result.events,
            },
        )
        .await?;

        match applied {
            ApplyTransitionResult::Applied => {
                transaction.commit().await?;
                tracing::info!(
                    deposit_id = %deposit.id,
                    chain = deposit.chain_id,
                    state = ?deposit.state,
                    attempt,
                    "deposit step persisted"
                );
                Ok(RunOnceResult::Applied { deposit_id })
            }
            ApplyTransitionResult::Stale => {
                transaction.commit().await?;
                tracing::debug!(
                    deposit_id = %deposit.id,
                    state = ?deposit.state,
                    "discarded stale deposit step result"
                );
                Ok(RunOnceResult::Stale { deposit_id })
            }
            ApplyTransitionResult::LockUnavailable => {
                transaction.rollback().await?;
                db::release_deposit_lease(&self.pool, deposit.id, lease_token).await?;
                tracing::info!(
                    deposit_id = %deposit.id,
                    "rate lock was consumed concurrently; deposit will retry at spot"
                );
                Ok(RunOnceResult::Contended { deposit_id })
            }
        }
    }

    async fn run_step(&self, deposit: &Deposit) -> StepResult {
        let state = deposit.state;
        let Some(step) = self.steps.get(state) else {
            tracing::error!(state = ?state, "no step registered for claimed state");
            return StepResult::new(
                StepOutcome::Retry {
                    error: RetryError::InvariantViolation,
                },
                json!({"outcome": "retry", "error": "missing_step"}),
            );
        };

        match timeout(self.config.step_timeout, step.run(deposit)).await {
            Ok(result) => result,
            Err(_) => {
                tracing::warn!(state = ?state, "deposit step timed out");
                StepResult::new(
                    StepOutcome::Retry {
                        error: RetryError::Transient,
                    },
                    json!({"outcome": "retry", "error": "step_timeout"}),
                )
            }
        }
    }
}

/// Failure while claiming, scheduling, or persisting one pump iteration.
#[derive(Debug)]
pub enum PumpError {
    /// PostgreSQL failed outside the transition writer.
    Database(sqlx::Error),
    /// The transition writer rejected or failed the persistence operation.
    ApplyTransition(ApplyTransitionError),
    /// A terminal state was unexpectedly claimed without a registered step.
    MissingStep(DepositState),
    /// A configured delay could not be represented as a UTC timestamp.
    ScheduleOutsideChronoRange,
}

impl Display for PumpError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => Display::fmt(error, formatter),
            Self::ApplyTransition(error) => Display::fmt(error, formatter),
            Self::MissingStep(state) => write!(formatter, "no pump step for state {state:?}"),
            Self::ScheduleOutsideChronoRange => {
                formatter.write_str("next attempt time is outside the supported UTC range")
            }
        }
    }
}

impl Error for PumpError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::ApplyTransition(error) => Some(error),
            Self::MissingStep(_) | Self::ScheduleOutsideChronoRange => None,
        }
    }
}

impl From<sqlx::Error> for PumpError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl From<ApplyTransitionError> for PumpError {
    fn from(error: ApplyTransitionError) -> Self {
        Self::ApplyTransition(error)
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::json;
    use topup_adapters::signer::actor::SignerHandle;
    use topup_core::Signer as _;
    use topup_core::deposit::{StepOutcome, WaitReason};

    use super::{Deposit, Step, StepResult};

    struct SignerBackedStep {
        signer: SignerHandle,
    }

    #[async_trait]
    impl Step for SignerBackedStep {
        async fn run(&self, _deposit: &Deposit) -> StepResult {
            let _ = self.signer.settlement_public_key().await;
            StepResult::new(
                StepOutcome::Wait {
                    reason: WaitReason::Paused,
                },
                json!({"outcome": "wait"}),
            )
        }
    }

    #[test]
    fn signer_handle_satisfies_step_send_bounds() {
        fn assert_step<T: Step>() {}
        assert_step::<SignerBackedStep>();
    }
}

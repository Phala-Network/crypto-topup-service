//! Concurrent deposit state-machine pumps and state-age alerts.

mod age;

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};
use tokio_util::sync::CancellationToken;
use topup_core::deposit::{
    DepositState, RetryError, StepOutcome, TransitionKind, WaitReason, next,
};
use topup_core::retry::backoff;
use uuid::Uuid;

use crate::db::{self, ApplyTransitionError, ApplyTransitionResult, Deposit, TransitionUpdate};

pub use age::{AgeAlertConfig, AgeAlertConfigError, AgeAlerter, PumpMetrics};

const LEASE_DURATION: Duration = Duration::from_secs(5 * 60);

/// One asynchronous operation for a non-terminal deposit state.
#[async_trait]
pub trait Step: Send + Sync {
    /// Runs the state-specific operation and returns its domain outcome.
    async fn run(&self, deposit: &Deposit) -> StepOutcome;
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

struct NoopStep;

#[async_trait]
impl Step for NoopStep {
    async fn run(&self, _deposit: &Deposit) -> StepOutcome {
        StepOutcome::Wait {
            reason: WaitReason::Paused,
        }
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
                Ok(RunOnceResult::Applied { .. } | RunOnceResult::Stale { .. }) => {}
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
        let outcome = self.run_step(deposit.clone()).await;
        let (outcome, transition) = match next(deposit.state, &outcome) {
            Ok(transition) => (outcome, transition),
            Err(error) => {
                tracing::error!(
                    deposit_id = %deposit.id,
                    state = ?deposit.state,
                    %error,
                    "step returned an invalid outcome"
                );
                let retry = StepOutcome::Retry {
                    error: RetryError::InvariantViolation,
                };
                let transition = next(deposit.state, &retry)
                    .map_err(|_| PumpError::MissingStep(deposit.state))?;
                (retry, transition)
            }
        };
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
            rejection_reason: match outcome {
                StepOutcome::Reject(reason) => Some(reason),
                _ => None,
            },
            attempt,
            next_attempt_at,
        };
        let evidence = outcome_evidence(&outcome);
        let mut transaction = self.pool.begin().await?;
        let applied = db::apply_transition(
            &mut transaction,
            deposit.id,
            deposit.state,
            lease_token,
            update,
            &evidence,
            &[],
        )
        .await?;
        transaction.commit().await?;

        match applied {
            ApplyTransitionResult::Applied => {
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
                tracing::debug!(
                    deposit_id = %deposit.id,
                    state = ?deposit.state,
                    "discarded stale deposit step result"
                );
                Ok(RunOnceResult::Stale { deposit_id })
            }
        }
    }

    async fn run_step(&self, deposit: Deposit) -> StepOutcome {
        let state = deposit.state;
        let steps = Arc::clone(&self.steps);
        let mut task: JoinHandle<Option<StepOutcome>> = tokio::spawn(async move {
            let step = steps.get(state)?;
            Some(step.run(&deposit).await)
        });

        match timeout(self.config.step_timeout, &mut task).await {
            Ok(Ok(Some(outcome))) => outcome,
            Ok(Ok(None)) => {
                tracing::error!(state = ?state, "no step registered for claimed state");
                StepOutcome::Retry {
                    error: RetryError::InvariantViolation,
                }
            }
            Ok(Err(error)) => {
                tracing::warn!(state = ?state, panic = error.is_panic(), "deposit step failed");
                StepOutcome::Retry {
                    error: RetryError::Transient,
                }
            }
            Err(_) => {
                task.abort();
                let _ = task.await;
                tracing::warn!(state = ?state, "deposit step timed out");
                StepOutcome::Retry {
                    error: RetryError::Transient,
                }
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

fn outcome_evidence(outcome: &StepOutcome) -> Value {
    match outcome {
        StepOutcome::Advance => json!({"outcome": "advance"}),
        StepOutcome::Reject(reason) => {
            json!({"outcome": "reject", "reason": reason.code()})
        }
        StepOutcome::Retry { error } => {
            json!({"outcome": "retry", "error": retry_error_code(*error)})
        }
        StepOutcome::Wait { reason } => {
            json!({"outcome": "wait", "reason": wait_reason_code(*reason)})
        }
        StepOutcome::AdoptProductAnswer { credited } => {
            json!({"outcome": "adopt_product_answer", "credited": credited})
        }
    }
}

const fn retry_error_code(error: RetryError) -> &'static str {
    match error {
        RetryError::Transient => "transient",
        RetryError::SanctionsInconclusive => "sanctions_inconclusive",
        RetryError::InvariantViolation => "invariant_violation",
    }
}

const fn wait_reason_code(reason: WaitReason) -> &'static str {
    match reason {
        WaitReason::Paused => "paused",
        WaitReason::ProductProcessing => "product_processing",
        WaitReason::FlushNotConfirmed => "flush_not_confirmed",
    }
}

//! Deposit states and their valid transitions.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use serde::{Deserialize, Serialize};

/// The durable processing state of a deposit.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DepositState {
    /// The finalized chain event has been detected but not yet valued.
    Detected,
    /// Finality and valuation have been confirmed.
    Confirmed,
    /// Screening and policy checks have passed.
    Cleared,
    /// The product has accepted the credit.
    Credited,
    /// A later confirmed flush covers the deposit.
    Swept,
    /// The deposit was deterministically denied credit.
    Rejected,
}

impl DepositState {
    /// Returns whether no further state transition is permitted.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Swept | Self::Rejected)
    }
}

/// A deterministic reason why a deposit cannot be credited.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// The asset has no configured route on the deposit chain.
    UnsupportedAsset,
    /// The computed credit is below the configured minimum.
    BelowMinimum,
    /// The computed credit does not fit the supported money type.
    OutOfRange,
    /// The source address appears on the sanctions list.
    Sanctioned,
    /// The deposited amount is outside the configured bounds.
    OutOfBounds,
    /// Settlement is paused for the account.
    AccountPaused,
    /// The product rejected the settlement request.
    ProductRefused,
}

impl RejectReason {
    /// Returns the stable snake-case code stored and exposed for this reason.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnsupportedAsset => "unsupported_asset",
            Self::BelowMinimum => "below_minimum",
            Self::OutOfRange => "out_of_range",
            Self::Sanctioned => "sanctioned",
            Self::OutOfBounds => "out_of_bounds",
            Self::AccountPaused => "account_paused",
            Self::ProductRefused => "product_refused",
        }
    }
}

/// The retry category reported by a step that could not complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryError {
    /// An external dependency or other transient operation failed.
    Transient,
    /// Stored or returned data violated an invariant and requires attention.
    InvariantViolation,
}

/// The expected condition that keeps a step in its current state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitReason {
    /// The product is still processing the settlement.
    ProductProcessing,
    /// No confirmed flush after the deposit has been observed yet.
    FlushNotConfirmed,
}

/// The domain result returned by one processing step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepOutcome {
    /// The step completed and should move to the next progress state.
    Advance,
    /// The step deterministically denied the deposit.
    Reject(RejectReason),
    /// The step failed and should be retried after backoff.
    Retry {
        /// The retryable error category.
        error: RetryError,
    },
    /// The step is waiting for an expected external condition.
    Wait {
        /// The expected condition that has not completed yet.
        reason: WaitReason,
    },
    /// The confirm step found the product's authoritative prior answer.
    AdoptProductAnswer {
        /// Whether the product had credited rather than rejected the deposit.
        credited: bool,
    },
}

impl StepOutcome {
    const fn kind(&self) -> StepOutcomeKind {
        match self {
            Self::Advance => StepOutcomeKind::Advance,
            Self::Reject(_) => StepOutcomeKind::Reject,
            Self::Retry { .. } => StepOutcomeKind::Retry,
            Self::Wait { .. } => StepOutcomeKind::Wait,
            Self::AdoptProductAnswer { .. } => StepOutcomeKind::AdoptProductAnswer,
        }
    }
}

/// The outcome category used when reporting an invalid transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StepOutcomeKind {
    /// A normal progress request.
    Advance,
    /// A deterministic rejection.
    Reject,
    /// A retryable failure.
    Retry,
    /// An expected wait.
    Wait,
    /// Adoption of an authoritative product answer.
    AdoptProductAnswer,
}

/// The effect that applying an outcome has on the state machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionKind {
    /// The deposit advanced to a later progress state.
    Advanced,
    /// The deposit entered the rejected terminal state.
    Rejected,
    /// The deposit stayed in place for a retry.
    Retry,
    /// The deposit stayed in place while waiting.
    Wait,
}

/// A validated deposit state transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Transition {
    /// The state before applying the outcome.
    pub from: DepositState,
    /// The state after applying the outcome.
    pub to: DepositState,
    /// The semantic kind of transition.
    pub kind: TransitionKind,
}

/// An outcome that is not valid for the supplied deposit state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidTransition {
    /// The state for which the outcome was invalid.
    pub state: DepositState,
    /// The invalid outcome category.
    pub outcome: StepOutcomeKind,
}

impl Display for InvalidTransition {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "outcome {:?} is invalid for deposit state {:?}",
            self.outcome, self.state
        )
    }
}

impl Error for InvalidTransition {}

/// Applies a step outcome and returns the only valid target state for it.
///
/// Retry and wait outcomes keep a non-terminal deposit in its current state.
/// Invalid state/outcome combinations are returned as errors and never panic.
pub fn next(state: DepositState, outcome: &StepOutcome) -> Result<Transition, InvalidTransition> {
    let transition = match (state, outcome) {
        (DepositState::Detected, StepOutcome::Advance) => Transition {
            from: state,
            to: DepositState::Confirmed,
            kind: TransitionKind::Advanced,
        },
        (DepositState::Confirmed, StepOutcome::Advance) => Transition {
            from: state,
            to: DepositState::Cleared,
            kind: TransitionKind::Advanced,
        },
        (DepositState::Cleared, StepOutcome::Advance) => Transition {
            from: state,
            to: DepositState::Credited,
            kind: TransitionKind::Advanced,
        },
        (DepositState::Credited, StepOutcome::Advance) => Transition {
            from: state,
            to: DepositState::Swept,
            kind: TransitionKind::Advanced,
        },
        (
            DepositState::Detected | DepositState::Confirmed | DepositState::Cleared,
            StepOutcome::Reject(_),
        ) => Transition {
            from: state,
            to: DepositState::Rejected,
            kind: TransitionKind::Rejected,
        },
        (
            DepositState::Detected
            | DepositState::Confirmed
            | DepositState::Cleared
            | DepositState::Credited,
            StepOutcome::Retry { .. },
        ) => Transition {
            from: state,
            to: state,
            kind: TransitionKind::Retry,
        },
        (
            DepositState::Detected
            | DepositState::Confirmed
            | DepositState::Cleared
            | DepositState::Credited,
            StepOutcome::Wait { .. },
        ) => Transition {
            from: state,
            to: state,
            kind: TransitionKind::Wait,
        },
        (DepositState::Detected, StepOutcome::AdoptProductAnswer { credited }) => Transition {
            from: state,
            to: if *credited {
                DepositState::Credited
            } else {
                DepositState::Rejected
            },
            kind: if *credited {
                TransitionKind::Advanced
            } else {
                TransitionKind::Rejected
            },
        },
        _ => {
            return Err(InvalidTransition {
                state,
                outcome: outcome.kind(),
            });
        }
    };

    Ok(transition)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ADVANCE: StepOutcome = StepOutcome::Advance;
    const REJECT: StepOutcome = StepOutcome::Reject(RejectReason::Sanctioned);
    const RETRY: StepOutcome = StepOutcome::Retry {
        error: RetryError::Transient,
    };
    const WAIT: StepOutcome = StepOutcome::Wait {
        reason: WaitReason::FlushNotConfirmed,
    };
    const ADOPT_CREDITED: StepOutcome = StepOutcome::AdoptProductAnswer { credited: true };

    fn transition(
        from: DepositState,
        to: DepositState,
        kind: TransitionKind,
    ) -> Result<Transition, InvalidTransition> {
        Ok(Transition { from, to, kind })
    }

    fn invalid(
        state: DepositState,
        outcome: StepOutcomeKind,
    ) -> Result<Transition, InvalidTransition> {
        Err(InvalidTransition { state, outcome })
    }

    #[test]
    fn transition_table_covers_every_state_and_outcome_kind() {
        let cases = [
            (
                DepositState::Detected,
                &ADVANCE,
                transition(
                    DepositState::Detected,
                    DepositState::Confirmed,
                    TransitionKind::Advanced,
                ),
            ),
            (
                DepositState::Detected,
                &REJECT,
                transition(
                    DepositState::Detected,
                    DepositState::Rejected,
                    TransitionKind::Rejected,
                ),
            ),
            (
                DepositState::Detected,
                &RETRY,
                transition(
                    DepositState::Detected,
                    DepositState::Detected,
                    TransitionKind::Retry,
                ),
            ),
            (
                DepositState::Detected,
                &WAIT,
                transition(
                    DepositState::Detected,
                    DepositState::Detected,
                    TransitionKind::Wait,
                ),
            ),
            (
                DepositState::Detected,
                &ADOPT_CREDITED,
                transition(
                    DepositState::Detected,
                    DepositState::Credited,
                    TransitionKind::Advanced,
                ),
            ),
            (
                DepositState::Confirmed,
                &ADVANCE,
                transition(
                    DepositState::Confirmed,
                    DepositState::Cleared,
                    TransitionKind::Advanced,
                ),
            ),
            (
                DepositState::Confirmed,
                &REJECT,
                transition(
                    DepositState::Confirmed,
                    DepositState::Rejected,
                    TransitionKind::Rejected,
                ),
            ),
            (
                DepositState::Confirmed,
                &RETRY,
                transition(
                    DepositState::Confirmed,
                    DepositState::Confirmed,
                    TransitionKind::Retry,
                ),
            ),
            (
                DepositState::Confirmed,
                &WAIT,
                transition(
                    DepositState::Confirmed,
                    DepositState::Confirmed,
                    TransitionKind::Wait,
                ),
            ),
            (
                DepositState::Confirmed,
                &ADOPT_CREDITED,
                invalid(DepositState::Confirmed, StepOutcomeKind::AdoptProductAnswer),
            ),
            (
                DepositState::Cleared,
                &ADVANCE,
                transition(
                    DepositState::Cleared,
                    DepositState::Credited,
                    TransitionKind::Advanced,
                ),
            ),
            (
                DepositState::Cleared,
                &REJECT,
                transition(
                    DepositState::Cleared,
                    DepositState::Rejected,
                    TransitionKind::Rejected,
                ),
            ),
            (
                DepositState::Cleared,
                &RETRY,
                transition(
                    DepositState::Cleared,
                    DepositState::Cleared,
                    TransitionKind::Retry,
                ),
            ),
            (
                DepositState::Cleared,
                &WAIT,
                transition(
                    DepositState::Cleared,
                    DepositState::Cleared,
                    TransitionKind::Wait,
                ),
            ),
            (
                DepositState::Cleared,
                &ADOPT_CREDITED,
                invalid(DepositState::Cleared, StepOutcomeKind::AdoptProductAnswer),
            ),
            (
                DepositState::Credited,
                &ADVANCE,
                transition(
                    DepositState::Credited,
                    DepositState::Swept,
                    TransitionKind::Advanced,
                ),
            ),
            (
                DepositState::Credited,
                &REJECT,
                invalid(DepositState::Credited, StepOutcomeKind::Reject),
            ),
            (
                DepositState::Credited,
                &RETRY,
                transition(
                    DepositState::Credited,
                    DepositState::Credited,
                    TransitionKind::Retry,
                ),
            ),
            (
                DepositState::Credited,
                &WAIT,
                transition(
                    DepositState::Credited,
                    DepositState::Credited,
                    TransitionKind::Wait,
                ),
            ),
            (
                DepositState::Credited,
                &ADOPT_CREDITED,
                invalid(DepositState::Credited, StepOutcomeKind::AdoptProductAnswer),
            ),
            (
                DepositState::Swept,
                &ADVANCE,
                invalid(DepositState::Swept, StepOutcomeKind::Advance),
            ),
            (
                DepositState::Swept,
                &REJECT,
                invalid(DepositState::Swept, StepOutcomeKind::Reject),
            ),
            (
                DepositState::Swept,
                &RETRY,
                invalid(DepositState::Swept, StepOutcomeKind::Retry),
            ),
            (
                DepositState::Swept,
                &WAIT,
                invalid(DepositState::Swept, StepOutcomeKind::Wait),
            ),
            (
                DepositState::Swept,
                &ADOPT_CREDITED,
                invalid(DepositState::Swept, StepOutcomeKind::AdoptProductAnswer),
            ),
            (
                DepositState::Rejected,
                &ADVANCE,
                invalid(DepositState::Rejected, StepOutcomeKind::Advance),
            ),
            (
                DepositState::Rejected,
                &REJECT,
                invalid(DepositState::Rejected, StepOutcomeKind::Reject),
            ),
            (
                DepositState::Rejected,
                &RETRY,
                invalid(DepositState::Rejected, StepOutcomeKind::Retry),
            ),
            (
                DepositState::Rejected,
                &WAIT,
                invalid(DepositState::Rejected, StepOutcomeKind::Wait),
            ),
            (
                DepositState::Rejected,
                &ADOPT_CREDITED,
                invalid(DepositState::Rejected, StepOutcomeKind::AdoptProductAnswer),
            ),
        ];

        for (state, outcome, expected) in cases {
            assert_eq!(next(state, outcome), expected);
        }
    }

    #[test]
    fn rejected_product_answer_is_adopted_from_detected() {
        assert_eq!(
            next(
                DepositState::Detected,
                &StepOutcome::AdoptProductAnswer { credited: false }
            ),
            transition(
                DepositState::Detected,
                DepositState::Rejected,
                TransitionKind::Rejected,
            )
        );
    }

    #[test]
    fn terminal_predicate_matches_terminal_states() {
        assert!(!DepositState::Detected.is_terminal());
        assert!(!DepositState::Confirmed.is_terminal());
        assert!(!DepositState::Cleared.is_terminal());
        assert!(!DepositState::Credited.is_terminal());
        assert!(DepositState::Swept.is_terminal());
        assert!(DepositState::Rejected.is_terminal());
    }

    #[test]
    fn state_and_reject_reason_serde_codes_are_stable() -> Result<(), serde_json::Error> {
        let states = [
            (DepositState::Detected, "\"detected\""),
            (DepositState::Confirmed, "\"confirmed\""),
            (DepositState::Cleared, "\"cleared\""),
            (DepositState::Credited, "\"credited\""),
            (DepositState::Swept, "\"swept\""),
            (DepositState::Rejected, "\"rejected\""),
        ];
        let reasons = [
            (RejectReason::UnsupportedAsset, "unsupported_asset"),
            (RejectReason::BelowMinimum, "below_minimum"),
            (RejectReason::OutOfRange, "out_of_range"),
            (RejectReason::Sanctioned, "sanctioned"),
            (RejectReason::OutOfBounds, "out_of_bounds"),
            (RejectReason::AccountPaused, "account_paused"),
            (RejectReason::ProductRefused, "product_refused"),
        ];

        for (state, encoded) in states {
            assert_eq!(serde_json::to_string(&state)?, encoded);
            assert_eq!(serde_json::from_str::<DepositState>(encoded)?, state);
        }
        for (reason, code) in reasons {
            let encoded = format!("\"{code}\"");
            assert_eq!(reason.code(), code);
            assert_eq!(serde_json::to_string(&reason)?, encoded);
            assert_eq!(serde_json::from_str::<RejectReason>(&encoded)?, reason);
        }

        Ok(())
    }
}

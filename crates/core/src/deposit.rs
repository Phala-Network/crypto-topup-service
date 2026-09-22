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

    #[derive(Clone, Copy)]
    enum Expected {
        Valid(DepositState, TransitionKind),
        Invalid(StepOutcomeKind),
    }

    #[test]
    fn transition_table_covers_every_state_and_outcome_kind() {
        use DepositState::{Cleared, Confirmed, Credited, Detected, Rejected, Swept};
        use Expected::{Invalid, Valid};
        use StepOutcomeKind as Outcome;
        use TransitionKind as Kind;

        let cases = [
            (Detected, &ADVANCE, Valid(Confirmed, Kind::Advanced)),
            (Detected, &REJECT, Valid(Rejected, Kind::Rejected)),
            (Detected, &RETRY, Valid(Detected, Kind::Retry)),
            (Detected, &WAIT, Valid(Detected, Kind::Wait)),
            (Detected, &ADOPT_CREDITED, Valid(Credited, Kind::Advanced)),
            (Confirmed, &ADVANCE, Valid(Cleared, Kind::Advanced)),
            (Confirmed, &REJECT, Valid(Rejected, Kind::Rejected)),
            (Confirmed, &RETRY, Valid(Confirmed, Kind::Retry)),
            (Confirmed, &WAIT, Valid(Confirmed, Kind::Wait)),
            (
                Confirmed,
                &ADOPT_CREDITED,
                Invalid(Outcome::AdoptProductAnswer),
            ),
            (Cleared, &ADVANCE, Valid(Credited, Kind::Advanced)),
            (Cleared, &REJECT, Valid(Rejected, Kind::Rejected)),
            (Cleared, &RETRY, Valid(Cleared, Kind::Retry)),
            (Cleared, &WAIT, Valid(Cleared, Kind::Wait)),
            (
                Cleared,
                &ADOPT_CREDITED,
                Invalid(Outcome::AdoptProductAnswer),
            ),
            (Credited, &ADVANCE, Valid(Swept, Kind::Advanced)),
            (Credited, &REJECT, Invalid(Outcome::Reject)),
            (Credited, &RETRY, Valid(Credited, Kind::Retry)),
            (Credited, &WAIT, Valid(Credited, Kind::Wait)),
            (
                Credited,
                &ADOPT_CREDITED,
                Invalid(Outcome::AdoptProductAnswer),
            ),
            (Swept, &ADVANCE, Invalid(Outcome::Advance)),
            (Swept, &REJECT, Invalid(Outcome::Reject)),
            (Swept, &RETRY, Invalid(Outcome::Retry)),
            (Swept, &WAIT, Invalid(Outcome::Wait)),
            (Swept, &ADOPT_CREDITED, Invalid(Outcome::AdoptProductAnswer)),
            (Rejected, &ADVANCE, Invalid(Outcome::Advance)),
            (Rejected, &REJECT, Invalid(Outcome::Reject)),
            (Rejected, &RETRY, Invalid(Outcome::Retry)),
            (Rejected, &WAIT, Invalid(Outcome::Wait)),
            (
                Rejected,
                &ADOPT_CREDITED,
                Invalid(Outcome::AdoptProductAnswer),
            ),
        ];

        for (state, outcome, expected) in cases {
            match expected {
                Valid(to, kind) => {
                    assert_eq!(
                        next(state, outcome),
                        Ok(Transition {
                            from: state,
                            to,
                            kind
                        })
                    );
                }
                Invalid(expected_outcome) => {
                    assert_eq!(
                        next(state, outcome),
                        Err(InvalidTransition {
                            state,
                            outcome: expected_outcome,
                        })
                    );
                }
            }
        }
    }

    #[test]
    fn rejected_product_answer_is_adopted_from_detected() {
        assert_eq!(
            next(
                DepositState::Detected,
                &StepOutcome::AdoptProductAnswer { credited: false }
            ),
            Ok(Transition {
                from: DepositState::Detected,
                to: DepositState::Rejected,
                kind: TransitionKind::Rejected,
            })
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

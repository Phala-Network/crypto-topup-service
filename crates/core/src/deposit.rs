//! Deposit states and their valid transitions.

use serde::{Deserialize, Serialize};

/// The durable processing state of a deposit.
///
/// Whether a deposit is final (its block at or below `finalized` on both providers) is a
/// timestamp, not a state: any state before finality can still become [`Self::Reversed`].
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DepositState {
    /// The transfer reached the required confirmation and has not been valued yet.
    Detected,
    /// Finality and valuation have been confirmed.
    Confirmed,
    /// Screening passed: the credit is final and owed to the product, which is told with a
    /// `deposit.credited` webhook.
    Credited,
    /// A finalized `Flushed` event after the final deposit emptied its forwarder.
    Swept,
    /// The deposit was deterministically denied credit.
    Rejected,
    /// The transfer is not part of the final chain: its transaction was dropped and its nonce
    /// consumed by another, or its receipt at finality lacks the transfer.
    Reversed,
}

impl DepositState {
    /// Returns whether no further state transition is permitted.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Swept | Self::Rejected | Self::Reversed)
    }

    /// Returns whether a deposit in this state may still become [`Self::Reversed`]: every state
    /// except `swept` (which requires a final deposit) and `reversed` itself.
    #[must_use]
    pub const fn is_reversible(self) -> bool {
        !matches!(self, Self::Swept | Self::Reversed)
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
    /// The asset has a route, but the payment settings the deposit is bound to do not accept it.
    AssetNotAccepted,
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
            Self::AssetNotAccepted => "asset_not_accepted",
        }
    }
}

/// The retry category reported by a step that could not complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryError {
    /// An external dependency or other transient operation failed.
    Transient,
    /// Independent RPC providers did not agree on finalized evidence.
    RpcDisagreement,
    /// Required price observations were unavailable or invalid.
    PriceUnavailable,
    /// The two sanctions checks did not produce a conclusive clear result.
    SanctionsInconclusive,
    /// Stored or returned data violated an invariant and requires attention.
    InvariantViolation,
}

/// The expected condition that keeps a step in its current state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitReason {
    /// At least one provider has not finalized the deposit block yet.
    Finality,
    /// At least one provider has not reached the route's depth or `safe` confirmation yet.
    Confirmations,
    /// Crediting is paused for the account, product, or route.
    Paused,
    /// Crediting the deposit before it is final would take the account's credit that is not
    /// final yet past its cap; it is credited once final.
    UnfinalizedCreditCap,
    /// The deposit was recorded while its account's payment settings were held after a restore;
    /// it waits for the merchant to reconfirm them.
    SettingsUnconfirmed,
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
}

impl StepOutcome {
    const fn kind(&self) -> StepOutcomeKind {
        match self {
            Self::Advance => StepOutcomeKind::Advance,
            Self::Reject(_) => StepOutcomeKind::Reject,
            Self::Retry { .. } => StepOutcomeKind::Retry,
            Self::Wait { .. } => StepOutcomeKind::Wait,
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
}

/// The effect that applying an outcome has on the state machine.
///
/// The pump owns the per-state retry counter. A retry counts a failed retry,
/// a wait leaves the counter unchanged, and advancing resets it to zero. Core
/// validates the transition but does not maintain that counter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransitionKind {
    /// The deposit advanced to a later progress state; the pump resets attempt to zero.
    Advanced,
    /// The deposit entered the rejected terminal state.
    Rejected,
    /// The deposit stayed in place after a failure; the pump increments attempt.
    Retry,
    /// The deposit stayed in place while waiting; the pump leaves attempt unchanged.
    Wait,
    /// The finality watch found the transfer gone from the final chain.
    Reversed,
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("outcome {outcome:?} is invalid for deposit state {state:?}")]
pub struct InvalidTransition {
    /// The state for which the outcome was invalid.
    pub state: DepositState,
    /// The invalid outcome category.
    pub outcome: StepOutcomeKind,
}

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
            to: DepositState::Credited,
            kind: TransitionKind::Advanced,
        },
        (DepositState::Credited, StepOutcome::Advance) => Transition {
            from: state,
            to: DepositState::Swept,
            kind: TransitionKind::Advanced,
        },
        (DepositState::Detected | DepositState::Confirmed, StepOutcome::Reject(_)) => Transition {
            from: state,
            to: DepositState::Rejected,
            kind: TransitionKind::Rejected,
        },
        (
            DepositState::Detected | DepositState::Confirmed | DepositState::Credited,
            StepOutcome::Retry { .. },
        ) => Transition {
            from: state,
            to: state,
            kind: TransitionKind::Retry,
        },
        (
            DepositState::Detected | DepositState::Confirmed | DepositState::Credited,
            StepOutcome::Wait { .. },
        ) => Transition {
            from: state,
            to: state,
            kind: TransitionKind::Wait,
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

/// Returns the transition into [`DepositState::Reversed`], which only the finality watch applies,
/// outside the pump's step outcomes.
pub fn reverse(state: DepositState) -> Result<Transition, InvalidReversal> {
    if state.is_reversible() {
        Ok(Transition {
            from: state,
            to: DepositState::Reversed,
            kind: TransitionKind::Reversed,
        })
    } else {
        Err(InvalidReversal { state })
    }
}

/// A reversal requested for a state that cannot be reversed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("deposit state {state:?} cannot be reversed")]
pub struct InvalidReversal {
    /// The state that cannot be reversed.
    pub state: DepositState,
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
        reason: WaitReason::Finality,
    };
    const PAUSED_WAIT: StepOutcome = StepOutcome::Wait {
        reason: WaitReason::Paused,
    };

    #[derive(Clone, Copy)]
    enum Expected {
        Valid(DepositState, TransitionKind),
        Invalid(StepOutcomeKind),
    }

    #[test]
    fn transition_table_covers_every_state_and_outcome_kind() {
        use DepositState::{Confirmed, Credited, Detected, Rejected, Reversed, Swept};
        use Expected::{Invalid, Valid};
        use StepOutcomeKind as Outcome;
        use TransitionKind as Kind;

        let cases = [
            (Detected, &ADVANCE, Valid(Confirmed, Kind::Advanced)),
            (Detected, &REJECT, Valid(Rejected, Kind::Rejected)),
            (Detected, &RETRY, Valid(Detected, Kind::Retry)),
            (Detected, &WAIT, Valid(Detected, Kind::Wait)),
            (Confirmed, &ADVANCE, Valid(Credited, Kind::Advanced)),
            (Confirmed, &REJECT, Valid(Rejected, Kind::Rejected)),
            (Confirmed, &RETRY, Valid(Confirmed, Kind::Retry)),
            (Confirmed, &WAIT, Valid(Confirmed, Kind::Wait)),
            (Confirmed, &PAUSED_WAIT, Valid(Confirmed, Kind::Wait)),
            (Credited, &ADVANCE, Valid(Swept, Kind::Advanced)),
            (Credited, &REJECT, Invalid(Outcome::Reject)),
            (Credited, &RETRY, Valid(Credited, Kind::Retry)),
            (Credited, &WAIT, Valid(Credited, Kind::Wait)),
            (Swept, &ADVANCE, Invalid(Outcome::Advance)),
            (Swept, &REJECT, Invalid(Outcome::Reject)),
            (Swept, &RETRY, Invalid(Outcome::Retry)),
            (Swept, &WAIT, Invalid(Outcome::Wait)),
            (Rejected, &ADVANCE, Invalid(Outcome::Advance)),
            (Rejected, &REJECT, Invalid(Outcome::Reject)),
            (Rejected, &RETRY, Invalid(Outcome::Retry)),
            (Rejected, &WAIT, Invalid(Outcome::Wait)),
            (Reversed, &ADVANCE, Invalid(Outcome::Advance)),
            (Reversed, &REJECT, Invalid(Outcome::Reject)),
            (Reversed, &RETRY, Invalid(Outcome::Retry)),
            (Reversed, &WAIT, Invalid(Outcome::Wait)),
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
    fn paused_deposit_waits_then_advances_after_resume() {
        assert_eq!(
            next(DepositState::Confirmed, &PAUSED_WAIT),
            Ok(Transition {
                from: DepositState::Confirmed,
                to: DepositState::Confirmed,
                kind: TransitionKind::Wait,
            })
        );
        assert_eq!(
            next(DepositState::Confirmed, &StepOutcome::Advance),
            Ok(Transition {
                from: DepositState::Confirmed,
                to: DepositState::Credited,
                kind: TransitionKind::Advanced,
            })
        );
    }

    #[test]
    fn terminal_predicate_matches_terminal_states() {
        assert!(!DepositState::Detected.is_terminal());
        assert!(!DepositState::Confirmed.is_terminal());
        assert!(!DepositState::Credited.is_terminal());
        assert!(DepositState::Swept.is_terminal());
        assert!(DepositState::Rejected.is_terminal());
        assert!(DepositState::Reversed.is_terminal());
    }

    #[test]
    fn every_state_before_finality_can_be_reversed_exactly_once() {
        for state in [
            DepositState::Detected,
            DepositState::Confirmed,
            DepositState::Credited,
            DepositState::Rejected,
        ] {
            assert_eq!(
                reverse(state),
                Ok(Transition {
                    from: state,
                    to: DepositState::Reversed,
                    kind: TransitionKind::Reversed,
                })
            );
        }
        for state in [DepositState::Swept, DepositState::Reversed] {
            assert_eq!(reverse(state), Err(InvalidReversal { state }));
        }
    }

    #[test]
    fn state_and_reject_reason_serde_codes_are_stable() -> Result<(), serde_json::Error> {
        let states = [
            (DepositState::Detected, "\"detected\""),
            (DepositState::Confirmed, "\"confirmed\""),
            (DepositState::Credited, "\"credited\""),
            (DepositState::Swept, "\"swept\""),
            (DepositState::Rejected, "\"rejected\""),
            (DepositState::Reversed, "\"reversed\""),
        ];
        let reasons = [
            (RejectReason::UnsupportedAsset, "unsupported_asset"),
            (RejectReason::BelowMinimum, "below_minimum"),
            (RejectReason::OutOfRange, "out_of_range"),
            (RejectReason::Sanctioned, "sanctioned"),
            (RejectReason::OutOfBounds, "out_of_bounds"),
            (RejectReason::AssetNotAccepted, "asset_not_accepted"),
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
        assert!(serde_json::from_str::<RejectReason>("\"account_paused\"").is_err());

        Ok(())
    }
}

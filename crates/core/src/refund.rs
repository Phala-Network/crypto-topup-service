//! Pure refund eligibility policy.

use crate::deposit::{DepositState, RejectReason};

/// Deposit facts needed to decide whether finance may review a refund request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefundDeposit {
    /// Current deposit state.
    pub state: DepositState,
    /// Stable rejection reason, when the deposit is rejected.
    pub reason: Option<RejectReason>,
}

/// Stable reason a deposit cannot enter the refund workflow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefundIneligible {
    /// Only rejected deposits can be refunded by the current persisted model.
    NotRejected,
    /// Sanctioned funds require the compliance process, not the refund workflow.
    Sanctioned,
    /// Below-minimum dust is not refundable under architecture section 15.
    BelowMinimumDust,
    /// A rejected row without a reason violates the deposit invariant.
    MissingRejectReason,
}

/// Applies the section 15 refund policy to persisted deposit facts.
///
/// `unsupported_asset` covers wrong-asset deposits and `out_of_bounds` covers
/// policy overpayments. Other rejected deposits remain refundable unless the
/// rejection is sanctions-related or below-minimum dust.
pub const fn refund_eligibility(deposit: RefundDeposit) -> Result<(), RefundIneligible> {
    if !matches!(deposit.state, DepositState::Rejected) {
        return Err(RefundIneligible::NotRejected);
    }

    match deposit.reason {
        Some(RejectReason::Sanctioned) => Err(RefundIneligible::Sanctioned),
        Some(RejectReason::BelowMinimum) => Err(RefundIneligible::BelowMinimumDust),
        Some(
            RejectReason::UnsupportedAsset
            | RejectReason::OutOfRange
            | RejectReason::OutOfBounds
            | RejectReason::ProductRefused,
        ) => Ok(()),
        None => Err(RefundIneligible::MissingRejectReason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refund_eligibility_table() {
        let cases = [
            (
                RefundDeposit {
                    state: DepositState::Rejected,
                    reason: Some(RejectReason::UnsupportedAsset),
                },
                Ok(()),
            ),
            (
                RefundDeposit {
                    state: DepositState::Rejected,
                    reason: Some(RejectReason::OutOfBounds),
                },
                Ok(()),
            ),
            (
                RefundDeposit {
                    state: DepositState::Rejected,
                    reason: Some(RejectReason::OutOfRange),
                },
                Ok(()),
            ),
            (
                RefundDeposit {
                    state: DepositState::Rejected,
                    reason: Some(RejectReason::ProductRefused),
                },
                Ok(()),
            ),
            (
                RefundDeposit {
                    state: DepositState::Rejected,
                    reason: Some(RejectReason::Sanctioned),
                },
                Err(RefundIneligible::Sanctioned),
            ),
            (
                RefundDeposit {
                    state: DepositState::Rejected,
                    reason: Some(RejectReason::BelowMinimum),
                },
                Err(RefundIneligible::BelowMinimumDust),
            ),
            (
                RefundDeposit {
                    state: DepositState::Rejected,
                    reason: None,
                },
                Err(RefundIneligible::MissingRejectReason),
            ),
            (
                RefundDeposit {
                    state: DepositState::Detected,
                    reason: None,
                },
                Err(RefundIneligible::NotRejected),
            ),
            (
                RefundDeposit {
                    state: DepositState::Credited,
                    reason: None,
                },
                Err(RefundIneligible::NotRejected),
            ),
            (
                RefundDeposit {
                    state: DepositState::Swept,
                    reason: None,
                },
                Err(RefundIneligible::NotRejected),
            ),
        ];

        for (deposit, expected) in cases {
            assert_eq!(refund_eligibility(deposit), expected);
        }
    }
}

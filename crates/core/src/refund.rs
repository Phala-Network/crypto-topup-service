//! Pure refund eligibility policy.

use crate::deposit::{DepositState, RejectReason};
use crate::money::AtomicAmount;

/// Deposit and route facts needed to decide whether finance may review a refund request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RefundDeposit {
    /// Current deposit state.
    pub state: DepositState,
    /// Stable rejection reason, when the deposit is rejected.
    pub reason: Option<RejectReason>,
    /// Deposited token amount.
    pub amount: AtomicAmount,
    /// Route minimum below which a deposit is non-refundable dust.
    pub min_refund: AtomicAmount,
}

/// Stable reason a deposit cannot enter the refund workflow.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefundIneligible {
    /// Credited value is never refundable through the token refund workflow.
    Credited,
    /// Sanctioned funds require the compliance process, not the refund workflow.
    Sanctioned,
    /// The deposit is below the route's refundable dust floor.
    Dust,
    /// Refund requests cannot race a deposit that is still in the settlement pipeline.
    NotRejected,
    /// No section 15 refundable case matched the supplied facts.
    NoRefundableCase,
}

/// Applies the section 15 refund policy to persisted deposit and route facts.
///
/// Every refundable case is a deposit rejected for a reason other than sanctions and at or above
/// the dust floor: a wrong token (`unsupported_asset`), a below-minimum credit (`below_minimum`),
/// out-of-bounds amounts, and product refusals, which include funds arriving after the workspace
/// closed. Credited deposits, including overpayments beyond the lock tolerance (credited at spot
/// for the full amount), are never refundable.
pub fn refund_eligibility(deposit: RefundDeposit) -> Result<(), RefundIneligible> {
    if matches!(deposit.state, DepositState::Credited | DepositState::Swept) {
        return Err(RefundIneligible::Credited);
    }
    if deposit.reason == Some(RejectReason::Sanctioned) {
        return Err(RefundIneligible::Sanctioned);
    }
    if deposit.amount < deposit.min_refund {
        return Err(RefundIneligible::Dust);
    }
    if deposit.state != DepositState::Rejected {
        return Err(RefundIneligible::NotRejected);
    }
    if deposit.reason.is_some() {
        return Ok(());
    }
    Err(RefundIneligible::NoRefundableCase)
}

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;

    use super::*;

    fn amount(value: u64) -> AtomicAmount {
        AtomicAmount::new(U256::from(value))
    }

    fn deposit() -> RefundDeposit {
        RefundDeposit {
            state: DepositState::Detected,
            reason: None,
            amount: amount(100),
            min_refund: amount(10),
        }
    }

    fn rejected(reason: RejectReason) -> RefundDeposit {
        RefundDeposit {
            state: DepositState::Rejected,
            reason: Some(reason),
            ..deposit()
        }
    }

    #[test]
    fn refund_eligibility_table_covers_policy_boundaries() {
        let cases = [
            (
                "ordinary deposit",
                deposit(),
                Err(RefundIneligible::NotRejected),
            ),
            (
                "wrong token",
                rejected(RejectReason::UnsupportedAsset),
                Ok(()),
            ),
            (
                "product refusal, including late funds to a closed workspace",
                rejected(RejectReason::ProductRefused),
                Ok(()),
            ),
            ("out of bounds", rejected(RejectReason::OutOfBounds), Ok(())),
            (
                "sanctioned",
                rejected(RejectReason::Sanctioned),
                Err(RefundIneligible::Sanctioned),
            ),
            (
                "below minimum, one below the dust floor",
                RefundDeposit {
                    amount: amount(9),
                    ..rejected(RejectReason::BelowMinimum)
                },
                Err(RefundIneligible::Dust),
            ),
            (
                "below minimum, at the dust floor",
                RefundDeposit {
                    amount: amount(10),
                    ..rejected(RejectReason::BelowMinimum)
                },
                Ok(()),
            ),
            (
                "pending unsupported asset is not refundable",
                RefundDeposit {
                    reason: Some(RejectReason::UnsupportedAsset),
                    ..deposit()
                },
                Err(RefundIneligible::NotRejected),
            ),
            (
                "rejected without a reason matches no case",
                RefundDeposit {
                    state: DepositState::Rejected,
                    ..deposit()
                },
                Err(RefundIneligible::NoRefundableCase),
            ),
            (
                "credited value, including an overpayment credited at spot",
                RefundDeposit {
                    state: DepositState::Credited,
                    ..deposit()
                },
                Err(RefundIneligible::Credited),
            ),
            (
                "swept credited value",
                RefundDeposit {
                    state: DepositState::Swept,
                    ..deposit()
                },
                Err(RefundIneligible::Credited),
            ),
        ];

        for (name, input, expected) in cases {
            assert_eq!(refund_eligibility(input), expected, "{name}");
        }
    }
}

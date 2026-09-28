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
/// Every refundable case is at or above the dust floor and not sanctioned: a deposit rejected for
/// a wrong token (`unsupported_asset`), a below-minimum credit (`below_minimum`), out-of-bounds
/// amounts, or a product refusal; and a credited deposit, which only the product can ask to
/// refund, for a credit it did not apply or has reversed (a closed workspace, its own cap, a
/// suspended account). Finance approves every request.
pub fn refund_eligibility(deposit: RefundDeposit) -> Result<(), RefundIneligible> {
    if deposit.reason == Some(RejectReason::Sanctioned) {
        return Err(RefundIneligible::Sanctioned);
    }
    if deposit.amount < deposit.min_refund {
        return Err(RefundIneligible::Dust);
    }
    if matches!(deposit.state, DepositState::Credited | DepositState::Swept) {
        return Ok(());
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
                "credited value the product did not apply",
                RefundDeposit {
                    state: DepositState::Credited,
                    ..deposit()
                },
                Ok(()),
            ),
            (
                "swept credited value the product did not apply",
                RefundDeposit {
                    state: DepositState::Swept,
                    ..deposit()
                },
                Ok(()),
            ),
            (
                "credited dust",
                RefundDeposit {
                    state: DepositState::Credited,
                    amount: amount(9),
                    ..deposit()
                },
                Err(RefundIneligible::Dust),
            ),
        ];

        for (name, input, expected) in cases {
            assert_eq!(refund_eligibility(input), expected, "{name}");
        }
    }
}

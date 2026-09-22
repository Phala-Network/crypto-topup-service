//! Pure refund eligibility policy.

use alloy_primitives::U512;

use crate::deposit::{DepositState, RejectReason};
use crate::money::{AtomicAmount, Bps};
use crate::valuation::UnixSeconds;

/// Workspace lifecycle state relevant to late-fund refunds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefundAccountStatus {
    /// The workspace can still receive and process deposits.
    Active,
    /// The workspace is closed, so later funds must be held for refund.
    Closed,
}

/// Deposit-address kind relevant to lock overpayment policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefundAddressKind {
    /// Reusable persistent deposit address.
    Persistent,
    /// Single-use rate-lock address.
    Lock,
}

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
    /// Owning workspace lifecycle state.
    pub account_status: RefundAccountStatus,
    /// Finalized chain time of the deposit.
    pub block_time: UnixSeconds,
    /// Time the workspace closed, when recorded.
    pub account_closed_at: Option<UnixSeconds>,
    /// Address kind that received the deposit.
    pub address_kind: RefundAddressKind,
    /// Expected amount for a lock address, when present.
    pub lock_amount: Option<AtomicAmount>,
    /// Accepted lock amount tolerance.
    pub lock_tolerance: Bps,
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
    if deposit.reason.is_some()
        || is_late_closed_workspace_fund(deposit)
        || is_lock_overpayment(deposit)
    {
        return Ok(());
    }
    Err(RefundIneligible::NoRefundableCase)
}

fn is_late_closed_workspace_fund(deposit: RefundDeposit) -> bool {
    deposit.account_status == RefundAccountStatus::Closed
        && deposit
            .account_closed_at
            .is_some_and(|closed_at| deposit.block_time > closed_at)
}

fn is_lock_overpayment(deposit: RefundDeposit) -> bool {
    let Some(lock_amount) = deposit.lock_amount else {
        return false;
    };
    if deposit.address_kind != RefundAddressKind::Lock {
        return false;
    }
    let scale = U512::from(10_000_u64);
    let actual = U512::from(deposit.amount.value())
        .checked_mul(scale)
        .unwrap_or(U512::MAX);
    let tolerance_scale = 10_000_u64.saturating_add(u64::from(deposit.lock_tolerance.value()));
    let maximum = U512::from(lock_amount.value())
        .checked_mul(U512::from(tolerance_scale))
        .unwrap_or(U512::MAX);
    actual > maximum
}

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;

    use super::*;

    fn amount(value: u64) -> AtomicAmount {
        AtomicAmount::new(U256::from(value))
    }

    fn bps(value: u16) -> Bps {
        Bps::new(value).expect("valid basis points")
    }

    fn deposit() -> RefundDeposit {
        RefundDeposit {
            state: DepositState::Detected,
            reason: None,
            amount: amount(100),
            min_refund: amount(10),
            account_status: RefundAccountStatus::Active,
            block_time: UnixSeconds::new(200),
            account_closed_at: None,
            address_kind: RefundAddressKind::Persistent,
            lock_amount: None,
            lock_tolerance: bps(100),
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
                "unsupported asset",
                RefundDeposit {
                    state: DepositState::Rejected,
                    reason: Some(RejectReason::UnsupportedAsset),
                    ..deposit()
                },
                Ok(()),
            ),
            (
                "rejected not sanctioned",
                RefundDeposit {
                    state: DepositState::Rejected,
                    reason: Some(RejectReason::ProductRefused),
                    ..deposit()
                },
                Ok(()),
            ),
            (
                "sanctioned",
                RefundDeposit {
                    state: DepositState::Rejected,
                    reason: Some(RejectReason::Sanctioned),
                    ..deposit()
                },
                Err(RefundIneligible::Sanctioned),
            ),
            (
                "dust one below minimum",
                RefundDeposit {
                    state: DepositState::Rejected,
                    reason: Some(RejectReason::BelowMinimum),
                    amount: amount(9),
                    ..deposit()
                },
                Err(RefundIneligible::Dust),
            ),
            (
                "minimum is refundable",
                RefundDeposit {
                    state: DepositState::Rejected,
                    reason: Some(RejectReason::BelowMinimum),
                    amount: amount(10),
                    ..deposit()
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
                "pending closed workspace late funds are not refundable",
                RefundDeposit {
                    account_status: RefundAccountStatus::Closed,
                    account_closed_at: Some(UnixSeconds::new(199)),
                    ..deposit()
                },
                Err(RefundIneligible::NotRejected),
            ),
            (
                "closed workspace deposit exactly at close is not late",
                RefundDeposit {
                    state: DepositState::Rejected,
                    account_status: RefundAccountStatus::Closed,
                    account_closed_at: Some(UnixSeconds::new(200)),
                    ..deposit()
                },
                Err(RefundIneligible::NoRefundableCase),
            ),
            (
                "closed workspace deposit one second after close is late",
                RefundDeposit {
                    state: DepositState::Rejected,
                    account_status: RefundAccountStatus::Closed,
                    account_closed_at: Some(UnixSeconds::new(199)),
                    ..deposit()
                },
                Ok(()),
            ),
            (
                "closed workspace without a close time is not enough",
                RefundDeposit {
                    state: DepositState::Rejected,
                    account_status: RefundAccountStatus::Closed,
                    ..deposit()
                },
                Err(RefundIneligible::NoRefundableCase),
            ),
            (
                "lock at upper tolerance",
                RefundDeposit {
                    state: DepositState::Rejected,
                    amount: amount(101),
                    address_kind: RefundAddressKind::Lock,
                    lock_amount: Some(amount(100)),
                    ..deposit()
                },
                Err(RefundIneligible::NoRefundableCase),
            ),
            (
                "lock beyond upper tolerance",
                RefundDeposit {
                    state: DepositState::Rejected,
                    amount: amount(102),
                    address_kind: RefundAddressKind::Lock,
                    lock_amount: Some(amount(100)),
                    ..deposit()
                },
                Ok(()),
            ),
            (
                "persistent address is not a lock overpayment",
                RefundDeposit {
                    state: DepositState::Rejected,
                    amount: amount(102),
                    lock_amount: Some(amount(100)),
                    ..deposit()
                },
                Err(RefundIneligible::NoRefundableCase),
            ),
            (
                "credited value",
                RefundDeposit {
                    state: DepositState::Credited,
                    account_status: RefundAccountStatus::Closed,
                    ..deposit()
                },
                Err(RefundIneligible::Credited),
            ),
            (
                "swept credited value",
                RefundDeposit {
                    state: DepositState::Swept,
                    account_status: RefundAccountStatus::Closed,
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

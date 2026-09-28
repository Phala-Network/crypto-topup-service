//! PostgreSQL schema migration and repository functions.

mod accounts;
mod addresses;
mod deposits;
mod flushes;
mod outbox;
mod pending;
mod scanner;
mod types;

use sqlx::PgPool;
use sqlx::migrate::Migrator;

pub use accounts::{Account, Customer, get_account, get_customer};
pub(crate) use addresses::list_chain_addresses_with_pause_scopes;
pub use addresses::{Address, get_address, list_chain_addresses};
pub(crate) use deposits::link_deposit_to_flush;
pub use deposits::{
    ApplyTransitionError, ApplyTransitionResult, CanonicalEvidence, ClaimedDeposit, Deposit,
    LockConsumption, NewDeposit, OutboxEvent, StoredValuation, TransitionEffects, TransitionUpdate,
    TransitionWrites, apply_transition, claim_deposit, get_deposit, insert_deposit,
    release_deposit_lease,
};
pub use flushes::{
    Flush, FlushStatus, FlushedEvent, NewFlush, confirm_flush, get_flush_locked,
    has_open_flush_locked, has_sent_flush, has_sent_flush_for_token, insert_flush, insert_flushed,
    insert_planned_flush, link_confirmed_flush, list_active_flush_exclusions, list_flushes,
    lock_flush_plan, lock_operator, mark_flush_reverted_locked, mark_flush_sent, next_flush_nonce,
    next_planned_flush, rebind_planned_flushes, store_flush_replacement_cas, update_sent_evidence,
    upsert_flush_exclusion, void_paused_plan,
};
pub use outbox::{EventObject, NewOutboxEvent, enqueue_in};
pub use pending::{
    HeadCommit, NewPendingTransfer, PendingTransfer, commit_head_scan, list_address_pending,
    list_watched_addresses,
};
pub use scanner::{
    ScanAddress, ScanCommit, commit_confirmed_scan, commit_scan, get_confirmed_cursor, get_cursor,
    list_scan_addresses,
};

/// Embedded SQL migrations for the service database.
pub static MIGRATOR: Migrator = sqlx::migrate!();

/// Applies every pending embedded migration.
pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}

pub(crate) fn state_code(state: topup_core::deposit::DepositState) -> &'static str {
    use topup_core::deposit::DepositState;

    match state {
        DepositState::Detected => "detected",
        DepositState::Confirmed => "confirmed",
        DepositState::Credited => "credited",
        DepositState::Swept => "swept",
        DepositState::Rejected => "rejected",
        DepositState::Reversed => "reversed",
    }
}

pub(crate) fn parse_state(value: &str) -> Result<topup_core::deposit::DepositState, sqlx::Error> {
    use topup_core::deposit::DepositState;

    match value {
        "detected" => Ok(DepositState::Detected),
        "confirmed" => Ok(DepositState::Confirmed),
        "credited" => Ok(DepositState::Credited),
        "swept" => Ok(DepositState::Swept),
        "rejected" => Ok(DepositState::Rejected),
        "reversed" => Ok(DepositState::Reversed),
        other => Err(sqlx::Error::Decode(
            format!("unknown deposit state `{other}`").into(),
        )),
    }
}

pub(crate) fn parse_reason(
    value: Option<&str>,
) -> Result<Option<topup_core::deposit::RejectReason>, sqlx::Error> {
    use topup_core::deposit::RejectReason;

    value
        .map(|reason| match reason {
            "unsupported_asset" => Ok(RejectReason::UnsupportedAsset),
            "below_minimum" => Ok(RejectReason::BelowMinimum),
            "out_of_range" => Ok(RejectReason::OutOfRange),
            "sanctioned" => Ok(RejectReason::Sanctioned),
            "out_of_bounds" => Ok(RejectReason::OutOfBounds),
            other => Err(sqlx::Error::Decode(
                format!("unknown rejection reason `{other}`").into(),
            )),
        })
        .transpose()
}

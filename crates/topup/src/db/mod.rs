//! PostgreSQL schema migration and repository functions.

mod accounts;
mod addresses;
mod deposits;
mod outbox;
mod pending;
mod scanner;
mod sweeps;
mod types;

use sqlx::PgPool;
use sqlx::migrate::Migrator;

pub use accounts::{Account, Customer, get_account, get_customer};
pub use addresses::{Address, get_address, list_chain_addresses};
pub use deposits::{
    ApplyTransitionError, ApplyTransitionResult, CanonicalEvidence, ClaimedDeposit, Deposit,
    LockConsumption, NewDeposit, OutboxEvent, StoredValuation, TransitionEffects, TransitionUpdate,
    TransitionWrites, apply_transition, claim_deposit, get_deposit, insert_deposit,
    release_deposit_lease,
};
pub use outbox::{EventObject, NewOutboxEvent, SYSTEM_ACTOR, enqueue_in};
pub use pending::{
    HeadCommit, NewPendingTransfer, PendingTransfer, commit_head_scan, list_address_pending,
    list_watched_addresses,
};
pub use scanner::{
    ScanAddress, ScanCommit, commit_confirmed_scan, commit_scan, get_confirmed_cursor, get_cursor,
    list_scan_addresses,
};
pub(crate) use sweeps::mark_swept;
pub use sweeps::{FactoryCommit, commit_factory_logs};

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

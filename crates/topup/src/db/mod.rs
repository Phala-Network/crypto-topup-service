//! PostgreSQL schema migration and repository functions.

mod accounts;
mod addresses;
mod audit;
mod deposits;
mod flushes;
mod outbox;
mod products;
mod scanner;
mod settlements;
mod types;

use sqlx::PgPool;
use sqlx::migrate::Migrator;

pub use accounts::{
    Account, NewAccount, create_account, delete_account, get_account, set_account_paused_scopes,
};
pub use addresses::{
    Address, AddressKind, NewAddress, find_active_persistent, find_address_by_chain, get_address,
    insert_address,
};
pub use audit::{AuditEntry, insert_audit};
pub use deposits::{
    ApplyTransitionError, ApplyTransitionResult, CanonicalEvidence, ClaimedDeposit, Deposit,
    LockConsumption, NewDeposit, OutboxEvent, SettlementAdoption, StoredValuation,
    TransitionEffects, TransitionUpdate, TransitionWrites, adopt_settlement_pricing,
    apply_transition, claim_deposit, get_deposit, insert_deposit, release_deposit_lease,
};
pub use flushes::{FlushedEvent, NewFlush, insert_flush, insert_flushed};
pub use outbox::{ClaimedOutboxEvent, NewOutboxEvent, claim_outbox, enqueue, mark_delivered};
pub use products::{
    NewProduct, Product, create_product, delete_product, get_product, set_product_paused_scopes,
};
pub use scanner::{ScanAddress, ScanCommit, commit_scan, get_cursor, list_scan_addresses};
pub use settlements::{
    Settlement, SettlementIntent, SettlementStatus, get_settlement, mark_accepted,
    mark_payload_mismatch, mark_rejected, mark_sent, mark_sent_with_receipt, upsert_intent,
    upsert_intent_in,
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
        DepositState::Cleared => "cleared",
        DepositState::Credited => "credited",
        DepositState::Swept => "swept",
        DepositState::Rejected => "rejected",
    }
}

pub(crate) fn parse_state(value: &str) -> Result<topup_core::deposit::DepositState, sqlx::Error> {
    use topup_core::deposit::DepositState;

    match value {
        "detected" => Ok(DepositState::Detected),
        "confirmed" => Ok(DepositState::Confirmed),
        "cleared" => Ok(DepositState::Cleared),
        "credited" => Ok(DepositState::Credited),
        "swept" => Ok(DepositState::Swept),
        "rejected" => Ok(DepositState::Rejected),
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
            "product_refused" => Ok(RejectReason::ProductRefused),
            other => Err(sqlx::Error::Decode(
                format!("unknown rejection reason `{other}`").into(),
            )),
        })
        .transpose()
}

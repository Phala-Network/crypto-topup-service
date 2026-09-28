//! Credited-to-swept deposit step: a credited deposit waits for its merchant's sweep.

use async_trait::async_trait;
use serde_json::json;
use topup_core::deposit::{StepOutcome, WaitReason};

use crate::db::Deposit;
use crate::pump::{Step, StepResult};

/// Credited-state pump step: a credited deposit waits for its merchant's sweep.
///
/// The service sends no transactions. The finalized scanner indexes the factory's `Flushed`
/// events, whoever sent them, and marks a final credited deposit `swept` in the same transaction
/// ([`crate::db::commit_factory_logs`]), so this step only records the wait.
pub struct SweepStep;

#[async_trait]
impl Step for SweepStep {
    async fn run(&self, _deposit: &Deposit) -> StepResult {
        StepResult::new(
            StepOutcome::Wait {
                reason: WaitReason::FlushNotConfirmed,
            },
            json!({"outcome": "wait", "reason": "flush_not_confirmed"}),
        )
    }
}

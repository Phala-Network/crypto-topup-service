use async_trait::async_trait;
use serde_json::json;
use topup_core::deposit::{StepOutcome, WaitReason};

use crate::db::Deposit;
use crate::pump::{Step, StepResult};

/// Credited-state pump step which advances once a confirmed later flush is linked.
pub struct SweepStep;

#[async_trait]
impl Step for SweepStep {
    async fn run(&self, deposit: &Deposit) -> StepResult {
        if let Some(flush_id) = deposit.flush_id {
            StepResult::new(
                StepOutcome::Advance,
                json!({"outcome": "advance", "flush_id": flush_id}),
            )
        } else {
            StepResult::new(
                StepOutcome::Wait {
                    reason: WaitReason::FlushNotConfirmed,
                },
                json!({"outcome": "wait", "reason": "flush_not_confirmed"}),
            )
        }
    }
}

//! Cleared-deposit settlement step.

use async_trait::async_trait;
use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use topup_adapters::settlement::http::{
    SettlementAnswer, SettlementClientError, SettlementHttpClient, SettlementRequest,
};
use topup_core::deposit::{RejectReason, RetryError, StepOutcome, WaitReason};
use topup_core::money::PRICE_SCALE;
use uuid::Uuid;

use crate::db::{self, OutboxEvent, SettlementIntent, SettlementStatus};
use crate::pump::{Step, StepResult};

/// Product settlement operation for a deposit in `cleared`.
#[derive(Clone)]
pub struct SettleStep {
    pool: PgPool,
    client_timeout: std::time::Duration,
    signer: topup_adapters::signer::actor::SignerHandle,
    client_override: Option<std::sync::Arc<dyn topup_adapters::settlement::http::SettlementApi>>,
}

impl SettleStep {
    /// Creates a settlement step backed by PostgreSQL and one product client.
    #[must_use]
    pub fn new(
        pool: PgPool,
        signer: topup_adapters::signer::actor::SignerHandle,
        client_timeout: std::time::Duration,
    ) -> Self {
        Self {
            pool,
            client_timeout,
            signer,
            client_override: None,
        }
    }

    /// Creates a settlement step with a mockable product API.
    #[must_use]
    pub fn with_api(
        pool: PgPool,
        signer: topup_adapters::signer::actor::SignerHandle,
        client: std::sync::Arc<dyn topup_adapters::settlement::http::SettlementApi>,
    ) -> Self {
        Self {
            pool,
            client_timeout: std::time::Duration::from_secs(1),
            signer,
            client_override: Some(client),
        }
    }

    async fn run_inner(&self, deposit: &db::Deposit) -> Result<StepResult, SettleStepError> {
        let account = db::get_account(&self.pool, deposit.account_id)
            .await?
            .ok_or(SettleStepError::MissingAccount)?;
        let address = db::get_address(&self.pool, deposit.address_id)
            .await?
            .ok_or(SettleStepError::MissingAddress)?;
        let product = db::get_product(&self.pool, account.product_id)
            .await?
            .ok_or(SettleStepError::MissingProduct)?;
        let client: std::sync::Arc<dyn topup_adapters::settlement::http::SettlementApi> =
            match &self.client_override {
                Some(client) => std::sync::Arc::clone(client),
                None => std::sync::Arc::new(
                    SettlementHttpClient::new(
                        &product.settlement_url,
                        self.signer.clone(),
                        self.client_timeout,
                    )
                    .map_err(SettleStepError::Client)?,
                ),
            };
        let payload = SettlementPayload::from_deposit(
            deposit,
            &account.external_id,
            address.address,
            address.lock_ref.clone(),
        )?;
        let payload = serde_json::to_value(payload).map_err(|_| SettleStepError::Encode)?;
        let key = format!("deposit:{}", deposit.id);
        let mut intent_transaction = self.pool.begin().await?;
        let settlement = db::upsert_intent_in(
            &mut intent_transaction,
            &SettlementIntent {
                deposit_id: deposit.id,
                product_id: account.product_id,
                key: key.clone(),
                payload,
            },
        )
        .await?;

        if settlement.key != key || settlement.product_id != account.product_id {
            intent_transaction.commit().await?;
            return Ok(invariant_result("settlement_identity_mismatch"));
        }
        let expected_payload = serde_json::to_value(SettlementPayload::from_deposit(
            deposit,
            &account.external_id,
            address.address,
            address.lock_ref,
        )?)
        .map_err(|_| SettleStepError::Encode)?;
        if settlement.payload != expected_payload {
            intent_transaction.commit().await?;
            return Ok(invariant_result("settlement_payload_changed"));
        }
        intent_transaction.commit().await?;
        let request = SettlementRequest {
            idempotency_key: settlement.key.clone(),
            payload: settlement.payload.clone(),
        };
        if settlement.status == SettlementStatus::Sent {
            match client.get_by_key(&settlement.key).await {
                Ok(Some(answer)) => return self.apply_answer(deposit, answer).await,
                Ok(None) if settlement.receipt.as_ref().is_some_and(is_payload_mismatch) => {
                    return Ok(invariant_result("settlement_payload_mismatch"));
                }
                Ok(None) => {}
                Err(error) => {
                    return Ok(transport_result("settlement_get_failed", &error));
                }
            }
        }

        match client.post(&request).await {
            Ok(answer) => self.apply_answer(deposit, answer).await,
            Err(error) => {
                db::mark_sent(&self.pool, deposit.id).await?;
                Ok(transport_result("settlement_post_failed", &error))
            }
        }
    }

    async fn apply_answer(
        &self,
        deposit: &db::Deposit,
        answer: SettlementAnswer,
    ) -> Result<StepResult, SettleStepError> {
        match answer {
            SettlementAnswer::Accepted { destination_tx_id } => {
                let receipt = json!({
                    "status": "accepted",
                    "destination_tx_id": &destination_tx_id,
                });
                db::mark_accepted(&self.pool, deposit.id, &destination_tx_id, &receipt).await?;
                Ok(StepResult {
                    outcome: StepOutcome::Advance,
                    evidence: receipt.clone(),
                    events: vec![event(
                        "deposit.credited",
                        json!({
                            "deposit_id": deposit.id,
                            "destination_tx_id": receipt["destination_tx_id"],
                            "amount_minor": deposit.credit_minor.map(|amount| amount.value().to_string()),
                            "unit": "USD",
                        }),
                    )],
                })
            }
            SettlementAnswer::Processing | SettlementAnswer::Conflict409 => {
                db::mark_sent(&self.pool, deposit.id).await?;
                Ok(StepResult::new(
                    StepOutcome::Wait {
                        reason: WaitReason::ProductProcessing,
                    },
                    json!({"outcome": "wait", "reason": "product_processing"}),
                ))
            }
            SettlementAnswer::Rejected { reason } => {
                let receipt = json!({"status": "rejected", "reason": reason});
                db::mark_rejected(&self.pool, deposit.id, &receipt).await?;
                Ok(StepResult {
                    outcome: StepOutcome::Reject(RejectReason::ProductRefused),
                    evidence: receipt.clone(),
                    events: vec![event(
                        "deposit.rejected",
                        json!({
                            "deposit_id": deposit.id,
                            "reason": RejectReason::ProductRefused.code(),
                            "product_reason": receipt["reason"],
                        }),
                    )],
                })
            }
            SettlementAnswer::PayloadMismatch422 => {
                db::mark_sent_with_receipt(
                    &self.pool,
                    deposit.id,
                    &json!({"status": "payload_mismatch"}),
                )
                .await?;
                Ok(invariant_result("settlement_payload_mismatch"))
            }
            SettlementAnswer::Unknown { status, body } => {
                db::mark_sent(&self.pool, deposit.id).await?;
                Ok(StepResult::new(
                    StepOutcome::Retry {
                        error: RetryError::Transient,
                    },
                    json!({
                        "outcome": "retry",
                        "error": "unknown_settlement_response",
                        "status": status,
                        "body": body,
                    }),
                ))
            }
        }
    }
}

#[async_trait]
impl Step for SettleStep {
    async fn run(&self, deposit: &db::Deposit) -> StepResult {
        match self.run_inner(deposit).await {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(deposit_id = %deposit.id, %error, "settlement step failed");
                StepResult::new(
                    StepOutcome::Retry {
                        error: error.retry_error(),
                    },
                    json!({"outcome": "retry", "error": error.code()}),
                )
            }
        }
    }
}

#[derive(Debug)]
enum SettleStepError {
    Database(sqlx::Error),
    MissingAccount,
    MissingAddress,
    MissingProduct,
    MissingField(&'static str),
    Encode,
    Client(SettlementClientError),
}

impl SettleStepError {
    const fn retry_error(&self) -> RetryError {
        match self {
            Self::Database(_) => RetryError::Transient,
            Self::Client(error) if !matches!(error, SettlementClientError::Signer(_)) => {
                RetryError::Transient
            }
            Self::MissingAccount
            | Self::MissingAddress
            | Self::MissingProduct
            | Self::MissingField(_)
            | Self::Encode
            | Self::Client(_) => RetryError::InvariantViolation,
        }
    }

    const fn code(&self) -> &'static str {
        match self {
            Self::Database(_) => "settlement_database_error",
            Self::MissingAccount => "settlement_account_missing",
            Self::MissingAddress => "settlement_address_missing",
            Self::MissingProduct => "settlement_product_missing",
            Self::MissingField(_) => "settlement_input_missing",
            Self::Encode => "settlement_payload_encode_failed",
            Self::Client(_) => "settlement_client_configuration_failed",
        }
    }
}

impl std::fmt::Display for SettleStepError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => std::fmt::Display::fmt(error, formatter),
            Self::MissingAccount => formatter.write_str("settlement account is missing"),
            Self::MissingAddress => formatter.write_str("settlement address is missing"),
            Self::MissingProduct => formatter.write_str("settlement product is missing"),
            Self::MissingField(field) => write!(formatter, "settlement field `{field}` is missing"),
            Self::Encode => formatter.write_str("failed to encode settlement payload"),
            Self::Client(error) => std::fmt::Display::fmt(error, formatter),
        }
    }
}

impl std::error::Error for SettleStepError {}

impl From<sqlx::Error> for SettleStepError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

#[derive(Serialize)]
struct SettlementPayload {
    version: u8,
    idempotency_key: String,
    account_id: String,
    unit: &'static str,
    amount_minor: String,
    source: &'static str,
    evidence: SettlementEvidence,
}

#[derive(Serialize)]
struct SettlementEvidence {
    chain_id: u64,
    asset_contract: String,
    route: String,
    route_version: u64,
    tx_hash: String,
    log_index: u64,
    to: String,
    amount_atomic: String,
    price_scaled: String,
    price_scale: u8,
    valuation_at: chrono::DateTime<Utc>,
    lock_ref: Option<String>,
}

impl SettlementPayload {
    fn from_deposit(
        deposit: &db::Deposit,
        external_id: &str,
        to: alloy_primitives::Address,
        lock_ref: Option<String>,
    ) -> Result<Self, SettleStepError> {
        let route = deposit
            .route
            .clone()
            .ok_or(SettleStepError::MissingField("route"))?;
        let route_version = deposit
            .route_version
            .ok_or(SettleStepError::MissingField("route_version"))?;
        let credit_minor = deposit
            .credit_minor
            .ok_or(SettleStepError::MissingField("credit_minor"))?;
        let price_scaled = deposit
            .price_scaled
            .ok_or(SettleStepError::MissingField("price_scaled"))?;
        let valuation_at = deposit
            .valuation_at
            .ok_or(SettleStepError::MissingField("valuation_at"))?;
        Ok(Self {
            version: 1,
            idempotency_key: format!("deposit:{}", deposit.id),
            account_id: external_id.to_owned(),
            unit: "USD",
            amount_minor: credit_minor.value().to_string(),
            source: "crypto_deposit",
            evidence: SettlementEvidence {
                chain_id: deposit.chain_id,
                asset_contract: format!("{:#x}", deposit.asset_contract),
                route,
                route_version,
                tx_hash: format!("{:#x}", deposit.tx_hash),
                log_index: deposit.log_index,
                to: format!("{to:#x}"),
                amount_atomic: deposit.amount_atomic.value().to_string(),
                price_scaled: price_scaled.to_string(),
                price_scale: PRICE_SCALE,
                valuation_at,
                lock_ref,
            },
        })
    }
}

fn event(event_type: &str, payload: Value) -> OutboxEvent {
    OutboxEvent {
        id: Uuid::new_v4(),
        event_type: event_type.to_owned(),
        payload,
        next_attempt_at: Utc::now(),
    }
}

fn invariant_result(error: &str) -> StepResult {
    StepResult::new(
        StepOutcome::Retry {
            error: RetryError::InvariantViolation,
        },
        json!({
            "outcome": "retry",
            "error": error,
            "alert_level": "alert",
        }),
    )
}

fn transport_result(error: &str, source: &SettlementClientError) -> StepResult {
    let kind = if matches!(source, SettlementClientError::Signer(_)) {
        RetryError::InvariantViolation
    } else {
        RetryError::Transient
    };
    StepResult::new(
        StepOutcome::Retry { error: kind },
        json!({"outcome": "retry", "error": error}),
    )
}

fn is_payload_mismatch(receipt: &Value) -> bool {
    receipt.get("status").and_then(Value::as_str) == Some("payload_mismatch")
}

//! Cleared-deposit settlement step.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use topup_adapters::settlement::http::{
    SettlementAnswer, SettlementApi, SettlementClient, SettlementClientError, SettlementRequest,
};
use topup_adapters::signer::actor::SignerHandle;
use topup_core::deposit::{RejectReason, RetryError, StepOutcome, WaitReason};
use topup_core::money::PRICE_SCALE;
use uuid::Uuid;

use crate::db::{self, OutboxEvent, SettlementIntent, SettlementStatus};
use crate::pause::{self, PauseScopeSources};
use crate::pump::{Step, StepResult};
use crate::routes::RouteSet;

/// Product settlement operation for a deposit in `cleared`.
#[derive(Clone)]
pub struct SettleStep {
    pool: PgPool,
    routes: Arc<RouteSet>,
    client_timeout: Duration,
    signer: SignerHandle,
    client_override: Option<Arc<dyn SettlementApi>>,
}

impl SettleStep {
    /// Creates a settlement step that calls each product at its attested route destination.
    #[must_use]
    pub fn new(
        pool: PgPool,
        routes: Arc<RouteSet>,
        signer: SignerHandle,
        client_timeout: Duration,
    ) -> Self {
        Self {
            pool,
            routes,
            client_timeout,
            signer,
            client_override: None,
        }
    }

    /// Creates a settlement step with a mockable product API.
    #[must_use]
    pub fn with_api(pool: PgPool, signer: SignerHandle, client: Arc<dyn SettlementApi>) -> Self {
        Self {
            pool,
            routes: Arc::default(),
            client_timeout: Duration::from_secs(1),
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
        let client: Arc<dyn SettlementApi> = match &self.client_override {
            Some(client) => Arc::clone(client),
            None => {
                let destination = self
                    .routes
                    .destination(&product.slug)
                    .ok_or(SettleStepError::Destination)?;
                Arc::new(
                    SettlementClient::new(
                        &destination.settlement_url,
                        self.signer.clone(),
                        self.client_timeout,
                    )
                    .map_err(SettleStepError::Client)?,
                )
            }
        };
        let lock_ref = settlement_lock_ref(deposit, address.lock_ref.as_deref());
        let payload = SettlementPayload::from_deposit(
            deposit,
            &account.external_id,
            address.address,
            lock_ref.clone(),
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
            lock_ref,
        )?)
        .map_err(|_| SettleStepError::Encode)?;
        if settlement.status == SettlementStatus::Intent && settlement.payload != expected_payload {
            intent_transaction.commit().await?;
            return Ok(invariant_result("settlement_payload_changed"));
        }
        intent_transaction.commit().await?;

        if let Some(answer) = terminal_answer(&settlement)? {
            return self
                .apply_answer(deposit, account.product_id, &account.external_id, answer)
                .await;
        }

        let request = SettlementRequest {
            idempotency_key: settlement.key.clone(),
            payload: settlement.payload.clone(),
        };
        if settlement.status != SettlementStatus::Intent {
            match client.get_by_key(&settlement.key).await {
                Ok(Some(answer)) => {
                    return self
                        .apply_answer(deposit, account.product_id, &account.external_id, answer)
                        .await;
                }
                Ok(None) => {}
                Err(error) => {
                    return Ok(transport_result("settlement_get_failed", &error));
                }
            }
        }

        if settlement.resend_forbidden {
            return Ok(invariant_result("settlement_payload_mismatch"));
        }

        if settlement_paused(
            &self.pool,
            deposit,
            &account.paused_scopes,
            &product.paused_scopes,
        )
        .await?
        {
            return Ok(StepResult::new(
                StepOutcome::Wait {
                    reason: WaitReason::Paused,
                },
                json!({"outcome": "wait", "reason": "settlement_paused"}),
            ));
        }

        db::mark_sent(&self.pool, deposit.id).await?;

        match client.post(&request).await {
            Ok(SettlementAnswer::PayloadMismatch422) => {
                db::mark_payload_mismatch(&self.pool, deposit.id).await?;
                match client.get_by_key(&settlement.key).await {
                    Ok(Some(answer)) => {
                        self.apply_answer(deposit, account.product_id, &account.external_id, answer)
                            .await
                    }
                    Ok(None) => Ok(invariant_result("settlement_payload_mismatch")),
                    Err(error) => Ok(transport_result("settlement_get_failed", &error)),
                }
            }
            Ok(answer) => {
                self.apply_answer(deposit, account.product_id, &account.external_id, answer)
                    .await
            }
            Err(error) => Ok(transport_result("settlement_post_failed", &error)),
        }
    }

    async fn apply_answer(
        &self,
        deposit: &db::Deposit,
        product_id: Uuid,
        account_external_id: &str,
        answer: SettlementAnswer,
    ) -> Result<StepResult, SettleStepError> {
        adopt_answer(&self.pool, deposit, product_id, account_external_id, answer).await
    }
}

pub(crate) fn validate_answer_identity(
    deposit: &db::Deposit,
    account_external_id: &str,
    answer: &SettlementAnswer,
) -> Result<(), SettleStepError> {
    match answer {
        SettlementAnswer::Accepted { payload, .. }
        | SettlementAnswer::Processing { payload }
        | SettlementAnswer::Rejected { payload, .. } => {
            SettlementPayload::from_product(payload.clone(), deposit, account_external_id)?;
        }
        SettlementAnswer::Conflict409
        | SettlementAnswer::PayloadMismatch422
        | SettlementAnswer::Unknown { .. } => {}
    }
    Ok(())
}

pub(crate) async fn adopt_answer(
    pool: &PgPool,
    deposit: &db::Deposit,
    product_id: Uuid,
    account_external_id: &str,
    answer: SettlementAnswer,
) -> Result<StepResult, SettleStepError> {
    match answer {
        SettlementAnswer::Accepted {
            destination_tx_id,
            payload,
        } => {
            let payload = SettlementPayload::from_product(payload, deposit, account_external_id)?;
            let receipt = json!({
                "status": "accepted",
                "destination_tx_id": &destination_tx_id,
                "payload": &payload,
            });
            db::mark_accepted(pool, deposit.id, &destination_tx_id, &receipt).await?;
            let pricing = payload.pricing()?;
            db::adopt_settlement_pricing(
                pool,
                deposit.id,
                pricing.amount_minor,
                pricing.price_scaled,
                pricing.valuation_at,
            )
            .await?;
            let evidence = terminal_evidence("accepted", deposit, &payload, &receipt)?;
            Ok(StepResult {
                outcome: StepOutcome::Advance,
                evidence,
                events: vec![event(
                    "deposit.credited",
                    json!({
                        "product_id": product_id,
                        "deposit_id": deposit.id,
                        "chain_id": deposit.chain_id,
                        "state": "credited",
                        "route": deposit.route.as_deref(),
                        "destination_tx_id": receipt["destination_tx_id"],
                        "amount_minor": payload.amount_minor,
                        "unit": payload.unit,
                        "price_scaled": payload.evidence.price_scaled,
                        "price_scale": payload.evidence.price_scale,
                        "valuation_at": payload.evidence.valuation_at,
                    }),
                )],
                effects: db::TransitionEffects::default(),
            })
        }
        SettlementAnswer::Processing { payload } => {
            let payload = SettlementPayload::from_product(payload, deposit, account_external_id)?;
            db::mark_sent_with_receipt(
                pool,
                deposit.id,
                &json!({"status": "processing", "payload": payload}),
            )
            .await?;
            Ok(StepResult::new(
                StepOutcome::Wait {
                    reason: WaitReason::ProductProcessing,
                },
                json!({"outcome": "wait", "reason": "product_processing"}),
            ))
        }
        SettlementAnswer::Conflict409 => {
            db::mark_sent_with_receipt(pool, deposit.id, &json!({"status": "conflict"})).await?;
            Ok(StepResult::new(
                StepOutcome::Wait {
                    reason: WaitReason::ProductProcessing,
                },
                json!({"outcome": "wait", "reason": "product_processing"}),
            ))
        }
        SettlementAnswer::Rejected { reason, payload } => {
            let payload = SettlementPayload::from_product(payload, deposit, account_external_id)?;
            let receipt = json!({"status": "rejected", "reason": reason, "payload": &payload});
            db::mark_rejected(pool, deposit.id, &receipt).await?;
            let pricing = payload.pricing()?;
            db::adopt_settlement_pricing(
                pool,
                deposit.id,
                pricing.amount_minor,
                pricing.price_scaled,
                pricing.valuation_at,
            )
            .await?;
            let evidence = terminal_evidence("rejected", deposit, &payload, &receipt)?;
            Ok(StepResult {
                outcome: StepOutcome::Reject(RejectReason::ProductRefused),
                evidence,
                events: vec![event(
                    "deposit.rejected",
                    json!({
                        "product_id": product_id,
                        "deposit_id": deposit.id,
                        "chain_id": deposit.chain_id,
                        "state": "rejected",
                        "route": deposit.route.as_deref(),
                        "reason": RejectReason::ProductRefused.code(),
                        "product_reason": receipt["reason"],
                    }),
                )],
                effects: db::TransitionEffects::default(),
            })
        }
        SettlementAnswer::PayloadMismatch422 => {
            db::mark_payload_mismatch(pool, deposit.id).await?;
            Ok(invariant_result("settlement_payload_mismatch"))
        }
        SettlementAnswer::Unknown { status, body } => {
            db::mark_sent_with_receipt(
                pool,
                deposit.id,
                &json!({"status": "unknown", "http_status": status, "body": body}),
            )
            .await?;
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

#[async_trait]
impl Step for SettleStep {
    async fn run(&self, deposit: &db::Deposit) -> StepResult {
        match self.run_inner(deposit).await {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(
                    deposit_id = %deposit.id,
                    chain_id = deposit.chain_id,
                    state = ?deposit.state,
                    route = deposit.route.as_deref().unwrap_or_default(),
                    %error,
                    "settlement step failed"
                );
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

#[derive(Debug, thiserror::Error)]
pub(crate) enum SettleStepError {
    #[error("{0}")]
    Database(#[from] sqlx::Error),
    #[error("settlement account is missing")]
    MissingAccount,
    #[error("settlement address is missing")]
    MissingAddress,
    #[error("settlement product is missing")]
    MissingProduct,
    #[error("no single attested route destination is configured for the product")]
    Destination,
    #[error("settlement field `{0}` is missing")]
    MissingField(&'static str),
    #[error("product settlement payload field `{0}` is invalid")]
    InvalidProductPayload(&'static str),
    #[error("stored settlement answer field `{0}` is invalid")]
    InvalidStoredAnswer(&'static str),
    #[error("failed to encode settlement payload")]
    Encode,
    #[error("{0}")]
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
            | Self::Destination
            | Self::MissingField(_)
            | Self::InvalidProductPayload(_)
            | Self::InvalidStoredAnswer(_)
            | Self::Encode
            | Self::Client(_) => RetryError::InvariantViolation,
        }
    }

    pub(crate) const fn code(&self) -> &'static str {
        match self {
            Self::Database(_) => "settlement_database_error",
            Self::MissingAccount => "settlement_account_missing",
            Self::MissingAddress => "settlement_address_missing",
            Self::MissingProduct => "settlement_product_missing",
            Self::Destination => "settlement_destination_unconfigured",
            Self::MissingField(_) => "settlement_input_missing",
            Self::InvalidProductPayload(_) => "settlement_product_payload_invalid",
            Self::InvalidStoredAnswer(_) => "settlement_stored_answer_invalid",
            Self::Encode => "settlement_payload_encode_failed",
            Self::Client(_) => "settlement_client_configuration_failed",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SettlementPayload {
    version: u8,
    idempotency_key: String,
    account_id: String,
    unit: String,
    amount_minor: String,
    source: String,
    evidence: SettlementEvidence,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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
            unit: "USD".to_owned(),
            amount_minor: credit_minor.value().to_string(),
            source: "crypto_deposit".to_owned(),
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

    fn from_product(
        value: Value,
        deposit: &db::Deposit,
        account_external_id: &str,
    ) -> Result<Self, SettleStepError> {
        let payload: Self = serde_json::from_value(value)
            .map_err(|_| SettleStepError::InvalidProductPayload("schema"))?;
        let expected_key = format!("deposit:{}", deposit.id);
        if payload.idempotency_key != expected_key {
            return Err(SettleStepError::InvalidProductPayload("idempotency_key"));
        }
        if payload.account_id != account_external_id {
            return Err(SettleStepError::InvalidProductPayload("account_id"));
        }
        if payload.evidence.chain_id != deposit.chain_id {
            return Err(SettleStepError::InvalidProductPayload("chain_id"));
        }
        if payload.evidence.tx_hash != format!("{:#x}", deposit.tx_hash) {
            return Err(SettleStepError::InvalidProductPayload("tx_hash"));
        }
        if payload.evidence.log_index != deposit.log_index {
            return Err(SettleStepError::InvalidProductPayload("log_index"));
        }
        Ok(payload)
    }

    fn pricing(&self) -> Result<ProductPricing, SettleStepError> {
        let amount_minor = self
            .amount_minor
            .parse()
            .map_err(|_| SettleStepError::InvalidProductPayload("amount_minor"))?;
        let price_scaled = self
            .evidence
            .price_scaled
            .parse()
            .map_err(|_| SettleStepError::InvalidProductPayload("price_scaled"))?;
        if price_scaled == 0 || self.evidence.price_scale != PRICE_SCALE {
            return Err(SettleStepError::InvalidProductPayload("price_scale"));
        }
        Ok(ProductPricing {
            amount_minor,
            price_scaled,
            valuation_at: self.evidence.valuation_at,
        })
    }
}

#[derive(Clone, Copy)]
struct ProductPricing {
    amount_minor: u64,
    price_scaled: u64,
    valuation_at: chrono::DateTime<Utc>,
}

fn event(event_type: &str, payload: Value) -> OutboxEvent {
    OutboxEvent {
        id: Uuid::new_v4(),
        event_type: event_type.to_owned(),
        payload,
        next_attempt_at: Utc::now(),
    }
}

fn settlement_lock_ref(deposit: &db::Deposit, lock_ref: Option<&str>) -> Option<String> {
    (deposit.price_source.as_deref() == Some("lock"))
        .then(|| lock_ref.map(str::to_owned))
        .flatten()
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

fn terminal_answer(
    settlement: &db::Settlement,
) -> Result<Option<SettlementAnswer>, SettleStepError> {
    let payload = || {
        settlement
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.get("payload"))
            .cloned()
            .unwrap_or_else(|| settlement.payload.clone())
    };
    match settlement.status {
        SettlementStatus::Accepted => {
            let destination_tx_id = settlement
                .destination_tx_id
                .clone()
                .filter(|value| !value.is_empty())
                .ok_or(SettleStepError::InvalidStoredAnswer("destination_tx_id"))?;
            Ok(Some(SettlementAnswer::Accepted {
                destination_tx_id,
                payload: payload(),
            }))
        }
        SettlementStatus::Rejected => {
            let reason = settlement
                .receipt
                .as_ref()
                .and_then(|receipt| receipt.get("reason"))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or(SettleStepError::InvalidStoredAnswer("reason"))?
                .to_owned();
            Ok(Some(SettlementAnswer::Rejected {
                reason,
                payload: payload(),
            }))
        }
        SettlementStatus::Intent | SettlementStatus::Sent => Ok(None),
    }
}

async fn settlement_paused(
    pool: &PgPool,
    deposit: &db::Deposit,
    account_scopes: &[String],
    product_scopes: &[String],
) -> Result<bool, sqlx::Error> {
    let Some(route) = deposit.route.as_deref() else {
        return Ok(
            PauseScopeSources::from_codes(account_scopes, product_scopes, &[])?
                .contains(topup_core::screening::PauseScope::Settlement),
        );
    };
    let route_scopes = pause::route_pause_scopes(pool, route).await?;
    Ok(
        PauseScopeSources::from_codes(account_scopes, product_scopes, &route_scopes)?
            .contains(topup_core::screening::PauseScope::Settlement),
    )
}

fn terminal_evidence(
    status: &str,
    deposit: &db::Deposit,
    payload: &SettlementPayload,
    receipt: &Value,
) -> Result<Value, SettleStepError> {
    let pricing = payload.pricing()?;
    Ok(json!({
        "status": status,
        "receipt": receipt,
        "pricing": {
            "local": {
                "amount_minor": deposit.credit_minor.map(|amount| amount.value()),
                "price_scaled": deposit.price_scaled,
                "valuation_at": deposit.valuation_at,
            },
            "product": {
                "amount_minor": pricing.amount_minor,
                "price_scaled": pricing.price_scaled,
                "valuation_at": pricing.valuation_at,
            },
        },
    }))
}

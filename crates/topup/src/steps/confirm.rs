//! Detected-to-confirmed deposit step.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use topup_adapters::chain::evm::{ChainError, ChainReader, FinalizedReader, TransferLog};
use topup_adapters::pricing::PriceSource;
use topup_core::deposit::{RejectReason, RetryError, StepOutcome, WaitReason};
use topup_core::money::{AtomicAmount, MinorAmount, PRICE_SCALE, ScaledPrice, credit};
use topup_core::route::RouteFile;
use topup_core::valuation::{
    LockTerms, RouteValuation, UnixSeconds, ValuationError, ValuationSource, value_deposit,
};
use uuid::Uuid;

use crate::db::{
    CanonicalEvidence, Deposit, LockConsumption, OutboxEvent, StoredValuation, TransitionEffects,
};
use crate::locks::pricing::{PricingRuntime, ValidatedQuote, valuation_error_code};
use crate::pump::{Step, StepResult};
use crate::routes::RouteSet;

#[async_trait]
trait FinalityReader: Send + Sync {
    async fn finalized_head(&self) -> Result<u64, ChainError>;
    async fn transfer_log_by_identity(
        &self,
        tx_hash: B256,
        log_index: u64,
    ) -> Result<Option<TransferLog>, ChainError>;
}

#[async_trait]
impl<R> FinalityReader for R
where
    R: ChainReader + Send + Sync,
{
    async fn finalized_head(&self) -> Result<u64, ChainError> {
        Ok(ChainReader::finalized_head(self).await?.number)
    }

    async fn transfer_log_by_identity(
        &self,
        tx_hash: B256,
        log_index: u64,
    ) -> Result<Option<TransferLog>, ChainError> {
        ChainReader::transfer_log_by_identity(self, tx_hash, log_index).await
    }
}

struct ChainPair {
    primary: Arc<dyn FinalityReader>,
    secondary: Arc<dyn FinalityReader>,
}

struct RouteRuntime {
    route: RouteFile,
    pricing: PricingRuntime,
}

/// Invalid detected-step runtime configuration.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{0}")]
pub struct ConfirmConfigError(String);

/// Two-provider finality, quote validation, and credit computation for detected deposits.
pub struct ConfirmStep {
    context_lookup: Arc<dyn ContextLookup>,
    routes: BTreeMap<(String, u64), RouteRuntime>,
    asset_routes: BTreeMap<(u64, Address), (String, u64)>,
    chains: BTreeMap<u64, ChainPair>,
}

impl ConfirmStep {
    /// Builds production adapters for every loaded route version and each chain's providers.
    pub fn from_routes(pool: PgPool, routes: &RouteSet) -> Result<Self, ConfirmConfigError> {
        let mut runtimes = BTreeMap::new();
        for route in routes.routes() {
            let pricing = PricingRuntime::configured(route).map_err(ConfirmConfigError)?;
            runtimes.insert(
                (route.route.clone(), route.version),
                RouteRuntime {
                    route: route.clone(),
                    pricing,
                },
            );
        }
        let asset_routes = routes
            .current()
            .map(|route| {
                (
                    (route.chain.chain_id, route.asset.contract),
                    (route.route.clone(), route.version),
                )
            })
            .collect();
        let mut chains = BTreeMap::new();
        for chain_id in routes.chain_ids() {
            let reader = |index| -> Result<Arc<dyn FinalityReader>, ConfirmConfigError> {
                let client = routes
                    .provider(chain_id, index)
                    .map_err(|error| ConfirmConfigError(error.to_string()))?;
                Ok(Arc::new(FinalizedReader::new(Arc::clone(client))))
            };
            chains.insert(
                chain_id,
                ChainPair {
                    primary: reader(0)?,
                    secondary: reader(1)?,
                },
            );
        }
        Ok(Self {
            context_lookup: Arc::new(PostgresContextLookup(pool)),
            routes: runtimes,
            asset_routes,
            chains,
        })
    }

    /// Builds a one-route step with injected chain and price adapters for tests.
    #[allow(clippy::too_many_arguments)]
    pub fn single<R1, R2>(
        pool: PgPool,
        route: RouteFile,
        primary_chain: R1,
        secondary_chain: R2,
        primary_price: Arc<dyn PriceSource>,
        check_price: Option<Arc<dyn PriceSource>>,
        fx_price: Option<Arc<dyn PriceSource>>,
    ) -> Self
    where
        R1: ChainReader + Send + Sync + 'static,
        R2: ChainReader + Send + Sync + 'static,
    {
        let chain_id = route.chain.chain_id;
        let asset_contract = route.asset.contract;
        let key = (route.route.clone(), route.version);
        Self {
            context_lookup: Arc::new(PostgresContextLookup(pool)),
            routes: BTreeMap::from([(
                key.clone(),
                RouteRuntime {
                    route,
                    pricing: PricingRuntime::injected(primary_price, check_price, fx_price),
                },
            )]),
            asset_routes: BTreeMap::from([((chain_id, asset_contract), key)]),
            chains: BTreeMap::from([(
                chain_id,
                ChainPair {
                    primary: Arc::new(primary_chain),
                    secondary: Arc::new(secondary_chain),
                },
            )]),
        }
    }

    async fn execute(&self, deposit: &Deposit) -> StepResult {
        let context = match self.context_lookup.load(deposit.address_id).await {
            Ok(context) => context,
            Err(error) => {
                tracing::error!(deposit_id = %deposit.id, %error, "confirm context load failed");
                return retry(
                    RetryError::Transient,
                    json!({"stage": "context", "error": "database"}),
                    TransitionEffects::default(),
                );
            }
        };
        let Some(chains) = self.chains.get(&deposit.chain_id) else {
            return retry(
                RetryError::InvariantViolation,
                json!({"stage": "chain", "error": "missing_chain"}),
                TransitionEffects::default(),
            );
        };

        let canonical = match finalized_evidence(chains, deposit, context.address).await {
            FinalityResult::Ready(log) => log,
            FinalityResult::Wait(evidence) => {
                return StepResult::new(
                    StepOutcome::Wait {
                        reason: WaitReason::Finality,
                    },
                    evidence,
                );
            }
            FinalityResult::Retry(error, evidence) => {
                return retry(error, evidence, TransitionEffects::default());
            }
        };
        let selected_route = if canonical.token == deposit.asset_contract {
            deposit.route.as_ref().zip(deposit.route_version)
        } else {
            self.asset_routes
                .get(&(deposit.chain_id, canonical.token))
                .map(|(name, version)| (name, *version))
        };
        let canonical_effect = canonical_effect(deposit, &canonical, selected_route);
        let mut effects = TransitionEffects {
            canonical_evidence: canonical_effect,
            ..TransitionEffects::default()
        };
        let Some((route_name, route_version)) = selected_route else {
            return rejected_result(
                deposit,
                context.product_id,
                RejectReason::UnsupportedAsset,
                json!({
                    "stage": "route",
                    "result": "unsupported_asset",
                    "providers": provider_evidence(&canonical),
                }),
                effects,
            );
        };
        let Some(runtime) = self.routes.get(&(route_name.clone(), route_version)) else {
            return retry(
                RetryError::InvariantViolation,
                json!({"stage": "route", "error": "unknown_route_version"}),
                effects,
            );
        };

        let valuation_at = Utc::now();
        let quote = match runtime.pricing.fetch(&runtime.route).await {
            Ok(quote) => quote,
            Err(evidence) => {
                return retry(RetryError::PriceUnavailable, evidence, effects);
            }
        };
        let lock = context.lock.as_ref().and_then(|lock| {
            if lock.route == runtime.route.route {
                Some(LockTerms {
                    asset: canonical.token,
                    amount: lock.amount,
                    price: lock.price,
                    credit_minor: lock.credit_minor,
                    expires_at: lock.expires_at,
                    block_time: unix_seconds(canonical.block_time)?,
                })
            } else {
                None
            }
        });
        let valuation = value_deposit(
            canonical.amount,
            quote.price,
            &RouteValuation::from(&runtime.route),
            lock.as_ref(),
        );
        let valuation = match valuation {
            Ok(valuation) => valuation,
            Err(ValuationError::BelowMinimum) => {
                let computed = credit(
                    canonical.amount,
                    quote.price,
                    runtime.route.asset.decimals,
                    runtime.route.destination.unit_decimals,
                );
                let Ok(credit_minor) = computed else {
                    return reject_out_of_range(deposit, context.product_id, effects, &quote);
                };
                effects.valuation = Some(stored_valuation(
                    valuation_at,
                    quote.price,
                    ValuationSource::Spot,
                    credit_minor,
                    quote.evidence.clone(),
                ));
                return rejected_result(
                    deposit,
                    context.product_id,
                    RejectReason::BelowMinimum,
                    json!({
                        "stage": "valuation",
                        "result": "below_minimum",
                        "providers": provider_evidence(&canonical),
                        "quote": quote.evidence,
                    }),
                    effects,
                );
            }
            Err(ValuationError::ArithmeticOutOfRange | ValuationError::Credit(_)) => {
                return reject_out_of_range(deposit, context.product_id, effects, &quote);
            }
            Err(error) => {
                return retry(
                    RetryError::PriceUnavailable,
                    json!({
                        "stage": "valuation",
                        "error": valuation_error_code(&error),
                        "quote": quote.evidence,
                    }),
                    effects,
                );
            }
        };
        if valuation.source == ValuationSource::Lock {
            effects.lock_consumption = Some(LockConsumption {
                address_id: deposit.address_id,
                idempotent: false,
            });
        }
        effects.valuation = Some(stored_valuation(
            valuation_at,
            valuation.price,
            valuation.source,
            valuation.credit_minor,
            quote.evidence.clone(),
        ));
        let event = OutboxEvent {
            id: Uuid::new_v4(),
            event_type: "deposit.confirmed".to_owned(),
            payload: json!({
                "product_id": context.product_id,
                "deposit_id": deposit.id,
                "chain_id": deposit.chain_id,
                "route": runtime.route.route,
                "route_version": runtime.route.version,
                "tx_hash": format!("{:#x}", deposit.tx_hash),
                "log_index": deposit.log_index,
                "amount_atomic": canonical.amount.value().to_string(),
                "price_scaled": valuation.price.value().to_string(),
                "price_scale": PRICE_SCALE,
                "price_source": valuation_source_code(valuation.source),
                "credit_minor": valuation.credit_minor.value().to_string(),
                "valuation_at": valuation_at,
            }),
            next_attempt_at: valuation_at,
        };
        StepResult {
            outcome: StepOutcome::Advance,
            evidence: json!({
                "stage": "confirmed",
                "providers": provider_evidence(&canonical),
                "corrected": effects.canonical_evidence.is_some(),
                "quote": quote.evidence,
                "valuation": {
                    "price_scaled": valuation.price.value().to_string(),
                    "price_source": valuation_source_code(valuation.source),
                    "credit_minor": valuation.credit_minor.value().to_string(),
                    "valuation_at": valuation_at,
                },
            }),
            events: vec![event],
            effects,
        }
    }
}

#[async_trait]
impl Step for ConfirmStep {
    async fn run(&self, deposit: &Deposit) -> StepResult {
        self.execute(deposit).await
    }
}

#[derive(Clone)]
struct ConfirmationContext {
    address: Address,
    product_id: Uuid,
    lock: Option<StoredLock>,
}

#[derive(Clone)]
struct StoredLock {
    route: String,
    amount: AtomicAmount,
    price: ScaledPrice,
    credit_minor: MinorAmount,
    expires_at: UnixSeconds,
}

#[async_trait]
trait ContextLookup: Send + Sync {
    async fn load(&self, address_id: Uuid) -> Result<ConfirmationContext, sqlx::Error>;
}

struct PostgresContextLookup(PgPool);

#[async_trait]
impl ContextLookup for PostgresContextLookup {
    async fn load(&self, address_id: Uuid) -> Result<ConfirmationContext, sqlx::Error> {
        load_context(&self.0, address_id).await
    }
}

async fn load_context(pool: &PgPool, address_id: Uuid) -> Result<ConfirmationContext, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT address.address, account.product_id, address.kind,
               rate_lock.route, rate_lock.amount_atomic::text AS amount_atomic,
               rate_lock.price_scaled::text AS price_scaled,
               rate_lock.credit_minor::text AS credit_minor,
               rate_lock.expires_at, rate_lock.consumed_by, rate_lock.status AS lock_status
        FROM addresses AS address
        JOIN accounts AS account ON account.id = address.account_id
        LEFT JOIN rate_locks AS rate_lock ON rate_lock.address_id = address.id
        WHERE address.id = $1
        "#,
    )
    .bind(address_id)
    .fetch_one(pool)
    .await?;
    let address_text: String = row.try_get("address")?;
    let address = Address::from_str(&address_text)
        .map_err(|error| sqlx::Error::Decode(format!("invalid address: {error}").into()))?;
    let kind: String = row.try_get("kind")?;
    let consumed_by: Option<Uuid> = row.try_get("consumed_by")?;
    let lock_status: Option<String> = row.try_get("lock_status")?;
    let lock = if kind == "lock"
        && consumed_by.is_none()
        && matches!(lock_status.as_deref(), Some("open" | "expired"))
    {
        let route: Option<String> = row.try_get("route")?;
        let amount: Option<String> = row.try_get("amount_atomic")?;
        let price: Option<String> = row.try_get("price_scaled")?;
        let credit_minor: Option<String> = row.try_get("credit_minor")?;
        let expires_at: Option<DateTime<Utc>> = row.try_get("expires_at")?;
        Some(StoredLock {
            route: route.ok_or_else(|| sqlx::Error::Decode("lock route is missing".into()))?,
            amount: AtomicAmount::new(
                U256::from_str(
                    amount
                        .as_deref()
                        .ok_or_else(|| sqlx::Error::Decode("lock amount is missing".into()))?,
                )
                .map_err(|error| sqlx::Error::Decode(error.to_string().into()))?,
            ),
            price: ScaledPrice::new(
                price
                    .as_deref()
                    .ok_or_else(|| sqlx::Error::Decode("lock price is missing".into()))?
                    .parse::<u64>()
                    .map_err(|error| sqlx::Error::Decode(error.to_string().into()))?,
                PRICE_SCALE,
            )
            .map_err(|error| sqlx::Error::Decode(error.to_string().into()))?,
            credit_minor: MinorAmount::new(
                credit_minor
                    .as_deref()
                    .ok_or_else(|| sqlx::Error::Decode("lock credit is missing".into()))?
                    .parse::<u64>()
                    .map_err(|error| sqlx::Error::Decode(error.to_string().into()))?,
            ),
            expires_at: unix_seconds(
                expires_at.ok_or_else(|| sqlx::Error::Decode("lock expiry is missing".into()))?,
            )
            .ok_or_else(|| sqlx::Error::Decode("lock expiry is invalid".into()))?,
        })
    } else {
        None
    };
    Ok(ConfirmationContext {
        address,
        product_id: row.try_get("product_id")?,
        lock,
    })
}

enum FinalityResult {
    Ready(TransferLog),
    Wait(Value),
    Retry(RetryError, Value),
}

async fn finalized_evidence(
    chains: &ChainPair,
    deposit: &Deposit,
    address: Address,
) -> FinalityResult {
    let (primary_head, secondary_head, primary_log, secondary_log) = tokio::join!(
        chains.primary.finalized_head(),
        chains.secondary.finalized_head(),
        chains
            .primary
            .transfer_log_by_identity(deposit.tx_hash, deposit.log_index),
        chains
            .secondary
            .transfer_log_by_identity(deposit.tx_hash, deposit.log_index),
    );
    let (primary_head, secondary_head, primary, secondary) =
        match (primary_head, secondary_head, primary_log, secondary_log) {
            (Ok(primary_head), Ok(secondary_head), Ok(primary), Ok(secondary)) => {
                (primary_head, secondary_head, primary, secondary)
            }
            (primary_head, secondary_head, primary_log, secondary_log) => {
                return FinalityResult::Retry(
                    RetryError::RpcDisagreement,
                    json!({
                        "stage": "finality",
                        "error": "rpc_failure",
                        "provider_a_head": chain_result_code(&primary_head),
                        "provider_b_head": chain_result_code(&secondary_head),
                        "provider_a_receipt": chain_result_code(&primary_log),
                        "provider_b_receipt": chain_result_code(&secondary_log),
                    }),
                );
            }
        };
    let required_block = primary
        .as_ref()
        .map(|log| log.block_number)
        .into_iter()
        .chain(secondary.as_ref().map(|log| log.block_number))
        .max()
        .unwrap_or(deposit.block_number);
    if primary_head < required_block || secondary_head < required_block {
        return FinalityResult::Wait(json!({
            "stage": "finality",
            "result": "not_final",
            "required_block": required_block,
            "provider_a_finalized": primary_head,
            "provider_b_finalized": secondary_head,
        }));
    }
    match (primary, secondary) {
        (Some(primary), Some(secondary)) if primary == secondary => {
            if primary.to != address {
                return FinalityResult::Retry(
                    RetryError::RpcDisagreement,
                    json!({
                        "stage": "finality",
                        "error": "recipient_mismatch",
                        "expected_to": format!("{address:#x}"),
                        "provider_a": provider_evidence(&primary),
                        "provider_b": provider_evidence(&secondary),
                    }),
                );
            }
            FinalityResult::Ready(primary)
        }
        // Deposits are born final, so both providers denying the log past its block is a
        // finality or provider fault, not a disagreement between them.
        (None, None) => FinalityResult::Retry(
            RetryError::InvariantViolation,
            json!({
                "stage": "finality",
                "error": "log_absent_at_finality",
                "provider_a_finalized": primary_head,
                "provider_b_finalized": secondary_head,
            }),
        ),
        (primary, secondary) => FinalityResult::Retry(
            RetryError::RpcDisagreement,
            json!({
                "stage": "finality",
                "error": "rpc_disagreement",
                "provider_a": primary.as_ref().map(provider_evidence),
                "provider_b": secondary.as_ref().map(provider_evidence),
            }),
        ),
    }
}

fn chain_result_code<T>(result: &Result<T, ChainError>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(ChainError::FinalizedHeadRegressed { .. } | ChainError::ProviderUnhealthy) => {
            "provider_unhealthy"
        }
        Err(_) => "rpc_error",
    }
}

fn canonical_effect(
    deposit: &Deposit,
    canonical: &TransferLog,
    selected_route: Option<(&String, u64)>,
) -> Option<CanonicalEvidence> {
    let selected_name = selected_route.map(|(name, _)| name.as_str());
    let selected_version = selected_route.map(|(_, version)| version);
    let changed = deposit.block_number != canonical.block_number
        || deposit.block_hash != canonical.block_hash
        || deposit.block_time != canonical.block_time
        || deposit.asset_contract != canonical.token
        || deposit.from_address != canonical.from
        || deposit.amount_atomic != canonical.amount
        || deposit.route.as_deref() != selected_name
        || deposit.route_version != selected_version;
    changed.then_some(CanonicalEvidence {
        block_number: canonical.block_number,
        block_hash: canonical.block_hash,
        block_time: canonical.block_time,
        asset_contract: canonical.token,
        from_address: canonical.from,
        amount_atomic: canonical.amount,
        route: selected_name.map(str::to_owned),
        route_version: selected_version,
    })
}

fn stored_valuation(
    valuation_at: DateTime<Utc>,
    price: ScaledPrice,
    source: ValuationSource,
    credit_minor: MinorAmount,
    quote: Value,
) -> StoredValuation {
    StoredValuation {
        valuation_at,
        price_scaled: price.value(),
        price_source: valuation_source_code(source).to_owned(),
        credit_minor,
        quote,
    }
}

const fn valuation_source_code(source: ValuationSource) -> &'static str {
    match source {
        ValuationSource::Spot => "spot",
        ValuationSource::Lock => "lock",
    }
}

fn reject_out_of_range(
    deposit: &Deposit,
    product_id: Uuid,
    effects: TransitionEffects,
    quote: &ValidatedQuote,
) -> StepResult {
    rejected_result(
        deposit,
        product_id,
        RejectReason::OutOfRange,
        json!({
            "stage": "valuation",
            "result": "out_of_range",
            "quote": quote.evidence,
        }),
        effects,
    )
}

fn rejected_result(
    deposit: &Deposit,
    product_id: Uuid,
    reason: RejectReason,
    evidence: Value,
    effects: TransitionEffects,
) -> StepResult {
    StepResult {
        outcome: StepOutcome::Reject(reason),
        evidence,
        events: vec![rejected_event(deposit, product_id, reason)],
        effects,
    }
}

fn rejected_event(deposit: &Deposit, product_id: Uuid, reason: RejectReason) -> OutboxEvent {
    OutboxEvent {
        id: Uuid::new_v4(),
        event_type: "deposit.rejected".to_owned(),
        payload: json!({
            "product_id": product_id,
            "deposit_id": deposit.id,
            "chain_id": deposit.chain_id,
            "state": "rejected",
            "route": deposit.route.as_deref(),
            "reason": reason.code(),
        }),
        next_attempt_at: Utc::now(),
    }
}

fn retry(error: RetryError, evidence: Value, effects: TransitionEffects) -> StepResult {
    StepResult {
        outcome: StepOutcome::Retry { error },
        evidence,
        events: Vec::new(),
        effects,
    }
}

fn unix_seconds(time: DateTime<Utc>) -> Option<UnixSeconds> {
    u64::try_from(time.timestamp()).ok().map(UnixSeconds::new)
}

fn provider_evidence(log: &TransferLog) -> Value {
    json!({
        "tx_hash": format!("{:#x}", log.tx_hash),
        "log_index": log.log_index,
        "block_number": log.block_number,
        "block_hash": format!("{:#x}", log.block_hash),
        "block_time": log.block_time,
        "token": format!("{:#x}", log.token),
        "from": format!("{:#x}", log.from),
        "to": format!("{:#x}", log.to),
        "amount_atomic": log.amount.value().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use std::future::ready;
    use std::time::Duration;

    use topup_adapters::chain::evm::FinalizedHead;
    use topup_adapters::pricing::{Observation, PriceError};
    use topup_core::route::PricingMode;
    use topup_core::valuation::SourceId;

    use super::*;
    use topup_core::deposit::DepositState;

    #[derive(Clone)]
    struct MockChain {
        head: Result<u64, ChainError>,
        logs: Result<Vec<TransferLog>, ChainError>,
    }

    impl ChainReader for MockChain {
        fn finalized_head(
            &self,
        ) -> impl std::future::Future<Output = Result<FinalizedHead, ChainError>> + Send {
            ready(self.head.clone().map(|number| FinalizedHead {
                number,
                time: DateTime::UNIX_EPOCH,
            }))
        }

        async fn transfer_logs_to(
            &self,
            _addresses: &[Address],
            _from_block: u64,
            _to_block: u64,
        ) -> Result<Vec<TransferLog>, ChainError> {
            panic!("confirm finality must locate the log by receipt identity")
        }

        fn transfer_log_by_identity(
            &self,
            tx_hash: B256,
            log_index: u64,
        ) -> impl std::future::Future<Output = Result<Option<TransferLog>, ChainError>> + Send
        {
            ready(self.logs.clone().map(|logs| {
                logs.into_iter()
                    .find(|log| log.tx_hash == tx_hash && log.log_index == log_index)
            }))
        }
    }

    struct MockPrice(Result<Observation, PriceError>);

    #[async_trait]
    impl PriceSource for MockPrice {
        async fn observe(&self) -> Result<Observation, PriceError> {
            self.0.clone()
        }
    }

    struct DelayedPrice {
        source: &'static str,
        value: u64,
        delay: Duration,
    }

    #[async_trait]
    impl PriceSource for DelayedPrice {
        async fn observe(&self) -> Result<Observation, PriceError> {
            tokio::time::sleep(self.delay).await;
            Ok(observation(self.source, self.value, now_seconds()))
        }
    }

    struct MockContext(ConfirmationContext);

    #[async_trait]
    impl ContextLookup for MockContext {
        async fn load(&self, _address_id: Uuid) -> Result<ConfirmationContext, sqlx::Error> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn provider_disagreement_retries() {
        let deposit = deposit(1_000);
        let first = transfer(&deposit);
        let mut second = first.clone();
        second.block_hash = B256::repeat_byte(9);
        let result = step(
            route(PricingMode::Spot),
            chain(100, vec![first]),
            chain(100, vec![second]),
            prices(now_seconds()),
            context(None),
        )
        .run(&deposit)
        .await;
        assert_eq!(
            result.outcome,
            StepOutcome::Retry {
                error: RetryError::RpcDisagreement
            }
        );
        assert_eq!(result.evidence["error"], "rpc_disagreement");
    }

    #[tokio::test]
    async fn log_absent_on_both_final_providers_is_not_a_disagreement() {
        let result = step(
            route(PricingMode::Spot),
            chain(100, Vec::new()),
            chain(100, Vec::new()),
            prices(now_seconds()),
            context(None),
        )
        .run(&deposit(1_000))
        .await;
        assert_eq!(
            result.outcome,
            StepOutcome::Retry {
                error: RetryError::InvariantViolation
            }
        );
        assert_eq!(result.evidence["error"], "log_absent_at_finality");
    }

    #[tokio::test]
    async fn lagging_provider_waits_for_finality() {
        let deposit = deposit(1_000);
        let log = transfer(&deposit);
        let result = step(
            route(PricingMode::Spot),
            chain(100, vec![log.clone()]),
            chain(9, vec![log]),
            prices(now_seconds()),
            context(None),
        )
        .run(&deposit)
        .await;
        assert_eq!(
            result.outcome,
            StepOutcome::Wait {
                reason: WaitReason::Finality
            }
        );
    }

    #[tokio::test]
    async fn lagging_provider_without_receipt_waits_for_finality() {
        let deposit = deposit(1_000);
        let log = transfer(&deposit);
        let result = step(
            route(PricingMode::Spot),
            chain(100, vec![log]),
            chain(9, Vec::new()),
            prices(now_seconds()),
            context(None),
        )
        .run(&deposit)
        .await;
        assert_eq!(
            result.outcome,
            StepOutcome::Wait {
                reason: WaitReason::Finality
            }
        );
    }

    #[tokio::test]
    async fn agreed_canonical_evidence_corrects_provisional_row() {
        let deposit = deposit(1_000);
        let mut canonical = transfer(&deposit);
        canonical.block_hash = B256::repeat_byte(7);
        canonical.from = Address::repeat_byte(8);
        let result = step(
            route(PricingMode::Spot),
            chain(100, vec![canonical.clone()]),
            chain(100, vec![canonical.clone()]),
            prices(now_seconds()),
            context(None),
        )
        .run(&deposit)
        .await;
        assert_eq!(result.outcome, StepOutcome::Advance);
        let correction = result.effects.canonical_evidence.expect("correction");
        assert_eq!(correction.block_hash, canonical.block_hash);
        assert_eq!(correction.from_address, canonical.from);
        assert_eq!(result.evidence["corrected"], true);
    }

    #[tokio::test]
    async fn wrong_provisional_block_is_corrected_from_receipt_identity() {
        let mut deposit = deposit(1_000);
        deposit.block_number = 4;
        let mut canonical = transfer(&deposit);
        canonical.block_number = 80;
        canonical.block_hash = B256::repeat_byte(8);
        let result = step(
            route(PricingMode::Spot),
            chain(100, vec![canonical.clone()]),
            chain(100, vec![canonical.clone()]),
            prices(now_seconds()),
            context(None),
        )
        .run(&deposit)
        .await;
        assert_eq!(result.outcome, StepOutcome::Advance);
        assert_eq!(
            result
                .effects
                .canonical_evidence
                .expect("block correction")
                .block_number,
            canonical.block_number
        );
    }

    #[tokio::test]
    async fn canonical_token_reselects_route_and_its_valuation_policy() {
        let deposit = deposit(1_000);
        let mut canonical_route = route(PricingMode::Spot);
        canonical_route.route = "canonical-token-route".to_owned();
        canonical_route.version = 7;
        canonical_route.asset.contract = Address::repeat_byte(9);
        canonical_route.asset.decimals = 1;
        canonical_route.screening.min_credit_minor = 5;
        let mut canonical = transfer(&deposit);
        canonical.token = canonical_route.asset.contract;
        let original_route = route(PricingMode::Spot);
        let original_key = (original_route.route.clone(), original_route.version);
        let canonical_key = (canonical_route.route.clone(), canonical_route.version);
        let now = now_seconds();
        let runtime = |route: RouteFile| RouteRuntime {
            route,
            pricing: PricingRuntime::injected(
                Arc::new(MockPrice(Ok(observation("primary", 10_000_000, now)))),
                Some(Arc::new(MockPrice(Ok(observation(
                    "check", 10_000_000, now,
                ))))),
                Some(Arc::new(MockPrice(Ok(observation("fx", 100_000_000, now))))),
            ),
        };
        let context = context(None);
        let product_id = context.product_id;
        let step = ConfirmStep {
            context_lookup: Arc::new(MockContext(context)),
            routes: BTreeMap::from([
                (original_key.clone(), runtime(original_route)),
                (canonical_key.clone(), runtime(canonical_route.clone())),
            ]),
            asset_routes: BTreeMap::from([
                ((deposit.chain_id, deposit.asset_contract), original_key),
                ((deposit.chain_id, canonical.token), canonical_key),
            ]),
            chains: BTreeMap::from([(
                deposit.chain_id,
                ChainPair {
                    primary: Arc::new(chain(100, vec![canonical.clone()])),
                    secondary: Arc::new(chain(100, vec![canonical])),
                },
            )]),
        };
        let result = step.run(&deposit).await;
        assert_eq!(result.outcome, StepOutcome::Advance);
        let correction = result.effects.canonical_evidence.expect("token correction");
        assert_eq!(correction.route.as_deref(), Some("canonical-token-route"));
        assert_eq!(correction.route_version, Some(7));
        assert_eq!(
            result.effects.valuation.expect("valuation").credit_minor,
            MinorAmount::new(10)
        );
        assert_eq!(
            result.events[0].payload["product_id"],
            product_id.to_string()
        );
        assert_eq!(result.events[0].payload["route"], "canonical-token-route");
        assert_eq!(result.events[0].payload["route_version"], 7);
    }

    #[tokio::test]
    async fn unsupported_canonical_token_rejects_with_event() {
        let deposit = deposit(1_000);
        let mut canonical = transfer(&deposit);
        canonical.token = Address::repeat_byte(9);
        let context = context(None);
        let product_id = context.product_id;
        let result = step(
            route(PricingMode::Spot),
            chain(100, vec![canonical.clone()]),
            chain(100, vec![canonical]),
            prices(now_seconds()),
            context,
        )
        .run(&deposit)
        .await;
        assert_eq!(
            result.outcome,
            StepOutcome::Reject(RejectReason::UnsupportedAsset)
        );
        assert_rejected_event(
            &result.events[0],
            RejectReason::UnsupportedAsset,
            product_id,
        );
    }

    #[tokio::test]
    async fn freshness_reference_is_captured_after_slow_price_fetches() {
        let mut route = route(PricingMode::Spot);
        route.pricing.max_age_s = 1;
        let runtime = RouteRuntime {
            route,
            pricing: PricingRuntime::injected(
                Arc::new(DelayedPrice {
                    source: "primary",
                    value: 10_000_000,
                    delay: Duration::from_secs(2),
                }),
                Some(Arc::new(DelayedPrice {
                    source: "check",
                    value: 10_000_000,
                    delay: Duration::from_secs(2),
                })),
                Some(Arc::new(DelayedPrice {
                    source: "fx",
                    value: 100_000_000,
                    delay: Duration::from_secs(2),
                })),
            ),
        };
        let quote = runtime
            .pricing
            .fetch(&runtime.route)
            .await
            .expect("slow quote remains fresh");
        assert_eq!(quote.price.value(), 10_000_000);
    }

    #[tokio::test]
    async fn stale_divergent_and_depegged_quotes_retry_with_observations() {
        let now = now_seconds();
        let deposit = deposit(1_000);
        let log = transfer(&deposit);
        let cases = [
            (
                route(PricingMode::Spot),
                PriceSet {
                    primary: observation("primary", 10_000_000, now - 121),
                    check: Some(observation("check", 10_000_000, now)),
                    fx: Some(observation("fx", 100_000_000, now)),
                },
                "stale",
            ),
            (
                route(PricingMode::Spot),
                PriceSet {
                    primary: observation("primary", 10_000_000, now),
                    check: Some(observation("check", 20_000_000, now)),
                    fx: Some(observation("fx", 100_000_000, now)),
                },
                "divergent",
            ),
            (
                route(PricingMode::Stablecoin),
                PriceSet {
                    primary: observation("primary", 90_000_000, now),
                    check: None,
                    fx: None,
                },
                "depeg",
            ),
        ];
        for (route, prices, error) in cases {
            let result = step(
                route,
                chain(100, vec![log.clone()]),
                chain(100, vec![log.clone()]),
                prices,
                context(None),
            )
            .run(&deposit)
            .await;
            assert_eq!(
                result.outcome,
                StepOutcome::Retry {
                    error: RetryError::PriceUnavailable
                }
            );
            assert_eq!(result.evidence["error"], error);
            assert!(result.evidence["quote"].is_object());
        }
    }

    #[tokio::test]
    async fn stablecoin_mode_uses_fixed_dollar_without_check_sources() {
        let now = now_seconds();
        let deposit = deposit(1_000);
        let log = transfer(&deposit);
        let result = step(
            route(PricingMode::Stablecoin),
            chain(100, vec![log.clone()]),
            chain(100, vec![log]),
            PriceSet {
                primary: observation("primary", 100_500_000, now),
                check: None,
                fx: None,
            },
            context(None),
        )
        .run(&deposit)
        .await;
        assert_eq!(result.outcome, StepOutcome::Advance);
        let valuation = result.effects.valuation.expect("valuation");
        assert_eq!(valuation.price_scaled, 100_000_000);
        assert_eq!(valuation.price_source, "spot");
    }

    #[tokio::test]
    async fn lock_at_both_tolerance_bounds_uses_frozen_credit() {
        for amount in [990, 1_010] {
            let now = now_seconds();
            let deposit = deposit(amount);
            let log = transfer(&deposit);
            let result = step(
                route(PricingMode::Spot),
                chain(100, vec![log.clone()]),
                chain(100, vec![log]),
                prices(now),
                context(Some(lock(now))),
            )
            .run(&deposit)
            .await;
            assert_eq!(result.outcome, StepOutcome::Advance);
            let valuation = result.effects.valuation.expect("valuation");
            assert_eq!(valuation.price_source, "lock");
            assert_eq!(valuation.price_scaled, 9_000_000);
            assert_eq!(valuation.credit_minor, MinorAmount::new(777));
            assert!(result.effects.lock_consumption.is_some());
        }
    }

    #[tokio::test]
    async fn lock_miss_falls_back_to_validated_spot() {
        let now = now_seconds();
        let deposit = deposit(1_011);
        let log = transfer(&deposit);
        let result = step(
            route(PricingMode::Spot),
            chain(100, vec![log.clone()]),
            chain(100, vec![log]),
            prices(now),
            context(Some(lock(now))),
        )
        .run(&deposit)
        .await;
        assert_eq!(result.outcome, StepOutcome::Advance);
        let valuation = result.effects.valuation.expect("valuation");
        assert_eq!(valuation.price_source, "spot");
        assert_eq!(valuation.credit_minor, MinorAmount::new(101));
        assert!(result.effects.lock_consumption.is_none());
    }

    #[tokio::test]
    async fn below_minimum_rejects_and_keeps_quote_fields() {
        let now = now_seconds();
        let mut route = route(PricingMode::Spot);
        route.screening.min_credit_minor = 200;
        let deposit = deposit(1_000);
        let log = transfer(&deposit);
        let context = context(None);
        let product_id = context.product_id;
        let result = step(
            route,
            chain(100, vec![log.clone()]),
            chain(100, vec![log]),
            prices(now),
            context,
        )
        .run(&deposit)
        .await;
        assert_eq!(
            result.outcome,
            StepOutcome::Reject(RejectReason::BelowMinimum)
        );
        let valuation = result.effects.valuation.expect("valuation");
        assert_eq!(valuation.credit_minor, MinorAmount::new(100));
        assert!(valuation.quote.is_object());
        assert_rejected_event(&result.events[0], RejectReason::BelowMinimum, product_id);
    }

    #[tokio::test]
    async fn arithmetic_out_of_range_rejects_with_event() {
        let now = now_seconds();
        let mut deposit = deposit(1);
        deposit.amount_atomic = AtomicAmount::new(U256::MAX);
        let log = transfer(&deposit);
        let context = context(None);
        let product_id = context.product_id;
        let result = step(
            route(PricingMode::Spot),
            chain(100, vec![log.clone()]),
            chain(100, vec![log]),
            prices(now),
            context,
        )
        .run(&deposit)
        .await;
        assert_eq!(
            result.outcome,
            StepOutcome::Reject(RejectReason::OutOfRange)
        );
        assert_rejected_event(&result.events[0], RejectReason::OutOfRange, product_id);
    }

    struct PriceSet {
        primary: Observation,
        check: Option<Observation>,
        fx: Option<Observation>,
    }

    fn step(
        route: RouteFile,
        primary_chain: MockChain,
        secondary_chain: MockChain,
        prices: PriceSet,
        context: ConfirmationContext,
    ) -> ConfirmStep {
        let chain_id = route.chain.chain_id;
        let asset_contract = route.asset.contract;
        let key = (route.route.clone(), route.version);
        ConfirmStep {
            context_lookup: Arc::new(MockContext(context)),
            routes: BTreeMap::from([(
                key.clone(),
                RouteRuntime {
                    route,
                    pricing: PricingRuntime::injected(
                        Arc::new(MockPrice(Ok(prices.primary))),
                        prices.check.map(|observation| {
                            Arc::new(MockPrice(Ok(observation))) as Arc<dyn PriceSource>
                        }),
                        prices.fx.map(|observation| {
                            Arc::new(MockPrice(Ok(observation))) as Arc<dyn PriceSource>
                        }),
                    ),
                },
            )]),
            asset_routes: BTreeMap::from([((chain_id, asset_contract), key)]),
            chains: BTreeMap::from([(
                chain_id,
                ChainPair {
                    primary: Arc::new(primary_chain),
                    secondary: Arc::new(secondary_chain),
                },
            )]),
        }
    }

    fn route(mode: PricingMode) -> RouteFile {
        let mut route: RouteFile =
            serde_saphyr::from_str(include_str!("../../tests/fixtures/phala-cloud-pha.yaml"))
                .expect("route fixture");
        route.pricing.mode = mode;
        route.asset.decimals = 0;
        route.destination.unit_decimals = 0;
        route.screening.min_credit_minor = 1;
        if mode == PricingMode::Stablecoin {
            route.pricing.check = None;
        }
        route
    }

    fn deposit(amount: u64) -> Deposit {
        let now = Utc::now();
        Deposit {
            id: Uuid::new_v4(),
            chain_id: 1,
            tx_hash: B256::repeat_byte(1),
            log_index: 3,
            block_number: 10,
            block_hash: B256::repeat_byte(2),
            block_time: now,
            address_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            route: Some("phala-cloud-ethereum-pha-usd".to_owned()),
            route_version: Some(1),
            asset_contract: asset(),
            from_address: Address::repeat_byte(4),
            amount_atomic: AtomicAmount::new(U256::from(amount)),
            state: DepositState::Detected,
            reason: None,
            attempt: 0,
            next_attempt_at: now,
            lease_token: None,
            lease_until: None,
            valuation_at: None,
            price_scaled: None,
            price_source: None,
            credit_minor: None,
            quote: None,
            flush_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn transfer(deposit: &Deposit) -> TransferLog {
        TransferLog {
            tx_hash: deposit.tx_hash,
            log_index: deposit.log_index,
            block_number: deposit.block_number,
            block_hash: deposit.block_hash,
            block_time: deposit.block_time,
            token: deposit.asset_contract,
            from: deposit.from_address,
            to: recipient(),
            amount: deposit.amount_atomic,
        }
    }

    fn chain(head: u64, logs: Vec<TransferLog>) -> MockChain {
        MockChain {
            head: Ok(head),
            logs: Ok(logs),
        }
    }

    fn context(lock: Option<StoredLock>) -> ConfirmationContext {
        ConfirmationContext {
            address: recipient(),
            product_id: Uuid::new_v4(),
            lock,
        }
    }

    fn lock(now: u64) -> StoredLock {
        StoredLock {
            route: "phala-cloud-ethereum-pha-usd".to_owned(),
            amount: AtomicAmount::new(U256::from(1_000_u64)),
            price: ScaledPrice::new(9_000_000, PRICE_SCALE).expect("lock price"),
            credit_minor: MinorAmount::new(777),
            expires_at: UnixSeconds::new(now + 300),
        }
    }

    fn prices(now: u64) -> PriceSet {
        PriceSet {
            primary: observation("primary", 10_000_000, now),
            check: Some(observation("check", 10_000_000, now)),
            fx: Some(observation("fx", 100_000_000, now)),
        }
    }

    fn observation(source: &str, value: u64, at: u64) -> Observation {
        Observation {
            source: SourceId::new(source),
            price: ScaledPrice::new(value, PRICE_SCALE).expect("price"),
            observed_at: UnixSeconds::new(at),
        }
    }

    fn now_seconds() -> u64 {
        u64::try_from(Utc::now().timestamp()).expect("current timestamp")
    }

    fn assert_rejected_event(event: &OutboxEvent, reason: RejectReason, product_id: Uuid) {
        assert_eq!(event.event_type, "deposit.rejected");
        assert_eq!(event.payload["product_id"], product_id.to_string());
        assert!(event.payload["deposit_id"].as_str().is_some());
        assert!(event.payload["chain_id"].as_u64().is_some());
        assert_eq!(event.payload["state"], "rejected");
        assert!(event.payload["route"].as_str().is_some());
        assert_eq!(event.payload["reason"], reason.code());
    }

    fn recipient() -> Address {
        Address::repeat_byte(3)
    }

    fn asset() -> Address {
        Address::from_str("0x6c5bA91642F10282b576d91922Ae6448C9d52f4E").expect("fixture asset")
    }
}

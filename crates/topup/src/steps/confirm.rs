//! Detected-to-confirmed deposit step.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{Address, B256, U256};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use topup_adapters::chain::evm::{ChainError, ChainReader, EvmChain, TransferLog};
use topup_adapters::pricing::binance::Binance;
use topup_adapters::pricing::coinmetrics::CoinMetrics;
use topup_adapters::pricing::kraken::Kraken;
use topup_adapters::pricing::{Observation, PriceSource};
use topup_adapters::settlement::http::{SettlementAnswer, SettlementApi, SettlementClient};
use topup_adapters::signer::actor::SignerHandle;
use topup_core::deposit::{RejectReason, RetryError, StepOutcome, WaitReason};
use topup_core::money::{AtomicAmount, MinorAmount, PRICE_SCALE, ScaledPrice, credit};
use topup_core::route::{PricingMode, RouteFile};
use topup_core::valuation::{
    FxObservation, LockTerms, RouteValuation, UnixSeconds, ValuationError, ValuationPolicy,
    ValuationSource, stablecoin_price, validate_spot, value_deposit,
};
use uuid::Uuid;

use crate::db::{
    CanonicalEvidence, Deposit, LockConsumption, OutboxEvent, SettlementAdoption, StoredValuation,
    TransitionEffects,
};
use crate::pump::{Step, StepResult};
use crate::rpc_provider::configured_provider_url;

/// Authoritative prior answer returned by the destination product.
#[derive(Clone, Debug, PartialEq)]
pub struct ProductAnswer {
    /// Whether the product accepted rather than rejected the original payload.
    pub accepted: bool,
    /// Product ledger transaction identifier, when one exists.
    pub destination_tx_id: Option<String>,
    /// Original immutable settlement payload.
    pub payload: Value,
}

/// Product lookup failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProductLookupError;

impl Display for ProductLookupError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("product answer lookup failed")
    }
}

impl Error for ProductLookupError {}

/// Minimal GET-by-idempotency-key boundary shared with the settlement client.
#[async_trait]
pub trait ProductLookup: Send + Sync {
    /// Returns the product's stored answer, when the key is known.
    async fn get_by_key(&self, key: &str) -> Result<Option<ProductAnswer>, ProductLookupError>;
}

/// Signed product lookup resolved from the deposit's owning product.
pub struct SettlementProductLookup {
    pool: PgPool,
    signer: SignerHandle,
    request_timeout: Duration,
}

impl SettlementProductLookup {
    /// Creates a lookup using each product's configured settlement endpoint.
    #[must_use]
    pub const fn new(pool: PgPool, signer: SignerHandle, request_timeout: Duration) -> Self {
        Self {
            pool,
            signer,
            request_timeout,
        }
    }
}

#[async_trait]
impl ProductLookup for SettlementProductLookup {
    async fn get_by_key(&self, key: &str) -> Result<Option<ProductAnswer>, ProductLookupError> {
        let deposit_id = key
            .strip_prefix("deposit:")
            .ok_or(ProductLookupError)?
            .parse::<Uuid>()
            .map_err(|_| ProductLookupError)?;
        let settlement_url = sqlx::query_scalar::<_, String>(
            r#"
            SELECT products.settlement_url
            FROM deposits
            JOIN accounts ON accounts.id = deposits.account_id
            JOIN products ON products.id = accounts.product_id
            WHERE deposits.id = $1
            "#,
        )
        .bind(deposit_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| ProductLookupError)?
        .ok_or(ProductLookupError)?;
        let client =
            SettlementClient::new(&settlement_url, self.signer.clone(), self.request_timeout)
                .map_err(|_| ProductLookupError)?;
        match client
            .get_by_key(key)
            .await
            .map_err(|_| ProductLookupError)?
        {
            None => Ok(None),
            Some(SettlementAnswer::Accepted {
                destination_tx_id,
                payload,
            }) => Ok(Some(ProductAnswer {
                accepted: true,
                destination_tx_id: Some(destination_tx_id),
                payload,
            })),
            Some(SettlementAnswer::Rejected { payload, .. }) => Ok(Some(ProductAnswer {
                accepted: false,
                destination_tx_id: None,
                payload,
            })),
            Some(
                SettlementAnswer::Processing { .. }
                | SettlementAnswer::Conflict409
                | SettlementAnswer::PayloadMismatch422
                | SettlementAnswer::Unknown { .. },
            ) => Err(ProductLookupError),
        }
    }
}

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
        ChainReader::finalized_head(self).await
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
    primary: Arc<dyn PriceSource>,
    check: Option<Arc<dyn PriceSource>>,
    fx: Option<Arc<dyn PriceSource>>,
}

/// Invalid detected-step runtime configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfirmConfigError(String);

impl Display for ConfirmConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for ConfirmConfigError {}

/// Two-provider finality, quote validation, and credit computation for detected deposits.
pub struct ConfirmStep {
    context_lookup: Arc<dyn ContextLookup>,
    routes: BTreeMap<(String, u64), RouteRuntime>,
    asset_routes: BTreeMap<(u64, Address), (String, u64)>,
    chains: BTreeMap<u64, ChainPair>,
    product_lookup: Arc<dyn ProductLookup>,
}

impl ConfirmStep {
    /// Builds production adapters from route files and provider URL environment variables.
    pub fn from_routes(
        pool: PgPool,
        routes: &[RouteFile],
        product_lookup: Arc<dyn ProductLookup>,
    ) -> Result<Self, ConfirmConfigError> {
        let mut runtimes = BTreeMap::new();
        let mut asset_routes = BTreeMap::new();
        let mut chains = BTreeMap::new();
        for route in routes {
            route
                .validate()
                .map_err(|error| ConfirmConfigError(error.to_string()))?;
            let key = (route.route.clone(), route.version);
            if runtimes.contains_key(&key) {
                return Err(ConfirmConfigError(format!(
                    "duplicate route `{}` version {}",
                    route.route, route.version
                )));
            }
            let primary = price_source(&route.pricing.primary.source, route)?;
            let (check, fx) = match route.pricing.mode {
                PricingMode::Spot => {
                    let check_config = route.pricing.check.as_ref().ok_or_else(|| {
                        ConfirmConfigError("spot route is missing pricing.check".to_owned())
                    })?;
                    (
                        Some(price_source(&check_config.source, route)?),
                        Some(price_source(&check_config.fx.source, route)?),
                    )
                }
                PricingMode::Stablecoin => (None, None),
            };
            runtimes.insert(
                key.clone(),
                RouteRuntime {
                    route: route.clone(),
                    primary,
                    check,
                    fx,
                },
            );
            let asset_key = (route.chain.chain_id, route.asset.contract);
            match asset_routes.get(&asset_key) {
                Some((name, _)) if name != &route.route => {
                    return Err(ConfirmConfigError(format!(
                        "routes `{name}` and `{}` both select chain {} asset {:#x}",
                        route.route, route.chain.chain_id, route.asset.contract
                    )));
                }
                Some((_, version)) if *version >= route.version => {}
                _ => {
                    asset_routes.insert(asset_key, key);
                }
            }

            if let std::collections::btree_map::Entry::Vacant(entry) =
                chains.entry(route.chain.chain_id)
            {
                let mut providers = route.chain.rpc_providers.iter();
                let first = providers.next().ok_or_else(|| {
                    ConfirmConfigError("chain has no primary RPC provider".to_owned())
                })?;
                let second = providers.next().ok_or_else(|| {
                    ConfirmConfigError("chain has no secondary RPC provider".to_owned())
                })?;
                let primary_url = configured_provider_url(first).map_err(|environment| {
                    ConfirmConfigError(format!("{environment} is required"))
                })?;
                let secondary_url = configured_provider_url(second).map_err(|environment| {
                    ConfirmConfigError(format!("{environment} is required"))
                })?;
                entry.insert(ChainPair {
                    primary: Arc::new(
                        EvmChain::new(&primary_url)
                            .map_err(|error| ConfirmConfigError(error.to_string()))?,
                    ),
                    secondary: Arc::new(
                        EvmChain::new(&secondary_url)
                            .map_err(|error| ConfirmConfigError(error.to_string()))?,
                    ),
                });
            }
        }
        Ok(Self {
            context_lookup: Arc::new(PostgresContextLookup(pool)),
            routes: runtimes,
            asset_routes,
            chains,
            product_lookup,
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
        product_lookup: Arc<dyn ProductLookup>,
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
                    primary: primary_price,
                    check: check_price,
                    fx: fx_price,
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
            product_lookup,
        }
    }

    async fn execute(&self, deposit: &Deposit) -> StepResult {
        let key = format!("deposit:{}", deposit.id);
        let product_answer = match self.product_lookup.get_by_key(&key).await {
            Ok(answer) => answer,
            Err(_) => {
                return retry(
                    RetryError::Transient,
                    json!({"stage": "product_lookup", "error": "lookup_failed"}),
                    TransitionEffects::default(),
                );
            }
        };
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
        if let Some(answer) = product_answer {
            return adopt_answer(deposit, &context, key, answer);
        }

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
            FinalityResult::Retry(evidence) => {
                return retry(
                    RetryError::RpcDisagreement,
                    evidence,
                    TransitionEffects::default(),
                );
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
        let quote = match fetch_quote(runtime).await {
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

fn price_source(
    source: &str,
    route: &RouteFile,
) -> Result<Arc<dyn PriceSource>, ConfirmConfigError> {
    match source {
        "coinmetrics" => CoinMetrics::new(
            route.pricing.primary.asset.clone(),
            route.pricing.primary.metric.clone(),
            route.pricing.primary.frequency.clone(),
        )
        .map(|source| Arc::new(source) as Arc<dyn PriceSource>)
        .map_err(|error| ConfirmConfigError(error.to_string())),
        "binance" => {
            let check = route.pricing.check.as_ref().ok_or_else(|| {
                ConfirmConfigError("binance source requires pricing.check".to_owned())
            })?;
            Binance::new(check.symbol.clone())
                .map(|source| Arc::new(source) as Arc<dyn PriceSource>)
                .map_err(|error| ConfirmConfigError(error.to_string()))
        }
        "kraken" => {
            let check = route.pricing.check.as_ref().ok_or_else(|| {
                ConfirmConfigError("kraken source requires pricing.check".to_owned())
            })?;
            Kraken::new(check.fx.pair.replace('/', ""))
                .map(|source| Arc::new(source) as Arc<dyn PriceSource>)
                .map_err(|error| ConfirmConfigError(error.to_string()))
        }
        other => Err(ConfirmConfigError(format!(
            "unsupported price source `{other}`"
        ))),
    }
}

#[derive(Clone)]
struct ConfirmationContext {
    address: Address,
    product_id: Uuid,
    lock_ref: Option<String>,
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
        SELECT address.address, address.lock_ref, account.product_id, address.kind,
               rate_lock.route, rate_lock.amount_atomic::text AS amount_atomic,
               rate_lock.price_scaled::text AS price_scaled,
               rate_lock.credit_minor::text AS credit_minor,
               rate_lock.expires_at, rate_lock.consumed_by
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
    let lock = if kind == "lock" && consumed_by.is_none() {
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
        lock_ref: row.try_get("lock_ref")?,
        lock,
    })
}

enum FinalityResult {
    Ready(TransferLog),
    Wait(Value),
    Retry(Value),
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
                return FinalityResult::Retry(json!({
                    "stage": "finality",
                    "error": "rpc_failure",
                    "provider_a_head": chain_result_code(&primary_head),
                    "provider_b_head": chain_result_code(&secondary_head),
                    "provider_a_receipt": chain_result_code(&primary_log),
                    "provider_b_receipt": chain_result_code(&secondary_log),
                }));
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
                return FinalityResult::Retry(json!({
                    "stage": "finality",
                    "error": "recipient_mismatch",
                    "expected_to": format!("{address:#x}"),
                    "provider_a": provider_evidence(&primary),
                    "provider_b": provider_evidence(&secondary),
                }));
            }
            FinalityResult::Ready(primary)
        }
        (primary, secondary) => FinalityResult::Retry(json!({
            "stage": "finality",
            "error": "rpc_disagreement",
            "provider_a": primary.as_ref().map(provider_evidence),
            "provider_b": secondary.as_ref().map(provider_evidence),
        })),
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

struct Quote {
    price: ScaledPrice,
    evidence: Value,
}

async fn fetch_quote(runtime: &RouteRuntime) -> Result<Quote, Value> {
    match runtime.route.pricing.mode {
        PricingMode::Spot => {
            let Some(check_source) = runtime.check.as_ref() else {
                return Err(json!({"stage": "pricing", "error": "missing_check_source"}));
            };
            let Some(fx_source) = runtime.fx.as_ref() else {
                return Err(json!({"stage": "pricing", "error": "missing_fx_source"}));
            };
            let (primary, check, fx) = tokio::join!(
                runtime.primary.observe(),
                check_source.observe(),
                fx_source.observe()
            );
            let evidence = json!({
                "mode": "spot",
                "primary": observation_result(&primary),
                "check": observation_result(&check),
                "fx": observation_result(&fx),
            });
            let (primary, check, fx) = match (primary, check, fx) {
                (Ok(primary), Ok(check), Ok(fx)) => (primary, check, fx),
                _ => {
                    return Err(
                        json!({"stage": "pricing", "error": "source_failure", "quote": evidence}),
                    );
                }
            };
            let fx = FxObservation {
                source: fx.source.clone(),
                rate: fx.price,
                observed_at: fx.observed_at,
            };
            let now = pricing_validation_time()?;
            match validate_spot(
                &primary,
                &check,
                Some(&fx),
                now,
                ValuationPolicy::from(&runtime.route.pricing),
            ) {
                Ok(price) => Ok(Quote { price, evidence }),
                Err(error) => Err(json!({
                    "stage": "pricing",
                    "error": valuation_error_code(&error),
                    "quote": evidence,
                })),
            }
        }
        PricingMode::Stablecoin => {
            let primary = runtime.primary.observe().await;
            let evidence = json!({
                "mode": "stablecoin",
                "primary": observation_result(&primary),
            });
            let primary = match primary {
                Ok(primary) => primary,
                Err(_) => {
                    return Err(
                        json!({"stage": "pricing", "error": "source_failure", "quote": evidence}),
                    );
                }
            };
            let now = pricing_validation_time()?;
            match stablecoin_price(&primary, now, ValuationPolicy::from(&runtime.route.pricing)) {
                Ok(price) => Ok(Quote { price, evidence }),
                Err(error) => Err(json!({
                    "stage": "pricing",
                    "error": valuation_error_code(&error),
                    "quote": evidence,
                })),
            }
        }
    }
}

fn pricing_validation_time() -> Result<UnixSeconds, Value> {
    unix_seconds(Utc::now()).ok_or_else(|| json!({"stage": "pricing", "error": "invalid_clock"}))
}

fn observation_result(result: &Result<Observation, topup_adapters::pricing::PriceError>) -> Value {
    match result {
        Ok(observation) => json!({
            "source": observation.source.as_str(),
            "price_scaled": observation.price.value().to_string(),
            "observed_at": observation.observed_at.value(),
        }),
        Err(_) => json!({"error": "unavailable"}),
    }
}

fn valuation_error_code(error: &ValuationError) -> &'static str {
    match error {
        ValuationError::Stale { .. } => "stale",
        ValuationError::Divergent { .. } => "divergent",
        ValuationError::FxDepeg => "fx_depeg",
        ValuationError::FxMissing => "fx_missing",
        ValuationError::Depeg => "depeg",
        ValuationError::BelowMinimum => "below_minimum",
        ValuationError::ArithmeticOutOfRange | ValuationError::Credit(_) => "out_of_range",
    }
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
    quote: &Quote,
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
        events: vec![rejected_event(deposit.id, product_id, reason)],
        effects,
    }
}

fn rejected_event(deposit_id: Uuid, product_id: Uuid, reason: RejectReason) -> OutboxEvent {
    OutboxEvent {
        id: Uuid::new_v4(),
        event_type: "deposit.rejected".to_owned(),
        payload: json!({
            "product_id": product_id,
            "deposit_id": deposit_id,
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

fn adopt_answer(
    deposit: &Deposit,
    context: &ConfirmationContext,
    key: String,
    answer: ProductAnswer,
) -> StepResult {
    let adopted = match parse_adopted_valuation(&answer.payload) {
        Ok(adopted) => adopted,
        Err(error) => {
            return retry(
                RetryError::InvariantViolation,
                json!({"stage": "product_lookup", "error": error}),
                TransitionEffects::default(),
            );
        }
    };
    if adopted.lock_ref.as_deref() != context.lock_ref.as_deref() && adopted.lock_ref.is_some() {
        return retry(
            RetryError::InvariantViolation,
            json!({"stage": "product_lookup", "error": "product_payload_lock_ref"}),
            TransitionEffects::default(),
        );
    }
    let ProductAnswer {
        accepted,
        destination_tx_id,
        payload,
    } = answer;
    let events = if accepted {
        Vec::new()
    } else {
        vec![rejected_event(
            deposit.id,
            context.product_id,
            RejectReason::ProductRefused,
        )]
    };
    let lock_consumption = adopted.lock_ref.map(|_| LockConsumption {
        address_id: deposit.address_id,
        idempotent: true,
    });
    let valuation = adopted.valuation;
    StepResult {
        outcome: StepOutcome::AdoptProductAnswer { credited: accepted },
        evidence: json!({
            "stage": "product_lookup",
            "result": if accepted { "accepted" } else { "rejected" },
            "key": key,
            "destination_tx_id": destination_tx_id,
            "valuation_at": valuation.valuation_at,
            "price_scaled": valuation.price_scaled.to_string(),
            "credit_minor": valuation.credit_minor.value().to_string(),
            "deposit_id": deposit.id,
        }),
        events,
        effects: TransitionEffects {
            canonical_evidence: None,
            valuation: Some(valuation),
            settlement_adoption: Some(SettlementAdoption {
                key,
                payload,
                accepted,
                destination_tx_id,
            }),
            lock_consumption,
        },
    }
}

struct AdoptedValuation {
    valuation: StoredValuation,
    lock_ref: Option<String>,
}

fn parse_adopted_valuation(payload: &Value) -> Result<AdoptedValuation, &'static str> {
    let credit_minor = payload
        .get("amount_minor")
        .and_then(Value::as_str)
        .ok_or("product_payload_amount_minor")?
        .parse::<u64>()
        .map_err(|_| "product_payload_amount_minor")?;
    let evidence = payload
        .get("evidence")
        .and_then(Value::as_object)
        .ok_or("product_payload_evidence")?;
    let price_scaled = evidence
        .get("price_scaled")
        .and_then(Value::as_str)
        .ok_or("product_payload_price_scaled")?
        .parse::<u64>()
        .map_err(|_| "product_payload_price_scaled")?;
    ScaledPrice::new(price_scaled, PRICE_SCALE).map_err(|_| "product_payload_price_scaled")?;
    let valuation_at = evidence
        .get("valuation_at")
        .and_then(Value::as_str)
        .ok_or("product_payload_valuation_at")?;
    let valuation_at = DateTime::parse_from_rfc3339(valuation_at)
        .map_err(|_| "product_payload_valuation_at")?
        .with_timezone(&Utc);
    let lock_ref = match evidence.get("lock_ref") {
        None | Some(Value::Null) => None,
        Some(Value::String(lock_ref)) if !lock_ref.is_empty() => Some(lock_ref.clone()),
        Some(_) => return Err("product_payload_lock_ref"),
    };
    Ok(AdoptedValuation {
        valuation: StoredValuation {
            valuation_at,
            price_scaled,
            price_source: if lock_ref.is_some() { "lock" } else { "spot" }.to_owned(),
            credit_minor: MinorAmount::new(credit_minor),
            quote: payload.clone(),
        },
        lock_ref,
    })
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

    use topup_adapters::pricing::PriceError;
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
        ) -> impl std::future::Future<Output = Result<u64, ChainError>> + Send {
            ready(self.head.clone())
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

    struct MockProduct(Result<Option<ProductAnswer>, ProductLookupError>);

    #[async_trait]
    impl ProductLookup for MockProduct {
        async fn get_by_key(
            &self,
            _key: &str,
        ) -> Result<Option<ProductAnswer>, ProductLookupError> {
            self.0.clone()
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
    async fn adopts_accepted_product_answer_before_external_reads() {
        let answer = ProductAnswer {
            accepted: true,
            destination_tx_id: Some("credit-1".to_owned()),
            payload: product_payload(None),
        };
        let result = step(
            route(PricingMode::Spot),
            chain(100, Vec::new()),
            chain(100, Vec::new()),
            prices(now_seconds()),
            Arc::new(MockProduct(Ok(Some(answer.clone())))),
            context_with_lock_ref(None, Some("lock-1")),
        )
        .run(&deposit(1_000))
        .await;
        assert_eq!(
            result.outcome,
            StepOutcome::AdoptProductAnswer { credited: true }
        );
        assert_eq!(
            result
                .effects
                .settlement_adoption
                .as_ref()
                .expect("adoption")
                .payload,
            answer.payload
        );
        assert_eq!(
            result.effects.valuation.expect("valuation").credit_minor,
            MinorAmount::new(1_234)
        );
        assert!(result.effects.lock_consumption.is_none());
    }

    #[tokio::test]
    async fn adopts_rejected_product_answer() {
        let deposit = deposit(1_000);
        let context = context_with_lock_ref(None, Some("lock-1"));
        let product_id = context.product_id;
        let result = step(
            route(PricingMode::Spot),
            chain(100, Vec::new()),
            chain(100, Vec::new()),
            prices(now_seconds()),
            Arc::new(MockProduct(Ok(Some(ProductAnswer {
                accepted: false,
                destination_tx_id: None,
                payload: product_payload(Some("lock-1")),
            })))),
            context,
        )
        .run(&deposit)
        .await;
        assert_eq!(
            result.outcome,
            StepOutcome::AdoptProductAnswer { credited: false }
        );
        assert_eq!(
            result.effects.valuation.expect("valuation").price_source,
            "lock"
        );
        assert_eq!(result.events.len(), 1);
        assert_rejected_event(&result.events[0], RejectReason::ProductRefused, product_id);
        assert_eq!(
            result.effects.lock_consumption,
            Some(LockConsumption {
                address_id: deposit.address_id,
                idempotent: true,
            })
        );
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
            no_product(),
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
    async fn lagging_provider_waits_for_finality() {
        let deposit = deposit(1_000);
        let log = transfer(&deposit);
        let result = step(
            route(PricingMode::Spot),
            chain(100, vec![log.clone()]),
            chain(9, vec![log]),
            prices(now_seconds()),
            no_product(),
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
            no_product(),
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
            no_product(),
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
            no_product(),
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
            primary: Arc::new(MockPrice(Ok(observation("primary", 10_000_000, now)))),
            check: Some(Arc::new(MockPrice(Ok(observation(
                "check", 10_000_000, now,
            ))))),
            fx: Some(Arc::new(MockPrice(Ok(observation("fx", 100_000_000, now))))),
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
            product_lookup: no_product(),
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
            no_product(),
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
            primary: Arc::new(DelayedPrice {
                source: "primary",
                value: 10_000_000,
                delay: Duration::from_secs(2),
            }),
            check: Some(Arc::new(DelayedPrice {
                source: "check",
                value: 10_000_000,
                delay: Duration::from_secs(2),
            })),
            fx: Some(Arc::new(DelayedPrice {
                source: "fx",
                value: 100_000_000,
                delay: Duration::from_secs(2),
            })),
        };
        let quote = fetch_quote(&runtime)
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
                no_product(),
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
            no_product(),
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
                no_product(),
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
            no_product(),
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
            no_product(),
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
            no_product(),
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
        product_lookup: Arc<dyn ProductLookup>,
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
                    primary: Arc::new(MockPrice(Ok(prices.primary))),
                    check: prices.check.map(|observation| {
                        Arc::new(MockPrice(Ok(observation))) as Arc<dyn PriceSource>
                    }),
                    fx: prices.fx.map(|observation| {
                        Arc::new(MockPrice(Ok(observation))) as Arc<dyn PriceSource>
                    }),
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
            product_lookup,
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
        context_with_lock_ref(lock, None)
    }

    fn context_with_lock_ref(
        lock: Option<StoredLock>,
        lock_ref: Option<&str>,
    ) -> ConfirmationContext {
        ConfirmationContext {
            address: recipient(),
            product_id: Uuid::new_v4(),
            lock_ref: lock_ref.map(str::to_owned),
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

    fn no_product() -> Arc<dyn ProductLookup> {
        Arc::new(MockProduct(Ok(None)))
    }

    fn assert_rejected_event(event: &OutboxEvent, reason: RejectReason, product_id: Uuid) {
        assert_eq!(event.event_type, "deposit.rejected");
        assert_eq!(event.payload["product_id"], product_id.to_string());
        assert!(event.payload["deposit_id"].as_str().is_some());
        assert_eq!(event.payload["reason"], reason.code());
    }

    fn product_payload(lock_ref: Option<&str>) -> Value {
        json!({
            "version": 1,
            "amount_minor": "1234",
            "evidence": {
                "price_scaled": "12345678",
                "valuation_at": "2026-09-22T00:00:00Z",
                "lock_ref": lock_ref,
            }
        })
    }

    fn recipient() -> Address {
        Address::repeat_byte(3)
    }

    fn asset() -> Address {
        Address::from_str("0x6c5bA91642F10282b576d91922Ae6448C9d52f4E").expect("fixture asset")
    }
}

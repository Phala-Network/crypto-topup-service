//! Shared current-price fetching and validation for confirmation and rate-lock creation.

use std::sync::Arc;

use chrono::Utc;
use serde_json::{Value, json};
use topup_adapters::pricing::binance::Binance;
use topup_adapters::pricing::coinmetrics::CoinMetrics;
use topup_adapters::pricing::kraken::Kraken;
use topup_adapters::pricing::{Observation, PriceSource};
use topup_core::money::ScaledPrice;
use topup_core::route::{PricingMode, RouteFile};
use topup_core::valuation::{
    FxObservation, UnixSeconds, ValuationError, ValuationPolicy, stablecoin_price, validate_spot,
};

/// Price adapters selected by one validated route.
pub struct PricingRuntime {
    primary: Arc<dyn PriceSource>,
    check: Option<Arc<dyn PriceSource>>,
    fx: Option<Arc<dyn PriceSource>>,
}

impl PricingRuntime {
    /// Builds the configured production adapters for a route.
    pub fn configured(route: &RouteFile) -> Result<Self, String> {
        let primary = price_source(&route.pricing.primary.source, route)?;
        let (check, fx) = match route.pricing.mode {
            PricingMode::Spot => {
                let check = route
                    .pricing
                    .check
                    .as_ref()
                    .ok_or_else(|| "spot route is missing pricing.check".to_owned())?;
                (
                    Some(price_source(&check.source, route)?),
                    Some(price_source(&check.fx.source, route)?),
                )
            }
            PricingMode::Stablecoin => (None, None),
        };
        Ok(Self { primary, check, fx })
    }

    /// Builds a runtime from injected adapters.
    #[must_use]
    pub fn injected(
        primary: Arc<dyn PriceSource>,
        check: Option<Arc<dyn PriceSource>>,
        fx: Option<Arc<dyn PriceSource>>,
    ) -> Self {
        Self { primary, check, fx }
    }

    /// Fetches and validates a quote using the same policy as confirmation.
    pub async fn fetch(&self, route: &RouteFile) -> Result<ValidatedQuote, Value> {
        match route.pricing.mode {
            PricingMode::Spot => self.fetch_spot(route).await,
            PricingMode::Stablecoin => self.fetch_stablecoin(route).await,
        }
    }

    async fn fetch_spot(&self, route: &RouteFile) -> Result<ValidatedQuote, Value> {
        let Some(check_source) = self.check.as_ref() else {
            return Err(json!({"stage": "pricing", "error": "missing_check_source"}));
        };
        let Some(fx_source) = self.fx.as_ref() else {
            return Err(json!({"stage": "pricing", "error": "missing_fx_source"}));
        };
        let (primary, check, fx) = tokio::join!(
            self.primary.observe(),
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
        let now = validation_time()?;
        validate_spot(
            &primary,
            &check,
            Some(&fx),
            now,
            ValuationPolicy::from(&route.pricing),
        )
        .map(|price| ValidatedQuote {
            price,
            evidence: evidence.clone(),
        })
        .map_err(|error| {
            json!({
                "stage": "pricing",
                "error": valuation_error_code(&error),
                "quote": evidence,
            })
        })
    }

    async fn fetch_stablecoin(&self, route: &RouteFile) -> Result<ValidatedQuote, Value> {
        let primary = self.primary.observe().await;
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
        let now = validation_time()?;
        stablecoin_price(&primary, now, ValuationPolicy::from(&route.pricing))
            .map(|price| ValidatedQuote {
                price,
                evidence: evidence.clone(),
            })
            .map_err(|error| {
                json!({
                    "stage": "pricing",
                    "error": valuation_error_code(&error),
                    "quote": evidence,
                })
            })
    }
}

/// A validated current price plus provider evidence safe to persist.
pub struct ValidatedQuote {
    /// Validated eight-decimal USD price.
    pub price: ScaledPrice,
    /// Sanitized source observations and validation mode.
    pub evidence: Value,
}

fn price_source(source: &str, route: &RouteFile) -> Result<Arc<dyn PriceSource>, String> {
    match source {
        "coinmetrics" => CoinMetrics::new(
            route.pricing.primary.asset.clone(),
            route.pricing.primary.metric.clone(),
            route.pricing.primary.frequency.clone(),
        )
        .map(|source| Arc::new(source) as Arc<dyn PriceSource>)
        .map_err(|error| error.to_string()),
        "binance" => {
            let check = route
                .pricing
                .check
                .as_ref()
                .ok_or_else(|| "binance source requires pricing.check".to_owned())?;
            Binance::new(check.symbol.clone())
                .map(|source| Arc::new(source) as Arc<dyn PriceSource>)
                .map_err(|error| error.to_string())
        }
        "kraken" => {
            let check = route
                .pricing
                .check
                .as_ref()
                .ok_or_else(|| "kraken source requires pricing.check".to_owned())?;
            Kraken::new(check.fx.pair.replace('/', ""))
                .map(|source| Arc::new(source) as Arc<dyn PriceSource>)
                .map_err(|error| error.to_string())
        }
        other => Err(format!("unsupported price source `{other}`")),
    }
}

fn validation_time() -> Result<UnixSeconds, Value> {
    u64::try_from(Utc::now().timestamp())
        .map(UnixSeconds::new)
        .map_err(|_| json!({"stage": "pricing", "error": "invalid_clock"}))
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

/// Stable validation error code shared with transition evidence.
pub(crate) const fn valuation_error_code(error: &ValuationError) -> &'static str {
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

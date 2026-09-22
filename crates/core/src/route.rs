//! Serde schemas and pure validation for attested chain and route files.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use alloy_primitives::Address;
use serde::{Deserialize, Serialize};

use crate::money::{AtomicAmount, Bps, PRICE_SCALE};

/// A complete route file with its inline chain configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteFile {
    /// Chain-specific settings.
    pub chain: ChainConfig,
    /// Stable route name.
    pub route: String,
    /// Attested route version.
    pub version: u64,
    /// Asset settings.
    pub asset: AssetConfig,
    /// Destination product settings.
    pub destination: DestinationConfig,
    /// Price-source and freshness settings.
    pub pricing: PricingConfig,
    /// Deposit screening settings.
    pub screening: ScreeningConfig,
    /// Quote-first rate-lock settings.
    pub rate_lock: RateLockConfig,
    /// State-age alert thresholds.
    pub alerts: AlertsConfig,
}

impl RouteFile {
    /// Validates cross-field constraints required before a route is enabled.
    pub fn validate(&self) -> Result<(), RouteError> {
        self.validate_with_template_addresses(false)
    }

    /// Validates a deployment template while allowing zero factory and treasury placeholders.
    ///
    /// Asset and sanctions-oracle addresses remain subject to normal non-zero validation.
    pub fn validate_template(&self) -> Result<(), RouteError> {
        self.validate_with_template_addresses(true)
    }

    fn validate_with_template_addresses(
        &self,
        allow_template_addresses: bool,
    ) -> Result<(), RouteError> {
        if !allow_template_addresses {
            validate_address(
                "chain.contracts.forwarder_factory",
                self.chain.contracts.forwarder_factory,
            )?;
            validate_address(
                "chain.contracts.implementation",
                self.chain.contracts.implementation,
            )?;
            validate_address("chain.contracts.treasury", self.chain.contracts.treasury)?;
        }
        validate_address("asset.contract", self.asset.contract)?;
        validate_address(
            "screening.sanctions_oracle",
            self.screening.sanctions_oracle,
        )?;
        validate_decimals("asset.decimals", self.asset.decimals)?;
        validate_decimals("destination.unit_decimals", self.destination.unit_decimals)?;
        validate_bps(
            "chain.flush.max_gas_ratio_bps",
            self.chain.flush.max_gas_ratio_bps,
        )?;
        validate_positive(
            "chain.flush.max_fee_per_gas_wei",
            self.chain.flush.max_fee_per_gas_wei,
        )?;
        if self.chain.flush.replacement_bps <= 10_000 {
            return Err(RouteError::validation(
                "chain.flush.replacement_bps",
                "must be greater than 10000",
            ));
        }
        validate_positive(
            "chain.flush.replacement_after_blocks",
            self.chain.flush.replacement_after_blocks,
        )?;
        if self.chain.flush.gas_limit_bps < 10_000 {
            return Err(RouteError::validation(
                "chain.flush.gas_limit_bps",
                "must be at least 10000",
            ));
        }
        validate_positive(
            "chain.flush.rpc_timeout_ms",
            self.chain.flush.rpc_timeout_ms,
        )?;
        validate_positive(
            "chain.flush.balance_batch_size",
            self.chain.flush.balance_batch_size,
        )?;
        validate_positive(
            "chain.flush.recovery_scan_blocks",
            self.chain.flush.recovery_scan_blocks,
        )?;
        validate_positive(
            "chain.flush.estimation_retry_after_s",
            self.chain.flush.estimation_retry_after_s,
        )?;
        validate_positive(
            "chain.flush.maintenance_interval_s",
            self.chain.flush.maintenance_interval_s,
        )?;
        if self.chain.flush.native_price_asset.trim().is_empty() {
            return Err(RouteError::validation(
                "chain.flush.native_price_asset",
                "must not be empty",
            ));
        }
        validate_bps("pricing.max_deviation_bps", self.pricing.max_deviation_bps)?;
        if self.pricing.mode == PricingMode::Spot {
            if self.pricing.check.is_none() {
                return Err(RouteError::validation(
                    "pricing.check",
                    "is required when pricing.mode is spot",
                ));
            }
            let max_fx_deviation_bps = self.pricing.max_fx_deviation_bps.ok_or_else(|| {
                RouteError::validation(
                    "pricing.max_fx_deviation_bps",
                    "is required when pricing.mode is spot",
                )
            })?;
            validate_bps("pricing.max_fx_deviation_bps", max_fx_deviation_bps)?;
        }
        validate_bps("rate_lock.spread_bps", self.rate_lock.spread_bps)?;
        validate_bps(
            "rate_lock.lock_tolerance_bps",
            self.rate_lock.lock_tolerance_bps,
        )?;
        if self.pricing.price_scale != PRICE_SCALE {
            return Err(RouteError::validation(
                "pricing.price_scale",
                format!("must be {PRICE_SCALE}"),
            ));
        }
        validate_positive("pricing.max_age_s", self.pricing.max_age_s)?;
        validate_positive("rate_lock.window_s", self.rate_lock.window_s)?;
        validate_positive(
            "rate_lock.max_creations_per_minute",
            self.rate_lock.max_creations_per_minute,
        )?;
        validate_positive(
            "alerts.stuck_after_s.detected",
            self.alerts.stuck_after_s.detected,
        )?;
        validate_positive(
            "alerts.stuck_after_s.confirmed",
            self.alerts.stuck_after_s.confirmed,
        )?;
        validate_positive(
            "alerts.stuck_after_s.cleared",
            self.alerts.stuck_after_s.cleared,
        )?;
        validate_positive(
            "alerts.stuck_after_s.credited",
            self.alerts.stuck_after_s.credited,
        )?;
        validate_rpc_providers(&self.chain.rpc_providers)?;
        if self.screening.min_deposit_atomic > self.screening.max_deposit_atomic {
            return Err(RouteError::validation(
                "screening.min_deposit_atomic",
                "must not exceed screening.max_deposit_atomic",
            ));
        }
        Ok(())
    }
}

/// Chain-specific configuration embedded in a route file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainConfig {
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Human-readable chain name.
    pub name: String,
    /// Reviewed finality rule, such as `finalized`.
    pub finality: String,
    /// Independent RPC provider identifiers.
    pub rpc_providers: Vec<String>,
    /// Forwarder contract addresses.
    pub contracts: ChainContracts,
    /// Flush scheduling and gas policy.
    pub flush: FlushConfig,
}

/// Contract addresses required for deterministic deposits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainContracts {
    /// Forwarder factory address.
    pub forwarder_factory: Address,
    /// Immutable EIP-1167 forwarder implementation address.
    pub implementation: Address,
    /// Immutable treasury address.
    pub treasury: Address,
}

/// Automatic flush policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlushConfig {
    /// Cron schedule for flush planning.
    pub schedule: String,
    /// Maximum gas-to-value ratio in basis points.
    pub max_gas_ratio_bps: Bps,
    /// Provider asset identifier for the chain's native gas token.
    pub native_price_asset: String,
    /// Hard maximum EIP-1559 fee per gas in wei.
    pub max_fee_per_gas_wei: u64,
    /// Required fee replacement multiplier in basis points.
    pub replacement_bps: u16,
    /// Blocks an unmined transaction waits before replacement.
    pub replacement_after_blocks: u64,
    /// Gas-limit multiplier over the estimate in basis points.
    pub gas_limit_bps: u16,
    /// Timeout for each flusher RPC request.
    pub rpc_timeout_ms: u64,
    /// Maximum addresses in one balance JSON-RPC batch.
    pub balance_batch_size: u64,
    /// Maximum blocks inspected in one nonce-recovery iteration.
    pub recovery_scan_blocks: u64,
    /// Delay before retrying a planning-time singleton exclusion.
    pub estimation_retry_after_s: u64,
    /// Interval between confirmation and replacement maintenance iterations.
    pub maintenance_interval_s: u64,
}

/// Deposited asset configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetConfig {
    /// Asset ticker symbol.
    pub symbol: String,
    /// ERC-20 contract address.
    pub contract: Address,
    /// ERC-20 decimal count.
    pub decimals: u8,
    /// Minimum on-chain balance considered for flushing.
    pub min_flush_atomic: AtomicAmount,
}

/// Product ledger destination configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationConfig {
    /// Product slug.
    pub product: String,
    /// Ledger unit, such as USD.
    pub unit: String,
    /// Number of minor-unit decimal places.
    pub unit_decimals: u8,
    /// Signed settlement endpoint.
    pub settlement_url: String,
    /// Product signing key identifier.
    pub product_kid: String,
}

/// Price validation configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PricingConfig {
    /// Valuation mode selected explicitly by the route.
    pub mode: PricingMode,
    /// Primary reference-rate source.
    pub primary: PrimaryPriceConfig,
    /// Independent market cross-check, required for spot pricing.
    pub check: Option<CheckPriceConfig>,
    /// Decimal scale for stored prices; currently fixed at eight.
    pub price_scale: u8,
    /// Maximum quote age in seconds.
    pub max_age_s: u64,
    /// Maximum primary/check divergence.
    pub max_deviation_bps: Bps,
    /// Maximum FX deviation.
    pub max_fx_deviation_bps: Option<Bps>,
}

/// Explicit route valuation mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingMode {
    /// Validate a primary asset/USD rate against an independent market and FX leg.
    Spot,
    /// Credit at one dollar after validating the primary rate as a depeg guard.
    Stablecoin,
}

/// Primary reference-rate descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrimaryPriceConfig {
    /// Provider identifier.
    pub source: String,
    /// Provider asset identifier.
    pub asset: String,
    /// Provider metric name.
    pub metric: String,
    /// Sampling frequency.
    pub frequency: String,
}

/// Market cross-check descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckPriceConfig {
    /// Provider identifier.
    pub source: String,
    /// Market symbol.
    pub symbol: String,
    /// FX cross-check descriptor.
    pub fx: FxPriceConfig,
}

/// Foreign-exchange cross-check descriptor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FxPriceConfig {
    /// Provider identifier.
    pub source: String,
    /// Market pair.
    pub pair: String,
}

/// Screening thresholds and oracle address.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreeningConfig {
    /// Sanctions oracle contract address.
    pub sanctions_oracle: Address,
    /// Minimum creditable deposit.
    pub min_deposit_atomic: AtomicAmount,
    /// Maximum creditable deposit.
    pub max_deposit_atomic: AtomicAmount,
    /// Minimum destination credit.
    pub min_credit_minor: u64,
}

/// Quote-first rate-lock policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLockConfig {
    /// Whether quote-first locks may be created.
    pub enabled: bool,
    /// Lock lifetime in seconds.
    pub window_s: u64,
    /// Price spread in basis points.
    pub spread_bps: Bps,
    /// Accepted transfer amount tolerance in basis points.
    pub lock_tolerance_bps: Bps,
    /// Maximum successful lock creations per account in one rolling minute.
    pub max_creations_per_minute: u64,
    /// Open exposure caps.
    pub max_open_minor: ExposureCaps,
}

/// Open rate-lock exposure caps in destination minor units.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExposureCaps {
    /// Per-account cap.
    pub account: u64,
    /// Per-product cap.
    pub product: u64,
    /// Global cap.
    pub global: u64,
}

/// Operational alert thresholds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertsConfig {
    /// Maximum age per active deposit state.
    pub stuck_after_s: StuckAfterConfig,
}

/// Maximum ages for active deposit states, in seconds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StuckAfterConfig {
    /// Detected-state threshold.
    pub detected: u64,
    /// Confirmed-state threshold.
    pub confirmed: u64,
    /// Cleared-state threshold.
    pub cleared: u64,
    /// Credited-state threshold.
    pub credited: u64,
}

/// Route validation failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RouteError {
    /// A parsed field violated a domain constraint.
    Validation {
        /// Dotted path to the invalid field.
        field: &'static str,
        /// Human-readable constraint failure.
        message: String,
    },
}

impl RouteError {
    fn validation(field: &'static str, message: impl Into<String>) -> Self {
        Self::Validation {
            field,
            message: message.into(),
        }
    }
}

impl fmt::Display for RouteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation { field, message } => {
                write!(formatter, "invalid route field `{field}`: {message}")
            }
        }
    }
}

impl Error for RouteError {}

fn validate_address(field: &'static str, address: Address) -> Result<(), RouteError> {
    if address.is_zero() {
        return Err(RouteError::validation(
            field,
            "must not be the zero address",
        ));
    }
    Ok(())
}

fn validate_decimals(field: &'static str, decimals: u8) -> Result<(), RouteError> {
    if decimals > 36 {
        return Err(RouteError::validation(field, "must be at most 36"));
    }
    Ok(())
}

fn validate_bps(field: &'static str, bps: Bps) -> Result<(), RouteError> {
    if bps.value() > 10_000 {
        return Err(RouteError::validation(field, "must be at most 10000"));
    }
    Ok(())
}

fn validate_positive(field: &'static str, value: u64) -> Result<(), RouteError> {
    if value == 0 {
        return Err(RouteError::validation(field, "must be greater than zero"));
    }
    Ok(())
}

fn validate_rpc_providers(providers: &[String]) -> Result<(), RouteError> {
    if providers.len() < 2 {
        return Err(RouteError::validation(
            "chain.rpc_providers",
            "must contain at least two providers",
        ));
    }

    let mut unique = BTreeSet::new();
    for provider in providers {
        let provider = provider.trim();
        if provider.is_empty() {
            return Err(RouteError::validation(
                "chain.rpc_providers",
                "provider ids must not be empty",
            ));
        }
        if !unique.insert(provider) {
            return Err(RouteError::validation(
                "chain.rpc_providers",
                "provider ids must be unique",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rpc_providers_must_be_distinct_and_non_empty() {
        assert!(
            validate_rpc_providers(&["alchemy".to_owned()])
                .expect_err("one provider must fail")
                .to_string()
                .contains("at least two")
        );
        assert!(
            validate_rpc_providers(&["alchemy".to_owned(), "alchemy".to_owned()])
                .expect_err("duplicate providers must fail")
                .to_string()
                .contains("unique")
        );
        assert!(
            validate_rpc_providers(&[String::new(), String::new()])
                .expect_err("empty providers must fail")
                .to_string()
                .contains("must not be empty")
        );
    }
}

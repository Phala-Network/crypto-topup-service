//! Serde schemas and pure validation for attested chain and route files.

use std::collections::BTreeSet;
use std::num::NonZeroU32;

use alloy_primitives::{Address, U256, address, keccak256};
use serde::{Deserialize, Serialize};

use crate::money::{AtomicAmount, Bps};

/// A resolved route: the route file with every default applied.
///
/// Route files are parsed as [`RouteSpec`], which names only the values that differ per route
/// or environment; every other value is a documented code default here, attested with the image
/// digest. Serializing a route writes the resolved [`RouteSpec`], every field explicit, which
/// parses back to the same route.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RouteSpec", into = "RouteSpec")]
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
                "chain.forwarder_factory",
                self.chain.contracts.forwarder_factory,
            )?;
            validate_address("chain.implementation", self.chain.contracts.implementation)?;
            validate_address("chain.treasury", self.chain.contracts.treasury)?;
        }
        validate_address("asset.contract", self.asset.contract)?;
        validate_address("chain.sanctions_oracle", self.screening.sanctions_oracle)?;
        validate_slug("asset.symbol", &self.asset.symbol)?;
        validate_slug("product", &self.destination.product)?;
        self.chain.operator_key_version()?;
        validate_decimals("asset.decimals", self.asset.decimals)?;
        validate_decimals("unit_decimals", self.destination.unit_decimals)?;
        validate_bps(
            "chain.flush.max_gas_ratio_bps",
            self.chain.flush.max_gas_ratio_bps,
        )?;
        validate_positive(
            "chain.flush.max_fee_per_gas_wei",
            self.chain.flush.max_fee_per_gas_wei,
        )?;
        if self.chain.flush.min_operator_balance_wei.value().is_zero() {
            return Err(RouteError::validation(
                "chain.flush.min_operator_balance_wei",
                "must be greater than zero",
            ));
        }
        if self.chain.flush.replacement_bps <= 10_000 {
            return Err(RouteError::validation(
                "chain.flush.replacement_bps",
                "must be greater than 10000",
            ));
        }
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
        validate_bps("quote.spread_bps", self.rate_lock.spread_bps)?;
        validate_bps("quote.tolerance_bps", self.rate_lock.lock_tolerance_bps)?;
        validate_positive("pricing.max_age_s", self.pricing.max_age_s)?;
        validate_positive("quote.window_s", self.rate_lock.window_s)?;
        validate_positive(
            "quote.max_creations_per_minute",
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
            "alerts.stuck_after_s.credited",
            self.alerts.stuck_after_s.credited,
        )?;
        validate_rpc_providers(&self.chain.rpc_providers)?;
        if self.screening.min_deposit_atomic > self.screening.max_deposit_atomic {
            return Err(RouteError::validation(
                "limits.min_deposit_atomic",
                "must not exceed limits.max_deposit_atomic",
            ));
        }
        Ok(())
    }
}

/// Chain-specific configuration of a resolved route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainConfig {
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Independent RPC provider identifiers.
    pub rpc_providers: Vec<String>,
    /// Derivation version of the operator key, selecting the `operator/v{n}` signer domain.
    pub operator_key_version: u32,
    /// Forwarder contract addresses.
    pub contracts: ChainContracts,
    /// Flush scheduling and gas policy.
    pub flush: FlushConfig,
}

impl ChainConfig {
    /// Returns the operator key derivation version, which must be at least one.
    pub fn operator_key_version(&self) -> Result<NonZeroU32, RouteError> {
        NonZeroU32::new(self.operator_key_version).ok_or_else(|| {
            RouteError::validation("chain.operator_key_version", "must be at least 1")
        })
    }
}

/// Contract addresses required for deterministic deposits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainContracts {
    /// Forwarder factory address.
    pub forwarder_factory: Address,
    /// Immutable EIP-1167 forwarder implementation address.
    pub implementation: Address,
    /// Immutable treasury address.
    pub treasury: Address,
}

/// Automatic flush policy.
#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Operator native balance in wei below which the flusher raises a gas-reserve alert.
    pub min_operator_balance_wei: AtomicAmount,
}

/// Deposited asset configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetConfig {
    /// Lowercase asset code the API names the asset by, such as `pha`.
    pub symbol: String,
    /// ERC-20 contract address.
    pub contract: Address,
    /// ERC-20 decimal count.
    pub decimals: u8,
    /// Minimum on-chain balance considered for flushing.
    pub min_flush_atomic: AtomicAmount,
    /// Minimum deposit amount eligible for a treasury refund.
    pub min_refund_atomic: AtomicAmount,
}

/// Product ledger destination configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DestinationConfig {
    /// Product slug.
    pub product: String,
    /// Number of USD minor-unit decimal places.
    pub unit_decimals: u8,
}

impl DestinationConfig {
    /// The key id the product signs its requests with: `{product}/v1`.
    ///
    /// A product key rotation replaces the stored public key under the same key id.
    #[must_use]
    pub fn product_kid(&self) -> String {
        format!("{}/v1", self.product)
    }
}

/// Price validation configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PricingConfig {
    /// Valuation mode selected explicitly by the route.
    pub mode: PricingMode,
    /// Primary reference-rate source.
    pub primary: PrimaryPriceConfig,
    /// Independent market cross-check, required for spot pricing.
    pub check: Option<CheckPriceConfig>,
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
    /// Provider asset identifier; the metric is always the one-minute `ReferenceRateUSD`.
    pub asset: String,
}

/// Market cross-check descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateLockConfig {
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlertsConfig {
    /// Maximum age per active deposit state.
    pub stuck_after_s: StuckAfterConfig,
}

/// Maximum ages for active deposit states, in seconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StuckAfterConfig {
    /// Detected-state threshold.
    pub detected: u64,
    /// Confirmed-state threshold.
    pub confirmed: u64,
    /// Credited-state threshold.
    pub credited: u64,
}

/// A route file as written: the values that differ per route or environment, plus optional
/// overrides of the code defaults below. See `docs/architecture.md` §14 for each default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteSpec {
    /// Stable route name.
    pub route: String,
    /// Attested route version.
    pub version: u64,
    /// Product slug; the product signs with key id `{product}/v1`.
    pub product: String,
    /// USD minor-unit decimals; default [`DEFAULT_UNIT_DECIMALS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_decimals: Option<u8>,
    /// Chain settings.
    pub chain: ChainSpec,
    /// Deposited asset.
    pub asset: AssetSpec,
    /// Price sources.
    pub pricing: PricingSpec,
    /// Policy limits.
    pub limits: LimitsSpec,
    /// Quote policy overrides.
    #[serde(default, skip_serializing_if = "QuoteSpec::is_empty")]
    pub quote: QuoteSpec,
    /// Alert threshold overrides.
    #[serde(default, skip_serializing_if = "AlertsSpec::is_empty")]
    pub alerts: AlertsSpec,
}

/// Chain settings of a route file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChainSpec {
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Forwarder factory address.
    pub forwarder_factory: Address,
    /// Treasury address, immutable in the factory's implementation.
    pub treasury: Address,
    /// Forwarder implementation; default the factory's first `CREATE` ([`factory_implementation`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implementation: Option<Address>,
    /// Sanctions oracle; default [`default_sanctions_oracle`] for the chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sanctions_oracle: Option<Address>,
    /// RPC provider ids; default [`DEFAULT_RPC_PROVIDERS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_providers: Option<Vec<String>>,
    /// Operator key derivation version; default 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_key_version: Option<u32>,
    /// Flush policy overrides.
    #[serde(default, skip_serializing_if = "FlushSpec::is_empty")]
    pub flush: FlushSpec,
}

/// Flush policy overrides; each field defaults to the `DEFAULT_FLUSH_*` constant.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlushSpec {
    /// Cron schedule for flush planning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
    /// Maximum gas-to-value ratio in basis points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_gas_ratio_bps: Option<Bps>,
    /// Price-source asset id of the native gas token; default [`default_native_price_asset`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_price_asset: Option<String>,
    /// Hard maximum EIP-1559 fee per gas in wei.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_fee_per_gas_wei: Option<u64>,
    /// Required fee replacement multiplier in basis points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_bps: Option<u16>,
    /// Operator gas reserve alert threshold in wei.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_operator_balance_wei: Option<AtomicAmount>,
}

impl FlushSpec {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Deposited asset of a route file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetSpec {
    /// Lowercase asset code, such as `pha`.
    pub symbol: String,
    /// ERC-20 contract address.
    pub contract: Address,
    /// ERC-20 decimal count, attested because credit math depends on it.
    pub decimals: u8,
}

/// Price sources of a route file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PricingSpec {
    /// Valuation mode; default spot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<PricingMode>,
    /// Primary reference-rate source.
    pub primary: PrimaryPriceConfig,
    /// Market cross-check, required in spot mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<CheckPriceSpec>,
    /// Maximum observation age; default [`DEFAULT_PRICE_MAX_AGE_S`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_s: Option<u64>,
    /// Maximum primary/check divergence; default [`DEFAULT_PRICE_MAX_DEVIATION_BPS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_deviation_bps: Option<Bps>,
    /// Maximum FX divergence in spot mode; default [`DEFAULT_PRICE_MAX_FX_DEVIATION_BPS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_fx_deviation_bps: Option<Bps>,
}

/// Market cross-check of a route file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckPriceSpec {
    /// Provider identifier.
    pub source: String,
    /// Market symbol.
    pub symbol: String,
    /// FX leg; default Kraken `USDT/USD` for a USDT-quoted market, required otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fx: Option<FxPriceConfig>,
}

/// Policy limits of a route file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsSpec {
    /// Minimum credit in USD minor units; smaller deposits are rejected `below_minimum`.
    pub min_credit_minor: u64,
    /// Maximum creditable deposit in token base units.
    pub max_deposit_atomic: AtomicAmount,
    /// Minimum refundable amount in token base units.
    pub min_refund_atomic: AtomicAmount,
    /// Open quote exposure caps in USD minor units.
    pub max_open_minor: ExposureCaps,
    /// Minimum creditable deposit in token base units; default 0 (`min_credit_minor` governs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_deposit_atomic: Option<AtomicAmount>,
    /// Minimum address balance to flush; default 0 (the gas-ratio rule governs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_flush_atomic: Option<AtomicAmount>,
}

/// Quote policy overrides; each field defaults to the `DEFAULT_QUOTE_*` constant.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuoteSpec {
    /// Payment window in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_s: Option<u64>,
    /// Spread below spot in basis points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spread_bps: Option<Bps>,
    /// Accepted payment tolerance in basis points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tolerance_bps: Option<Bps>,
    /// Quote creations per account in a rolling minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_creations_per_minute: Option<u64>,
}

impl QuoteSpec {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Alert threshold overrides.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertsSpec {
    /// Maximum age per active deposit state.
    #[serde(default, skip_serializing_if = "StuckAfterSpec::is_empty")]
    pub stuck_after_s: StuckAfterSpec,
}

impl AlertsSpec {
    fn is_empty(&self) -> bool {
        self.stuck_after_s.is_empty()
    }
}

/// Maximum ages per active deposit state, in seconds; each defaults to `DEFAULT_STUCK_AFTER_*`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StuckAfterSpec {
    /// Detected-state threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected: Option<u64>,
    /// Confirmed-state threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed: Option<u64>,
    /// Credited-state threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credited: Option<u64>,
}

impl StuckAfterSpec {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// USD cents.
pub const DEFAULT_UNIT_DECIMALS: u8 = 2;
/// Provider ids whose URLs are `TOPUP_RPC_PROVIDER_A_URL` and `TOPUP_RPC_PROVIDER_B_URL`.
pub const DEFAULT_RPC_PROVIDERS: [&str; 2] = ["provider-a", "provider-b"];
/// The first operator key; bumped only after an operator rotation.
pub const DEFAULT_OPERATOR_KEY_VERSION: u32 = 1;
/// Flush planning every six hours.
pub const DEFAULT_FLUSH_SCHEDULE: &str = "0 */6 * * *";
/// An address's share of batch gas is at most 2% of its value.
pub const DEFAULT_FLUSH_MAX_GAS_RATIO_BPS: u16 = 200;
/// 500 gwei: a runaway-fee guard far above normal Ethereum and Base fees.
pub const DEFAULT_FLUSH_MAX_FEE_PER_GAS_WEI: u64 = 500_000_000_000;
/// A 25% fee bump, above every client's replacement rule (geth needs 10%).
pub const DEFAULT_FLUSH_REPLACEMENT_BPS: u16 = 12_500;
/// 0.05 ETH of operator gas reserve.
pub const DEFAULT_FLUSH_MIN_OPERATOR_BALANCE_WEI: u64 = 50_000_000_000_000_000;
/// Two Coin Metrics one-minute reference-rate intervals.
pub const DEFAULT_PRICE_MAX_AGE_S: u64 = 120;
/// 1% between the primary rate and the market check.
pub const DEFAULT_PRICE_MAX_DEVIATION_BPS: u16 = 100;
/// 0.5% between USDT and USD flags a depeg.
pub const DEFAULT_PRICE_MAX_FX_DEVIATION_BPS: u16 = 50;
/// A 15-minute payment window.
pub const DEFAULT_QUOTE_WINDOW_S: u64 = 900;
/// Quotes are priced 0.5% below spot.
pub const DEFAULT_QUOTE_SPREAD_BPS: u16 = 50;
/// 1% absorbs wallet rounding without accepting a real underpayment.
pub const DEFAULT_QUOTE_TOLERANCE_BPS: u16 = 100;
/// Quote creations per account in a rolling minute.
pub const DEFAULT_QUOTE_MAX_CREATIONS_PER_MINUTE: u64 = 10;
/// Detected deposits normally confirm within minutes.
pub const DEFAULT_STUCK_AFTER_DETECTED_S: u64 = 1_800;
/// Confirmed deposits normally credit within minutes.
pub const DEFAULT_STUCK_AFTER_CONFIRMED_S: u64 = 1_800;
/// Credited deposits wait for a flush (six-hourly, gas-ratio gated): two days.
pub const DEFAULT_STUCK_AFTER_CREDITED_S: u64 = 172_800;

const CHAINALYSIS_ORACLE: Address = address!("0x40C57923924B5c5c5455c48D93317139ADDaC8fb");
const CHAINALYSIS_ORACLE_BASE: Address = address!("0x3A91A31cB3dC49b4db9Ce721F50a9D076c8D739B");

/// The Chainalysis sanctions oracle published for `chain_id`, if any
/// (<https://go.chainalysis.com/chainalysis-oracle-docs.html>).
#[must_use]
pub const fn default_sanctions_oracle(chain_id: u64) -> Option<Address> {
    match chain_id {
        1 | 10 | 56 | 137 | 250 | 42_161 | 42_220 | 43_114 => Some(CHAINALYSIS_ORACLE),
        8_453 => Some(CHAINALYSIS_ORACLE_BASE),
        _ => None,
    }
}

/// The price-source asset id of `chain_id`'s native gas token, if known.
#[must_use]
pub const fn default_native_price_asset(chain_id: u64) -> Option<&'static str> {
    match chain_id {
        // Ethereum, Sepolia, and Base pay gas in ETH.
        1 | 11_155_111 | 8_453 => Some("eth"),
        _ => None,
    }
}

/// The forwarder implementation `ForwarderFactory` creates in its constructor: the factory's
/// first `CREATE`, at nonce 1 (EIP-161), `keccak256(rlp([factory, 1]))[12..]`.
#[must_use]
pub fn factory_implementation(factory: Address) -> Address {
    let mut preimage = Vec::with_capacity(23);
    // RLP list of 22 payload bytes: a 20-byte string (0x94 prefix) and the single byte 0x01.
    preimage.extend_from_slice(&[0xd6, 0x94]);
    preimage.extend_from_slice(factory.as_slice());
    preimage.push(0x01);
    Address::from_word(keccak256(preimage))
}

impl TryFrom<RouteSpec> for RouteFile {
    type Error = RouteError;

    fn try_from(spec: RouteSpec) -> Result<Self, Self::Error> {
        let chain_id = spec.chain.chain_id;
        let flush = spec.chain.flush;
        let native_price_asset = flush
            .native_price_asset
            .or_else(|| default_native_price_asset(chain_id).map(str::to_owned))
            .ok_or_else(|| {
                RouteError::validation(
                    "chain.flush.native_price_asset",
                    format!("is required for chain {chain_id}, which has no default"),
                )
            })?;
        let sanctions_oracle = spec
            .chain
            .sanctions_oracle
            .or_else(|| default_sanctions_oracle(chain_id))
            .ok_or_else(|| {
                RouteError::validation(
                    "chain.sanctions_oracle",
                    format!("is required for chain {chain_id}, which has no Chainalysis oracle"),
                )
            })?;
        let check = spec
            .pricing
            .check
            .map(|check| {
                let fx = match check.fx {
                    Some(fx) => fx,
                    None if check.symbol.ends_with("USDT") => FxPriceConfig {
                        source: "kraken".to_owned(),
                        pair: "USDT/USD".to_owned(),
                    },
                    None => {
                        return Err(RouteError::validation(
                            "pricing.check.fx",
                            "is required for a market not quoted in USDT",
                        ));
                    }
                };
                Ok(CheckPriceConfig {
                    source: check.source,
                    symbol: check.symbol,
                    fx,
                })
            })
            .transpose()?;
        let mode = spec.pricing.mode.unwrap_or(PricingMode::Spot);
        let max_fx_deviation_bps = match (spec.pricing.max_fx_deviation_bps, mode) {
            (Some(bps), _) => Some(bps),
            (None, PricingMode::Spot) => Some(bps(
                "pricing.max_fx_deviation_bps",
                DEFAULT_PRICE_MAX_FX_DEVIATION_BPS,
            )?),
            (None, PricingMode::Stablecoin) => None,
        };
        let stuck = spec.alerts.stuck_after_s;
        Ok(Self {
            chain: ChainConfig {
                chain_id,
                rpc_providers: spec.chain.rpc_providers.unwrap_or_else(|| {
                    DEFAULT_RPC_PROVIDERS
                        .iter()
                        .map(|&id| id.to_owned())
                        .collect()
                }),
                operator_key_version: spec
                    .chain
                    .operator_key_version
                    .unwrap_or(DEFAULT_OPERATOR_KEY_VERSION),
                contracts: ChainContracts {
                    forwarder_factory: spec.chain.forwarder_factory,
                    implementation: spec
                        .chain
                        .implementation
                        .unwrap_or_else(|| factory_implementation(spec.chain.forwarder_factory)),
                    treasury: spec.chain.treasury,
                },
                flush: FlushConfig {
                    schedule: flush
                        .schedule
                        .unwrap_or_else(|| DEFAULT_FLUSH_SCHEDULE.to_owned()),
                    max_gas_ratio_bps: match flush.max_gas_ratio_bps {
                        Some(value) => value,
                        None => bps(
                            "chain.flush.max_gas_ratio_bps",
                            DEFAULT_FLUSH_MAX_GAS_RATIO_BPS,
                        )?,
                    },
                    native_price_asset,
                    max_fee_per_gas_wei: flush
                        .max_fee_per_gas_wei
                        .unwrap_or(DEFAULT_FLUSH_MAX_FEE_PER_GAS_WEI),
                    replacement_bps: flush
                        .replacement_bps
                        .unwrap_or(DEFAULT_FLUSH_REPLACEMENT_BPS),
                    min_operator_balance_wei: flush.min_operator_balance_wei.unwrap_or_else(|| {
                        AtomicAmount::new(U256::from(DEFAULT_FLUSH_MIN_OPERATOR_BALANCE_WEI))
                    }),
                },
            },
            route: spec.route,
            version: spec.version,
            asset: AssetConfig {
                symbol: spec.asset.symbol,
                contract: spec.asset.contract,
                decimals: spec.asset.decimals,
                min_flush_atomic: spec.limits.min_flush_atomic.unwrap_or_default(),
                min_refund_atomic: spec.limits.min_refund_atomic,
            },
            destination: DestinationConfig {
                product: spec.product,
                unit_decimals: spec.unit_decimals.unwrap_or(DEFAULT_UNIT_DECIMALS),
            },
            pricing: PricingConfig {
                mode,
                primary: spec.pricing.primary,
                check,
                max_age_s: spec.pricing.max_age_s.unwrap_or(DEFAULT_PRICE_MAX_AGE_S),
                max_deviation_bps: match spec.pricing.max_deviation_bps {
                    Some(value) => value,
                    None => bps("pricing.max_deviation_bps", DEFAULT_PRICE_MAX_DEVIATION_BPS)?,
                },
                max_fx_deviation_bps,
            },
            screening: ScreeningConfig {
                sanctions_oracle,
                min_deposit_atomic: spec.limits.min_deposit_atomic.unwrap_or_default(),
                max_deposit_atomic: spec.limits.max_deposit_atomic,
                min_credit_minor: spec.limits.min_credit_minor,
            },
            rate_lock: RateLockConfig {
                window_s: spec.quote.window_s.unwrap_or(DEFAULT_QUOTE_WINDOW_S),
                spread_bps: match spec.quote.spread_bps {
                    Some(value) => value,
                    None => bps("quote.spread_bps", DEFAULT_QUOTE_SPREAD_BPS)?,
                },
                lock_tolerance_bps: match spec.quote.tolerance_bps {
                    Some(value) => value,
                    None => bps("quote.tolerance_bps", DEFAULT_QUOTE_TOLERANCE_BPS)?,
                },
                max_creations_per_minute: spec
                    .quote
                    .max_creations_per_minute
                    .unwrap_or(DEFAULT_QUOTE_MAX_CREATIONS_PER_MINUTE),
                max_open_minor: spec.limits.max_open_minor,
            },
            alerts: AlertsConfig {
                stuck_after_s: StuckAfterConfig {
                    detected: stuck.detected.unwrap_or(DEFAULT_STUCK_AFTER_DETECTED_S),
                    confirmed: stuck.confirmed.unwrap_or(DEFAULT_STUCK_AFTER_CONFIRMED_S),
                    credited: stuck.credited.unwrap_or(DEFAULT_STUCK_AFTER_CREDITED_S),
                },
            },
        })
    }
}

impl From<RouteFile> for RouteSpec {
    /// The resolved route with every field explicit.
    fn from(route: RouteFile) -> Self {
        Self {
            route: route.route,
            version: route.version,
            product: route.destination.product,
            unit_decimals: Some(route.destination.unit_decimals),
            chain: ChainSpec {
                chain_id: route.chain.chain_id,
                forwarder_factory: route.chain.contracts.forwarder_factory,
                treasury: route.chain.contracts.treasury,
                implementation: Some(route.chain.contracts.implementation),
                sanctions_oracle: Some(route.screening.sanctions_oracle),
                rpc_providers: Some(route.chain.rpc_providers),
                operator_key_version: Some(route.chain.operator_key_version),
                flush: FlushSpec {
                    schedule: Some(route.chain.flush.schedule),
                    max_gas_ratio_bps: Some(route.chain.flush.max_gas_ratio_bps),
                    native_price_asset: Some(route.chain.flush.native_price_asset),
                    max_fee_per_gas_wei: Some(route.chain.flush.max_fee_per_gas_wei),
                    replacement_bps: Some(route.chain.flush.replacement_bps),
                    min_operator_balance_wei: Some(route.chain.flush.min_operator_balance_wei),
                },
            },
            asset: AssetSpec {
                symbol: route.asset.symbol,
                contract: route.asset.contract,
                decimals: route.asset.decimals,
            },
            pricing: PricingSpec {
                mode: Some(route.pricing.mode),
                primary: route.pricing.primary,
                check: route.pricing.check.map(|check| CheckPriceSpec {
                    source: check.source,
                    symbol: check.symbol,
                    fx: Some(check.fx),
                }),
                max_age_s: Some(route.pricing.max_age_s),
                max_deviation_bps: Some(route.pricing.max_deviation_bps),
                max_fx_deviation_bps: route.pricing.max_fx_deviation_bps,
            },
            limits: LimitsSpec {
                min_credit_minor: route.screening.min_credit_minor,
                max_deposit_atomic: route.screening.max_deposit_atomic,
                min_refund_atomic: route.asset.min_refund_atomic,
                max_open_minor: route.rate_lock.max_open_minor,
                min_deposit_atomic: Some(route.screening.min_deposit_atomic),
                min_flush_atomic: Some(route.asset.min_flush_atomic),
            },
            quote: QuoteSpec {
                window_s: Some(route.rate_lock.window_s),
                spread_bps: Some(route.rate_lock.spread_bps),
                tolerance_bps: Some(route.rate_lock.lock_tolerance_bps),
                max_creations_per_minute: Some(route.rate_lock.max_creations_per_minute),
            },
            alerts: AlertsSpec {
                stuck_after_s: StuckAfterSpec {
                    detected: Some(route.alerts.stuck_after_s.detected),
                    confirmed: Some(route.alerts.stuck_after_s.confirmed),
                    credited: Some(route.alerts.stuck_after_s.credited),
                },
            },
        }
    }
}

fn bps(field: &'static str, value: u16) -> Result<Bps, RouteError> {
    Bps::new(value).map_err(|error| RouteError::validation(field, error.to_string()))
}

/// Route validation failure.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RouteError {
    /// A parsed field violated a domain constraint.
    #[error("invalid route field `{field}`: {message}")]
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

fn validate_address(field: &'static str, address: Address) -> Result<(), RouteError> {
    if address.is_zero() {
        return Err(RouteError::validation(
            field,
            "must not be the zero address",
        ));
    }
    Ok(())
}

fn validate_slug(field: &'static str, value: &str) -> Result<(), RouteError> {
    let valid = value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !valid {
        return Err(RouteError::validation(
            field,
            "must match ^[a-z0-9][a-z0-9-]{0,62}$",
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
    fn factory_implementation_is_the_factorys_first_create() {
        // Staging's Sepolia factory and the implementation its constructor created.
        assert_eq!(
            factory_implementation(address!("0x2407bE5Be2b632F5b166872A49E4946a70CCa531")),
            address!("0x70B714508BFa441449DC09f790Ca03Baa5170360")
        );
        // The A1 deterministic test-vector deployment.
        assert_eq!(
            factory_implementation(address!("0xe8A9Ab1AbC7651A5b7C2ED5B662F2f80BF5C446d")),
            address!("0xfeb1871c9897251C74b39DFC74e577888290faE6")
        );
    }

    #[test]
    fn chain_defaults_cover_only_reviewed_chains() {
        assert_eq!(default_sanctions_oracle(1), Some(CHAINALYSIS_ORACLE));
        assert_eq!(
            default_sanctions_oracle(8_453),
            Some(CHAINALYSIS_ORACLE_BASE)
        );
        assert_eq!(default_sanctions_oracle(11_155_111), None);
        assert_eq!(default_native_price_asset(11_155_111), Some("eth"));
        assert_eq!(default_native_price_asset(137), None);
    }

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

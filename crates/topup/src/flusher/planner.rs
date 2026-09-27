use std::sync::Arc;

use alloy_primitives::{Address, B256, U256, U512};
use chrono::{Duration, Utc};
use serde_json::to_value;
use sqlx::PgPool;
use topup_adapters::signer::actor::SignerHandle;
use topup_core::Signer;
use topup_core::money::{AtomicAmount, Bps, ScaledPrice};
use topup_core::route::RouteFile;
use topup_core::screening::PauseScope;
use uuid::Uuid;

use crate::db;
use crate::pause::{self, PauseScopeSources};

use super::types::{
    AlertSink, ChainClient, FlushAlert, FlushCallBinding, FlushEvidence, PlannedAddress,
    PriceSource,
};
use super::{FlusherError, map_chain, map_price, map_signer};

/// Seconds before a singleton excluded by a reverting gas estimate is planned again.
const ESTIMATION_RETRY_AFTER_S: i64 = 3_600;

/// Scheduled per-route flush planner.
pub struct Planner {
    pool: PgPool,
    chain: Arc<dyn ChainClient>,
    signer: SignerHandle,
    prices: Arc<dyn PriceSource>,
    alerts: Arc<dyn AlertSink>,
}

impl Planner {
    /// Creates a planner for one chain client and signer.
    #[must_use]
    pub fn new(
        pool: PgPool,
        chain: Arc<dyn ChainClient>,
        signer: SignerHandle,
        prices: Arc<dyn PriceSource>,
        alerts: Arc<dyn AlertSink>,
    ) -> Self {
        Self {
            pool,
            chain,
            signer,
            prices,
            alerts,
        }
    }

    /// Evaluates a route and persists one or more independently estimable batches.
    pub async fn plan(&self, route: &RouteFile) -> Result<Option<Uuid>, FlusherError> {
        if crate::reconciler::chain_is_blocked(&self.pool, route.chain.chain_id).await? {
            tracing::warn!(
                chain_id = route.chain.chain_id,
                route = %route.route,
                "flush planning skipped because reconciliation froze the chain"
            );
            return Ok(None);
        }
        let operator = self.signer.operator_address().await.map_err(map_signer)?;
        let pending = self
            .chain
            .pending_nonce(operator)
            .await
            .map_err(map_chain)?;
        match self.rebind_existing(route, operator, pending).await? {
            ExistingPlan::None => {}
            ExistingPlan::Planned(id) => return Ok(Some(id)),
            ExistingPlan::Sent => return Ok(None),
        }
        let mut excluded = db::list_active_flush_exclusions(
            &self.pool,
            route.chain.chain_id,
            route.asset.contract,
            Utc::now(),
        )
        .await?
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
        excluded
            .extend(crate::reconciler::blocked_addresses(&self.pool, route.chain.chain_id).await?);
        let route_scopes = pause::route_pause_scopes(&self.pool, &route.route).await?;
        let addresses =
            db::list_chain_addresses_with_pause_scopes(&self.pool, route.chain.chain_id)
                .await?
                .into_iter()
                .filter_map(|scoped| {
                    let paused = PauseScopeSources::from_codes(
                        &scoped.account_scopes,
                        &scoped.product_scopes,
                        &route_scopes,
                    )
                    .map(|scopes| scopes.contains(PauseScope::Flush));
                    match paused {
                        Ok(false) if !excluded.contains(&scoped.address.id) => {
                            Some(Ok(scoped.address))
                        }
                        Ok(_) => None,
                        Err(error) => Some(Err(error)),
                    }
                })
                .collect::<Result<Vec<_>, sqlx::Error>>()?;
        if addresses.is_empty() {
            return Ok(None);
        }
        let physical = addresses
            .iter()
            .map(|address| address.address)
            .collect::<Vec<_>>();
        let balances = self
            .chain
            .token_balances(route.asset.contract, &physical)
            .await
            .map_err(map_chain)?;
        let native = self
            .chain
            .native_balances(&physical)
            .await
            .map_err(map_chain)?;
        if balances.len() != addresses.len() || native.len() != addresses.len() {
            return Err(FlusherError::Invariant(
                "chain balance batch length did not match address count",
            ));
        }
        for (address, amount) in addresses.iter().zip(native) {
            if !amount.is_zero() {
                self.alerts.emit(FlushAlert::NativeBalance {
                    chain_id: route.chain.chain_id,
                    address: address.address,
                    amount,
                });
            }
        }

        let token_price = self
            .prices
            .price_usd(&route.pricing.primary.asset)
            .await
            .map_err(map_price)?;
        let native_price = self
            .prices
            .price_usd(&route.chain.flush.native_price_asset)
            .await
            .map_err(map_price)?;
        let fee = self.chain.fee_quote().await.map_err(map_chain)?;
        let selected = addresses
            .into_iter()
            .zip(balances)
            .filter(|(_, balance)| *balance >= route.asset.min_flush_atomic.value())
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return Ok(None);
        }
        let mut queue = vec![selected];
        let mut evidence = Vec::new();
        while let Some(group) = queue.pop() {
            match self.estimate_group(route, operator, group).await? {
                EstimateOutcome::Ready { plan, gas } => {
                    let count = u64::try_from(plan.len())
                        .map_err(|_| FlusherError::Invariant("batch address count exceeds u64"))?;
                    let retained = plan
                        .into_iter()
                        .map(|item| {
                            gas_ratio_allowed(GasRatioInput {
                                balance: AtomicAmount::new(item.1),
                                token_decimals: route.asset.decimals,
                                batch_gas: gas,
                                address_count: count,
                                max_fee_per_gas: fee.max_fee_per_gas,
                                native_price,
                                token_price,
                                maximum: route.chain.flush.max_gas_ratio_bps,
                            })
                            .map(|allowed| allowed.then_some(item))
                        })
                        .collect::<Result<Vec<_>, _>>()?
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>();
                    if retained.is_empty() {
                        continue;
                    }
                    if retained.len() != count_to_usize(count)? {
                        queue.push(retained);
                        continue;
                    }
                    evidence.push(self.evidence(route, retained, gas));
                }
                EstimateOutcome::Split(left, right) => {
                    queue.push(right);
                    queue.push(left);
                }
                EstimateOutcome::Excluded => {}
            }
        }
        self.persist_plans(route, operator, pending, evidence).await
    }

    async fn rebind_existing(
        &self,
        route: &RouteFile,
        operator: Address,
        pending: u64,
    ) -> Result<ExistingPlan, FlusherError> {
        let mut transaction = self.pool.begin().await?;
        db::lock_flush_plan(&mut transaction, route.chain.chain_id, route.asset.contract).await?;
        db::lock_operator(&mut transaction, route.chain.chain_id, operator).await?;
        if db::has_sent_flush_for_token(
            &mut transaction,
            route.chain.chain_id,
            route.asset.contract,
        )
        .await?
        {
            transaction.commit().await?;
            return Ok(ExistingPlan::Sent);
        }
        let existing = db::rebind_planned_flushes(
            &mut transaction,
            route.chain.chain_id,
            route.asset.contract,
            operator,
            pending,
        )
        .await?;
        transaction.commit().await?;
        Ok(existing
            .first()
            .map_or(ExistingPlan::None, |plan| ExistingPlan::Planned(plan.id)))
    }

    async fn estimate_group(
        &self,
        route: &RouteFile,
        operator: Address,
        group: Vec<(db::Address, U256)>,
    ) -> Result<EstimateOutcome, FlusherError> {
        let salts = group
            .iter()
            .map(|(address, _)| address.salt)
            .collect::<Vec<_>>();
        match self
            .chain
            .estimate_flush_gas(
                route.chain.contracts.forwarder_factory,
                operator,
                route.chain.contracts.treasury,
                &salts,
                route.asset.contract,
            )
            .await
        {
            Ok(gas) => Ok(EstimateOutcome::Ready { plan: group, gas }),
            Err(error) if error.is_estimation_revert() && group.len() > 1 => {
                let middle = group.len() / 2;
                let right = group[middle..].to_vec();
                let left = group[..middle].to_vec();
                Ok(EstimateOutcome::Split(left, right))
            }
            Err(error) if error.is_estimation_revert() => {
                let (address, _) = group
                    .first()
                    .ok_or(FlusherError::Invariant("estimate group is empty"))?;
                let retry_after = Utc::now()
                    .checked_add_signed(Duration::seconds(ESTIMATION_RETRY_AFTER_S))
                    .ok_or(FlusherError::Arithmetic)?;
                let reason = error.to_string();
                db::upsert_flush_exclusion(
                    &self.pool,
                    route.chain.chain_id,
                    route.asset.contract,
                    address.id,
                    &reason,
                    retry_after,
                )
                .await?;
                self.alerts.emit(FlushAlert::PlanningExcluded {
                    chain_id: route.chain.chain_id,
                    token: route.asset.contract,
                    address_id: address.id,
                    reason,
                });
                Ok(EstimateOutcome::Excluded)
            }
            Err(error) => Err(map_chain(error)),
        }
    }

    fn evidence(
        &self,
        route: &RouteFile,
        selected: Vec<(db::Address, U256)>,
        gas: u64,
    ) -> FlushEvidence {
        let plan = selected
            .iter()
            .map(|(address, balance)| PlannedAddress {
                address_id: address.id,
                salt: format!("{:#x}", address.salt),
                address: format!("{:#x}", address.address),
                balance_atomic: balance.to_string(),
            })
            .collect::<Vec<_>>();
        let binding = FlushCallBinding {
            route: route.route.clone(),
            config_version: route.version,
            factory: format!("{:#x}", route.chain.contracts.forwarder_factory),
            token: format!("{:#x}", route.asset.contract),
            treasury: format!("{:#x}", route.chain.contracts.treasury),
            salts: plan.iter().map(|item| item.salt.clone()).collect(),
        };
        FlushEvidence::planned(binding, plan, gas)
    }

    async fn persist_plans(
        &self,
        route: &RouteFile,
        operator: Address,
        pending: u64,
        plans: Vec<FlushEvidence>,
    ) -> Result<Option<Uuid>, FlusherError> {
        if plans.is_empty() {
            return Ok(None);
        }
        let mut transaction = self.pool.begin().await?;
        db::lock_flush_plan(&mut transaction, route.chain.chain_id, route.asset.contract).await?;
        db::lock_operator(&mut transaction, route.chain.chain_id, operator).await?;
        if db::has_open_flush_locked(&mut transaction, route.chain.chain_id, route.asset.contract)
            .await?
        {
            transaction.commit().await?;
            return Ok(None);
        }
        let mut first = None;
        for evidence in plans {
            let nonce =
                db::next_flush_nonce(&mut transaction, route.chain.chain_id, operator, pending)
                    .await?;
            let id = Uuid::new_v4();
            first.get_or_insert(id);
            db::insert_planned_flush(
                &mut transaction,
                id,
                route.chain.chain_id,
                route.asset.contract,
                operator,
                nonce,
                &to_value(evidence)?,
            )
            .await?;
        }
        transaction.commit().await?;
        Ok(first)
    }
}

enum EstimateOutcome {
    Ready {
        plan: Vec<(db::Address, U256)>,
        gas: u64,
    },
    Split(Vec<(db::Address, U256)>, Vec<(db::Address, U256)>),
    Excluded,
}

enum ExistingPlan {
    None,
    Planned(Uuid),
    Sent,
}

/// Applies the planner's per-address gas-to-value policy.
pub fn gas_ratio_allowed(input: GasRatioInput) -> Result<bool, FlusherError> {
    if input.address_count == 0 {
        return Err(FlusherError::Invariant(
            "gas ratio requires at least one address",
        ));
    }
    let share = input
        .batch_gas
        .checked_add(input.address_count.saturating_sub(1))
        .and_then(|value| value.checked_div(input.address_count))
        .ok_or(FlusherError::Arithmetic)?;
    let gas_value = U512::from(share)
        .checked_mul(U512::from(input.max_fee_per_gas))
        .and_then(|value| value.checked_mul(U512::from(input.native_price.value())))
        .and_then(|value| value.checked_mul(power_of_ten(input.token_decimals)))
        .and_then(|value| value.checked_mul(U512::from(10_000_u16)))
        .ok_or(FlusherError::Arithmetic)?;
    let token_value = U512::from(input.balance.value())
        .checked_mul(U512::from(input.token_price.value()))
        .and_then(|value| value.checked_mul(power_of_ten(18)))
        .and_then(|value| value.checked_mul(U512::from(input.maximum.value())))
        .ok_or(FlusherError::Arithmetic)?;
    Ok(gas_value <= token_value)
}

/// Inputs for comparing one address's gas share and token balance in USD.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GasRatioInput {
    /// Token balance in atomic units.
    pub balance: AtomicAmount,
    /// ERC-20 decimal count.
    pub token_decimals: u8,
    /// Estimated gas for the complete batch.
    pub batch_gas: u64,
    /// Addresses sharing the estimated gas evenly.
    pub address_count: u64,
    /// Maximum wei paid per gas unit.
    pub max_fee_per_gas: u128,
    /// USD price of one native gas token.
    pub native_price: ScaledPrice,
    /// USD price of one flushed token.
    pub token_price: ScaledPrice,
    /// Maximum gas-cost share in basis points.
    pub maximum: Bps,
}

fn power_of_ten(exponent: u8) -> U512 {
    U512::from(10_u8).pow(U512::from(exponent))
}

fn count_to_usize(value: u64) -> Result<usize, FlusherError> {
    usize::try_from(value).map_err(|_| FlusherError::Invariant("address count exceeds usize"))
}

pub(super) fn parse_salt(value: &str) -> Result<B256, FlusherError> {
    value
        .parse()
        .map_err(|_| FlusherError::StoredEvidence("invalid salt hex"))
}

pub(super) fn parse_address(value: &str) -> Result<Address, FlusherError> {
    value
        .parse()
        .map_err(|_| FlusherError::StoredEvidence("invalid address hex"))
}

pub(super) fn parse_u256(value: &str) -> Result<U256, FlusherError> {
    value
        .parse()
        .map_err(|_| FlusherError::StoredEvidence("invalid atomic amount"))
}

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;
    use topup_core::money::{AtomicAmount, Bps, PRICE_SCALE, ScaledPrice};

    use super::{GasRatioInput, gas_ratio_allowed};

    fn price() -> ScaledPrice {
        ScaledPrice::new(25_000_000, PRICE_SCALE).expect("test price is valid")
    }

    #[test]
    fn rejects_zero_address_count() {
        assert!(
            gas_ratio_allowed(GasRatioInput {
                balance: AtomicAmount::new(U256::from(1_000_u64)),
                token_decimals: 18,
                batch_gas: 100_000,
                address_count: 0,
                max_fee_per_gas: 1,
                native_price: price(),
                token_price: price(),
                maximum: Bps::new(200).expect("bps is valid"),
            })
            .is_err()
        );
    }

    #[test]
    fn applies_even_ceiling_share_and_boundary_ratio() {
        let maximum = Bps::new(200).expect("bps is valid");
        let fee = 1_000_000_000_u128;
        let gas = 100_001;
        let count = 2;
        let share = 50_001_u128;
        let minimum_balance = share * fee * 10_000 / u128::from(maximum.value());
        assert!(
            gas_ratio_allowed(GasRatioInput {
                balance: AtomicAmount::new(U256::from(minimum_balance)),
                token_decimals: 18,
                batch_gas: gas,
                address_count: count,
                max_fee_per_gas: fee,
                native_price: price(),
                token_price: price(),
                maximum,
            })
            .expect("calculation should fit")
        );
        assert!(
            !gas_ratio_allowed(GasRatioInput {
                balance: AtomicAmount::new(U256::from(minimum_balance - 1)),
                token_decimals: 18,
                batch_gas: gas,
                address_count: count,
                max_fee_per_gas: fee,
                native_price: price(),
                token_price: price(),
                maximum,
            })
            .expect("calculation should fit")
        );
    }

    #[test]
    fn normalizes_token_decimals_against_native_wei() {
        let allowed = gas_ratio_allowed(GasRatioInput {
            balance: AtomicAmount::new(U256::from(1_000_000_u64)),
            token_decimals: 6,
            batch_gas: 21_000,
            address_count: 1,
            max_fee_per_gas: 1_000_000_000,
            native_price: price(),
            token_price: price(),
            maximum: Bps::new(500).expect("bps is valid"),
        })
        .expect("calculation should fit");
        assert!(allowed);
    }

    #[test]
    fn compares_native_and_token_usd_prices() {
        let pha = ScaledPrice::new(5_000_000, PRICE_SCALE).expect("PHA price is valid");
        let eth = ScaledPrice::new(300_000_000_000, PRICE_SCALE).expect("ETH price is valid");
        let allowed = gas_ratio_allowed(GasRatioInput {
            balance: AtomicAmount::new(
                U256::from(20_000_u64) * U256::from(10_u64).pow(U256::from(18)),
            ),
            token_decimals: 18,
            batch_gas: 20_000_000,
            address_count: 1,
            max_fee_per_gas: 1_000_000_000,
            native_price: eth,
            token_price: pha,
            maximum: Bps::new(200).expect("bps is valid"),
        })
        .expect("calculation should fit");
        assert!(!allowed);
    }
}

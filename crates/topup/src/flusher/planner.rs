use std::sync::Arc;

use alloy_primitives::{Address, B256, U256, U512};
use serde_json::to_value;
use sqlx::PgPool;
use topup_adapters::signer::actor::SignerHandle;
use topup_core::Signer;
use topup_core::money::{AtomicAmount, Bps, ScaledPrice};
use topup_core::route::RouteFile;
use uuid::Uuid;

use crate::db;

use super::types::{
    AlertSink, ChainClient, FlushAlert, FlushEvidence, PlannedAddress, PriceSource,
};
use super::{FlusherError, map_chain, map_price, map_signer};

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

    /// Evaluates a route and persists at most one planned batch.
    pub async fn plan(&self, route: &RouteFile) -> Result<Option<Uuid>, FlusherError> {
        if db::has_open_flush(&self.pool, route.chain.chain_id, route.asset.contract).await? {
            return Ok(None);
        }
        let addresses = db::list_chain_addresses(&self.pool, route.chain.chain_id).await?;
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

        let price = self
            .prices
            .latest_primary_price(&route.route)
            .await
            .map_err(map_price)?;
        let operator = self.signer.operator_address().await.map_err(map_signer)?;
        let fee = self.chain.fee_quote().await.map_err(map_chain)?;
        let mut selected = addresses
            .into_iter()
            .zip(balances)
            .filter(|(_, balance)| *balance >= route.asset.min_flush_atomic.value())
            .collect::<Vec<_>>();

        loop {
            if selected.is_empty() {
                return Ok(None);
            }
            let salts = selected
                .iter()
                .map(|(address, _)| address.salt)
                .collect::<Vec<_>>();
            let gas = self
                .chain
                .estimate_flush_gas(
                    route.chain.contracts.forwarder_factory,
                    operator,
                    &salts,
                    route.asset.contract,
                )
                .await
                .map_err(map_chain)?;
            let count = u64::try_from(selected.len())
                .map_err(|_| FlusherError::Invariant("batch address count exceeds u64"))?;
            let retained = selected
                .into_iter()
                .map(|item| {
                    gas_ratio_allowed(
                        AtomicAmount::new(item.1),
                        route.asset.decimals,
                        gas,
                        count,
                        fee.max_fee_per_gas,
                        price,
                        route.chain.flush.max_gas_ratio_bps,
                    )
                    .map(|allowed| allowed.then_some(item))
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            if retained.len() == count_to_usize(count)? {
                let plan = retained
                    .iter()
                    .map(|(address, balance)| PlannedAddress {
                        address_id: address.id,
                        salt: format!("{:#x}", address.salt),
                        address: format!("{:#x}", address.address),
                        balance_atomic: balance.to_string(),
                    })
                    .collect();
                let evidence = FlushEvidence::planned(plan, gas);
                return self.persist_plan(route, operator, evidence).await;
            }
            selected = retained;
        }
    }

    async fn persist_plan(
        &self,
        route: &RouteFile,
        operator: Address,
        evidence: FlushEvidence,
    ) -> Result<Option<Uuid>, FlusherError> {
        let pending = self
            .chain
            .pending_nonce(operator)
            .await
            .map_err(map_chain)?;
        let mut transaction = self.pool.begin().await?;
        db::lock_flush_plan(&mut transaction, route.chain.chain_id, route.asset.contract).await?;
        db::lock_operator(&mut transaction, route.chain.chain_id, operator).await?;
        if db::has_open_flush_locked(&mut transaction, route.chain.chain_id, route.asset.contract)
            .await?
        {
            transaction.commit().await?;
            return Ok(None);
        }
        let nonce =
            db::next_flush_nonce(&mut transaction, route.chain.chain_id, operator, pending).await?;
        let id = Uuid::new_v4();
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
        transaction.commit().await?;
        Ok(Some(id))
    }
}

/// Applies the planner's per-address gas-to-value policy.
pub fn gas_ratio_allowed(
    balance: AtomicAmount,
    token_decimals: u8,
    batch_gas: u64,
    address_count: u64,
    max_fee_per_gas: u128,
    primary_price: ScaledPrice,
    maximum: Bps,
) -> Result<bool, FlusherError> {
    if address_count == 0 {
        return Err(FlusherError::Invariant(
            "gas ratio requires at least one address",
        ));
    }
    let share = batch_gas
        .checked_add(address_count.saturating_sub(1))
        .and_then(|value| value.checked_div(address_count))
        .ok_or(FlusherError::Arithmetic)?;
    let gas_value = U512::from(share)
        .checked_mul(U512::from(max_fee_per_gas))
        .and_then(|value| value.checked_mul(U512::from(primary_price.value())))
        .and_then(|value| value.checked_mul(power_of_ten(token_decimals)))
        .and_then(|value| value.checked_mul(U512::from(10_000_u16)))
        .ok_or(FlusherError::Arithmetic)?;
    let token_value = U512::from(balance.value())
        .checked_mul(U512::from(primary_price.value()))
        .and_then(|value| value.checked_mul(power_of_ten(18)))
        .and_then(|value| value.checked_mul(U512::from(maximum.value())))
        .ok_or(FlusherError::Arithmetic)?;
    Ok(gas_value <= token_value)
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

    use super::gas_ratio_allowed;

    fn price() -> ScaledPrice {
        ScaledPrice::new(25_000_000, PRICE_SCALE).expect("test price is valid")
    }

    #[test]
    fn rejects_zero_address_count() {
        assert!(
            gas_ratio_allowed(
                AtomicAmount::new(U256::from(1_000_u64)),
                18,
                100_000,
                0,
                1,
                price(),
                Bps::new(200).expect("bps is valid"),
            )
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
            gas_ratio_allowed(
                AtomicAmount::new(U256::from(minimum_balance)),
                18,
                gas,
                count,
                fee,
                price(),
                maximum,
            )
            .expect("calculation should fit")
        );
        assert!(
            !gas_ratio_allowed(
                AtomicAmount::new(U256::from(minimum_balance - 1)),
                18,
                gas,
                count,
                fee,
                price(),
                maximum,
            )
            .expect("calculation should fit")
        );
    }

    #[test]
    fn normalizes_token_decimals_against_native_wei() {
        let allowed = gas_ratio_allowed(
            AtomicAmount::new(U256::from(1_000_000_u64)),
            6,
            21_000,
            1,
            1_000_000_000,
            price(),
            Bps::new(500).expect("bps is valid"),
        )
        .expect("calculation should fit");
        assert!(allowed);
    }
}

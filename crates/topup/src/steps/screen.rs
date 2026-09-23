//! Confirmed-to-cleared screening step.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;

use alloy_primitives::Address;
use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use sqlx::PgPool;
use topup_adapters::risk::oracle::{SanctionsOracle, SanctionsOracleConfigError, SanctionsSource};
use topup_core::deposit::{DepositState, RejectReason, RetryError, StepOutcome};
use topup_core::screening::{Bounds, PauseScopes, SanctionsResult, screen};
use uuid::Uuid;

use crate::db::{Deposit, OutboxEvent};
use crate::pause::{self, PauseScopeSources};
use crate::pump::{Step, StepResult};
use crate::routes::{ProviderError, RouteSet};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RouteKey {
    name: String,
    version: u64,
}

/// Screening policy and sanctions source for one immutable route version.
#[derive(Clone)]
pub struct ScreenRoute {
    key: RouteKey,
    oracle: Address,
    bounds: Bounds,
    sanctions: Arc<dyn SanctionsSource>,
}

impl ScreenRoute {
    /// Creates an injectable route configuration, including a mockable sanctions source.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        version: u64,
        oracle: Address,
        bounds: Bounds,
        sanctions: Arc<dyn SanctionsSource>,
    ) -> Self {
        Self {
            key: RouteKey {
                name: name.into(),
                version,
            },
            oracle,
            bounds,
            sanctions,
        }
    }

    async fn evaluate(
        &self,
        deposit: &Deposit,
        product_id: Uuid,
        pause_scopes: PauseScopeSources,
    ) -> StepResult {
        let sanctions = self
            .sanctions
            .sanctions(deposit.from_address, deposit.block_number)
            .await;
        if sanctions.block_number != deposit.block_number {
            return invariant_result("sanctions_block_mismatch", deposit.block_number);
        }
        let outcome = screen(
            deposit.amount_atomic,
            &sanctions,
            &self.bounds,
            &pause_scopes.effective,
            &PauseScopes::default(),
        );
        let evidence = screening_evidence(self.oracle, sanctions, self.bounds, &pause_scopes);
        let mut result = StepResult::new(outcome, evidence);
        if let StepOutcome::Reject(reason) = outcome {
            result
                .events
                .push(rejected_event(deposit, product_id, reason));
        }
        result
    }
}

/// Failure while constructing the route-to-screening registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScreenStepConfigError {
    /// One of the route's first two RPC providers is unusable.
    Provider {
        /// Stable route name.
        route: String,
        /// Immutable route version.
        version: u64,
        /// Non-secret provider failure.
        source: ProviderError,
    },
    /// A route's sanctions-oracle client could not be configured.
    InvalidOracle {
        /// Stable route name.
        route: String,
        /// Immutable route version.
        version: u64,
        /// Non-secret configuration failure.
        source: SanctionsOracleConfigError,
    },
    /// Two supplied route files used the same name and version.
    DuplicateRoute {
        /// Stable route name.
        route: String,
        /// Immutable route version.
        version: u64,
    },
}

impl Display for ScreenStepConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider {
                route,
                version,
                source,
            } => write!(formatter, "route `{route}` version {version}: {source}"),
            Self::InvalidOracle {
                route,
                version,
                source,
            } => write!(
                formatter,
                "route `{route}` version {version} has invalid sanctions configuration: {source}"
            ),
            Self::DuplicateRoute { route, version } => {
                write!(formatter, "duplicate route `{route}` version {version}")
            }
        }
    }
}

impl Error for ScreenStepConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Provider { source, .. } => Some(source),
            Self::InvalidOracle { source, .. } => Some(source),
            Self::DuplicateRoute { .. } => None,
        }
    }
}

/// Real screening step backed by PostgreSQL pause scopes and route-specific oracle clients.
pub struct ScreenStep {
    pool: PgPool,
    routes: BTreeMap<RouteKey, ScreenRoute>,
}

impl ScreenStep {
    /// Creates a step from already composed route policies and sanctions sources.
    pub fn new(
        pool: PgPool,
        routes: impl IntoIterator<Item = ScreenRoute>,
    ) -> Result<Self, ScreenStepConfigError> {
        let mut by_key = BTreeMap::new();
        for route in routes {
            let key = route.key.clone();
            if by_key.insert(key.clone(), route).is_some() {
                return Err(ScreenStepConfigError::DuplicateRoute {
                    route: key.name,
                    version: key.version,
                });
            }
        }
        Ok(Self {
            pool,
            routes: by_key,
        })
    }

    /// Creates route-specific sanctions checks on each route chain's first two providers.
    pub fn from_routes(pool: PgPool, routes: &RouteSet) -> Result<Self, ScreenStepConfigError> {
        let mut screening_routes = Vec::with_capacity(routes.routes().len());
        for route in routes.routes() {
            let provider = |index| {
                routes
                    .provider(route.chain.chain_id, index)
                    .map(Arc::clone)
                    .map_err(|source| ScreenStepConfigError::Provider {
                        route: route.route.clone(),
                        version: route.version,
                        source,
                    })
            };
            let oracle =
                SanctionsOracle::new(provider(0)?, provider(1)?, route.screening.sanctions_oracle)
                    .map_err(|source| ScreenStepConfigError::InvalidOracle {
                        route: route.route.clone(),
                        version: route.version,
                        source,
                    })?;
            screening_routes.push(ScreenRoute::new(
                route.route.clone(),
                route.version,
                route.screening.sanctions_oracle,
                Bounds::from(&route.screening),
                Arc::new(oracle),
            ));
        }
        Self::new(pool, screening_routes)
    }

    async fn pause_scopes(
        &self,
        account_id: Uuid,
        route: &str,
    ) -> Result<Option<(Uuid, PauseScopeSources)>, sqlx::Error> {
        pause::account_pause_scopes(&self.pool, account_id, route).await
    }
}

#[async_trait]
impl Step for ScreenStep {
    async fn run(&self, deposit: &Deposit) -> StepResult {
        if deposit.state != DepositState::Confirmed {
            return invariant_result("screen_step_requires_confirmed", deposit.block_number);
        }
        let (Some(route), Some(version)) = (&deposit.route, deposit.route_version) else {
            return invariant_result("missing_deposit_route", deposit.block_number);
        };
        let key = RouteKey {
            name: route.clone(),
            version,
        };
        let Some(screening_route) = self.routes.get(&key) else {
            return invariant_result("unknown_deposit_route", deposit.block_number);
        };
        let pauses = match self.pause_scopes(deposit.account_id, route).await {
            Ok(Some(pauses)) => pauses,
            Ok(None) => return invariant_result("account_not_found", deposit.block_number),
            Err(_) => return transient_result("pause_scope_load_failed", deposit.block_number),
        };
        let (product_id, pause_scopes) = pauses;
        screening_route
            .evaluate(deposit, product_id, pause_scopes)
            .await
    }
}

fn screening_evidence(
    oracle: Address,
    sanctions: SanctionsResult,
    bounds: Bounds,
    pause_scopes: &PauseScopeSources,
) -> serde_json::Value {
    json!({
        "oracle": format!("{oracle:#x}"),
        "block_number": sanctions.block_number,
        "provider_a": sanctions.provider_a,
        "provider_b": sanctions.provider_b,
        "bounds": {
            "min_atomic": bounds.min_atomic,
            "max_atomic": bounds.max_atomic,
        },
        "pause_scopes": {
            "account": pause_scopes.account,
            "product": pause_scopes.product,
            "route": pause_scopes.route,
        },
    })
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

fn invariant_result(error: &'static str, block_number: u64) -> StepResult {
    StepResult::new(
        StepOutcome::Retry {
            error: RetryError::InvariantViolation,
        },
        json!({
            "block_number": block_number,
            "error": error,
        }),
    )
}

fn transient_result(error: &'static str, block_number: u64) -> StepResult {
    StepResult::new(
        StepOutcome::Retry {
            error: RetryError::Transient,
        },
        json!({
            "block_number": block_number,
            "error": error,
        }),
    )
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{B256, U256};
    use chrono::Utc;
    use topup_core::deposit::{StepOutcome, WaitReason};
    use topup_core::money::AtomicAmount;
    use topup_core::screening::SanctionsAnswer;

    use super::*;

    struct FixedSanctions(SanctionsResult);

    #[async_trait]
    impl SanctionsSource for FixedSanctions {
        async fn sanctions(&self, _address: Address, _block_number: u64) -> SanctionsResult {
            self.0
        }
    }

    fn amount(value: u64) -> AtomicAmount {
        AtomicAmount::new(U256::from(value))
    }

    fn deposit(amount_atomic: AtomicAmount) -> Deposit {
        let now = Utc::now();
        Deposit {
            id: Uuid::new_v4(),
            chain_id: 1,
            tx_hash: B256::ZERO,
            log_index: 0,
            block_number: 123,
            block_hash: B256::ZERO,
            block_time: now,
            address_id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            route: Some("route".to_owned()),
            route_version: Some(1),
            asset_contract: Address::ZERO,
            from_address: Address::repeat_byte(7),
            amount_atomic,
            state: DepositState::Confirmed,
            reason: None,
            attempt: 0,
            next_attempt_at: now,
            lease_token: None,
            lease_until: None,
            valuation_at: Some(now),
            price_scaled: Some(1),
            price_source: Some("spot".to_owned()),
            credit_minor: None,
            quote: None,
            flush_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn route(provider_a: SanctionsAnswer, provider_b: SanctionsAnswer) -> ScreenRoute {
        ScreenRoute::new(
            "route",
            1,
            Address::repeat_byte(9),
            Bounds {
                min_atomic: amount(10),
                max_atomic: amount(20),
            },
            Arc::new(FixedSanctions(SanctionsResult {
                provider_a,
                provider_b,
                block_number: 123,
            })),
        )
    }

    fn pauses(account: &[&str], product: &[&str], route: &[&str]) -> PauseScopeSources {
        let account = account.iter().map(ToString::to_string).collect::<Vec<_>>();
        let product = product.iter().map(ToString::to_string).collect::<Vec<_>>();
        let route = route.iter().map(ToString::to_string).collect::<Vec<_>>();
        PauseScopeSources::from_codes(&account, &product, &route).expect("valid pause scopes")
    }

    #[tokio::test]
    async fn sanctions_truth_table_maps_to_step_results() {
        use SanctionsAnswer::{Clear, Sanctioned, Unavailable};

        let cases = [
            (
                Sanctioned,
                Sanctioned,
                StepOutcome::Reject(RejectReason::Sanctioned),
            ),
            (
                Sanctioned,
                Clear,
                StepOutcome::Reject(RejectReason::Sanctioned),
            ),
            (
                Sanctioned,
                Unavailable,
                StepOutcome::Reject(RejectReason::Sanctioned),
            ),
            (
                Clear,
                Sanctioned,
                StepOutcome::Reject(RejectReason::Sanctioned),
            ),
            (Clear, Clear, StepOutcome::Advance),
            (
                Clear,
                Unavailable,
                StepOutcome::Retry {
                    error: RetryError::SanctionsInconclusive,
                },
            ),
            (
                Unavailable,
                Sanctioned,
                StepOutcome::Reject(RejectReason::Sanctioned),
            ),
            (
                Unavailable,
                Clear,
                StepOutcome::Retry {
                    error: RetryError::SanctionsInconclusive,
                },
            ),
            (
                Unavailable,
                Unavailable,
                StepOutcome::Retry {
                    error: RetryError::SanctionsInconclusive,
                },
            ),
        ];

        for (provider_a, provider_b, expected) in cases {
            let deposit = deposit(amount(15));
            let product_id = Uuid::new_v4();
            let result = route(provider_a, provider_b)
                .evaluate(&deposit, product_id, pauses(&[], &[], &[]))
                .await;
            assert_eq!(result.outcome, expected);
            assert_eq!(result.evidence["block_number"], 123);
            if matches!(expected, StepOutcome::Reject(_)) {
                assert_eq!(result.events.len(), 1);
                assert_eq!(result.events[0].event_type, "deposit.rejected");
                assert_eq!(
                    result.events[0].payload["product_id"],
                    product_id.to_string()
                );
                assert_eq!(result.events[0].payload["reason"], "sanctioned");
                assert_eq!(result.events[0].payload["state"], "rejected");
                assert_eq!(result.events[0].payload["route"], "route");
            } else {
                assert!(result.events.is_empty());
            }
        }
    }

    #[tokio::test]
    async fn bounds_and_pause_outcomes_preserve_evidence_and_event_rules() {
        let product_id = Uuid::new_v4();
        let out_of_bounds = route(SanctionsAnswer::Clear, SanctionsAnswer::Clear)
            .evaluate(&deposit(amount(21)), product_id, pauses(&[], &[], &[]))
            .await;
        assert_eq!(
            out_of_bounds.outcome,
            StepOutcome::Reject(RejectReason::OutOfBounds)
        );
        assert_eq!(out_of_bounds.events[0].payload["reason"], "out_of_bounds");
        assert_eq!(out_of_bounds.evidence["bounds"]["min_atomic"], "10");
        assert_eq!(out_of_bounds.evidence["bounds"]["max_atomic"], "20");

        let waiting = route(SanctionsAnswer::Clear, SanctionsAnswer::Clear)
            .evaluate(
                &deposit(amount(15)),
                product_id,
                pauses(&["settlement"], &[], &[]),
            )
            .await;
        assert_eq!(
            waiting.outcome,
            StepOutcome::Wait {
                reason: WaitReason::Paused,
            }
        );
        assert!(waiting.events.is_empty());
        assert_eq!(waiting.evidence["pause_scopes"]["account"][0], "settlement");

        let route_paused = route(SanctionsAnswer::Clear, SanctionsAnswer::Clear)
            .evaluate(
                &deposit(amount(15)),
                product_id,
                pauses(&[], &[], &["settlement"]),
            )
            .await;
        assert_eq!(
            route_paused.outcome,
            StepOutcome::Wait {
                reason: WaitReason::Paused,
            }
        );
        assert_eq!(
            route_paused.evidence["pause_scopes"]["route"][0],
            "settlement"
        );

        let non_settlement_route_pause = route(SanctionsAnswer::Clear, SanctionsAnswer::Clear)
            .evaluate(
                &deposit(amount(15)),
                product_id,
                pauses(&[], &[], &["flush"]),
            )
            .await;
        assert_eq!(non_settlement_route_pause.outcome, StepOutcome::Advance);
    }

    #[tokio::test]
    async fn source_cannot_substitute_a_different_evidence_block() {
        let mut route = route(SanctionsAnswer::Clear, SanctionsAnswer::Clear);
        route.sanctions = Arc::new(FixedSanctions(SanctionsResult {
            provider_a: SanctionsAnswer::Clear,
            provider_b: SanctionsAnswer::Clear,
            block_number: 124,
        }));
        let result = route
            .evaluate(&deposit(amount(15)), Uuid::new_v4(), pauses(&[], &[], &[]))
            .await;
        assert_eq!(
            result.outcome,
            StepOutcome::Retry {
                error: RetryError::InvariantViolation,
            }
        );
        assert_eq!(result.evidence["error"], "sanctions_block_mismatch");
    }
}

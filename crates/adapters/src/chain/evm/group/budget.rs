//! Atomic two-level admission using governor's GCRA and transactional in-memory state.
use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use governor::{
    Quota, RateLimiter,
    clock::{Clock, DefaultClock},
    nanos::Nanos,
    state::{NotKeyed, StateStore},
};
use serde::{Deserialize, Serialize};
use tokio::time::{Instant, sleep_until};

/// One shared rate/burst limit, independent of RPC method.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BudgetSpec {
    /// Sustained sends per second.
    pub requests_per_second: u32,
    /// Maximum burst.
    pub burst: u32,
}

#[derive(Clone, Default)]
struct State(Arc<Mutex<Option<Nanos>>>);
impl StateStore for State {
    type Key = NotKeyed;
    fn measure_and_replace<T, F, E>(&self, _: &NotKeyed, f: F) -> Result<T, E>
    where
        F: Fn(Option<Nanos>) -> Result<(T, Nanos), E>,
    {
        let mut state = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let (result, next) = f(*state)?;
        *state = Some(next);
        Ok(result)
    }
}
struct Budget {
    limiter: RateLimiter<NotKeyed, State, DefaultClock>,
    state: State,
    paused: Instant,
    rate: u32,
}

/// Process-wide registry; no limiter can be mutated outside its admission transaction.
pub struct Budgets(Mutex<BTreeMap<String, Budget>>);
impl Budgets {
    /// Builds validated account and credential scopes.
    pub fn new(specs: &BTreeMap<String, BudgetSpec>) -> Result<Self, &'static str> {
        let mut budgets = BTreeMap::new();
        for (id, spec) in specs {
            let rate =
                NonZeroU32::new(spec.requests_per_second).ok_or("RPC rate must be positive")?;
            let burst = NonZeroU32::new(spec.burst).ok_or("RPC burst must be positive")?;
            let state = State::default();
            budgets.insert(
                id.clone(),
                Budget {
                    limiter: RateLimiter::new(
                        Quota::per_second(rate).allow_burst(burst),
                        state.clone(),
                        DefaultClock::default(),
                    ),
                    state,
                    paused: Instant::now(),
                    rate: spec.requests_per_second,
                },
            );
        }
        Ok(Self(Mutex::new(budgets)))
    }
    /// Admits both scopes in one transaction immediately before dispatch. On a denied scope,
    /// rolls back every tentative GCRA update, releases the lock, waits and rechecks both.
    pub async fn admit(
        &self,
        account: &str,
        key: &str,
        deadline: Instant,
    ) -> Result<(), &'static str> {
        loop {
            let wake = {
                let mut budgets = self.0.lock().unwrap_or_else(PoisonError::into_inner);
                let ids = if account == key {
                    vec![account]
                } else {
                    vec![account, key]
                };
                let mut snapshots = Vec::new();
                let mut wake = Instant::now();
                for id in &ids {
                    let b = budgets.get(*id).ok_or("unknown RPC budget")?;
                    snapshots.push((
                        *id,
                        *b.state.0.lock().unwrap_or_else(PoisonError::into_inner),
                    ));
                    wake = wake.max(b.paused);
                }
                if wake <= Instant::now() {
                    let mut denied = false;
                    for id in &ids {
                        let b = budgets.get(*id).ok_or("unknown RPC budget")?;
                        if let Err(until) = b.limiter.check() {
                            denied = true;
                            wake = wake.max(
                                Instant::now()
                                    .checked_add(until.wait_time_from(b.limiter.clock().now()))
                                    .unwrap_or(deadline),
                            );
                        }
                    }
                    if !denied {
                        return Ok(());
                    }
                    for (id, previous) in snapshots {
                        if let Some(b) = budgets.get_mut(id) {
                            *b.state.0.lock().unwrap_or_else(PoisonError::into_inner) = previous;
                        }
                    }
                }
                wake
            };
            if wake >= deadline || Instant::now() >= deadline {
                return Err("RPC admission deadline");
            }
            sleep_until(wake).await;
        }
    }
    /// Conservative admission time for a bounded verification plan, across both scopes.
    pub fn planned_time(
        &self,
        account: &str,
        key: &str,
        sends: u64,
    ) -> Result<Duration, &'static str> {
        let budgets = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let rate = [account, key]
            .iter()
            .map(|id| budgets.get(*id).map(|b| b.rate).ok_or("unknown RPC budget"))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .min()
            .ok_or("missing RPC rate")?;
        Ok(Duration::from_millis(
            sends.saturating_mul(2000).div_ceil(u64::from(rate)),
        ))
    }
    /// Selection skips explicit quota pauses; ordinary rate admission still waits fairly.
    pub fn paused(&self, account: &str, key: &str) -> bool {
        let budgets = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        [account, key]
            .iter()
            .any(|id| budgets.get(*id).is_none_or(|b| b.paused > Instant::now()))
    }
    /// Conservatively pauses an account on an unknown 429; classified key limits may narrow it.
    pub fn pause(&self, id: &str, duration: Duration) {
        if let Some(b) = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_mut(id)
        {
            b.paused = b.paused.max(
                Instant::now()
                    .checked_add(duration)
                    .unwrap_or_else(Instant::now),
            );
        }
    }
}

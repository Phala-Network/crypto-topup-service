//! PostgreSQL integration tests for concurrent deposit pumps.

use std::env;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool, Row};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use topup::db::{
    self, AddressKind, NewAccount, NewAddress, NewDeposit, NewProduct, SettlementIntent,
};
use topup::pump::{
    AgeAlertConfig, AgeAlerter, JitterSource, Pump, PumpConfig, PumpMetrics, RunOnceResult, Step,
    StepSet,
};
use topup_core::deposit::{DepositState, RetryError, StepOutcome, WaitReason};
use topup_core::identity::deposit_id;
use topup_core::money::AtomicAmount;
use topup_core::route::RouteFile;
use url::Url;
use uuid::Uuid;

type TestFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

struct TestContext {
    admin_pool: PgPool,
    owner_pool: PgPool,
    app_pool: PgPool,
    database_name: String,
    app_role: String,
}

impl TestContext {
    async fn create() -> Result<Option<Self>> {
        let Some(owner_template) = required_url("MIGRATE_DATABASE_URL") else {
            return Ok(None);
        };
        let Some(app_template) = required_url("DATABASE_URL") else {
            return Ok(None);
        };

        let mut admin_url =
            Url::parse(&owner_template).context("MIGRATE_DATABASE_URL must be a PostgreSQL URL")?;
        admin_url.set_path("/postgres");
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(admin_url.as_str())
            .await
            .context("connect to the PostgreSQL maintenance database")?;
        sqlx::query("SELECT pg_advisory_lock(704_202_001)")
            .execute(&admin_pool)
            .await?;

        let suffix = Uuid::new_v4().simple().to_string();
        let database_name = format!("topup_c2_{suffix}");
        let app_role = format!("topup_c2_app_{suffix}");
        let password = format!("c2_{suffix}");
        admin_pool
            .execute(format!("CREATE DATABASE \"{database_name}\"").as_str())
            .await
            .context("create isolated test database")?;

        let mut owner_url = Url::parse(&owner_template)?;
        owner_url.set_path(&format!("/{database_name}"));
        let owner_pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(owner_url.as_str())
            .await
            .context("connect to isolated test database as owner")?;
        db::migrate(&owner_pool).await.context("apply migrations")?;

        admin_pool
            .execute(
                format!("CREATE ROLE \"{app_role}\" LOGIN PASSWORD '{password}' IN ROLE topup_app")
                    .as_str(),
            )
            .await
            .context("create isolated application login role")?;

        let mut app_url = Url::parse(&app_template).context("DATABASE_URL must be a URL")?;
        app_url
            .set_username(&app_role)
            .map_err(|()| anyhow::anyhow!("DATABASE_URL cannot accept an application username"))?;
        app_url
            .set_password(Some(&password))
            .map_err(|()| anyhow::anyhow!("DATABASE_URL cannot accept an application password"))?;
        app_url.set_path(&format!("/{database_name}"));
        let app_pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(app_url.as_str())
            .await
            .context("connect to isolated test database as application role")?;

        sqlx::query("SELECT pg_advisory_unlock(704_202_001)")
            .execute(&admin_pool)
            .await?;

        Ok(Some(Self {
            admin_pool,
            owner_pool,
            app_pool,
            database_name,
            app_role,
        }))
    }

    async fn cleanup(self) -> Result<()> {
        self.app_pool.close().await;
        self.owner_pool.close().await;
        self.admin_pool
            .execute(format!("DROP DATABASE \"{}\" WITH (FORCE)", self.database_name).as_str())
            .await
            .context("drop isolated test database")?;
        self.admin_pool
            .execute(format!("DROP ROLE \"{}\"", self.app_role).as_str())
            .await
            .context("drop isolated application login role")?;
        self.admin_pool.close().await;
        Ok(())
    }
}

fn required_url(name: &str) -> Option<String> {
    match env::var(name).ok().filter(|value| !value.is_empty()) {
        Some(value) => Some(value),
        None => {
            eprintln!("skipping pump integration test: {name} is not set");
            None
        }
    }
}

async fn with_database<F>(test: F) -> Result<()>
where
    F: for<'a> FnOnce(&'a TestContext) -> TestFuture<'a>,
{
    let Some(context) = TestContext::create().await? else {
        return Ok(());
    };
    let result = test(&context).await;
    let cleanup = context.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn two_pumps_racing_on_one_deposit_apply_exactly_one_transition() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let seed = seed_account(&context.app_pool, 1).await?;
            let id = insert_deposit(&context.app_pool, seed, 1).await?;
            let control = Arc::new(StepControl::default());
            let steps = blocking_steps(Arc::clone(&control), StepOutcome::Advance);
            let pump = test_pump(&context.app_pool, steps, PumpConfig::default(), 0)?;

            let first_pump = pump.clone();
            let first = tokio::spawn(async move { first_pump.run_once().await });
            control.wait_started().await?;
            let second = pump.run_once().await?;
            ensure!(second == RunOnceResult::Idle);
            control.release();
            ensure!(
                first.await.context("first pump task")??
                    == RunOnceResult::Applied { deposit_id: id }
            );
            ensure!(transition_count(&context.app_pool, id).await? == 1);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn late_step_result_is_stale_after_the_lease_is_reclaimed() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let seed = seed_account(&context.app_pool, 2).await?;
            let id = insert_deposit(&context.app_pool, seed, 2).await?;
            let control = Arc::new(StepControl::default());
            let slow = test_pump(
                &context.app_pool,
                blocking_steps(Arc::clone(&control), StepOutcome::Advance),
                PumpConfig::default(),
                0,
            )?;
            let slow_task = tokio::spawn(async move { slow.run_once().await });
            control.wait_started().await?;

            sqlx::query(
                "UPDATE deposits SET lease_until = now() - interval '1 second' WHERE id = $1",
            )
            .bind(id)
            .execute(&context.app_pool)
            .await?;
            let fast = test_pump(
                &context.app_pool,
                static_steps(StepOutcome::Advance),
                PumpConfig::default(),
                0,
            )?;
            ensure!(fast.run_once().await? == RunOnceResult::Applied { deposit_id: id });
            control.release();
            ensure!(
                slow_task.await.context("slow pump task")??
                    == RunOnceResult::Stale { deposit_id: id }
            );
            ensure!(transition_count(&context.app_pool, id).await? == 1);
            let stored = db::get_deposit(&context.app_pool, id)
                .await?
                .context("deposit must exist")?;
            ensure!(stored.state == DepositState::Confirmed);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn intent_survives_a_step_panic_and_the_deposit_is_reclaimed() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let seed = seed_account(&context.app_pool, 3).await?;
            let id = insert_deposit(&context.app_pool, seed, 3).await?;
            let panic_step = IntentThenPanicStep {
                pool: context.app_pool.clone(),
                product_id: seed.product_id,
            };
            let steps = StepSet::new(
                Box::new(panic_step),
                Box::new(StaticStep(StepOutcome::Advance)),
                Box::new(StaticStep(StepOutcome::Advance)),
                Box::new(StaticStep(StepOutcome::Advance)),
            );
            let first = test_pump(&context.app_pool, steps, PumpConfig::default(), u64::MAX)?;
            ensure!(first.run_once().await? == RunOnceResult::Applied { deposit_id: id });
            let stored = db::get_deposit(&context.app_pool, id)
                .await?
                .context("deposit must exist")?;
            ensure!(stored.state == DepositState::Detected && stored.attempt == 1);
            let intents: i64 =
                sqlx::query("SELECT count(*) FROM settlements WHERE deposit_id = $1")
                    .bind(id)
                    .fetch_one(&context.app_pool)
                    .await?
                    .try_get(0)?;
            ensure!(intents == 1);

            let second = test_pump(
                &context.app_pool,
                static_steps(StepOutcome::Wait {
                    reason: WaitReason::Paused,
                }),
                PumpConfig {
                    wait_interval: StdDuration::from_secs(5),
                    ..PumpConfig::default()
                },
                0,
            )?;
            ensure!(second.run_once().await? == RunOnceResult::Applied { deposit_id: id });
            ensure!(transition_count(&context.app_pool, id).await? == 2);
            ensure!(
                db::get_deposit(&context.app_pool, id)
                    .await?
                    .context("deposit must exist")?
                    .attempt
                    == 1
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn step_timeout_is_persisted_as_a_retry() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let seed = seed_account(&context.app_pool, 7).await?;
            let id = insert_deposit(&context.app_pool, seed, 10).await?;
            let steps = StepSet::new(
                Box::new(SlowStep),
                Box::new(StaticStep(StepOutcome::Advance)),
                Box::new(StaticStep(StepOutcome::Advance)),
                Box::new(StaticStep(StepOutcome::Advance)),
            );
            let pump = test_pump(
                &context.app_pool,
                steps,
                PumpConfig {
                    step_timeout: StdDuration::from_millis(10),
                    ..PumpConfig::default()
                },
                u64::MAX,
            )?;
            ensure!(pump.run_once().await? == RunOnceResult::Applied { deposit_id: id });
            let stored = db::get_deposit(&context.app_pool, id)
                .await?
                .context("deposit must exist")?;
            ensure!(stored.state == DepositState::Detected && stored.attempt == 1);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn retry_wait_and_advance_maintain_attempt_and_schedule_contracts() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let seed = seed_account(&context.app_pool, 4).await?;

            let retry_id = insert_deposit(&context.app_pool, seed, 4).await?;
            sqlx::query("UPDATE deposits SET attempt = 2 WHERE id = $1")
                .bind(retry_id)
                .execute(&context.app_pool)
                .await?;
            let before_retry = Utc::now();
            let retry_pump = test_pump(
                &context.app_pool,
                static_steps(StepOutcome::Retry {
                    error: RetryError::Transient,
                }),
                PumpConfig::default(),
                0,
            )?;
            retry_pump.run_once().await?;
            let retry = db::get_deposit(&context.app_pool, retry_id)
                .await?
                .context("retry deposit")?;
            ensure!(retry.attempt == 3 && retry.state == DepositState::Detected);
            ensure!(retry.next_attempt_at >= before_retry + Duration::seconds(120));
            ensure!(retry.next_attempt_at <= Utc::now() + Duration::seconds(121));

            let wait_id = insert_deposit(&context.app_pool, seed, 5).await?;
            sqlx::query("UPDATE deposits SET attempt = 2 WHERE id = $1")
                .bind(wait_id)
                .execute(&context.app_pool)
                .await?;
            let before_wait = Utc::now();
            let wait_pump = test_pump(
                &context.app_pool,
                static_steps(StepOutcome::Wait {
                    reason: WaitReason::Paused,
                }),
                PumpConfig {
                    wait_interval: StdDuration::from_secs(5),
                    ..PumpConfig::default()
                },
                0,
            )?;
            wait_pump.run_once().await?;
            let wait = db::get_deposit(&context.app_pool, wait_id)
                .await?
                .context("wait deposit")?;
            ensure!(wait.attempt == 2 && wait.state == DepositState::Detected);
            ensure!(wait.next_attempt_at >= before_wait + Duration::seconds(5));
            ensure!(wait.next_attempt_at <= Utc::now() + Duration::seconds(6));

            let advance_id = insert_deposit(&context.app_pool, seed, 6).await?;
            sqlx::query("UPDATE deposits SET attempt = 2 WHERE id = $1")
                .bind(advance_id)
                .execute(&context.app_pool)
                .await?;
            let advance_pump = test_pump(
                &context.app_pool,
                static_steps(StepOutcome::Advance),
                PumpConfig::default(),
                0,
            )?;
            advance_pump.run_once().await?;
            let advance = db::get_deposit(&context.app_pool, advance_id)
                .await?
                .context("advance deposit")?;
            ensure!(advance.attempt == 0 && advance.state == DepositState::Confirmed);
            ensure!(advance.next_attempt_at <= Utc::now());
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn graceful_shutdown_finishes_in_flight_work_and_claims_nothing_else() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let seed = seed_account(&context.app_pool, 5).await?;
            let first_id = insert_deposit(&context.app_pool, seed, 7).await?;
            let second_id = insert_deposit(&context.app_pool, seed, 8).await?;
            let control = Arc::new(StepControl::default());
            let pump = test_pump(
                &context.app_pool,
                blocking_steps(
                    Arc::clone(&control),
                    StepOutcome::Wait {
                        reason: WaitReason::Paused,
                    },
                ),
                PumpConfig {
                    wait_interval: StdDuration::from_secs(5),
                    idle_poll_interval: StdDuration::from_millis(10),
                    ..PumpConfig::default()
                },
                0,
            )?;
            let cancellation = CancellationToken::new();
            let worker_cancellation = cancellation.clone();
            let worker = tokio::spawn(async move {
                pump.run(worker_cancellation).await;
            });
            control.wait_started().await?;
            cancellation.cancel();
            control.release();
            tokio::time::timeout(StdDuration::from_secs(2), worker)
                .await
                .context("pump did not stop")?
                .context("pump task failed")?;

            ensure!(total_transition_count(&context.app_pool).await? == 1);
            let claimed = control.claimed_ids();
            ensure!(claimed.len() == 1);
            let claimed_id = claimed[0];
            ensure!(claimed_id == first_id || claimed_id == second_id);
            let untouched_id = if claimed_id == first_id {
                second_id
            } else {
                first_id
            };
            let untouched = db::get_deposit(&context.app_pool, untouched_id)
                .await?
                .context("untouched deposit")?;
            ensure!(untouched.lease_token.is_none());
            ensure!(transition_count(&context.app_pool, untouched_id).await? == 0);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn age_alert_uses_route_threshold_and_increments_the_metric() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let seed = seed_account(&context.app_pool, 6).await?;
            let id = insert_deposit(&context.app_pool, seed, 9).await?;
            sqlx::query(
                "UPDATE deposits SET created_at = now() - interval '2 hours' WHERE id = $1",
            )
            .bind(id)
            .execute(&context.app_pool)
            .await?;
            let yaml = include_str!("fixtures/phala-cloud-pha.yaml");
            let mut route: RouteFile = serde_saphyr::from_str(yaml)?;
            route.alerts.stuck_after_s.detected = 1;
            let config = AgeAlertConfig::from_routes(&[route])?;
            let metrics = Arc::new(PumpMetrics::default());
            let alerter = AgeAlerter::new(
                context.app_pool.clone(),
                config,
                Arc::clone(&metrics),
                StdDuration::from_secs(60),
            );
            ensure!(alerter.scan_once().await? == 1);
            ensure!(metrics.stuck_deposit_alerts() == 1);
            Ok(())
        })
    })
    .await
}

#[derive(Clone, Copy)]
struct StaticStep(StepOutcome);

#[async_trait]
impl Step for StaticStep {
    async fn run(&self, _deposit: &db::Deposit) -> StepOutcome {
        self.0
    }
}

struct SlowStep;

#[async_trait]
impl Step for SlowStep {
    async fn run(&self, _deposit: &db::Deposit) -> StepOutcome {
        tokio::time::sleep(StdDuration::from_secs(5)).await;
        StepOutcome::Advance
    }
}

struct StepControl {
    started: Semaphore,
    release: Semaphore,
    claimed: Mutex<Vec<Uuid>>,
}

impl Default for StepControl {
    fn default() -> Self {
        Self {
            started: Semaphore::new(0),
            release: Semaphore::new(0),
            claimed: Mutex::new(Vec::new()),
        }
    }
}

impl StepControl {
    async fn wait_started(&self) -> Result<()> {
        self.started
            .acquire()
            .await
            .context("started semaphore closed")?
            .forget();
        Ok(())
    }

    fn release(&self) {
        self.release.add_permits(1);
    }

    fn claimed_ids(&self) -> Vec<Uuid> {
        self.claimed.lock().expect("claimed lock poisoned").clone()
    }
}

struct BlockingStep {
    control: Arc<StepControl>,
    outcome: StepOutcome,
}

#[async_trait]
impl Step for BlockingStep {
    async fn run(&self, deposit: &db::Deposit) -> StepOutcome {
        self.control
            .claimed
            .lock()
            .expect("claimed lock poisoned")
            .push(deposit.id);
        self.control.started.add_permits(1);
        if let Ok(permit) = self.control.release.acquire().await {
            permit.forget();
        }
        self.outcome
    }
}

struct IntentThenPanicStep {
    pool: PgPool,
    product_id: Uuid,
}

#[async_trait]
impl Step for IntentThenPanicStep {
    async fn run(&self, deposit: &db::Deposit) -> StepOutcome {
        let result = db::upsert_intent(
            &self.pool,
            &SettlementIntent {
                deposit_id: deposit.id,
                product_id: self.product_id,
                key: format!("deposit:{}", deposit.id),
                payload: json!({"deposit_id": deposit.id}),
            },
        )
        .await;
        if let Err(error) = result {
            panic!("failed to write settlement intent: {error}");
        }
        panic!("simulated crash after settlement intent");
    }
}

struct FixedJitter(u64);

impl JitterSource for FixedJitter {
    fn next_u64(&self) -> u64 {
        self.0
    }
}

fn static_steps(outcome: StepOutcome) -> StepSet {
    StepSet::new(
        Box::new(StaticStep(outcome)),
        Box::new(StaticStep(outcome)),
        Box::new(StaticStep(outcome)),
        Box::new(StaticStep(outcome)),
    )
}

fn blocking_steps(control: Arc<StepControl>, outcome: StepOutcome) -> StepSet {
    StepSet::new(
        Box::new(BlockingStep { control, outcome }),
        Box::new(StaticStep(outcome)),
        Box::new(StaticStep(outcome)),
        Box::new(StaticStep(outcome)),
    )
}

fn test_pump(pool: &PgPool, steps: StepSet, config: PumpConfig, jitter: u64) -> Result<Pump> {
    Pump::with_jitter(
        pool.clone(),
        Arc::new(steps),
        config,
        Arc::new(FixedJitter(jitter)),
    )
    .map_err(Into::into)
}

#[derive(Clone, Copy)]
struct Seed {
    product_id: Uuid,
    account_id: Uuid,
    address_id: Uuid,
}

async fn seed_account(pool: &PgPool, number: u8) -> Result<Seed> {
    let product = NewProduct {
        id: Uuid::new_v4(),
        slug: format!("product-{number}"),
        settlement_url: format!("https://product-{number}.test/settlements"),
        webhook_url: format!("https://product-{number}.test/webhooks"),
        pubkey: format!("public-key-{number}"),
        kid: format!("product/{number}"),
        paused_scopes: Vec::new(),
    };
    db::create_product(pool, &product).await?;
    let account = NewAccount {
        id: Uuid::new_v4(),
        product_id: product.id,
        external_id: format!("workspace-{number}"),
        paused_scopes: Vec::new(),
    };
    db::create_account(pool, &account).await?;
    let address = NewAddress {
        id: Uuid::new_v4(),
        account_id: account.id,
        chain_id: 1,
        kind: AddressKind::Persistent,
        version: 1,
        lock_ref: None,
        salt: b256(number),
        address: evm_address(number),
        retired_at: None,
    };
    db::insert_address(pool, &address).await?;
    Ok(Seed {
        product_id: product.id,
        account_id: account.id,
        address_id: address.id,
    })
}

async fn insert_deposit(pool: &PgPool, seed: Seed, number: u8) -> Result<Uuid> {
    let deposit = NewDeposit {
        chain_id: 1,
        tx_hash: b256(number),
        log_index: 0,
        block_number: 100 + u64::from(number),
        block_hash: b256(number.wrapping_add(1)),
        block_time: Utc::now(),
        address_id: seed.address_id,
        account_id: seed.account_id,
        route: Some("phala-cloud-ethereum-pha-usd".to_owned()),
        route_version: Some(1),
        asset_contract: evm_address(200),
        from_address: evm_address(number.wrapping_add(100)),
        amount_atomic: AtomicAmount::new(U256::from(1_000)),
        state: DepositState::Detected,
        reason: None,
        next_attempt_at: Utc::now() - Duration::seconds(1),
    };
    let id = deposit_id(deposit.chain_id, deposit.tx_hash, deposit.log_index);
    ensure!(db::insert_deposit(pool, &deposit).await?);
    Ok(id)
}

async fn transition_count(pool: &PgPool, deposit_id: Uuid) -> Result<i64> {
    Ok(
        sqlx::query("SELECT count(*) FROM transitions WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(pool)
            .await?
            .try_get(0)?,
    )
}

async fn total_transition_count(pool: &PgPool) -> Result<i64> {
    Ok(sqlx::query("SELECT count(*) FROM transitions")
        .fetch_one(pool)
        .await?
        .try_get(0)?)
}

fn evm_address(byte: u8) -> Address {
    Address::from([byte; 20])
}

fn b256(byte: u8) -> B256 {
    B256::from([byte; 32])
}

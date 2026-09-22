//! PostgreSQL integration tests for the cleared-deposit settlement step.

use std::collections::VecDeque;
use std::env;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration as StdDuration;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool, Row};
use tokio::sync::{Mutex, Semaphore};
use topup::db::{self, AddressKind, NewAccount, NewAddress, NewDeposit, NewProduct};
use topup::pump::{JitterSource, Pump, PumpConfig, RunOnceResult, Step, StepResult, StepSet};
use topup::steps::settle::SettleStep;
use topup_adapters::settlement::http::{
    SettlementAnswer, SettlementApi, SettlementClientError, SettlementRequest,
};
use topup_adapters::signer::actor::SignerHandle;
use topup_core::deposit::{DepositState, StepOutcome, WaitReason};
use topup_core::identity::deposit_id;
use topup_core::money::AtomicAmount;
use topup_core::{
    Ed25519PublicKey, Ed25519Signature, SignedTx, Signer as CoreSigner, SignerError, TxRequest,
};
use url::Url;
use uuid::Uuid;

type TestFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

#[derive(Clone)]
struct TestSigner(ed25519_dalek::SigningKey);

impl CoreSigner for TestSigner {
    async fn sign_operator_tx(&self, _tx: TxRequest) -> Result<SignedTx, SignerError> {
        Err(SignerError::SigningFailed)
    }

    async fn sign_settlement(&self, payload: &[u8]) -> Result<Ed25519Signature, SignerError> {
        use ed25519_dalek::Signer as _;
        Ok(Ed25519Signature(self.0.sign(payload).to_bytes()))
    }

    async fn operator_address(&self) -> Result<Address, SignerError> {
        Err(SignerError::KeyUnavailable)
    }

    async fn settlement_public_key(&self) -> Result<Ed25519PublicKey, SignerError> {
        Ok(Ed25519PublicKey(self.0.verifying_key().to_bytes()))
    }
}

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
        let mut admin_url = Url::parse(&owner_template)?;
        admin_url.set_path("/postgres");
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(admin_url.as_str())
            .await?;
        sqlx::query("SELECT pg_advisory_lock(704_206_001)")
            .execute(&admin_pool)
            .await?;
        let suffix = Uuid::new_v4().simple().to_string();
        let database_name = format!("topup_c6_{suffix}");
        let app_role = format!("topup_c6_app_{suffix}");
        let password = format!("c6_{suffix}");
        admin_pool
            .execute(format!("CREATE DATABASE \"{database_name}\"").as_str())
            .await?;
        let mut owner_url = Url::parse(&owner_template)?;
        owner_url.set_path(&format!("/{database_name}"));
        let owner_pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(owner_url.as_str())
            .await?;
        db::migrate(&owner_pool).await?;
        admin_pool
            .execute(
                format!("CREATE ROLE \"{app_role}\" LOGIN PASSWORD '{password}' IN ROLE topup_app")
                    .as_str(),
            )
            .await?;
        let mut app_url = Url::parse(&app_template)?;
        app_url
            .set_username(&app_role)
            .map_err(|()| anyhow::anyhow!("DATABASE_URL cannot accept a username"))?;
        app_url
            .set_password(Some(&password))
            .map_err(|()| anyhow::anyhow!("DATABASE_URL cannot accept a password"))?;
        app_url.set_path(&format!("/{database_name}"));
        let app_pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(app_url.as_str())
            .await?;
        sqlx::query("SELECT pg_advisory_unlock(704_206_001)")
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
            .await?;
        self.admin_pool
            .execute(format!("DROP ROLE \"{}\"", self.app_role).as_str())
            .await?;
        self.admin_pool.close().await;
        Ok(())
    }
}

fn required_url(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

async fn with_database<F>(test: F) -> Result<()>
where
    F: for<'a> FnOnce(&'a TestContext) -> TestFuture<'a>,
{
    let Some(context) = TestContext::create().await? else {
        eprintln!("skipping settlement integration test: database URLs are not set");
        return Ok(());
    };
    let result = test(&context).await;
    result.and(context.cleanup().await)
}

#[derive(Clone)]
enum MockOutcome {
    Answer(SettlementAnswer),
    Failure,
}

#[derive(Clone)]
struct MockSettlementApi {
    posts: Arc<Mutex<Vec<Value>>>,
    gets: Arc<AtomicUsize>,
    post_outcomes: Arc<Mutex<VecDeque<MockOutcome>>>,
    get_outcomes: Arc<Mutex<VecDeque<MockOutcome>>>,
    entered: Arc<Semaphore>,
    release: Arc<Semaphore>,
    block_post: bool,
}

impl Default for MockSettlementApi {
    fn default() -> Self {
        Self {
            posts: Arc::new(Mutex::new(Vec::new())),
            gets: Arc::new(AtomicUsize::new(0)),
            post_outcomes: Arc::new(Mutex::new(VecDeque::new())),
            get_outcomes: Arc::new(Mutex::new(VecDeque::new())),
            entered: Arc::new(Semaphore::new(0)),
            release: Arc::new(Semaphore::new(0)),
            block_post: false,
        }
    }
}

impl MockSettlementApi {
    fn with_post(outcome: MockOutcome) -> Self {
        Self {
            post_outcomes: Arc::new(Mutex::new([outcome].into_iter().collect())),
            ..Self::default()
        }
    }

    fn with_sequences(posts: Vec<MockOutcome>, gets: Vec<MockOutcome>) -> Self {
        Self {
            post_outcomes: Arc::new(Mutex::new(posts.into_iter().collect())),
            get_outcomes: Arc::new(Mutex::new(gets.into_iter().collect())),
            ..Self::default()
        }
    }

    fn blocking() -> Self {
        Self {
            block_post: true,
            ..Self::with_post(MockOutcome::Answer(SettlementAnswer::Processing))
        }
    }
}

#[async_trait]
impl SettlementApi for MockSettlementApi {
    async fn post(
        &self,
        request: &SettlementRequest,
    ) -> Result<SettlementAnswer, SettlementClientError> {
        self.posts.lock().await.push(request.payload.clone());
        if self.block_post {
            self.entered.add_permits(1);
            self.release
                .acquire()
                .await
                .map_err(|_| SettlementClientError::InvalidEndpoint)?
                .forget();
        }
        match self
            .post_outcomes
            .lock()
            .await
            .pop_front()
            .unwrap_or(MockOutcome::Failure)
        {
            MockOutcome::Answer(answer) => Ok(answer),
            MockOutcome::Failure => Err(SettlementClientError::InvalidEndpoint),
        }
    }

    async fn get_by_key(
        &self,
        _key: &str,
    ) -> Result<Option<SettlementAnswer>, SettlementClientError> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        match self
            .get_outcomes
            .lock()
            .await
            .pop_front()
            .unwrap_or(MockOutcome::Failure)
        {
            MockOutcome::Answer(answer) => Ok(Some(answer)),
            MockOutcome::Failure => Ok(None),
        }
    }
}

#[tokio::test]
async fn accepted_and_rejected_answers_persist_rows_transitions_and_events() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let accepted_api =
                MockSettlementApi::with_post(MockOutcome::Answer(SettlementAnswer::Accepted {
                    destination_tx_id: "credit-1".to_owned(),
                }));
            let accepted_id = seed_cleared(&context.app_pool, 1).await?;
            let accepted = pump(&context.app_pool, accepted_api, StdDuration::from_secs(1))?;
            ensure!(
                accepted.run_once().await?
                    == RunOnceResult::Applied {
                        deposit_id: accepted_id
                    }
            );
            assert_state_and_event(
                &context.app_pool,
                accepted_id,
                "credited",
                "deposit.credited",
            )
            .await?;
            let row = settlement(&context.app_pool, accepted_id).await?;
            ensure!(row.0 == "accepted" && row.1.as_deref() == Some("credit-1"));

            let rejected_api =
                MockSettlementApi::with_post(MockOutcome::Answer(SettlementAnswer::Rejected {
                    reason: "cap".to_owned(),
                }));
            let rejected_id = seed_cleared(&context.app_pool, 2).await?;
            let rejected = pump(&context.app_pool, rejected_api, StdDuration::from_secs(1))?;
            rejected.run_once().await?;
            assert_state_and_event(
                &context.app_pool,
                rejected_id,
                "rejected",
                "deposit.rejected",
            )
            .await?;
            let row = settlement(&context.app_pool, rejected_id).await?;
            ensure!(row.0 == "rejected" && row.1.is_none());
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn unknown_result_gets_before_resend_and_keeps_payload_identical() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let api = MockSettlementApi::with_sequences(
                vec![
                    MockOutcome::Failure,
                    MockOutcome::Answer(SettlementAnswer::Accepted {
                        destination_tx_id: "credit-2".to_owned(),
                    }),
                ],
                vec![MockOutcome::Failure],
            );
            let id = seed_cleared(&context.app_pool, 3).await?;
            let worker = pump(&context.app_pool, api.clone(), StdDuration::from_secs(1))?;
            worker.run_once().await?;
            sqlx::query(
                "UPDATE deposits SET next_attempt_at = now() - interval '1 second' WHERE id = $1",
            )
            .bind(id)
            .execute(&context.app_pool)
            .await?;
            worker.run_once().await?;
            ensure!(api.gets.load(Ordering::SeqCst) == 1);
            let posts = api.posts.lock().await;
            ensure!(posts.len() == 2 && posts[0] == posts[1]);
            let row = settlement(&context.app_pool, id).await?;
            ensure!(row.0 == "accepted" && row.1.as_deref() == Some("credit-2"));
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn payload_mismatch_is_alert_retry_and_is_never_resent() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let api = MockSettlementApi::with_sequences(
                vec![MockOutcome::Answer(SettlementAnswer::PayloadMismatch422)],
                Vec::new(),
            );
            let id = seed_cleared(&context.app_pool, 4).await?;
            let worker = pump(&context.app_pool, api.clone(), StdDuration::from_secs(1))?;
            worker.run_once().await?;
            let evidence: Value =
                sqlx::query("SELECT evidence FROM transitions WHERE deposit_id = $1")
                    .bind(id)
                    .fetch_one(&context.app_pool)
                    .await?
                    .try_get(0)?;
            ensure!(evidence["alert_level"] == "alert");
            ensure!(api.posts.lock().await.len() == 1);
            ensure!(settlement(&context.app_pool, id).await?.0 == "sent");
            sqlx::query(
                "UPDATE deposits SET next_attempt_at = now() - interval '1 second' WHERE id = $1",
            )
            .bind(id)
            .execute(&context.app_pool)
            .await?;
            worker.run_once().await?;
            ensure!(api.posts.lock().await.len() == 1);
            ensure!(api.gets.load(Ordering::SeqCst) == 1);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn two_pumps_cannot_both_post_one_cleared_deposit() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let api = MockSettlementApi::blocking();
            let id = seed_cleared(&context.app_pool, 5).await?;
            let worker = pump(&context.app_pool, api.clone(), StdDuration::from_secs(5))?;
            let first_worker = worker.clone();
            let first = tokio::spawn(async move { first_worker.run_once().await });
            api.entered.acquire().await?.forget();
            ensure!(worker.run_once().await? == RunOnceResult::Idle);
            ensure!(api.posts.lock().await.len() == 1);
            api.release.add_permits(1);
            ensure!(first.await?? == RunOnceResult::Applied { deposit_id: id });
            Ok(())
        })
    })
    .await
}

#[derive(Clone)]
struct StaticStep;

#[async_trait]
impl Step for StaticStep {
    async fn run(&self, _deposit: &db::Deposit) -> StepResult {
        StepResult::new(
            StepOutcome::Wait {
                reason: WaitReason::Paused,
            },
            json!({"source": "test"}),
        )
    }
}

struct FixedJitter;

impl JitterSource for FixedJitter {
    fn next_u64(&self) -> u64 {
        0
    }
}

fn pump(pool: &PgPool, api: MockSettlementApi, timeout: StdDuration) -> Result<Pump> {
    let signer = SignerHandle::spawn(
        TestSigner(ed25519_dalek::SigningKey::from_bytes(&[2; 32])),
        std::num::NonZeroUsize::new(4).unwrap_or(std::num::NonZeroUsize::MIN),
        StdDuration::from_secs(1),
    )?;
    let steps = StepSet::new(
        Box::new(StaticStep),
        Box::new(StaticStep),
        Box::new(SettleStep::with_api(pool.clone(), signer, Arc::new(api))),
        Box::new(StaticStep),
    );
    Ok(Pump::with_jitter(
        pool.clone(),
        Arc::new(steps),
        PumpConfig {
            step_timeout: timeout,
            ..PumpConfig::default()
        },
        Arc::new(FixedJitter),
    )?)
}

async fn seed_cleared(pool: &PgPool, number: u8) -> Result<Uuid> {
    let product_id = Uuid::new_v4();
    db::create_product(
        pool,
        &NewProduct {
            id: product_id,
            slug: format!("product-{number}"),
            settlement_url: "http://product.test/settlements".to_owned(),
            webhook_url: "http://product.test/webhooks".to_owned(),
            pubkey: "test".to_owned(),
            kid: "product/v1".to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    let account_id = Uuid::new_v4();
    db::create_account(
        pool,
        &NewAccount {
            id: account_id,
            product_id,
            external_id: format!("workspace-{number}"),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    let address_id = Uuid::new_v4();
    db::insert_address(
        pool,
        &NewAddress {
            id: address_id,
            account_id,
            chain_id: 1,
            kind: AddressKind::Persistent,
            version: 1,
            lock_ref: None,
            salt: B256::from([number; 32]),
            address: Address::from([number; 20]),
            retired_at: None,
        },
    )
    .await?;
    let deposit = NewDeposit {
        chain_id: 1,
        tx_hash: B256::from([number; 32]),
        log_index: 0,
        block_number: 100,
        block_hash: B256::from([number.wrapping_add(1); 32]),
        block_time: Utc::now(),
        address_id,
        account_id,
        route: Some("ethereum-pha".to_owned()),
        route_version: Some(1),
        asset_contract: Address::from([200; 20]),
        from_address: Address::from([201; 20]),
        amount_atomic: AtomicAmount::new(U256::from(1_000_u64)),
        state: DepositState::Cleared,
        reason: None,
        next_attempt_at: Utc::now() - Duration::seconds(1),
    };
    let id = deposit_id(deposit.chain_id, deposit.tx_hash, deposit.log_index);
    ensure!(db::insert_deposit(pool, &deposit).await?);
    sqlx::query(
        "UPDATE deposits SET valuation_at = $2, price_scaled = 25000000, price_source = 'spot', credit_minor = 250 WHERE id = $1",
    )
    .bind(id)
    .bind(Utc::now())
    .execute(pool)
    .await?;
    Ok(id)
}

async fn settlement(pool: &PgPool, id: Uuid) -> Result<(String, Option<String>)> {
    let row =
        sqlx::query("SELECT status, destination_tx_id FROM settlements WHERE deposit_id = $1")
            .bind(id)
            .fetch_one(pool)
            .await?;
    Ok((row.try_get(0)?, row.try_get(1)?))
}

async fn assert_state_and_event(
    pool: &PgPool,
    id: Uuid,
    state: &str,
    event_type: &str,
) -> Result<()> {
    let stored = db::get_deposit(pool, id)
        .await?
        .context("deposit must exist")?;
    ensure!(format!("{:?}", stored.state).to_lowercase() == state);
    let count: i64 = sqlx::query("SELECT count(*) FROM outbox WHERE event_type = $1")
        .bind(event_type)
        .fetch_one(pool)
        .await?
        .try_get(0)?;
    ensure!(count == 1);
    let transition_count: i64 =
        sqlx::query("SELECT count(*) FROM transitions WHERE deposit_id = $1")
            .bind(id)
            .fetch_one(pool)
            .await?
            .try_get(0)?;
    ensure!(transition_count == 1);
    Ok(())
}

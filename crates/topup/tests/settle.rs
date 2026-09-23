//! PostgreSQL integration tests for the cleared-deposit settlement step.

mod support;

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
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{Duration, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool, Row};
use tokio::sync::{Mutex, Semaphore};
use topup::db::{self, AddressKind, NewAccount, NewAddress, NewDeposit, NewProduct};
use topup::jitter::JitterSource;
use topup::outbox::{DeliveryConfig, DeliveryWorker};
use topup::pump::{Pump, PumpConfig, RunOnceResult, Step, StepResult, StepSet};
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

/// Waits out transient `max_connections` exhaustion when many test databases share one
/// server under load; sqlx's 30 s default turns that into spurious `PoolTimedOut` failures.
const DB_ACQUIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

type TestFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

#[derive(Clone)]
struct TestSigner(ed25519_dalek::SigningKey);

impl TestSigner {
    fn verifying_key(&self) -> VerifyingKey {
        self.0.verifying_key()
    }
}

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

#[derive(Clone)]
struct WebhookState {
    verifying_key: VerifyingKey,
    bodies: Arc<Mutex<Vec<Value>>>,
}

struct ReferenceReceiver {
    url: String,
    bodies: Arc<Mutex<Vec<Value>>>,
    task: tokio::task::JoinHandle<()>,
}

impl ReferenceReceiver {
    async fn start(verifying_key: VerifyingKey) -> Result<Self> {
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/webhooks", post(reference_webhook))
            .with_state(WebhookState {
                verifying_key,
                bodies: Arc::clone(&bodies),
            });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            url: format!("http://{address}/webhooks"),
            bodies,
            task,
        })
    }

    async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

async fn reference_webhook(
    State(state): State<WebhookState>,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    if verify_standard_webhook(&state.verifying_key, &headers, &body).is_err() {
        return StatusCode::BAD_REQUEST;
    }
    let Ok(envelope) = serde_json::from_slice(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    state.bodies.lock().await.push(envelope);
    StatusCode::OK
}

fn verify_standard_webhook(
    verifying_key: &VerifyingKey,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), ()> {
    let id = webhook_header(headers, "webhook-id")?;
    let timestamp = webhook_header(headers, "webhook-timestamp")?;
    let timestamp_value = timestamp.parse::<i64>().map_err(|_| ())?;
    if Utc::now().timestamp().abs_diff(timestamp_value) > 5 * 60 {
        return Err(());
    }
    let content = [id.as_bytes(), b".", timestamp.as_bytes(), b".", body].concat();
    let signatures = webhook_header(headers, "webhook-signature")?;
    if signatures.split_whitespace().any(|candidate| {
        let Some(("v1a", encoded)) = candidate.split_once(',') else {
            return false;
        };
        let Ok(bytes) = STANDARD.decode(encoded) else {
            return false;
        };
        let Ok(bytes) = <[u8; 64]>::try_from(bytes) else {
            return false;
        };
        verifying_key
            .verify_strict(&content, &Signature::from_bytes(&bytes))
            .is_ok()
    }) {
        Ok(())
    } else {
        Err(())
    }
}

fn webhook_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, ()> {
    headers.get(name).ok_or(())?.to_str().map_err(|_| ())
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
            .acquire_timeout(DB_ACQUIRE_TIMEOUT)
            .connect(admin_url.as_str())
            .await?;
        support::ensure_app_role(&admin_pool).await?;
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
            .acquire_timeout(DB_ACQUIRE_TIMEOUT)
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
            .acquire_timeout(DB_ACQUIRE_TIMEOUT)
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
        let payload = json!({});
        Self {
            block_post: true,
            ..Self::with_post(MockOutcome::Answer(SettlementAnswer::Processing {
                payload,
            }))
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
            let accepted_id = seed_cleared(&context.app_pool, 1).await?;
            let accepted_api =
                MockSettlementApi::with_post(MockOutcome::Answer(SettlementAnswer::Accepted {
                    destination_tx_id: "credit-1".to_owned(),
                    payload: settlement_payload(&context.app_pool, accepted_id).await?,
                }));
            let accepted = pump(&context.app_pool, accepted_api)?;
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
            assert_event_product(&context.app_pool, accepted_id, "deposit.credited").await?;

            let rejected_id = seed_cleared(&context.app_pool, 2).await?;
            let rejected_api =
                MockSettlementApi::with_post(MockOutcome::Answer(SettlementAnswer::Rejected {
                    reason: "cap".to_owned(),
                    payload: settlement_payload(&context.app_pool, rejected_id).await?,
                }));
            let rejected = pump(&context.app_pool, rejected_api)?;
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
            assert_event_product(&context.app_pool, rejected_id, "deposit.rejected").await?;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn credited_and_rejected_events_deliver_to_reference_receiver() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let webhook_signer =
                Arc::new(TestSigner(ed25519_dalek::SigningKey::from_bytes(&[12; 32])));
            let receiver = ReferenceReceiver::start(webhook_signer.verifying_key()).await?;

            let accepted_id = seed_cleared(&context.app_pool, 12).await?;
            set_product_webhook(&context.app_pool, accepted_id, &receiver.url).await?;
            let accepted_api =
                MockSettlementApi::with_post(MockOutcome::Answer(SettlementAnswer::Accepted {
                    destination_tx_id: "credit-delivery".to_owned(),
                    payload: settlement_payload(&context.app_pool, accepted_id).await?,
                }));
            pump(&context.app_pool, accepted_api)?.run_once().await?;
            sqlx::query(
                "UPDATE deposits SET next_attempt_at = now() + interval '1 hour' WHERE id = $1",
            )
            .bind(accepted_id)
            .execute(&context.app_pool)
            .await?;

            let rejected_id = seed_cleared(&context.app_pool, 13).await?;
            set_product_webhook(&context.app_pool, rejected_id, &receiver.url).await?;
            let rejected_api =
                MockSettlementApi::with_post(MockOutcome::Answer(SettlementAnswer::Rejected {
                    reason: "cap".to_owned(),
                    payload: settlement_payload(&context.app_pool, rejected_id).await?,
                }));
            pump(&context.app_pool, rejected_api)?.run_once().await?;
            ensure!(
                db::get_deposit(&context.app_pool, accepted_id)
                    .await?
                    .context("accepted deposit must exist")?
                    .state
                    == DepositState::Credited
            );
            ensure!(
                db::get_deposit(&context.app_pool, rejected_id)
                    .await?
                    .context("rejected deposit must exist")?
                    .state
                    == DepositState::Rejected
            );

            let delivery = DeliveryWorker::new(
                context.app_pool.clone(),
                webhook_signer,
                DeliveryConfig {
                    batch_size: 2,
                    request_timeout: TEST_TIMEOUT,
                    claim_lease: StdDuration::from_secs(60),
                    poll_interval: StdDuration::from_millis(10),
                    response_body_limit: 1024,
                    age_alert_threshold: StdDuration::from_secs(60),
                },
            )?;
            ensure!(delivery.run_once().await? == 2);
            let bodies = receiver.bodies.lock().await;
            ensure!(bodies.len() == 2);
            ensure!(bodies.iter().any(|body| {
                body["type"] == "deposit.credited"
                    && body["data"]["deposit_id"] == accepted_id.to_string()
                    && body["data"].get("product_id").is_some()
            }));
            ensure!(bodies.iter().any(|body| {
                body["type"] == "deposit.rejected"
                    && body["data"]["deposit_id"] == rejected_id.to_string()
                    && body["data"].get("product_id").is_some()
            }));
            drop(bodies);
            receiver.stop().await;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn sent_row_gets_authoritative_answer_and_adopts_product_pricing() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let id = seed_cleared(&context.app_pool, 6).await?;
            let mut payload = settlement_payload(&context.app_pool, id).await?;
            let product_valuation = Utc::now() - Duration::minutes(7);
            payload["amount_minor"] = json!("999");
            payload["evidence"]["price_scaled"] = json!("33300000");
            payload["evidence"]["valuation_at"] = json!(product_valuation);
            persist_intent(&context.app_pool, id, settlement_payload(&context.app_pool, id).await?)
                .await?;
            db::mark_sent(&context.app_pool, id).await?;
            let api = MockSettlementApi::with_sequences(
                Vec::new(),
                vec![MockOutcome::Answer(SettlementAnswer::Accepted {
                    destination_tx_id: "credit-authoritative".to_owned(),
                    payload,
                })],
            );
            let worker = pump(&context.app_pool, api.clone())?;
            worker.run_once().await?;

            ensure!(api.gets.load(Ordering::SeqCst) == 1);
            ensure!(api.posts.lock().await.is_empty());
            let deposit = db::get_deposit(&context.app_pool, id)
                .await?
                .context("deposit must exist")?;
            ensure!(deposit.credit_minor.context("credit minor")?.value() == 999);
            ensure!(deposit.price_scaled == Some(33_300_000));
            ensure!(
                deposit.valuation_at.map(|value| value.timestamp_micros())
                    == Some(product_valuation.timestamp_micros())
            );
            let transition: Value =
                sqlx::query_scalar("SELECT evidence FROM transitions WHERE deposit_id = $1")
                    .bind(id)
                    .fetch_one(&context.app_pool)
                    .await?;
            ensure!(transition["pricing"]["local"]["amount_minor"] == 250);
            ensure!(transition["pricing"]["product"]["amount_minor"] == 999);
            let event: Value = sqlx::query_scalar(
                "SELECT payload FROM outbox WHERE event_type = 'deposit.credited' AND payload->>'deposit_id' = $1",
            )
            .bind(id.to_string())
            .fetch_one(&context.app_pool)
            .await?;
            ensure!(event["amount_minor"] == "999");
            ensure!(event["price_scaled"] == "33300000");
            ensure!(event["price_scale"] == topup_core::money::PRICE_SCALE);
            ensure!(
                event["valuation_at"]
                    .as_str()
                    .context("event valuation timestamp")?
                    .parse::<chrono::DateTime<Utc>>()?
                    .timestamp_micros()
                    == product_valuation.timestamp_micros()
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn terminal_row_restores_transition_without_get_or_post() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let id = seed_cleared(&context.app_pool, 7).await?;
            let payload = settlement_payload(&context.app_pool, id).await?;
            persist_intent(&context.app_pool, id, payload.clone()).await?;
            db::mark_accepted(
                &context.app_pool,
                id,
                "credit-saved",
                &json!({
                    "status": "accepted",
                    "destination_tx_id": "credit-saved",
                    "payload": payload,
                }),
            )
            .await?;
            let api = MockSettlementApi::default();
            let worker = pump(&context.app_pool, api.clone())?;
            worker.run_once().await?;

            ensure!(api.gets.load(Ordering::SeqCst) == 0);
            ensure!(api.posts.lock().await.is_empty());
            assert_state_and_event(&context.app_pool, id, "credited", "deposit.credited").await?;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn pauses_block_new_posts_but_not_get_adoption() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let api = MockSettlementApi::default();
            for (number, scope) in [(8, "account"), (9, "product"), (10, "route")] {
                let id = seed_cleared(&context.app_pool, number).await?;
                pause_settlement(&context.app_pool, id, scope).await?;
                let worker = pump(&context.app_pool, api.clone())?;
                worker.run_once().await?;
                let stored = db::get_deposit(&context.app_pool, id)
                    .await?
                    .context("deposit must exist")?;
                ensure!(stored.state == DepositState::Cleared);
                let evidence: Value = sqlx::query_scalar(
                    "SELECT evidence FROM transitions WHERE deposit_id = $1 ORDER BY created_at DESC LIMIT 1",
                )
                .bind(id)
                .fetch_one(&context.app_pool)
                .await?;
                ensure!(evidence["reason"] == "settlement_paused");
            }
            ensure!(api.posts.lock().await.is_empty());

            let id = seed_cleared(&context.app_pool, 11).await?;
            let payload = settlement_payload(&context.app_pool, id).await?;
            persist_intent(&context.app_pool, id, payload.clone()).await?;
            db::mark_sent(&context.app_pool, id).await?;
            pause_settlement(&context.app_pool, id, "account").await?;
            let adopting_api = MockSettlementApi::with_sequences(
                Vec::new(),
                vec![MockOutcome::Answer(SettlementAnswer::Accepted {
                    destination_tx_id: "credit-paused".to_owned(),
                    payload,
                })],
            );
            let worker = pump(&context.app_pool, adopting_api.clone())?;
            worker.run_once().await?;
            ensure!(adopting_api.gets.load(Ordering::SeqCst) == 1);
            ensure!(adopting_api.posts.lock().await.is_empty());
            ensure!(
                db::get_deposit(&context.app_pool, id)
                    .await?
                    .context("deposit must exist")?
                    .state
                    == DepositState::Credited
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn unknown_result_gets_before_resend_and_keeps_payload_identical() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let id = seed_cleared(&context.app_pool, 3).await?;
            let api = MockSettlementApi::with_sequences(
                vec![
                    MockOutcome::Failure,
                    MockOutcome::Answer(SettlementAnswer::Accepted {
                        destination_tx_id: "credit-2".to_owned(),
                        payload: settlement_payload(&context.app_pool, id).await?,
                    }),
                ],
                vec![MockOutcome::Failure],
            );
            let worker = pump(&context.app_pool, api.clone())?;
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
            let worker = pump(&context.app_pool, api.clone())?;
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
            ensure!(
                db::get_settlement(&context.app_pool, id)
                    .await?
                    .context("settlement must exist")?
                    .resend_forbidden
            );
            sqlx::query(
                "UPDATE deposits SET next_attempt_at = now() - interval '1 second' WHERE id = $1",
            )
            .bind(id)
            .execute(&context.app_pool)
            .await?;
            worker.run_once().await?;
            ensure!(api.posts.lock().await.len() == 1);
            ensure!(api.gets.load(Ordering::SeqCst) == 2);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn payload_mismatch_guard_survives_unknown_and_missing_gets() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let api = MockSettlementApi::with_sequences(
                vec![MockOutcome::Answer(SettlementAnswer::PayloadMismatch422)],
                vec![
                    MockOutcome::Answer(SettlementAnswer::Unknown {
                        status: 500,
                        body: "product failure".to_owned(),
                    }),
                    MockOutcome::Failure,
                    MockOutcome::Failure,
                ],
            );
            let id = seed_cleared(&context.app_pool, 14).await?;
            let worker = pump(&context.app_pool, api.clone())?;

            worker.run_once().await?;
            ensure!(api.posts.lock().await.len() == 1);
            ensure!(api.gets.load(Ordering::SeqCst) == 1);
            let stored = db::get_settlement(&context.app_pool, id)
                .await?
                .context("settlement must exist")?;
            ensure!(stored.resend_forbidden);
            ensure!(stored.receipt.context("receipt")?["status"] == "unknown");

            for expected_gets in [2, 3] {
                sqlx::query(
                    "UPDATE deposits SET next_attempt_at = now() - interval '1 second' WHERE id = $1",
                )
                .bind(id)
                .execute(&context.app_pool)
                .await?;
                worker.run_once().await?;
                ensure!(api.posts.lock().await.len() == 1);
                ensure!(api.gets.load(Ordering::SeqCst) == expected_gets);
                let evidence: Value = sqlx::query_scalar(
                    "SELECT evidence FROM transitions WHERE deposit_id = $1 ORDER BY created_at DESC LIMIT 1",
                )
                .bind(id)
                .fetch_one(&context.app_pool)
                .await?;
                ensure!(evidence["alert_level"] == "alert");
            }
            ensure!(
                db::get_settlement(&context.app_pool, id)
                    .await?
                    .context("settlement must exist")?
                    .resend_forbidden
            );
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
            let worker = pump(&context.app_pool, api.clone())?;
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

// No test here exercises a timeout firing; keep them generous so a loaded host does not
// turn a slow step or signer into a spurious failure.
const TEST_TIMEOUT: StdDuration = StdDuration::from_secs(10);

struct FixedJitter;

impl JitterSource for FixedJitter {
    fn next_u64(&self) -> u64 {
        0
    }
}

fn pump(pool: &PgPool, api: MockSettlementApi) -> Result<Pump> {
    let signer = SignerHandle::spawn(
        TestSigner(ed25519_dalek::SigningKey::from_bytes(&[2; 32])),
        std::num::NonZeroUsize::new(4).unwrap_or(std::num::NonZeroUsize::MIN),
        TEST_TIMEOUT,
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
            step_timeout: TEST_TIMEOUT,
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

async fn settlement_payload(pool: &PgPool, id: Uuid) -> Result<Value> {
    let deposit = db::get_deposit(pool, id)
        .await?
        .context("deposit must exist")?;
    let account = db::get_account(pool, deposit.account_id)
        .await?
        .context("account must exist")?;
    let address = db::get_address(pool, deposit.address_id)
        .await?
        .context("address must exist")?;
    Ok(json!({
        "version": 1,
        "idempotency_key": format!("deposit:{}", deposit.id),
        "account_id": account.external_id,
        "unit": "USD",
        "amount_minor": deposit.credit_minor.context("credit minor")?.value().to_string(),
        "source": "crypto_deposit",
        "evidence": {
            "chain_id": deposit.chain_id,
            "asset_contract": format!("{:#x}", deposit.asset_contract),
            "route": deposit.route.context("route")?,
            "route_version": deposit.route_version.context("route version")?,
            "tx_hash": format!("{:#x}", deposit.tx_hash),
            "log_index": deposit.log_index,
            "to": format!("{:#x}", address.address),
            "amount_atomic": deposit.amount_atomic.value().to_string(),
            "price_scaled": deposit.price_scaled.context("price scaled")?.to_string(),
            "price_scale": topup_core::money::PRICE_SCALE,
            "valuation_at": deposit.valuation_at.context("valuation at")?,
            "lock_ref": address.lock_ref,
        },
    }))
}

async fn persist_intent(pool: &PgPool, id: Uuid, payload: Value) -> Result<()> {
    let deposit = db::get_deposit(pool, id)
        .await?
        .context("deposit must exist")?;
    let account = db::get_account(pool, deposit.account_id)
        .await?
        .context("account must exist")?;
    db::upsert_intent(
        pool,
        &db::SettlementIntent {
            deposit_id: id,
            product_id: account.product_id,
            key: format!("deposit:{id}"),
            payload,
        },
    )
    .await?;
    Ok(())
}

async fn pause_settlement(pool: &PgPool, id: Uuid, level: &str) -> Result<()> {
    let deposit = db::get_deposit(pool, id)
        .await?
        .context("deposit must exist")?;
    let account = db::get_account(pool, deposit.account_id)
        .await?
        .context("account must exist")?;
    match level {
        "account" => {
            db::set_account_paused_scopes(pool, account.id, &["settlement".to_owned()]).await?;
        }
        "product" => {
            db::set_product_paused_scopes(pool, account.product_id, &["settlement".to_owned()])
                .await?;
        }
        "route" => {
            sqlx::query(
                "INSERT INTO route_pauses (route, paused_scopes) VALUES ($1, ARRAY['settlement']) ON CONFLICT (route) DO UPDATE SET paused_scopes = EXCLUDED.paused_scopes",
            )
            .bind(deposit.route.context("route")?)
            .execute(pool)
            .await?;
        }
        _ => anyhow::bail!("unknown pause level"),
    }
    Ok(())
}

async fn set_product_webhook(pool: &PgPool, id: Uuid, webhook_url: &str) -> Result<()> {
    let deposit = db::get_deposit(pool, id)
        .await?
        .context("deposit must exist")?;
    let account = db::get_account(pool, deposit.account_id)
        .await?
        .context("account must exist")?;
    sqlx::query("UPDATE products SET webhook_url = $2 WHERE id = $1")
        .bind(account.product_id)
        .bind(webhook_url)
        .execute(pool)
        .await?;
    Ok(())
}

async fn assert_event_product(pool: &PgPool, id: Uuid, event_type: &str) -> Result<()> {
    let deposit = db::get_deposit(pool, id)
        .await?
        .context("deposit must exist")?;
    let payload: Value = sqlx::query_scalar(
        "SELECT payload FROM outbox WHERE event_type = $1 AND payload->>'deposit_id' = $2",
    )
    .bind(event_type)
    .bind(id.to_string())
    .fetch_one(pool)
    .await?;
    ensure!(payload.get("product_id").and_then(Value::as_str).is_some());
    ensure!(payload["chain_id"] == deposit.chain_id);
    ensure!(payload["state"] == format!("{:?}", deposit.state).to_lowercase());
    ensure!(payload["route"] == deposit.route.context("deposit route")?);
    Ok(())
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

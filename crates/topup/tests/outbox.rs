//! PostgreSQL and reference-receiver tests for Standard Webhooks delivery.

mod support;

use std::collections::VecDeque;
use std::env;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration as StdDuration;

use anyhow::{Context, Result, ensure};
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header::LOCATION};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{Duration, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde_json::Value;
use sqlx::{PgPool, Row};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use topup::db::{self, EventObject, NewDeposit, NewOutboxEvent};
use topup::outbox::{DeliveryConfig, DeliveryWorker, SignedWebhook};
use topup::routes::RouteSet;
use topup_core::deposit::DepositState;
use topup_core::identity::deposit_id;
use topup_core::money::AtomicAmount;
use topup_core::{Ed25519PublicKey, Ed25519Signature, Signer as CoreSigner, SignerError};
use uuid::Uuid;

use support::TestDatabase;
use support::seed::{self, NewAccount, NewAddress, NewCustomer};

const TIMESTAMP_TOLERANCE_SECONDS: i64 = 5 * 60;

#[derive(Clone)]
struct TestSigner(SigningKey);

impl TestSigner {
    fn fixed() -> Self {
        Self(SigningKey::from_bytes(&[11_u8; 32]))
    }

    fn verifying_key(&self) -> VerifyingKey {
        self.0.verifying_key()
    }
}

impl CoreSigner for TestSigner {
    async fn sign_settlement(&self, content: &[u8]) -> Result<Ed25519Signature, SignerError> {
        Ok(Ed25519Signature(self.0.sign(content).to_bytes()))
    }

    async fn settlement_public_key(&self) -> Result<Ed25519PublicKey, SignerError> {
        Ok(Ed25519PublicKey(self.0.verifying_key().to_bytes()))
    }
}

#[derive(Clone, Debug)]
struct ReceivedWebhook {
    id: String,
    body: Vec<u8>,
}

#[derive(Clone)]
struct ResponsePlan {
    status: StatusCode,
    delay: StdDuration,
    location: Option<String>,
}

impl ResponsePlan {
    fn new(status: StatusCode, delay: StdDuration) -> Self {
        Self {
            status,
            delay,
            location: None,
        }
    }

    fn redirect(location: &str) -> Self {
        Self {
            status: StatusCode::FOUND,
            delay: StdDuration::ZERO,
            location: Some(location.to_owned()),
        }
    }
}

#[derive(Clone)]
struct ReceiverState {
    verifying_key: VerifyingKey,
    plans: Arc<Mutex<VecDeque<ResponsePlan>>>,
    fallback_plan: ResponsePlan,
    received: Arc<Mutex<Vec<ReceivedWebhook>>>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
    redirect_hits: Arc<AtomicUsize>,
}

struct ReferenceReceiver {
    url: String,
    received: Arc<Mutex<Vec<ReceivedWebhook>>>,
    max_in_flight: Arc<AtomicUsize>,
    redirect_hits: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl ReferenceReceiver {
    async fn start(
        verifying_key: VerifyingKey,
        status: StatusCode,
        delay: StdDuration,
    ) -> Result<Self> {
        Self::start_with_plans(verifying_key, vec![ResponsePlan::new(status, delay)]).await
    }

    async fn start_with_plans(
        verifying_key: VerifyingKey,
        plans: Vec<ResponsePlan>,
    ) -> Result<Self> {
        let fallback_plan = plans
            .last()
            .cloned()
            .context("reference receiver requires at least one response plan")?;
        let received = Arc::new(Mutex::new(Vec::new()));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let redirect_hits = Arc::new(AtomicUsize::new(0));
        let state = ReceiverState {
            verifying_key,
            plans: Arc::new(Mutex::new(plans.into_iter().collect())),
            fallback_plan,
            received: Arc::clone(&received),
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_in_flight: Arc::clone(&max_in_flight),
            redirect_hits: Arc::clone(&redirect_hits),
        };
        let app = Router::new()
            .route("/webhooks", post(reference_webhook))
            .route("/redirect-target", post(redirect_target))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            url: format!("http://{address}/webhooks"),
            received,
            max_in_flight,
            redirect_hits,
            task,
        })
    }

    async fn count(&self) -> usize {
        self.received.lock().await.len()
    }

    async fn ids(&self) -> Vec<String> {
        self.received
            .lock()
            .await
            .iter()
            .map(|delivery| delivery.id.clone())
            .collect()
    }

    async fn first_body(&self) -> Option<Value> {
        self.received
            .lock()
            .await
            .first()
            .and_then(|delivery| serde_json::from_slice(&delivery.body).ok())
    }

    async fn bodies(&self) -> Vec<Vec<u8>> {
        self.received
            .lock()
            .await
            .iter()
            .map(|delivery| delivery.body.clone())
            .collect()
    }

    async fn wait_for_count(&self, expected: usize) -> Result<()> {
        tokio::time::timeout(StdDuration::from_secs(2), async {
            loop {
                if self.count().await >= expected {
                    return;
                }
                sleep(StdDuration::from_millis(5)).await;
            }
        })
        .await
        .context("timed out waiting for webhook request")?;
        Ok(())
    }

    fn maximum_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }

    fn redirect_hits(&self) -> usize {
        self.redirect_hits.load(Ordering::SeqCst)
    }

    async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

struct InFlightGuard(Arc<AtomicUsize>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn reference_webhook(
    State(state): State<ReceiverState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let id = match verify_standard_webhook(
        &state.verifying_key,
        &headers,
        &body,
        Utc::now().timestamp(),
    ) {
        Ok(id) => id,
        Err(()) => return (StatusCode::BAD_REQUEST, "invalid signature").into_response(),
    };
    if serde_json::from_slice::<Value>(&body).is_err() {
        return (StatusCode::BAD_REQUEST, "invalid JSON").into_response();
    }
    let plan = state
        .plans
        .lock()
        .await
        .pop_front()
        .unwrap_or_else(|| state.fallback_plan.clone());
    let current = state
        .in_flight
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);
    state.max_in_flight.fetch_max(current, Ordering::SeqCst);
    let _guard = InFlightGuard(Arc::clone(&state.in_flight));
    state.received.lock().await.push(ReceivedWebhook {
        id,
        body: body.to_vec(),
    });
    if !plan.delay.is_zero() {
        sleep(plan.delay).await;
    };
    let mut response = (plan.status, "receiver-response-body").into_response();
    if let Some(location) = plan.location {
        let Ok(location) = HeaderValue::from_str(&location) else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "invalid redirect location",
            )
                .into_response();
        };
        response.headers_mut().insert(LOCATION, location);
    }
    response
}

async fn redirect_target(State(state): State<ReceiverState>) -> StatusCode {
    state.redirect_hits.fetch_add(1, Ordering::SeqCst);
    StatusCode::OK
}

fn verify_standard_webhook(
    verifying_key: &VerifyingKey,
    headers: &HeaderMap,
    body: &[u8],
    now: i64,
) -> Result<String, ()> {
    let id = header(headers, "webhook-id")?;
    let timestamp = header(headers, "webhook-timestamp")?;
    let timestamp_value = timestamp.parse::<i64>().map_err(|_| ())?;
    if now.abs_diff(timestamp_value) > u64::try_from(TIMESTAMP_TOLERANCE_SECONDS).map_err(|_| ())? {
        return Err(());
    }
    let mut content = Vec::with_capacity(id.len() + timestamp.len() + body.len() + 2);
    content.extend_from_slice(id.as_bytes());
    content.push(b'.');
    content.extend_from_slice(timestamp.as_bytes());
    content.push(b'.');
    content.extend_from_slice(body);

    let signatures = header(headers, "webhook-signature")?;
    let verified = signatures.split_whitespace().any(|candidate| {
        let Some((version, encoded)) = candidate.split_once(',') else {
            return false;
        };
        if version != "v1a" {
            return false;
        }
        let Ok(bytes) = STANDARD.decode(encoded) else {
            return false;
        };
        let Ok(bytes) = <[u8; 64]>::try_from(bytes) else {
            return false;
        };
        let signature = Signature::from_bytes(&bytes);
        verifying_key.verify_strict(&content, &signature).is_ok()
    });
    if verified { Ok(id.to_owned()) } else { Err(()) }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, ()> {
    headers.get(name).ok_or(())?.to_str().map_err(|_| ())
}

fn worker(pool: &PgPool, signer: Arc<TestSigner>) -> Result<DeliveryWorker<TestSigner>> {
    worker_with_timeout(pool, signer, StdDuration::from_secs(2))
}

fn worker_with_timeout(
    pool: &PgPool,
    signer: Arc<TestSigner>,
    request_timeout: StdDuration,
) -> Result<DeliveryWorker<TestSigner>> {
    DeliveryWorker::new(
        pool.clone(),
        Arc::new(RouteSet::new(Vec::new()).map_err(anyhow::Error::msg)?),
        signer,
        DeliveryConfig {
            batch_size: 1,
            request_timeout,
            claim_lease: StdDuration::from_secs(30),
            poll_interval: StdDuration::from_millis(10),
            response_body_limit: 16,
            age_alert_threshold: StdDuration::from_secs(60),
        },
    )
    .map_err(Into::into)
}

/// Seeds a live account whose one webhook endpoint is `webhook_url`; returns the account id.
async fn seed_account(pool: &PgPool, webhook_url: &str) -> Result<Uuid> {
    let account = seed::create_account(
        pool,
        &NewAccount {
            webhook_url: webhook_url.to_owned(),
            ..NewAccount::named("webhook-test")
        },
    )
    .await?;
    Ok(account.id)
}

/// Adds a webhook endpoint to `account_id` in the given mode.
async fn add_endpoint(pool: &PgPool, account_id: Uuid, livemode: bool, url: &str) -> Result<()> {
    sqlx::query(
        "INSERT INTO webhook_endpoints (id, account_id, livemode, url) VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::new_v4())
    .bind(account_id)
    .bind(livemode)
    .bind(url)
    .execute(pool)
    .await?;
    Ok(())
}

/// Enqueues `deposit.credited` about a new live deposit of `account_id`, due now; returns the
/// deposit id.
async fn seed_account_event(pool: &PgPool, account_id: Uuid, event_id: Uuid) -> Result<Uuid> {
    let unique = alloy_primitives::keccak256(event_id.as_bytes());
    let customer = seed::create_customer(
        pool,
        &NewCustomer {
            id: Uuid::new_v4(),
            account_id,
            livemode: true,
            client_reference_id: format!("account-{event_id}"),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    let address = seed::insert_address(
        pool,
        &NewAddress {
            id: Uuid::new_v4(),
            customer_id: customer.id,
            chain_id: 1,
            route: "phala-cloud-ethereum-pha-usd".to_owned(),
            salt: unique,
            address: alloy_primitives::Address::from_word(unique),
        },
    )
    .await?;
    let deposit = NewDeposit {
        chain_id: 1,
        tx_hash: alloy_primitives::keccak256(unique),
        log_index: 0,
        receipt_log_index: 0,
        tx_from: alloy_primitives::Address::ZERO,
        tx_nonce: 0,
        is_final: true,
        block_number: 100,
        block_hash: unique,
        block_time: Utc::now(),
        address_id: address.id,
        route: None,
        route_version: None,
        asset_contract: alloy_primitives::Address::repeat_byte(0x42),
        from_address: alloy_primitives::Address::repeat_byte(0x43),
        amount_atomic: AtomicAmount::new(alloy_primitives::U256::from(1_000_u64)),
        state: DepositState::Detected,
        reason: None,
        next_attempt_at: Utc::now() + Duration::hours(1),
    };
    let deposit_id = deposit_id(deposit.chain_id, deposit.tx_hash, deposit.log_index);
    ensure!(db::insert_deposit(pool, &deposit).await?);
    let mut connection = pool.acquire().await?;
    db::enqueue_in(
        &mut connection,
        &NewOutboxEvent {
            id: event_id,
            event_type: "deposit.credited".to_owned(),
            account_id,
            livemode: true,
            object: EventObject::Deposit(deposit_id),
            next_attempt_at: Utc::now() - Duration::seconds(1),
        },
    )
    .await?;
    Ok(deposit_id)
}

/// The `webhook-id` of a Stripe-style event.
fn evt(event_id: Uuid) -> String {
    topup::ids::format(topup::ids::EVENT, event_id)
}

async fn seed_event(pool: &PgPool, webhook_url: &str, event_id: Uuid) -> Result<Uuid> {
    let account_id = seed_account(pool, webhook_url).await?;
    seed_account_event(pool, account_id, event_id).await?;
    Ok(account_id)
}

#[tokio::test]
async fn reference_receiver_rejects_tampering_and_stale_timestamps() -> Result<()> {
    let signer = TestSigner::fixed();
    let receiver =
        ReferenceReceiver::start(signer.verifying_key(), StatusCode::OK, StdDuration::ZERO).await?;
    let client = reqwest::Client::new();
    let event_id = Uuid::new_v4();
    let body = br#"{"type":"deposit.confirmed","data":{}}"#;

    let signed = SignedWebhook::new(&signer, &evt(event_id), Utc::now().timestamp(), body).await?;
    let tampered = client
        .post(&receiver.url)
        .header("webhook-id", &signed.id)
        .header("webhook-timestamp", &signed.timestamp)
        .header("webhook-signature", &signed.signature)
        .body(br#"{"type":"deposit.rejected","data":{}}"#.to_vec())
        .send()
        .await?;
    ensure!(tampered.status() == StatusCode::BAD_REQUEST);

    let stale_timestamp = Utc::now().timestamp() - TIMESTAMP_TOLERANCE_SECONDS - 1;
    let stale = SignedWebhook::new(&signer, &evt(event_id), stale_timestamp, body).await?;
    let stale_response = client
        .post(&receiver.url)
        .header("webhook-id", &stale.id)
        .header("webhook-timestamp", &stale.timestamp)
        .header("webhook-signature", &stale.signature)
        .body(body.to_vec())
        .send()
        .await?;
    ensure!(stale_response.status() == StatusCode::BAD_REQUEST);
    ensure!(receiver.count().await == 0);
    receiver.stop().await;
    Ok(())
}

#[tokio::test]
async fn successful_delivery_marks_delivered_and_stores_response() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let signer = Arc::new(TestSigner::fixed());
    let receiver =
        ReferenceReceiver::start(signer.verifying_key(), StatusCode::OK, StdDuration::ZERO).await?;
    let event_id = Uuid::new_v4();
    seed_event(&context.app_pool, &receiver.url, event_id).await?;

    ensure!(worker(&context.app_pool, signer)?.run_once().await? == 1);
    let row = sqlx::query(
        "SELECT delivered_at, attempts, response FROM webhook_deliveries WHERE event_id = $1",
    )
    .bind(event_id)
    .fetch_one(&context.app_pool)
    .await?;
    ensure!(
        row.try_get::<Option<chrono::DateTime<Utc>>, _>("delivered_at")?
            .is_some()
    );
    ensure!(row.try_get::<i32, _>("attempts")? == 0);
    let response: Value = row.try_get("response")?;
    ensure!(response["status"] == 200);
    ensure!(response["body"] == "receiver-respons");
    ensure!(receiver.count().await == 1);
    let envelope = receiver
        .first_body()
        .await
        .context("missing webhook body")?;
    ensure!(envelope["id"] == evt(event_id) && envelope["object"] == "event");
    ensure!(envelope["type"] == "deposit.credited");
    ensure!(envelope["created"].is_i64());
    let deposit = &envelope["data"]["object"];
    ensure!(
        deposit["object"] == "deposit" && deposit["status"] == "detected",
        "{envelope}"
    );
    ensure!(
        deposit["id"]
            .as_str()
            .is_some_and(|id| id.starts_with("dep_"))
    );
    // The rendered data is stored, so every retry and replay sends it unchanged.
    let stored: Value = sqlx::query_scalar("SELECT data FROM events WHERE id = $1")
        .bind(event_id)
        .fetch_one(&context.app_pool)
        .await?;
    ensure!(stored == envelope["data"]);

    receiver.stop().await;
    context.cleanup().await
}

/// An event reaches every enabled endpoint of its own account and mode, with one body, and no
/// endpoint of another account or of the other mode.
#[tokio::test]
async fn events_reach_only_their_accounts_endpoints_in_their_mode() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let signer = Arc::new(TestSigner::fixed());
    let start =
        || ReferenceReceiver::start(signer.verifying_key(), StatusCode::OK, StdDuration::ZERO);
    let (first, second, test_mode, other_account) = (
        start().await?,
        start().await?,
        start().await?,
        start().await?,
    );
    let account_id = seed_account(&context.app_pool, &first.url).await?;
    add_endpoint(&context.app_pool, account_id, true, &second.url).await?;
    add_endpoint(&context.app_pool, account_id, false, &test_mode.url).await?;
    seed_account(&context.app_pool, &other_account.url).await?;
    let event_id = Uuid::new_v4();
    seed_account_event(&context.app_pool, account_id, event_id).await?;

    let delivery = worker(&context.app_pool, signer)?;
    ensure!(delivery.run_once().await? == 1);
    ensure!(delivery.run_once().await? == 1);
    ensure!(delivery.run_once().await? == 0);
    ensure!(first.ids().await == vec![evt(event_id)]);
    ensure!(second.ids().await == vec![evt(event_id)]);
    ensure!(first.bodies().await == second.bodies().await);
    ensure!(test_mode.count().await == 0 && other_account.count().await == 0);

    for receiver in [first, second, test_mode, other_account] {
        receiver.stop().await;
    }
    context.cleanup().await
}

#[tokio::test]
async fn server_error_increments_attempts_and_schedules_backoff() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let signer = Arc::new(TestSigner::fixed());
    let receiver = ReferenceReceiver::start(
        signer.verifying_key(),
        StatusCode::INTERNAL_SERVER_ERROR,
        StdDuration::ZERO,
    )
    .await?;
    let event_id = Uuid::nil();
    seed_event(&context.app_pool, &receiver.url, event_id).await?;
    let before = Utc::now();

    ensure!(worker(&context.app_pool, signer)?.run_once().await? == 1);
    let after = Utc::now();
    let row = sqlx::query(
        "SELECT delivered_at, attempts, next_attempt_at, response FROM webhook_deliveries WHERE event_id = $1",
    )
    .bind(event_id)
    .fetch_one(&context.app_pool)
    .await?;
    ensure!(
        row.try_get::<Option<chrono::DateTime<Utc>>, _>("delivered_at")?
            .is_none()
    );
    ensure!(row.try_get::<i32, _>("attempts")? == 1);
    let next_attempt_at: chrono::DateTime<Utc> = row.try_get("next_attempt_at")?;
    ensure!(next_attempt_at >= before);
    ensure!(next_attempt_at <= after + Duration::seconds(30));
    let response: Value = row.try_get("response")?;
    ensure!(response["status"] == 500);
    ensure!(response["error"] == "non_2xx_status");

    receiver.stop().await;
    context.cleanup().await
}

#[tokio::test]
async fn racing_workers_do_not_double_send_one_row() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let signer = Arc::new(TestSigner::fixed());
    let receiver = ReferenceReceiver::start(
        signer.verifying_key(),
        StatusCode::OK,
        StdDuration::from_millis(150),
    )
    .await?;
    let event_id = Uuid::new_v4();
    seed_event(&context.app_pool, &receiver.url, event_id).await?;
    let first = worker(&context.app_pool, Arc::clone(&signer))?;
    let second = worker(&context.app_pool, signer)?;

    let (first_count, second_count) = tokio::join!(first.run_once(), second.run_once());
    ensure!(first_count? + second_count? == 1);
    ensure!(receiver.count().await == 1);

    receiver.stop().await;
    context.cleanup().await
}

#[tokio::test]
async fn cancelled_delivery_releases_the_endpoint_lock() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let signer = Arc::new(TestSigner::fixed());
    let receiver = ReferenceReceiver::start_with_plans(
        signer.verifying_key(),
        vec![
            ResponsePlan::new(StatusCode::OK, StdDuration::from_secs(5)),
            ResponsePlan::new(StatusCode::OK, StdDuration::ZERO),
        ],
    )
    .await?;
    let event_id = Uuid::new_v4();
    seed_event(&context.app_pool, &receiver.url, event_id).await?;
    let first = worker(&context.app_pool, Arc::clone(&signer))?;
    let first_task = tokio::spawn(async move { first.run_once().await });

    receiver.wait_for_count(1).await?;
    first_task.abort();
    let cancelled = first_task.await;
    ensure!(cancelled.is_err_and(|error| error.is_cancelled()));

    let second = worker(&context.app_pool, signer)?;
    let claimed = tokio::time::timeout(StdDuration::from_secs(2), second.run_once())
        .await
        .context("second worker remained blocked after cancellation")??;
    ensure!(claimed == 1);
    receiver.wait_for_count(2).await?;
    let delivered_at: Option<chrono::DateTime<Utc>> =
        sqlx::query_scalar("SELECT delivered_at FROM webhook_deliveries WHERE event_id = $1")
            .bind(event_id)
            .fetch_one(&context.app_pool)
            .await?;
    ensure!(delivered_at.is_some());

    receiver.stop().await;
    context.cleanup().await
}

#[tokio::test]
async fn different_events_for_one_endpoint_are_delivered_sequentially() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let signer = Arc::new(TestSigner::fixed());
    let receiver = ReferenceReceiver::start(
        signer.verifying_key(),
        StatusCode::OK,
        StdDuration::from_millis(150),
    )
    .await?;
    let account_id = seed_account(&context.app_pool, &receiver.url).await?;
    let first_event = Uuid::new_v4();
    let second_event = Uuid::new_v4();
    seed_account_event(&context.app_pool, account_id, first_event).await?;
    seed_account_event(&context.app_pool, account_id, second_event).await?;
    let first = worker(&context.app_pool, Arc::clone(&signer))?;
    let second = worker(&context.app_pool, Arc::clone(&signer))?;

    let (first_count, second_count) = tokio::join!(first.run_once(), second.run_once());
    ensure!(first_count? + second_count? == 2);
    sqlx::query(
        "UPDATE webhook_deliveries SET next_attempt_at = now() - interval '1 second' \
         WHERE delivered_at IS NULL",
    )
    .execute(&context.app_pool)
    .await?;
    ensure!(worker(&context.app_pool, signer)?.run_once().await? == 1);
    ensure!(receiver.count().await == 2);
    ensure!(receiver.maximum_in_flight() == 1);

    receiver.stop().await;
    context.cleanup().await
}

#[tokio::test]
async fn request_timeout_is_recorded_as_a_retry() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let signer = Arc::new(TestSigner::fixed());
    let receiver = ReferenceReceiver::start(
        signer.verifying_key(),
        StatusCode::OK,
        StdDuration::from_millis(250),
    )
    .await?;
    let event_id = Uuid::new_v4();
    seed_event(&context.app_pool, &receiver.url, event_id).await?;

    let delivery = worker_with_timeout(&context.app_pool, signer, StdDuration::from_millis(50))?;
    ensure!(delivery.run_once().await? == 1);
    let row = sqlx::query(
        "SELECT delivered_at, attempts, response FROM webhook_deliveries WHERE event_id = $1",
    )
    .bind(event_id)
    .fetch_one(&context.app_pool)
    .await?;
    ensure!(
        row.try_get::<Option<chrono::DateTime<Utc>>, _>("delivered_at")?
            .is_none()
    );
    ensure!(row.try_get::<i32, _>("attempts")? == 1);
    let response: Value = row.try_get("response")?;
    ensure!(response["status"].is_null());
    ensure!(response["error"] == "request_timeout");

    receiver.stop().await;
    context.cleanup().await
}

#[tokio::test]
async fn redirect_is_not_followed_and_is_recorded() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let signer = Arc::new(TestSigner::fixed());
    let receiver = ReferenceReceiver::start_with_plans(
        signer.verifying_key(),
        vec![ResponsePlan::redirect("/redirect-target")],
    )
    .await?;
    let event_id = Uuid::new_v4();
    seed_event(&context.app_pool, &receiver.url, event_id).await?;

    ensure!(worker(&context.app_pool, signer)?.run_once().await? == 1);
    let row = sqlx::query("SELECT attempts, response FROM webhook_deliveries WHERE event_id = $1")
        .bind(event_id)
        .fetch_one(&context.app_pool)
        .await?;
    ensure!(row.try_get::<i32, _>("attempts")? == 1);
    let response: Value = row.try_get("response")?;
    ensure!(response["status"] == 302);
    ensure!(response["error"] == "non_2xx_status");
    ensure!(receiver.redirect_hits() == 0);

    receiver.stop().await;
    context.cleanup().await
}

#[tokio::test]
async fn successful_retry_keeps_the_same_webhook_id_and_body() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let signer = Arc::new(TestSigner::fixed());
    let receiver = ReferenceReceiver::start_with_plans(
        signer.verifying_key(),
        vec![
            ResponsePlan::new(StatusCode::INTERNAL_SERVER_ERROR, StdDuration::ZERO),
            ResponsePlan::new(StatusCode::OK, StdDuration::ZERO),
        ],
    )
    .await?;
    let event_id = Uuid::new_v4();
    seed_event(&context.app_pool, &receiver.url, event_id).await?;
    let delivery = worker(&context.app_pool, signer)?;

    ensure!(delivery.run_once().await? == 1);
    sqlx::query(
        "UPDATE webhook_deliveries SET next_attempt_at = now() - interval '1 second' \
         WHERE event_id = $1",
    )
    .bind(event_id)
    .execute(&context.app_pool)
    .await?;
    ensure!(delivery.run_once().await? == 1);
    ensure!(receiver.ids().await == vec![evt(event_id), evt(event_id)]);
    let mut bodies = receiver.bodies().await.into_iter();
    let first_body = bodies.next().context("missing failed delivery body")?;
    let second_body = bodies.next().context("missing successful retry body")?;
    ensure!(first_body == second_body);
    ensure!(bodies.next().is_none());
    let row =
        sqlx::query("SELECT delivered_at, attempts FROM webhook_deliveries WHERE event_id = $1")
            .bind(event_id)
            .fetch_one(&context.app_pool)
            .await?;
    ensure!(
        row.try_get::<Option<chrono::DateTime<Utc>>, _>("delivered_at")?
            .is_some()
    );
    ensure!(row.try_get::<i32, _>("attempts")? == 1);

    receiver.stop().await;
    context.cleanup().await
}

#[tokio::test]
async fn forced_cli_replay_redelivers_with_the_same_webhook_id() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let signer = Arc::new(TestSigner::fixed());
    let receiver =
        ReferenceReceiver::start(signer.verifying_key(), StatusCode::OK, StdDuration::ZERO).await?;
    let event_id = Uuid::new_v4();
    seed_event(&context.app_pool, &receiver.url, event_id).await?;
    let delivery = worker(&context.app_pool, signer)?;
    ensure!(delivery.run_once().await? == 1);

    let output = Command::new(env!("CARGO_BIN_EXE_topup"))
        .args(["outbox", "replay", "--id", &evt(event_id), "--force"])
        .env("DATABASE_URL", &context.app_url)
        .output()
        .context("run topup outbox replay")?;
    ensure!(
        output.status.success(),
        "replay failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(delivery.run_once().await? == 1);
    ensure!(receiver.ids().await == vec![evt(event_id), evt(event_id)]);
    let audit_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit WHERE action = 'outbox.replay' AND subject = $1",
    )
    .bind(format!("event:{event_id}"))
    .fetch_one(&context.app_pool)
    .await?;
    ensure!(audit_count == 1);

    receiver.stop().await;
    context.cleanup().await
}

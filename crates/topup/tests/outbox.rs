//! PostgreSQL and reference-receiver tests for Standard Webhooks delivery.

mod support;

use std::collections::VecDeque;
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
use sha2::{Digest as _, Sha256};
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
use topup_core::{
    Ed25519PublicKey, Ed25519Signature, Signer as CoreSigner, SignerError, WebhookKeyId,
};
use uuid::Uuid;

use support::TestDatabase;
use support::seed::{self, NewAccount, NewAddress, NewCustomer};

const TIMESTAMP_TOLERANCE_SECONDS: i64 = 5 * 60;

/// Derives each webhook key as `SHA-256(seed ‖ domain)`, so every account, mode, and version has
/// its own key, as dstack derives them.
#[derive(Clone)]
struct TestSigner([u8; 32]);

impl TestSigner {
    fn fixed() -> Self {
        Self([11_u8; 32])
    }

    fn signing_key(&self, key: &WebhookKeyId) -> SigningKey {
        let mut hasher = Sha256::new();
        hasher.update(self.0);
        hasher.update(key.domain().as_bytes());
        SigningKey::from_bytes(&hasher.finalize().into())
    }

    /// The key of `account`'s deliveries in `livemode` at `version`.
    fn key(&self, account: Uuid, livemode: bool, version: u32) -> VerifyingKey {
        let id = WebhookKeyId::new(&acct(account), livemode, version)
            .unwrap_or_else(|| unreachable!("acct_ ids name keys"));
        self.signing_key(&id).verifying_key()
    }

    /// The live key of `account`'s deliveries at version 1.
    fn verifying_key(&self, account: Uuid) -> VerifyingKey {
        self.key(account, true, 1)
    }
}

impl CoreSigner for TestSigner {
    async fn sign_webhook(
        &self,
        key: &WebhookKeyId,
        content: &[u8],
    ) -> Result<Ed25519Signature, SignerError> {
        Ok(Ed25519Signature(
            self.signing_key(key).sign(content).to_bytes(),
        ))
    }

    async fn webhook_public_key(
        &self,
        key: &WebhookKeyId,
    ) -> Result<Ed25519PublicKey, SignerError> {
        Ok(Ed25519PublicKey(
            self.signing_key(key).verifying_key().to_bytes(),
        ))
    }
}

/// An account's `acct_` id.
fn acct(account: Uuid) -> String {
    topup::ids::format(topup::ids::ACCOUNT, account)
}

#[derive(Clone, Debug)]
struct ReceivedWebhook {
    id: String,
    signature: String,
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

    async fn signatures(&self) -> Vec<String> {
        self.received
            .lock()
            .await
            .iter()
            .map(|delivery| delivery.signature.clone())
            .collect()
    }

    async fn bodies(&self) -> Vec<Vec<u8>> {
        self.received
            .lock()
            .await
            .iter()
            .map(|delivery| delivery.body.clone())
            .collect()
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
        signature: header(&headers, "webhook-signature")
            .unwrap_or_default()
            .to_owned(),
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

/// A worker of live-mode events.
fn worker(pool: &PgPool, signer: Arc<TestSigner>) -> Result<DeliveryWorker<TestSigner>> {
    worker_with_timeout(pool, signer, StdDuration::from_secs(2))
}

fn worker_with_timeout(
    pool: &PgPool,
    signer: Arc<TestSigner>,
    request_timeout: StdDuration,
) -> Result<DeliveryWorker<TestSigner>> {
    mode_worker(pool, signer, true, request_timeout)
}

fn mode_worker(
    pool: &PgPool,
    signer: Arc<TestSigner>,
    livemode: bool,
    request_timeout: StdDuration,
) -> Result<DeliveryWorker<TestSigner>> {
    DeliveryWorker::new(
        pool.clone(),
        Arc::new(RouteSet::new(Vec::new()).map_err(anyhow::Error::msg)?),
        signer,
        livemode,
        DeliveryConfig {
            max_in_flight: 1,
            endpoint_concurrency: 1,
            request_timeout,
            ..test_config()
        },
    )
    .map_err(Into::into)
}

/// Test limits: one pass claims every due delivery, 4 at a time per endpoint.
fn test_config() -> DeliveryConfig {
    DeliveryConfig {
        max_in_flight: 16,
        endpoint_concurrency: 4,
        request_timeout: StdDuration::from_secs(2),
        claim_lease: StdDuration::from_secs(30),
        poll_interval: StdDuration::from_millis(10),
        response_body_limit: 16,
        age_alert_threshold: StdDuration::from_secs(60),
        proxy: None,
    }
}

/// A live-mode worker with `config`.
fn configured_worker(
    pool: &PgPool,
    signer: Arc<TestSigner>,
    config: DeliveryConfig,
) -> Result<DeliveryWorker<TestSigner>> {
    DeliveryWorker::new(
        pool.clone(),
        Arc::new(RouteSet::new(Vec::new()).map_err(anyhow::Error::msg)?),
        signer,
        true,
        config,
    )
    .map_err(Into::into)
}

/// Seeds the live account `id` whose one webhook endpoint is `webhook_url`; returns its id.
async fn seed_account(pool: &PgPool, id: Uuid, webhook_url: &str) -> Result<Uuid> {
    let account = seed::create_account(
        pool,
        &NewAccount {
            id,
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
    seed_mode_event(pool, account_id, true, event_id).await
}

/// Enqueues `deposit.credited` about a new deposit of `account_id` in `livemode`, due now;
/// returns the deposit id.
async fn seed_mode_event(
    pool: &PgPool,
    account_id: Uuid,
    livemode: bool,
    event_id: Uuid,
) -> Result<Uuid> {
    let unique = alloy_primitives::keccak256(event_id.as_bytes());
    let customer = seed::create_customer(
        pool,
        &NewCustomer {
            id: Uuid::new_v4(),
            account_id,
            livemode,
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
    // The deposit starts with a copy of its quote's metadata, and events render it.
    sqlx::query("UPDATE quotes SET metadata = $2 WHERE id = $1")
        .bind(address.quote_id)
        .bind(serde_json::json!({ "order_id": "6735" }))
        .execute(pool)
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
            livemode,
            object: EventObject::Deposit(deposit_id),
            next_attempt_at: Utc::now() - Duration::seconds(1),
            actor: topup::db::SYSTEM_ACTOR.to_owned(),
        },
    )
    .await?;
    Ok(deposit_id)
}

/// The `webhook-id` of a Stripe-style event.
fn evt(event_id: Uuid) -> String {
    topup::ids::format(topup::ids::EVENT, event_id)
}

async fn seed_event(
    pool: &PgPool,
    account: Uuid,
    webhook_url: &str,
    event_id: Uuid,
) -> Result<Uuid> {
    let account_id = seed_account(pool, account, webhook_url).await?;
    seed_account_event(pool, account_id, event_id).await?;
    Ok(account_id)
}

#[tokio::test]
async fn reference_receiver_rejects_tampering_and_stale_timestamps() -> Result<()> {
    let signer = TestSigner::fixed();
    let account = Uuid::new_v4();
    let receiver = ReferenceReceiver::start(
        signer.verifying_key(account),
        StatusCode::OK,
        StdDuration::ZERO,
    )
    .await?;
    let client = reqwest::Client::new();
    let event_id = Uuid::new_v4();
    let body = br#"{"type":"deposit.confirmed","data":{}}"#;

    let key = WebhookKeyId::new(&acct(account), true, 1).context("key id")?;
    let signed = SignedWebhook::new(
        &signer,
        std::slice::from_ref(&key),
        &evt(event_id),
        Utc::now().timestamp(),
        body,
    )
    .await?;
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
    let stale = SignedWebhook::new(&signer, &[key], &evt(event_id), stale_timestamp, body).await?;
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
    let account = Uuid::new_v4();
    let receiver = ReferenceReceiver::start(
        signer.verifying_key(account),
        StatusCode::OK,
        StdDuration::ZERO,
    )
    .await?;
    let event_id = Uuid::new_v4();
    seed_event(&context.app_pool, account, &receiver.url, event_id).await?;

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
        deposit["object"] == "deposit" && deposit["status"] == "pending",
        "{envelope}"
    );
    ensure!(deposit["metadata"] == serde_json::json!({ "order_id": "6735" }));
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
    let account = Uuid::new_v4();
    let start = || {
        ReferenceReceiver::start(
            signer.verifying_key(account),
            StatusCode::OK,
            StdDuration::ZERO,
        )
    };
    let (first, second, test_mode, other_account) = (
        start().await?,
        start().await?,
        start().await?,
        start().await?,
    );
    let account_id = seed_account(&context.app_pool, account, &first.url).await?;
    add_endpoint(&context.app_pool, account_id, true, &second.url).await?;
    add_endpoint(&context.app_pool, account_id, false, &test_mode.url).await?;
    seed_account(&context.app_pool, Uuid::new_v4(), &other_account.url).await?;
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
    let account = Uuid::new_v4();
    let receiver = ReferenceReceiver::start(
        signer.verifying_key(account),
        StatusCode::INTERNAL_SERVER_ERROR,
        StdDuration::ZERO,
    )
    .await?;
    let event_id = Uuid::nil();
    seed_event(&context.app_pool, account, &receiver.url, event_id).await?;
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
    let account = Uuid::new_v4();
    let receiver = ReferenceReceiver::start(
        signer.verifying_key(account),
        StatusCode::OK,
        StdDuration::from_millis(150),
    )
    .await?;
    let event_id = Uuid::new_v4();
    seed_event(&context.app_pool, account, &receiver.url, event_id).await?;
    let first = worker(&context.app_pool, Arc::clone(&signer))?;
    let second = worker(&context.app_pool, signer)?;

    let (first_count, second_count) = tokio::join!(first.run_once(), second.run_once());
    ensure!(first_count? + second_count? == 1);
    ensure!(receiver.count().await == 1);

    receiver.stop().await;
    context.cleanup().await
}

#[tokio::test]
async fn request_timeout_is_recorded_as_a_retry() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let signer = Arc::new(TestSigner::fixed());
    let account = Uuid::new_v4();
    let receiver = ReferenceReceiver::start(
        signer.verifying_key(account),
        StatusCode::OK,
        StdDuration::from_millis(250),
    )
    .await?;
    let event_id = Uuid::new_v4();
    seed_event(&context.app_pool, account, &receiver.url, event_id).await?;

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
    let account = Uuid::new_v4();
    let receiver = ReferenceReceiver::start_with_plans(
        signer.verifying_key(account),
        vec![ResponsePlan::redirect("/redirect-target")],
    )
    .await?;
    let event_id = Uuid::new_v4();
    seed_event(&context.app_pool, account, &receiver.url, event_id).await?;

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
    let account = Uuid::new_v4();
    let receiver = ReferenceReceiver::start_with_plans(
        signer.verifying_key(account),
        vec![
            ResponsePlan::new(StatusCode::INTERNAL_SERVER_ERROR, StdDuration::ZERO),
            ResponsePlan::new(StatusCode::OK, StdDuration::ZERO),
        ],
    )
    .await?;
    let event_id = Uuid::new_v4();
    seed_event(&context.app_pool, account, &receiver.url, event_id).await?;
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
async fn deliveries_verify_only_with_their_accounts_key_in_their_mode() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let signer = Arc::new(TestSigner::fixed());
    let account = Uuid::new_v4();
    let other = Uuid::new_v4();
    let start = |key| ReferenceReceiver::start(key, StatusCode::OK, StdDuration::ZERO);
    let own = start(signer.key(account, true, 1)).await?;
    let other_account = start(signer.key(other, true, 1)).await?;
    let other_mode = start(signer.key(account, false, 1)).await?;
    seed_account(&context.app_pool, account, &own.url).await?;
    seed_account(&context.app_pool, other, "").await?;
    add_endpoint(&context.app_pool, account, true, &other_account.url).await?;
    add_endpoint(&context.app_pool, account, true, &other_mode.url).await?;
    let event_id = Uuid::new_v4();
    seed_account_event(&context.app_pool, account, event_id).await?;

    let delivery = worker(&context.app_pool, signer)?;
    for _ in 0..3 {
        ensure!(delivery.run_once().await? == 1);
    }
    ensure!(own.ids().await == vec![evt(event_id)]);
    let envelope = own.first_body().await.context("missing webhook body")?;
    ensure!(envelope["account"] == acct(account) && envelope["livemode"] == true);
    ensure!(envelope["data"]["object"]["livemode"] == true);
    ensure!(other_account.count().await == 0 && other_mode.count().await == 0);
    let refused: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM webhook_deliveries \
         WHERE event_id = $1 AND delivered_at IS NULL AND response ->> 'status' = '400'",
    )
    .bind(event_id)
    .fetch_one(&context.app_pool)
    .await?;
    ensure!(refused == 2);

    for receiver in [own, other_account, other_mode] {
        receiver.stop().await;
    }
    context.cleanup().await
}

/// Test and live events have separate workers: each delivers its own mode's events only, signed
/// with that mode's key.
#[tokio::test]
async fn test_and_live_workers_deliver_only_their_modes_events() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let signer = Arc::new(TestSigner::fixed());
    let account = Uuid::new_v4();
    let live = ReferenceReceiver::start(
        signer.key(account, true, 1),
        StatusCode::OK,
        StdDuration::ZERO,
    )
    .await?;
    let test = ReferenceReceiver::start(
        signer.key(account, false, 1),
        StatusCode::OK,
        StdDuration::ZERO,
    )
    .await?;
    seed_account(&context.app_pool, account, &live.url).await?;
    add_endpoint(&context.app_pool, account, false, &test.url).await?;
    let (live_event, test_event) = (Uuid::new_v4(), Uuid::new_v4());
    seed_mode_event(&context.app_pool, account, true, live_event).await?;
    seed_mode_event(&context.app_pool, account, false, test_event).await?;

    let timeout = StdDuration::from_secs(2);
    let test_worker = mode_worker(&context.app_pool, Arc::clone(&signer), false, timeout)?;
    let live_worker = mode_worker(&context.app_pool, signer, true, timeout)?;
    ensure!(test_worker.run_once().await? == 1);
    ensure!(test_worker.run_once().await? == 0);
    ensure!(test.ids().await == vec![evt(test_event)] && live.count().await == 0);
    let envelope = test
        .first_body()
        .await
        .context("missing test webhook body")?;
    ensure!(envelope["livemode"] == false && envelope["data"]["object"]["livemode"] == false);
    ensure!(live_worker.run_once().await? == 1);
    ensure!(live_worker.run_once().await? == 0);
    ensure!(live.ids().await == vec![evt(live_event)]);

    live.stop().await;
    test.stop().await;
    context.cleanup().await
}

/// A roll signs with both keys during the overlap, so receivers pinned to either verify; after
/// the overlap only the new key signs.
#[tokio::test]
async fn a_rolled_key_signs_beside_the_new_one_until_its_overlap_ends() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let signer = Arc::new(TestSigner::fixed());
    let account = Uuid::new_v4();
    let old = ReferenceReceiver::start(
        signer.key(account, true, 1),
        StatusCode::OK,
        StdDuration::ZERO,
    )
    .await?;
    let new = ReferenceReceiver::start(
        signer.key(account, true, 2),
        StatusCode::OK,
        StdDuration::ZERO,
    )
    .await?;
    seed_account(&context.app_pool, account, &old.url).await?;
    add_endpoint(&context.app_pool, account, true, &new.url).await?;
    let scope = topup::tenancy::Scope::new(account, true);
    let actor =
        topup::audit::Actor::api_key(topup::ids::format(topup::ids::API_KEY, Uuid::new_v4()));
    let keys =
        topup::webhook_keys::roll(&context.app_pool, scope, Duration::hours(1), &actor).await?;
    ensure!(
        keys.versions
            .iter()
            .map(|key| key.version)
            .collect::<Vec<_>>()
            == [2, 1]
    );
    // The roll's own `account.updated` is not about this test.
    sqlx::query("DELETE FROM webhook_deliveries")
        .execute(&context.app_pool)
        .await?;
    let during = Uuid::new_v4();
    seed_account_event(&context.app_pool, account, during).await?;

    let delivery = worker(&context.app_pool, signer)?;
    ensure!(delivery.run_once().await? == 1);
    ensure!(delivery.run_once().await? == 1);
    ensure!(old.ids().await == vec![evt(during)] && new.ids().await == vec![evt(during)]);
    ensure!(
        new.signatures()
            .await
            .iter()
            .all(|signature| signature.split(' ').count() == 2)
    );

    sqlx::query("UPDATE retiring_webhook_keys SET expires_at = now() - interval '1 second'")
        .execute(&context.app_pool)
        .await?;
    let after = Uuid::new_v4();
    seed_account_event(&context.app_pool, account, after).await?;
    ensure!(delivery.run_once().await? == 1);
    ensure!(delivery.run_once().await? == 1);
    ensure!(new.ids().await == vec![evt(during), evt(after)]);
    ensure!(old.ids().await == vec![evt(during)]);
    ensure!(
        new.signatures()
            .await
            .last()
            .is_some_and(|signature| signature.split(' ').count() == 1)
    );

    old.stop().await;
    new.stop().await;
    context.cleanup().await
}

/// Adds a live endpoint of `account_id` subscribed to `events`; returns its id.
async fn add_subscribed_endpoint(
    pool: &PgPool,
    account_id: Uuid,
    url: &str,
    events: &[&str],
) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO webhook_endpoints (id, account_id, livemode, url, enabled_events) \
         VALUES ($1, $2, true, $3, $4)",
    )
    .bind(id)
    .bind(account_id)
    .bind(url)
    .bind(events)
    .execute(pool)
    .await?;
    Ok(id)
}

async fn endpoint_id(pool: &PgPool, url: &str) -> Result<Uuid> {
    Ok(
        sqlx::query_scalar("SELECT id FROM webhook_endpoints WHERE url = $1")
            .bind(url)
            .fetch_one(pool)
            .await?,
    )
}

/// The `type` of every event the receiver got, in order.
async fn types(receiver: &ReferenceReceiver) -> Vec<String> {
    receiver
        .bodies()
        .await
        .iter()
        .filter_map(|body| serde_json::from_slice::<Value>(body).ok())
        .filter_map(|body| body["type"].as_str().map(str::to_owned))
        .collect()
}

/// Runs passes until nothing is due.
async fn drain(worker: &DeliveryWorker<TestSigner>) -> Result<()> {
    for _ in 0..20 {
        if worker.run_once().await? == 0 {
            return Ok(());
        }
    }
    anyhow::bail!("deliveries kept coming")
}

async fn wait_until_count(
    receiver: &ReferenceReceiver,
    expected: usize,
    within: StdDuration,
) -> Result<()> {
    tokio::time::timeout(within, async {
        while receiver.count().await < expected {
            sleep(StdDuration::from_millis(5)).await;
        }
    })
    .await
    .with_context(|| format!("timed out waiting for {expected} webhook requests"))
}

fn merchant_actor() -> topup::audit::Actor {
    topup::audit::Actor::api_key(topup::ids::format(topup::ids::API_KEY, Uuid::new_v4()))
}

/// An event reaches every enabled endpoint subscribed to its type or to `*`, and no other.
#[tokio::test]
async fn events_fan_out_to_every_endpoint_subscribed_to_them() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let pool = &context.app_pool;
    let signer = Arc::new(TestSigner::fixed());
    let account = Uuid::new_v4();
    let start = || {
        ReferenceReceiver::start(
            signer.verifying_key(account),
            StatusCode::OK,
            StdDuration::ZERO,
        )
    };
    let (all, credited, refunds, disabled) = (
        start().await?,
        start().await?,
        start().await?,
        start().await?,
    );
    let account_id = seed_account(pool, account, &all.url).await?;
    add_subscribed_endpoint(
        pool,
        account_id,
        &credited.url,
        &["deposit.credited", "quote.expired"],
    )
    .await?;
    add_subscribed_endpoint(pool, account_id, &refunds.url, &["refund.failed"]).await?;
    let off = add_subscribed_endpoint(pool, account_id, &disabled.url, &["*"]).await?;
    sqlx::query("UPDATE webhook_endpoints SET status = 'disabled' WHERE id = $1")
        .bind(off)
        .execute(pool)
        .await?;
    let event_id = Uuid::new_v4();
    seed_account_event(pool, account_id, event_id).await?;

    ensure!(
        configured_worker(pool, signer, test_config())?
            .run_once()
            .await?
            == 2
    );
    ensure!(all.ids().await == vec![evt(event_id)]);
    ensure!(credited.ids().await == vec![evt(event_id)]);
    ensure!(refunds.count().await == 0 && disabled.count().await == 0);

    for receiver in [all, credited, refunds, disabled] {
        receiver.stop().await;
    }
    context.cleanup().await
}

/// Account events reach every enabled endpoint of the mode whatever it subscribes to, and an
/// endpoint's change is announced to that endpoint first, at the URL it had before the change,
/// even when the change disables or deletes it.
#[tokio::test]
async fn account_events_bypass_filters_and_changed_endpoints_hear_of_it_first() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let pool = &context.app_pool;
    let signer = Arc::new(TestSigner::fixed());
    let account = Uuid::new_v4();
    let start = || {
        ReferenceReceiver::start(
            signer.verifying_key(account),
            StatusCode::OK,
            StdDuration::ZERO,
        )
    };
    let (first, moved, second) = (start().await?, start().await?, start().await?);
    seed_account(pool, account, "").await?;
    let scope = topup::tenancy::Scope::new(account, true);
    let actor = merchant_actor();
    let create = |url: String, events: Vec<String>| async move {
        topup::webhook_endpoints::create(
            pool,
            scope,
            &topup::webhook_endpoints::NewEndpoint {
                url: &url,
                enabled_events: &events,
                description: None,
                metadata: Default::default(),
            },
            &merchant_actor(),
        )
        .await
    };
    let a = create(first.url.clone(), vec!["deposit.credited".to_owned()]).await?;
    let b = create(second.url.clone(), vec!["refund.failed".to_owned()]).await?;
    let delivery = configured_worker(pool, Arc::clone(&signer), test_config())?;
    drain(&delivery).await?;
    // Subscribed to neither, both hear of every endpoint created after them.
    ensure!(types(&first).await == ["webhook_endpoint.created"; 2]);
    ensure!(types(&second).await == ["webhook_endpoint.created"]);
    let key = topup::api_keys::create(pool, scope, "rotated", &actor, "").await;
    ensure!(key.is_ok(), "api key creation failed");
    drain(&delivery).await?;
    ensure!(types(&first).await.last().map(String::as_str) == Some("api_key.created"));
    ensure!(types(&second).await.last().map(String::as_str) == Some("api_key.created"));

    // A new URL: the notice goes where the endpoint listened, not to the new URL.
    let changes = topup::webhook_endpoints::Changes {
        url: Some(&moved.url),
        ..Default::default()
    };
    topup::webhook_endpoints::update(pool, scope, a.id, &changes, &actor).await?;
    drain(&delivery).await?;
    ensure!(types(&first).await.last().map(String::as_str) == Some("webhook_endpoint.updated"));
    ensure!(moved.count().await == 0);
    let notice = first.bodies().await.pop().context("missing notice")?;
    let notice: Value = serde_json::from_slice(&notice)?;
    ensure!(notice["data"]["object"]["url"] == moved.url.as_str());
    ensure!(
        notice["data"]["previous_attributes"]["url"] == first.url.as_str(),
        "{notice}"
    );
    ensure!(notice["actor"] == actor.id.as_str());

    // Disabling: the endpoint still hears of it, at its URL; then it hears nothing more.
    let disable = topup::webhook_endpoints::Changes {
        disabled: Some(true),
        ..Default::default()
    };
    topup::webhook_endpoints::update(pool, scope, a.id, &disable, &actor).await?;
    drain(&delivery).await?;
    ensure!(types(&moved).await == ["webhook_endpoint.updated"]);

    // Deletion: the deleted endpoint hears of it; then nothing more reaches it.
    topup::webhook_endpoints::delete(pool, scope, b.id, &actor).await?;
    drain(&delivery).await?;
    ensure!(types(&second).await.last().map(String::as_str) == Some("webhook_endpoint.deleted"));
    let deleted: Value =
        serde_json::from_slice(&second.bodies().await.pop().context("missing deletion")?)?;
    ensure!(deleted["data"]["object"]["deleted"] == true);
    let (first_count, moved_count, second_count) = (
        first.count().await,
        moved.count().await,
        second.count().await,
    );
    seed_account_event(pool, account, Uuid::new_v4()).await?;
    topup::webhook_endpoints::update(pool, scope, a.id, &changes, &actor)
        .await
        .map(|_| ())?;
    drain(&delivery).await?;
    ensure!(first.count().await == first_count);
    ensure!(moved.count().await == moved_count && second.count().await == second_count);

    for receiver in [first, moved, second] {
        receiver.stop().await;
    }
    context.cleanup().await
}

/// A slow endpoint holds at most 4 deliveries in flight and never delays another endpoint.
#[tokio::test]
async fn a_slow_endpoint_does_not_delay_another() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let pool = &context.app_pool;
    let signer = Arc::new(TestSigner::fixed());
    let account = Uuid::new_v4();
    let slow = ReferenceReceiver::start(
        signer.verifying_key(account),
        StatusCode::OK,
        StdDuration::from_secs(5),
    )
    .await?;
    let fast = ReferenceReceiver::start(
        signer.verifying_key(account),
        StatusCode::OK,
        StdDuration::ZERO,
    )
    .await?;
    let account_id = seed_account(pool, account, &slow.url).await?;
    add_endpoint(pool, account_id, true, &fast.url).await?;
    for _ in 0..8 {
        seed_account_event(pool, account_id, Uuid::new_v4()).await?;
    }
    let delivery = configured_worker(
        pool,
        signer,
        DeliveryConfig {
            request_timeout: StdDuration::from_secs(10),
            ..test_config()
        },
    )?;
    let shutdown = tokio_util::sync::CancellationToken::new();
    let running = {
        let shutdown = shutdown.clone();
        tokio::spawn(async move { delivery.run(shutdown).await })
    };

    // Every fast delivery lands while the slow endpoint's first four are still in flight.
    wait_until_count(&fast, 8, StdDuration::from_secs(4)).await?;
    ensure!(slow.count().await <= 4);
    wait_until_count(&slow, 8, StdDuration::from_secs(20)).await?;
    ensure!(
        slow.maximum_in_flight() == 4,
        "{}",
        slow.maximum_in_flight()
    );
    shutdown.cancel();
    running.await?;

    slow.stop().await;
    fast.stop().await;
    context.cleanup().await
}

/// A failing endpoint is retried forever with backoff capped at an hour and never disabled: with
/// no email channel, a disabled endpoint would drop a credit silently (owner decision, design
/// §11). Nothing is announced and its other deliveries stay pending.
#[tokio::test]
async fn a_failing_endpoint_is_retried_forever_and_never_disabled() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let pool = &context.app_pool;
    let signer = Arc::new(TestSigner::fixed());
    let account = Uuid::new_v4();
    let failing = ReferenceReceiver::start(
        signer.verifying_key(account),
        StatusCode::SERVICE_UNAVAILABLE,
        StdDuration::ZERO,
    )
    .await?;
    let healthy = ReferenceReceiver::start(
        signer.verifying_key(account),
        StatusCode::OK,
        StdDuration::ZERO,
    )
    .await?;
    let account_id = seed_account(pool, account, &failing.url).await?;
    add_endpoint(pool, account_id, true, &healthy.url).await?;
    let failing_id = endpoint_id(pool, &failing.url).await?;
    let event_id = Uuid::new_v4();
    seed_account_event(pool, account_id, event_id).await?;
    let delivery = configured_worker(pool, signer, test_config())?;
    ensure!(delivery.run_once().await? == 2);

    // Weeks of failures later, the delivery is still retried within the hour.
    sqlx::query(
        "UPDATE webhook_deliveries SET attempts = 500, next_attempt_at = now() \
         WHERE endpoint_id = $1",
    )
    .bind(failing_id)
    .execute(pool)
    .await?;
    sqlx::query("UPDATE events SET created = now() - interval '30 days' WHERE id = $1")
        .bind(event_id)
        .execute(pool)
        .await?;
    let before = Utc::now();
    ensure!(delivery.run_once().await? == 1);
    let row = sqlx::query(
        "SELECT endpoint.status, endpoint.disabled_reason, delivery.failed_at, \
         delivery.attempts, delivery.next_attempt_at \
         FROM webhook_endpoints AS endpoint JOIN webhook_deliveries AS delivery \
         ON delivery.endpoint_id = endpoint.id WHERE endpoint.id = $1 AND delivery.event_id = $2",
    )
    .bind(failing_id)
    .bind(event_id)
    .fetch_one(pool)
    .await?;
    ensure!(row.try_get::<String, _>("status")? == "enabled");
    ensure!(
        row.try_get::<Option<String>, _>("disabled_reason")?
            .is_none()
    );
    ensure!(
        row.try_get::<Option<chrono::DateTime<Utc>>, _>("failed_at")?
            .is_none()
    );
    ensure!(row.try_get::<i32, _>("attempts")? == 501);
    let next: chrono::DateTime<Utc> = row.try_get("next_attempt_at")?;
    ensure!(
        next <= before + Duration::hours(1) + Duration::seconds(5),
        "{next}"
    );
    let announced: i64 =
        sqlx::query_scalar("SELECT count(*) FROM events WHERE type = 'webhook_endpoint.updated'")
            .fetch_one(pool)
            .await?;
    ensure!(announced == 0);
    ensure!(types(&healthy).await == ["deposit.credited"]);
    ensure!(failing.count().await == 2);

    failing.stop().await;
    healthy.stop().await;
    context.cleanup().await
}

/// A dead endpoint's cost is bounded whatever its queue: once it fails it cools down and is then
/// probed one delivery at a time, while another endpoint's deliveries all go through.
#[tokio::test]
async fn a_dead_endpoint_is_probed_one_delivery_at_a_time() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let pool = &context.app_pool;
    let signer = Arc::new(TestSigner::fixed());
    let account = Uuid::new_v4();
    let dead = ReferenceReceiver::start(
        signer.verifying_key(account),
        StatusCode::INTERNAL_SERVER_ERROR,
        StdDuration::ZERO,
    )
    .await?;
    let healthy = ReferenceReceiver::start(
        signer.verifying_key(account),
        StatusCode::OK,
        StdDuration::ZERO,
    )
    .await?;
    let account_id = seed_account(pool, account, &dead.url).await?;
    add_endpoint(pool, account_id, true, &healthy.url).await?;
    let dead_id = endpoint_id(pool, &dead.url).await?;
    for _ in 0..20 {
        seed_account_event(pool, account_id, Uuid::new_v4()).await?;
    }
    let delivery = configured_worker(pool, signer, test_config())?;
    let shutdown = tokio_util::sync::CancellationToken::new();
    let running = {
        let shutdown = shutdown.clone();
        tokio::spawn(async move { delivery.run(shutdown).await })
    };
    wait_until_count(&healthy, 20, StdDuration::from_secs(15)).await?;
    sleep(StdDuration::from_secs(1)).await;
    shutdown.cancel();
    running.await?;

    // The first claim sends at most its 4 slots; then cooldowns (full jitter up to 30 s, then up
    // to 60 s, doubling to an hour) hold it to single probes. Its other deliveries wait unattempted.
    ensure!(dead.count().await <= 8, "{}", dead.count().await);
    ensure!(dead.maximum_in_flight() <= 4);
    let untouched: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM webhook_deliveries WHERE endpoint_id = $1 AND attempts = 0",
    )
    .bind(dead_id)
    .fetch_one(pool)
    .await?;
    ensure!(untouched >= 12, "{untouched}");
    let status: String = sqlx::query_scalar("SELECT status FROM webhook_endpoints WHERE id = $1")
        .bind(dead_id)
        .fetch_one(pool)
        .await?;
    ensure!(status == "enabled");

    dead.stop().await;
    healthy.stop().await;
    context.cleanup().await
}

/// `410 Gone` disables the endpoint at once.
#[tokio::test]
async fn gone_disables_the_endpoint_at_once() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let pool = &context.app_pool;
    let signer = Arc::new(TestSigner::fixed());
    let account = Uuid::new_v4();
    let gone = ReferenceReceiver::start(
        signer.verifying_key(account),
        StatusCode::GONE,
        StdDuration::ZERO,
    )
    .await?;
    let first_event = Uuid::new_v4();
    let account_id = seed_event(pool, account, &gone.url, first_event).await?;
    let second_event = Uuid::new_v4();
    seed_account_event(pool, account_id, second_event).await?;
    let gone_id = endpoint_id(pool, &gone.url).await?;

    ensure!(worker(pool, signer)?.run_once().await? == 1);
    let (status, reason): (String, Option<String>) =
        sqlx::query_as("SELECT status, disabled_reason FROM webhook_endpoints WHERE id = $1")
            .bind(gone_id)
            .fetch_one(pool)
            .await?;
    ensure!(status == "disabled" && reason.as_deref() == Some("gone"));
    // Its other pending delivery stops too; both stay readable and resendable.
    let stopped: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM webhook_deliveries WHERE endpoint_id = $1 AND failed_at IS NOT NULL",
    )
    .bind(gone_id)
    .fetch_one(pool)
    .await?;
    ensure!(stopped == 2);
    ensure!(gone.count().await == 1);

    gone.stop().await;
    context.cleanup().await
}

/// A resend delivers the event again with the same `webhook-id` and body, to an endpoint that
/// got it, and to one that was never sent it; a disabled endpoint is refused.
#[tokio::test]
async fn a_resend_redelivers_the_same_event() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let pool = &context.app_pool;
    let signer = Arc::new(TestSigner::fixed());
    let account = Uuid::new_v4();
    let start = || {
        ReferenceReceiver::start(
            signer.verifying_key(account),
            StatusCode::OK,
            StdDuration::ZERO,
        )
    };
    let (subscribed, other) = (start().await?, start().await?);
    let event_id = Uuid::new_v4();
    let account_id = seed_event(pool, account, &subscribed.url, event_id).await?;
    let other_id =
        add_subscribed_endpoint(pool, account_id, &other.url, &["refund.failed"]).await?;
    let subscribed_id = endpoint_id(pool, &subscribed.url).await?;
    let delivery = configured_worker(pool, signer, test_config())?;
    drain(&delivery).await?;

    let scope = topup::tenancy::Scope::new(account_id, true);
    let actor = merchant_actor();
    for endpoint in [subscribed_id, other_id] {
        topup::webhook_endpoints::resend(pool, scope, event_id, endpoint, &actor).await?;
    }
    drain(&delivery).await?;
    ensure!(subscribed.ids().await == vec![evt(event_id), evt(event_id)]);
    ensure!(other.ids().await == vec![evt(event_id)]);
    let bodies = subscribed.bodies().await;
    ensure!(bodies.first() == bodies.get(1) && bodies.first() == other.bodies().await.first());

    sqlx::query("UPDATE webhook_endpoints SET status = 'disabled' WHERE id = $1")
        .bind(other_id)
        .execute(pool)
        .await?;
    let refused = topup::webhook_endpoints::resend(pool, scope, event_id, other_id, &actor).await;
    ensure!(matches!(
        refused,
        Err(topup::webhook_endpoints::EndpointError::Disabled)
    ));
    let elsewhere = topup::tenancy::Scope::new(Uuid::new_v4(), true);
    let foreign =
        topup::webhook_endpoints::resend(pool, elsewhere, event_id, subscribed_id, &actor).await;
    ensure!(matches!(
        foreign,
        Err(topup::webhook_endpoints::EndpointError::EventNotFound)
    ));

    subscribed.stop().await;
    other.stop().await;
    context.cleanup().await
}

/// With a proxy configured, every delivery goes through it: the proxy, not the service, decides
/// which addresses a URL may reach.
#[tokio::test]
async fn deliveries_go_through_the_configured_proxy() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let pool = &context.app_pool;
    let signer = Arc::new(TestSigner::fixed());
    let account = Uuid::new_v4();
    // The reference receiver stands in for the proxy: it gets the absolute-form request.
    let proxy = ReferenceReceiver::start(
        signer.verifying_key(account),
        StatusCode::OK,
        StdDuration::ZERO,
    )
    .await?;
    let event_id = Uuid::new_v4();
    seed_event(pool, account, "http://merchant.invalid/webhooks", event_id).await?;
    let proxy_url = proxy.url.trim_end_matches("/webhooks").parse()?;
    let delivery = configured_worker(
        pool,
        signer,
        DeliveryConfig {
            proxy: Some(proxy_url),
            ..test_config()
        },
    )?;

    ensure!(delivery.run_once().await? == 1);
    ensure!(proxy.ids().await == vec![evt(event_id)]);

    proxy.stop().await;
    context.cleanup().await
}

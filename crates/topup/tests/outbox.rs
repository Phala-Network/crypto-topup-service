//! PostgreSQL and reference-receiver tests for Standard Webhooks delivery.

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
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool, Row};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio::time::sleep;
use topup::db::{self, NewOutboxEvent, NewProduct};
use topup::outbox::{DeliveryConfig, DeliveryWorker, SignedWebhook};
use topup_core::{
    Ed25519PublicKey, Ed25519Signature, SignedTx, Signer as CoreSigner, SignerError, TxRequest,
};
use url::Url;
use uuid::Uuid;

/// Waits out transient `max_connections` exhaustion when many test databases share one
/// server under load; sqlx's 30 s default turns that into spurious `PoolTimedOut` failures.
const DB_ACQUIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

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
    async fn sign_operator_tx(&self, _tx: TxRequest) -> Result<SignedTx, SignerError> {
        Err(SignerError::SigningFailed)
    }

    async fn sign_settlement(&self, content: &[u8]) -> Result<Ed25519Signature, SignerError> {
        Ok(Ed25519Signature(self.0.sign(content).to_bytes()))
    }

    async fn operator_address(&self) -> Result<alloy_primitives::Address, SignerError> {
        Err(SignerError::KeyUnavailable)
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

struct TestContext {
    admin_pool: PgPool,
    owner_pool: PgPool,
    app_pool: PgPool,
    database_name: String,
    app_role: String,
    app_url: String,
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
        sqlx::query("SELECT pg_advisory_lock(704_201_013)")
            .execute(&admin_pool)
            .await?;

        let suffix = Uuid::new_v4().simple().to_string();
        let database_name = format!("topup_c13_{suffix}");
        let app_role = format!("topup_c13_app_{suffix}");
        let password = format!("c13_{suffix}");
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
            .map_err(|()| anyhow::anyhow!("DATABASE_URL cannot accept an application username"))?;
        app_url
            .set_password(Some(&password))
            .map_err(|()| anyhow::anyhow!("DATABASE_URL cannot accept an application password"))?;
        app_url.set_path(&format!("/{database_name}"));
        let app_url = app_url.to_string();
        let app_pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(DB_ACQUIRE_TIMEOUT)
            .connect(&app_url)
            .await?;
        sqlx::query("SELECT pg_advisory_unlock(704_201_013)")
            .execute(&admin_pool)
            .await?;

        Ok(Some(Self {
            admin_pool,
            owner_pool,
            app_pool,
            database_name,
            app_role,
            app_url,
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
    match env::var(name).ok().filter(|value| !value.is_empty()) {
        Some(value) => Some(value),
        None => {
            eprintln!("skipping outbox integration test: {name} is not set");
            None
        }
    }
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

async fn seed_product(pool: &PgPool, webhook_url: &str) -> Result<Uuid> {
    let product_id = Uuid::new_v4();
    db::create_product(
        pool,
        &NewProduct {
            id: product_id,
            slug: format!("product-{product_id}"),
            settlement_url: "https://product.test/settlements".to_owned(),
            webhook_url: webhook_url.to_owned(),
            pubkey: "product-public-key".to_owned(),
            kid: "product/v1".to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    Ok(product_id)
}

async fn seed_product_event(pool: &PgPool, product_id: Uuid, event_id: Uuid) -> Result<()> {
    db::enqueue(
        pool,
        &NewOutboxEvent {
            id: event_id,
            event_type: "deposit.confirmed".to_owned(),
            payload: json!({
                "product_id": product_id,
                "deposit_id": Uuid::new_v4(),
            }),
            next_attempt_at: Utc::now() - Duration::seconds(1),
        },
    )
    .await?;
    Ok(())
}

async fn seed_event(pool: &PgPool, webhook_url: &str, event_id: Uuid) -> Result<Uuid> {
    let product_id = seed_product(pool, webhook_url).await?;
    seed_product_event(pool, product_id, event_id).await?;
    Ok(product_id)
}

#[tokio::test]
async fn reference_receiver_rejects_tampering_and_stale_timestamps() -> Result<()> {
    let signer = TestSigner::fixed();
    let receiver =
        ReferenceReceiver::start(signer.verifying_key(), StatusCode::OK, StdDuration::ZERO).await?;
    let client = reqwest::Client::new();
    let event_id = Uuid::new_v4();
    let body = br#"{"type":"deposit.confirmed","data":{}}"#;

    let signed = SignedWebhook::new(&signer, event_id, Utc::now().timestamp(), body).await?;
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
    let stale = SignedWebhook::new(&signer, event_id, stale_timestamp, body).await?;
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
    let Some(context) = TestContext::create().await? else {
        return Ok(());
    };
    let signer = Arc::new(TestSigner::fixed());
    let receiver =
        ReferenceReceiver::start(signer.verifying_key(), StatusCode::OK, StdDuration::ZERO).await?;
    let event_id = Uuid::new_v4();
    seed_event(&context.app_pool, &receiver.url, event_id).await?;

    ensure!(worker(&context.app_pool, signer)?.run_once().await? == 1);
    let row = sqlx::query("SELECT delivered_at, attempts, response FROM outbox WHERE id = $1")
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
    ensure!(envelope["event_id"] == event_id.to_string());
    ensure!(envelope["type"] == "deposit.confirmed");

    receiver.stop().await;
    context.cleanup().await
}

#[tokio::test]
async fn server_error_increments_attempts_and_schedules_backoff() -> Result<()> {
    let Some(context) = TestContext::create().await? else {
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
        "SELECT delivered_at, attempts, next_attempt_at, response FROM outbox WHERE id = $1",
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
    let Some(context) = TestContext::create().await? else {
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
async fn cancelled_delivery_releases_the_product_lock() -> Result<()> {
    let Some(context) = TestContext::create().await? else {
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
        sqlx::query_scalar("SELECT delivered_at FROM outbox WHERE id = $1")
            .bind(event_id)
            .fetch_one(&context.app_pool)
            .await?;
    ensure!(delivered_at.is_some());

    receiver.stop().await;
    context.cleanup().await
}

#[tokio::test]
async fn different_events_for_one_product_are_delivered_sequentially() -> Result<()> {
    let Some(context) = TestContext::create().await? else {
        return Ok(());
    };
    let signer = Arc::new(TestSigner::fixed());
    let receiver = ReferenceReceiver::start(
        signer.verifying_key(),
        StatusCode::OK,
        StdDuration::from_millis(150),
    )
    .await?;
    let product_id = seed_product(&context.app_pool, &receiver.url).await?;
    let first_event = Uuid::new_v4();
    let second_event = Uuid::new_v4();
    seed_product_event(&context.app_pool, product_id, first_event).await?;
    seed_product_event(&context.app_pool, product_id, second_event).await?;
    let first = worker(&context.app_pool, Arc::clone(&signer))?;
    let second = worker(&context.app_pool, Arc::clone(&signer))?;

    let (first_count, second_count) = tokio::join!(first.run_once(), second.run_once());
    ensure!(first_count? + second_count? == 2);
    sqlx::query(
        "UPDATE outbox SET next_attempt_at = now() - interval '1 second' WHERE delivered_at IS NULL",
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
    let Some(context) = TestContext::create().await? else {
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
    let row = sqlx::query("SELECT delivered_at, attempts, response FROM outbox WHERE id = $1")
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
    let Some(context) = TestContext::create().await? else {
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
    let row = sqlx::query("SELECT attempts, response FROM outbox WHERE id = $1")
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
    let Some(context) = TestContext::create().await? else {
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
    sqlx::query("UPDATE outbox SET next_attempt_at = now() - interval '1 second' WHERE id = $1")
        .bind(event_id)
        .execute(&context.app_pool)
        .await?;
    ensure!(delivery.run_once().await? == 1);
    ensure!(receiver.ids().await == vec![event_id.to_string(), event_id.to_string()]);
    let mut bodies = receiver.bodies().await.into_iter();
    let first_body = bodies.next().context("missing failed delivery body")?;
    let second_body = bodies.next().context("missing successful retry body")?;
    ensure!(first_body == second_body);
    ensure!(bodies.next().is_none());
    let row = sqlx::query("SELECT delivered_at, attempts FROM outbox WHERE id = $1")
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
    let Some(context) = TestContext::create().await? else {
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
        .args(["outbox", "replay", "--id", &event_id.to_string(), "--force"])
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
    ensure!(receiver.ids().await == vec![event_id.to_string(), event_id.to_string()]);
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

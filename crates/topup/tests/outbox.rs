//! PostgreSQL and reference-receiver tests for Standard Webhooks delivery.

use std::env;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use anyhow::{Context, Result, ensure};
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
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
use topup::outbox::{DeliveryConfig, DeliveryWorker, EventSignError, EventSigner, SignedWebhook};
use url::Url;
use uuid::Uuid;

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

impl EventSigner for TestSigner {
    fn sign_event(&self, content: &[u8]) -> Result<[u8; 64], EventSignError> {
        Ok(self.0.sign(content).to_bytes())
    }
}

#[derive(Clone, Debug)]
struct ReceivedWebhook {
    id: String,
    body: Value,
}

#[derive(Clone)]
struct ReceiverState {
    verifying_key: VerifyingKey,
    status: StatusCode,
    delay: StdDuration,
    received: Arc<Mutex<Vec<ReceivedWebhook>>>,
}

struct ReferenceReceiver {
    url: String,
    received: Arc<Mutex<Vec<ReceivedWebhook>>>,
    task: JoinHandle<()>,
}

impl ReferenceReceiver {
    async fn start(
        verifying_key: VerifyingKey,
        status: StatusCode,
        delay: StdDuration,
    ) -> Result<Self> {
        let received = Arc::new(Mutex::new(Vec::new()));
        let state = ReceiverState {
            verifying_key,
            status,
            delay,
            received: Arc::clone(&received),
        };
        let app = Router::new()
            .route("/webhooks", post(reference_webhook))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            url: format!("http://{address}/webhooks"),
            received,
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
            .map(|delivery| delivery.body.clone())
    }

    async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

async fn reference_webhook(
    State(state): State<ReceiverState>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, &'static str) {
    let id = match verify_standard_webhook(
        &state.verifying_key,
        &headers,
        &body,
        Utc::now().timestamp(),
    ) {
        Ok(id) => id,
        Err(()) => return (StatusCode::BAD_REQUEST, "invalid signature"),
    };
    let Ok(body) = serde_json::from_slice(&body) else {
        return (StatusCode::BAD_REQUEST, "invalid JSON");
    };
    if !state.delay.is_zero() {
        sleep(state.delay).await;
    }
    state
        .received
        .lock()
        .await
        .push(ReceivedWebhook { id, body });
    (state.status, "receiver-response-body")
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
    DeliveryWorker::new(
        pool.clone(),
        signer,
        DeliveryConfig {
            batch_size: 1,
            request_timeout: StdDuration::from_secs(2),
            claim_lease: StdDuration::from_secs(30),
            poll_interval: StdDuration::from_millis(10),
            response_body_limit: 16,
            age_alert_threshold: StdDuration::from_secs(60),
        },
    )
    .map_err(Into::into)
}

async fn seed_event(pool: &PgPool, webhook_url: &str, event_id: Uuid) -> Result<Uuid> {
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

    let signed = SignedWebhook::new(&signer, event_id, Utc::now().timestamp(), body)?;
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
    let stale = SignedWebhook::new(&signer, event_id, stale_timestamp, body)?;
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
    ensure!(next_attempt_at >= before + Duration::seconds(29));
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

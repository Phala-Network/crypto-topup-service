use std::error::Error;
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde_json::{Value, json};
use sqlx::postgres::PgPool;
use sqlx::{PgConnection, Row};
use tokio::sync::watch;
use tokio::time::sleep;
use topup_core::retry::backoff;
use uuid::Uuid;

use super::{EventEnvelope, EventSigner, SignedWebhook};

const MAX_POSTGRES_INTERVAL_SECONDS: u64 = i32::MAX as u64;

/// Runtime limits for the outbox delivery loop.
#[derive(Clone, Debug)]
pub struct DeliveryConfig {
    /// Maximum number of due rows reserved by one polling pass.
    pub batch_size: u32,
    /// Complete HTTP request timeout, including reading the response body.
    pub request_timeout: Duration,
    /// Reservation duration; this must exceed the request timeout.
    pub claim_lease: Duration,
    /// Delay between empty polls or database failures.
    pub poll_interval: Duration,
    /// Maximum response body bytes retained in `outbox.response`.
    pub response_body_limit: usize,
    /// Pending age after which every claimed event emits a warning.
    pub age_alert_threshold: Duration,
}

impl Default for DeliveryConfig {
    fn default() -> Self {
        Self {
            batch_size: 16,
            request_timeout: Duration::from_secs(20),
            claim_lease: Duration::from_secs(5 * 60),
            poll_interval: Duration::from_secs(1),
            response_body_limit: 4 * 1024,
            age_alert_threshold: Duration::from_secs(24 * 60 * 60),
        }
    }
}

/// Failure to configure or access the delivery repository.
#[derive(Debug)]
pub enum DeliveryError {
    /// Configuration is internally inconsistent.
    InvalidConfig(&'static str),
    /// The HTTP client could not be constructed.
    Client(reqwest::Error),
    /// PostgreSQL could not claim or persist an event.
    Database(sqlx::Error),
    /// A session-level product lock could not be released normally.
    AdvisoryUnlock,
}

impl Display for DeliveryError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidConfig(message) => write!(formatter, "invalid delivery config: {message}"),
            Self::Client(error) => write!(formatter, "failed to build webhook client: {error}"),
            Self::Database(error) => write!(formatter, "outbox database operation failed: {error}"),
            Self::AdvisoryUnlock => formatter.write_str("failed to release product delivery lock"),
        }
    }
}

impl Error for DeliveryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Client(error) => Some(error),
            Self::Database(error) => Some(error),
            Self::InvalidConfig(_) | Self::AdvisoryUnlock => None,
        }
    }
}

impl From<sqlx::Error> for DeliveryError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

#[derive(Clone, Debug)]
struct ClaimedEvent {
    id: Uuid,
    event_type: String,
    payload: Value,
    attempts: i32,
    created_at: DateTime<Utc>,
    claim_until: DateTime<Utc>,
}

/// PostgreSQL-backed Standard Webhooks sender.
pub struct DeliveryWorker<S> {
    pool: PgPool,
    client: Client,
    signer: Arc<S>,
    config: DeliveryConfig,
}

impl<S> DeliveryWorker<S>
where
    S: EventSigner,
{
    /// Builds a worker with redirects disabled and a bounded request timeout.
    pub fn new(
        pool: PgPool,
        signer: Arc<S>,
        config: DeliveryConfig,
    ) -> Result<Self, DeliveryError> {
        validate_config(&config)?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(config.request_timeout)
            .build()
            .map_err(DeliveryError::Client)?;
        Ok(Self {
            pool,
            client,
            signer,
            config,
        })
    }

    /// Polls until shutdown, retaining failed events for unlimited retries.
    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) {
        loop {
            if *shutdown.borrow() {
                return;
            }

            let should_pause = match self.run_once().await {
                Ok(claimed) => claimed == 0,
                Err(error) => {
                    tracing::error!(%error, "outbox delivery poll failed");
                    true
                }
            };

            if should_pause {
                tokio::select! {
                    () = sleep(self.config.poll_interval) => {}
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Claims one small batch and attempts each event once.
    pub async fn run_once(&self) -> Result<usize, DeliveryError> {
        let events = claim_due(&self.pool, &self.config).await?;
        let count = events.len();
        for event in events {
            self.warn_if_old(&event);
            self.deliver_claimed(event).await?;
        }
        Ok(count)
    }

    fn warn_if_old(&self, event: &ClaimedEvent) {
        let Ok(threshold) = chrono::Duration::from_std(self.config.age_alert_threshold) else {
            return;
        };
        let age = Utc::now().signed_duration_since(event.created_at);
        if age > threshold {
            tracing::warn!(
                event_id = %event.id,
                event_type = event.event_type,
                age_seconds = age.num_seconds(),
                threshold_seconds = threshold.num_seconds(),
                "outbox event exceeded the delivery age threshold"
            );
        }
    }

    async fn deliver_claimed(&self, event: ClaimedEvent) -> Result<(), DeliveryError> {
        let product_id = match product_id(&event.payload) {
            Ok(product_id) => product_id,
            Err(message) => {
                record_failure(
                    &self.pool,
                    &event,
                    None,
                    None,
                    message,
                    self.retry_delay(&event),
                )
                .await?;
                return Ok(());
            }
        };

        let mut connection = self.pool.acquire().await?;
        let lock_key = product_id.to_string();
        let locked =
            sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock(hashtextextended($1, 0))")
                .bind(&lock_key)
                .fetch_one(&mut *connection)
                .await?;
        if !locked {
            release_claim(&mut connection, &event).await?;
            return Ok(());
        }

        let result = self
            .deliver_with_product_lock(&mut connection, &event, product_id)
            .await;
        let unlock_result =
            sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock(hashtextextended($1, 0))")
                .bind(&lock_key)
                .fetch_one(&mut *connection)
                .await;

        match unlock_result {
            Ok(true) => result,
            Ok(false) => {
                let _ = connection.close().await;
                result?;
                Err(DeliveryError::AdvisoryUnlock)
            }
            Err(unlock_error) => {
                let _ = connection.close().await;
                match result {
                    Ok(()) => Err(DeliveryError::Database(unlock_error)),
                    Err(delivery_error) => Err(delivery_error),
                }
            }
        }
    }

    async fn deliver_with_product_lock(
        &self,
        connection: &mut PgConnection,
        event: &ClaimedEvent,
        product_id: Uuid,
    ) -> Result<(), DeliveryError> {
        let webhook_url =
            sqlx::query_scalar::<_, String>("SELECT webhook_url FROM products WHERE id = $1")
                .bind(product_id)
                .fetch_optional(&mut *connection)
                .await?;
        let Some(webhook_url) = webhook_url else {
            record_failure_on(
                connection,
                event,
                None,
                None,
                "product_not_found",
                self.retry_delay(event),
            )
            .await?;
            return Ok(());
        };

        let envelope = EventEnvelope {
            event_id: event.id,
            event_type: event.event_type.clone(),
            created_at: event.created_at,
            data: event.payload.clone(),
        };
        let body = match serde_json::to_vec(&envelope) {
            Ok(body) => body,
            Err(_) => {
                record_failure_on(
                    connection,
                    event,
                    None,
                    None,
                    "envelope_serialization",
                    self.retry_delay(event),
                )
                .await?;
                return Ok(());
            }
        };
        let signed = match SignedWebhook::new(
            self.signer.as_ref(),
            event.id,
            Utc::now().timestamp(),
            &body,
        ) {
            Ok(signed) => signed,
            Err(_) => {
                record_failure_on(
                    connection,
                    event,
                    None,
                    None,
                    "signing_failed",
                    self.retry_delay(event),
                )
                .await?;
                return Ok(());
            }
        };

        let response = self
            .client
            .post(webhook_url)
            .header("content-type", "application/json")
            .header("webhook-id", &signed.id)
            .header("webhook-timestamp", &signed.timestamp)
            .header("webhook-signature", &signed.signature)
            .body(body)
            .send()
            .await;

        let response = match response {
            Ok(response) => response,
            Err(error) => {
                record_failure_on(
                    connection,
                    event,
                    None,
                    None,
                    request_error_code(&error),
                    self.retry_delay(event),
                )
                .await?;
                return Ok(());
            }
        };

        let status = response.status();
        let response_body = response.bytes().await;
        let (body, body_error) = match response_body {
            Ok(bytes) => (
                Some(truncate_body(&bytes, self.config.response_body_limit)),
                None,
            ),
            Err(_) => (None, Some("response_body_read")),
        };

        if status.is_success() {
            let stored = response_value(Some(status.as_u16()), body, body_error);
            mark_delivered(connection, event, &stored).await?;
        } else {
            record_failure_on(
                connection,
                event,
                Some(status.as_u16()),
                body,
                body_error.unwrap_or("non_2xx_status"),
                self.retry_delay(event),
            )
            .await?;
        }
        Ok(())
    }

    fn retry_delay(&self, event: &ClaimedEvent) -> Duration {
        let attempt = u32::try_from(event.attempts).unwrap_or(u32::MAX);
        backoff(attempt, event_jitter(event.id))
    }
}

fn validate_config(config: &DeliveryConfig) -> Result<(), DeliveryError> {
    if config.batch_size == 0 {
        return Err(DeliveryError::InvalidConfig("batch_size must be positive"));
    }
    if config.request_timeout.is_zero() {
        return Err(DeliveryError::InvalidConfig(
            "request_timeout must be positive",
        ));
    }
    if config.claim_lease <= config.request_timeout {
        return Err(DeliveryError::InvalidConfig(
            "claim_lease must exceed request_timeout",
        ));
    }
    if config.claim_lease.as_secs() > MAX_POSTGRES_INTERVAL_SECONDS {
        return Err(DeliveryError::InvalidConfig("claim_lease is too large"));
    }
    Ok(())
}

async fn claim_due(
    pool: &PgPool,
    config: &DeliveryConfig,
) -> Result<Vec<ClaimedEvent>, sqlx::Error> {
    let batch_size = i64::from(config.batch_size);
    let lease_seconds = i32::try_from(config.claim_lease.as_secs()).unwrap_or(i32::MAX);
    let rows = sqlx::query(
        r#"
        WITH candidates AS (
            SELECT id
            FROM outbox
            WHERE delivered_at IS NULL AND next_attempt_at <= now()
            ORDER BY next_attempt_at, id
            FOR UPDATE SKIP LOCKED
            LIMIT $1
        )
        UPDATE outbox AS event
        SET next_attempt_at = now() + make_interval(secs => $2)
        FROM candidates
        WHERE event.id = candidates.id
        RETURNING event.id, event.event_type, event.payload, event.attempts,
                  event.created_at, event.next_attempt_at
        "#,
    )
    .bind(batch_size)
    .bind(lease_seconds)
    .fetch_all(pool)
    .await?;

    rows.into_iter()
        .map(|row| {
            Ok(ClaimedEvent {
                id: row.try_get("id")?,
                event_type: row.try_get("event_type")?,
                payload: row.try_get("payload")?,
                attempts: row.try_get("attempts")?,
                created_at: row.try_get("created_at")?,
                claim_until: row.try_get("next_attempt_at")?,
            })
        })
        .collect()
}

fn product_id(payload: &Value) -> Result<Uuid, &'static str> {
    let Some(raw) = payload.get("product_id").and_then(Value::as_str) else {
        return Err("missing_product_id");
    };
    Uuid::parse_str(raw).map_err(|_| "invalid_product_id")
}

async fn release_claim(
    connection: &mut PgConnection,
    event: &ClaimedEvent,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE outbox
        SET next_attempt_at = now()
        WHERE id = $1 AND delivered_at IS NULL AND next_attempt_at = $2
        "#,
    )
    .bind(event.id)
    .bind(event.claim_until)
    .execute(connection)
    .await?;
    Ok(())
}

async fn mark_delivered(
    connection: &mut PgConnection,
    event: &ClaimedEvent,
    response: &Value,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE outbox
        SET delivered_at = now(), response = $3
        WHERE id = $1 AND delivered_at IS NULL AND next_attempt_at = $2
        "#,
    )
    .bind(event.id)
    .bind(event.claim_until)
    .bind(response)
    .execute(connection)
    .await?;
    Ok(())
}

async fn record_failure(
    pool: &PgPool,
    event: &ClaimedEvent,
    status: Option<u16>,
    body: Option<String>,
    error: &'static str,
    delay: Duration,
) -> Result<(), sqlx::Error> {
    let mut connection = pool.acquire().await?;
    record_failure_on(&mut connection, event, status, body, error, delay).await
}

async fn record_failure_on(
    connection: &mut PgConnection,
    event: &ClaimedEvent,
    status: Option<u16>,
    body: Option<String>,
    error: &'static str,
    delay: Duration,
) -> Result<(), sqlx::Error> {
    let delay_seconds = i32::try_from(delay.as_secs()).unwrap_or(i32::MAX);
    let response = response_value(status, body, Some(error));
    sqlx::query(
        r#"
        UPDATE outbox
        SET attempts = CASE WHEN attempts < 2147483647 THEN attempts + 1 ELSE attempts END,
            next_attempt_at = now() + make_interval(secs => $3),
            response = $4
        WHERE id = $1 AND delivered_at IS NULL AND next_attempt_at = $2
        "#,
    )
    .bind(event.id)
    .bind(event.claim_until)
    .bind(delay_seconds)
    .bind(response)
    .execute(connection)
    .await?;
    Ok(())
}

fn response_value(status: Option<u16>, body: Option<String>, error: Option<&str>) -> Value {
    json!({
        "status": status,
        "body": body,
        "error": error,
    })
}

fn truncate_body(body: &[u8], limit: usize) -> String {
    let end = body.len().min(limit);
    String::from_utf8_lossy(&body[..end]).into_owned()
}

fn request_error_code(error: &reqwest::Error) -> &'static str {
    if error.is_timeout() {
        "request_timeout"
    } else if error.is_connect() {
        "connection_failed"
    } else if error.is_request() {
        "request_failed"
    } else if error.is_body() {
        "request_body"
    } else if error.is_builder() {
        "request_builder"
    } else if error.is_redirect() {
        "redirect_rejected"
    } else {
        "transport_error"
    }
}

fn event_jitter(event_id: Uuid) -> u64 {
    let bytes = event_id.as_bytes();
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&bytes[..8]);
    u64::from_be_bytes(prefix)
}

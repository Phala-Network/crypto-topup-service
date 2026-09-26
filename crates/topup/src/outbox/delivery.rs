use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::Client;
use serde_json::{Value, json};
use sqlx::postgres::PgPool;
use sqlx::{PgConnection, Postgres, Row, Transaction};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use topup_core::{Signer, retry::backoff};
use tracing::Instrument as _;
use uuid::Uuid;

use super::{EventEnvelope, SignedWebhook};
use crate::jitter::{JitterSource, OsJitter};

const MAX_POSTGRES_INTERVAL_SECONDS: u64 = i32::MAX as u64;

/// Runtime limits for the outbox delivery loop.
#[derive(Clone, Debug)]
pub struct DeliveryConfig {
    /// Maximum number of due rows reserved by one polling pass.
    pub batch_size: u32,
    /// Complete HTTP request timeout, including reading the response body.
    pub request_timeout: Duration,
    /// Reservation duration; this must exceed the whole batch request timeout.
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
            batch_size: 8,
            request_timeout: Duration::from_secs(20),
            claim_lease: Duration::from_secs(5 * 60),
            poll_interval: Duration::from_secs(1),
            response_body_limit: 4 * 1024,
            age_alert_threshold: Duration::from_secs(24 * 60 * 60),
        }
    }
}

/// Failure to configure or access the delivery repository.
#[derive(Debug, thiserror::Error)]
pub enum DeliveryError {
    /// Configuration is internally inconsistent.
    #[error("invalid delivery config: {0}")]
    InvalidConfig(&'static str),
    /// The HTTP client could not be constructed.
    #[error("failed to build webhook client: {0}")]
    Client(#[source] reqwest::Error),
    /// PostgreSQL could not claim or persist an event.
    #[error("outbox database operation failed: {0}")]
    Database(#[from] sqlx::Error),
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

struct ClaimedDelivery {
    transaction: Transaction<'static, Postgres>,
    event: ClaimedEvent,
    product_id: Result<Uuid, &'static str>,
}

enum ClaimResult {
    Empty,
    Deferred,
    Ready(ClaimedDelivery),
}

/// PostgreSQL-backed Standard Webhooks sender.
pub struct DeliveryWorker<S> {
    pool: PgPool,
    client: Client,
    signer: Arc<S>,
    config: DeliveryConfig,
    entropy: Arc<dyn JitterSource>,
}

impl<S> DeliveryWorker<S>
where
    S: Signer,
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
            entropy: Arc::new(OsJitter),
        })
    }

    /// Polls until shutdown, retaining failed events for unlimited retries.
    pub async fn run(&self, shutdown: CancellationToken) {
        self.run_with_instance("0".to_owned(), shutdown).await;
    }

    /// Polls one named delivery worker until shutdown.
    pub async fn run_with_instance(&self, instance: String, shutdown: CancellationToken) {
        let monitor = crate::observability::CronMonitor::outbox(&instance);
        loop {
            if shutdown.is_cancelled() {
                return;
            }
            monitor.check_in(true);

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
                    () = shutdown.cancelled() => return,
                }
            }
        }
    }

    /// Claims one small batch and attempts each event once.
    pub async fn run_once(&self) -> Result<usize, DeliveryError> {
        let mut claimed = 0_usize;
        for _ in 0..self.config.batch_size {
            match claim_next(&self.pool, &self.config).await? {
                ClaimResult::Empty => break,
                ClaimResult::Deferred => {
                    claimed = claimed.saturating_add(1);
                }
                ClaimResult::Ready(delivery) => {
                    claimed = claimed.saturating_add(1);
                    self.warn_if_old(&delivery.event);
                    let span = crate::observability::outbox_delivery_span(
                        delivery.event.id,
                        &delivery.event.payload,
                        delivery.event.attempts,
                    );
                    self.deliver_claimed(delivery).instrument(span).await?;
                }
            }
        }
        Ok(claimed)
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

    async fn deliver_claimed(&self, delivery: ClaimedDelivery) -> Result<(), DeliveryError> {
        let ClaimedDelivery {
            mut transaction,
            event,
            product_id,
        } = delivery;
        let product_id = match product_id {
            Ok(product_id) => product_id,
            Err(message) => {
                self.record_failure(&mut transaction, &event, None, None, message)
                    .await?;
                transaction.commit().await?;
                return Ok(());
            }
        };

        self.deliver_in_transaction(&mut transaction, &event, product_id)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    async fn deliver_in_transaction(
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
            self.record_failure(connection, event, None, None, "product_not_found")
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
                self.record_failure(connection, event, None, None, "envelope_serialization")
                    .await?;
                return Ok(());
            }
        };
        let signed = match SignedWebhook::new(
            self.signer.as_ref(),
            event.id,
            Utc::now().timestamp(),
            &body,
        )
        .await
        {
            Ok(signed) => signed,
            Err(_) => {
                self.record_failure(connection, event, None, None, "signing_failed")
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
                self.record_failure(connection, event, None, None, request_error_code(&error))
                    .await?;
                return Ok(());
            }
        };

        let status = response.status();
        let (body, body_error) =
            read_response_body(response, self.config.response_body_limit).await;

        if status.is_success() {
            let stored = response_value(Some(status.as_u16()), body, body_error);
            mark_delivered(connection, event, &stored).await?;
        } else {
            self.record_failure(
                connection,
                event,
                Some(status.as_u16()),
                body,
                body_error.unwrap_or("non_2xx_status"),
            )
            .await?;
        }
        Ok(())
    }

    async fn record_failure(
        &self,
        connection: &mut PgConnection,
        event: &ClaimedEvent,
        status: Option<u16>,
        body: Option<String>,
        error: &'static str,
    ) -> Result<(), DeliveryError> {
        let delay = retry_delay(event.attempts, self.entropy.as_ref());
        record_failure_on(connection, event, status, body, error, delay).await?;
        Ok(())
    }
}

fn validate_config(config: &DeliveryConfig) -> Result<(), DeliveryError> {
    if config.batch_size == 0 {
        return Err(DeliveryError::InvalidConfig("batch_size must be positive"));
    }
    if config.batch_size > 100 {
        return Err(DeliveryError::InvalidConfig(
            "batch_size must not exceed 100",
        ));
    }
    if config.request_timeout.is_zero() {
        return Err(DeliveryError::InvalidConfig(
            "request_timeout must be positive",
        ));
    }
    let Some(batch_timeout) = config.request_timeout.checked_mul(config.batch_size) else {
        return Err(DeliveryError::InvalidConfig(
            "batch request timeout is too large",
        ));
    };
    if config.claim_lease <= batch_timeout {
        return Err(DeliveryError::InvalidConfig(
            "claim_lease must exceed the whole batch request timeout",
        ));
    }
    if config.claim_lease.as_secs() > MAX_POSTGRES_INTERVAL_SECONDS {
        return Err(DeliveryError::InvalidConfig("claim_lease is too large"));
    }
    Ok(())
}

async fn claim_next(pool: &PgPool, config: &DeliveryConfig) -> Result<ClaimResult, sqlx::Error> {
    let lease_seconds = i32::try_from(config.claim_lease.as_secs()).unwrap_or(i32::MAX);
    let mut transaction = pool.begin().await?;
    let row = sqlx::query(
        r#"
        WITH candidates AS (
            SELECT id
            FROM outbox
            WHERE delivered_at IS NULL AND next_attempt_at <= now()
            ORDER BY next_attempt_at, id
            FOR UPDATE SKIP LOCKED
            LIMIT 1
        )
        UPDATE outbox AS event
        SET next_attempt_at = now() + make_interval(secs => $1)
        FROM candidates
        WHERE event.id = candidates.id
        RETURNING event.id, event.event_type, event.payload, event.attempts,
                  event.created_at, event.next_attempt_at
        "#,
    )
    .bind(lease_seconds)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(row) = row else {
        transaction.commit().await?;
        return Ok(ClaimResult::Empty);
    };
    let event = ClaimedEvent {
        id: row.try_get("id")?,
        event_type: row.try_get("event_type")?,
        payload: row.try_get("payload")?,
        attempts: row.try_get("attempts")?,
        created_at: row.try_get("created_at")?,
        claim_until: row.try_get("next_attempt_at")?,
    };
    let product_id = product_id(&event.payload);
    if let Ok(product_id) = product_id {
        let locked = sqlx::query_scalar::<_, bool>(
            "SELECT pg_try_advisory_xact_lock(hashtextextended($1, 0))",
        )
        .bind(product_id.to_string())
        .fetch_one(&mut *transaction)
        .await?;
        if !locked {
            release_claim(&mut transaction, &event, config.poll_interval).await?;
            transaction.commit().await?;
            return Ok(ClaimResult::Deferred);
        }
    }

    Ok(ClaimResult::Ready(ClaimedDelivery {
        transaction,
        event,
        product_id,
    }))
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
    poll_interval: Duration,
) -> Result<(), sqlx::Error> {
    let delay_seconds = i32::try_from(poll_interval.as_secs().max(1)).unwrap_or(i32::MAX);
    sqlx::query(
        r#"
        UPDATE outbox
        SET next_attempt_at = now() + make_interval(secs => $3)
        WHERE id = $1 AND delivered_at IS NULL AND next_attempt_at = $2
        "#,
    )
    .bind(event.id)
    .bind(event.claim_until)
    .bind(delay_seconds)
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

async fn read_response_body(
    mut response: reqwest::Response,
    limit: usize,
) -> (Option<String>, Option<&'static str>) {
    let mut retained = Vec::with_capacity(limit.min(4 * 1024));
    while retained.len() < limit {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(_) => return (None, Some("response_body_read")),
        };
        let remaining = limit.saturating_sub(retained.len());
        let take = remaining.min(chunk.len());
        retained.extend_from_slice(&chunk[..take]);
    }
    (Some(String::from_utf8_lossy(&retained).into_owned()), None)
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

fn retry_delay(attempts: i32, entropy: &dyn JitterSource) -> Duration {
    let attempt = u32::try_from(attempts).unwrap_or(u32::MAX);
    backoff(attempt, entropy.next_u64())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use super::*;

    struct SequenceEntropy {
        values: Mutex<VecDeque<u64>>,
    }

    impl SequenceEntropy {
        fn new(values: impl IntoIterator<Item = u64>) -> Self {
            Self {
                values: Mutex::new(values.into_iter().collect()),
            }
        }
    }

    impl JitterSource for SequenceEntropy {
        fn next_u64(&self) -> u64 {
            self.values
                .lock()
                .expect("entropy lock is not poisoned")
                .pop_front()
                .expect("test supplied enough entropy")
        }
    }

    #[test]
    fn retry_delay_uses_fresh_entropy_and_caps_the_exponential_ceiling() {
        let entropy = SequenceEntropy::new([0, u64::MAX, 0]);

        assert_eq!(retry_delay(0, &entropy), Duration::from_secs(30));
        assert_eq!(retry_delay(0, &entropy), Duration::ZERO);
        assert_eq!(
            retry_delay(i32::MAX, &entropy),
            Duration::from_secs(60 * 60)
        );
    }
}

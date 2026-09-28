use chrono::{DateTime, Utc};
use sqlx::PgPool;

use uuid::Uuid;

use crate::audit::{self, Actor};

/// Administrative selector for pending or previously delivered events.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplaySelector {
    /// Select one stable event identifier.
    Id(Uuid),
    /// Select events created at or after this instant.
    Since(DateTime<Utc>),
}

/// Makes the selected events' webhook deliveries immediately due and appends one audit entry
/// atomically; returns the number of deliveries rescheduled.
///
/// Previously delivered deliveries are selected only when `force` is true. Their `delivered_at`
/// value is then cleared so the normal worker delivers them again.
pub async fn replay(
    pool: &PgPool,
    selector: ReplaySelector,
    force: bool,
    actor: &Actor,
    reason: &str,
) -> Result<u64, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let (id, since, subject) = match selector {
        ReplaySelector::Id(id) => (Some(id), None, format!("event:{id}")),
        ReplaySelector::Since(since) => {
            (None, Some(since), format!("since:{}", since.to_rfc3339()))
        }
    };
    let count = sqlx::query(
        r#"
        UPDATE webhook_deliveries AS delivery
        SET next_attempt_at = now(),
            delivered_at = CASE WHEN $3 THEN NULL ELSE delivery.delivered_at END
        FROM events AS event
        WHERE event.id = delivery.event_id
          AND (event.id = $1 OR $1 IS NULL)
          AND (event.created >= $2 OR $2 IS NULL)
          AND (delivery.delivered_at IS NULL OR $3)
        "#,
    )
    .bind(id)
    .bind(since)
    .bind(force)
    .execute(&mut *transaction)
    .await?
    .rows_affected();

    if count > 0 {
        audit::insert(
            &mut *transaction,
            &audit::Entry {
                account_id: None,
                actor,
                action: "outbox.replay",
                subject: &subject,
                reason,
            },
        )
        .await?;
    }
    transaction.commit().await?;
    Ok(count)
}

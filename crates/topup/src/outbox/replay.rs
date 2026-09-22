use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// Administrative selector for pending or previously delivered events.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplaySelector {
    /// Select one stable event identifier.
    Id(Uuid),
    /// Select events created at or after this instant.
    Since(DateTime<Utc>),
}

/// Makes selected events immediately eligible and appends one audit entry atomically.
///
/// Previously delivered events are selected only when `force` is true. Their
/// `delivered_at` value is then cleared so the normal worker can redeliver them.
pub async fn replay(
    pool: &PgPool,
    selector: ReplaySelector,
    force: bool,
    actor: &str,
    reason: &str,
) -> Result<u64, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let (count, subject) = match selector {
        ReplaySelector::Id(id) => {
            let result = sqlx::query(
                r#"
                UPDATE outbox
                SET next_attempt_at = now(),
                    delivered_at = CASE WHEN $2 THEN NULL ELSE delivered_at END
                WHERE id = $1 AND (delivered_at IS NULL OR $2)
                "#,
            )
            .bind(id)
            .bind(force)
            .execute(&mut *transaction)
            .await?;
            (result.rows_affected(), format!("event:{id}"))
        }
        ReplaySelector::Since(since) => {
            let result = sqlx::query(
                r#"
                UPDATE outbox
                SET next_attempt_at = now(),
                    delivered_at = CASE WHEN $2 THEN NULL ELSE delivered_at END
                WHERE created_at >= $1 AND (delivered_at IS NULL OR $2)
                "#,
            )
            .bind(since)
            .bind(force)
            .execute(&mut *transaction)
            .await?;
            (
                result.rows_affected(),
                format!("since:{}", since.to_rfc3339()),
            )
        }
    };

    if count > 0 {
        sqlx::query(
            r#"
            INSERT INTO audit (id, actor, action, subject, reason)
            VALUES ($1, $2, 'outbox.replay', $3, $4)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(actor)
        .bind(subject)
        .bind(reason)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(count)
}

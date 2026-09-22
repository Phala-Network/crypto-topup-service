//! Backup RPO heartbeat persistence.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row as _};

/// Heartbeat row recorded for restore-point freshness checks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Heartbeat {
    /// Monotonic database-local identity.
    pub id: i64,
    /// Database clock time at insertion.
    pub recorded_at: DateTime<Utc>,
    /// Maximum accepted recovery point age in seconds.
    pub rpo_seconds: i32,
}

/// Inserts the once-per-minute restore heartbeat.
pub async fn record(pool: &PgPool) -> Result<Heartbeat, sqlx::Error> {
    let row =
        sqlx::query("INSERT INTO heartbeat DEFAULT VALUES RETURNING id, recorded_at, rpo_seconds")
            .fetch_one(pool)
            .await?;
    Ok(Heartbeat {
        id: row.try_get("id")?,
        recorded_at: row.try_get("recorded_at")?,
        rpo_seconds: row.try_get("rpo_seconds")?,
    })
}

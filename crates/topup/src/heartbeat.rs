//! Backup RPO heartbeat persistence.

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row as _};

/// Heartbeat row recorded for restore-point freshness checks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Heartbeat {
    /// Monotonic database-local identity.
    pub id: i64,
    /// Database clock time at insertion.
    pub recorded_at: DateTime<Utc>,
    /// Maximum accepted recovery point age in seconds.
    pub rpo_seconds: i32,
    /// WAL write location read after the heartbeat committed, so it covers the heartbeat row.
    pub wal_lsn: String,
}

/// Inserts the once-per-minute restore heartbeat and reads the source WAL location after commit.
///
/// The logged `recorded_at` and `wal_lsn` are the external failure point `restore-check` needs.
pub async fn record(pool: &PgPool) -> Result<Heartbeat, sqlx::Error> {
    let row =
        sqlx::query("INSERT INTO heartbeat DEFAULT VALUES RETURNING id, recorded_at, rpo_seconds")
            .fetch_one(pool)
            .await?;
    let wal_lsn = sqlx::query_scalar("SELECT pg_current_wal_lsn()::text")
        .fetch_one(pool)
        .await?;
    Ok(Heartbeat {
        id: row.try_get("id")?,
        recorded_at: row.try_get("recorded_at")?,
        rpo_seconds: row.try_get("rpo_seconds")?,
        wal_lsn,
    })
}

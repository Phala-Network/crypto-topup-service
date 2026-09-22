use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

/// An immutable administrative audit entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuditEntry {
    /// Audit entry identifier.
    pub id: Uuid,
    /// Authenticated actor.
    pub actor: String,
    /// Administrative action.
    pub action: String,
    /// Action subject.
    pub subject: String,
    /// Human-supplied reason.
    pub reason: String,
    /// Entry creation time.
    pub created_at: DateTime<Utc>,
}

/// Inserts an append-only audit entry.
pub async fn insert_audit(
    pool: &PgPool,
    id: Uuid,
    actor: &str,
    action: &str,
    subject: &str,
    reason: &str,
) -> Result<AuditEntry, sqlx::Error> {
    sqlx::query_as!(
        AuditEntry,
        r#"
        INSERT INTO audit (id, actor, action, subject, reason)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, actor, action, subject, reason, created_at
        "#,
        id,
        actor,
        action,
        subject,
        reason
    )
    .fetch_one(pool)
    .await
}

//! The append-only `audit` table: who did what to which subject, and why (architecture §13; design
//! §13 security history).

use sqlx::PgExecutor;
use uuid::Uuid;

/// The kind of actor an audit row names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActorType {
    /// A dashboard user.
    User,
    /// An account's API credential.
    ApiKey,
    /// The operator, through the admin API.
    Admin,
    /// A service component acting on its own.
    System,
}

impl ActorType {
    /// The stored code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::ApiKey => "api_key",
            Self::Admin => "admin",
            Self::System => "system",
        }
    }
}

/// The actor of an audited action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Actor {
    /// Kind of actor.
    pub actor_type: ActorType,
    /// The actor's id within its kind: a key id, a user id, or a component name.
    pub id: String,
}

impl Actor {
    /// An account's API credential, by its key id.
    #[must_use]
    pub fn api_key(key_id: impl Into<String>) -> Self {
        Self {
            actor_type: ActorType::ApiKey,
            id: key_id.into(),
        }
    }

    /// The operator, by the admin key id.
    #[must_use]
    pub fn admin(key_id: impl Into<String>) -> Self {
        Self {
            actor_type: ActorType::Admin,
            id: key_id.into(),
        }
    }

    /// A service component.
    #[must_use]
    pub fn system(component: impl Into<String>) -> Self {
        Self {
            actor_type: ActorType::System,
            id: component.into(),
        }
    }
}

impl std::fmt::Display for Actor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}:{}", self.actor_type.code(), self.id)
    }
}

/// One audit row. `account_id` is the account the action touched, for its security history, or
/// `None` for a platform action.
pub struct Entry<'a> {
    /// The account the action touched.
    pub account_id: Option<Uuid>,
    /// Who acted.
    pub actor: &'a Actor,
    /// Stable action code.
    pub action: &'a str,
    /// The object acted on, `type:id`.
    pub subject: &'a str,
    /// Why, or the evidence of the change.
    pub reason: &'a str,
}

/// Appends `entry` with a fresh id.
pub async fn insert<'e>(
    executor: impl PgExecutor<'e>,
    entry: &Entry<'_>,
) -> Result<(), sqlx::Error> {
    insert_with_id(executor, Uuid::new_v4(), entry).await
}

/// Appends `entry` with a caller-chosen id, so a retried write stays one row.
pub async fn insert_with_id<'e>(
    executor: impl PgExecutor<'e>,
    id: Uuid,
    entry: &Entry<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO audit (id, account_id, actor_type, actor_id, action, subject, reason)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
    )
    .bind(id)
    .bind(entry.account_id)
    .bind(entry.actor.actor_type.code())
    .bind(&entry.actor.id)
    .bind(entry.action)
    .bind(entry.subject)
    .bind(entry.reason)
    .execute(executor)
    .await?;
    Ok(())
}

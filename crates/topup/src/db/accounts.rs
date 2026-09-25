use sqlx::PgPool;
use uuid::Uuid;

/// A product-owned customer account.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Account {
    /// Stable account identifier.
    pub id: Uuid,
    /// Owning product identifier.
    pub product_id: Uuid,
    /// Product-provided account identifier.
    pub external_id: String,
    /// Workspace lifecycle state (`active` or `closed`).
    pub status: String,
    /// Runtime pause scopes.
    pub paused_scopes: Vec<String>,
}

/// Fetches an account by identifier.
pub async fn get_account(pool: &PgPool, id: Uuid) -> Result<Option<Account>, sqlx::Error> {
    sqlx::query_as!(
        Account,
        "SELECT id, product_id, external_id, status, paused_scopes FROM accounts WHERE id = $1",
        id
    )
    .fetch_optional(pool)
    .await
}

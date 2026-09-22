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
    /// Runtime pause scopes.
    pub paused_scopes: Vec<String>,
}

/// Values used to create an account.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewAccount {
    /// Stable account identifier.
    pub id: Uuid,
    /// Owning product identifier.
    pub product_id: Uuid,
    /// Product-provided account identifier.
    pub external_id: String,
    /// Runtime pause scopes.
    pub paused_scopes: Vec<String>,
}

/// Inserts an account.
pub async fn create_account(pool: &PgPool, account: &NewAccount) -> Result<Account, sqlx::Error> {
    sqlx::query_as!(
        Account,
        r#"
        INSERT INTO accounts (id, product_id, external_id, paused_scopes)
        VALUES ($1, $2, $3, $4)
        RETURNING id, product_id, external_id, paused_scopes
        "#,
        account.id,
        account.product_id,
        account.external_id,
        &account.paused_scopes
    )
    .fetch_one(pool)
    .await
}

/// Fetches an account by identifier.
pub async fn get_account(pool: &PgPool, id: Uuid) -> Result<Option<Account>, sqlx::Error> {
    sqlx::query_as!(
        Account,
        "SELECT id, product_id, external_id, paused_scopes FROM accounts WHERE id = $1",
        id
    )
    .fetch_optional(pool)
    .await
}

/// Replaces an account's runtime pause scopes and returns the updated row when present.
pub async fn set_account_paused_scopes(
    pool: &PgPool,
    id: Uuid,
    paused_scopes: &[String],
) -> Result<Option<Account>, sqlx::Error> {
    sqlx::query_as!(
        Account,
        r#"
        UPDATE accounts
        SET paused_scopes = $2
        WHERE id = $1
        RETURNING id, product_id, external_id, paused_scopes
        "#,
        id,
        paused_scopes
    )
    .fetch_optional(pool)
    .await
}

/// Deletes an account and returns whether a row was removed.
pub async fn delete_account(pool: &PgPool, id: Uuid) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!("DELETE FROM accounts WHERE id = $1", id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() == 1)
}

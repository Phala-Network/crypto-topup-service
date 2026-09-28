use sqlx::PgPool;
use uuid::Uuid;

/// A merchant account, the tenant (design D6).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Account {
    /// Stable account identifier.
    pub id: Uuid,
    /// API id, `acct_…`.
    pub public_id: String,
    /// Display name.
    pub name: String,
    /// Runtime pause scopes of the whole account: the operator's and the merchant's own.
    pub paused_scopes: Vec<String>,
}

/// A merchant's end customer, named by the merchant's `client_reference_id`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Customer {
    /// Stable customer identifier.
    pub id: Uuid,
    /// Owning account.
    pub account_id: Uuid,
    /// Mode the customer was created in.
    pub livemode: bool,
    /// The merchant's identifier for the customer.
    pub client_reference_id: String,
    /// Runtime pause scopes of this customer.
    pub paused_scopes: Vec<String>,
}

/// Fetches an account by identifier.
pub async fn get_account(pool: &PgPool, id: Uuid) -> Result<Option<Account>, sqlx::Error> {
    sqlx::query_as!(
        Account,
        r#"
        SELECT id, public_id AS "public_id!", name,
               ARRAY(
                   SELECT DISTINCT scope
                   FROM unnest(paused_scopes || self_paused_scopes) AS scope
                   ORDER BY scope
               ) AS "paused_scopes!"
        FROM accounts
        WHERE id = $1
        "#,
        id
    )
    .fetch_optional(pool)
    .await
}

/// Fetches a customer by identifier.
pub async fn get_customer(pool: &PgPool, id: Uuid) -> Result<Option<Customer>, sqlx::Error> {
    sqlx::query_as!(
        Customer,
        r#"
        SELECT id, account_id, livemode, client_reference_id, paused_scopes
        FROM customers
        WHERE id = $1
        "#,
        id
    )
    .fetch_optional(pool)
    .await
}

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

/// The customer `client_reference_id` of an account in a mode, created if it does not exist, and
/// locked `FOR NO KEY UPDATE` to the end of `connection`'s transaction (as quote and address
/// issuance lock it, without blocking foreign-key checks of scanner inserts). Created in the
/// caller's transaction, so a request refused later leaves no customer behind.
pub async fn ensure_customer_in(
    connection: &mut sqlx::PgConnection,
    account_id: Uuid,
    livemode: bool,
    client_reference_id: &str,
) -> Result<Customer, sqlx::Error> {
    sqlx::query(
        "INSERT INTO customers (id, account_id, livemode, client_reference_id) \
         VALUES ($1, $2, $3, $4) \
         ON CONFLICT (account_id, livemode, client_reference_id) DO NOTHING",
    )
    .bind(Uuid::new_v4())
    .bind(account_id)
    .bind(livemode)
    .bind(client_reference_id)
    .execute(&mut *connection)
    .await?;
    let (id, paused_scopes): (Uuid, Vec<String>) = sqlx::query_as(
        "SELECT id, paused_scopes FROM customers \
         WHERE account_id = $1 AND livemode = $2 AND client_reference_id = $3 \
         FOR NO KEY UPDATE",
    )
    .bind(account_id)
    .bind(livemode)
    .bind(client_reference_id)
    .fetch_one(&mut *connection)
    .await?;
    Ok(Customer {
        id,
        account_id,
        livemode,
        client_reference_id: client_reference_id.to_owned(),
        paused_scopes,
    })
}

//! Direct row writes and reads that set up and inspect test databases. Production writes these
//! rows through the API repository and the scanner.

use alloy_primitives::{Address as EvmAddress, B256, address};
use sqlx::PgPool;
use topup::db::{self, Account, Address, Customer};
use uuid::Uuid;

/// The treasury of the route fixture, which seeded addresses pay.
pub const FIXTURE_TREASURY: EvmAddress = address!("0x0000000000000000000000000000000000007EA5");

/// Values used to create a merchant account with its request signing key and, when
/// `webhook_url` is not empty, one webhook endpoint in the key's mode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewAccount {
    pub id: Uuid,
    pub name: String,
    pub livemode: bool,
    pub public_key: String,
    pub webhook_url: String,
    pub paused_scopes: Vec<String>,
}

impl NewAccount {
    /// A live account with no key material a test signs with and no webhook endpoint.
    pub fn named(name: &str) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            livemode: true,
            public_key: "test-key".to_owned(),
            webhook_url: String::new(),
            paused_scopes: Vec::new(),
        }
    }
}

/// Values used to create an account's customer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewCustomer {
    pub id: Uuid,
    pub account_id: Uuid,
    pub livemode: bool,
    pub client_reference_id: String,
    pub paused_scopes: Vec<String>,
}

/// Values used to insert a customer's forwarder address. The address gets a canceled quote on
/// `route`, so payments to it are valued at spot, as for any address without an open quote.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewAddress {
    pub id: Uuid,
    pub customer_id: Uuid,
    pub chain_id: u64,
    pub route: String,
    pub salt: B256,
    pub address: EvmAddress,
}

/// The key id an account signs its requests with, `{acct_…}/v1`.
pub fn key_id(account: &Account) -> String {
    format!("{}/v1", account.public_id)
}

pub async fn create_account(pool: &PgPool, account: &NewAccount) -> Result<Account, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    sqlx::query("INSERT INTO accounts (id, name, paused_scopes) VALUES ($1, $2, $3)")
        .bind(account.id)
        .bind(&account.name)
        .bind(&account.paused_scopes)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "INSERT INTO request_signing_keys (account_id, livemode, public_key) VALUES ($1, $2, $3)",
    )
    .bind(account.id)
    .bind(account.livemode)
    .bind(&account.public_key)
    .execute(&mut *transaction)
    .await?;
    if !account.webhook_url.is_empty() {
        sqlx::query(
            "INSERT INTO webhook_endpoints (id, account_id, livemode, url) VALUES ($1, $2, $3, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(account.id)
        .bind(account.livemode)
        .bind(&account.webhook_url)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    db::get_account(pool, account.id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

pub async fn create_customer(
    pool: &PgPool,
    customer: &NewCustomer,
) -> Result<Customer, sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO customers (id, account_id, livemode, client_reference_id, paused_scopes)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(customer.id)
    .bind(customer.account_id)
    .bind(customer.livemode)
    .bind(&customer.client_reference_id)
    .bind(&customer.paused_scopes)
    .execute(pool)
    .await?;
    db::get_customer(pool, customer.id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

/// Creates an account and one customer of it in the account's mode.
pub async fn create_account_and_customer(
    pool: &PgPool,
    account: &NewAccount,
    client_reference_id: &str,
) -> Result<(Account, Customer), sqlx::Error> {
    let account_row = create_account(pool, account).await?;
    let customer = create_customer(
        pool,
        &NewCustomer {
            id: Uuid::new_v4(),
            account_id: account.id,
            livemode: account.livemode,
            client_reference_id: client_reference_id.to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    Ok((account_row, customer))
}

pub async fn insert_address(pool: &PgPool, address: &NewAddress) -> Result<Address, sqlx::Error> {
    let chain_id = i64::try_from(address.chain_id).map_err(|error| encode_error(&error))?;
    let quote_id = Uuid::new_v4();
    let mut transaction = pool.begin().await?;
    sqlx::query(
        r#"
        INSERT INTO quotes (
            id, account_id, livemode, customer_id, route, amount_atomic, price_scaled,
            credit_minor, expires_at, status, closed_at
        )
        SELECT $1, account_id, livemode, id, $3, 1, 100000000, 1, now(), 'cancelled', now()
        FROM customers
        WHERE id = $2
        "#,
    )
    .bind(quote_id)
    .bind(address.customer_id)
    .bind(&address.route)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO addresses
            (id, account_id, livemode, chain_id, quote_id, salt, treasury, address)
        SELECT $1, account_id, livemode, $2, id, $3, $4, $5
        FROM quotes
        WHERE id = $6
        "#,
    )
    .bind(address.id)
    .bind(chain_id)
    .bind(format!("{:#x}", address.salt))
    .bind(format!("{FIXTURE_TREASURY:#x}"))
    .bind(format!("{:#x}", address.address))
    .bind(quote_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    db::get_address(pool, address.id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

pub async fn set_customer_paused_scopes(
    pool: &PgPool,
    id: Uuid,
    paused_scopes: &[String],
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE customers SET paused_scopes = $2 WHERE id = $1")
        .bind(id)
        .bind(paused_scopes)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn set_account_paused_scopes(
    pool: &PgPool,
    id: Uuid,
    paused_scopes: &[String],
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE accounts SET paused_scopes = $2 WHERE id = $1")
        .bind(id)
        .bind(paused_scopes)
        .execute(pool)
        .await?;
    Ok(())
}

fn encode_error(error: &std::num::TryFromIntError) -> sqlx::Error {
    sqlx::Error::Encode(format!("value is outside PostgreSQL bigint: {error}").into())
}

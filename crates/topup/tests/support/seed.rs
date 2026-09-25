//! Direct row writes and reads that set up and inspect test databases. Production writes these
//! rows through the API repository, the scanner, and the settlement step.

use alloy_primitives::{Address as EvmAddress, B256};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use topup::db::{self, Account, Address, AddressKind, Product, Settlement, SettlementStatus};
use uuid::Uuid;

/// Values used to create a product.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewProduct {
    pub id: Uuid,
    pub slug: String,
    pub webhook_url: String,
    pub pubkey: String,
    pub paused_scopes: Vec<String>,
}

/// Values used to create an account.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewAccount {
    pub id: Uuid,
    pub product_id: Uuid,
    pub external_id: String,
    pub paused_scopes: Vec<String>,
}

/// Values used to insert an address.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewAddress {
    pub id: Uuid,
    pub account_id: Uuid,
    pub chain_id: u64,
    pub kind: AddressKind,
    pub version: u64,
    pub lock_ref: Option<String>,
    pub salt: B256,
    pub address: EvmAddress,
    pub retired_at: Option<DateTime<Utc>>,
}

pub async fn create_product(pool: &PgPool, product: &NewProduct) -> Result<Product, sqlx::Error> {
    sqlx::query_as!(
        Product,
        r#"
        INSERT INTO products (id, slug, webhook_url, pubkey, paused_scopes)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, slug, webhook_url, pubkey, paused_scopes
        "#,
        product.id,
        product.slug,
        product.webhook_url,
        product.pubkey,
        &product.paused_scopes
    )
    .fetch_one(pool)
    .await
}

pub async fn create_account(pool: &PgPool, account: &NewAccount) -> Result<Account, sqlx::Error> {
    sqlx::query_as!(
        Account,
        r#"
        INSERT INTO accounts (id, product_id, external_id, paused_scopes)
        VALUES ($1, $2, $3, $4)
        RETURNING id, product_id, external_id, status, paused_scopes
        "#,
        account.id,
        account.product_id,
        account.external_id,
        &account.paused_scopes
    )
    .fetch_one(pool)
    .await
}

pub async fn insert_address(pool: &PgPool, address: &NewAddress) -> Result<Address, sqlx::Error> {
    let kind = match address.kind {
        AddressKind::Persistent => "persistent",
        AddressKind::Lock => "lock",
    };
    let chain_id = i64::try_from(address.chain_id).map_err(|error| encode_error(&error))?;
    let version = i64::try_from(address.version).map_err(|error| encode_error(&error))?;
    sqlx::query(
        r#"
        INSERT INTO addresses
            (id, account_id, chain_id, kind, version, lock_ref, salt, address, retired_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        "#,
    )
    .bind(address.id)
    .bind(address.account_id)
    .bind(chain_id)
    .bind(kind)
    .bind(version)
    .bind(&address.lock_ref)
    .bind(format!("{:#x}", address.salt))
    .bind(format!("{:#x}", address.address))
    .bind(address.retired_at)
    .execute(pool)
    .await?;
    db::get_address(pool, address.id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

pub async fn insert_audit(
    pool: &PgPool,
    id: Uuid,
    actor: &str,
    action: &str,
    subject: &str,
    reason: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO audit (id, actor, action, subject, reason) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(actor)
    .bind(action)
    .bind(subject)
    .bind(reason)
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

pub async fn set_product_paused_scopes(
    pool: &PgPool,
    id: Uuid,
    paused_scopes: &[String],
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE products SET paused_scopes = $2 WHERE id = $1")
        .bind(id)
        .bind(paused_scopes)
        .execute(pool)
        .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct SettlementRecord {
    deposit_id: Uuid,
    product_id: Uuid,
    key: String,
    payload: Value,
    status: String,
    destination_tx_id: Option<String>,
    receipt: Option<Value>,
    resend_forbidden: bool,
    sent_at: Option<DateTime<Utc>>,
}

pub async fn get_settlement(
    pool: &PgPool,
    deposit_id: Uuid,
) -> Result<Option<Settlement>, sqlx::Error> {
    let Some(record) = sqlx::query_as::<_, SettlementRecord>(
        r#"
        SELECT deposit_id, product_id, key, payload, status,
               destination_tx_id, receipt, resend_forbidden, sent_at
        FROM settlements
        WHERE deposit_id = $1
        "#,
    )
    .bind(deposit_id)
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let status = match record.status.as_str() {
        "intent" => SettlementStatus::Intent,
        "sent" => SettlementStatus::Sent,
        "accepted" => SettlementStatus::Accepted,
        "rejected" => SettlementStatus::Rejected,
        other => {
            return Err(sqlx::Error::Decode(
                format!("unknown settlement status `{other}`").into(),
            ));
        }
    };
    Ok(Some(Settlement {
        deposit_id: record.deposit_id,
        product_id: record.product_id,
        key: record.key,
        payload: record.payload,
        status,
        destination_tx_id: record.destination_tx_id,
        receipt: record.receipt,
        resend_forbidden: record.resend_forbidden,
        sent_at: record.sent_at,
    }))
}

fn encode_error(error: &std::num::TryFromIntError) -> sqlx::Error {
    sqlx::Error::Encode(format!("value is outside PostgreSQL bigint: {error}").into())
}

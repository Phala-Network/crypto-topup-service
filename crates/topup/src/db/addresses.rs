use alloy_primitives::{Address as EvmAddress, B256};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::types::{parse_address, parse_b256, to_i64, to_u64};

/// The derivation purpose of a deposit address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressKind {
    /// Reusable account address.
    Persistent,
    /// Single-use rate-lock address.
    Lock,
}

impl AddressKind {
    fn parse(value: &str) -> Result<Self, sqlx::Error> {
        match value {
            "persistent" => Ok(Self::Persistent),
            "lock" => Ok(Self::Lock),
            other => Err(sqlx::Error::Decode(
                format!("unknown address kind `{other}`").into(),
            )),
        }
    }
}

/// A stored physical deposit address.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Address {
    /// Address row identifier.
    pub id: Uuid,
    /// Owning account identifier.
    pub account_id: Uuid,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Address derivation purpose.
    pub kind: AddressKind,
    /// Persistent address version, or zero for locks.
    pub version: u64,
    /// Product lock reference for lock addresses.
    pub lock_ref: Option<String>,
    /// CREATE2 salt.
    pub salt: B256,
    /// Physical chain address.
    pub address: EvmAddress,
    /// Retirement time for rotated persistent addresses.
    pub retired_at: Option<DateTime<Utc>>,
}

#[derive(Debug, sqlx::FromRow)]
struct AddressRecord {
    id: Uuid,
    account_id: Uuid,
    chain_id: i64,
    kind: String,
    version: i64,
    lock_ref: Option<String>,
    salt: String,
    address: String,
    retired_at: Option<DateTime<Utc>>,
}

pub(crate) struct AddressWithPauseScopes {
    pub(crate) address: Address,
    pub(crate) account_scopes: Vec<String>,
    pub(crate) product_scopes: Vec<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct AddressWithPauseScopesRecord {
    id: Uuid,
    account_id: Uuid,
    chain_id: i64,
    kind: String,
    version: i64,
    lock_ref: Option<String>,
    salt: String,
    address: String,
    retired_at: Option<DateTime<Utc>>,
    account_scopes: Vec<String>,
    product_scopes: Vec<String>,
}

impl TryFrom<AddressRecord> for Address {
    type Error = sqlx::Error;

    fn try_from(record: AddressRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.id,
            account_id: record.account_id,
            chain_id: to_u64(record.chain_id, "addresses.chain_id")?,
            kind: AddressKind::parse(&record.kind)?,
            version: to_u64(record.version, "addresses.version")?,
            lock_ref: record.lock_ref,
            salt: parse_b256(&record.salt)?,
            address: parse_address(&record.address)?,
            retired_at: record.retired_at,
        })
    }
}

/// Fetches an address by row identifier.
pub async fn get_address(pool: &PgPool, id: Uuid) -> Result<Option<Address>, sqlx::Error> {
    let record = sqlx::query_as!(
        AddressRecord,
        "SELECT id, account_id, chain_id, kind, version, lock_ref, salt, address, retired_at FROM addresses WHERE id = $1",
        id
    )
    .fetch_optional(pool)
    .await?;
    record.map(TryInto::try_into).transpose()
}

/// Lists every active or retired forwarder address stored for a chain.
pub async fn list_chain_addresses(
    pool: &PgPool,
    chain_id: u64,
) -> Result<Vec<Address>, sqlx::Error> {
    let chain_id = to_i64(chain_id, "addresses.chain_id")?;
    let records = sqlx::query_as::<_, AddressRecord>(
        r#"
        SELECT id, account_id, chain_id, kind, version, lock_ref, salt, address, retired_at
        FROM addresses
        WHERE chain_id = $1
        ORDER BY address, id
        "#,
    )
    .bind(chain_id)
    .fetch_all(pool)
    .await?;
    records.into_iter().map(TryInto::try_into).collect()
}

pub(crate) async fn list_chain_addresses_with_pause_scopes(
    pool: &PgPool,
    chain_id: u64,
) -> Result<Vec<AddressWithPauseScopes>, sqlx::Error> {
    let chain_id = to_i64(chain_id, "addresses.chain_id")?;
    let records = sqlx::query_as::<_, AddressWithPauseScopesRecord>(
        r#"
        SELECT
            address.id,
            address.account_id,
            address.chain_id,
            address.kind,
            address.version,
            address.lock_ref,
            address.salt,
            address.address,
            address.retired_at,
            account.paused_scopes AS account_scopes,
            product.paused_scopes AS product_scopes
        FROM addresses AS address
        JOIN accounts AS account ON account.id = address.account_id
        JOIN products AS product ON product.id = account.product_id
        WHERE address.chain_id = $1
        ORDER BY address.address, address.id
        "#,
    )
    .bind(chain_id)
    .fetch_all(pool)
    .await?;
    records
        .into_iter()
        .map(|record| {
            let address = AddressRecord {
                id: record.id,
                account_id: record.account_id,
                chain_id: record.chain_id,
                kind: record.kind,
                version: record.version,
                lock_ref: record.lock_ref,
                salt: record.salt,
                address: record.address,
                retired_at: record.retired_at,
            }
            .try_into()?;
            Ok(AddressWithPauseScopes {
                address,
                account_scopes: record.account_scopes,
                product_scopes: record.product_scopes,
            })
        })
        .collect()
}

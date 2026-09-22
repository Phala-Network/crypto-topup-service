use alloy_primitives::{Address as EvmAddress, B256};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::types::{address_hex, b256_hex, parse_address, parse_b256, to_i64, to_u64};

/// The derivation purpose of a deposit address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AddressKind {
    /// Reusable account address.
    Persistent,
    /// Single-use rate-lock address.
    Lock,
}

impl AddressKind {
    fn code(self) -> &'static str {
        match self {
            Self::Persistent => "persistent",
            Self::Lock => "lock",
        }
    }

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

/// Values used to insert an address.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NewAddress {
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

/// Inserts an address.
pub async fn insert_address(pool: &PgPool, address: &NewAddress) -> Result<Address, sqlx::Error> {
    let kind = address.kind.code();
    let chain_id = to_i64(address.chain_id, "addresses.chain_id")?;
    let version = to_i64(address.version, "addresses.version")?;
    let salt = b256_hex(address.salt);
    let physical_address = address_hex(address.address);
    let record = sqlx::query_as!(
        AddressRecord,
        r#"
        INSERT INTO addresses
            (id, account_id, chain_id, kind, version, lock_ref, salt, address, retired_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        RETURNING id, account_id, chain_id, kind, version, lock_ref, salt, address, retired_at
        "#,
        address.id,
        address.account_id,
        chain_id,
        kind,
        version,
        address.lock_ref,
        salt,
        physical_address,
        address.retired_at
    )
    .fetch_one(pool)
    .await?;
    record.try_into()
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

/// Finds an address by its chain and physical address.
pub async fn find_address_by_chain(
    pool: &PgPool,
    chain_id: u64,
    address: EvmAddress,
) -> Result<Option<Address>, sqlx::Error> {
    let chain_id = to_i64(chain_id, "addresses.chain_id")?;
    let address = address_hex(address);
    let record = sqlx::query_as!(
        AddressRecord,
        "SELECT id, account_id, chain_id, kind, version, lock_ref, salt, address, retired_at FROM addresses WHERE chain_id = $1 AND address = $2",
        chain_id,
        address
    )
    .fetch_optional(pool)
    .await?;
    record.map(TryInto::try_into).transpose()
}

/// Finds the active persistent address for an account and chain.
pub async fn find_active_persistent(
    pool: &PgPool,
    account_id: Uuid,
    chain_id: u64,
) -> Result<Option<Address>, sqlx::Error> {
    let chain_id = to_i64(chain_id, "addresses.chain_id")?;
    let record = sqlx::query_as!(
        AddressRecord,
        r#"
        SELECT id, account_id, chain_id, kind, version, lock_ref, salt, address, retired_at
        FROM addresses
        WHERE account_id = $1 AND chain_id = $2 AND kind = 'persistent' AND retired_at IS NULL
        "#,
        account_id,
        chain_id
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

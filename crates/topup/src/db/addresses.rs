use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

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
    pub chain_id: i64,
    /// Address derivation purpose.
    pub kind: AddressKind,
    /// Persistent address version, or zero for locks.
    pub version: i64,
    /// Product lock reference for lock addresses.
    pub lock_ref: Option<String>,
    /// CREATE2 salt as a normalized string.
    pub salt: String,
    /// Physical chain address.
    pub address: String,
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
    pub chain_id: i64,
    /// Address derivation purpose.
    pub kind: AddressKind,
    /// Persistent address version, or zero for locks.
    pub version: i64,
    /// Product lock reference for lock addresses.
    pub lock_ref: Option<String>,
    /// CREATE2 salt as a normalized string.
    pub salt: String,
    /// Physical chain address.
    pub address: String,
    /// Retirement time for rotated persistent addresses.
    pub retired_at: Option<DateTime<Utc>>,
}

#[derive(Debug)]
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
            chain_id: record.chain_id,
            kind: AddressKind::parse(&record.kind)?,
            version: record.version,
            lock_ref: record.lock_ref,
            salt: record.salt,
            address: record.address,
            retired_at: record.retired_at,
        })
    }
}

/// Inserts an address.
pub async fn insert_address(pool: &PgPool, address: &NewAddress) -> Result<Address, sqlx::Error> {
    let kind = address.kind.code();
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
        address.chain_id,
        kind,
        address.version,
        address.lock_ref,
        address.salt,
        address.address,
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
    chain_id: i64,
    address: &str,
) -> Result<Option<Address>, sqlx::Error> {
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
    chain_id: i64,
) -> Result<Option<Address>, sqlx::Error> {
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

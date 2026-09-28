use alloy_primitives::{Address as EvmAddress, B256};
use sqlx::PgPool;
use uuid::Uuid;

use super::types::{parse_address, parse_b256, to_i64, to_u64};

/// A stored forwarder address, a quote's or a deposit address's.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Address {
    /// Address row identifier.
    pub id: Uuid,
    /// Owning account.
    pub account_id: Uuid,
    /// Mode of the owning quote.
    pub livemode: bool,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// The quote the address was issued for; `None` for a deposit address.
    pub quote_id: Option<Uuid>,
    /// The deposit address this forwarder is; `None` for a quote's address.
    pub deposit_address_id: Option<Uuid>,
    /// CREATE2 salt.
    pub salt: B256,
    /// The treasury the forwarder pays, its clone argument.
    pub treasury: EvmAddress,
    /// Physical chain address.
    pub address: EvmAddress,
}

#[derive(Debug, sqlx::FromRow)]
struct AddressRecord {
    id: Uuid,
    account_id: Uuid,
    livemode: bool,
    chain_id: i64,
    quote_id: Option<Uuid>,
    deposit_address_id: Option<Uuid>,
    salt: String,
    treasury: String,
    address: String,
}

impl TryFrom<AddressRecord> for Address {
    type Error = sqlx::Error;

    fn try_from(record: AddressRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.id,
            account_id: record.account_id,
            livemode: record.livemode,
            chain_id: to_u64(record.chain_id, "addresses.chain_id")?,
            quote_id: record.quote_id,
            deposit_address_id: record.deposit_address_id,
            salt: parse_b256(&record.salt)?,
            treasury: parse_address(&record.treasury)?,
            address: parse_address(&record.address)?,
        })
    }
}

/// Fetches an address by row identifier.
pub async fn get_address(pool: &PgPool, id: Uuid) -> Result<Option<Address>, sqlx::Error> {
    let record = sqlx::query_as!(
        AddressRecord,
        r#"
        SELECT id, account_id, livemode, chain_id, quote_id, deposit_address_id, salt, treasury,
               address
        FROM addresses
        WHERE id = $1
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    record.map(TryInto::try_into).transpose()
}

/// Lists every forwarder address stored for a chain, across accounts.
pub async fn list_chain_addresses(
    pool: &PgPool,
    chain_id: u64,
) -> Result<Vec<Address>, sqlx::Error> {
    let chain_id = to_i64(chain_id, "addresses.chain_id")?;
    let records = sqlx::query_as::<_, AddressRecord>(
        r#"
        SELECT id, account_id, livemode, chain_id, quote_id, deposit_address_id, salt, treasury,
               address
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

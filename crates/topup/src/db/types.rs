use std::str::FromStr;

use alloy_primitives::{Address, B256, U256};
use topup_core::money::{AtomicAmount, MinorAmount};

pub(crate) fn atomic_decimal(amount: AtomicAmount) -> String {
    amount.value().to_string()
}

pub(crate) fn parse_atomic_decimal(value: &str) -> Result<AtomicAmount, sqlx::Error> {
    validate_decimal(value)?;
    U256::from_str(value)
        .map(AtomicAmount::new)
        .map_err(|error| decode_error(format!("atomic amount is outside U256: {error}")))
}

pub(crate) fn parse_optional_u64_decimal(value: Option<&str>) -> Result<Option<u64>, sqlx::Error> {
    value
        .map(|decimal| {
            validate_decimal(decimal)?;
            decimal
                .parse::<u64>()
                .map_err(|error| decode_error(format!("integer is outside u64: {error}")))
        })
        .transpose()
}

pub(crate) fn parse_optional_minor_decimal(
    value: Option<&str>,
) -> Result<Option<MinorAmount>, sqlx::Error> {
    parse_optional_u64_decimal(value).map(|amount| amount.map(MinorAmount::new))
}

pub(crate) fn address_hex(address: Address) -> String {
    format!("{address:#x}")
}

pub(crate) fn b256_hex(value: B256) -> String {
    format!("{value:#x}")
}

pub(crate) fn parse_address(value: &str) -> Result<Address, sqlx::Error> {
    let address = Address::from_str(value)
        .map_err(|error| decode_error(format!("invalid address: {error}")))?;
    if address_hex(address) != value {
        return Err(decode_error("address is not canonical lowercase hex"));
    }
    Ok(address)
}

pub(crate) fn parse_b256(value: &str) -> Result<B256, sqlx::Error> {
    let hash = B256::from_str(value)
        .map_err(|error| decode_error(format!("invalid 32-byte hash: {error}")))?;
    if b256_hex(hash) != value {
        return Err(decode_error("hash is not canonical lowercase hex"));
    }
    Ok(hash)
}

pub(crate) fn to_i64(value: u64, field: &'static str) -> Result<i64, sqlx::Error> {
    i64::try_from(value).map_err(|error| {
        sqlx::Error::Encode(format!("{field} is outside PostgreSQL bigint: {error}").into())
    })
}

pub(crate) fn to_u64(value: i64, field: &'static str) -> Result<u64, sqlx::Error> {
    u64::try_from(value).map_err(|error| decode_error(format!("{field} is negative: {error}")))
}

fn validate_decimal(value: &str) -> Result<(), sqlx::Error> {
    if value.is_empty() || value.len() > 78 || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(decode_error(
            "numeric value must contain 1 to 78 decimal digits",
        ));
    }
    Ok(())
}

fn decode_error(message: impl Into<String>) -> sqlx::Error {
    sqlx::Error::Decode(message.into().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_decimal_parser_rejects_non_integer_and_out_of_range_values() {
        for invalid in [
            "1.5",
            "NaN",
            "115792089237316195423570985008687907853269984665640564039457584007913129639936",
        ] {
            assert!(
                parse_atomic_decimal(invalid).is_err(),
                "{invalid} must fail"
            );
        }
    }

    #[test]
    fn atomic_decimal_parser_accepts_u256_max() {
        let maximum = U256::MAX.to_string();
        assert_eq!(
            parse_atomic_decimal(&maximum).expect("U256 max must parse"),
            AtomicAmount::new(U256::MAX)
        );
    }
}

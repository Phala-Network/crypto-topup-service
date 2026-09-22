//! Checked integer money arithmetic and documented rounding rules.

use std::error::Error;
use std::fmt;
use std::str::FromStr;

use alloy_primitives::{U256, U512};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The fixed number of decimal places used by scaled prices.
pub const PRICE_SCALE: u8 = 8;

/// An amount in the asset's smallest on-chain unit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct AtomicAmount(U256);

impl AtomicAmount {
    /// Creates an atomic amount.
    #[must_use]
    pub const fn new(value: U256) -> Self {
        Self(value)
    }

    /// Returns the underlying unsigned integer.
    #[must_use]
    pub const fn value(self) -> U256 {
        self.0
    }

    /// Adds two atomic amounts, returning `None` on overflow.
    #[must_use]
    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    /// Subtracts two atomic amounts, returning `None` on underflow.
    #[must_use]
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }
}

impl Serialize for AtomicAmount {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for AtomicAmount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        U256::from_str(&value)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

/// An amount in the destination product's minor unit.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MinorAmount(u64);

impl MinorAmount {
    /// Creates a minor-unit amount.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the underlying unsigned integer.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }

    /// Adds two minor-unit amounts, returning `None` on overflow.
    #[must_use]
    pub fn checked_add(self, other: Self) -> Option<Self> {
        self.0.checked_add(other.0).map(Self)
    }

    /// Subtracts two minor-unit amounts, returning `None` on underflow.
    #[must_use]
    pub fn checked_sub(self, other: Self) -> Option<Self> {
        self.0.checked_sub(other.0).map(Self)
    }
}

/// A price stored as an integer with a fixed decimal scale.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ScaledPrice {
    value: u64,
    scale: u8,
}

impl ScaledPrice {
    /// Creates a non-zero price at the required eight-decimal scale.
    pub fn new(value: u64, scale: u8) -> Result<Self, MoneyTypeError> {
        if value == 0 {
            return Err(MoneyTypeError::ZeroPrice);
        }
        if scale != PRICE_SCALE {
            return Err(MoneyTypeError::InvalidPriceScale { scale });
        }
        Ok(Self { value, scale })
    }

    /// Returns the scaled integer value.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.value
    }

    /// Returns the number of decimal places.
    #[must_use]
    pub const fn scale(self) -> u8 {
        self.scale
    }
}

/// Basis points, constrained to the inclusive range 0 through 10,000.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bps(u16);

impl Bps {
    /// Creates a validated basis-point value.
    pub fn new(value: u16) -> Result<Self, MoneyTypeError> {
        if value > 10_000 {
            return Err(MoneyTypeError::InvalidBps { value });
        }
        Ok(Self(value))
    }

    /// Returns the underlying basis-point value.
    #[must_use]
    pub const fn value(self) -> u16 {
        self.0
    }
}

/// Errors constructing constrained money types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoneyTypeError {
    /// A price of zero cannot be used for inverse quote calculations.
    ZeroPrice,
    /// A price scale other than eight was supplied.
    InvalidPriceScale {
        /// The rejected scale.
        scale: u8,
    },
    /// A basis-point value exceeded 10,000.
    InvalidBps {
        /// The rejected value.
        value: u16,
    },
}

impl fmt::Display for MoneyTypeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroPrice => formatter.write_str("price must be greater than zero"),
            Self::InvalidPriceScale { scale } => {
                write!(formatter, "price scale must be {PRICE_SCALE}, got {scale}")
            }
            Self::InvalidBps { value } => {
                write!(formatter, "basis points must be at most 10000, got {value}")
            }
        }
    }
}

impl Error for MoneyTypeError {}

/// Errors computing destination credit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CreditError {
    /// A decimal exponent could not be represented in the 512-bit intermediate.
    ScaleOutOfRange,
    /// The floored result does not fit into a `u64` minor amount.
    OutOfRange,
}

impl fmt::Display for CreditError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScaleOutOfRange => formatter.write_str("decimal scale is out of range"),
            Self::OutOfRange => formatter.write_str("credit is out of range for a minor amount"),
        }
    }
}

impl Error for CreditError {}

/// Computes destination credit and rounds down toward zero.
///
/// The calculation uses a 512-bit intermediate. For a non-negative exponent it computes
/// `floor(amount * price / 10^exp)`; for a negative exponent it computes
/// `amount * price * 10^(-exp)`. The final value is rejected when it does not fit in `u64`.
pub fn credit(
    amount: AtomicAmount,
    price: ScaledPrice,
    asset_decimals: u8,
    unit_decimals: u8,
) -> Result<MinorAmount, CreditError> {
    let exponent = decimal_exponent(asset_decimals, price.scale, unit_decimals);
    let product = U512::from(amount.0)
        .checked_mul(U512::from(price.value))
        .ok_or(CreditError::OutOfRange)?;
    let result = if exponent >= 0 {
        let divisor = power_of_ten(exponent.unsigned_abs())?;
        product
            .checked_div(divisor)
            .ok_or(CreditError::ScaleOutOfRange)?
    } else {
        product
            .checked_mul(power_of_ten(exponent.unsigned_abs())?)
            .ok_or(CreditError::OutOfRange)?
    };
    u64::try_from(result)
        .map(MinorAmount)
        .map_err(|_| CreditError::OutOfRange)
}

/// Applies a spread to a spot price and rounds the quotient to nearest, ties to even.
///
/// The returned price is never greater than `spot`; a positive spread lowers the price.
#[must_use]
pub fn lock_price(spot: ScaledPrice, spread: Bps) -> ScaledPrice {
    let numerator = U256::from(spot.value)
        .checked_mul(U256::from(10_000_u16))
        .unwrap_or(U256::MAX);
    let denominator = U256::from(10_000_u16)
        .checked_add(U256::from(spread.0))
        .unwrap_or(U256::MAX);
    let quotient = numerator.checked_div(denominator).unwrap_or_default();
    let remainder = numerator.checked_rem(denominator).unwrap_or_default();
    let twice_remainder = remainder.checked_mul(U256::from(2_u8)).unwrap_or(U256::MAX);
    let round_up =
        twice_remainder > denominator || (twice_remainder == denominator && quotient.bit(0));
    let rounded = if round_up {
        quotient.checked_add(U256::from(1_u8)).unwrap_or(quotient)
    } else {
        quotient
    };
    let value = u64::try_from(rounded).unwrap_or(spot.value);
    ScaledPrice {
        value,
        scale: spot.scale,
    }
}

/// Returns the smallest atomic amount whose floored credit reaches `target`.
///
/// This is the inverse of [`credit`] over validated route decimal counts (at most 36). Division
/// rounds up. If inputs outside that configured domain exceed `U256`, the result saturates at the
/// largest atomic amount because the requested token amount is not representable.
#[must_use]
pub fn tokens_for_credit(
    target: MinorAmount,
    price: ScaledPrice,
    asset_decimals: u8,
    unit_decimals: u8,
) -> AtomicAmount {
    let exponent = decimal_exponent(asset_decimals, price.scale, unit_decimals);
    let target = U512::from(target.0);
    let price = U512::from(price.value);
    let (numerator, denominator) = if exponent >= 0 {
        (
            target
                .checked_mul(power_of_ten_saturating(exponent.unsigned_abs()))
                .unwrap_or(U512::MAX),
            price,
        )
    } else {
        (
            target,
            price
                .checked_mul(power_of_ten_saturating(exponent.unsigned_abs()))
                .unwrap_or(U512::MAX),
        )
    };
    let quotient = numerator.checked_div(denominator).unwrap_or(U512::MAX);
    let remainder = numerator.checked_rem(denominator).unwrap_or_default();
    let rounded = if remainder.is_zero() {
        quotient
    } else {
        quotient.checked_add(U512::from(1_u8)).unwrap_or(U512::MAX)
    };
    let (value, overflow) = U256::overflowing_from_limbs_slice(rounded.as_limbs());
    AtomicAmount(if overflow { U256::MAX } else { value })
}

fn decimal_exponent(asset_decimals: u8, price_scale: u8, unit_decimals: u8) -> i16 {
    i16::from(asset_decimals)
        .checked_add(i16::from(price_scale))
        .and_then(|value| value.checked_sub(i16::from(unit_decimals)))
        .unwrap_or_default()
}

fn power_of_ten(exponent: u16) -> Result<U512, CreditError> {
    U512::from(10_u8)
        .checked_pow(U512::from(exponent))
        .ok_or(CreditError::ScaleOutOfRange)
}

fn power_of_ten_saturating(exponent: u16) -> U512 {
    U512::from(10_u8).saturating_pow(U512::from(exponent))
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn price(value: u64) -> ScaledPrice {
        ScaledPrice::new(value, PRICE_SCALE).expect("test prices are valid")
    }

    proptest! {
        #[test]
        fn credit_is_monotone_in_amount(first in any::<u128>(), extra in any::<u64>(), price_value in 1_u64..=u64::MAX) {
            let first = U256::from(first);
            let second = first + U256::from(extra);
            let first_credit = credit(AtomicAmount::new(first), price(price_value), 18, 2);
            let second_credit = credit(AtomicAmount::new(second), price(price_value), 18, 2);
            if let (Ok(first_credit), Ok(second_credit)) = (first_credit, second_credit) {
                prop_assert!(first_credit <= second_credit);
            }
        }

        #[test]
        fn credit_is_monotone_in_price(amount in any::<u128>(), first_price in 1_u64..=u64::MAX, extra in any::<u32>()) {
            let second_price = first_price.saturating_add(u64::from(extra));
            let first_credit = credit(AtomicAmount::new(U256::from(amount)), price(first_price), 18, 2);
            let second_credit = credit(AtomicAmount::new(U256::from(amount)), price(second_price), 18, 2);
            if let (Ok(first_credit), Ok(second_credit)) = (first_credit, second_credit) {
                prop_assert!(first_credit <= second_credit);
            }
        }

        #[test]
        fn splitting_loses_at_most_n_minus_one(parts in proptest::collection::vec(0_u64..1_000_000_000_000_u64, 1..20), price_value in 1_u64..1_000_000_000_u64) {
            let total_atomic: u128 = parts.iter().map(|part| u128::from(*part)).sum();
            let whole = credit(AtomicAmount::new(U256::from(total_atomic)), price(price_value), 6, 2)
                .expect("bounded input fits")
                .value();
            let split: u64 = parts.iter().map(|part| {
                credit(AtomicAmount::new(U256::from(*part)), price(price_value), 6, 2)
                    .expect("bounded part fits")
                    .value()
            }).sum();
            let loss = whole - split;
            prop_assert!(loss <= u64::try_from(parts.len() - 1).expect("test vector length fits"));
        }

        #[test]
        fn tokens_round_up_to_requested_credit(target in any::<u32>(), price_value in 1_u64..=u64::MAX) {
            let target = MinorAmount::new(u64::from(target));
            let locked_price = price(price_value);
            let tokens = tokens_for_credit(target, locked_price, 18, 2);
            let actual = credit(tokens, locked_price, 18, 2).expect("inverse result fits");
            prop_assert!(actual >= target);
        }

        #[test]
        fn positive_spread_never_increases_price(value in 1_u64..=u64::MAX, spread in 1_u16..=10_000_u16) {
            let spot = price(value);
            let locked = lock_price(spot, Bps::new(spread).expect("spread is bounded"));
            prop_assert!(locked <= spot);
        }
    }

    #[test]
    fn negative_exponent_multiplies_instead_of_dividing() {
        assert_eq!(
            credit(AtomicAmount::new(U256::from(3_u8)), price(2), 2, 12),
            Ok(MinorAmount::new(600))
        );
    }

    #[test]
    fn overflowing_credit_is_rejected() {
        assert_eq!(
            credit(AtomicAmount::new(U256::MAX), price(u64::MAX), 0, 0),
            Err(CreditError::OutOfRange)
        );
    }

    #[test]
    fn lock_price_uses_half_even_rounding() {
        assert_eq!(
            lock_price(price(20_001), Bps::new(10_000).unwrap()).value(),
            10_000
        );
        assert_eq!(
            lock_price(price(20_003), Bps::new(10_000).unwrap()).value(),
            10_002
        );
    }
}

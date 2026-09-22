use topup_core::money::{PRICE_SCALE, ScaledPrice};

use super::PriceError;

const SCALE_FACTOR: u128 = 100_000_000;

pub(super) fn parse_scaled(value: &str) -> Result<ScaledPrice, PriceError> {
    let mut saw_digit = false;
    let mut saw_decimal = false;
    let mut integer = 0_u128;
    let mut fraction = 0_u128;
    let mut fraction_digits = 0_u8;
    let mut first_discarded = None;
    let mut later_discarded_nonzero = false;

    for byte in value.bytes() {
        match byte {
            b'0'..=b'9' => {
                saw_digit = true;
                let digit_byte = byte.checked_sub(b'0').ok_or(PriceError::InvalidPrice)?;
                let digit = u128::from(digit_byte);
                if !saw_decimal {
                    integer = integer
                        .checked_mul(10)
                        .and_then(|current| current.checked_add(digit))
                        .ok_or(PriceError::InvalidPrice)?;
                } else if fraction_digits < PRICE_SCALE {
                    fraction = fraction
                        .checked_mul(10)
                        .and_then(|current| current.checked_add(digit))
                        .ok_or(PriceError::InvalidPrice)?;
                    fraction_digits = fraction_digits
                        .checked_add(1)
                        .ok_or(PriceError::InvalidPrice)?;
                } else if first_discarded.is_none() {
                    first_discarded = Some(digit_byte);
                } else if byte != b'0' {
                    later_discarded_nonzero = true;
                }
            }
            b'.' if !saw_decimal => saw_decimal = true,
            _ => return Err(PriceError::InvalidPrice),
        }
    }
    if !saw_digit {
        return Err(PriceError::InvalidPrice);
    }
    while fraction_digits < PRICE_SCALE {
        fraction = fraction.checked_mul(10).ok_or(PriceError::InvalidPrice)?;
        fraction_digits = fraction_digits
            .checked_add(1)
            .ok_or(PriceError::InvalidPrice)?;
    }
    let mut scaled = integer
        .checked_mul(SCALE_FACTOR)
        .and_then(|current| current.checked_add(fraction))
        .ok_or(PriceError::InvalidPrice)?;
    if let Some(discarded) = first_discarded {
        let round_up =
            discarded > 5 || (discarded == 5 && (later_discarded_nonzero || scaled % 2 == 1));
        if round_up {
            scaled = scaled.checked_add(1).ok_or(PriceError::InvalidPrice)?;
        }
    }
    let scaled = u64::try_from(scaled).map_err(|_| PriceError::InvalidPrice)?;
    ScaledPrice::new(scaled, PRICE_SCALE).map_err(|_| PriceError::InvalidPrice)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_rounds_decimal_strings_with_ties_to_even() {
        for (raw, expected) in [
            ("1", 100_000_000),
            (".25", 25_000_000),
            ("1.234567884", 123_456_788),
            ("1.234567885", 123_456_788),
            ("1.234567895", 123_456_790),
            ("1.2345678851", 123_456_789),
        ] {
            assert_eq!(parse_scaled(raw).expect("valid decimal").value(), expected);
        }
    }

    #[test]
    fn rejects_non_decimal_zero_and_out_of_range_values() {
        for raw in [
            "",
            ".",
            "0",
            "-1",
            "+1",
            "1e2",
            "1.2.3",
            "999999999999999999999",
        ] {
            assert!(parse_scaled(raw).is_err(), "{raw} must fail");
        }
    }
}

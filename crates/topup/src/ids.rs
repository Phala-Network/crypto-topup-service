//! Prefixed public object ids, Stripe's convention: a type prefix and the 32 lowercase hex digits
//! of the object's UUID, such as `qt_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10`.

use uuid::Uuid;

/// Quote ids.
pub const QUOTE: &str = "qt_";
/// Deposit ids; the UUID is the deterministic deposit id (architecture §0).
pub const DEPOSIT: &str = "dep_";
/// Refund ids.
pub const REFUND: &str = "re_";
/// Event ids; the UUID is the event's derived id (architecture §11).
pub const EVENT: &str = "evt_";

/// Formats `id` with `prefix`.
#[must_use]
pub fn format(prefix: &str, id: Uuid) -> String {
    format!("{prefix}{}", id.simple())
}

/// Parses an id of `prefix`; anything but the prefix and 32 lowercase hex digits is `None`.
#[must_use]
pub fn parse(prefix: &str, value: &str) -> Option<Uuid> {
    let hex = value.strip_prefix(prefix)?;
    let canonical = hex.len() == 32
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    canonical.then(|| Uuid::try_parse(hex).ok()).flatten()
}

/// Parses an id of `prefix`, or the bare UUID that logs and responses carried before prefixed
/// ids. Operator input takes either form.
#[must_use]
pub fn parse_or_uuid(prefix: &str, value: &str) -> Option<Uuid> {
    parse(prefix, value).or_else(|| Uuid::try_parse(value).ok())
}

/// Parses a `webhook-id`: an `evt_` id, or the bare UUID of an event written before prefixed ids.
#[must_use]
pub fn parse_event(value: &str) -> Option<Uuid> {
    parse_or_uuid(EVENT, value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_reject_other_forms() {
        let id = Uuid::from_u128(0x0c6e_1d0a_9b3f_4c2e_8d7a_6b5c_4d3e_2f10);
        let quote = format(QUOTE, id);
        assert_eq!(quote, "qt_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10");
        assert_eq!(parse(QUOTE, &quote), Some(id));
        for invalid in [
            "dep_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10",
            "qt_0C6E1D0A9B3F4C2E8D7A6B5C4D3E2F10",
            "qt_0c6e1d0a-9b3f-4c2e-8d7a-6b5c4d3e2f10",
            "qt_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f1",
            "0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10",
        ] {
            assert_eq!(parse(QUOTE, invalid), None, "{invalid}");
        }
        assert_eq!(parse_or_uuid(REFUND, &format(REFUND, id)), Some(id));
        assert_eq!(parse_or_uuid(REFUND, &id.to_string()), Some(id));
        assert_eq!(parse_or_uuid(REFUND, &format(DEPOSIT, id)), None);
    }
}

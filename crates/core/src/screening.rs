//! Pure sanctions, deposit-bound, and pause-scope screening rules.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::money::AtomicAmount;
use crate::route::ScreeningConfig;

/// One provider's direct sanctions-list answer.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SanctionsAnswer {
    /// The source address appears on the sanctions list.
    Sanctioned,
    /// The source address does not appear on the sanctions list.
    Clear,
    /// The provider could not return a usable answer.
    Unavailable,
}

/// Sanctions answers from both providers at the same recorded block.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SanctionsResult {
    /// The answer returned by provider A.
    pub provider_a: SanctionsAnswer,
    /// The answer returned by provider B.
    pub provider_b: SanctionsAnswer,
    /// The block number at which both providers performed the check.
    pub block_number: u64,
}

/// Inclusive per-deposit amount bounds derived from route screening configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Bounds {
    /// The minimum accepted atomic amount.
    pub min_atomic: AtomicAmount,
    /// The maximum accepted atomic amount.
    pub max_atomic: AtomicAmount,
}

impl From<&ScreeningConfig> for Bounds {
    fn from(config: &ScreeningConfig) -> Self {
        Self {
            min_atomic: config.min_deposit_atomic,
            max_atomic: config.max_deposit_atomic,
        }
    }
}

/// A runtime operation that can be paused independently.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseScope {
    /// Creating new rate quotes.
    Quotes,
    /// Issuing or rotating deposit addresses.
    Addresses,
    /// Starting new product settlements.
    Settlement,
    /// Flushing deposited funds to the treasury.
    Flush,
    /// Processing refund requests.
    Refunds,
}

impl PauseScope {
    /// Returns the stable text code used by APIs and persistence.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Quotes => "quotes",
            Self::Addresses => "addresses",
            Self::Settlement => "settlement",
            Self::Flush => "flush",
            Self::Refunds => "refunds",
        }
    }
}

impl FromStr for PauseScope {
    type Err = ParsePauseScopeError;

    fn from_str(code: &str) -> Result<Self, Self::Err> {
        match code {
            "quotes" => Ok(Self::Quotes),
            "addresses" => Ok(Self::Addresses),
            "settlement" => Ok(Self::Settlement),
            "flush" => Ok(Self::Flush),
            "refunds" => Ok(Self::Refunds),
            _ => Err(ParsePauseScopeError {
                code: code.to_owned(),
            }),
        }
    }
}

/// A set of independently paused runtime operations.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct PauseScopes(BTreeSet<PauseScope>);

impl PauseScopes {
    /// Parses a set from stable text codes, rejecting every unknown code.
    pub fn from_codes<I, S>(codes: I) -> Result<Self, ParsePauseScopeError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let scopes = codes
            .into_iter()
            .map(|code| PauseScope::from_str(code.as_ref()))
            .collect::<Result<BTreeSet<_>, _>>()?;
        Ok(Self(scopes))
    }

    /// Returns whether the supplied operation is paused.
    #[must_use]
    pub fn contains(&self, scope: PauseScope) -> bool {
        self.0.contains(&scope)
    }
}

/// An unknown pause-scope text code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsePauseScopeError {
    code: String,
}

impl ParsePauseScopeError {
    /// Returns the rejected text code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }
}

impl Display for ParsePauseScopeError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown pause scope `{}`", self.code)
    }
}

impl Error for ParsePauseScopeError {}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, U256};

    use super::*;

    fn amount(value: u32) -> AtomicAmount {
        AtomicAmount::new(U256::from(value))
    }

    #[test]
    fn bounds_are_derived_from_route_screening_config() {
        let config = ScreeningConfig {
            sanctions_oracle: Address::ZERO,
            min_deposit_atomic: amount(10),
            max_deposit_atomic: amount(20),
            min_credit_minor: 1,
        };

        assert_eq!(
            Bounds::from(&config),
            Bounds {
                min_atomic: amount(10),
                max_atomic: amount(20),
            }
        );
    }

    #[test]
    fn pause_scopes_parse_exact_codes_into_a_set() {
        let scopes = PauseScopes::from_codes([
            "quotes",
            "addresses",
            "settlement",
            "flush",
            "refunds",
            "settlement",
        ])
        .expect("documented scope codes must parse");

        assert!(scopes.contains(PauseScope::Quotes));
        assert!(scopes.contains(PauseScope::Addresses));
        assert!(scopes.contains(PauseScope::Settlement));
        assert!(scopes.contains(PauseScope::Flush));
        assert!(scopes.contains(PauseScope::Refunds));
    }

    #[test]
    fn pause_scopes_reject_unknown_text_codes() {
        let error = PauseScopes::from_codes(["settlement", "payments"])
            .expect_err("unknown scope must fail parsing");

        assert_eq!(error.code(), "payments");
        assert_eq!(error.to_string(), "unknown pause scope `payments`");
    }

    #[test]
    fn pause_scopes_serde_round_trip_uses_exact_codes() {
        let scopes =
            PauseScopes::from_codes(["refunds", "flush", "settlement", "addresses", "quotes"])
                .expect("documented scope codes must parse");

        let json = serde_json::to_string(&scopes).expect("pause scopes must serialize");
        assert_eq!(
            json,
            r#"["quotes","addresses","settlement","flush","refunds"]"#
        );
        assert_eq!(
            serde_json::from_str::<PauseScopes>(&json).expect("pause scopes must deserialize"),
            scopes
        );
    }

    #[test]
    fn pause_scopes_serde_rejects_unknown_codes() {
        let error = serde_json::from_str::<PauseScopes>(r#"["settlement","payments"]"#)
            .expect_err("unknown scope must fail deserialization");

        assert!(error.to_string().contains("unknown variant `payments`"));
    }
}

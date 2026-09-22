//! Pure sanctions, deposit-bound, and pause-scope screening rules.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::deposit::{RejectReason, RetryError, StepOutcome, WaitReason};
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

/// Screens a confirmed deposit for direct sanctions, amount bounds, and settlement pauses.
///
/// Checks are deliberately ordered as sanctions, bounds, then pause. A sanctions hit therefore
/// rejects even when the amount is out of bounds or settlement is paused, and an out-of-bounds
/// deposit rejects instead of waiting behind a pause. Only the `settlement` pause scope gates this
/// step because it holds a deposit before product crediting; the other scopes govern their own
/// operations.
///
/// The sanctions truth table is:
///
/// | Provider A | Provider B | Decision |
/// | --- | --- | --- |
/// | `Sanctioned` | `Sanctioned` | reject |
/// | `Sanctioned` | `Clear` | reject |
/// | `Sanctioned` | `Unavailable` | reject |
/// | `Clear` | `Sanctioned` | reject |
/// | `Clear` | `Clear` | continue |
/// | `Clear` | `Unavailable` | retry |
/// | `Unavailable` | `Sanctioned` | reject |
/// | `Unavailable` | `Clear` | retry |
/// | `Unavailable` | `Unavailable` | retry |
#[must_use]
pub fn screen(
    amount: AtomicAmount,
    sanctions: &SanctionsResult,
    bounds: &Bounds,
    account_scopes: &PauseScopes,
    product_scopes: &PauseScopes,
) -> StepOutcome {
    if matches!(sanctions.provider_a, SanctionsAnswer::Sanctioned)
        || matches!(sanctions.provider_b, SanctionsAnswer::Sanctioned)
    {
        return StepOutcome::Reject(RejectReason::Sanctioned);
    }

    if matches!(sanctions.provider_a, SanctionsAnswer::Unavailable)
        || matches!(sanctions.provider_b, SanctionsAnswer::Unavailable)
    {
        return StepOutcome::Retry {
            error: RetryError::SanctionsInconclusive,
        };
    }

    if amount < bounds.min_atomic || amount > bounds.max_atomic {
        return StepOutcome::Reject(RejectReason::OutOfBounds);
    }

    if account_scopes.contains(PauseScope::Settlement)
        || product_scopes.contains(PauseScope::Settlement)
    {
        return StepOutcome::Wait {
            reason: WaitReason::Paused,
        };
    }

    StepOutcome::Advance
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, U256};

    use super::*;

    fn amount(value: u32) -> AtomicAmount {
        AtomicAmount::new(U256::from(value))
    }

    fn sanctions(provider_a: SanctionsAnswer, provider_b: SanctionsAnswer) -> SanctionsResult {
        SanctionsResult {
            provider_a,
            provider_b,
            block_number: 21_000_000,
        }
    }

    fn bounds() -> Bounds {
        Bounds {
            min_atomic: amount(10),
            max_atomic: amount(20),
        }
    }

    fn scopes(codes: &[&str]) -> PauseScopes {
        PauseScopes::from_codes(codes.iter().copied()).expect("test scope codes must parse")
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

    #[test]
    fn sanctions_pair_truth_table_is_complete() {
        use SanctionsAnswer::{Clear, Sanctioned, Unavailable};

        let reject = StepOutcome::Reject(RejectReason::Sanctioned);
        let retry = StepOutcome::Retry {
            error: RetryError::SanctionsInconclusive,
        };
        let cases = [
            (Sanctioned, Sanctioned, reject),
            (Sanctioned, Clear, reject),
            (Sanctioned, Unavailable, reject),
            (Clear, Sanctioned, reject),
            (Clear, Clear, StepOutcome::Advance),
            (Clear, Unavailable, retry),
            (Unavailable, Sanctioned, reject),
            (Unavailable, Clear, retry),
            (Unavailable, Unavailable, retry),
        ];

        for (provider_a, provider_b, expected) in cases {
            assert_eq!(
                screen(
                    amount(15),
                    &sanctions(provider_a, provider_b),
                    &bounds(),
                    &PauseScopes::default(),
                    &PauseScopes::default(),
                ),
                expected,
                "unexpected decision for {provider_a:?}/{provider_b:?}"
            );
        }
    }

    #[test]
    fn deposit_bounds_are_inclusive() {
        let sanctions = sanctions(SanctionsAnswer::Clear, SanctionsAnswer::Clear);
        let bounds = bounds();
        let no_pauses = PauseScopes::default();
        let cases = [
            (amount(10), StepOutcome::Advance),
            (amount(20), StepOutcome::Advance),
            (amount(9), StepOutcome::Reject(RejectReason::OutOfBounds)),
            (amount(21), StepOutcome::Reject(RejectReason::OutOfBounds)),
        ];

        for (amount, expected) in cases {
            assert_eq!(
                screen(amount, &sanctions, &bounds, &no_pauses, &no_pauses),
                expected
            );
        }
    }

    #[test]
    fn settlement_pause_on_account_or_product_waits() {
        let sanctions = sanctions(SanctionsAnswer::Clear, SanctionsAnswer::Clear);
        let bounds = bounds();
        let active = PauseScopes::default();
        let paused = scopes(&["settlement"]);
        let wait = StepOutcome::Wait {
            reason: WaitReason::Paused,
        };
        let cases = [
            (&active, &active, StepOutcome::Advance),
            (&paused, &active, wait),
            (&active, &paused, wait),
            (&paused, &paused, wait),
        ];

        for (account_scopes, product_scopes, expected) in cases {
            assert_eq!(
                screen(
                    amount(15),
                    &sanctions,
                    &bounds,
                    account_scopes,
                    product_scopes,
                ),
                expected
            );
        }
    }

    #[test]
    fn non_settlement_pause_scopes_do_not_gate_screening() {
        let non_settlement = scopes(&["quotes", "addresses", "flush", "refunds"]);

        assert_eq!(
            screen(
                amount(15),
                &sanctions(SanctionsAnswer::Clear, SanctionsAnswer::Clear),
                &bounds(),
                &non_settlement,
                &non_settlement,
            ),
            StepOutcome::Advance
        );
    }

    #[test]
    fn sanctions_are_evaluated_before_bounds_and_pause() {
        assert_eq!(
            screen(
                amount(9),
                &sanctions(SanctionsAnswer::Clear, SanctionsAnswer::Sanctioned),
                &bounds(),
                &scopes(&["settlement"]),
                &scopes(&["settlement"]),
            ),
            StepOutcome::Reject(RejectReason::Sanctioned)
        );
    }

    #[test]
    fn inconclusive_sanctions_are_evaluated_before_bounds_and_pause() {
        assert_eq!(
            screen(
                amount(9),
                &sanctions(SanctionsAnswer::Unavailable, SanctionsAnswer::Clear),
                &bounds(),
                &scopes(&["settlement"]),
                &scopes(&["settlement"]),
            ),
            StepOutcome::Retry {
                error: RetryError::SanctionsInconclusive,
            }
        );
    }

    #[test]
    fn bounds_are_evaluated_before_pause() {
        assert_eq!(
            screen(
                amount(21),
                &sanctions(SanctionsAnswer::Clear, SanctionsAnswer::Clear),
                &bounds(),
                &scopes(&["settlement"]),
                &scopes(&["settlement"]),
            ),
            StepOutcome::Reject(RejectReason::OutOfBounds)
        );
    }
}

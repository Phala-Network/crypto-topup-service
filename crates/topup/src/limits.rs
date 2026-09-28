//! Caps per account and mode (design §12): open quotes, their credit per account and per
//! customer, and active deposit addresses. `account_limits` holds the operator's per-account
//! values (`POST /v1/admin/accounts/{account}`, `limits`); a column left null, or an account
//! without a row, has the mode's default. There is no global cap: the merchant, not Phala, bears
//! price exposure, and test mode never uses live headroom.

use serde::Serialize;
use sqlx::PgConnection;
use utoipa::ToSchema;

use crate::tenancy::Scope;

/// The caps of one account in one mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub struct Limits {
    /// Open quotes the account may hold in the mode.
    pub max_open_quotes: u64,
    /// Cap on the credit of the account's open quotes in the mode, in cents.
    pub max_open_amount_per_account: u64,
    /// Cap on the credit of one customer's open quotes, in cents.
    pub max_open_amount_per_customer: u64,
    /// Active deposit addresses the account may hold in the mode.
    pub max_active_deposit_addresses: u64,
}

/// Live-mode defaults: 1 000 open quotes and 100 000 active deposit addresses (design §12),
/// $50 000 of open quotes per account and $5 000 per customer.
pub const DEFAULT_LIVE: Limits = Limits {
    max_open_quotes: 1_000,
    max_open_amount_per_account: 5_000_000,
    max_open_amount_per_customer: 500_000,
    max_active_deposit_addresses: 100_000,
};

/// Test-mode defaults: 100 open quotes and 1 000 active deposit addresses (design §12), $10 000
/// of open quotes per account and $5 000 per customer.
pub const DEFAULT_TEST: Limits = Limits {
    max_open_quotes: 100,
    max_open_amount_per_account: 1_000_000,
    max_open_amount_per_customer: 500_000,
    max_active_deposit_addresses: 1_000,
};

impl Limits {
    /// The defaults of a mode.
    #[must_use]
    pub const fn default_for(livemode: bool) -> Self {
        if livemode { DEFAULT_LIVE } else { DEFAULT_TEST }
    }
}

/// An operator's change to one mode's caps; absent fields stay.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LimitsChange {
    /// Open quotes.
    pub max_open_quotes: Option<u64>,
    /// Open credit per account, cents.
    pub max_open_amount_per_account: Option<u64>,
    /// Open credit per customer, cents.
    pub max_open_amount_per_customer: Option<u64>,
    /// Active deposit addresses.
    pub max_active_deposit_addresses: Option<u64>,
}

/// Why a stored or requested cap is unusable.
#[derive(Debug, thiserror::Error)]
pub enum LimitsError {
    /// A value does not fit its column.
    #[error("{0} is out of range")]
    OutOfRange(&'static str),
    /// A stored value violates the table's checks.
    #[error("account_limits holds an invalid value")]
    Invalid,
    /// PostgreSQL failed.
    #[error("{0}")]
    Database(#[from] sqlx::Error),
}

type Row = (Option<i32>, Option<i64>, Option<i64>, Option<i32>);

/// The effective caps of `scope`: the account's values over the mode's defaults.
pub async fn load(connection: &mut PgConnection, scope: Scope) -> Result<Limits, LimitsError> {
    let row: Option<Row> = sqlx::query_as(
        "SELECT max_open_quotes, max_open_minor_account, max_open_minor_customer, \
                max_active_deposit_addresses \
         FROM account_limits WHERE account_id = $1 AND livemode = $2",
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .fetch_optional(connection)
    .await?;
    let defaults = Limits::default_for(scope.livemode());
    let Some((quotes, account, customer, addresses)) = row else {
        return Ok(defaults);
    };
    let stored = |value: Option<i64>, default: u64| {
        value.map_or(Ok(default), |value| {
            u64::try_from(value).map_err(|_| LimitsError::Invalid)
        })
    };
    Ok(Limits {
        max_open_quotes: stored(quotes.map(i64::from), defaults.max_open_quotes)?,
        max_open_amount_per_account: stored(account, defaults.max_open_amount_per_account)?,
        max_open_amount_per_customer: stored(customer, defaults.max_open_amount_per_customer)?,
        max_active_deposit_addresses: stored(
            addresses.map(i64::from),
            defaults.max_active_deposit_addresses,
        )?,
    })
}

/// Stores `change` for `scope`, keeping the columns it leaves absent.
pub async fn update(
    connection: &mut PgConnection,
    scope: Scope,
    change: &LimitsChange,
) -> Result<(), LimitsError> {
    let count = |value: Option<u64>, field: &'static str| {
        value
            .map(|value| match i32::try_from(value) {
                Ok(value) if value > 0 => Ok(value),
                _ => Err(LimitsError::OutOfRange(field)),
            })
            .transpose()
    };
    let amount = |value: Option<u64>, field: &'static str| {
        value
            .map(|value| i64::try_from(value).map_err(|_| LimitsError::OutOfRange(field)))
            .transpose()
    };
    sqlx::query(
        r#"
        INSERT INTO account_limits (
            account_id, livemode, max_open_quotes, max_open_minor_account,
            max_open_minor_customer, max_active_deposit_addresses
        )
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (account_id, livemode) DO UPDATE SET
            max_open_quotes = COALESCE(EXCLUDED.max_open_quotes, account_limits.max_open_quotes),
            max_open_minor_account =
                COALESCE(EXCLUDED.max_open_minor_account, account_limits.max_open_minor_account),
            max_open_minor_customer =
                COALESCE(EXCLUDED.max_open_minor_customer, account_limits.max_open_minor_customer),
            max_active_deposit_addresses = COALESCE(
                EXCLUDED.max_active_deposit_addresses,
                account_limits.max_active_deposit_addresses
            )
        "#,
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(count(change.max_open_quotes, "max_open_quotes")?)
    .bind(amount(
        change.max_open_amount_per_account,
        "max_open_amount_per_account",
    )?)
    .bind(amount(
        change.max_open_amount_per_customer,
        "max_open_amount_per_customer",
    )?)
    .bind(count(
        change.max_active_deposit_addresses,
        "max_active_deposit_addresses",
    )?)
    .execute(connection)
    .await?;
    Ok(())
}

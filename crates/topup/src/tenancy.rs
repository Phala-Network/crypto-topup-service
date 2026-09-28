//! Tenant isolation (design D13): the server-built [`Scope`] every merchant query takes, and the
//! one authorization table that roles and API keys share.
//!
//! A merchant request is scoped to one account and one mode. The scope is built by the server from
//! the authenticated credential, never from a client-supplied account id, and every query that
//! reads or writes a tenant table on behalf of a merchant filters on both of its values. A row of
//! another account or mode is therefore indistinguishable from a missing one: `404`. Tables
//! without `account_id` are reached only through a scoped parent (`transitions` and
//! `pending_transfers` through `deposits` and `addresses`, `webhook_deliveries` through `events`).
//!
//! The chain workers (scanner, pump, finality watch, reconciler, webhook delivery) and the
//! operator's admin API act for the platform, not for a merchant, and read across accounts; they
//! never serve a merchant request.

use sqlx::PgExecutor;
use uuid::Uuid;

/// The account and mode a merchant request acts in.
///
/// Built only from an authenticated credential (the API key, or today the account's request
/// signing key; later the dashboard session's membership). It has no parser and no deserializer,
/// so request data cannot become a scope.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Scope {
    account_id: Uuid,
    livemode: bool,
}

impl Scope {
    /// The scope of a credential that authenticated for `account_id` in the given mode.
    #[must_use]
    pub const fn new(account_id: Uuid, livemode: bool) -> Self {
        Self {
            account_id,
            livemode,
        }
    }

    /// The account every scoped query filters on.
    #[must_use]
    pub const fn account_id(self) -> Uuid {
        self.account_id
    }

    /// The mode every scoped query filters on.
    #[must_use]
    pub const fn livemode(self) -> bool {
        self.livemode
    }
}

/// A permission in the authorization table (`permissions`, design D13).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Permission {
    /// Read the account and its configuration.
    AccountRead,
    /// Read quotes.
    QuotesRead,
    /// Create and cancel quotes.
    QuotesWrite,
    /// Read deposits.
    DepositsRead,
    /// Read refunds.
    RefundsRead,
    /// Request refunds.
    RefundsWrite,
}

impl Permission {
    /// The permission's code in the authorization table.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::AccountRead => "account.read",
            Self::QuotesRead => "quotes.read",
            Self::QuotesWrite => "quotes.write",
            Self::DepositsRead => "deposits.read",
            Self::RefundsRead => "refunds.read",
            Self::RefundsWrite => "refunds.write",
        }
    }
}

/// Who holds permissions: a dashboard role or an API key kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Principal {
    /// A secret API key, which holds every API permission. The account's request signing key
    /// counts as one until API keys replace it (design PR 6).
    SecretKey,
    /// A restricted API key, limited further by its own grants (design PR 14).
    RestrictedKey,
    /// A member's role in the account.
    Role(Role),
}

/// A member's role (design D6), Stripe's team roles.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    /// Holds every permission.
    Owner,
    /// Holds every permission but ownership.
    Administrator,
    /// Reads, and writes keys, endpoints, and refunds.
    Developer,
    /// Reads only.
    ViewOnly,
}

impl Principal {
    /// The principal's code in the authorization table.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::SecretKey => "key:secret",
            Self::RestrictedKey => "key:restricted",
            Self::Role(Role::Owner) => "role:owner",
            Self::Role(Role::Administrator) => "role:administrator",
            Self::Role(Role::Developer) => "role:developer",
            Self::Role(Role::ViewOnly) => "role:view_only",
        }
    }
}

/// Whether the authorization table grants `permission` to `principal`.
pub async fn holds<'e>(
    executor: impl PgExecutor<'e>,
    principal: Principal,
    permission: Permission,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM permissions WHERE principal = $1 AND permission = $2)",
    )
    .bind(principal.code())
    .bind(permission.code())
    .fetch_one(executor)
    .await
}

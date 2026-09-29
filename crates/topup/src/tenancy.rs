//! Tenant isolation (design D13): the server-built [`Scope`] every merchant query takes, and the
//! permissions each API key kind holds.
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

use uuid::Uuid;

/// The account and mode a merchant request acts in.
///
/// Built only from an authenticated API key: the key's account and mode. It has no parser and
/// no deserializer, so request data cannot become a scope.
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

/// A permission a merchant route requires (design D13).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Permission {
    /// Read the account and its configuration.
    AccountRead,
    /// Change the account's own settings, such as rolling its webhook key.
    AccountWrite,
    /// Read quotes.
    QuotesRead,
    /// Create and cancel quotes.
    QuotesWrite,
    /// Read deposits.
    DepositsRead,
    /// Update deposits' metadata.
    DepositsWrite,
    /// Read deposit addresses.
    DepositAddressesRead,
    /// Create and rotate deposit addresses.
    DepositAddressesWrite,
    /// Read refunds.
    RefundsRead,
    /// Request refunds.
    RefundsWrite,
    /// List and read the API keys of the account and mode.
    ApiKeysRead,
    /// Create, roll, and revoke the API keys of the account and mode.
    ApiKeysWrite,
    /// Read the treasuries of the account and mode.
    TreasuryRead,
    /// Prove, change, and cancel changes of the treasuries of the account and mode.
    TreasuryWrite,
    /// List and read webhook endpoints.
    EndpointsRead,
    /// Create, update, delete, and test webhook endpoints, and resend events to them.
    EndpointsWrite,
    /// List and read events.
    EventsRead,
    /// Read the balance held in forwarders and the sweeps that moved it.
    SweepsRead,
    /// Read every issued forwarder with its `(factory, salt, treasury)`, the export.
    ForwardersRead,
}

impl Permission {
    /// Every permission, in the order of the enum.
    pub const ALL: [Self; 19] = [
        Self::AccountRead,
        Self::AccountWrite,
        Self::QuotesRead,
        Self::QuotesWrite,
        Self::DepositsRead,
        Self::DepositsWrite,
        Self::DepositAddressesRead,
        Self::DepositAddressesWrite,
        Self::RefundsRead,
        Self::RefundsWrite,
        Self::ApiKeysRead,
        Self::ApiKeysWrite,
        Self::TreasuryRead,
        Self::TreasuryWrite,
        Self::EndpointsRead,
        Self::EndpointsWrite,
        Self::EventsRead,
        Self::SweepsRead,
        Self::ForwardersRead,
    ];

    /// The permission of `code`, such as `quotes.write`.
    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|permission| permission.code() == code)
    }

    /// The `read` permission of the same resource, which a `write` grant includes as Stripe's
    /// restricted keys do; `None` for a `read` permission or a resource without one.
    #[must_use]
    pub fn read_of_write(self) -> Option<Self> {
        let resource = self.code().strip_suffix(".write")?;
        Self::parse(&format!("{resource}.read"))
    }

    /// The permission's code, as a restricted key's grants name it.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::AccountRead => "account.read",
            Self::AccountWrite => "account.write",
            Self::QuotesRead => "quotes.read",
            Self::QuotesWrite => "quotes.write",
            Self::DepositsRead => "deposits.read",
            Self::DepositsWrite => "deposits.write",
            Self::DepositAddressesRead => "deposit_addresses.read",
            Self::DepositAddressesWrite => "deposit_addresses.write",
            Self::RefundsRead => "refunds.read",
            Self::RefundsWrite => "refunds.write",
            Self::ApiKeysRead => "api_keys.read",
            Self::ApiKeysWrite => "api_keys.write",
            Self::TreasuryRead => "treasury.read",
            Self::TreasuryWrite => "treasury.write",
            Self::EndpointsRead => "endpoints.read",
            Self::EndpointsWrite => "endpoints.write",
            Self::EventsRead => "events.read",
            Self::SweepsRead => "sweeps.read",
            Self::ForwardersRead => "forwarders.read",
        }
    }
}

/// Who holds permissions: an API key kind. There are no users or roles (design D8): the
/// operator creates accounts, and accounts act through API keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Principal {
    /// A secret API key, which holds every API permission.
    SecretKey,
    /// A restricted API key, limited further by its own grants (design PR 12). It never holds
    /// `api_keys.write`, `treasury.write`, `endpoints.write`, or `account.write`: keys,
    /// treasuries, webhook endpoints, webhook keys, and account settings need a secret key.
    RestrictedKey,
}

impl Principal {
    /// Whether the key kind holds `permission`.
    #[must_use]
    pub const fn holds(self, permission: Permission) -> bool {
        match self {
            Self::SecretKey => true,
            Self::RestrictedKey => !matches!(
                permission,
                Permission::ApiKeysWrite
                    | Permission::TreasuryWrite
                    | Permission::EndpointsWrite
                    | Permission::AccountWrite
            ),
        }
    }
}

//! Per-account, per-mode webhook keys (design D11).
//!
//! Each account has one ed25519 Standard Webhooks `v1a` key per mode, derived from dstack KMS at
//! `settlement/{account}/{live|test}/v{version}` ([`WebhookKeyId`]); the service stores only the
//! version. `accounts.webhook_key_version` holds each mode's current version. A roll bumps it and
//! keeps the previous version in `retiring_webhook_keys` for an overlap of at most 7 days, during
//! which every delivery carries one signature per version (the Standard Webhooks multi-signature
//! rotation) and attestation binds every version.

use chrono::{DateTime, Duration, Utc};
use sqlx::{PgConnection, PgPool};
use topup_core::WebhookKeyId;

use crate::audit::{self, Actor};
use crate::db::{self, EventObject, NewOutboxEvent};
use crate::routes::RouteSet;
use crate::tenancy::Scope;

/// The longest overlap a roll may keep the previous key signing: 7 days, as an API key roll.
pub const MAX_ROLL_OVERLAP: Duration = Duration::days(7);

/// One version of an account's key in one mode.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyVersion {
    /// Key version, from 1.
    pub version: u32,
    /// When a previous version stops signing; `None` for the current version.
    pub expires_at: Option<DateTime<Utc>>,
}

/// The keys that sign an account's deliveries in one mode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebhookKeys {
    /// The account's `acct_` id.
    pub account: String,
    /// The mode.
    pub livemode: bool,
    /// The current version first, then previous versions still in their overlap, newest first.
    pub versions: Vec<KeyVersion>,
}

impl WebhookKeys {
    /// The key ids of every version, in order; `None` if the stored account id cannot name a key.
    #[must_use]
    pub fn ids(&self) -> Option<Vec<WebhookKeyId>> {
        self.versions
            .iter()
            .map(|version| WebhookKeyId::new(&self.account, self.livemode, version.version))
            .collect()
    }
}

/// A failure to roll a webhook key.
#[derive(Debug, thiserror::Error)]
pub enum WebhookKeyError {
    /// The overlap is negative or longer than [`MAX_ROLL_OVERLAP`].
    #[error("webhook key overlap is out of range")]
    InvalidExpiry,
    /// The account does not exist.
    #[error("account not found")]
    NotFound,
    /// The version cannot grow further.
    #[error("webhook key version is exhausted")]
    VersionExhausted,
    /// PostgreSQL failed.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

const fn mode(livemode: bool) -> &'static str {
    if livemode { "live" } else { "test" }
}

/// The keys that currently sign the scope's deliveries; `None` if the account does not exist.
pub async fn active(
    connection: &mut PgConnection,
    scope: Scope,
) -> Result<Option<WebhookKeys>, sqlx::Error> {
    let Some((account, current)) = sqlx::query_as::<_, (String, i32)>(
        "SELECT public_id, (webhook_key_version ->> $2)::integer FROM accounts WHERE id = $1",
    )
    .bind(scope.account_id())
    .bind(mode(scope.livemode()))
    .fetch_optional(&mut *connection)
    .await?
    else {
        return Ok(None);
    };
    let retiring = sqlx::query_as::<_, (i32, DateTime<Utc>)>(
        r#"
        SELECT version, expires_at FROM retiring_webhook_keys
        WHERE account_id = $1 AND livemode = $2 AND expires_at > now() AND version <> $3
        ORDER BY version DESC
        "#,
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(current)
    .fetch_all(&mut *connection)
    .await?;
    let version =
        |value: i32| u32::try_from(value).map_err(|error| sqlx::Error::Decode(Box::new(error)));
    let mut versions = vec![KeyVersion {
        version: version(current)?,
        expires_at: None,
    }];
    for (retired, expires_at) in retiring {
        versions.push(KeyVersion {
            version: version(retired)?,
            expires_at: Some(expires_at),
        });
    }
    Ok(Some(WebhookKeys {
        account,
        livemode: scope.livemode(),
        versions,
    }))
}

/// Rolls the scope's webhook key: the next version signs from now on, and the current one keeps
/// signing beside it for `expires_in` (at most 7 days), or stops at once for zero. Previous
/// versions still in an overlap stop no later than the new one. Audited, and announced as
/// `account.updated` in the scope's mode, signed by every key still signing.
pub async fn roll(
    pool: &PgPool,
    routes: &RouteSet,
    scope: Scope,
    expires_in: Duration,
    actor: &Actor,
) -> Result<WebhookKeys, WebhookKeyError> {
    if expires_in < Duration::zero() || expires_in > MAX_ROLL_OVERLAP {
        return Err(WebhookKeyError::InvalidExpiry);
    }
    let mut transaction = pool.begin().await?;
    let object = EventObject::Account(scope.account_id());
    let (account, current) = sqlx::query_as::<_, (String, i32)>(
        "SELECT public_id, (webhook_key_version ->> $2)::integer FROM accounts \
         WHERE id = $1 FOR UPDATE",
    )
    .bind(scope.account_id())
    .bind(mode(scope.livemode()))
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or(WebhookKeyError::NotFound)?;
    let before = db::render(&mut transaction, routes, scope, object).await?;
    let next = current
        .checked_add(1)
        .filter(|next| *next <= 999_999_999)
        .ok_or(WebhookKeyError::VersionExhausted)?;
    sqlx::query(
        "DELETE FROM retiring_webhook_keys WHERE account_id = $1 AND livemode = $2 \
         AND expires_at <= now()",
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "UPDATE retiring_webhook_keys SET expires_at = LEAST(expires_at, now() + $3) \
         WHERE account_id = $1 AND livemode = $2",
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(expires_in)
    .execute(&mut *transaction)
    .await?;
    if !expires_in.is_zero() {
        sqlx::query(
            "INSERT INTO retiring_webhook_keys (account_id, livemode, version, expires_at) \
             VALUES ($1, $2, $3, now() + $4)",
        )
        .bind(scope.account_id())
        .bind(scope.livemode())
        .bind(current)
        .bind(expires_in)
        .execute(&mut *transaction)
        .await?;
    }
    sqlx::query(
        "UPDATE accounts SET webhook_key_version = \
         jsonb_set(webhook_key_version, ARRAY[$2], to_jsonb($3::integer)) WHERE id = $1",
    )
    .bind(scope.account_id())
    .bind(mode(scope.livemode()))
    .bind(next)
    .execute(&mut *transaction)
    .await?;
    audit::insert(
        &mut *transaction,
        &audit::Entry {
            account_id: Some(scope.account_id()),
            actor,
            action: "webhook_key.roll",
            subject: &format!("webhook_key:{account}/{}", mode(scope.livemode())),
            reason: &serde_json::json!({
                "from_version": current,
                "to_version": next,
                "overlap_seconds": expires_in.num_seconds(),
            })
            .to_string(),
        },
    )
    .await?;
    let event = NewOutboxEvent::new("account.updated", scope, object, actor);
    db::enqueue_in(&mut transaction, routes, &event, Some(&before)).await?;
    let keys = active(&mut transaction, scope)
        .await?
        .ok_or(WebhookKeyError::NotFound)?;
    transaction.commit().await?;
    Ok(keys)
}

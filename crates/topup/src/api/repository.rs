//! API-specific PostgreSQL queries.
//!
//! Merchant queries take the request's [`Scope`] and filter every tenant table on its account and
//! mode, so another tenant's row answers like a missing one. The admin functions below them act
//! for the operator across accounts and are reachable only through the admin-key router.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use alloy_primitives::{Address as EvmAddress, B256, U256};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::postgres::PgRow;
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder, Row};
use topup_core::money::AtomicAmount;
use topup_core::refund::{RefundDeposit, refund_eligibility};
use topup_core::route::RouteFile;
use uuid::Uuid;

use crate::api_keys::{self, IssuedKey};
use crate::audit::{self, Actor};
use crate::db::Customer;
use crate::tenancy::Scope;

use super::auth::VerifiedSignature;
use super::error::ApiError;
use super::models::{
    DailyReportResponse, DepositEventResponse, DepositResponse, DepositTransitionResponse,
    NudgeResponse, OutboxReplayResponse, ReconciliationBlockLiftResponse,
    ReconciliationBlockReport, RouteDailyReport, SupportDepositResponse,
};

/// Records a verified request signature exactly once within the acceptance window.
pub async fn record_signature(
    pool: &PgPool,
    signature: &VerifiedSignature,
) -> Result<(), ApiError> {
    let mut transaction = pool.begin().await?;
    sqlx::query("DELETE FROM seen_signatures WHERE created < now() - interval '5 minutes'")
        .execute(&mut *transaction)
        .await?;
    let inserted = sqlx::query(
        r#"
        INSERT INTO seen_signatures (kid, signature_hash, created)
        VALUES ($1, $2, $3)
        ON CONFLICT DO NOTHING
        "#,
    )
    .bind(&signature.kid)
    .bind(signature.signature_hash.as_slice())
    .bind(signature.created)
    .execute(&mut *transaction)
    .await?;
    if inserted.rows_affected() == 0 {
        return Err(ApiError::signature_replayed());
    }
    transaction.commit().await?;
    Ok(())
}

/// An account as the admin API shows it.
#[derive(FromRow)]
pub struct AdminAccount {
    /// Account id.
    pub id: Uuid,
    /// `acct_…`.
    pub public_id: String,
    /// Display name.
    pub name: String,
    /// `{name, email}`.
    pub contact: Value,
    /// `{reference, reviewed_at, reviewed_by}`.
    pub due_diligence: Value,
    /// Live mode.
    pub charges_enabled: bool,
    /// Restricted for review.
    pub restricted: bool,
    /// Account-level pause scopes.
    pub paused_scopes: Vec<String>,
    /// Creation time.
    pub created_at: DateTime<Utc>,
}

/// The columns of [`AdminAccount`].
macro_rules! admin_account_columns {
    () => {
        "id, public_id, name, contact, due_diligence, charges_enabled, restricted, \
         paused_scopes, created_at"
    };
}

/// An account with the keys the admin request issued, each with its secret.
pub struct IssuedAccount {
    /// The account.
    pub account: AdminAccount,
    /// Keys issued by the request.
    pub api_keys: Vec<IssuedKey>,
}

/// A new account's values (design D8).
pub struct NewAccount<'a> {
    /// Display name.
    pub name: &'a str,
    /// `{name, email}`.
    pub contact: Value,
    /// `{reference, reviewed_at, reviewed_by}`.
    pub due_diligence: Value,
    /// Live mode.
    pub charges_enabled: bool,
    /// Webhook URL registered in each enabled mode, until design PR 8.
    pub webhook_url: Option<&'a str>,
}

/// Creates an account with the first secret key of test mode and, with `charges_enabled`, of
/// live mode, a webhook endpoint per enabled mode when a URL is given, and the audit rows and
/// `api_key.created` events, in one transaction.
pub async fn create_account(
    pool: &PgPool,
    account: &NewAccount<'_>,
    actor: &Actor,
    reason: &str,
) -> Result<IssuedAccount, ApiError> {
    let mut transaction = pool.begin().await?;
    let created = sqlx::query_as::<_, AdminAccount>(concat!(
        "INSERT INTO accounts (id, name, contact, due_diligence, charges_enabled) \
         VALUES ($1, $2, $3, $4, $5) RETURNING ",
        admin_account_columns!()
    ))
    .bind(Uuid::new_v4())
    .bind(account.name)
    .bind(&account.contact)
    .bind(&account.due_diligence)
    .bind(account.charges_enabled)
    .fetch_one(&mut *transaction)
    .await?;
    audit::insert(
        &mut *transaction,
        &audit::Entry {
            account_id: Some(created.id),
            actor,
            action: "account.create",
            subject: &format!("account:{}", created.public_id),
            reason: &serde_json::json!({
                "reason": reason,
                "charges_enabled": account.charges_enabled,
                "due_diligence": account.due_diligence,
            })
            .to_string(),
        },
    )
    .await?;
    let modes: &[bool] = if account.charges_enabled {
        &[false, true]
    } else {
        &[false]
    };
    let mut issued = Vec::with_capacity(modes.len());
    for &livemode in modes {
        if let Some(url) = account.webhook_url {
            insert_endpoint(&mut transaction, created.id, livemode, url).await?;
        }
        issued.push(
            api_keys::create_in(
                &mut transaction,
                Scope::new(created.id, livemode),
                "",
                actor,
                "the account's first key",
            )
            .await
            .map_err(super::keys::map_error)?,
        );
    }
    transaction.commit().await?;
    Ok(IssuedAccount {
        account: created,
        api_keys: issued,
    })
}

/// An admin update of an account; absent fields stay.
pub struct AccountChanges<'a> {
    /// Live mode.
    pub charges_enabled: Option<bool>,
    /// Restricted for review.
    pub restricted: Option<bool>,
    /// `{name, email}`.
    pub contact: Option<Value>,
    /// Webhook URL of every endpoint, until design PR 8.
    pub webhook_url: Option<&'a str>,
}

/// Applies `changes`, with an audit row and an `account.updated` event per enabled mode, in one
/// transaction; an update that changes nothing writes nothing. Enabling live mode for an account
/// without a live key issues its first live key, and registers a live webhook endpoint with the
/// test endpoint's URL when there is none.
pub async fn update_account(
    pool: &PgPool,
    account_id: Uuid,
    changes: &AccountChanges<'_>,
    actor: &Actor,
    reason: &str,
) -> Result<IssuedAccount, ApiError> {
    let mut transaction = pool.begin().await?;
    let before = sqlx::query_as::<_, AdminAccount>(concat!(
        "SELECT ",
        admin_account_columns!(),
        " FROM accounts WHERE id = $1 FOR UPDATE"
    ))
    .bind(account_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let urls: Vec<(bool, String)> = sqlx::query_as(
        "SELECT livemode, url FROM webhook_endpoints WHERE account_id = $1 ORDER BY created_at, id",
    )
    .bind(account_id)
    .fetch_all(&mut *transaction)
    .await?;
    let after = sqlx::query_as::<_, AdminAccount>(concat!(
        "UPDATE accounts SET charges_enabled = COALESCE($2, charges_enabled), \
         restricted = COALESCE($3, restricted), contact = COALESCE($4, contact) \
         WHERE id = $1 RETURNING ",
        admin_account_columns!()
    ))
    .bind(account_id)
    .bind(changes.charges_enabled)
    .bind(changes.restricted)
    .bind(&changes.contact)
    .fetch_one(&mut *transaction)
    .await?;
    let url_changed = changes
        .webhook_url
        .is_some_and(|url| urls.is_empty() || urls.iter().any(|(_, stored)| stored != url));
    let unchanged = after.charges_enabled == before.charges_enabled
        && after.restricted == before.restricted
        && after.contact == before.contact
        && !url_changed;
    if unchanged {
        transaction.commit().await?;
        return Ok(IssuedAccount {
            account: after,
            api_keys: Vec::new(),
        });
    }
    let modes: &[bool] = if after.charges_enabled {
        &[false, true]
    } else {
        &[false]
    };
    let url = changes
        .webhook_url
        .map(str::to_owned)
        .or_else(|| urls.first().map(|(_, url)| url.clone()));
    if let Some(url) = &url {
        sqlx::query("UPDATE webhook_endpoints SET url = $2 WHERE account_id = $1")
            .bind(account_id)
            .bind(url)
            .execute(&mut *transaction)
            .await?;
        for &livemode in modes {
            let registered = urls.iter().any(|(mode, _)| *mode == livemode);
            if !registered {
                insert_endpoint(&mut transaction, account_id, livemode, url).await?;
            }
        }
    }
    let mut issued = Vec::new();
    if after.charges_enabled && !before.charges_enabled {
        let has_live_key: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM api_keys \
             WHERE account_id = $1 AND livemode AND revoked_at IS NULL)",
        )
        .bind(account_id)
        .fetch_one(&mut *transaction)
        .await?;
        if !has_live_key {
            issued.push(
                api_keys::create_in(
                    &mut transaction,
                    Scope::new(account_id, true),
                    "",
                    actor,
                    "the account's first live key",
                )
                .await
                .map_err(super::keys::map_error)?,
            );
        }
    }
    audit::insert(
        &mut *transaction,
        &audit::Entry {
            account_id: Some(account_id),
            actor,
            action: "account.update",
            subject: &format!("account:{}", after.public_id),
            reason: &serde_json::json!({
                "reason": reason,
                "replaced": {
                    "charges_enabled": before.charges_enabled,
                    "restricted": before.restricted,
                    "contact": before.contact,
                    "webhook_url": urls.first().map(|(_, url)| url),
                },
            })
            .to_string(),
        },
    )
    .await?;
    for &livemode in modes {
        crate::db::enqueue_in(
            &mut transaction,
            &crate::db::NewOutboxEvent {
                id: Uuid::new_v4(),
                event_type: "account.updated".to_owned(),
                account_id,
                livemode,
                object: crate::db::EventObject::Account(account_id),
                next_attempt_at: Utc::now(),
                actor: api_keys::event_actor(actor),
            },
        )
        .await?;
    }
    transaction.commit().await?;
    Ok(IssuedAccount {
        account: after,
        api_keys: issued,
    })
}

async fn insert_endpoint(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    account_id: Uuid,
    livemode: bool,
    url: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO webhook_endpoints (id, account_id, livemode, url) VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::new_v4())
    .bind(account_id)
    .bind(livemode)
    .bind(url)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

/// Finds or creates the customer `client_reference_id` of `scope`.
pub async fn ensure_customer(
    pool: &PgPool,
    scope: Scope,
    client_reference_id: &str,
) -> Result<Customer, ApiError> {
    sqlx::query(
        r#"
        INSERT INTO customers (id, account_id, livemode, client_reference_id)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (account_id, livemode, client_reference_id) DO NOTHING
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(client_reference_id)
    .execute(pool)
    .await?;
    find_customer(pool, scope, client_reference_id)
        .await?
        .ok_or_else(ApiError::internal)
}

/// Finds the customer `client_reference_id` of `scope`.
pub async fn find_customer(
    pool: &PgPool,
    scope: Scope,
    client_reference_id: &str,
) -> Result<Option<Customer>, ApiError> {
    let row = sqlx::query_as::<_, CustomerRow>(
        r#"
        SELECT id, account_id, livemode, client_reference_id, paused_scopes
        FROM customers
        WHERE account_id = $1 AND livemode = $2 AND client_reference_id = $3
        "#,
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(client_reference_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

/// A refund request: the deposit, destination, and amount (the unrefunded remainder when absent).
pub struct NewRefund<'a> {
    /// The request's scope.
    pub scope: Scope,
    /// Deposit to refund.
    pub deposit_id: Uuid,
    /// The deposit's route, or the fallback route of an unrouted deposit.
    pub route: &'a RouteFile,
    /// Customer-controlled destination, already screened.
    pub destination: EvmAddress,
    /// Requested amount; `None` refunds the remainder.
    pub amount: Option<AtomicAmount>,
    /// The refund's validated metadata.
    pub metadata: &'a super::metadata::Metadata,
    /// Audit actor.
    pub actor: &'a Actor,
}

/// Creates a `pending` refund after every policy check and returns its id: the deposit is final
/// and refundable, nothing pauses refunds, and the amount fits the deposit's remainder after its
/// pending and succeeded refunds, which it then reserves.
pub async fn request_refund(pool: &PgPool, refund: &NewRefund<'_>) -> Result<Uuid, ApiError> {
    let mut transaction = pool.begin().await?;
    let row = sqlx::query(
        r#"
        SELECT deposit.amount_atomic::text AS amount_atomic, deposit.state, deposit.reason,
               deposit.chain_id, deposit.final_at IS NOT NULL AS is_final,
               customer.paused_scopes AS customer_scopes,
               account.paused_scopes AS account_scopes,
               COALESCE(route_pause.paused_scopes, '{}') AS route_scopes
        FROM deposits AS deposit
        JOIN customers AS customer ON customer.id = deposit.customer_id
        JOIN accounts AS account ON account.id = deposit.account_id
        LEFT JOIN route_pauses AS route_pause ON route_pause.route = COALESCE(deposit.route, $4)
        WHERE deposit.id = $1 AND deposit.account_id = $2 AND deposit.livemode = $3
        FOR UPDATE OF deposit, customer
        "#,
    )
    .bind(refund.deposit_id)
    .bind(refund.scope.account_id())
    .bind(refund.scope.livemode())
    .bind(&refund.route.route)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| ApiError::not_found().with_param("deposit"))?;

    let customer_scopes: Vec<String> = row.try_get("customer_scopes")?;
    let account_scopes: Vec<String> = row.try_get("account_scopes")?;
    let route_scopes: Vec<String> = row.try_get("route_scopes")?;
    if [&customer_scopes, &account_scopes, &route_scopes]
        .into_iter()
        .any(|scopes| scopes.iter().any(|scope| scope == "refunds"))
    {
        return Err(ApiError::paused("refund requests are paused"));
    }

    let deposit_amount = parse_atomic(row.try_get::<String, _>("amount_atomic")?)?;
    refund_eligibility(refund_deposit_from_row(&row, refund.route)?)
        .map_err(|_| ApiError::deposit_not_refundable())?;
    // Nothing is paid back for a deposit that could still be reversed.
    if !row.try_get::<bool, _>("is_final")? {
        return Err(ApiError::deposit_not_final());
    }
    let chain_id: i64 = row.try_get("chain_id")?;

    let reserved = sqlx::query_scalar::<_, String>(
        r#"
        SELECT COALESCE(sum(amount_atomic), 0)::text
        FROM refunds
        WHERE deposit_id = $1 AND status IN ('pending', 'succeeded')
        "#,
    )
    .bind(refund.deposit_id)
    .fetch_one(&mut *transaction)
    .await?;
    let remaining = deposit_amount
        .checked_sub(parse_atomic(reserved)?)
        .ok_or_else(ApiError::internal)?;
    let amount = refund.amount.map_or(remaining, AtomicAmount::value);
    if amount.is_zero() {
        return Err(ApiError::amount_too_small(
            "amount_atomic",
            "nothing is left to refund",
        ));
    }
    if amount > remaining {
        return Err(ApiError::amount_too_large(
            "amount_atomic",
            format!("at most {remaining} base units are left to refund"),
        ));
    }

    let refund_id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO refunds
            (id, account_id, livemode, chain_id, deposit_id, amount_atomic, destination_address,
             status, metadata)
        VALUES ($1, $2, $3, $4, $5, $6::text::numeric, $7, 'pending', $8)
        "#,
    )
    .bind(refund_id)
    .bind(refund.scope.account_id())
    .bind(refund.scope.livemode())
    .bind(chain_id)
    .bind(refund.deposit_id)
    .bind(amount.to_string())
    .bind(format!("{:#x}", refund.destination))
    .bind(sqlx::types::Json(refund.metadata))
    .execute(&mut *transaction)
    .await?;
    insert_audit_tx(
        &mut transaction,
        Some(refund.scope.account_id()),
        refund.actor,
        "refund_requested",
        &format!("refund:{refund_id}"),
    )
    .await?;
    transaction.commit().await?;
    Ok(refund_id)
}

/// Attaches the merchant's refund transaction to a pending refund; the verification worker checks
/// it at finality. Repeating the same transaction is a no-op; another one is `409`, since only the
/// verification outcome or a cancel ends a pending refund.
pub async fn mark_refund_paid(
    pool: &PgPool,
    scope: Scope,
    refund_id: Uuid,
    tx_hash: B256,
    log_index: Option<u64>,
    actor: &Actor,
) -> Result<(), ApiError> {
    let mut transaction = pool.begin().await?;
    let (status, current_hash, current_log) = locked_refund(&mut transaction, scope, refund_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let tx_hash = format!("{tx_hash:#x}");
    let log_index = log_index
        .map(|index| {
            i64::try_from(index)
                .map_err(|_| ApiError::invalid_param("log_index", "log_index is too large"))
        })
        .transpose()?;
    if let Some(current) = current_hash {
        let same = current == tx_hash && log_index.is_none_or(|index| current_log == Some(index));
        if same && status != "canceled" {
            return Ok(());
        }
        return Err(ApiError::refund_unexpected_state(if status == "pending" {
            "already marked paid with another transaction".to_owned()
        } else {
            status
        }));
    }
    if status != "pending" {
        return Err(ApiError::refund_unexpected_state(status));
    }
    let updated = sqlx::query(
        r#"
        UPDATE refunds
        SET tx_hash = $2, log_index = $3, next_check_at = now(), updated_at = now()
        WHERE id = $1
        "#,
    )
    .bind(refund_id)
    .bind(&tx_hash)
    .bind(log_index)
    .execute(&mut *transaction)
    .await;
    match updated {
        Ok(_) => {}
        Err(sqlx::Error::Database(error))
            if error.constraint() == Some("refunds_transfer_unique") =>
        {
            return Err(ApiError::transfer_already_used());
        }
        Err(error) => return Err(error.into()),
    }
    insert_audit_tx_with_reason(
        &mut transaction,
        Some(scope.account_id()),
        actor,
        "refund_marked_paid",
        &format!("refund:{refund_id}"),
        &tx_hash,
    )
    .await?;
    transaction.commit().await?;
    Ok(())
}

/// Cancels a pending refund, releasing its reservation; canceling a canceled refund is a no-op.
pub async fn cancel_refund(
    pool: &PgPool,
    scope: Scope,
    refund_id: Uuid,
    actor: &Actor,
) -> Result<(), ApiError> {
    let mut transaction = pool.begin().await?;
    let (status, _, _) = locked_refund(&mut transaction, scope, refund_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    match status.as_str() {
        "canceled" => return Ok(()),
        "pending" => {}
        _ => return Err(ApiError::refund_unexpected_state(status)),
    }
    sqlx::query("UPDATE refunds SET status = 'canceled', updated_at = now() WHERE id = $1")
        .bind(refund_id)
        .execute(&mut *transaction)
        .await?;
    insert_audit_tx(
        &mut transaction,
        Some(scope.account_id()),
        actor,
        "refund_canceled",
        &format!("refund:{refund_id}"),
    )
    .await?;
    transaction.commit().await?;
    Ok(())
}

/// The scope's refund `refund_id`, locked: its status, transaction, and log.
async fn locked_refund(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    scope: Scope,
    refund_id: Uuid,
) -> Result<Option<(String, Option<String>, Option<i64>)>, ApiError> {
    Ok(sqlx::query_as(
        r#"
        SELECT status, tx_hash, log_index
        FROM refunds
        WHERE id = $1 AND account_id = $2 AND livemode = $3
        FOR UPDATE
        "#,
    )
    .bind(refund_id)
    .bind(scope.account_id())
    .bind(scope.livemode())
    .fetch_optional(&mut **transaction)
    .await?)
}

/// One deposit with its transitions and webhook events, for the operator.
pub async fn admin_deposit(
    pool: &PgPool,
    deposit_id: Uuid,
) -> Result<Option<SupportDepositResponse>, ApiError> {
    let mut query = deposit_query();
    query.push(" WHERE deposit.id = ").push_bind(deposit_id);
    Ok(fetch_support_page(pool, query).await?.into_iter().next())
}

/// Makes a deposit immediately claimable without changing its state.
pub async fn nudge_deposit(
    pool: &PgPool,
    deposit_id: Uuid,
    actor: &Actor,
) -> Result<NudgeResponse, ApiError> {
    let mut transaction = pool.begin().await?;
    let (next_attempt_at, account_id) = sqlx::query_as::<_, (DateTime<Utc>, Uuid)>(
        r#"
        UPDATE deposits SET next_attempt_at = now() WHERE id = $1
        RETURNING next_attempt_at, account_id
        "#,
    )
    .bind(deposit_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(ApiError::not_found)?;
    insert_audit_tx(
        &mut transaction,
        Some(account_id),
        actor,
        "deposit_nudged",
        &format!("deposit:{deposit_id}"),
    )
    .await?;
    transaction.commit().await?;
    Ok(NudgeResponse {
        deposit_id: crate::ids::format(crate::ids::DEPOSIT, deposit_id),
        next_attempt_at,
    })
}

/// Lifts a reconciliation block and appends an audit row, carrying the block, in the same
/// transaction.
///
/// Lifting is manual (architecture §13): the service does not re-check the finding first. If it
/// still reproduces, the reconciler writes the block again on its next round. A repeated lift of
/// a lifted block returns the first lift without another audit row.
pub async fn lift_reconciliation_block(
    pool: &PgPool,
    block_key: &str,
    actor: &Actor,
    reason: &str,
) -> Result<ReconciliationBlockLiftResponse, ApiError> {
    let mut transaction = pool.begin().await?;
    let subject = format!("reconciliation_block:{block_key}");
    let lifted = sqlx::query_as::<_, ReconciliationBlockRow>(
        r#"
        DELETE FROM reconciliation_blocks
        WHERE block_key = $1
        RETURNING block_key, scope, chain_id, address_id, check_name, reason, created_at
        "#,
    )
    .bind(block_key)
    .fetch_optional(&mut *transaction)
    .await?;
    if let Some(block) = lifted {
        let evidence = serde_json::json!({
            "reason": reason,
            "block": {
                "scope": block.scope,
                "chain_id": block.chain_id,
                "address_id": block.address_id,
                "check": block.check_name,
                "reason": block.reason,
                "created_at": block.created_at,
            },
        });
        insert_audit_tx_with_reason(
            &mut transaction,
            None,
            actor,
            "reconciliation_block.lift",
            &subject,
            &evidence.to_string(),
        )
        .await?;
    }
    let lifted_at = sqlx::query_scalar::<_, DateTime<Utc>>(
        r#"
        SELECT created_at FROM audit
        WHERE action = 'reconciliation_block.lift' AND subject = $1
        ORDER BY created_at DESC
        LIMIT 1
        "#,
    )
    .bind(&subject)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(ApiError::not_found)?;
    transaction.commit().await?;
    Ok(ReconciliationBlockLiftResponse {
        block_key: block_key.to_owned(),
        lifted_at,
    })
}

/// Queues one webhook event for delivery again to every endpoint it was queued for, and appends
/// an audit row in the same transaction.
///
/// A delivered delivery is marked undelivered and a pending one becomes due now; the event's
/// identifier and payload never change. Repeating the request while every delivery is already
/// due changes nothing and writes no audit row.
pub async fn replay_outbox_event(
    pool: &PgPool,
    event_id: Uuid,
    actor: &Actor,
    reason: &str,
) -> Result<OutboxReplayResponse, ApiError> {
    let mut transaction = pool.begin().await?;
    let (event_type, account_id) = sqlx::query_as::<_, (String, Uuid)>(
        "SELECT type, account_id FROM events WHERE id = $1 FOR UPDATE",
    )
    .bind(event_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let rescheduled = sqlx::query(
        r#"
        UPDATE webhook_deliveries
        SET next_attempt_at = now(), delivered_at = NULL
        WHERE event_id = $1 AND NOT (delivered_at IS NULL AND next_attempt_at <= now())
        "#,
    )
    .bind(event_id)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    if rescheduled > 0 {
        insert_audit_tx_with_reason(
            &mut transaction,
            Some(account_id),
            actor,
            "outbox.replay",
            &format!("event:{event_id}"),
            reason,
        )
        .await?;
    }
    let next_attempt_at = sqlx::query_scalar::<_, Option<DateTime<Utc>>>(
        "SELECT min(next_attempt_at) FROM webhook_deliveries WHERE event_id = $1",
    )
    .bind(event_id)
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(OutboxReplayResponse {
        event_id: crate::outbox::webhook_id(event_id),
        event_type,
        next_attempt_at,
    })
}

/// Adds or removes a customer's pause scopes and appends an audit row in the same transaction.
pub async fn mutate_customer_scopes(
    pool: &PgPool,
    customer: &Customer,
    requested: &[String],
    pause: bool,
    actor: &Actor,
) -> Result<Vec<String>, ApiError> {
    let mut transaction = pool.begin().await?;
    let current: Vec<String> =
        sqlx::query_scalar("SELECT paused_scopes FROM customers WHERE id = $1 FOR UPDATE")
            .bind(customer.id)
            .fetch_one(&mut *transaction)
            .await?;
    let updated = updated_scopes(current, requested, pause);
    sqlx::query("UPDATE customers SET paused_scopes = $2 WHERE id = $1")
        .bind(customer.id)
        .bind(&updated)
        .execute(&mut *transaction)
        .await?;
    insert_audit_tx(
        &mut transaction,
        Some(customer.account_id),
        actor,
        if pause { "pause" } else { "resume" },
        &format!("customer:{}", customer.id),
    )
    .await?;
    transaction.commit().await?;
    Ok(updated)
}

/// Adds or removes route pause scopes and appends the required administrative audit row.
pub async fn mutate_route_scopes(
    pool: &PgPool,
    route: &str,
    requested: &[String],
    pause: bool,
    actor: &Actor,
) -> Result<Vec<String>, ApiError> {
    let mut transaction = pool.begin().await?;
    sqlx::query(
        "INSERT INTO route_pauses (route, paused_scopes) VALUES ($1, '{}') ON CONFLICT DO NOTHING",
    )
    .bind(route)
    .execute(&mut *transaction)
    .await?;
    let current: Vec<String> =
        sqlx::query_scalar("SELECT paused_scopes FROM route_pauses WHERE route = $1 FOR UPDATE")
            .bind(route)
            .fetch_one(&mut *transaction)
            .await?;
    let updated = updated_scopes(current, requested, pause);
    sqlx::query("UPDATE route_pauses SET paused_scopes = $2 WHERE route = $1")
        .bind(route)
        .bind(&updated)
        .execute(&mut *transaction)
        .await?;
    insert_audit_tx(
        &mut transaction,
        None,
        actor,
        if pause { "pause" } else { "resume" },
        &format!("route:{route}"),
    )
    .await?;
    transaction.commit().await?;
    Ok(updated)
}

/// Returns active pause scopes for a route, or an empty set when it has no pause row.
pub async fn route_paused_scopes(pool: &PgPool, route: &str) -> Result<Vec<String>, ApiError> {
    Ok(
        sqlx::query_scalar("SELECT paused_scopes FROM route_pauses WHERE route = $1")
            .bind(route)
            .fetch_optional(pool)
            .await?
            .unwrap_or_default(),
    )
}

#[derive(FromRow)]
struct CustomerRow {
    id: Uuid,
    account_id: Uuid,
    livemode: bool,
    client_reference_id: String,
    paused_scopes: Vec<String>,
}

impl From<CustomerRow> for Customer {
    fn from(row: CustomerRow) -> Self {
        Self {
            id: row.id,
            account_id: row.account_id,
            livemode: row.livemode,
            client_reference_id: row.client_reference_id,
            paused_scopes: row.paused_scopes,
        }
    }
}

#[derive(FromRow)]
struct DepositViewRow {
    id: Uuid,
    account: String,
    livemode: bool,
    external_id: String,
    chain_id: i64,
    tx_hash: String,
    receipt_log_index: i64,
    log_index: i64,
    block_number: i64,
    block_time: DateTime<Utc>,
    final_at: Option<DateTime<Utc>>,
    address: String,
    quote_id: Option<Uuid>,
    deposit_address_id: Option<Uuid>,
    route: Option<String>,
    route_version: Option<i64>,
    asset_contract: String,
    from_address: String,
    amount_atomic: String,
    state: String,
    valuation_at: Option<DateTime<Utc>>,
    price_scaled: Option<String>,
    price_source: Option<String>,
    credit_minor: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(FromRow)]
struct DepositTransitionRow {
    id: Uuid,
    deposit_id: Uuid,
    from_state: String,
    to_state: String,
    attempt: i32,
    evidence: Value,
    created_at: DateTime<Utc>,
}

impl From<DepositTransitionRow> for DepositTransitionResponse {
    fn from(row: DepositTransitionRow) -> Self {
        Self {
            id: row.id,
            from_state: row.from_state,
            to_state: row.to_state,
            attempt: row.attempt,
            evidence: row.evidence,
            created_at: row.created_at,
        }
    }
}

#[derive(FromRow)]
struct DepositEventRow {
    id: Uuid,
    deposit_id: Uuid,
    event_type: String,
    created_at: DateTime<Utc>,
    delivered_at: Option<DateTime<Utc>>,
}

impl From<DepositEventRow> for DepositEventResponse {
    fn from(row: DepositEventRow) -> Self {
        Self {
            id: crate::outbox::webhook_id(row.id),
            event_type: row.event_type,
            created_at: row.created_at,
            delivered_at: row.delivered_at,
        }
    }
}

#[derive(FromRow)]
struct ReconciliationBlockRow {
    block_key: String,
    scope: String,
    chain_id: i64,
    address_id: Option<Uuid>,
    check_name: String,
    reason: String,
    created_at: DateTime<Utc>,
}

impl TryFrom<ReconciliationBlockRow> for ReconciliationBlockReport {
    type Error = ApiError;

    fn try_from(row: ReconciliationBlockRow) -> Result<Self, Self::Error> {
        Ok(Self {
            block_key: row.block_key,
            scope: row.scope,
            chain_id: count_u64(row.chain_id)?,
            address_id: row.address_id,
            check: row.check_name,
            reason: row.reason,
            created_at: row.created_at,
        })
    }
}

impl TryFrom<DepositViewRow> for DepositResponse {
    type Error = ApiError;

    fn try_from(row: DepositViewRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: crate::ids::format(crate::ids::DEPOSIT, row.id),
            account: row.account,
            livemode: row.livemode,
            external_id: row.external_id,
            chain_id: u64::try_from(row.chain_id).map_err(|_| ApiError::internal())?,
            tx_hash: row.tx_hash,
            receipt_log_index: u64::try_from(row.receipt_log_index)
                .map_err(|_| ApiError::internal())?,
            log_index: u64::try_from(row.log_index).map_err(|_| ApiError::internal())?,
            block_number: u64::try_from(row.block_number).map_err(|_| ApiError::internal())?,
            block_time: row.block_time,
            final_at: row.final_at,
            address: row.address,
            lock_ref: row.quote_id.map(crate::locks::quote_id),
            deposit_address: row
                .deposit_address_id
                .map(crate::deposit_addresses::public_id),
            route: row.route,
            route_version: row
                .route_version
                .map(u64::try_from)
                .transpose()
                .map_err(|_| ApiError::internal())?,
            asset_contract: row.asset_contract,
            from_address: row.from_address,
            amount_atomic: row.amount_atomic,
            state: row.state,
            valuation_at: row.valuation_at,
            price_scaled: row.price_scaled,
            price_source: row.price_source,
            credit_minor: row.credit_minor,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

fn deposit_query() -> QueryBuilder<Postgres> {
    QueryBuilder::new(
        r#"
        SELECT deposit.id, account.public_id AS account, deposit.livemode,
               customer.client_reference_id AS external_id, deposit.chain_id, deposit.tx_hash,
               deposit.receipt_log_index, deposit.log_index,
               deposit.block_number, deposit.block_time, deposit.final_at, address.address,
               address.quote_id, address.deposit_address_id,
               deposit.route, deposit.route_version, deposit.asset_contract,
               deposit.from_address, deposit.amount_atomic::text AS amount_atomic,
               deposit.state, deposit.valuation_at, deposit.price_scaled::text AS price_scaled,
               deposit.price_source, deposit.credit_minor::text AS credit_minor,
               deposit.created_at, deposit.updated_at
        FROM deposits AS deposit
        JOIN accounts AS account ON account.id = deposit.account_id
        JOIN customers AS customer ON customer.id = deposit.customer_id
        JOIN addresses AS address ON address.id = deposit.address_id
        "#,
    )
}

async fn fetch_support_page(
    pool: &PgPool,
    mut query: QueryBuilder<Postgres>,
) -> Result<Vec<SupportDepositResponse>, ApiError> {
    query.push(" ORDER BY deposit.created_at DESC, deposit.id DESC LIMIT 50");
    let rows = query
        .build_query_as::<DepositViewRow>()
        .fetch_all(pool)
        .await?;
    let deposits = rows
        .into_iter()
        .map(|row| Ok((row.id, DepositResponse::try_from(row)?)))
        .collect::<Result<Vec<(Uuid, DepositResponse)>, ApiError>>()?;
    let ids = deposits.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let transitions = sqlx::query_as::<_, DepositTransitionRow>(
        r#"
        SELECT id, deposit_id, from_state, to_state, attempt, evidence, created_at
        FROM transitions
        WHERE deposit_id = ANY($1)
        ORDER BY created_at, id
        "#,
    )
    .bind(&ids)
    .fetch_all(pool)
    .await?;
    let mut by_deposit = BTreeMap::<Uuid, Vec<DepositTransitionResponse>>::new();
    for transition in transitions {
        by_deposit
            .entry(transition.deposit_id)
            .or_default()
            .push(transition.into());
    }
    let events = sqlx::query_as::<_, DepositEventRow>(
        r#"
        SELECT event.id, event.object_id AS deposit_id, event.type AS event_type,
               event.created AS created_at,
               (SELECT CASE WHEN bool_and(delivery.delivered_at IS NOT NULL)
                            THEN max(delivery.delivered_at) END
                FROM webhook_deliveries AS delivery
                WHERE delivery.event_id = event.id) AS delivered_at
        FROM events AS event
        WHERE event.object_type = 'deposit' AND event.object_id = ANY($1)
        ORDER BY event.created, event.id
        "#,
    )
    .bind(&ids)
    .fetch_all(pool)
    .await?;
    let mut events_by_deposit = BTreeMap::<Uuid, Vec<DepositEventResponse>>::new();
    for event in events {
        events_by_deposit
            .entry(event.deposit_id)
            .or_default()
            .push(event.into());
    }
    Ok(deposits
        .into_iter()
        .map(|(id, deposit)| SupportDepositResponse {
            timeline: by_deposit.remove(&id).unwrap_or_default(),
            events: events_by_deposit.remove(&id).unwrap_or_default(),
            deposit,
        })
        .collect())
}

/// Computes the daily finance report entirely from persisted integer values.
pub async fn daily_report(
    pool: &PgPool,
    routes: &[RouteFile],
    generated_at: DateTime<Utc>,
) -> Result<DailyReportResponse, ApiError> {
    let mut reports = BTreeMap::<String, RouteDailyReport>::new();
    for route in routes {
        reports
            .entry(route.route.clone())
            .or_insert_with(|| empty_route_report(route));
    }
    for row in
        sqlx::query("SELECT DISTINCT chain_id, asset_contract FROM deposits WHERE route IS NULL")
            .fetch_all(pool)
            .await?
    {
        let chain_id = count_u64(row.try_get("chain_id")?)?;
        let asset_contract: String = row.try_get("asset_contract")?;
        let key = unrouted_key(chain_id, &asset_contract);
        reports
            .entry(key.clone())
            .or_insert_with(|| empty_unrouted_report(key, chain_id, asset_contract));
    }

    for row in sqlx::query(
        r#"
        SELECT COALESCE(route, 'unrouted:' || chain_id::text || ':' || asset_contract) AS report_key,
               state, count(*)::bigint AS count
        FROM deposits
        GROUP BY report_key, state
        "#,
    )
    .fetch_all(pool)
    .await?
    {
        let route: String = row.try_get("report_key")?;
        let state: String = row.try_get("state")?;
        let count = count_u64(row.try_get("count")?)?;
        if let Some(report) = reports.get_mut(&route) {
            report.deposits_by_state.insert(state, count);
        }
    }

    for row in sqlx::query(
        r#"
        WITH held AS (
            SELECT COALESCE(route, 'unrouted:' || chain_id::text || ':' || asset_contract)
                       AS report_key,
                   chain_id, asset_contract, sum(amount_atomic) AS deposited
            FROM deposits
            WHERE state <> 'reversed'
            GROUP BY report_key, chain_id, asset_contract
        ), swept AS (
            SELECT chain_id, token, sum(amount_atomic) AS flushed
            FROM flushed
            GROUP BY chain_id, token
        )
        SELECT held.report_key,
               GREATEST(sum(held.deposited - COALESCE(swept.flushed, 0)), 0)::text AS amount
        FROM held
        LEFT JOIN swept
            ON swept.chain_id = held.chain_id AND swept.token = held.asset_contract
        GROUP BY held.report_key
        "#,
    )
    .fetch_all(pool)
    .await?
    {
        let route: String = row.try_get("report_key")?;
        if let Some(report) = reports.get_mut(&route) {
            report.unflushed_balance_atomic = row.try_get("amount")?;
        }
    }

    for row in sqlx::query(
        r#"
        SELECT route, COALESCE(sum(amount_atomic), 0)::text AS amount
        FROM quotes
        WHERE consumed_by IS NULL AND expires_at > $1
        GROUP BY route
        "#,
    )
    .bind(generated_at)
    .fetch_all(pool)
    .await?
    {
        let route: String = row.try_get("route")?;
        if let Some(report) = reports.get_mut(&route) {
            report.open_rate_lock_exposure_atomic = row.try_get("amount")?;
        }
    }

    for row in sqlx::query(
        r#"
        WITH succeeded AS (
            SELECT deposit_id, sum(amount_atomic) AS amount
            FROM refunds
            WHERE status = 'succeeded'
            GROUP BY deposit_id
        )
        SELECT COALESCE(
                   deposit.route,
                   'unrouted:' || deposit.chain_id::text || ':' || deposit.asset_contract
               ) AS report_key,
               COALESCE(sum(GREATEST(deposit.amount_atomic - COALESCE(succeeded.amount, 0), 0)), 0)::text AS amount
        FROM deposits AS deposit
        LEFT JOIN succeeded ON succeeded.deposit_id = deposit.id
        WHERE deposit.state = 'rejected'
        GROUP BY report_key
        "#,
    )
    .fetch_all(pool)
    .await?
    {
        let route: String = row.try_get("report_key")?;
        if let Some(report) = reports.get_mut(&route) {
            report.rejected_holds_atomic = row.try_get("amount")?;
        }
    }

    for row in sqlx::query(
        r#"
        SELECT COALESCE(
                   deposit.route,
                   'unrouted:' || deposit.chain_id::text || ':' || deposit.asset_contract
               ) AS report_key,
               count(DISTINCT event.id)::bigint AS count,
               COALESCE(
                   max(extract(epoch FROM ($1 - event.created)))::bigint, 0
               ) AS max_age_seconds
        FROM events AS event
        JOIN webhook_deliveries AS delivery ON delivery.event_id = event.id
        JOIN deposits AS deposit ON deposit.id = event.object_id
        WHERE event.type = 'deposit.credited' AND delivery.delivered_at IS NULL
        GROUP BY report_key
        "#,
    )
    .bind(generated_at)
    .fetch_all(pool)
    .await?
    {
        let route: String = row.try_get("report_key")?;
        let count = count_u64(row.try_get("count")?)?;
        let max_age = count_u64(row.try_get::<i64, _>("max_age_seconds")?.max(0))?;
        if let Some(report) = reports.get_mut(&route) {
            report.credited_undelivered = count;
            report.credited_undelivered_max_age_seconds = max_age;
        }
    }

    for row in sqlx::query(
        r#"
        SELECT COALESCE(
                   deposit.route,
                   'unrouted:' || deposit.chain_id::text || ':' || deposit.asset_contract
               ) AS report_key,
               refund.status, count(*)::bigint AS count
        FROM refunds AS refund
        JOIN deposits AS deposit ON deposit.id = refund.deposit_id
        GROUP BY report_key, refund.status
        "#,
    )
    .fetch_all(pool)
    .await?
    {
        let route: String = row.try_get("report_key")?;
        let status: String = row.try_get("status")?;
        let count = count_u64(row.try_get("count")?)?;
        if let Some(report) = reports.get_mut(&route) {
            report.refunds_by_status.insert(status, count);
        }
    }

    for row in sqlx::query(
        r#"
        SELECT COALESCE(
                   deposit.route,
                   'unrouted:' || deposit.chain_id::text || ':' || deposit.asset_contract
               ) AS report_key,
               deposit.state,
               max(GREATEST(
                   0,
                   floor(extract(epoch FROM ($1 - COALESCE(state_entry.entered_at, deposit.created_at))))
               ))::bigint AS age_seconds
        FROM deposits AS deposit
        LEFT JOIN LATERAL (
            SELECT max(transition.created_at) AS entered_at
            FROM transitions AS transition
            WHERE transition.deposit_id = deposit.id
              AND transition.to_state = deposit.state
              AND transition.from_state <> transition.to_state
        ) AS state_entry ON true
        GROUP BY report_key, deposit.state
        "#,
    )
    .bind(generated_at)
    .fetch_all(pool)
    .await?
    {
        let route: String = row.try_get("report_key")?;
        let state: String = row.try_get("state")?;
        let age = count_u64(row.try_get("age_seconds")?)?;
        if let Some(report) = reports.get_mut(&route) {
            report.age_in_state_max_seconds.insert(state, age);
        }
    }

    let exposure_minor = sqlx::query_scalar(
        r#"
        SELECT COALESCE(sum(credit_minor), 0)::text
        FROM quotes
        WHERE status = 'open' AND exposure_reserved
        "#,
    )
    .fetch_one(pool)
    .await?;

    let reconciliation_blocks = sqlx::query_as::<_, ReconciliationBlockRow>(
        r#"
        SELECT block_key, scope, chain_id, address_id, check_name, reason, created_at
        FROM reconciliation_blocks
        ORDER BY block_key
        "#,
    )
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(TryInto::try_into)
    .collect::<Result<_, _>>()?;

    Ok(DailyReportResponse {
        generated_at,
        exposure_minor: Some(exposure_minor),
        routes: reports.into_values().collect(),
        reconciliation: None,
        reconciliation_blocks,
    })
}

fn empty_route_report(route: &RouteFile) -> RouteDailyReport {
    RouteDailyReport {
        route: route.route.clone(),
        chain_id: route.chain.chain_id,
        asset_contract: format!("{:#x}", route.asset.contract),
        unflushed_balance_atomic: "0".to_owned(),
        open_rate_lock_exposure_atomic: "0".to_owned(),
        rejected_holds_atomic: "0".to_owned(),
        deposits_by_state: zero_counts(&[
            "detected",
            "confirmed",
            "credited",
            "swept",
            "rejected",
            "reversed",
        ]),
        credited_undelivered: 0,
        credited_undelivered_max_age_seconds: 0,
        refunds_by_status: zero_counts(&["pending", "succeeded", "failed", "canceled"]),
        age_in_state_max_seconds: zero_counts(&[
            "detected",
            "confirmed",
            "credited",
            "swept",
            "rejected",
        ]),
    }
}

fn empty_unrouted_report(route: String, chain_id: u64, asset_contract: String) -> RouteDailyReport {
    RouteDailyReport {
        route,
        chain_id,
        asset_contract,
        unflushed_balance_atomic: "0".to_owned(),
        open_rate_lock_exposure_atomic: "0".to_owned(),
        rejected_holds_atomic: "0".to_owned(),
        deposits_by_state: zero_counts(&[
            "detected",
            "confirmed",
            "credited",
            "swept",
            "rejected",
            "reversed",
        ]),
        credited_undelivered: 0,
        credited_undelivered_max_age_seconds: 0,
        refunds_by_status: zero_counts(&["pending", "succeeded", "failed", "canceled"]),
        age_in_state_max_seconds: zero_counts(&[
            "detected",
            "confirmed",
            "credited",
            "swept",
            "rejected",
        ]),
    }
}

fn unrouted_key(chain_id: u64, asset_contract: &str) -> String {
    format!("unrouted:{chain_id}:{asset_contract}")
}

fn zero_counts(codes: &[&str]) -> BTreeMap<String, u64> {
    codes.iter().map(|code| ((*code).to_owned(), 0)).collect()
}

fn count_u64(value: i64) -> Result<u64, ApiError> {
    u64::try_from(value).map_err(|_| ApiError::internal())
}

fn parse_atomic(value: String) -> Result<U256, ApiError> {
    U256::from_str(&value).map_err(|_| ApiError::internal())
}

fn refund_deposit_from_row(row: &PgRow, route: &RouteFile) -> Result<RefundDeposit, ApiError> {
    let state: String = row.try_get("state")?;
    let reason: Option<String> = row.try_get("reason")?;
    Ok(RefundDeposit {
        state: crate::db::parse_state(&state).map_err(|_| ApiError::internal())?,
        reason: crate::db::parse_reason(reason.as_deref()).map_err(|_| ApiError::internal())?,
        amount: AtomicAmount::new(parse_atomic(row.try_get("amount_atomic")?)?),
        min_refund: route.asset.min_refund_atomic,
    })
}

fn updated_scopes(current: Vec<String>, requested: &[String], pause: bool) -> Vec<String> {
    let mut scopes = current.into_iter().collect::<BTreeSet<_>>();
    for scope in requested {
        if pause {
            scopes.insert(scope.clone());
        } else {
            scopes.remove(scope);
        }
    }
    scopes.into_iter().collect()
}

async fn insert_audit_tx(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    account_id: Option<Uuid>,
    actor: &Actor,
    action: &str,
    subject: &str,
) -> Result<(), ApiError> {
    insert_audit_tx_with_reason(
        transaction,
        account_id,
        actor,
        action,
        subject,
        "signed API request",
    )
    .await
}

async fn insert_audit_tx_with_reason(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    account_id: Option<Uuid>,
    actor: &Actor,
    action: &str,
    subject: &str,
    reason: &str,
) -> Result<(), ApiError> {
    audit::insert(
        &mut **transaction,
        &audit::Entry {
            account_id,
            actor,
            action,
            subject,
            reason,
        },
    )
    .await?;
    Ok(())
}

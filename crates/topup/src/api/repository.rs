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

use crate::audit::{self, Actor};
use crate::db::{Account, Customer};
use crate::tenancy::Scope;

use super::auth::VerifiedSignature;
use super::error::ApiError;
use super::models::{
    AdminRefundResponse, DailyReportResponse, DepositEventResponse, DepositResponse,
    DepositTransitionResponse, NudgeResponse, OutboxReplayResponse,
    ReconciliationBlockLiftResponse, ReconciliationBlockReport, RouteDailyReport,
    SupportDepositResponse,
};

/// The columns of [`IssuedAccount`] for account `$1`.
macro_rules! issued_account_select {
    () => {
        r#"
        SELECT account.id, account.public_id, account.name, account.paused_scopes,
               signing_key.livemode, signing_key.public_key,
               (SELECT endpoint.url FROM webhook_endpoints AS endpoint
                WHERE endpoint.account_id = account.id AND endpoint.livemode = signing_key.livemode
                ORDER BY endpoint.created_at, endpoint.id LIMIT 1) AS webhook_url
        FROM accounts AS account
        JOIN request_signing_keys AS signing_key ON signing_key.account_id = account.id
        WHERE account.id = $1"#
    };
}

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

/// An account's request signing key and the mode it selects.
pub struct SigningKey {
    /// The account the key belongs to.
    pub account: Account,
    /// The mode the key acts in.
    pub livemode: bool,
    /// Standard base64 of the ed25519 public key.
    pub public_key: String,
}

/// Finds the request signing key of `account_id`.
pub async fn find_signing_key(
    pool: &PgPool,
    account_id: Uuid,
) -> Result<Option<SigningKey>, ApiError> {
    let row = sqlx::query(
        r#"
        SELECT account.id, account.public_id, account.name, account.paused_scopes,
               signing_key.livemode, signing_key.public_key
        FROM request_signing_keys AS signing_key
        JOIN accounts AS account ON account.id = signing_key.account_id
        WHERE account.id = $1
        "#,
    )
    .bind(account_id)
    .fetch_optional(pool)
    .await?;
    row.map(|row| {
        Ok(SigningKey {
            account: account_from_row(&row)?,
            livemode: row.try_get("livemode")?,
            public_key: row.try_get("public_key")?,
        })
    })
    .transpose()
}

/// An account as the admin API shows it: with its signing key and webhook endpoint.
pub struct IssuedAccount {
    /// The account.
    pub account: Account,
    /// The mode its signing key acts in.
    pub livemode: bool,
    /// Standard base64 of its ed25519 public key.
    pub public_key: String,
    /// Its webhook endpoint's URL.
    pub webhook_url: String,
}

/// Issues an account with its request signing key and one webhook endpoint in the key's mode,
/// and appends an audit row, in one transaction. Transitional until self-serve signup and API
/// keys (design PRs 5 and 6).
pub async fn create_account(
    pool: &PgPool,
    name: &str,
    livemode: bool,
    public_key: &str,
    webhook_url: &str,
    actor: &Actor,
) -> Result<IssuedAccount, ApiError> {
    let mut transaction = pool.begin().await?;
    let row = sqlx::query(
        r#"
        INSERT INTO accounts (id, name)
        VALUES ($1, $2)
        RETURNING id, public_id, name, paused_scopes
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(name)
    .fetch_one(&mut *transaction)
    .await?;
    let account = account_from_row(&row)?;
    sqlx::query(
        "INSERT INTO request_signing_keys (account_id, livemode, public_key) VALUES ($1, $2, $3)",
    )
    .bind(account.id)
    .bind(livemode)
    .bind(public_key)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "INSERT INTO webhook_endpoints (id, account_id, livemode, url) VALUES ($1, $2, $3, $4)",
    )
    .bind(Uuid::new_v4())
    .bind(account.id)
    .bind(livemode)
    .bind(webhook_url)
    .execute(&mut *transaction)
    .await?;
    audit::insert(
        &mut *transaction,
        &audit::Entry {
            account_id: Some(account.id),
            actor,
            action: "account.issue",
            subject: &format!("account:{}", account.public_id),
            reason: "signed API request",
        },
    )
    .await?;
    transaction.commit().await?;
    Ok(IssuedAccount {
        account,
        livemode,
        public_key: public_key.to_owned(),
        webhook_url: webhook_url.to_owned(),
    })
}

/// Replaces an issued account's verification key and the URL of its webhook endpoints in the
/// key's mode, and appends an audit row carrying the reason and the replaced values, in the same
/// transaction.
///
/// Merchant requests read the key on every request, so the replaced key stops verifying when this
/// commits: a hard cut, with no overlap. Repeating the request with the stored values changes
/// nothing and writes no audit row.
pub async fn update_account(
    pool: &PgPool,
    account_id: Uuid,
    public_key: &str,
    webhook_url: &str,
    actor: &Actor,
    reason: &str,
) -> Result<IssuedAccount, ApiError> {
    let mut transaction = pool.begin().await?;
    let existing = issued_account(&mut transaction, account_id, true)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if existing.public_key == public_key && existing.webhook_url == webhook_url {
        transaction.commit().await?;
        return Ok(existing);
    }
    sqlx::query("UPDATE request_signing_keys SET public_key = $2 WHERE account_id = $1")
        .bind(account_id)
        .bind(public_key)
        .execute(&mut *transaction)
        .await?;
    sqlx::query("UPDATE webhook_endpoints SET url = $3 WHERE account_id = $1 AND livemode = $2")
        .bind(account_id)
        .bind(existing.livemode)
        .bind(webhook_url)
        .execute(&mut *transaction)
        .await?;
    let evidence = serde_json::json!({
        "reason": reason,
        "replaced": {"public_key": existing.public_key, "webhook_url": existing.webhook_url},
    });
    audit::insert(
        &mut *transaction,
        &audit::Entry {
            account_id: Some(account_id),
            actor,
            action: "account.update",
            subject: &format!("account:{}", existing.account.public_id),
            reason: &evidence.to_string(),
        },
    )
    .await?;
    let updated = issued_account(&mut transaction, account_id, false)
        .await?
        .ok_or_else(ApiError::internal)?;
    transaction.commit().await?;
    Ok(updated)
}

async fn issued_account(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    account_id: Uuid,
    lock: bool,
) -> Result<Option<IssuedAccount>, ApiError> {
    let query = if lock {
        concat!(
            issued_account_select!(),
            " FOR UPDATE OF account, signing_key"
        )
    } else {
        issued_account_select!()
    };
    let row = sqlx::query(query)
        .bind(account_id)
        .fetch_optional(&mut **transaction)
        .await?;
    row.map(|row| {
        Ok(IssuedAccount {
            account: account_from_row(&row)?,
            livemode: row.try_get("livemode")?,
            public_key: row.try_get("public_key")?,
            webhook_url: row.try_get("webhook_url")?,
        })
    })
    .transpose()
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
    /// Fallback route of an unrouted deposit.
    pub route: &'a RouteFile,
    /// Customer-controlled destination.
    pub destination: EvmAddress,
    /// Requested amount; `None` refunds the remainder.
    pub amount: Option<AtomicAmount>,
    /// `Idempotency-Key` of the request.
    pub idempotency_key: Option<&'a str>,
    /// Audit actor.
    pub actor: &'a Actor,
}

/// Creates a refund request after every policy check and returns its id; a repeated
/// `Idempotency-Key` returns the refund created with it.
pub async fn request_refund(pool: &PgPool, refund: &NewRefund<'_>) -> Result<Uuid, ApiError> {
    if let Some(key) = refund.idempotency_key
        && let Some(existing) = refund_by_key(pool, refund.scope, key).await?
    {
        return existing.replay(refund);
    }
    let mut transaction = pool.begin().await?;
    let row = sqlx::query(
        r#"
        SELECT deposit.amount_atomic::text AS amount_atomic, deposit.state, deposit.reason,
               deposit.final_at IS NOT NULL AS is_final,
               COALESCE(deposit.route, $4) AS effective_route,
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
    let effective_route: String = row.try_get("effective_route")?;

    let to_address = format!("{:#x}", refund.destination);
    if refund.idempotency_key.is_none()
        && let Some(amount) = refund.amount
        && let Some(existing) = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT id FROM refunds
            WHERE deposit_id = $1 AND to_address = $2 AND amount_atomic = $3::text::numeric
            "#,
        )
        .bind(refund.deposit_id)
        .bind(&to_address)
        .bind(amount.value().to_string())
        .fetch_optional(&mut *transaction)
        .await?
    {
        transaction.commit().await?;
        return Ok(existing);
    }

    let prior_total = sqlx::query_scalar::<_, String>(
        "SELECT COALESCE(sum(amount_atomic), 0)::text FROM refunds WHERE deposit_id = $1",
    )
    .bind(refund.deposit_id)
    .fetch_one(&mut *transaction)
    .await?;
    let prior_total = parse_atomic(prior_total)?;
    let remaining = deposit_amount
        .checked_sub(prior_total)
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
    let inserted = sqlx::query(
        r#"
        INSERT INTO refunds
            (id, account_id, livemode, deposit_id, amount_atomic, to_address, route, status,
             requested_by, idempotency_key)
        VALUES ($1, $2, $3, $4, $5::text::numeric, $6, $7, 'requested', $8, $9)
        "#,
    )
    .bind(refund_id)
    .bind(refund.scope.account_id())
    .bind(refund.scope.livemode())
    .bind(refund.deposit_id)
    .bind(amount.to_string())
    .bind(to_address)
    .bind(effective_route)
    .bind(refund.actor.to_string())
    .bind(refund.idempotency_key)
    .execute(&mut *transaction)
    .await;
    match inserted {
        Ok(_) => {}
        // A concurrent request with the same key committed first: answer as its repeat.
        Err(sqlx::Error::Database(error))
            if error.constraint() == Some("refunds_idempotency_key_unique") =>
        {
            drop(transaction);
            let key = refund.idempotency_key.ok_or_else(ApiError::internal)?;
            return refund_by_key(pool, refund.scope, key)
                .await?
                .ok_or_else(ApiError::internal)?
                .replay(refund);
        }
        Err(error) => return Err(error.into()),
    }
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

/// The parameters a refund was created with, to answer a repeated `Idempotency-Key`.
struct StoredRefund {
    id: Uuid,
    deposit_id: Uuid,
    to_address: String,
    amount: U256,
}

impl StoredRefund {
    fn replay(self, request: &NewRefund<'_>) -> Result<Uuid, ApiError> {
        let same = self.deposit_id == request.deposit_id
            && self.to_address == format!("{:#x}", request.destination)
            && request
                .amount
                .is_none_or(|amount| amount.value() == self.amount);
        if same {
            Ok(self.id)
        } else {
            Err(ApiError::idempotency_key_reused())
        }
    }
}

async fn refund_by_key(
    pool: &PgPool,
    scope: Scope,
    key: &str,
) -> Result<Option<StoredRefund>, ApiError> {
    let row = sqlx::query_as::<_, (Uuid, Uuid, String, String)>(
        r#"
        SELECT id, deposit_id, to_address, amount_atomic::text
        FROM refunds
        WHERE account_id = $1 AND livemode = $2 AND idempotency_key = $3
        "#,
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(key)
    .fetch_optional(pool)
    .await?;
    row.map(|(id, deposit_id, to_address, amount)| {
        Ok(StoredRefund {
            id,
            deposit_id,
            to_address,
            amount: parse_atomic(amount)?,
        })
    })
    .transpose()
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

/// Approves a requested refund idempotently and appends an audit row.
pub async fn approve_refund(
    pool: &PgPool,
    refund_id: Uuid,
    routes: &[RouteFile],
    actor: &Actor,
) -> Result<AdminRefundResponse, ApiError> {
    let mut transaction = pool.begin().await?;
    let current = refund_admin_row(&mut transaction, refund_id).await?;
    if current.status == "requested" {
        if refund_approval_paused(&mut transaction, refund_id).await? {
            return Err(ApiError::paused("refund approvals are paused"));
        }
        let route = routes
            .iter()
            .filter(|route| route.route == current.route)
            .max_by_key(|route| route.version)
            .ok_or_else(ApiError::internal)?;
        let eligibility = refund_approval_eligibility(&mut transaction, refund_id, route).await?;
        refund_eligibility(eligibility)
            .map_err(|_| ApiError::conflict("deposit is no longer eligible for a refund"))?;
        sqlx::query(
            "UPDATE refunds SET status = 'approved', approved_by = $2, updated_at = now() WHERE id = $1",
        )
        .bind(refund_id)
        .bind(actor.to_string())
        .execute(&mut *transaction)
        .await?;
        insert_audit_tx(
            &mut transaction,
            Some(current.account_id),
            actor,
            "refund_approved",
            &format!("refund:{refund_id}"),
        )
        .await?;
    }
    let updated = refund_admin_row(&mut transaction, refund_id).await?;
    transaction.commit().await?;
    Ok(updated.into())
}

/// Records the treasury transaction and moves an approved refund to `sent`.
pub async fn record_refund(
    pool: &PgPool,
    refund_id: Uuid,
    tx_hash: B256,
    actor: &Actor,
) -> Result<AdminRefundResponse, ApiError> {
    let mut transaction = pool.begin().await?;
    let current = refund_admin_row(&mut transaction, refund_id).await?;
    let tx_hash = format!("{tx_hash:#x}");
    match current.status.as_str() {
        "approved" => {
            sqlx::query(
                r#"
                UPDATE refunds
                SET status = 'sent', tx_hash = $2, next_check_at = now(),
                    tx_version = tx_version + 1,
                    confirmation_evidence = jsonb_build_object(
                        'result', 'recorded', 'tx_hash', $2,
                        'version', tx_version + 1
                    ),
                    updated_at = now()
                WHERE id = $1
                "#,
            )
            .bind(refund_id)
            .bind(&tx_hash)
            .execute(&mut *transaction)
            .await?;
            insert_audit_tx(
                &mut transaction,
                Some(current.account_id),
                actor,
                "refund_recorded",
                &format!("refund:{refund_id}"),
            )
            .await?;
        }
        "sent" if current.tx_hash.as_deref() == Some(tx_hash.as_str()) => {}
        "sent" => {
            let previous = current.tx_hash.as_deref().ok_or_else(ApiError::internal)?;
            sqlx::query(
                r#"
                UPDATE refunds
                SET tx_hash = $2, tx_version = tx_version + 1, next_check_at = now(),
                    confirmation_evidence = jsonb_build_object(
                        'result', 'tx_hash_corrected',
                        'previous_tx_hash', tx_hash,
                        'replacement_tx_hash', $2,
                        'previous_evidence', confirmation_evidence,
                        'version', tx_version + 1
                    ),
                    updated_at = now()
                WHERE id = $1
                "#,
            )
            .bind(refund_id)
            .bind(&tx_hash)
            .execute(&mut *transaction)
            .await?;
            insert_audit_tx_with_reason(
                &mut transaction,
                Some(current.account_id),
                actor,
                "refund_tx_hash_corrected",
                &format!("refund:{refund_id}"),
                &format!("replaced {previous} with {tx_hash}"),
            )
            .await?;
        }
        "confirmed" if current.tx_hash.as_deref() == Some(tx_hash.as_str()) => {}
        "confirmed" => {
            return Err(ApiError::conflict(
                "confirmed refund transaction hash cannot be changed",
            ));
        }
        _ => {
            return Err(ApiError::conflict(
                "refund must be approved before recording",
            ));
        }
    }
    let updated = refund_admin_row(&mut transaction, refund_id).await?;
    transaction.commit().await?;
    Ok(updated.into())
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

fn account_from_row(row: &PgRow) -> Result<Account, ApiError> {
    Ok(Account {
        id: row.try_get("id")?,
        public_id: row.try_get("public_id")?,
        name: row.try_get("name")?,
        paused_scopes: row.try_get("paused_scopes")?,
    })
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
    quote_id: Uuid,
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

#[derive(FromRow)]
struct RefundAdminRow {
    id: Uuid,
    account_id: Uuid,
    route: String,
    status: String,
    tx_hash: Option<String>,
    confirmation_evidence: Option<Value>,
}

impl From<RefundAdminRow> for AdminRefundResponse {
    fn from(row: RefundAdminRow) -> Self {
        Self {
            id: crate::ids::format(crate::ids::REFUND, row.id),
            status: row.status,
            tx_hash: row.tx_hash,
            confirmation_evidence: row.confirmation_evidence,
        }
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
            lock_ref: Some(crate::locks::quote_id(row.quote_id)),
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
               address.quote_id,
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
        WITH confirmed AS (
            SELECT deposit_id, sum(amount_atomic) AS amount
            FROM refunds
            WHERE status = 'confirmed'
            GROUP BY deposit_id
        )
        SELECT COALESCE(
                   deposit.route,
                   'unrouted:' || deposit.chain_id::text || ':' || deposit.asset_contract
               ) AS report_key,
               COALESCE(sum(GREATEST(deposit.amount_atomic - COALESCE(confirmed.amount, 0), 0)), 0)::text AS amount
        FROM deposits AS deposit
        LEFT JOIN confirmed ON confirmed.deposit_id = deposit.id
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
        treasury_balance_atomic: None,
        treasury_balance_note: "treasury balance has not been observed".to_owned(),
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
        refunds_by_status: zero_counts(&["requested", "approved", "sent", "confirmed"]),
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
        treasury_balance_atomic: None,
        treasury_balance_note: "unrouted assets do not have an RPC route configuration".to_owned(),
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
        refunds_by_status: zero_counts(&["requested", "approved", "sent", "confirmed"]),
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

async fn refund_admin_row(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    refund_id: Uuid,
) -> Result<RefundAdminRow, ApiError> {
    sqlx::query_as::<_, RefundAdminRow>(
        r#"
        SELECT id, account_id, route, status, tx_hash, confirmation_evidence
        FROM refunds
        WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(refund_id)
    .fetch_optional(&mut **transaction)
    .await?
    .ok_or_else(ApiError::not_found)
}

async fn refund_approval_eligibility(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    refund_id: Uuid,
    route: &RouteFile,
) -> Result<RefundDeposit, ApiError> {
    let row = sqlx::query(
        r#"
        SELECT deposit.amount_atomic::text AS amount_atomic, deposit.state, deposit.reason
        FROM refunds AS refund
        JOIN deposits AS deposit ON deposit.id = refund.deposit_id
        JOIN customers AS customer ON customer.id = deposit.customer_id
        WHERE refund.id = $1
        FOR UPDATE OF deposit, customer
        "#,
    )
    .bind(refund_id)
    .fetch_one(&mut **transaction)
    .await?;
    refund_deposit_from_row(&row, route)
}

async fn refund_approval_paused(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    refund_id: Uuid,
) -> Result<bool, ApiError> {
    let row = sqlx::query(
        r#"
        SELECT customer.paused_scopes AS customer_scopes,
               account.paused_scopes AS account_scopes,
               COALESCE(route_pause.paused_scopes, '{}') AS route_scopes
        FROM refunds AS refund
        JOIN deposits AS deposit ON deposit.id = refund.deposit_id
        JOIN customers AS customer ON customer.id = deposit.customer_id
        JOIN accounts AS account ON account.id = deposit.account_id
        LEFT JOIN route_pauses AS route_pause ON route_pause.route = refund.route
        WHERE refund.id = $1
        "#,
    )
    .bind(refund_id)
    .fetch_one(&mut **transaction)
    .await?;
    let customer_scopes: Vec<String> = row.try_get("customer_scopes")?;
    let account_scopes: Vec<String> = row.try_get("account_scopes")?;
    let route_scopes: Vec<String> = row.try_get("route_scopes")?;
    Ok([customer_scopes, account_scopes, route_scopes]
        .iter()
        .any(|scopes| scopes.iter().any(|scope| scope == "refunds")))
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

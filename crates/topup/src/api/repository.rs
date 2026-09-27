//! API-specific PostgreSQL queries with tenant predicates at the database boundary.

use std::collections::{BTreeMap, BTreeSet};
use std::str::FromStr;

use alloy_primitives::{Address as EvmAddress, B256, U256};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::postgres::PgRow;
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder, Row};
use topup_core::address::{forwarder_address, persistent_salt};
use topup_core::money::AtomicAmount;
use topup_core::refund::{RefundDeposit, refund_eligibility};
use topup_core::route::RouteFile;
use uuid::Uuid;

use crate::db::{Account, Address, AddressKind, Product};

use super::auth::VerifiedSignature;
use super::error::ApiError;
use super::models::{
    AdminRefundResponse, DailyReportResponse, DepositEventResponse, DepositResponse,
    DepositTransitionResponse, NudgeResponse, OutboxReplayResponse,
    ReconciliationBlockLiftResponse, ReconciliationBlockReport, RouteDailyReport,
    SupportDepositResponse,
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

/// Finds the unique product selected by an external API slug.
pub async fn find_product_by_slug(pool: &PgPool, slug: &str) -> Result<Option<Product>, ApiError> {
    let row = sqlx::query_as::<_, ProductRow>(
        r#"
        SELECT id, slug, webhook_url, pubkey, paused_scopes
        FROM products
        WHERE slug = $1
        "#,
    )
    .bind(slug)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

/// Registers a product with an audit row, or returns the existing product when every value
/// matches. A slug registered with a different key or webhook URL is a conflict.
pub async fn register_product(
    pool: &PgPool,
    slug: &str,
    pubkey: &str,
    webhook_url: &str,
    actor: &str,
) -> Result<Product, ApiError> {
    let mut transaction = pool.begin().await?;
    let inserted = sqlx::query_as::<_, ProductRow>(
        r#"
        INSERT INTO products (id, slug, webhook_url, pubkey)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (slug) DO NOTHING
        RETURNING id, slug, webhook_url, pubkey, paused_scopes
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(slug)
    .bind(webhook_url)
    .bind(pubkey)
    .fetch_optional(&mut *transaction)
    .await?;
    let product = match inserted {
        Some(row) => {
            insert_audit_tx(
                &mut transaction,
                actor,
                "product.issue",
                &format!("product:{slug}"),
            )
            .await?;
            row
        }
        None => {
            let existing = sqlx::query_as::<_, ProductRow>(
                "SELECT id, slug, webhook_url, pubkey, paused_scopes FROM products WHERE slug = $1",
            )
            .bind(slug)
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or_else(ApiError::internal)?;
            if existing.pubkey != pubkey || existing.webhook_url != webhook_url {
                return Err(ApiError::conflict(
                    "product slug is already registered with a different public key or webhook \
                     URL; replace them with PUT /v1/admin/products/{slug}",
                ));
            }
            existing
        }
    };
    transaction.commit().await?;
    Ok(product.into())
}

/// Replaces an issued product's verification key and webhook URL and appends an audit row,
/// carrying the reason and the replaced values, in the same transaction.
///
/// Product requests read the key on every request, so the replaced key stops verifying when this
/// commits: a hard cut, with no overlap (the attested route names one key id per product).
/// Repeating the request with the stored values changes nothing and writes no audit row.
pub async fn update_product(
    pool: &PgPool,
    slug: &str,
    pubkey: &str,
    webhook_url: &str,
    actor: &str,
    reason: &str,
) -> Result<Product, ApiError> {
    let mut transaction = pool.begin().await?;
    let existing = sqlx::query_as::<_, ProductRow>(
        "SELECT id, slug, webhook_url, pubkey, paused_scopes FROM products WHERE slug = $1 FOR UPDATE",
    )
    .bind(slug)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(ApiError::not_found)?;
    if existing.pubkey == pubkey && existing.webhook_url == webhook_url {
        transaction.commit().await?;
        return Ok(existing.into());
    }
    let updated = sqlx::query_as::<_, ProductRow>(
        r#"
        UPDATE products SET pubkey = $2, webhook_url = $3
        WHERE id = $1
        RETURNING id, slug, webhook_url, pubkey, paused_scopes
        "#,
    )
    .bind(existing.id)
    .bind(pubkey)
    .bind(webhook_url)
    .fetch_one(&mut *transaction)
    .await?;
    let evidence = serde_json::json!({
        "reason": reason,
        "replaced": {"public_key": existing.pubkey, "webhook_url": existing.webhook_url},
    });
    insert_audit_tx_with_reason(
        &mut transaction,
        actor,
        "product.update",
        &format!("product:{slug}"),
        &evidence.to_string(),
    )
    .await?;
    transaction.commit().await?;
    Ok(updated.into())
}

/// Registers an account or returns the existing account with the same product identifier.
pub async fn register_account(
    pool: &PgPool,
    product_id: Uuid,
    external_id: &str,
) -> Result<Account, ApiError> {
    let mut transaction = pool.begin().await?;
    sqlx::query(
        r#"
        INSERT INTO accounts (id, product_id, external_id, paused_scopes)
        VALUES ($1, $2, $3, '{}')
        ON CONFLICT (product_id, external_id) DO NOTHING
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(product_id)
    .bind(external_id)
    .execute(&mut *transaction)
    .await?;
    let account = sqlx::query_as::<_, AccountRow>(
        r#"
        SELECT id, product_id, external_id, status, paused_scopes
        FROM accounts
        WHERE product_id = $1 AND external_id = $2
        "#,
    )
    .bind(product_id)
    .bind(external_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(ApiError::internal)?;
    transaction.commit().await?;
    Ok(account.into())
}

/// Finds an account only when it belongs to the requested product.
pub async fn find_account(
    pool: &PgPool,
    product_id: Uuid,
    external_id: &str,
) -> Result<Option<Account>, ApiError> {
    let row = sqlx::query_as::<_, AccountRow>(
        r#"
        SELECT id, product_id, external_id, status, paused_scopes
        FROM accounts
        WHERE product_id = $1 AND external_id = $2
        "#,
    )
    .bind(product_id)
    .bind(external_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(Into::into))
}

/// Returns the active persistent address, creating version one atomically when absent.
#[allow(clippy::too_many_arguments)]
pub async fn get_or_create_persistent_address(
    pool: &PgPool,
    product_id: Uuid,
    account: &Account,
    product_slug: &str,
    chain_id: u64,
    factory: EvmAddress,
    implementation: EvmAddress,
) -> Result<Address, ApiError> {
    let mut transaction = pool.begin().await?;
    lock_account(&mut transaction, product_id, account.id).await?;
    if let Some(address) = find_active_address(&mut transaction, account.id, chain_id).await? {
        transaction.commit().await?;
        return Ok(address);
    }
    let address = insert_persistent_address(
        &mut transaction,
        account,
        product_slug,
        chain_id,
        factory,
        implementation,
        1,
    )
    .await?;
    transaction.commit().await?;
    Ok(address)
}

/// Retires the active address and creates the next persistent version atomically.
#[allow(clippy::too_many_arguments)]
pub async fn rotate_persistent_address(
    pool: &PgPool,
    product_id: Uuid,
    account: &Account,
    product_slug: &str,
    chain_id: u64,
    factory: EvmAddress,
    implementation: EvmAddress,
    from_version: u64,
) -> Result<Address, ApiError> {
    let mut transaction = pool.begin().await?;
    lock_account(&mut transaction, product_id, account.id).await?;
    let current = find_active_address(&mut transaction, account.id, chain_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if current.version > from_version {
        transaction.commit().await?;
        return Ok(current);
    }
    if current.version < from_version {
        return Err(ApiError::conflict(
            "from_version is newer than the current address version",
        ));
    }
    let version = current
        .version
        .checked_add(1)
        .ok_or_else(|| ApiError::conflict("address version is exhausted"))?;
    sqlx::query("UPDATE addresses SET retired_at = now() WHERE id = $1 AND retired_at IS NULL")
        .bind(current.id)
        .execute(&mut *transaction)
        .await?;
    let address = insert_persistent_address(
        &mut transaction,
        account,
        product_slug,
        chain_id,
        factory,
        implementation,
        version,
    )
    .await?;
    transaction.commit().await?;
    Ok(address)
}

/// A refund request: the deposit, destination, and amount (the unrefunded remainder when absent).
pub struct NewRefund<'a> {
    /// Authenticated product.
    pub product_id: Uuid,
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
    pub actor: &'a str,
}

/// Creates a refund request after every policy check and returns its id; a repeated
/// `Idempotency-Key` returns the refund created with it.
pub async fn request_refund(pool: &PgPool, refund: &NewRefund<'_>) -> Result<Uuid, ApiError> {
    if let Some(key) = refund.idempotency_key
        && let Some(existing) = refund_by_key(pool, refund.product_id, key).await?
    {
        return existing.replay(refund);
    }
    let mut transaction = pool.begin().await?;
    let row = sqlx::query(
        r#"
        SELECT deposit.amount_atomic::text AS amount_atomic, deposit.state, deposit.reason,
               COALESCE(deposit.route, $3) AS effective_route,
               account.paused_scopes AS account_scopes,
               product.paused_scopes AS product_scopes,
               COALESCE(route_pause.paused_scopes, '{}') AS route_scopes
        FROM deposits AS deposit
        JOIN accounts AS account ON account.id = deposit.account_id
        JOIN products AS product ON product.id = account.product_id
        LEFT JOIN route_pauses AS route_pause ON route_pause.route = COALESCE(deposit.route, $3)
        WHERE deposit.id = $1 AND product.id = $2
        FOR UPDATE OF deposit, account
        "#,
    )
    .bind(refund.deposit_id)
    .bind(refund.product_id)
    .bind(&refund.route.route)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(|| ApiError::not_found().with_param("deposit"))?;

    let account_scopes: Vec<String> = row.try_get("account_scopes")?;
    let product_scopes: Vec<String> = row.try_get("product_scopes")?;
    let route_scopes: Vec<String> = row.try_get("route_scopes")?;
    if [&account_scopes, &product_scopes, &route_scopes]
        .into_iter()
        .any(|scopes| scopes.iter().any(|scope| scope == "refunds"))
    {
        return Err(ApiError::paused("refund requests are paused"));
    }

    let deposit_amount = parse_atomic(row.try_get::<String, _>("amount_atomic")?)?;
    refund_eligibility(refund_deposit_from_row(&row, refund.route)?)
        .map_err(|_| ApiError::deposit_not_refundable())?;
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
            (id, deposit_id, amount_atomic, to_address, route, status, requested_by, product_id,
             idempotency_key)
        VALUES ($1, $2, $3::text::numeric, $4, $5, 'requested', $6, $7, $8)
        "#,
    )
    .bind(refund_id)
    .bind(refund.deposit_id)
    .bind(amount.to_string())
    .bind(to_address)
    .bind(effective_route)
    .bind(refund.actor)
    .bind(refund.product_id)
    .bind(refund.idempotency_key)
    .execute(&mut *transaction)
    .await;
    match inserted {
        Ok(_) => {}
        // A concurrent request with the same key committed first: answer as its repeat.
        Err(sqlx::Error::Database(error))
            if error.constraint() == Some("refunds_product_idempotency_key_unique") =>
        {
            drop(transaction);
            let key = refund.idempotency_key.ok_or_else(ApiError::internal)?;
            return refund_by_key(pool, refund.product_id, key)
                .await?
                .ok_or_else(ApiError::internal)?
                .replay(refund);
        }
        Err(error) => return Err(error.into()),
    }
    insert_audit_tx(
        &mut transaction,
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
    product_id: Uuid,
    key: &str,
) -> Result<Option<StoredRefund>, ApiError> {
    let row = sqlx::query_as::<_, (Uuid, Uuid, String, String)>(
        r#"
        SELECT id, deposit_id, to_address, amount_atomic::text
        FROM refunds
        WHERE product_id = $1 AND idempotency_key = $2
        "#,
    )
    .bind(product_id)
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
    actor: &str,
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
        .bind(actor)
        .execute(&mut *transaction)
        .await?;
        insert_audit_tx(
            &mut transaction,
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
    actor: &str,
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
    actor: &str,
) -> Result<NudgeResponse, ApiError> {
    let mut transaction = pool.begin().await?;
    let next_attempt_at = sqlx::query_scalar::<_, DateTime<Utc>>(
        "UPDATE deposits SET next_attempt_at = now() WHERE id = $1 RETURNING next_attempt_at",
    )
    .bind(deposit_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(ApiError::not_found)?;
    insert_audit_tx(
        &mut transaction,
        actor,
        "deposit_nudged",
        &format!("deposit:{deposit_id}"),
    )
    .await?;
    transaction.commit().await?;
    Ok(NudgeResponse {
        deposit_id,
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
    actor: &str,
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

/// Queues one webhook event for delivery again and appends an audit row in the same transaction.
///
/// A delivered event is marked undelivered and a pending one becomes due now; the event's
/// identifier and payload never change. Repeating the request while the event is already due
/// changes nothing and writes no audit row.
pub async fn replay_outbox_event(
    pool: &PgPool,
    event_id: Uuid,
    actor: &str,
    reason: &str,
) -> Result<OutboxReplayResponse, ApiError> {
    let mut transaction = pool.begin().await?;
    let row = sqlx::query(
        r#"
        SELECT event_type, next_attempt_at,
               delivered_at IS NULL AND next_attempt_at <= now() AS due
        FROM outbox
        WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(event_id)
    .fetch_optional(&mut *transaction)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let mut next_attempt_at: DateTime<Utc> = row.try_get("next_attempt_at")?;
    if !row.try_get::<bool, _>("due")? {
        next_attempt_at = sqlx::query_scalar(
            r#"
            UPDATE outbox SET next_attempt_at = now(), delivered_at = NULL
            WHERE id = $1
            RETURNING next_attempt_at
            "#,
        )
        .bind(event_id)
        .fetch_one(&mut *transaction)
        .await?;
        insert_audit_tx_with_reason(
            &mut transaction,
            actor,
            "outbox.replay",
            &format!("event:{event_id}"),
            reason,
        )
        .await?;
    }
    transaction.commit().await?;
    Ok(OutboxReplayResponse {
        event_id,
        event_type: row.try_get("event_type")?,
        next_attempt_at,
    })
}

/// Adds or removes account pause scopes and appends an audit row in the same transaction.
pub async fn mutate_account_scopes(
    pool: &PgPool,
    product_id: Uuid,
    account_id: Uuid,
    requested: &[String],
    pause: bool,
    actor: &str,
) -> Result<Vec<String>, ApiError> {
    let mut transaction = pool.begin().await?;
    let current: Option<Vec<String>> = sqlx::query_scalar(
        "SELECT paused_scopes FROM accounts WHERE id = $1 AND product_id = $2 FOR UPDATE",
    )
    .bind(account_id)
    .bind(product_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let current = current.ok_or_else(ApiError::not_found)?;
    let updated = updated_scopes(current, requested, pause);
    sqlx::query("UPDATE accounts SET paused_scopes = $2 WHERE id = $1")
        .bind(account_id)
        .bind(&updated)
        .execute(&mut *transaction)
        .await?;
    insert_audit_tx(
        &mut transaction,
        actor,
        if pause { "pause" } else { "resume" },
        &format!("account:{account_id}"),
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
    actor: &str,
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
struct ProductRow {
    id: Uuid,
    slug: String,
    webhook_url: String,
    pubkey: String,
    paused_scopes: Vec<String>,
}

impl From<ProductRow> for Product {
    fn from(row: ProductRow) -> Self {
        Self {
            id: row.id,
            slug: row.slug,
            webhook_url: row.webhook_url,
            pubkey: row.pubkey,
            paused_scopes: row.paused_scopes,
        }
    }
}

#[derive(FromRow)]
struct AccountRow {
    id: Uuid,
    product_id: Uuid,
    external_id: String,
    status: String,
    paused_scopes: Vec<String>,
}

impl From<AccountRow> for Account {
    fn from(row: AccountRow) -> Self {
        Self {
            id: row.id,
            product_id: row.product_id,
            external_id: row.external_id,
            status: row.status,
            paused_scopes: row.paused_scopes,
        }
    }
}

#[derive(FromRow)]
struct AddressRow {
    id: Uuid,
    account_id: Uuid,
    chain_id: i64,
    kind: String,
    version: i64,
    lock_ref: Option<String>,
    salt: String,
    address: String,
    retired_at: Option<DateTime<Utc>>,
}

impl TryFrom<AddressRow> for Address {
    type Error = ApiError;

    fn try_from(row: AddressRow) -> Result<Self, Self::Error> {
        let kind = match row.kind.as_str() {
            "persistent" => AddressKind::Persistent,
            "lock" => AddressKind::Lock,
            _ => return Err(ApiError::internal()),
        };
        Ok(Self {
            id: row.id,
            account_id: row.account_id,
            chain_id: u64::try_from(row.chain_id).map_err(|_| ApiError::internal())?,
            kind,
            version: u64::try_from(row.version).map_err(|_| ApiError::internal())?,
            lock_ref: row.lock_ref,
            salt: B256::from_str(&row.salt).map_err(|_| ApiError::internal())?,
            address: EvmAddress::from_str(&row.address).map_err(|_| ApiError::internal())?,
            retired_at: row.retired_at,
        })
    }
}

#[derive(FromRow)]
struct DepositViewRow {
    id: Uuid,
    external_id: String,
    chain_id: i64,
    tx_hash: String,
    log_index: i64,
    block_number: i64,
    block_time: DateTime<Utc>,
    address: String,
    lock_ref: Option<String>,
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
            id: row.id,
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
    route: String,
    status: String,
    tx_hash: Option<String>,
    confirmation_evidence: Option<Value>,
}

impl From<RefundAdminRow> for AdminRefundResponse {
    fn from(row: RefundAdminRow) -> Self {
        Self {
            id: row.id,
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
            id: row.id,
            external_id: row.external_id,
            chain_id: u64::try_from(row.chain_id).map_err(|_| ApiError::internal())?,
            tx_hash: row.tx_hash,
            log_index: u64::try_from(row.log_index).map_err(|_| ApiError::internal())?,
            block_number: u64::try_from(row.block_number).map_err(|_| ApiError::internal())?,
            block_time: row.block_time,
            address: row.address,
            lock_ref: row.lock_ref,
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
        SELECT deposit.id, account.external_id, deposit.chain_id, deposit.tx_hash,
               deposit.log_index,
               deposit.block_number, deposit.block_time, address.address, address.lock_ref,
               deposit.route, deposit.route_version, deposit.asset_contract,
               deposit.from_address, deposit.amount_atomic::text AS amount_atomic,
               deposit.state, deposit.valuation_at, deposit.price_scaled::text AS price_scaled,
               deposit.price_source, deposit.credit_minor::text AS credit_minor,
               deposit.created_at, deposit.updated_at
        FROM deposits AS deposit
        JOIN accounts AS account ON account.id = deposit.account_id
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
        .map(TryInto::try_into)
        .collect::<Result<Vec<DepositResponse>, ApiError>>()?;
    let ids = deposits
        .iter()
        .map(|deposit| deposit.id)
        .collect::<Vec<_>>();
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
        SELECT id, (payload ->> 'deposit_id')::uuid AS deposit_id, event_type, created_at,
               delivered_at
        FROM outbox
        WHERE payload ->> 'deposit_id' = ANY($1)
        ORDER BY created_at, id
        "#,
    )
    .bind(ids.iter().map(Uuid::to_string).collect::<Vec<_>>())
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
        .map(|deposit| SupportDepositResponse {
            timeline: by_deposit.remove(&deposit.id).unwrap_or_default(),
            events: events_by_deposit.remove(&deposit.id).unwrap_or_default(),
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
        SELECT COALESCE(route, 'unrouted:' || chain_id::text || ':' || asset_contract) AS report_key,
               COALESCE(sum(amount_atomic), 0)::text AS amount
        FROM deposits
        WHERE flush_id IS NULL
        GROUP BY report_key
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
        FROM rate_locks
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
               count(*)::bigint AS count,
               COALESCE(
                   max(extract(epoch FROM ($1 - outbox.created_at)))::bigint, 0
               ) AS max_age_seconds
        FROM outbox
        JOIN deposits AS deposit ON deposit.id::text = outbox.payload ->> 'deposit_id'
        WHERE outbox.event_type = 'deposit.credited' AND outbox.delivered_at IS NULL
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
        FROM rate_locks
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
        deposits_by_state: zero_counts(&["detected", "confirmed", "credited", "swept", "rejected"]),
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
        flush_planning: None,
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
        deposits_by_state: zero_counts(&["detected", "confirmed", "credited", "swept", "rejected"]),
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
        flush_planning: None,
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
        SELECT id, route, status, tx_hash, confirmation_evidence
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
        JOIN accounts AS account ON account.id = deposit.account_id
        WHERE refund.id = $1
        FOR UPDATE OF deposit, account
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
        SELECT account.paused_scopes AS account_scopes,
               product.paused_scopes AS product_scopes,
               COALESCE(route_pause.paused_scopes, '{}') AS route_scopes
        FROM refunds AS refund
        JOIN deposits AS deposit ON deposit.id = refund.deposit_id
        JOIN accounts AS account ON account.id = deposit.account_id
        JOIN products AS product ON product.id = account.product_id
        LEFT JOIN route_pauses AS route_pause ON route_pause.route = refund.route
        WHERE refund.id = $1
        "#,
    )
    .bind(refund_id)
    .fetch_one(&mut **transaction)
    .await?;
    let account_scopes: Vec<String> = row.try_get("account_scopes")?;
    let product_scopes: Vec<String> = row.try_get("product_scopes")?;
    let route_scopes: Vec<String> = row.try_get("route_scopes")?;
    Ok([account_scopes, product_scopes, route_scopes]
        .iter()
        .any(|scopes| scopes.iter().any(|scope| scope == "refunds")))
}

async fn lock_account(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    product_id: Uuid,
    account_id: Uuid,
) -> Result<(), ApiError> {
    let found = sqlx::query("SELECT id FROM accounts WHERE id = $1 AND product_id = $2 FOR UPDATE")
        .bind(account_id)
        .bind(product_id)
        .fetch_optional(&mut **transaction)
        .await?;
    if found.is_none() {
        return Err(ApiError::not_found());
    }
    Ok(())
}

async fn find_active_address(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    account_id: Uuid,
    chain_id: u64,
) -> Result<Option<Address>, ApiError> {
    let chain_id = i64::try_from(chain_id).map_err(|_| ApiError::internal())?;
    let row = sqlx::query_as::<_, AddressRow>(
        r#"
        SELECT id, account_id, chain_id, kind, version, lock_ref, salt, address, retired_at
        FROM addresses
        WHERE account_id = $1 AND chain_id = $2 AND kind = 'persistent' AND retired_at IS NULL
        FOR UPDATE
        "#,
    )
    .bind(account_id)
    .bind(chain_id)
    .fetch_optional(&mut **transaction)
    .await?;
    row.map(TryInto::try_into).transpose()
}

#[allow(clippy::too_many_arguments)]
async fn insert_persistent_address(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    account: &Account,
    product_slug: &str,
    chain_id: u64,
    factory: EvmAddress,
    implementation: EvmAddress,
    version: u64,
) -> Result<Address, ApiError> {
    let salt = persistent_salt(product_slug, &account.external_id, version);
    let address = forwarder_address(factory, implementation, salt);
    // Each version's address is first issued here, so, as for lock addresses, the scanner
    // covers it from the chain's committed cursor instead of backfilling from genesis.
    let row = sqlx::query_as::<_, AddressRow>(
        r#"
        INSERT INTO addresses
            (id, account_id, chain_id, kind, version, lock_ref, salt, address, retired_at,
             created_block)
        VALUES (
            $1, $2, $3, 'persistent', $4, NULL, $5, $6, NULL,
            COALESCE((SELECT scanned_block FROM cursors WHERE chain_id = $3), 0)
        )
        RETURNING id, account_id, chain_id, kind, version, lock_ref, salt, address, retired_at
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(account.id)
    .bind(i64::try_from(chain_id).map_err(|_| ApiError::internal())?)
    .bind(i64::try_from(version).map_err(|_| ApiError::conflict("address version is exhausted"))?)
    .bind(format!("{salt:#x}"))
    .bind(format!("{address:#x}"))
    .fetch_one(&mut **transaction)
    .await?;
    row.try_into()
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
    actor: &str,
    action: &str,
    subject: &str,
) -> Result<(), ApiError> {
    insert_audit_tx_with_reason(transaction, actor, action, subject, "signed API request").await
}

async fn insert_audit_tx_with_reason(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    actor: &str,
    action: &str,
    subject: &str,
    reason: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO audit (id, actor, action, subject, reason) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::new_v4())
    .bind(actor)
    .bind(action)
    .bind(subject)
    .bind(reason)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

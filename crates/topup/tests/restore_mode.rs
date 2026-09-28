//! Restore mode (docs/architecture.md §14, docs/design/multi-tenant.md §13) on PostgreSQL: a
//! restore freezes merchant writes and the crediting tasks; the operator re-applies lost security
//! changes, re-issues lost deposit addresses identically, imports delivered events so none is sent
//! again with another body, and unfreezes once the chains are rescanned, audited.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use axum::Router;
use axum::body::to_bytes;
use axum::http::{Method, StatusCode};
use chrono::{TimeDelta, Utc};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use sqlx::PgPool;
use topup::api::{AppState, PublicOrigin, VerificationKey};
use topup::db::{self, NewDeposit};
use topup::restore_mode;
use topup_adapters::attestation::DstackAttestor;
use topup_core::address::{deposit_address_salt, forwarder_address};
use topup_core::deposit::DepositState;
use topup_core::identity::{credited_event_id, deposit_id};
use topup_core::money::AtomicAmount;
use topup_core::route::RouteFile;
use tower::ServiceExt;
use uuid::Uuid;

use support::seed::{self, NewAccount};
use support::{TEST_ORIGIN, TestDatabase, merchant_request, public_key_base64, signed_request};

const ADMIN_KID: &str = "admin/v1";

struct Harness {
    app: Router,
    read_only: Router,
    pool: PgPool,
    owner: PgPool,
    route: RouteFile,
    admin_key: SigningKey,
    /// Admin signatures are single-use, so each admin request is signed at a distinct second.
    created: AtomicI64,
    account: db::Account,
    key: String,
}

struct Answer {
    status: StatusCode,
    retry_after: Option<String>,
    body: Value,
}

impl Harness {
    async fn new(database: &TestDatabase) -> Result<Self> {
        let pool = database.app_pool.clone();
        let admin_key = SigningKey::from_bytes(&[91; 32]);
        let route: RouteFile =
            serde_saphyr::from_str(include_str!("fixtures/phala-cloud-pha.yaml"))?;
        let state = AppState {
            pool: pool.clone(),
            routes: Arc::new(
                topup::routes::RouteSet::new(vec![route.clone()]).map_err(anyhow::Error::msg)?,
            ),
            admin_key: VerificationKey::from_base64(
                ADMIN_KID.to_owned(),
                &public_key_base64(&admin_key),
            )
            .map_err(anyhow::Error::msg)?,
            public_origin: PublicOrigin::parse(TEST_ORIGIN)?,
            attestor: Arc::new(DstackAttestor::new()),
            rate_lock_quotes: Arc::new(topup::locks::UnavailableQuoteProvider),
            client_reads: Arc::default(),
            rate_limits: Arc::default(),
            screening: Arc::new(topup::refunds::UnavailableDestinationScreener),
            contract_signatures: Arc::new(topup::treasuries::UnavailableContractSignatures),
        };
        let account = seed::create_account(
            &pool,
            &NewAccount {
                webhook_url: "https://merchant.example/webhooks".to_owned(),
                ..NewAccount::named("merchant")
            },
        )
        .await?;
        let key = seed::create_api_key(&pool, account.id, true).await?;
        seed::set_treasury(&pool, account.id, true, 1, seed::FIXTURE_TREASURY).await?;
        Ok(Self {
            app: topup::api::router(state.clone()).0,
            read_only: topup::api::read_only_router(state, None),
            pool,
            owner: database.owner_pool.clone(),
            route,
            admin_key,
            created: AtomicI64::new(Utc::now().timestamp()),
            account,
            key,
        })
    }

    async fn admin(&self, method: Method, path: &str, body: &Value) -> Result<Answer> {
        self.admin_on(&self.app, method, path, body).await
    }

    async fn admin_on(
        &self,
        app: &Router,
        method: Method,
        path: &str,
        body: &Value,
    ) -> Result<Answer> {
        let created = self.created.fetch_sub(1, Ordering::Relaxed);
        let body = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(body)?
        };
        let request = signed_request(method, path, body, ADMIN_KID, &self.admin_key, created);
        answer(app.clone().oneshot(request).await?).await
    }

    async fn merchant(&self, method: Method, path: &str, body: &Value) -> Result<Answer> {
        self.merchant_with(&self.app, method, path, body, &self.key)
            .await
    }

    async fn merchant_with(
        &self,
        app: &Router,
        method: Method,
        path: &str,
        body: &Value,
        key: &str,
    ) -> Result<Answer> {
        let body = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(body)?
        };
        answer(
            app.clone()
                .oneshot(merchant_request(method, path, body, key))
                .await?,
        )
        .await
    }

    /// Records a restore as `restore-check` does after a restore on boot.
    async fn restore(&self) -> Result<restore_mode::Restore> {
        Ok(restore_mode::freeze_after_restore(&self.owner).await?)
    }

    /// Sets the chain's cursor as the finalized scanner commits it.
    async fn scan_to(&self, block: i64, block_time: chrono::DateTime<Utc>) -> Result<()> {
        sqlx::query(
            "INSERT INTO cursors (chain_id, scanned_block, scanned_block_time) VALUES (1, $1, $2) \
             ON CONFLICT (chain_id) DO UPDATE \
             SET scanned_block = EXCLUDED.scanned_block, \
                 scanned_block_time = EXCLUDED.scanned_block_time",
        )
        .bind(block)
        .bind(block_time)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    fn account_id(&self) -> &str {
        &self.account.public_id
    }

    /// The customer's deposit address of `version`, as the merchant recomputes it.
    fn derived_address(&self, customer: &str, version: u64) -> Address {
        let contracts = &self.route.chain.contracts;
        forwarder_address(
            contracts.forwarder_factory,
            contracts.implementation,
            seed::FIXTURE_TREASURY,
            deposit_address_salt(&self.account.public_id, true, customer, version),
        )
    }
}

async fn answer(response: axum::response::Response) -> Result<Answer> {
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .map(|value| value.to_str().map(str::to_owned))
        .transpose()?;
    let bytes = to_bytes(response.into_body(), 1_048_576).await?;
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    Ok(Answer {
        status,
        retry_after,
        body,
    })
}

fn checklist(reason: &str) -> Value {
    json!({
        "reason": reason,
        "security_changes_reapplied": true,
        "deposit_addresses_reissued": true,
        "delivered_events_imported": true,
    })
}

#[tokio::test]
async fn a_new_timeline_freezes_the_service_once() -> Result<()> {
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            // The migration acknowledged the timeline it ran on: no restore.
            ensure!(restore_mode::detect(&harness.pool).await?.is_none());
            ensure!(!restore_mode::is_frozen(&harness.pool).await?);
            harness.scan_to(40, Utc::now()).await?;
            sqlx::query("INSERT INTO heartbeat DEFAULT VALUES")
                .execute(&harness.pool)
                .await?;

            // A promotion out of archive recovery starts a newer timeline than the acknowledged
            // one; emulate it by acknowledging an older one.
            sqlx::query(
                "ALTER TABLE restore_timeline DROP CONSTRAINT restore_timeline_timeline_id_check",
            )
            .execute(&harness.owner)
            .await?;
            sqlx::query("UPDATE restore_timeline SET timeline_id = 0")
                .execute(&harness.owner)
                .await?;
            let restore = restore_mode::detect(&harness.pool)
                .await?
                .context("a newer timeline freezes the service")?;
            ensure!(restore.detected_by == "timeline" && restore.timeline_id >= 1);
            ensure!(restore.restored_cursors.get(&1) == Some(&40));
            ensure!(restore.restore_point.is_some());
            ensure!(restore_mode::is_frozen(&harness.pool).await?);
            // The timeline is acknowledged: a restart finds the same freeze, not another.
            ensure!(restore_mode::detect(&harness.pool).await? == Some(restore.clone()));
            // restore-check's marker keeps the one freeze too.
            ensure!(harness.restore().await?.id == restore.id);
            let restores: i64 = sqlx::query_scalar("SELECT count(*) FROM restores")
                .fetch_one(&harness.pool)
                .await?;
            ensure!(restores == 1);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn frozen_merchant_writes_answer_service_restoring_and_reads_work() -> Result<()> {
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            let body = json!({"name": "ci"});
            ensure!(
                harness
                    .merchant(Method::POST, "/v1/api_keys", &body)
                    .await?
                    .status
                    == StatusCode::OK
            );
            harness.restore().await?;

            for (method, path, body) in [
                (Method::POST, "/v1/api_keys", json!({"name": "ci"})),
                (
                    Method::POST,
                    "/v1/deposit_addresses",
                    json!({"client_reference_id": "team-1"}),
                ),
                (Method::POST, "/v1/account/pause", Value::Null),
                (
                    Method::POST,
                    "/v1/api_keys",
                    json!({"name": "worker", "type": "restricted", "permissions": ["quotes.read"]}),
                ),
                (Method::POST, "/v1/treasuries/trs_0/pause", Value::Null),
                (Method::POST, "/v1/treasuries/trs_0/resume", Value::Null),
                (
                    Method::POST,
                    "/v1/account/webhook_keys/roll",
                    json!({"expires_in": 0}),
                ),
                (Method::DELETE, "/v1/webhook_endpoints/we_0", Value::Null),
            ] {
                let refused = harness.merchant(method, path, &body).await?;
                ensure!(
                    refused.status == StatusCode::SERVICE_UNAVAILABLE,
                    "{path}: {}",
                    refused.status
                );
                ensure!(refused.body["error"]["code"] == "service_restoring");
                ensure!(refused.retry_after.as_deref() == Some("300"));
            }
            // Refused before authentication, so no idempotency key stores the refusal.
            let anonymous = harness
                .merchant_with(
                    &harness.app,
                    Method::POST,
                    "/v1/api_keys",
                    &body,
                    "ppay_sk_live_invalid",
                )
                .await?;
            ensure!(anonymous.body["error"]["code"] == "service_restoring");
            let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_keys")
                .fetch_one(&harness.pool)
                .await?;
            ensure!(stored == 0);

            let account = harness
                .merchant(Method::GET, "/v1/account", &Value::Null)
                .await?;
            ensure!(account.status == StatusCode::OK && account.body["id"] == harness.account_id());
            let status = harness
                .admin(Method::GET, "/v1/admin/restore", &Value::Null)
                .await?;
            ensure!(status.status == StatusCode::OK);
            ensure!(status.body["frozen"] == true);
            ensure!(status.body["restore"]["detected_by"] == "restore_check");
            ensure!(status.body["rescan"][0]["chain_id"] == 1);
            ensure!(status.body["rescan"][0]["complete"] == false);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn unfreeze_needs_the_rescan_and_the_checklist_and_is_audited() -> Result<()> {
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            let not_frozen = harness
                .admin(
                    Method::POST,
                    "/v1/admin/restore/unfreeze",
                    &checklist("drill"),
                )
                .await?;
            ensure!(not_frozen.status == StatusCode::BAD_REQUEST);
            ensure!(not_frozen.body["error"]["code"] == "restore_not_frozen");

            harness
                .scan_to(100, Utc::now() - TimeDelta::hours(1))
                .await?;
            let restore = harness.restore().await?;
            let held = tokio::spawn({
                let pool = harness.pool.clone();
                async move {
                    restore_mode::wait_until_unfrozen(
                        &pool,
                        std::time::Duration::from_millis(50),
                        &tokio_util::sync::CancellationToken::new(),
                    )
                    .await
                }
            });

            let mut unchecked = checklist("drill");
            unchecked["deposit_addresses_reissued"] = json!(false);
            let refused = harness
                .admin(Method::POST, "/v1/admin/restore/unfreeze", &unchecked)
                .await?;
            ensure!(refused.status == StatusCode::BAD_REQUEST);
            ensure!(refused.body["error"]["param"] == "deposit_addresses_reissued");

            // The chain has not finalized past the moment the restore was detected.
            let incomplete = harness
                .admin(
                    Method::POST,
                    "/v1/admin/restore/unfreeze",
                    &checklist("drill"),
                )
                .await?;
            ensure!(incomplete.status == StatusCode::BAD_REQUEST);
            ensure!(incomplete.body["error"]["code"] == "restore_rescan_incomplete");

            // Caught up, but an issued address's history is not read yet.
            harness
                .scan_to(200, restore.detected_at + TimeDelta::seconds(1))
                .await?;
            let customer = seed::create_customer(
                &harness.pool,
                &seed::NewCustomer {
                    id: Uuid::new_v4(),
                    account_id: harness.account.id,
                    livemode: true,
                    client_reference_id: "team-7".to_owned(),
                    paused_scopes: Vec::new(),
                },
            )
            .await?;
            let address = seed::insert_address(
                &harness.pool,
                &seed::NewAddress {
                    id: Uuid::new_v4(),
                    customer_id: customer.id,
                    chain_id: 1,
                    route: harness.route.route.clone(),
                    salt: B256::repeat_byte(0x11),
                    address: Address::repeat_byte(0x12),
                },
            )
            .await?;
            let status = harness
                .admin(Method::GET, "/v1/admin/restore", &Value::Null)
                .await?;
            ensure!(status.body["rescan"][0]["pending_backfills"] == 1);
            ensure!(status.body["rescan"][0]["complete"] == false);
            sqlx::query("UPDATE addresses SET backfilled = true WHERE id = $1")
                .bind(address.id)
                .execute(&harness.pool)
                .await?;
            ensure!(!held.is_finished());

            let unfrozen = harness
                .admin(
                    Method::POST,
                    "/v1/admin/restore/unfreeze",
                    &checklist("INC-42 reconciled"),
                )
                .await?;
            ensure!(unfrozen.status == StatusCode::OK, "{}", unfrozen.body);
            ensure!(unfrozen.body["id"] == restore.id.to_string());
            ensure!(unfrozen.body["unfrozen_by"] == format!("admin:{ADMIN_KID}"));
            let reason = unfrozen.body["unfreeze_reason"]
                .as_str()
                .context("reason")?;
            ensure!(reason.starts_with("INC-42 reconciled; checklist: "));
            let audit: (String, String, String) = sqlx::query_as(
                "SELECT actor_id, subject, reason FROM audit WHERE action = 'restore.unfreeze'",
            )
            .fetch_one(&harness.pool)
            .await?;
            ensure!(
                audit
                    == (
                        ADMIN_KID.to_owned(),
                        format!("restore:{}", restore.id),
                        reason.to_owned()
                    )
            );
            ensure!(tokio::time::timeout(std::time::Duration::from_secs(5), held).await??);

            // Merchant writes work again; the restore stays on record.
            let created = harness
                .merchant(Method::POST, "/v1/api_keys", &json!({"name": "after"}))
                .await?;
            ensure!(created.status == StatusCode::OK);
            let again = harness
                .admin(
                    Method::POST,
                    "/v1/admin/restore/unfreeze",
                    &checklist("again"),
                )
                .await?;
            ensure!(again.body["error"]["code"] == "restore_not_frozen");
            let status = harness
                .admin(Method::GET, "/v1/admin/restore", &Value::Null)
                .await?;
            ensure!(status.body["frozen"] == false);
            ensure!(status.body["restore"]["unfrozen_at"].is_i64());
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_key_revoked_after_the_restore_point_is_revoked_again_before_the_unfreeze() -> Result<()>
{
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            // The merchant's second key, revoked after the restore point: the restore made it
            // valid again.
            let leaked = seed::create_api_key(&harness.pool, harness.account.id, true).await?;
            let restore_path = "/v1/admin/restore/api_keys/revoke";
            let by_prefix = json!({
                "account": harness.account_id(),
                "prefix": "ppay_sk_live_",
                "last4": &leaked[leaked.len() - 4..],
                "reason": "merchant revoked it at 10:02 PDT",
            });
            let not_frozen = harness
                .admin(Method::POST, restore_path, &by_prefix)
                .await?;
            ensure!(not_frozen.body["error"]["code"] == "restore_not_frozen");
            harness.restore().await?;
            let works = harness
                .merchant_with(
                    &harness.app,
                    Method::GET,
                    "/v1/account",
                    &Value::Null,
                    &leaked,
                )
                .await?;
            ensure!(works.status == StatusCode::OK);

            let revoked = harness
                .admin(Method::POST, restore_path, &by_prefix)
                .await?;
            ensure!(revoked.status == StatusCode::OK, "{}", revoked.body);
            ensure!(revoked.body["status"] == "revoked");
            let refused = harness
                .merchant_with(
                    &harness.app,
                    Method::GET,
                    "/v1/account",
                    &Value::Null,
                    &leaked,
                )
                .await?;
            ensure!(refused.status == StatusCode::UNAUTHORIZED);
            // Repeating it is harmless; the other key keeps working.
            let repeated = harness
                .admin(Method::POST, restore_path, &by_prefix)
                .await?;
            ensure!(repeated.status == StatusCode::OK && repeated.body["id"] == revoked.body["id"]);
            ensure!(
                harness
                    .merchant(Method::GET, "/v1/account", &Value::Null)
                    .await?
                    .status
                    == StatusCode::OK
            );
            let audited: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM audit WHERE action = 'restore.api_key_revoke'",
            )
            .fetch_one(&harness.pool)
            .await?;
            ensure!(audited == 2);
            let announced: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM events WHERE type = 'api_key.revoked' AND account_id = $1",
            )
            .bind(harness.account.id)
            .fetch_one(&harness.pool)
            .await?;
            ensure!(announced == 1);
            // Neither selector, or an unknown one.
            let neither = harness
                .admin(
                    Method::POST,
                    restore_path,
                    &json!({"account": harness.account_id(), "reason": "x"}),
                )
                .await?;
            ensure!(neither.status == StatusCode::BAD_REQUEST);
            let unknown = harness
                .admin(
                    Method::POST,
                    restore_path,
                    &json!({
                        "account": harness.account_id(),
                        "prefix": "ppay_sk_live_",
                        "last4": "zzzz",
                        "reason": "x",
                    }),
                )
                .await?;
            ensure!(unknown.status == StatusCode::NOT_FOUND);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_treasury_cancellation_lost_in_the_restore_is_applied_again() -> Result<()> {
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            let pending = seed::schedule_treasury(
                &harness.pool,
                harness.account.id,
                true,
                1,
                Address::repeat_byte(0x42),
                Utc::now() + TimeDelta::hours(48),
            )
            .await?;
            harness.restore().await?;
            let pending_id = topup::ids::format(topup::ids::TREASURY, pending);
            let missing_id = topup::ids::format(topup::ids::TREASURY, Uuid::new_v4());
            let received = |reapply: bool| {
                json!({
                    "account": harness.account_id(),
                    "livemode": true,
                    "treasuries": [
                        {"id": pending_id, "object": "treasury", "status": "canceled", "chain_id": 1,
                         "address": format!("{:#x}", Address::repeat_byte(0x42))},
                        {"id": missing_id, "status": "pending", "chain_id": 1,
                         "address": format!("{:#x}", Address::repeat_byte(0x43))},
                    ],
                    "reapply": reapply,
                    "reason": "treasury events the merchant received",
                })
            };
            let path = "/v1/admin/restore/treasuries/verify";
            let verified = harness.admin(Method::POST, path, &received(false)).await?;
            ensure!(verified.status == StatusCode::OK, "{}", verified.body);
            ensure!(verified.body["data"][0]["result"] == "cancellation_lost");
            ensure!(verified.body["data"][0]["status"] == "pending");
            ensure!(verified.body["data"][1]["result"] == "missing");
            ensure!(verified.body["data"][1]["status"].is_null());

            let reapplied = harness.admin(Method::POST, path, &received(true)).await?;
            ensure!(reapplied.body["data"][0]["result"] == "canceled");
            ensure!(reapplied.body["data"][0]["status"] == "canceled");
            let again = harness.admin(Method::POST, path, &received(true)).await?;
            ensure!(again.body["data"][0]["result"] == "matches");
            let canceled: Option<chrono::DateTime<Utc>> =
                sqlx::query_scalar("SELECT canceled_at FROM treasuries WHERE id = $1")
                    .bind(pending)
                    .fetch_one(&harness.pool)
                    .await?;
            ensure!(canceled.is_some());
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_treasury_crediting_pause_lost_in_the_restore_is_applied_again() -> Result<()> {
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            let current: Uuid = sqlx::query_scalar(
                "SELECT id FROM treasuries WHERE account_id = $1 AND replaced_at IS NULL",
            )
            .bind(harness.account.id)
            .fetch_one(&harness.pool)
            .await?;
            harness.restore().await?;
            let paused_by = || async {
                let owners: Vec<String> =
                    sqlx::query_scalar("SELECT crediting_paused_by FROM treasuries WHERE id = $1")
                        .bind(current)
                        .fetch_one(&harness.pool)
                        .await?;
                anyhow::Ok(owners)
            };
            let received = |owners: Value, reapply: bool| {
                json!({
                    "account": harness.account_id(),
                    "livemode": true,
                    "treasuries": [{
                        "id": topup::ids::format(topup::ids::TREASURY, current),
                        "status": "active",
                        "chain_id": 1,
                        "address": format!("{:#x}", seed::FIXTURE_TREASURY),
                        "crediting_paused": true,
                        "crediting_paused_by": owners,
                    }],
                    "reapply": reapply,
                    "reason": "treasury.updated the merchant received",
                })
            };
            let path = "/v1/admin/restore/treasuries/verify";
            // The merchant paused crediting to the treasury after the restore point.
            let lost = harness
                .admin(Method::POST, path, &received(json!(["merchant"]), false))
                .await?;
            ensure!(lost.status == StatusCode::OK, "{}", lost.body);
            ensure!(lost.body["data"][0]["result"] == "matches");
            ensure!(lost.body["data"][0]["crediting"] == "pause_lost");
            ensure!(paused_by().await?.is_empty());
            let paused = harness
                .admin(Method::POST, path, &received(json!(["merchant"]), true))
                .await?;
            ensure!(paused.body["data"][0]["crediting"] == "paused");
            ensure!(paused_by().await? == vec!["merchant".to_owned()]);
            let announced: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM events WHERE type = 'treasury.updated' AND object_id = $1",
            )
            .bind(current)
            .fetch_one(&harness.pool)
            .await?;
            ensure!(announced == 1);

            // A resume lost in the window is applied again; the operator's pause is not the
            // merchant's to lift.
            sqlx::query(
                "UPDATE treasuries SET crediting_paused_by = ARRAY['merchant', 'operator'] \
                 WHERE id = $1",
            )
            .bind(current)
            .execute(&harness.pool)
            .await?;
            let resumed = harness
                .admin(Method::POST, path, &received(json!(["operator"]), true))
                .await?;
            ensure!(resumed.body["data"][0]["crediting"] == "resumed");
            ensure!(paused_by().await? == vec!["operator".to_owned()]);
            let matches = harness
                .admin(Method::POST, path, &received(json!(["operator"]), true))
                .await?;
            ensure!(matches.body["data"][0]["crediting"] == "matches");
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_deposit_address_given_out_after_the_restore_point_is_reissued_identically() -> Result<()>
{
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            let first = harness
                .merchant(
                    Method::POST,
                    "/v1/deposit_addresses",
                    &json!({"client_reference_id": "team-42", "metadata": {"plan": "pro"}}),
                )
                .await?;
            ensure!(first.status == StatusCode::OK && first.body["version"] == 1);
            harness.scan_to(100, Utc::now()).await?;
            let restore = harness.restore().await?;
            // The scanner moved on while frozen; the re-issued address is still read from block 100.
            harness.scan_to(250, Utc::now()).await?;

            // After the restore point the merchant rotated the address twice and holds version 3.
            let held = harness.derived_address("team-42", 3);
            let held_id = topup::ids::format(topup::ids::DEPOSIT_ADDRESS, Uuid::new_v4());
            let request = json!({
                "account": harness.account_id(),
                "livemode": true,
                "client_reference_id": "team-42",
                "address": format!("{held:#x}"),
                "id": held_id,
                "reason": "the merchant's export",
            });
            let path = "/v1/admin/restore/deposit_addresses";
            // A version that disagrees with the address is refused before anything is issued.
            let mut disagreeing = request.clone();
            disagreeing["version"] = json!(2);
            let refused = harness.admin(Method::POST, path, &disagreeing).await?;
            ensure!(
                refused.status == StatusCode::BAD_REQUEST,
                "{}",
                refused.body
            );
            let versions: i64 = sqlx::query_scalar("SELECT max(version) FROM deposit_addresses")
                .fetch_one(&harness.pool)
                .await?;
            ensure!(versions == 1);
            let reissued = harness.admin(Method::POST, path, &request).await?;
            ensure!(reissued.status == StatusCode::OK, "{}", reissued.body);
            ensure!(reissued.body["reissued"] == true);
            let object = &reissued.body["deposit_address"];
            ensure!(object["id"] == held_id && object["version"] == 3);
            ensure!(object["status"] == "active" && object["address"] == format!("{held:#x}"));
            ensure!(object["metadata"]["plan"] == "pro");
            let versions: Vec<(i64, String)> = sqlx::query_as(
                "SELECT deposit_address.version, deposit_address.status \
                 FROM deposit_addresses AS deposit_address \
                 JOIN customers AS customer ON customer.id = deposit_address.customer_id \
                 WHERE customer.client_reference_id = 'team-42' ORDER BY 1",
            )
            .fetch_all(&harness.pool)
            .await?;
            ensure!(
                versions
                    == vec![
                        (1, "retired".to_owned()),
                        (2, "retired".to_owned()),
                        (3, "active".to_owned())
                    ],
                "{versions:?}"
            );
            // Version 2's and 3's forwarders are read from the restored cursor.
            let backfill: Vec<(i64, i64, bool)> = sqlx::query_as(
                "SELECT deposit_address.version, address.created_block, address.backfilled \
                 FROM addresses AS address \
                 JOIN deposit_addresses AS deposit_address \
                   ON deposit_address.id = address.deposit_address_id \
                 WHERE deposit_address.version > 1 ORDER BY 1",
            )
            .fetch_all(&harness.pool)
            .await?;
            ensure!(
                backfill == vec![(2, 100, false), (3, 100, false)],
                "{backfill:?}"
            );
            ensure!(restore.restored_cursors.get(&1) == Some(&100));

            // Repeating it returns the same address; a version by number works too.
            let repeated = harness.admin(Method::POST, path, &request).await?;
            ensure!(repeated.body["reissued"] == false);
            ensure!(repeated.body["deposit_address"]["id"] == held_id);
            let new_customer = harness
                .admin(
                    Method::POST,
                    path,
                    &json!({
                        "account": harness.account_id(),
                        "livemode": true,
                        "client_reference_id": "team-new",
                        "version": 1,
                        "reason": "the merchant's export",
                    }),
                )
                .await?;
            ensure!(
                new_customer.body["deposit_address"]["address"]
                    == format!("{:#x}", harness.derived_address("team-new", 1))
            );
            // An address that is not the customer's is refused.
            let mut foreign = request.clone();
            foreign["address"] = json!(format!("{:#x}", Address::repeat_byte(0x99)));
            foreign["id"] = Value::Null;
            let refused = harness.admin(Method::POST, path, &foreign).await?;
            ensure!(
                refused.status == StatusCode::BAD_REQUEST,
                "{}",
                refused.body
            );
            let audited: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM audit WHERE action = 'deposit_address.reissue'",
            )
            .fetch_one(&harness.pool)
            .await?;
            ensure!(audited == 2);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_delivered_event_is_kept_as_delivered_and_never_sent_again() -> Result<()> {
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            let address = harness
                .merchant(
                    Method::POST,
                    "/v1/deposit_addresses",
                    &json!({"client_reference_id": "team-9"}),
                )
                .await?;
            let address_id: Uuid =
                sqlx::query_scalar("SELECT id FROM addresses WHERE address = $1 AND chain_id = 1")
                    .bind(address.body["address"].as_str().context("address")?)
                    .fetch_one(&harness.pool)
                    .await?;
            harness.restore().await?;

            // The merchant received deposit.credited for a deposit credited after the restore
            // point; the rescan has not re-derived it yet.
            let tx_hash = B256::repeat_byte(0x5a);
            let deposit = deposit_id(1, tx_hash, 0);
            let event_id = credited_event_id(deposit);
            let delivered = json!({
                "id": topup::ids::format(topup::ids::EVENT, event_id),
                "object": "event",
                "account": harness.account_id(),
                "livemode": true,
                "type": "deposit.credited",
                "created": 1_790_000_000,
                "actor": "system",
                "data": {"object": {
                    "id": topup::ids::format(topup::ids::DEPOSIT, deposit),
                    "object": "deposit",
                    "livemode": true,
                    "status": "credited",
                    "amount_atomic": "7",
                    "amount": 250,
                }},
            });
            let path = "/v1/admin/restore/events";
            let import = |events: Vec<Value>| json!({"events": events, "reason": "merchant log"});
            let imported = harness
                .admin(Method::POST, path, &import(vec![delivered.clone()]))
                .await?;
            ensure!(imported.status == StatusCode::OK, "{}", imported.body);
            ensure!(imported.body["data"][0]["result"] == "imported");
            let stored = |id: Uuid| {
                let pool = harness.pool.clone();
                async move {
                    let stored: (Value, i64, i64) = sqlx::query_as(
                        "SELECT data, extract(epoch FROM created)::bigint, \
                                (SELECT count(*) FROM webhook_deliveries WHERE event_id = $1) \
                         FROM events WHERE id = $1",
                    )
                    .bind(id)
                    .fetch_one(&pool)
                    .await?;
                    anyhow::Ok(stored)
                }
            };
            ensure!(stored(event_id).await? == (delivered["data"].clone(), 1_790_000_000, 0));

            // After the unfreeze the rescan credits the deposit again, re-valued: its event is
            // recorded already, so nothing is delivered and the delivered body stays.
            let mut transaction = harness.pool.begin().await?;
            db::enqueue_in(
                &mut transaction,
                &topup::routes::RouteSet::default(),
                &db::NewOutboxEvent {
                    id: event_id,
                    event_type: "deposit.credited".to_owned(),
                    account_id: harness.account.id,
                    livemode: true,
                    object: db::EventObject::Deposit(deposit),
                    next_attempt_at: Utc::now(),
                    actor: db::SYSTEM_ACTOR.to_owned(),
                    request: None,
                    signing_key_version: None,
                },
                None,
            )
            .await?;
            transaction.commit().await?;
            ensure!(stored(event_id).await? == (delivered["data"].clone(), 1_790_000_000, 0));

            // Importing it again matches; another body is a mismatch and changes nothing.
            let mut altered = delivered.clone();
            altered["data"]["object"]["amount"] = json!(260);
            let again = harness
                .admin(
                    Method::POST,
                    path,
                    &import(vec![delivered.clone(), altered]),
                )
                .await?;
            ensure!(again.body["data"][0]["result"] == "matches");
            ensure!(again.body["data"][1]["result"] == "mismatch");
            ensure!(stored(event_id).await? == (delivered["data"].clone(), 1_790_000_000, 0));

            // Compared with the ledger: pending until the rescan values the deposit, then a
            // mismatch when the re-valued credit differs from the delivered one.
            let findings = |body: &Value| body["delivered_events"]["findings"].clone();
            let status = harness
                .admin(Method::GET, "/v1/admin/restore", &Value::Null)
                .await?;
            ensure!(status.body["delivered_events"]["imported"] == 1);
            ensure!(findings(&status.body)[0]["status"] == "pending");
            ensure!(
                db::insert_deposit(
                    &harness.pool,
                    &NewDeposit {
                        chain_id: 1,
                        tx_hash,
                        receipt_log_index: 0,
                        log_index: 0,
                        block_number: 120,
                        block_hash: B256::repeat_byte(0xb1),
                        block_time: Utc::now(),
                        address_id,
                        route: Some(harness.route.route.clone()),
                        route_version: Some(harness.route.version),
                        asset_contract: harness.route.asset.contract,
                        from_address: Address::repeat_byte(0x74),
                        amount_atomic: AtomicAmount::new(U256::from(7_u64)),
                        state: DepositState::Detected,
                        reason: None,
                        next_attempt_at: Utc::now(),
                        tx_from: Address::repeat_byte(0x74),
                        tx_nonce: 0,
                        is_final: false,
                    },
                )
                .await?
            );
            sqlx::query("UPDATE deposits SET credit_minor = 260 WHERE id = $1")
                .bind(deposit)
                .execute(&harness.owner)
                .await?;
            let status = harness
                .admin(Method::GET, "/v1/admin/restore", &Value::Null)
                .await?;
            let finding = &findings(&status.body)[0];
            ensure!(finding["status"] == "mismatch", "{finding}");
            ensure!(finding["delivered_amount"] == "250" && finding["ledger_amount"] == "260");
            sqlx::query("UPDATE deposits SET credit_minor = 250 WHERE id = $1")
                .bind(deposit)
                .execute(&harness.owner)
                .await?;
            let status = harness
                .admin(Method::GET, "/v1/admin/restore", &Value::Null)
                .await?;
            ensure!(findings(&status.body) == json!([]));

            // An event whose id is not the one its type and deposit derive is refused.
            let mut forged = delivered.clone();
            forged["id"] = json!(topup::ids::format(topup::ids::EVENT, Uuid::new_v4()));
            let refused = harness
                .admin(Method::POST, path, &import(vec![forged]))
                .await?;
            ensure!(refused.status == StatusCode::BAD_REQUEST);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn the_restore_check_instance_takes_only_reads_and_the_restore_reconciliation() -> Result<()>
{
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            harness.restore().await?;
            let refused = harness
                .merchant_with(
                    &harness.read_only,
                    Method::POST,
                    "/v1/api_keys",
                    &json!({"name": "ci"}),
                    &harness.key,
                )
                .await?;
            ensure!(refused.status == StatusCode::SERVICE_UNAVAILABLE);
            ensure!(refused.body["error"]["code"] == "service_restoring");
            ensure!(refused.retry_after.as_deref() == Some("300"));
            let admin = harness
                .admin_on(
                    &harness.read_only,
                    Method::POST,
                    &format!("/v1/admin/accounts/{}/pause", harness.account_id()),
                    &json!({"scopes": ["quotes"], "reason": "x"}),
                )
                .await?;
            ensure!(admin.body["error"]["code"] == "service_restoring");
            let read = harness
                .merchant_with(
                    &harness.read_only,
                    Method::GET,
                    "/v1/account",
                    &Value::Null,
                    &harness.key,
                )
                .await?;
            ensure!(read.status == StatusCode::OK);
            // The restore reconciliation reaches its handler: nothing scans here, so the freeze
            // cannot be lifted, but a key can be revoked again.
            let unfreeze = harness
                .admin_on(
                    &harness.read_only,
                    Method::POST,
                    "/v1/admin/restore/unfreeze",
                    &checklist("drill"),
                )
                .await?;
            ensure!(unfreeze.body["error"]["code"] == "restore_rescan_incomplete");
            let leaked = seed::create_api_key(&harness.pool, harness.account.id, true).await?;
            let revoked = harness
                .admin_on(
                    &harness.read_only,
                    Method::POST,
                    "/v1/admin/restore/api_keys/revoke",
                    &json!({
                        "account": harness.account_id(),
                        "prefix": "ppay_sk_live_",
                        "last4": &leaked[leaked.len() - 4..],
                        "reason": "merchant revoked it",
                    }),
                )
                .await?;
            ensure!(revoked.status == StatusCode::OK, "{}", revoked.body);
            Ok(())
        })
    })
    .await
}

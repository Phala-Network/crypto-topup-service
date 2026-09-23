//! C12 refund and support integration tests.

mod support;

use std::collections::{BTreeMap, VecDeque};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use axum::body::to_bytes;
use axum::http::{Method, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use chrono::{Duration, Utc};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use sqlx::Row;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use topup::api::{AppState, PublicOrigin, UnavailableAttestor, VerificationKey};
use topup::db::{AddressKind, NewAccount, NewAddress, NewDeposit, NewProduct};
use topup::refunds::{
    EvmRefundChainReader, RefundChainReader, RefundCheck, RefundConfirmationConfig,
    RefundConfirmationWorker, RefundObservation, RefundReadError, RefundTransfer,
};
use topup_core::deposit::{DepositState, RejectReason};
use topup_core::identity::deposit_id;
use topup_core::money::AtomicAmount;
use topup_core::route::RouteFile;
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;

use support::{TEST_ORIGIN, TestDatabase, public_key_base64, signed_request};

const PRODUCT_KID: &str = "phala-cloud/v1";
const ADMIN_KID: &str = "admin/v1";
const REFUND_DESTINATION: &str = "0x4444444444444444444444444444444444444444";
const REFUND_TX: &str = "0xdddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const REPLACEMENT_TX: &str = "0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";

#[tokio::test]
async fn refund_flow_confirms_only_matching_finalized_transfer() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[41; 32]);
        let other_key = SigningKey::from_bytes(&[42; 32]);
        let admin_key = SigningKey::from_bytes(&[43; 32]);
        let product =
            seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        let other = seed_product(&database.app_pool, "builder", &other_key).await?;
        let deposit =
            seed_rejected_deposit(&database.app_pool, product.id, "refund-account", 150).await?;
        let other_deposit =
            seed_rejected_deposit(&database.app_pool, other.id, "other-account", 150).await?;
        insert_transition(&database.app_pool, deposit).await?;
        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();
        let body = serde_json::to_vec(&json!({
            "to_address": REFUND_DESTINATION,
            "amount": "100"
        }))?;

        let cross_tenant_path = format!(
            "/v1/products/{}/deposits/{other_deposit}/refund-requests",
            product.slug
        );
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &cross_tenant_path,
                body.clone(),
                PRODUCT_KID,
                &product_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::NOT_FOUND);

        topup::db::set_account_paused_scopes(
            &database.app_pool,
            account_id(&database.app_pool, deposit).await?,
            &["refunds".to_owned()],
        )
        .await?;
        let request_path = format!(
            "/v1/products/{}/deposits/{deposit}/refund-requests",
            product.slug
        );
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &request_path,
                body.clone(),
                PRODUCT_KID,
                &product_key,
                now + 1,
            ))
            .await?;
        ensure!(response.status() == StatusCode::LOCKED);
        topup::db::set_account_paused_scopes(
            &database.app_pool,
            account_id(&database.app_pool, deposit).await?,
            &[],
        )
        .await?;

        topup::db::set_product_paused_scopes(
            &database.app_pool,
            product.id,
            &["refunds".to_owned()],
        )
        .await?;
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &request_path,
                body.clone(),
                PRODUCT_KID,
                &product_key,
                now + 2,
            ))
            .await?;
        ensure!(response.status() == StatusCode::LOCKED);
        topup::db::set_product_paused_scopes(&database.app_pool, product.id, &[]).await?;

        sqlx::query(
            "INSERT INTO route_pauses (route, paused_scopes) VALUES ($1, ARRAY['refunds']) ON CONFLICT (route) DO UPDATE SET paused_scopes = EXCLUDED.paused_scopes",
        )
        .bind("phala-cloud-ethereum-pha-usd")
        .execute(&database.app_pool)
        .await?;
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &request_path,
                body.clone(),
                PRODUCT_KID,
                &product_key,
                now + 3,
            ))
            .await?;
        ensure!(response.status() == StatusCode::LOCKED);
        sqlx::query("UPDATE route_pauses SET paused_scopes = '{}' WHERE route = $1")
            .bind("phala-cloud-ethereum-pha-usd")
            .execute(&database.app_pool)
            .await?;

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &request_path,
                body.clone(),
                PRODUCT_KID,
                &product_key,
                now + 4,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let requested = response_json(response).await?;
        let refund_id = Uuid::parse_str(requested["id"].as_str().context("refund id")?)?;
        ensure!(requested["status"] == "requested");
        ensure!(requested["amount_atomic"] == "100");

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &request_path,
                body,
                PRODUCT_KID,
                &product_key,
                now + 5,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        ensure!(response_json(response).await?["id"] == requested["id"]);
        let refund_count: i64 = sqlx::query_scalar("SELECT count(*) FROM refunds")
            .fetch_one(&database.app_pool)
            .await?;
        ensure!(refund_count == 1);

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &request_path,
                serde_json::to_vec(&json!({
                    "to_address": "0x6666666666666666666666666666666666666666",
                    "amount": "60"
                }))?,
                PRODUCT_KID,
                &product_key,
                now + 6,
            ))
            .await?;
        ensure!(response.status() == StatusCode::CONFLICT);

        let approve_path = format!("/v1/admin/refunds/{refund_id}/approve");
        topup::db::set_account_paused_scopes(
            &database.app_pool,
            account_id(&database.app_pool, deposit).await?,
            &["refunds".to_owned()],
        )
        .await?;
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &approve_path,
                Vec::new(),
                ADMIN_KID,
                &admin_key,
                now + 7,
            ))
            .await?;
        ensure!(response.status() == StatusCode::LOCKED);
        topup::db::set_account_paused_scopes(
            &database.app_pool,
            account_id(&database.app_pool, deposit).await?,
            &[],
        )
        .await?;

        topup::db::set_product_paused_scopes(
            &database.app_pool,
            product.id,
            &["refunds".to_owned()],
        )
        .await?;
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &approve_path,
                Vec::new(),
                ADMIN_KID,
                &admin_key,
                now + 8,
            ))
            .await?;
        ensure!(response.status() == StatusCode::LOCKED);
        topup::db::set_product_paused_scopes(&database.app_pool, product.id, &[]).await?;

        sqlx::query(
            "UPDATE route_pauses SET paused_scopes = ARRAY['refunds'] WHERE route = $1",
        )
        .bind("phala-cloud-ethereum-pha-usd")
        .execute(&database.app_pool)
        .await?;
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &approve_path,
                Vec::new(),
                ADMIN_KID,
                &admin_key,
                now + 9,
            ))
            .await?;
        ensure!(response.status() == StatusCode::LOCKED);
        sqlx::query("UPDATE route_pauses SET paused_scopes = '{}' WHERE route = $1")
            .bind("phala-cloud-ethereum-pha-usd")
            .execute(&database.app_pool)
            .await?;

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &approve_path,
                Vec::new(),
                ADMIN_KID,
                &admin_key,
                now + 10,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        ensure!(response_json(response).await?["status"] == "approved");

        let record_path = format!("/v1/admin/refunds/{refund_id}/record");
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &record_path,
                serde_json::to_vec(&json!({"tx_hash": REFUND_TX}))?,
                ADMIN_KID,
                &admin_key,
                now + 11,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        ensure!(response_json(response).await?["status"] == "sent");

        let route = route_fixture();
        let reader = ScriptedReader::new(vec![
            RefundObservation::Finalized {
                block_number: 90,
                succeeded: true,
                transfers: vec![RefundTransfer {
                    log_index: 4,
                    transferred_atomic: U256::from(99_u64),
                }],
            },
            RefundObservation::Finalized {
                block_number: 90,
                succeeded: true,
                transfers: vec![RefundTransfer {
                    log_index: 4,
                    transferred_atomic: U256::from(100_u64),
                }],
            },
        ]);
        let worker = RefundConfirmationWorker::new(
            database.app_pool.clone(),
            reader,
            &[route],
            RefundConfirmationConfig {
                poll_interval: StdDuration::ZERO,
                retry_interval: StdDuration::ZERO,
                request_timeout: StdDuration::from_secs(1),
                observe_timeout: StdDuration::from_secs(1),
            },
        )?;
        ensure!(worker.check_once().await?);
        let mismatch =
            sqlx::query("SELECT status, confirmation_evidence FROM refunds WHERE id = $1")
                .bind(refund_id)
                .fetch_one(&database.app_pool)
                .await?;
        ensure!(mismatch.try_get::<String, _>("status")? == "sent");
        ensure!(mismatch.try_get::<Value, _>("confirmation_evidence")?["result"] == "mismatch");
        ensure!(worker.check_once().await?);

        let confirmed =
            sqlx::query("SELECT status, confirmation_evidence FROM refunds WHERE id = $1")
                .bind(refund_id)
                .fetch_one(&database.app_pool)
                .await?;
        ensure!(confirmed.try_get::<String, _>("status")? == "confirmed");
        ensure!(confirmed.try_get::<Value, _>("confirmation_evidence")?["result"] == "matched");
        let event: Value =
            sqlx::query_scalar("SELECT payload FROM outbox WHERE event_type = 'deposit.refunded'")
                .fetch_one(&database.app_pool)
                .await?;
        ensure!(event["refund_id"] == refund_id.to_string());
        ensure!(event["product_id"] == product.id.to_string());

        let tx_hash: String = sqlx::query_scalar("SELECT tx_hash FROM deposits WHERE id = $1")
            .bind(deposit)
            .fetch_one(&database.app_pool)
            .await?;
        let lookup_path = format!(
            "/v1/products/{}/deposits?tx_hash={tx_hash}",
            product.slug
        );
        let response = app
            .oneshot(signed_request(
                Method::GET,
                &lookup_path,
                Vec::new(),
                PRODUCT_KID,
                &product_key,
                now + 6,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let lookup = response_json(response).await?;
        ensure!(lookup["deposits"][0]["id"] == deposit.to_string());
        ensure!(lookup["deposits"][0]["timeline"][0]["to_state"] == "rejected");
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn refund_request_requires_rejection_and_approval_rechecks_current_state() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[61; 32]);
        let admin_key = SigningKey::from_bytes(&[62; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        let pending = seed_deposit(
            &database.app_pool,
            product.id,
            "pending-refund",
            100,
            DepositState::Detected,
            None,
        )
        .await?;
        let refundable =
            seed_rejected_deposit(&database.app_pool, product.id, "approval-recheck", 100).await?;
        let sanctioned =
            seed_rejected_deposit(&database.app_pool, product.id, "sanctions-recheck", 100).await?;
        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();
        let body = serde_json::to_vec(&json!({
            "to_address": REFUND_DESTINATION,
            "amount": "100"
        }))?;

        let pending_path = format!(
            "/v1/products/{}/deposits/{pending}/refund-requests",
            product.slug
        );
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &pending_path,
                body.clone(),
                PRODUCT_KID,
                &product_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::CONFLICT);

        let request_path = format!(
            "/v1/products/{}/deposits/{refundable}/refund-requests",
            product.slug
        );
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &request_path,
                body,
                PRODUCT_KID,
                &product_key,
                now + 1,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let requested = response_json(response).await?;
        let refund_id = Uuid::parse_str(requested["id"].as_str().context("refund id")?)?;

        sqlx::query("UPDATE deposits SET state = 'credited', reason = NULL WHERE id = $1")
            .bind(refundable)
            .execute(&database.app_pool)
            .await?;
        let approve_path = format!("/v1/admin/refunds/{refund_id}/approve");
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &approve_path,
                Vec::new(),
                ADMIN_KID,
                &admin_key,
                now + 2,
            ))
            .await?;
        ensure!(response.status() == StatusCode::CONFLICT);
        let status: String = sqlx::query_scalar("SELECT status FROM refunds WHERE id = $1")
            .bind(refund_id)
            .fetch_one(&database.app_pool)
            .await?;
        ensure!(status == "requested");
        let approvals: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit WHERE action = 'refund_approved' AND subject = $1",
        )
        .bind(format!("refund:{refund_id}"))
        .fetch_one(&database.app_pool)
        .await?;
        ensure!(approvals == 0);

        let sanctioned_path = format!(
            "/v1/products/{}/deposits/{sanctioned}/refund-requests",
            product.slug
        );
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &sanctioned_path,
                serde_json::to_vec(&json!({
                    "to_address": REFUND_DESTINATION,
                    "amount": "100"
                }))?,
                PRODUCT_KID,
                &product_key,
                now + 3,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let requested = response_json(response).await?;
        let sanctioned_refund =
            Uuid::parse_str(requested["id"].as_str().context("sanctioned refund id")?)?;
        sqlx::query("UPDATE deposits SET reason = 'sanctioned' WHERE id = $1")
            .bind(sanctioned)
            .execute(&database.app_pool)
            .await?;
        let approve_path = format!("/v1/admin/refunds/{sanctioned_refund}/approve");
        let response = app
            .oneshot(signed_request(
                Method::POST,
                &approve_path,
                Vec::new(),
                ADMIN_KID,
                &admin_key,
                now + 4,
            ))
            .await?;
        ensure!(response.status() == StatusCode::CONFLICT);
        let status: String = sqlx::query_scalar("SELECT status FROM refunds WHERE id = $1")
            .bind(sanctioned_refund)
            .fetch_one(&database.app_pool)
            .await?;
        ensure!(status == "requested");
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn unsupported_refund_approval_uses_persisted_fallback_route_pause() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[63; 32]);
        let admin_key = SigningKey::from_bytes(&[64; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        let deposit =
            seed_rejected_deposit(&database.app_pool, product.id, "unsupported-refund", 100)
                .await?;
        sqlx::query(
            r#"
            UPDATE deposits
            SET route = NULL, route_version = NULL, reason = 'unsupported_asset',
                asset_contract = '0x7777777777777777777777777777777777777777'
            WHERE id = $1
            "#,
        )
        .bind(deposit)
        .execute(&database.app_pool)
        .await?;
        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();
        let request_path = format!(
            "/v1/products/{}/deposits/{deposit}/refund-requests",
            product.slug
        );
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &request_path,
                serde_json::to_vec(&json!({
                    "to_address": REFUND_DESTINATION,
                    "amount": "100"
                }))?,
                PRODUCT_KID,
                &product_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let requested = response_json(response).await?;
        let refund_id = Uuid::parse_str(requested["id"].as_str().context("refund id")?)?;
        let persisted_route: String = sqlx::query_scalar("SELECT route FROM refunds WHERE id = $1")
            .bind(refund_id)
            .fetch_one(&database.app_pool)
            .await?;
        ensure!(persisted_route == "phala-cloud-ethereum-pha-usd");

        sqlx::query(
            r#"
            INSERT INTO route_pauses (route, paused_scopes)
            VALUES ($1, ARRAY['refunds'])
            ON CONFLICT (route) DO UPDATE SET paused_scopes = EXCLUDED.paused_scopes
            "#,
        )
        .bind(&persisted_route)
        .execute(&database.app_pool)
        .await?;
        let approve_path = format!("/v1/admin/refunds/{refund_id}/approve");
        let response = app
            .oneshot(signed_request(
                Method::POST,
                &approve_path,
                Vec::new(),
                ADMIN_KID,
                &admin_key,
                now + 1,
            ))
            .await?;
        ensure!(response.status() == StatusCode::LOCKED);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn one_transfer_log_confirms_only_one_refund() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let key = SigningKey::from_bytes(&[44; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", &key).await?;
        let first = seed_rejected_deposit(&database.app_pool, product.id, "claim-one", 100).await?;
        let second =
            seed_rejected_deposit(&database.app_pool, product.id, "claim-two", 100).await?;
        let first_refund = seed_sent_refund(&database.app_pool, first, 100, REFUND_TX).await?;
        let second_refund = seed_sent_refund(&database.app_pool, second, 100, REFUND_TX).await?;
        let observation = RefundObservation::Finalized {
            block_number: 90,
            succeeded: true,
            transfers: vec![RefundTransfer {
                log_index: 7,
                transferred_atomic: U256::from(100_u64),
            }],
        };
        let worker = test_worker(
            &database.app_pool,
            ScriptedReader::new(vec![observation.clone(), observation]),
        )?;
        ensure!(worker.check_once().await?);
        ensure!(worker.check_once().await?);

        let confirmed: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM refunds WHERE id = ANY($1) AND status = 'confirmed'",
        )
        .bind([first_refund, second_refund])
        .fetch_one(&database.app_pool)
        .await?;
        let sent: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM refunds WHERE id = ANY($1) AND status = 'sent'",
        )
        .bind([first_refund, second_refund])
        .fetch_one(&database.app_pool)
        .await?;
        let claims: i64 = sqlx::query_scalar("SELECT count(*) FROM refund_payment_claims")
            .fetch_one(&database.app_pool)
            .await?;
        let events: i64 =
            sqlx::query_scalar("SELECT count(*) FROM outbox WHERE event_type = 'deposit.refunded'")
                .fetch_one(&database.app_pool)
                .await?;
        ensure!(confirmed == 1);
        ensure!(sent == 1);
        ensure!(claims == 1);
        ensure!(events == 1);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn corrected_hash_rejects_stale_observation_then_confirms_replacement() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[45; 32]);
        let admin_key = SigningKey::from_bytes(&[46; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        let deposit =
            seed_rejected_deposit(&database.app_pool, product.id, "correction", 100).await?;
        let refund_id = seed_approved_refund(&database.app_pool, deposit, 100).await?;
        let app = test_router(&database.app_pool, &admin_key);
        let record_path = format!("/v1/admin/refunds/{refund_id}/record");
        let now = Utc::now().timestamp();

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &record_path,
                serde_json::to_vec(&json!({"tx_hash": REFUND_TX}))?,
                ADMIN_KID,
                &admin_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);

        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let stale_worker = test_worker(
            &database.app_pool,
            BlockingReader {
                expected_tx: B256::from_str(REFUND_TX)?,
                started: Arc::clone(&started),
                release: Arc::clone(&release),
            },
        )?;
        let stale_task = tokio::spawn(async move { stale_worker.check_once().await });
        started.notified().await;

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &record_path,
                serde_json::to_vec(&json!({"tx_hash": REPLACEMENT_TX}))?,
                ADMIN_KID,
                &admin_key,
                now + 1,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let corrected = response_json(response).await?;
        ensure!(corrected["tx_hash"] == REPLACEMENT_TX);
        ensure!(corrected["confirmation_evidence"]["previous_tx_hash"] == REFUND_TX);

        release.notify_one();
        ensure!(stale_task.await??);
        let row = sqlx::query(
            "SELECT status, tx_hash, tx_version, confirmation_evidence FROM refunds WHERE id = $1",
        )
        .bind(refund_id)
        .fetch_one(&database.app_pool)
        .await?;
        ensure!(row.try_get::<String, _>("status")? == "sent");
        ensure!(row.try_get::<String, _>("tx_hash")? == REPLACEMENT_TX);
        ensure!(row.try_get::<i64, _>("tx_version")? == 2);
        ensure!(row.try_get::<Value, _>("confirmation_evidence")?["result"] == "tx_hash_corrected");

        let corrected_worker = test_worker(
            &database.app_pool,
            ScriptedReader::for_tx(
                REPLACEMENT_TX,
                vec![RefundObservation::Finalized {
                    block_number: 91,
                    succeeded: true,
                    transfers: vec![RefundTransfer {
                        log_index: 8,
                        transferred_atomic: U256::from(100_u64),
                    }],
                }],
            ),
        )?;
        ensure!(corrected_worker.check_once().await?);
        let status: String = sqlx::query_scalar("SELECT status FROM refunds WHERE id = $1")
            .bind(refund_id)
            .fetch_one(&database.app_pool)
            .await?;
        ensure!(status == "confirmed");
        let correction_audits: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit WHERE action = 'refund_tx_hash_corrected' AND subject = $1",
        )
        .bind(format!("refund:{refund_id}"))
        .fetch_one(&database.app_pool)
        .await?;
        ensure!(correction_audits == 1);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn support_lookup_uses_tenant_scoped_keyset_pages() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[47; 32]);
        let admin_key = SigningKey::from_bytes(&[48; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        let address = seed_same_address_deposits(&database.app_pool, product.id, 52).await?;
        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();
        let first_path = format!("/v1/products/{}/deposits?address={address}", product.slug);
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::GET,
                &first_path,
                Vec::new(),
                PRODUCT_KID,
                &product_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let first = response_json(response).await?;
        ensure!(first["deposits"].as_array().context("first page")?.len() == 50);
        let cursor = first["next_cursor"].as_str().context("next cursor")?;
        let second_path = format!("{first_path}&cursor={cursor}");
        let response = app
            .oneshot(signed_request(
                Method::GET,
                &second_path,
                Vec::new(),
                PRODUCT_KID,
                &product_key,
                now + 1,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let second = response_json(response).await?;
        ensure!(second["deposits"].as_array().context("second page")?.len() == 2);
        ensure!(second["next_cursor"].is_null());
        let first_ids = first["deposits"]
            .as_array()
            .context("first page")?
            .iter()
            .map(|deposit| deposit["id"].as_str().unwrap_or_default())
            .collect::<std::collections::BTreeSet<_>>();
        ensure!(
            second["deposits"]
                .as_array()
                .context("second page")?
                .iter()
                .all(|deposit| !first_ids.contains(deposit["id"].as_str().unwrap_or_default()))
        );
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn evm_reader_rejects_wrong_or_unfinalized_transfers_and_times_out() -> Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().route("/", post(refund_rpc))).await
    });
    let reader = EvmRefundChainReader::from_chain_urls(
        BTreeMap::from([(1, Url::parse(&format!("http://{address}"))?)]),
        StdDuration::from_millis(50),
    )?;
    let route = route_fixture();
    let treasury = route.chain.contracts.treasury;
    let destination = Address::from_str(REFUND_DESTINATION)?;
    let cases = [
        (1_u64, false, true, 0_usize),
        (2, false, true, 0),
        (3, false, true, 0),
        (4, true, true, 0),
        (5, false, false, 1),
    ];
    for (hash, pending, succeeded, transfers) in cases {
        let observation = reader
            .observe(&RefundCheck {
                refund_id: Uuid::new_v4(),
                product_id: Uuid::new_v4(),
                deposit_id: Uuid::new_v4(),
                chain_id: 1,
                asset_contract: route.asset.contract,
                treasury,
                to_address: destination,
                amount_atomic: U256::from(100_u64),
                tx_hash: B256::from(U256::from(hash)),
                tx_version: 1,
            })
            .await?;
        if pending {
            ensure!(observation == RefundObservation::Pending);
        } else {
            let RefundObservation::Finalized {
                succeeded: actual_succeeded,
                transfers: actual_transfers,
                ..
            } = observation
            else {
                anyhow::bail!("expected finalized observation");
            };
            ensure!(actual_succeeded == succeeded);
            ensure!(actual_transfers.len() == transfers);
        }
    }
    let hung = reader
        .observe(&RefundCheck {
            refund_id: Uuid::new_v4(),
            product_id: Uuid::new_v4(),
            deposit_id: Uuid::new_v4(),
            chain_id: 1,
            asset_contract: route.asset.contract,
            treasury,
            to_address: destination,
            amount_atomic: U256::from(100_u64),
            tx_hash: B256::from(U256::from(6_u64)),
            tx_version: 1,
        })
        .await;
    ensure!(matches!(hung, Err(RefundReadError::Rpc(_))));
    server.abort();
    let _ = server.await;
    Ok(())
}

#[tokio::test]
async fn worker_shutdown_cancels_a_hung_observation() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let key = SigningKey::from_bytes(&[49; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", &key).await?;
        let deposit =
            seed_rejected_deposit(&database.app_pool, product.id, "hung-worker", 100).await?;
        seed_sent_refund(&database.app_pool, deposit, 100, REFUND_TX).await?;
        let started = Arc::new(Notify::new());
        let worker = RefundConfirmationWorker::new(
            database.app_pool.clone(),
            HangingReader {
                started: Arc::clone(&started),
            },
            &[route_fixture()],
            RefundConfirmationConfig {
                poll_interval: StdDuration::from_secs(60),
                retry_interval: StdDuration::ZERO,
                request_timeout: StdDuration::from_secs(60),
                observe_timeout: StdDuration::from_secs(60),
            },
        )?;
        let cancellation = CancellationToken::new();
        let child = cancellation.clone();
        let task = tokio::spawn(async move { worker.run(child).await });
        started.notified().await;
        cancellation.cancel();
        tokio::time::timeout(StdDuration::from_millis(100), task).await??;
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn admin_nudge_and_daily_report_use_seeded_integer_facts() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[51; 32]);
        let admin_key = SigningKey::from_bytes(&[52; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        let rejected = seed_rejected_deposit(&database.app_pool, product.id, "report-rejected", 100).await?;
        let unsupported =
            seed_rejected_deposit(&database.app_pool, product.id, "report-unsupported", 25).await?;
        let credited = seed_deposit(
            &database.app_pool,
            product.id,
            "report-credited",
            200,
            DepositState::Credited,
            None,
        )
        .await?;
        sqlx::query(
            r#"
            UPDATE deposits
            SET route = NULL,
                route_version = NULL,
                reason = 'unsupported_asset',
                asset_contract = '0x7777777777777777777777777777777777777777'
            WHERE id = $1
            "#,
        )
        .bind(unsupported)
        .execute(&database.app_pool)
        .await?;
        sqlx::query("UPDATE deposits SET next_attempt_at = now() + interval '1 day', created_at = now() - interval '2 hours' WHERE id = ANY($1)")
            .bind([rejected, credited])
            .execute(&database.app_pool)
            .await?;
        sqlx::query(
            r#"
            INSERT INTO settlements (deposit_id, product_id, key, payload, status, destination_tx_id)
            VALUES ($1, $2, $3, '{}', 'accepted', 'destination-1')
            "#,
        )
        .bind(credited)
        .bind(product.id)
        .bind(format!("deposit:{credited}"))
        .execute(&database.app_pool)
        .await?;
        seed_open_lock(&database.app_pool, product.id).await?;
        seed_expired_lock(&database.app_pool, product.id).await?;
        seed_refund_row(&database.app_pool, rejected, 20).await?;
        // The report reads the global counter that lock creation maintains.
        sqlx::query("INSERT INTO lock_exposure (scope_key, open_minor) VALUES ('global', 50)")
            .execute(&database.app_pool)
            .await?;

        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();
        let nudge_path = format!("/v1/admin/deposits/{rejected}/nudge");
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &nudge_path,
                Vec::new(),
                ADMIN_KID,
                &admin_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let state: String = sqlx::query_scalar("SELECT state FROM deposits WHERE id = $1")
            .bind(rejected)
            .fetch_one(&database.app_pool)
            .await?;
        ensure!(state == "rejected");
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit WHERE action = 'deposit_nudged' AND subject = $1",
        )
        .bind(format!("deposit:{rejected}"))
        .fetch_one(&database.app_pool)
        .await?;
        ensure!(audit_count == 1);

        let response = app
            .oneshot(signed_request(
                Method::GET,
                "/v1/admin/report/daily",
                Vec::new(),
                ADMIN_KID,
                &admin_key,
                now + 1,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let report = response_json(response).await?;
        ensure!(report["exposure_minor"] == "50");
        let routes = report["routes"].as_array().context("report routes")?;
        let route = routes
            .iter()
            .find(|route| route["route"] == "phala-cloud-ethereum-pha-usd")
            .context("configured route report")?;
        ensure!(route["route"] == "phala-cloud-ethereum-pha-usd");
        ensure!(route["treasury_balance_atomic"].is_null());
        ensure!(route["treasury_balance_note"]
            .as_str()
            .context("treasury note")?
            .contains("not configured"));
        ensure!(route["unflushed_balance_atomic"] == "300");
        ensure!(route["open_rate_lock_exposure_atomic"] == "50");
        ensure!(route["rejected_holds_atomic"] == "100");
        ensure!(route["deposits_by_state"]["rejected"] == 1);
        ensure!(route["deposits_by_state"]["credited"] == 1);
        ensure!(route["settlements_by_status"]["accepted"] == 1);
        ensure!(route["refunds_by_status"]["requested"] == 1);
        ensure!(route["age_in_state_max_seconds"]["credited"].as_u64().context("credited age")? >= 7_000);
        let unrouted = routes
            .iter()
            .find(|route| {
                route["route"]
                    == "unrouted:1:0x7777777777777777777777777777777777777777"
            })
            .context("unrouted asset report")?;
        ensure!(unrouted["unflushed_balance_atomic"] == "25");
        ensure!(unrouted["rejected_holds_atomic"] == "25");
        ensure!(unrouted["deposits_by_state"]["rejected"] == 1);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

struct ScriptedReader {
    expected_tx: B256,
    observations: Mutex<VecDeque<RefundObservation>>,
}

impl ScriptedReader {
    fn new(observations: Vec<RefundObservation>) -> Self {
        Self::for_tx(REFUND_TX, observations)
    }

    fn for_tx(tx_hash: &str, observations: Vec<RefundObservation>) -> Self {
        Self {
            expected_tx: B256::from_str(tx_hash).expect("valid scripted refund tx"),
            observations: Mutex::new(observations.into()),
        }
    }
}

#[async_trait]
impl RefundChainReader for ScriptedReader {
    async fn observe(&self, check: &RefundCheck) -> Result<RefundObservation, RefundReadError> {
        assert_eq!(check.tx_hash, self.expected_tx);
        assert_eq!(
            check.to_address,
            Address::from_str(REFUND_DESTINATION).expect("valid destination")
        );
        assert_eq!(check.amount_atomic, U256::from(100_u64));
        self.observations
            .lock()
            .map_err(|_| RefundReadError::Rpc("mock lock"))?
            .pop_front()
            .ok_or(RefundReadError::Rpc("mock observation"))
    }
}

struct BlockingReader {
    expected_tx: B256,
    started: Arc<Notify>,
    release: Arc<Notify>,
}

struct HangingReader {
    started: Arc<Notify>,
}

#[async_trait]
impl RefundChainReader for HangingReader {
    async fn observe(&self, _check: &RefundCheck) -> Result<RefundObservation, RefundReadError> {
        self.started.notify_one();
        std::future::pending().await
    }
}

async fn refund_rpc(Json(request): Json<Value>) -> Json<Value> {
    let id = request["id"].clone();
    let method = request["method"].as_str().unwrap_or_default();
    if method == "eth_getTransactionReceipt" {
        let tx_hash = request["params"][0].as_str().unwrap_or_default();
        if tx_hash == format!("{:#x}", B256::from(U256::from(6_u64))) {
            tokio::time::sleep(StdDuration::from_millis(250)).await;
        }
        return Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": refund_receipt(tx_hash),
        }));
    }
    Json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": finalized_block(),
    }))
}

fn refund_receipt(tx_hash: &str) -> Value {
    let scenario = (1_u64..=6)
        .find(|value| tx_hash == format!("{:#x}", B256::from(U256::from(*value))))
        .unwrap_or_default();
    let asset = if scenario == 1 {
        "0x7777777777777777777777777777777777777777"
    } else {
        "0x6c5ba91642f10282b576d91922ae6448c9d52f4e"
    };
    let from = if scenario == 2 {
        "0x3333333333333333333333333333333333333333"
    } else {
        "0x0000000000000000000000000000000000007ea5"
    };
    let to = if scenario == 3 {
        "0x5555555555555555555555555555555555555555"
    } else {
        REFUND_DESTINATION
    };
    let block_number = if scenario == 4 { "0x65" } else { "0x5a" };
    let status = if scenario == 5 { "0x0" } else { "0x1" };
    json!({
        "transactionHash": tx_hash,
        "transactionIndex": "0x0",
        "blockHash": format!("{:#x}", B256::from(U256::from(900_u64))),
        "blockNumber": block_number,
        "from": from,
        "to": to,
        "cumulativeGasUsed": "0x5208",
        "gasUsed": "0x5208",
        "contractAddress": null,
        "logs": [{
            "address": asset,
            "topics": [
                "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
                address_topic(from),
                address_topic(to)
            ],
            "data": format!("0x{:064x}", 100_u64),
            "blockNumber": block_number,
            "transactionHash": tx_hash,
            "transactionIndex": "0x0",
            "blockHash": format!("{:#x}", B256::from(U256::from(900_u64))),
            "logIndex": "0x7",
            "removed": false
        }],
        "logsBloom": format!("0x{}", "00".repeat(256)),
        "status": status,
        "type": "0x2",
        "effectiveGasPrice": "0x1"
    })
}

fn finalized_block() -> Value {
    json!({
        "hash": format!("{:#x}", B256::from(U256::from(100_u64))),
        "parentHash": format!("{:#x}", B256::from(U256::from(99_u64))),
        "sha3Uncles": format!("{:#x}", B256::ZERO),
        "miner": "0x0000000000000000000000000000000000000000",
        "stateRoot": format!("{:#x}", B256::ZERO),
        "transactionsRoot": format!("{:#x}", B256::ZERO),
        "receiptsRoot": format!("{:#x}", B256::ZERO),
        "logsBloom": format!("0x{}", "00".repeat(256)),
        "difficulty": "0x0",
        "number": "0x64",
        "gasLimit": "0x1c9c380",
        "gasUsed": "0x0",
        "timestamp": "0x0",
        "extraData": "0x",
        "mixHash": format!("{:#x}", B256::ZERO),
        "nonce": "0x0000000000000000",
        "transactions": [],
        "uncles": []
    })
}

fn address_topic(address: &str) -> String {
    format!("0x{:0>64}", address.trim_start_matches("0x"))
}

#[async_trait]
impl RefundChainReader for BlockingReader {
    async fn observe(&self, check: &RefundCheck) -> Result<RefundObservation, RefundReadError> {
        assert_eq!(check.tx_hash, self.expected_tx);
        self.started.notify_one();
        self.release.notified().await;
        Ok(RefundObservation::Finalized {
            block_number: 90,
            succeeded: true,
            transfers: vec![RefundTransfer {
                log_index: 8,
                transferred_atomic: U256::from(100_u64),
            }],
        })
    }
}

fn test_worker<R: RefundChainReader>(
    pool: &sqlx::PgPool,
    reader: R,
) -> Result<RefundConfirmationWorker<R>> {
    Ok(RefundConfirmationWorker::new(
        pool.clone(),
        reader,
        &[route_fixture()],
        RefundConfirmationConfig {
            poll_interval: StdDuration::ZERO,
            retry_interval: StdDuration::ZERO,
            request_timeout: StdDuration::from_secs(1),
            observe_timeout: StdDuration::from_secs(1),
        },
    )?)
}

fn test_router(pool: &sqlx::PgPool, admin_key: &SigningKey) -> axum::Router {
    let route = route_fixture();
    let state = AppState {
        pool: pool.clone(),
        routes: Arc::new(vec![route]),
        admin_key: VerificationKey::from_base64(
            ADMIN_KID.to_owned(),
            &public_key_base64(admin_key),
        )
        .expect("admin key is valid"),
        public_origin: PublicOrigin::parse(TEST_ORIGIN).expect("test origin is valid"),
        attestor: Arc::new(UnavailableAttestor),
        rate_lock_quotes: Arc::new(topup::locks::UnavailableQuoteProvider),
    };
    topup::api::router(state).0
}

fn route_fixture() -> RouteFile {
    let route: RouteFile = serde_saphyr::from_str(include_str!("fixtures/phala-cloud-pha.yaml"))
        .expect("route fixture parses");
    route.validate().expect("route fixture validates");
    route
}

async fn seed_product(
    pool: &sqlx::PgPool,
    slug: &str,
    key: &SigningKey,
) -> Result<topup::db::Product> {
    Ok(topup::db::create_product(
        pool,
        &NewProduct {
            id: Uuid::new_v4(),
            slug: slug.to_owned(),
            webhook_url: "https://product.test/webhooks".to_owned(),
            pubkey: public_key_base64(key),
            paused_scopes: Vec::new(),
        },
    )
    .await?)
}

async fn seed_rejected_deposit(
    pool: &sqlx::PgPool,
    product_id: Uuid,
    external_id: &str,
    amount: u64,
) -> Result<Uuid> {
    seed_deposit(
        pool,
        product_id,
        external_id,
        amount,
        DepositState::Rejected,
        Some(RejectReason::OutOfBounds),
    )
    .await
}

async fn seed_deposit(
    pool: &sqlx::PgPool,
    product_id: Uuid,
    external_id: &str,
    amount: u64,
    state: DepositState,
    reason: Option<RejectReason>,
) -> Result<Uuid> {
    let account = topup::db::create_account(
        pool,
        &NewAccount {
            id: Uuid::new_v4(),
            product_id,
            external_id: external_id.to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    let index = Uuid::new_v4().as_u128();
    let address = topup::db::insert_address(
        pool,
        &NewAddress {
            id: Uuid::new_v4(),
            account_id: account.id,
            chain_id: 1,
            kind: AddressKind::Persistent,
            version: 1,
            lock_ref: None,
            salt: B256::from(U256::from(index)),
            address: Address::from_word(B256::from(U256::from(index))),
            retired_at: None,
        },
    )
    .await?;
    let tx_hash = B256::from(U256::from(
        index.checked_add(1).context("test hash overflow")?,
    ));
    topup::db::insert_deposit(
        pool,
        &NewDeposit {
            chain_id: 1,
            tx_hash,
            log_index: 0,
            block_number: 80,
            block_hash: B256::from(U256::from(
                index.checked_add(2).context("test block overflow")?,
            )),
            block_time: Utc::now() - Duration::hours(2),
            address_id: address.id,
            account_id: account.id,
            route: Some("phala-cloud-ethereum-pha-usd".to_owned()),
            route_version: Some(1),
            asset_contract: route_fixture().asset.contract,
            from_address: Address::from_str("0x3333333333333333333333333333333333333333")?,
            amount_atomic: AtomicAmount::new(U256::from(amount)),
            state,
            reason,
            next_attempt_at: Utc::now() + Duration::hours(1),
        },
    )
    .await?;
    Ok(deposit_id(1, tx_hash, 0))
}

async fn insert_transition(pool: &sqlx::PgPool, deposit_id: Uuid) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO transitions (id, deposit_id, from_state, to_state, attempt, evidence)
        VALUES ($1, $2, 'detected', 'rejected', 0, '{"reason":"out_of_bounds"}')
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(deposit_id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn account_id(pool: &sqlx::PgPool, deposit_id: Uuid) -> Result<Uuid> {
    Ok(
        sqlx::query_scalar("SELECT account_id FROM deposits WHERE id = $1")
            .bind(deposit_id)
            .fetch_one(pool)
            .await?,
    )
}

async fn seed_open_lock(pool: &sqlx::PgPool, product_id: Uuid) -> Result<()> {
    seed_lock(pool, product_id, "open-lock", "lock-1", 50, "1 hour").await
}

async fn seed_expired_lock(pool: &sqlx::PgPool, product_id: Uuid) -> Result<()> {
    seed_lock(
        pool,
        product_id,
        "expired-lock",
        "lock-expired",
        70,
        "-1 hour",
    )
    .await
}

async fn seed_lock(
    pool: &sqlx::PgPool,
    product_id: Uuid,
    external_id: &str,
    lock_ref: &str,
    amount: u64,
    expiry: &str,
) -> Result<()> {
    let account = topup::db::create_account(
        pool,
        &NewAccount {
            id: Uuid::new_v4(),
            product_id,
            external_id: external_id.to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    let address_id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO addresses (id, account_id, chain_id, kind, version, lock_ref, salt, address)
        VALUES ($1, $2, 1, 'lock', 1, $3, $4, $5)
        "#,
    )
    .bind(address_id)
    .bind(account.id)
    .bind(lock_ref)
    .bind(format!("0x{:064x}", Uuid::new_v4().as_u128()))
    .bind(format!("0x{:040x}", Uuid::new_v4().as_u128()))
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO rate_locks (
            address_id, route, amount_atomic, price_scaled, credit_minor, expires_at
        )
        VALUES ($1, 'phala-cloud-ethereum-pha-usd', $2::text::numeric, 100000000,
                $2::text::numeric,
                now() + $3::text::interval)
        "#,
    )
    .bind(address_id)
    .bind(amount.to_string())
    .bind(expiry)
    .execute(pool)
    .await?;
    Ok(())
}

async fn seed_approved_refund(pool: &sqlx::PgPool, deposit_id: Uuid, amount: u64) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO refunds (
            id, deposit_id, amount_atomic, to_address, route, status, requested_by, approved_by
        )
        VALUES ($1, $2, $3::text::numeric, $4, 'phala-cloud-ethereum-pha-usd',
                'approved', 'test', 'admin:test')
        "#,
    )
    .bind(id)
    .bind(deposit_id)
    .bind(amount.to_string())
    .bind(REFUND_DESTINATION)
    .execute(pool)
    .await?;
    Ok(id)
}

async fn seed_sent_refund(
    pool: &sqlx::PgPool,
    deposit_id: Uuid,
    amount: u64,
    tx_hash: &str,
) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO refunds (
            id, deposit_id, amount_atomic, to_address, route, tx_hash, status,
            requested_by, approved_by, tx_version, next_check_at
        )
        VALUES ($1, $2, $3::text::numeric, $4, 'phala-cloud-ethereum-pha-usd', $5,
                'sent', 'test', 'admin:test', 1, now())
        "#,
    )
    .bind(id)
    .bind(deposit_id)
    .bind(amount.to_string())
    .bind(REFUND_DESTINATION)
    .bind(tx_hash)
    .execute(pool)
    .await?;
    Ok(id)
}

async fn seed_same_address_deposits(
    pool: &sqlx::PgPool,
    product_id: Uuid,
    count: u64,
) -> Result<String> {
    let account = topup::db::create_account(
        pool,
        &NewAccount {
            id: Uuid::new_v4(),
            product_id,
            external_id: "support-pages".to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    let receiving = Address::from_str("0x5656565656565656565656565656565656565656")?;
    let address = topup::db::insert_address(
        pool,
        &NewAddress {
            id: Uuid::new_v4(),
            account_id: account.id,
            chain_id: 1,
            kind: AddressKind::Persistent,
            version: 1,
            lock_ref: None,
            salt: B256::from(U256::from(99_u64)),
            address: receiving,
            retired_at: None,
        },
    )
    .await?;
    for index in 1..=count {
        topup::db::insert_deposit(
            pool,
            &NewDeposit {
                chain_id: 1,
                tx_hash: B256::from(U256::from(index)),
                log_index: 0,
                block_number: index,
                block_hash: B256::from(U256::from(index.checked_add(1).context("block hash")?)),
                block_time: Utc::now(),
                address_id: address.id,
                account_id: account.id,
                route: Some("phala-cloud-ethereum-pha-usd".to_owned()),
                route_version: Some(1),
                asset_contract: route_fixture().asset.contract,
                from_address: Address::from_str("0x3333333333333333333333333333333333333333")?,
                amount_atomic: AtomicAmount::new(U256::from(100_u64)),
                state: DepositState::Rejected,
                reason: Some(RejectReason::OutOfBounds),
                next_attempt_at: Utc::now(),
            },
        )
        .await?;
    }
    Ok(format!("{receiving:#x}"))
}

async fn seed_refund_row(pool: &sqlx::PgPool, deposit_id: Uuid, amount: u64) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO refunds (
            id, deposit_id, amount_atomic, to_address, route, status, requested_by
        )
        VALUES ($1, $2, $3::text::numeric, $4, 'phala-cloud-ethereum-pha-usd',
                'requested', 'test')
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(deposit_id)
    .bind(amount.to_string())
    .bind(REFUND_DESTINATION)
    .execute(pool)
    .await?;
    Ok(())
}

async fn response_json(response: axum::response::Response) -> Result<Value> {
    let bytes = to_bytes(response.into_body(), 1_048_576).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

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
use topup::api::{AppState, PublicOrigin, VerificationKey};
use topup::db::{Account, NewDeposit};
use topup::refunds::{
    EvmRefundChainReader, RefundChainReader, RefundCheck, RefundConfirmationConfig,
    RefundConfirmationWorker, RefundObservation, RefundReadError, RefundTransfer,
};
use topup_adapters::attestation::DstackAttestor;
use topup_adapters::chain::evm::EvmClient;
use topup_core::deposit::{DepositState, RejectReason};
use topup_core::identity::deposit_id;
use topup_core::money::AtomicAmount;
use topup_core::route::RouteFile;
use tower::ServiceExt;
use uuid::Uuid;

use support::seed::{self, NewAccount, NewAddress, NewCustomer};
use support::{TEST_ORIGIN, TestDatabase, public_key_base64, signed_request};

const ADMIN_KID: &str = "admin/v1";
const REFUND_DESTINATION: &str = "0x4444444444444444444444444444444444444444";
const REFUND_TX: &str = "0xdddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
/// A `POST /v1/refunds` body.
fn refund_body(deposit: Uuid, destination: &str, amount: &str) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&json!({
        "deposit": format!("dep_{}", deposit.simple()),
        "destination_address": destination,
        "amount_atomic": amount,
    }))?)
}

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
        let product_kid = seed::key_id(&product);
        let other = seed_product(&database.app_pool, "builder", &other_key).await?;
        let deposit =
            seed_rejected_deposit(&database.app_pool, product.id, "refund-account", 150).await?;
        let other_deposit =
            seed_rejected_deposit(&database.app_pool, other.id, "other-account", 150).await?;
        insert_transition(&database.app_pool, deposit).await?;
        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();

        let cross_tenant_path = "/v1/refunds";
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                cross_tenant_path,
                refund_body(other_deposit, REFUND_DESTINATION, "100")?,
                &product_kid,
                &product_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::NOT_FOUND);

        seed::set_customer_paused_scopes(
            &database.app_pool,
            customer_id(&database.app_pool, deposit).await?,
            &["refunds".to_owned()],
        )
        .await?;
        let request_path = "/v1/refunds";
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                request_path,
                refund_body(deposit, REFUND_DESTINATION, "100")?,
                &product_kid,
                &product_key,
                now + 1,
            ))
            .await?;
        ensure!(response.status() == StatusCode::CONFLICT);
        seed::set_customer_paused_scopes(
            &database.app_pool,
            customer_id(&database.app_pool, deposit).await?,
            &[],
        )
        .await?;

        seed::set_account_paused_scopes(
            &database.app_pool,
            product.id,
            &["refunds".to_owned()],
        )
        .await?;
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                request_path,
                refund_body(deposit, REFUND_DESTINATION, "100")?,
                &product_kid,
                &product_key,
                now + 2,
            ))
            .await?;
        ensure!(response.status() == StatusCode::CONFLICT);
        seed::set_account_paused_scopes(&database.app_pool, product.id, &[]).await?;

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
                request_path,
                refund_body(deposit, REFUND_DESTINATION, "100")?,
                &product_kid,
                &product_key,
                now + 3,
            ))
            .await?;
        ensure!(response.status() == StatusCode::CONFLICT);
        sqlx::query("UPDATE route_pauses SET paused_scopes = '{}' WHERE route = $1")
            .bind("phala-cloud-ethereum-pha-usd")
            .execute(&database.app_pool)
            .await?;

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                request_path,
                refund_body(deposit, REFUND_DESTINATION, "100")?,
                &product_kid,
                &product_key,
                now + 4,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let requested = response_json(response).await?;
        let refund_id = topup::ids::parse(topup::ids::REFUND, requested["id"].as_str().context("refund id")?)
            .context("re_ id")?;
        ensure!(requested["status"] == "pending" && requested["object"] == "refund");
        ensure!(requested["deposit"] == format!("dep_{}", deposit.simple()));
        ensure!(requested["amount_atomic"] == "100");

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                request_path,
                refund_body(deposit, REFUND_DESTINATION, "100")?,
                &product_kid,
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
                request_path,
                refund_body(deposit, "0x6666666666666666666666666666666666666666", "60")?,
                &product_kid,
                &product_key,
                now + 6,
            ))
            .await?;
        ensure!(response.status() == StatusCode::BAD_REQUEST);
        let error = response_json(response).await?;
        ensure!(error["error"]["code"] == "amount_too_large", "{error}");
        ensure!(error["error"]["param"] == "amount_atomic", "{error}");

        // A malformed id is an unknown refund; a path that is not UTF-8 names the parameter.
        for (path, status, code, created) in [
            (
                format!("/v1/admin/refunds/re_{refund_id}/approve"),
                StatusCode::NOT_FOUND,
                "resource_missing",
                now + 20,
            ),
            (
                "/v1/admin/refunds/re_%FF/approve".to_owned(),
                StatusCode::BAD_REQUEST,
                "parameter_invalid",
                now + 21,
            ),
        ] {
            let response = app
                .clone()
                .oneshot(signed_request(
                    Method::POST,
                    &path,
                    Vec::new(),
                    ADMIN_KID,
                    &admin_key,
                    created,
                ))
                .await?;
            ensure!(response.status() == status, "{path}");
            let error = response_json(response).await?;
            ensure!(error["error"]["type"] == "invalid_request_error", "{error}");
            ensure!(error["error"]["code"] == code, "{error}");
            if status == StatusCode::BAD_REQUEST {
                ensure!(error["error"]["param"] == "id", "{error}");
            }
        }

        // Operators take the `re_` id from the product API; the bare UUID of older logs also works.
        let approve_path = format!(
            "/v1/admin/refunds/{}/approve",
            requested["id"].as_str().context("refund id")?
        );
        seed::set_customer_paused_scopes(
            &database.app_pool,
            customer_id(&database.app_pool, deposit).await?,
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
        ensure!(response.status() == StatusCode::CONFLICT);
        seed::set_customer_paused_scopes(
            &database.app_pool,
            customer_id(&database.app_pool, deposit).await?,
            &[],
        )
        .await?;

        seed::set_account_paused_scopes(
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
        ensure!(response.status() == StatusCode::CONFLICT);
        seed::set_account_paused_scopes(&database.app_pool, product.id, &[]).await?;

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
        ensure!(response.status() == StatusCode::CONFLICT);
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
        let approved = response_json(response).await?;
        ensure!(approved["status"] == "approved" && approved["id"] == requested["id"], "{approved}");

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
        let recorded = response_json(response).await?;
        ensure!(recorded["status"] == "sent" && recorded["id"] == requested["id"], "{recorded}");

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
        let event = sqlx::query(
            "SELECT id, account_id, object_id FROM events WHERE type = 'deposit.refunded'",
        )
        .fetch_one(&database.app_pool)
        .await?;
        ensure!(
            event.try_get::<Uuid, _>("id")?
                == topup_core::identity::event_id("deposit.refunded", refund_id)
        );
        ensure!(event.try_get::<Uuid, _>("account_id")? == product.id);
        ensure!(event.try_get::<Option<Uuid>, _>("object_id")? == Some(deposit));

        let tx_hash: String = sqlx::query_scalar("SELECT tx_hash FROM deposits WHERE id = $1")
            .bind(deposit)
            .fetch_one(&database.app_pool)
            .await?;
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::GET,
                &format!("/v1/deposits?tx_hash={tx_hash}"),
                Vec::new(),
                &product_kid,
                &product_key,
                now + 6,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let lookup = response_json(response).await?;
        ensure!(lookup["object"] == "list" && lookup["has_more"] == false);
        let found = &lookup["data"][0];
        ensure!(found["id"] == format!("dep_{}", deposit.simple()));
        ensure!(found["account_id"] == "refund-account");
        ensure!(found["amount_refunded_atomic"] == "100" && found["refunded"] == false);
        let refund = app
            .clone()
            .oneshot(signed_request(
                Method::GET,
                &format!("/v1/refunds/re_{}?expand[]=deposit", refund_id.simple()),
                Vec::new(),
                &product_kid,
                &product_key,
                now + 7,
            ))
            .await?;
        ensure!(refund.status() == StatusCode::OK);
        let refund = response_json(refund).await?;
        ensure!(refund["status"] == "succeeded", "{refund}");
        ensure!(refund["deposit"]["id"] == format!("dep_{}", deposit.simple()), "{refund}");
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::GET,
                &format!("/v1/admin/deposits/dep_{}", deposit.simple()),
                Vec::new(),
                ADMIN_KID,
                &admin_key,
                now + 8,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let detail = response_json(response).await?;
        ensure!(detail["id"] == format!("dep_{}", deposit.simple()), "{detail}");
        ensure!(detail["external_id"] == "refund-account");
        ensure!(detail["timeline"][0]["to_state"] == "rejected");
        let response = app
            .oneshot(signed_request(
                Method::GET,
                &format!("/v1/admin/deposits/{deposit}"),
                Vec::new(),
                ADMIN_KID,
                &admin_key,
                now + 9,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        ensure!(response_json(response).await?["id"] == detail["id"]);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn refund_request_requires_a_final_outcome_and_approval_rechecks_current_state() -> Result<()>
{
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[61; 32]);
        let admin_key = SigningKey::from_bytes(&[62; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        let product_kid = seed::key_id(&product);
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

        let pending_path = "/v1/refunds";
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                pending_path,
                refund_body(pending, REFUND_DESTINATION, "100")?,
                &product_kid,
                &product_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::CONFLICT);

        let request_path = "/v1/refunds";
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                request_path,
                refund_body(refundable, REFUND_DESTINATION, "100")?,
                &product_kid,
                &product_key,
                now + 1,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let requested = response_json(response).await?;
        let refund_id = topup::ids::parse(
            topup::ids::REFUND,
            requested["id"].as_str().context("refund id")?,
        )
        .context("re_ id")?;

        sqlx::query("UPDATE deposits SET state = 'confirmed', reason = NULL WHERE id = $1")
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

        let app_for_credited = app.clone();
        let sanctioned_path = "/v1/refunds";
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                sanctioned_path,
                refund_body(sanctioned, REFUND_DESTINATION, "100")?,
                &product_kid,
                &product_key,
                now + 3,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let requested = response_json(response).await?;
        let sanctioned_refund = topup::ids::parse(
            topup::ids::REFUND,
            requested["id"].as_str().context("refund id")?,
        )
        .context("re_ id")?;
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

        // The product asks to refund a credit it did not apply (for example a closed
        // workspace); finance still approves.
        let credited = seed_deposit(
            &database.app_pool,
            product.id,
            "credited-refund",
            100,
            DepositState::Credited,
            None,
        )
        .await?;
        let credited_path = "/v1/refunds";
        let response = app_for_credited
            .oneshot(signed_request(
                Method::POST,
                credited_path,
                refund_body(credited, REFUND_DESTINATION, "100")?,
                &product_kid,
                &product_key,
                now + 5,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        ensure!(response_json(response).await?["status"] == "pending");
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn a_deposit_that_could_still_be_reversed_is_not_refunded() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[63; 32]);
        let admin_key = SigningKey::from_bytes(&[64; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        let product_kid = seed::key_id(&product);
        let deposit =
            seed_rejected_deposit(&database.app_pool, product.id, "not-final-yet", 100).await?;
        sqlx::query("UPDATE deposits SET final_at = NULL WHERE id = $1")
            .bind(deposit)
            .execute(&database.app_pool)
            .await?;
        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                "/v1/refunds",
                refund_body(deposit, REFUND_DESTINATION, "100")?,
                &product_kid,
                &product_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::CONFLICT);
        ensure!(response_json(response).await?["error"]["code"] == "deposit_not_final");

        sqlx::query("UPDATE deposits SET final_at = now() WHERE id = $1")
            .bind(deposit)
            .execute(&database.app_pool)
            .await?;
        let response = app
            .oneshot(signed_request(
                Method::POST,
                "/v1/refunds",
                refund_body(deposit, REFUND_DESTINATION, "100")?,
                &product_kid,
                &product_key,
                now + 1,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
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
        let product_kid = seed::key_id(&product);
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
        let request_path = "/v1/refunds";
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                request_path,
                refund_body(deposit, REFUND_DESTINATION, "100")?,
                &product_kid,
                &product_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let requested = response_json(response).await?;
        let refund_id = topup::ids::parse(
            topup::ids::REFUND,
            requested["id"].as_str().context("refund id")?,
        )
        .context("re_ id")?;
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
        ensure!(response.status() == StatusCode::CONFLICT);
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
            sqlx::query_scalar("SELECT count(*) FROM events WHERE type = 'deposit.refunded'")
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
async fn refund_idempotency_keys_replay_and_refuse_other_parameters() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[71; 32]);
        let admin_key = SigningKey::from_bytes(&[72; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        let product_kid = seed::key_id(&product);
        let deposit = seed_rejected_deposit(&database.app_pool, product.id, "keyed", 150).await?;
        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();
        let request = |body: Vec<u8>, created: i64| {
            support::signed_request_with_key(
                Method::POST,
                "/v1/refunds",
                body,
                &product_kid,
                &product_key,
                created,
                "\"refund-1\"",
            )
        };
        // Without an amount the whole unrefunded remainder is requested.
        let body = serde_json::to_vec(&json!({
            "deposit": format!("dep_{}", deposit.simple()),
            "destination_address": REFUND_DESTINATION,
        }))?;
        let first = app.clone().oneshot(request(body.clone(), now)).await?;
        ensure!(first.status() == StatusCode::OK);
        let first = response_json(first).await?;
        ensure!(first["amount_atomic"] == "150", "{first}");
        let repeat = app.clone().oneshot(request(body, now + 1)).await?;
        ensure!(response_json(repeat).await?["id"] == first["id"]);
        let other = app
            .clone()
            .oneshot(request(
                refund_body(deposit, "0x6666666666666666666666666666666666666666", "150")?,
                now + 2,
            ))
            .await?;
        ensure!(other.status() == StatusCode::CONFLICT);
        ensure!(response_json(other).await?["error"]["type"] == "idempotency_error");
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM refunds")
            .fetch_one(&database.app_pool)
            .await?;
        ensure!(count == 1);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn deposit_lists_page_with_stripe_cursors() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[47; 32]);
        let admin_key = SigningKey::from_bytes(&[48; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        let product_kid = seed::key_id(&product);
        seed_same_address_deposits(&database.app_pool, product.id, 52).await?;
        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();
        let created = std::sync::atomic::AtomicI64::new(now);
        let list = |query: String, kid: &str, key: &SigningKey| {
            let app = app.clone();
            let request = signed_request(
                Method::GET,
                &format!("/v1/deposits{query}"),
                Vec::new(),
                kid,
                key,
                created.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            );
            async move {
                let response = app.oneshot(request).await?;
                let status = response.status();
                anyhow::Ok((status, response_json(response).await?))
            }
        };
        let ids = |page: &Value| -> Result<Vec<String>> {
            page["data"]
                .as_array()
                .with_context(|| format!("no data in {page}"))?
                .iter()
                .map(|deposit| deposit["id"].as_str().map(str::to_owned).context("id"))
                .collect()
        };

        let (status, first) = list("?limit=50".to_owned(), &product_kid, &product_key).await?;
        ensure!(status == StatusCode::OK, "{first}");
        ensure!(first["object"] == "list" && first["url"] == "/v1/deposits");
        ensure!(first["has_more"] == true);
        let first_ids = ids(&first)?;
        ensure!(first_ids.len() == 50);
        let last = first_ids.last().context("last")?;
        let (_, second) = list(
            format!("?limit=50&starting_after={last}"),
            &product_kid,
            &product_key,
        )
        .await?;
        let second_ids = ids(&second)?;
        ensure!(second_ids.len() == 2 && second["has_more"] == false);
        ensure!(second_ids.iter().all(|id| !first_ids.contains(id)));
        // Paging back from the second page returns the end of the first, in the same order.
        let (_, back) = list(
            format!("?limit=3&ending_before={}", second_ids[0]),
            &product_kid,
            &product_key,
        )
        .await?;
        ensure!(ids(&back)? == first_ids[47..]);
        ensure!(back["has_more"] == true);
        let (_, default) = list(String::new(), &product_kid, &product_key).await?;
        ensure!(ids(&default)?.len() == 10);

        for (query, param) in [
            ("?limit=101", "limit"),
            ("?status=paid", "status"),
            ("?expand[]=quote", "expand"),
            ("?color=red", "color"),
        ] {
            let (status, error) = list(query.to_owned(), &product_kid, &product_key).await?;
            ensure!(status == StatusCode::BAD_REQUEST, "{query}");
            ensure!(error["error"]["param"] == param, "{query}: {error}");
        }
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
    let reader = EvmRefundChainReader::new(BTreeMap::from([(
        1,
        Arc::new(EvmClient::with_timeout(
            &format!("http://{address}"),
            StdDuration::from_millis(50),
        )?),
    )]));
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
                account_id: Uuid::new_v4(),
                livemode: true,
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
            account_id: Uuid::new_v4(),
            livemode: true,
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
        // The credited deposit's fulfillment event is still waiting for the account's endpoint.
        sqlx::query(
            r#"
            WITH event AS (
                INSERT INTO events (id, account_id, livemode, type, object_type, object_id,
                                    created)
                VALUES ($1, $2, true, 'deposit.credited', 'deposit', $3, now() - interval '1 hour')
                RETURNING id, account_id
            )
            INSERT INTO webhook_deliveries (event_id, endpoint_id, next_attempt_at)
            SELECT event.id, endpoint.id, now()
            FROM event
            JOIN webhook_endpoints AS endpoint ON endpoint.account_id = event.account_id
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(product.id)
        .bind(credited)
        .execute(&database.app_pool)
        .await?;
        seed_open_lock(&database.app_pool, product.id).await?;
        seed_expired_lock(&database.app_pool, product.id).await?;
        seed_refund_row(&database.app_pool, rejected, 20).await?;
        // The report sums the open reserved locks, as lock creation would have reserved them.
        sqlx::query("UPDATE quotes SET exposure_reserved = true WHERE expires_at > now()")
            .execute(&database.app_pool)
            .await?;

        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();
        // The `dep_` id and the bare UUID of older logs both name the deposit.
        for (nudge_path, created) in [
            (format!("/v1/admin/deposits/dep_{}/nudge", rejected.simple()), now - 1),
            (format!("/v1/admin/deposits/{rejected}/nudge"), now),
        ] {
            let response = app
                .clone()
                .oneshot(signed_request(
                    Method::POST,
                    &nudge_path,
                    Vec::new(),
                    ADMIN_KID,
                    &admin_key,
                    created,
                ))
                .await?;
            ensure!(response.status() == StatusCode::OK);
            let nudged = response_json(response).await?;
            ensure!(nudged["deposit_id"] == format!("dep_{}", rejected.simple()), "{nudged}");
        }
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
        ensure!(audit_count == 2);

        // Production has no logs: the report says why the route's last planning run stopped.
        topup::observability::record_flush_planning(
            "phala-cloud-ethereum-pha-usd",
            topup::observability::FlushPlanningOutcome::Failed,
            Some("chain operation failed: rate limit exceeded".to_owned()),
        );
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
        ensure!(route["credited_undelivered"] == 1);
        ensure!(
            route["credited_undelivered_max_age_seconds"]
                .as_u64()
                .context("undelivered age")?
                >= 3_600
        );
        ensure!(route["refunds_by_status"]["requested"] == 1);
        ensure!(route["age_in_state_max_seconds"]["credited"].as_u64().context("credited age")? >= 7_000);
        ensure!(route["flush_planning"]["outcome"] == "failed");
        ensure!(route["flush_planning"]["error"] == "chain operation failed: rate limit exceeded");
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
        ensure!(unrouted["flush_planning"].is_null());
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
            observe_timeout: StdDuration::from_secs(1),
        },
    )?)
}

fn test_router(pool: &sqlx::PgPool, admin_key: &SigningKey) -> axum::Router {
    let route = route_fixture();
    let state = AppState {
        pool: pool.clone(),
        routes: Arc::new(topup::routes::RouteSet::new(vec![route]).expect("route loads")),
        admin_key: VerificationKey::from_base64(
            ADMIN_KID.to_owned(),
            &public_key_base64(admin_key),
        )
        .expect("admin key is valid"),
        public_origin: PublicOrigin::parse(TEST_ORIGIN).expect("test origin is valid"),
        attestor: Arc::new(DstackAttestor::new()),
        rate_lock_quotes: Arc::new(topup::locks::UnavailableQuoteProvider),
        client_reads: Arc::default(),
    };
    topup::api::router(state).0
}

fn route_fixture() -> RouteFile {
    let route: RouteFile = serde_saphyr::from_str(include_str!("fixtures/phala-cloud-pha.yaml"))
        .expect("route fixture parses");
    route.validate().expect("route fixture validates");
    route
}

/// A live account signing with `key`, with a webhook endpoint.
async fn seed_product(pool: &sqlx::PgPool, name: &str, key: &SigningKey) -> Result<Account> {
    Ok(seed::create_account(
        pool,
        &NewAccount {
            public_key: public_key_base64(key),
            webhook_url: "https://product.test/webhooks".to_owned(),
            ..NewAccount::named(name)
        },
    )
    .await?)
}

async fn seed_customer(
    pool: &sqlx::PgPool,
    account_id: Uuid,
    client_reference_id: &str,
) -> Result<topup::db::Customer> {
    Ok(seed::create_customer(
        pool,
        &NewCustomer {
            id: Uuid::new_v4(),
            account_id,
            livemode: true,
            client_reference_id: client_reference_id.to_owned(),
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
    let customer = seed_customer(pool, product_id, external_id).await?;
    let index = Uuid::new_v4().as_u128();
    let address = seed::insert_address(
        pool,
        &NewAddress {
            id: Uuid::new_v4(),
            customer_id: customer.id,
            chain_id: 1,
            route: "phala-cloud-ethereum-pha-usd".to_owned(),
            salt: B256::from(U256::from(index)),
            address: Address::from_word(B256::from(U256::from(index))),
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
            receipt_log_index: 0,
            tx_from: alloy_primitives::Address::ZERO,
            tx_nonce: 0,
            is_final: true,
            block_number: 80,
            block_hash: B256::from(U256::from(
                index.checked_add(2).context("test block overflow")?,
            )),
            block_time: Utc::now() - Duration::hours(2),
            address_id: address.id,
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

async fn customer_id(pool: &sqlx::PgPool, deposit_id: Uuid) -> Result<Uuid> {
    Ok(
        sqlx::query_scalar("SELECT customer_id FROM deposits WHERE id = $1")
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
    let customer = seed_customer(pool, product_id, external_id).await?;
    let address = seed::insert_address(
        pool,
        &NewAddress {
            id: Uuid::new_v4(),
            customer_id: customer.id,
            chain_id: 1,
            route: "phala-cloud-ethereum-pha-usd".to_owned(),
            salt: B256::from(U256::from(Uuid::new_v4().as_u128())),
            address: Address::from_word(B256::from(U256::from(Uuid::new_v4().as_u128()))),
        },
    )
    .await?;
    sqlx::query(
        r#"
        UPDATE quotes
        SET amount_atomic = $2::text::numeric, price_scaled = 100000000,
            credit_minor = $2::text::numeric, expires_at = now() + $3::text::interval,
            status = 'open', closed_at = NULL, idempotency_key = $4
        WHERE id = $1
        "#,
    )
    .bind(address.quote_id)
    .bind(amount.to_string())
    .bind(expiry)
    .bind(lock_ref)
    .execute(pool)
    .await?;
    Ok(())
}

async fn seed_approved_refund(pool: &sqlx::PgPool, deposit_id: Uuid, amount: u64) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO refunds (
            id, account_id, livemode, deposit_id, amount_atomic, to_address, route, status,
            requested_by, approved_by
        )
        SELECT $1, account_id, livemode, id, $3::text::numeric, $4,
               'phala-cloud-ethereum-pha-usd', 'approved', 'test', 'admin:test'
        FROM deposits WHERE id = $2
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
            id, account_id, livemode, deposit_id, amount_atomic, to_address, route, tx_hash,
            status, requested_by, approved_by, tx_version, next_check_at
        )
        SELECT $1, account_id, livemode, id, $3::text::numeric, $4,
               'phala-cloud-ethereum-pha-usd', $5, 'sent', 'test', 'admin:test', 1, now()
        FROM deposits WHERE id = $2
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
    let customer = seed_customer(pool, product_id, "support-pages").await?;
    let receiving = Address::from_str("0x5656565656565656565656565656565656565656")?;
    let address = seed::insert_address(
        pool,
        &NewAddress {
            id: Uuid::new_v4(),
            customer_id: customer.id,
            chain_id: 1,
            route: "phala-cloud-ethereum-pha-usd".to_owned(),
            salt: B256::from(U256::from(99_u64)),
            address: receiving,
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
                receipt_log_index: 0,
                tx_from: alloy_primitives::Address::ZERO,
                tx_nonce: 0,
                is_final: true,
                block_number: index,
                block_hash: B256::from(U256::from(index.checked_add(1).context("block hash")?)),
                block_time: Utc::now(),
                address_id: address.id,
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
            id, account_id, livemode, deposit_id, amount_atomic, to_address, route, status,
            requested_by
        )
        SELECT $1, account_id, livemode, id, $3::text::numeric, $4,
               'phala-cloud-ethereum-pha-usd', 'requested', 'test'
        FROM deposits WHERE id = $2
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

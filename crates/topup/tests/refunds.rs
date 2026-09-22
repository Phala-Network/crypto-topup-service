//! C12 refund and support integration tests.

mod support;

use std::collections::VecDeque;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use axum::body::to_bytes;
use axum::http::{Method, StatusCode};
use chrono::{Duration, Utc};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use sqlx::Row;
use topup::api::{AppState, UnavailableAttestor, VerificationKey};
use topup::db::{AddressKind, NewAccount, NewAddress, NewDeposit, NewProduct};
use topup::refunds::{
    RefundChainReader, RefundCheck, RefundConfirmationConfig, RefundConfirmationWorker,
    RefundObservation, RefundReadError,
};
use topup_core::deposit::{DepositState, RejectReason};
use topup_core::identity::deposit_id;
use topup_core::money::AtomicAmount;
use topup_core::route::RouteFile;
use tower::ServiceExt;
use uuid::Uuid;

use support::{TestDatabase, public_key_base64, signed_request};

const PRODUCT_KID: &str = "phala-cloud/v1";
const OTHER_KID: &str = "builder/v1";
const ADMIN_KID: &str = "admin/v1";
const REFUND_DESTINATION: &str = "0x4444444444444444444444444444444444444444";
const REFUND_TX: &str = "0xdddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

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
            seed_product(&database.app_pool, "phala-cloud", PRODUCT_KID, &product_key).await?;
        let other = seed_product(&database.app_pool, "builder", OTHER_KID, &other_key).await?;
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
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &approve_path,
                Vec::new(),
                ADMIN_KID,
                &admin_key,
                now,
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
                now + 1,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        ensure!(response_json(response).await?["status"] == "sent");

        let route = route_fixture();
        let reader = ScriptedReader::new(vec![
            RefundObservation::Finalized {
                block_number: 90,
                succeeded: true,
                transferred_atomic: U256::from(99_u64),
            },
            RefundObservation::Finalized {
                block_number: 90,
                succeeded: true,
                transferred_atomic: U256::from(100_u64),
            },
        ]);
        let worker = RefundConfirmationWorker::new(
            database.app_pool.clone(),
            reader,
            &[route],
            RefundConfirmationConfig {
                poll_interval: StdDuration::ZERO,
                retry_interval: StdDuration::ZERO,
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
async fn admin_nudge_and_daily_report_use_seeded_integer_facts() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[51; 32]);
        let admin_key = SigningKey::from_bytes(&[52; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", PRODUCT_KID, &product_key).await?;
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
        seed_refund_row(&database.app_pool, rejected, 20).await?;

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
        let routes = report["routes"].as_array().context("report routes")?;
        let route = routes
            .iter()
            .find(|route| route["route"] == "phala-cloud-ethereum-pha-usd")
            .context("configured route report")?;
        ensure!(route["route"] == "phala-cloud-ethereum-pha-usd");
        ensure!(route["treasury_balance_atomic"].is_null());
        ensure!(route["treasury_balance_note"].as_str().context("treasury note")?.contains("C7"));
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
    observations: Mutex<VecDeque<RefundObservation>>,
}

impl ScriptedReader {
    fn new(observations: Vec<RefundObservation>) -> Self {
        Self {
            observations: Mutex::new(observations.into()),
        }
    }
}

#[async_trait]
impl RefundChainReader for ScriptedReader {
    async fn observe(&self, check: &RefundCheck) -> Result<RefundObservation, RefundReadError> {
        assert_eq!(
            check.tx_hash,
            B256::from_str(REFUND_TX).expect("valid refund tx")
        );
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
        attestor: Arc::new(UnavailableAttestor),
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
    kid: &str,
    key: &SigningKey,
) -> Result<topup::db::Product> {
    Ok(topup::db::create_product(
        pool,
        &NewProduct {
            id: Uuid::new_v4(),
            slug: slug.to_owned(),
            settlement_url: "https://product.test/settlements".to_owned(),
            webhook_url: "https://product.test/webhooks".to_owned(),
            pubkey: public_key_base64(key),
            kid: kid.to_owned(),
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
    let account = topup::db::create_account(
        pool,
        &NewAccount {
            id: Uuid::new_v4(),
            product_id,
            external_id: "open-lock".to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    let address_id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO addresses (id, account_id, chain_id, kind, version, lock_ref, salt, address)
        VALUES ($1, $2, 1, 'lock', 1, 'lock-1', $3, $4)
        "#,
    )
    .bind(address_id)
    .bind(account.id)
    .bind("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    .bind("0x5555555555555555555555555555555555555555")
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        INSERT INTO rate_locks (address_id, route, amount_atomic, price_scaled, expires_at)
        VALUES ($1, 'phala-cloud-ethereum-pha-usd', 50, 100000000, now() + interval '1 hour')
        "#,
    )
    .bind(address_id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn seed_refund_row(pool: &sqlx::PgPool, deposit_id: Uuid, amount: u64) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO refunds (id, deposit_id, amount_atomic, to_address, status, requested_by)
        VALUES ($1, $2, $3::text::numeric, $4, 'requested', 'test')
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

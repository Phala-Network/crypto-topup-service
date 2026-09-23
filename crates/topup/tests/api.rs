//! In-process C9 API tests backed by PostgreSQL.

mod support;

use std::str::FromStr;
use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use axum::body::{Body, to_bytes};
use axum::http::{Method, StatusCode};
use chrono::{Duration, Utc};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use sqlx::Row;
#[cfg(feature = "dev-signer")]
use topup::api::models::AttestationResponse;
use topup::api::{AppState, Attestor, UnavailableAttestor, VerificationKey};
#[cfg(feature = "dev-signer")]
use topup::api::{AttestationError, AttestationFuture};
use topup::db::{AddressKind, NewAccount, NewAddress, NewDeposit, NewProduct};
#[cfg(feature = "dev-signer")]
use topup_adapters::attestation::report_data;
#[cfg(feature = "dev-signer")]
use topup_adapters::signer::DevSigner;
use topup_core::deposit::DepositState;
use topup_core::identity::deposit_id;
use topup_core::money::AtomicAmount;
use topup_core::route::RouteFile;
#[cfg(feature = "dev-signer")]
use topup_core::{SETTLEMENT_KEY_DOMAIN, SecretKey32, Signer as _};
use tower::ServiceExt;
use uuid::Uuid;

use support::{
    SignatureOptions, SignatureParameter, TestDatabase, public_key_base64, signed_request,
    signed_request_with_options,
};

const PRODUCT_KID: &str = "phala-cloud/v1";
const ADMIN_KID: &str = "admin/v1";

#[tokio::test]
async fn signature_verification_vectors() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[7; 32]);
        let admin_key = SigningKey::from_bytes(&[9; 32]);
        let product =
            seed_product(&database.app_pool, "phala-cloud", PRODUCT_KID, &product_key).await?;
        let app = test_router(&database.app_pool, &admin_key);
        let path = format!("/v1/products/{}/accounts", product.slug);
        let body = serde_json::to_vec(&json!({"external_id": "signed-account"}))?;
        let now = Utc::now().timestamp();

        let response = app
            .clone()
            .oneshot(signed_request_with_options(
                Method::POST,
                &path,
                body.clone(),
                PRODUCT_KID,
                &product_key,
                now,
                &SignatureOptions {
                    parameters: vec![SignatureParameter::Created, SignatureParameter::KeyId],
                    ..SignatureOptions::default()
                },
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);

        let response = app
            .clone()
            .oneshot(signed_request_with_options(
                Method::POST,
                &path,
                serde_json::to_vec(&json!({"external_id": "with-idempotency"}))?,
                PRODUCT_KID,
                &product_key,
                now,
                &SignatureOptions {
                    idempotency_key: Some("\"deposit:test\"".to_owned()),
                    ..SignatureOptions::default()
                },
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &path,
                serde_json::to_vec(&json!({"external_id": "with-alg"}))?,
                PRODUCT_KID,
                &product_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);

        let response = app
            .clone()
            .oneshot(signed_request_with_options(
                Method::POST,
                &path,
                serde_json::to_vec(&json!({"external_id": "reordered-parameters"}))?,
                PRODUCT_KID,
                &product_key,
                now,
                &SignatureOptions {
                    parameters: vec![
                        SignatureParameter::Algorithm("ed25519".to_owned()),
                        SignatureParameter::KeyId,
                        SignatureParameter::Created,
                    ],
                    ..SignatureOptions::default()
                },
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);

        let response = app
            .clone()
            .oneshot(signed_request_with_options(
                Method::POST,
                &path,
                serde_json::to_vec(&json!({"external_id": "different-label"}))?,
                PRODUCT_KID,
                &product_key,
                now,
                &SignatureOptions {
                    label: "checkout".to_owned(),
                    ..SignatureOptions::default()
                },
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);

        let origin_path = format!("{path}?source=checkout");
        let response = app
            .clone()
            .oneshot(signed_request_with_options(
                Method::POST,
                &origin_path,
                serde_json::to_vec(&json!({"external_id": "origin-form-query"}))?,
                PRODUCT_KID,
                &product_key,
                now,
                &SignatureOptions {
                    origin_form: true,
                    ..SignatureOptions::default()
                },
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);

        let response = app
            .clone()
            .oneshot(signed_request_with_options(
                Method::POST,
                &path,
                serde_json::to_vec(&json!({"external_id": "wrong-algorithm"}))?,
                PRODUCT_KID,
                &product_key,
                now,
                &SignatureOptions {
                    parameters: vec![
                        SignatureParameter::Created,
                        SignatureParameter::KeyId,
                        SignatureParameter::Algorithm("rsa-pss-sha512".to_owned()),
                    ],
                    ..SignatureOptions::default()
                },
            ))
            .await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);

        let wrong_key = SigningKey::from_bytes(&[8; 32]);
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &path,
                body.clone(),
                PRODUCT_KID,
                &wrong_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &path,
                serde_json::to_vec(&json!({"external_id": "future-created"}))?,
                PRODUCT_KID,
                &product_key,
                (Utc::now() + Duration::minutes(6)).timestamp(),
            ))
            .await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);

        let mut tampered = signed_request(
            Method::POST,
            &path,
            body.clone(),
            PRODUCT_KID,
            &product_key,
            now,
        );
        *tampered.body_mut() = Body::from(r#"{"external_id":"tampered"}"#);
        let response = app.clone().oneshot(tampered).await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &path,
                body.clone(),
                PRODUCT_KID,
                &product_key,
                (Utc::now() - Duration::minutes(6)).timestamp(),
            ))
            .await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);

        let mut missing_component = signed_request(
            Method::POST,
            &path,
            serde_json::to_vec(&json!({"external_id": "missing-component"}))?,
            PRODUCT_KID,
            &product_key,
            now,
        );
        missing_component.headers_mut().insert(
            "signature-input",
            format!(
                "sig1=(\"@method\" \"@target-uri\");created={now};keyid=\"{PRODUCT_KID}\";alg=\"ed25519\""
            )
            .parse()?,
        );
        let response = app.clone().oneshot(missing_component).await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);

        let replay_body = serde_json::to_vec(&json!({"external_id": "replay"}))?;
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &path,
                replay_body.clone(),
                PRODUCT_KID,
                &product_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let response = app
            .oneshot(signed_request(
                Method::POST,
                &path,
                replay_body,
                PRODUCT_KID,
                &product_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::CONFLICT);
        ensure!(response_json(response).await?["error"]["code"] == "signature_replayed");
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn account_address_rotation_tenant_and_pause_routes() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[17; 32]);
        let other_key = SigningKey::from_bytes(&[18; 32]);
        let admin_key = SigningKey::from_bytes(&[19; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", PRODUCT_KID, &product_key).await?;
        let other = seed_product(&database.app_pool, "builder", "builder/v1", &other_key).await?;
        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();

        let register_path = format!("/v1/products/{}/accounts", product.slug);
        let register_body = serde_json::to_vec(&json!({"external_id": "account-001"}))?;
        let first = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &register_path,
                register_body.clone(),
                PRODUCT_KID,
                &product_key,
                now,
            ))
            .await?;
        ensure!(first.status() == StatusCode::OK);
        let account: Value = response_json(first).await?;

        let second = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &register_path,
                register_body,
                PRODUCT_KID,
                &product_key,
                now + 1,
            ))
            .await?;
        ensure!(second.status() == StatusCode::OK);
        ensure!(response_json(second).await?["id"] == account["id"]);

        let address_path = format!(
            "/v1/products/{}/accounts/account-001/deposit-address",
            product.slug
        );
        let first_address = app
            .clone()
            .oneshot(signed_request(Method::POST, &address_path, Vec::new(), PRODUCT_KID, &product_key, now))
            .await?;
        ensure!(first_address.status() == StatusCode::OK);
        let first_address = response_json(first_address).await?;
        ensure!(first_address["address"] == "0x382ca64bfc7332eef90547e1a345779fa26590c2");
        ensure!(first_address["salt_inputs"]["version"] == 1);

        let same_address = app
            .clone()
            .oneshot(signed_request(Method::GET, &address_path, Vec::new(), PRODUCT_KID, &product_key, now))
            .await?;
        ensure!(same_address.status() == StatusCode::OK);
        ensure!(response_json(same_address).await?["address"] == first_address["address"]);

        let rotate_path = format!("{address_path}/rotate");
        let rotate_body = serde_json::to_vec(&json!({"from_version": 1}))?;
        let rotated = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &rotate_path,
                rotate_body.clone(),
                PRODUCT_KID,
                &product_key,
                now,
            ))
            .await?;
        ensure!(rotated.status() == StatusCode::OK);
        let rotated = response_json(rotated).await?;
        ensure!(rotated["address"] != first_address["address"]);
        ensure!(rotated["salt_inputs"]["version"] == 2);
        let retried = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &rotate_path,
                rotate_body,
                PRODUCT_KID,
                &product_key,
                now + 1,
            ))
            .await?;
        ensure!(retried.status() == StatusCode::OK);
        ensure!(response_json(retried).await?["address"] == rotated["address"]);
        let address_counts = sqlx::query(
            "SELECT count(*) AS total, count(*) FILTER (WHERE retired_at IS NOT NULL) AS retired FROM addresses WHERE account_id = $1",
        )
        .bind(Uuid::parse_str(account["id"].as_str().context("account id string")?)?)
        .fetch_one(&database.app_pool)
        .await?;
        ensure!(address_counts.try_get::<i64, _>("total")? == 2);
        ensure!(address_counts.try_get::<i64, _>("retired")? == 1);

        let other_deposit = seed_other_tenant_deposit(&database.app_pool, other.id).await?;
        let cross_tenant_path = format!("/v1/products/{}/deposits/{other_deposit}", product.slug);
        let response = app
            .clone()
            .oneshot(signed_request(Method::GET, &cross_tenant_path, Vec::new(), PRODUCT_KID, &product_key, now))
            .await?;
        ensure!(response.status() == StatusCode::NOT_FOUND);

        let pause_path = format!("/v1/products/{}/accounts/account-001/pause", product.slug);
        let pause_body = serde_json::to_vec(&json!({"scopes": ["addresses", "settlement"]}))?;
        let response = app
            .clone()
            .oneshot(signed_request(Method::POST, &pause_path, pause_body, PRODUCT_KID, &product_key, now))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let paused = response_json(response).await?;
        ensure!(paused["paused_scopes"] == json!(["addresses", "settlement"]));
        let stored_scopes: Vec<String> =
            sqlx::query_scalar("SELECT paused_scopes FROM accounts WHERE id = $1")
                .bind(Uuid::parse_str(
                    account["id"].as_str().context("account id")?,
                )?)
                .fetch_one(&database.app_pool)
                .await?;
        ensure!(stored_scopes == ["addresses", "settlement"]);
        let audit_count: i64 = sqlx::query_scalar("SELECT count(*) FROM audit WHERE subject = $1")
            .bind(format!("account:{}", account["id"].as_str().context("account id")?))
            .fetch_one(&database.app_pool)
            .await?;
        ensure!(audit_count == 1);

        let paused_address = app
            .clone()
            .oneshot(signed_request(
                Method::GET,
                &address_path,
                Vec::new(),
                PRODUCT_KID,
                &product_key,
                now + 2,
            ))
            .await?;
        ensure!(paused_address.status() == StatusCode::LOCKED);
        ensure!(response_json(paused_address).await?["error"]["code"] == "paused");

        let admin_path = "/v1/admin/routes/phala-cloud-ethereum-pha-usd/pause";
        let admin_body = serde_json::to_vec(&json!({"scopes": ["flush"]}))?;
        let product_signed = app
            .clone()
            .oneshot(signed_request(Method::POST, admin_path, admin_body.clone(), PRODUCT_KID, &product_key, now))
            .await?;
        ensure!(product_signed.status() == StatusCode::UNAUTHORIZED);
        let admin_signed = app
            .oneshot(signed_request(Method::POST, admin_path, admin_body, ADMIN_KID, &admin_key, now))
            .await?;
        ensure!(admin_signed.status() == StatusCode::OK);
        let admin_audit_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM audit WHERE subject = $1")
                .bind("route:phala-cloud-ethereum-pha-usd")
                .fetch_one(&database.app_pool)
                .await?;
        ensure!(admin_audit_count == 1);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn persistent_addresses_start_scanning_at_the_chain_cursor() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[23; 32]);
        let admin_key = SigningKey::from_bytes(&[24; 32]);
        let product =
            seed_product(&database.app_pool, "phala-cloud", PRODUCT_KID, &product_key).await?;
        topup::db::create_account(
            &database.app_pool,
            &NewAccount {
                id: Uuid::new_v4(),
                product_id: product.id,
                external_id: "cursor-001".to_owned(),
                paused_scopes: Vec::new(),
            },
        )
        .await?;
        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();
        let address_path = format!(
            "/v1/products/{}/accounts/cursor-001/deposit-address",
            product.slug
        );

        sqlx::query("INSERT INTO cursors (chain_id, scanned_block) VALUES (1, 1234)")
            .execute(&database.app_pool)
            .await?;
        let issued = app
            .clone()
            .oneshot(signed_request(Method::POST, &address_path, Vec::new(), PRODUCT_KID, &product_key, now))
            .await?;
        ensure!(issued.status() == StatusCode::OK);

        sqlx::query("UPDATE cursors SET scanned_block = 5678 WHERE chain_id = 1")
            .execute(&database.app_pool)
            .await?;
        let rotated = app
            .oneshot(signed_request(
                Method::POST,
                &format!("{address_path}/rotate"),
                serde_json::to_vec(&json!({"from_version": 1}))?,
                PRODUCT_KID,
                &product_key,
                now + 1,
            ))
            .await?;
        ensure!(rotated.status() == StatusCode::OK);

        let versions: Vec<(i64, i64, bool)> = sqlx::query_as(
            "SELECT version, created_block, backfilled FROM addresses WHERE kind = 'persistent' ORDER BY version",
        )
        .fetch_all(&database.app_pool)
        .await?;
        ensure!(versions == [(1, 1234, false), (2, 5678, false)], "{versions:?}");
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn route_pause_controls_address_routes() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[27; 32]);
        let admin_key = SigningKey::from_bytes(&[28; 32]);
        let product =
            seed_product(&database.app_pool, "phala-cloud", PRODUCT_KID, &product_key).await?;
        let account = topup::db::create_account(
            &database.app_pool,
            &NewAccount {
                id: Uuid::new_v4(),
                product_id: product.id,
                external_id: "route-pause-account".to_owned(),
                paused_scopes: Vec::new(),
            },
        )
        .await?;
        topup::db::set_product_paused_scopes(
            &database.app_pool,
            product.id,
            &["quotes".to_owned()],
        )
        .await?;
        topup::db::set_account_paused_scopes(
            &database.app_pool,
            account.id,
            &["settlement".to_owned()],
        )
        .await?;

        let app = test_router(&database.app_pool, &admin_key);
        let health = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/healthz")
                    .body(Body::empty())?,
            )
            .await?;
        ensure!(health.status() == StatusCode::OK);
        let now = Utc::now().timestamp();
        let address_path = "/v1/products/phala-cloud/accounts/route-pause-account/deposit-address";
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                address_path,
                Vec::new(),
                PRODUCT_KID,
                &product_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);

        let pause_path = "/v1/admin/routes/phala-cloud-ethereum-pha-usd/pause";
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                pause_path,
                serde_json::to_vec(&json!({"scopes": ["flush"]}))?,
                ADMIN_KID,
                &admin_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::GET,
                address_path,
                Vec::new(),
                PRODUCT_KID,
                &product_key,
                now + 1,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                pause_path,
                serde_json::to_vec(&json!({"scopes": ["addresses"]}))?,
                ADMIN_KID,
                &admin_key,
                now + 1,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::GET,
                address_path,
                Vec::new(),
                PRODUCT_KID,
                &product_key,
                now + 2,
            ))
            .await?;
        ensure!(response.status() == StatusCode::LOCKED);
        ensure!(response_json(response).await?["error"]["code"] == "paused");

        let resume_path = "/v1/admin/routes/phala-cloud-ethereum-pha-usd/resume";
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                resume_path,
                serde_json::to_vec(&json!({"scopes": ["addresses"]}))?,
                ADMIN_KID,
                &admin_key,
                now + 2,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);

        let response = app
            .oneshot(signed_request(
                Method::GET,
                address_path,
                Vec::new(),
                PRODUCT_KID,
                &product_key,
                now + 3,
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
async fn frozen_chain_refuses_address_issuance_and_rate_locks() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[31; 32]);
        let admin_key = SigningKey::from_bytes(&[32; 32]);
        let product =
            seed_product(&database.app_pool, "phala-cloud", PRODUCT_KID, &product_key).await?;
        topup::db::create_account(
            &database.app_pool,
            &NewAccount {
                id: Uuid::new_v4(),
                product_id: product.id,
                external_id: "frozen-account".to_owned(),
                paused_scopes: Vec::new(),
            },
        )
        .await?;
        sqlx::query(
            r#"
            INSERT INTO reconciliation_blocks (block_key, scope, chain_id, check_name, reason)
            VALUES ('chain:1', 'chain', 1, 'address_derivation', 'test freeze')
            "#,
        )
        .execute(&database.app_pool)
        .await?;

        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();
        let address_path = "/v1/products/phala-cloud/accounts/frozen-account/deposit-address";
        for (method, created) in [(Method::GET, now), (Method::POST, now + 1)] {
            let response = app
                .clone()
                .oneshot(signed_request(
                    method,
                    address_path,
                    Vec::new(),
                    PRODUCT_KID,
                    &product_key,
                    created,
                ))
                .await?;
            ensure!(response.status() == StatusCode::LOCKED);
            ensure!(response_json(response).await?["error"]["code"] == "chain_frozen");
        }
        let issued: i64 = sqlx::query_scalar("SELECT count(*) FROM addresses")
            .fetch_one(&database.app_pool)
            .await?;
        ensure!(issued == 0);

        let lock_body = serde_json::to_vec(&json!({
            "amount_minor": "1000",
            "product_lock_ref": "checkout-1",
        }))?;
        let response = app
            .oneshot(signed_request(
                Method::POST,
                "/v1/products/phala-cloud/accounts/frozen-account/rate-locks",
                lock_body,
                PRODUCT_KID,
                &product_key,
                now + 2,
            ))
            .await?;
        ensure!(response.status() == StatusCode::LOCKED);
        ensure!(response_json(response).await?["error"]["code"] == "chain_frozen");
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[cfg(feature = "dev-signer")]
#[tokio::test]
async fn attestation_http_path_uses_the_dev_signer() -> Result<()> {
    let admin_key = SigningKey::from_bytes(&[30; 32]);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1/unused")?;
    let attestor = Arc::new(DevHttpAttestor(DevSigner::new(
        SecretKey32::new([1; 32]),
        SecretKey32::new([2; 32]),
    )));
    let state = app_state_with_attestor(pool, &admin_key, attestor);
    let app = topup::api::router(state).0;
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/v1/attestation?nonce=00010203")
                .body(Body::empty())?,
        )
        .await?;
    ensure!(response.status() == StatusCode::OK);
    let response = response_json(response).await?;
    ensure!(response["keyid"] == SETTLEMENT_KEY_DOMAIN);
    ensure!(response["settlement_pubkey"].as_str().map(str::len) == Some(64));
    ensure!(response["report_data"].as_str().map(str::len) == Some(64));
    ensure!(response["quote"] == "");
    Ok(())
}

#[tokio::test]
async fn openapi_snapshot() -> Result<()> {
    let admin_key = SigningKey::from_bytes(&[29; 32]);
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://unused:unused@127.0.0.1/unused")?;
    let state = app_state(pool, &admin_key);
    let actual = topup::api::openapi_json(state)?;
    let document: Value = serde_json::from_str(&actual)?;
    ensure!(document["info"]["title"] == "Crypto Top-up Service API");
    ensure!(document["info"]["version"] == env!("CARGO_PKG_VERSION"));
    ensure!(document["info"]["description"].as_str().is_some());
    assert_query_parameters(&document)?;
    let path = format!("{}/openapi.json", env!("CARGO_MANIFEST_DIR"));
    if std::env::var_os("UPDATE_OPENAPI").is_some() {
        std::fs::write(&path, &actual)?;
    }
    let expected = std::fs::read_to_string(&path).context("read committed OpenAPI snapshot")?;
    ensure!(
        actual == expected,
        "openapi.json drifted; regenerate and review it"
    );
    Ok(())
}

fn test_router(pool: &sqlx::PgPool, admin_key: &SigningKey) -> axum::Router {
    topup::api::router(app_state(pool.clone(), admin_key)).0
}

fn app_state(pool: sqlx::PgPool, admin_key: &SigningKey) -> AppState {
    app_state_with_attestor(pool, admin_key, Arc::new(UnavailableAttestor))
}

fn app_state_with_attestor(
    pool: sqlx::PgPool,
    admin_key: &SigningKey,
    attestor: Arc<dyn Attestor>,
) -> AppState {
    let route: RouteFile = serde_saphyr::from_str(include_str!("fixtures/phala-cloud-pha.yaml"))
        .expect("valid route fixture");
    route.validate().expect("route fixture validates");
    AppState {
        pool,
        routes: Arc::new(vec![route]),
        admin_key: VerificationKey::from_base64(
            ADMIN_KID.to_owned(),
            &public_key_base64(admin_key),
        )
        .expect("admin key is valid"),
        attestor,
        rate_lock_quotes: Arc::new(topup::locks::UnavailableQuoteProvider),
    }
}

#[cfg(feature = "dev-signer")]
struct DevHttpAttestor(DevSigner);

#[cfg(feature = "dev-signer")]
impl Attestor for DevHttpAttestor {
    fn attest<'a>(&'a self, nonce: &'a [u8]) -> AttestationFuture<'a> {
        Box::pin(async move {
            let public_key = self
                .0
                .settlement_public_key()
                .await
                .map_err(|_| AttestationError::Unavailable)?;
            Ok(AttestationResponse {
                keyid: SETTLEMENT_KEY_DOMAIN.to_owned(),
                settlement_pubkey: hex::encode(public_key.0),
                report_data: hex::encode(report_data(nonce, &public_key)),
                quote: String::new(),
            })
        })
    }
}

fn assert_query_parameters(document: &Value) -> Result<()> {
    let cases = [
        (
            "/v1/products/{p}/accounts/{ext}/deposits",
            &["state", "from", "to", "cursor"][..],
        ),
        (
            "/v1/products/{p}/deposits",
            &["tx_hash", "address", "lock_ref", "cursor"][..],
        ),
    ];
    for (path, expected_names) in cases {
        let parameters = document["paths"][path]["get"]["parameters"]
            .as_array()
            .context("OpenAPI operation parameters")?;
        for name in expected_names {
            let parameter = parameters
                .iter()
                .find(|parameter| parameter["name"] == *name)
                .with_context(|| format!("missing query parameter {name}"))?;
            ensure!(parameter["in"] == "query");
            ensure!(parameter.get("required").and_then(Value::as_bool) != Some(true));
        }
    }

    let nonce = document["paths"]["/v1/attestation"]["get"]["parameters"]
        .as_array()
        .context("attestation parameters")?
        .iter()
        .find(|parameter| parameter["name"] == "nonce")
        .context("missing nonce parameter")?;
    ensure!(nonce["in"] == "query");
    ensure!(nonce["required"] == true);
    Ok(())
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

async fn seed_other_tenant_deposit(pool: &sqlx::PgPool, product_id: Uuid) -> Result<Uuid> {
    let account = topup::db::create_account(
        pool,
        &NewAccount {
            id: Uuid::new_v4(),
            product_id,
            external_id: "other-account".to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    let address = topup::db::insert_address(
        pool,
        &NewAddress {
            id: Uuid::new_v4(),
            account_id: account.id,
            chain_id: 1,
            kind: AddressKind::Persistent,
            version: 1,
            lock_ref: None,
            salt: B256::from_str(
                "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )?,
            address: Address::from_str("0x1111111111111111111111111111111111111111")?,
            retired_at: None,
        },
    )
    .await?;
    let tx_hash =
        B256::from_str("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")?;
    topup::db::insert_deposit(
        pool,
        &NewDeposit {
            chain_id: 1,
            tx_hash,
            log_index: 0,
            block_number: 1,
            block_hash: B256::from_str(
                "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            )?,
            block_time: Utc::now(),
            address_id: address.id,
            account_id: account.id,
            route: Some("other-route".to_owned()),
            route_version: Some(1),
            asset_contract: Address::from_str("0x2222222222222222222222222222222222222222")?,
            from_address: Address::from_str("0x3333333333333333333333333333333333333333")?,
            amount_atomic: AtomicAmount::new(U256::from(100_u64)),
            state: DepositState::Detected,
            reason: None,
            next_attempt_at: Utc::now(),
        },
    )
    .await?;
    Ok(deposit_id(1, tx_hash, 0))
}

async fn response_json(response: axum::response::Response) -> Result<Value> {
    let bytes = to_bytes(response.into_body(), 1_048_576).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

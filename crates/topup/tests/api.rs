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
use topup::api::{AppState, Attestor, PublicOrigin, VerificationKey};
#[cfg(feature = "dev-signer")]
use topup::api::{AttestationError, AttestationFuture};
use topup::db::{Account, NewDeposit};
use topup_adapters::attestation::DstackAttestor;
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

use support::seed::{self, NewAccount, NewAddress, NewCustomer};
use support::{
    SignatureOptions, SignatureParameter, TEST_ORIGIN, TestDatabase, merchant_request,
    public_key_base64, signed_request, signed_request_with_options,
};

const ADMIN_KID: &str = "admin/v1";

/// The admin API keeps RFC 9421 request signatures (design D7).
#[tokio::test]
async fn admin_signature_verification_vectors() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let admin_key = SigningKey::from_bytes(&[9; 32]);
        let app = test_router(&database.app_pool, &admin_key);
        let path = "/v1/admin/report/daily".to_owned();
        let body = serde_json::to_vec(&json!({"external_id": "signed-account"}))?;
        let now = Utc::now().timestamp();

        let response = app
            .clone()
            .oneshot(signed_request_with_options(
                Method::GET,
                &path,
                body.clone(),
                ADMIN_KID,
                &admin_key,
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
                Method::GET,
                &path,
                serde_json::to_vec(&json!({"external_id": "with-idempotency"}))?,
                ADMIN_KID,
                &admin_key,
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
                Method::GET,
                &path,
                serde_json::to_vec(&json!({"external_id": "with-alg"}))?,
                ADMIN_KID,
                &admin_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);

        let response = app
            .clone()
            .oneshot(signed_request_with_options(
                Method::GET,
                &path,
                serde_json::to_vec(&json!({"external_id": "reordered-parameters"}))?,
                ADMIN_KID,
                &admin_key,
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
                Method::GET,
                &path,
                serde_json::to_vec(&json!({"external_id": "different-label"}))?,
                ADMIN_KID,
                &admin_key,
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
                Method::GET,
                &origin_path,
                serde_json::to_vec(&json!({"external_id": "origin-form-query"}))?,
                ADMIN_KID,
                &admin_key,
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
                Method::GET,
                &path,
                serde_json::to_vec(&json!({"external_id": "wrong-algorithm"}))?,
                ADMIN_KID,
                &admin_key,
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
                Method::GET,
                &path,
                body.clone(),
                ADMIN_KID,
                &wrong_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::GET,
                &path,
                serde_json::to_vec(&json!({"external_id": "future-created"}))?,
                ADMIN_KID,
                &admin_key,
                (Utc::now() + Duration::minutes(6)).timestamp(),
            ))
            .await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);

        let mut tampered = signed_request(
            Method::GET,
            &path,
            body.clone(),
            ADMIN_KID,
            &admin_key,
            now,
        );
        *tampered.body_mut() = Body::from(r#"{"external_id":"tampered"}"#);
        let response = app.clone().oneshot(tampered).await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::GET,
                &path,
                body.clone(),
                ADMIN_KID,
                &admin_key,
                (Utc::now() - Duration::minutes(6)).timestamp(),
            ))
            .await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);

        let mut missing_component = signed_request(
            Method::GET,
            &path,
            serde_json::to_vec(&json!({"external_id": "missing-component"}))?,
            ADMIN_KID,
            &admin_key,
            now,
        );
        missing_component.headers_mut().insert(
            "signature-input",
            format!(
                "sig1=(\"@method\" \"@target-uri\");created={now};keyid=\"{ADMIN_KID}\";alg=\"ed25519\""
            )
            .parse()?,
        );
        let response = app.clone().oneshot(missing_component).await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);

        let replay_body = serde_json::to_vec(&json!({"external_id": "replay"}))?;
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::GET,
                &path,
                replay_body.clone(),
                ADMIN_KID,
                &admin_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let response = app
            .oneshot(signed_request(
                Method::GET,
                &path,
                replay_body,
                ADMIN_KID,
                &admin_key,
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

/// Behind the gateway `Host` and `X-Forwarded-*` describe the internal hop; only the configured
/// public origin determines an admin request's `@target-uri`.
#[tokio::test]
async fn admin_target_uri_uses_the_configured_public_origin() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let admin_key = SigningKey::from_bytes(&[9; 32]);
        let app = test_router(&database.app_pool, &admin_key);
        let path = "/v1/admin/report/daily".to_owned();
        let now = Utc::now().timestamp();
        let request = |external_id: &str, origin: &str| -> Result<_> {
            let mut request = signed_request_with_options(
                Method::GET,
                &path,
                serde_json::to_vec(&json!({"external_id": external_id}))?,
                ADMIN_KID,
                &admin_key,
                now,
                &SignatureOptions {
                    origin_form: true,
                    origin: origin.to_owned(),
                    ..SignatureOptions::default()
                },
            );
            let headers = request.headers_mut();
            headers.insert("host", "topup-internal:8080".parse()?);
            headers.insert("x-forwarded-proto", "https".parse()?);
            headers.insert("x-forwarded-host", "attacker.example".parse()?);
            Ok(request)
        };

        let response = app
            .clone()
            .oneshot(request("public-origin", TEST_ORIGIN)?)
            .await?;
        ensure!(response.status() == StatusCode::OK);

        for (external_id, origin) in [
            ("internal-host", "http://topup-internal:8080"),
            ("forwarded-proto", "https://api.test"),
            ("forwarded-host", "https://attacker.example"),
            ("other-port", "http://api.test:8080"),
        ] {
            let response = app.clone().oneshot(request(external_id, origin)?).await?;
            ensure!(
                response.status() == StatusCode::UNAUTHORIZED,
                "a signature for {origin} must not verify"
            );
        }

        // A signature for one path must not authorize a request to another route.
        let mut tampered = request("tampered-path", TEST_ORIGIN)?;
        *tampered.uri_mut() = "/v1/admin/deposits/dep_00000000000000000000000000000000".parse()?;
        let response = app.clone().oneshot(tampered).await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn tenant_isolation_and_operator_pauses() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let admin_key = SigningKey::from_bytes(&[19; 32]);
        let (product, product_key) = seed_product(&database.app_pool, "phala-cloud").await?;
        let (other, _) = seed_product(&database.app_pool, "builder").await?;
        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();

        let customer_id = seed_customer(&database.app_pool, product.id, "account-001")
            .await?
            .id;

        let other_deposit = seed_other_tenant_deposit(&database.app_pool, other.id).await?;
        let cross_tenant_path = format!("/v1/deposits/dep_{}", other_deposit.simple());
        let response = app
            .clone()
            .oneshot(merchant_request(
                Method::GET,
                &cross_tenant_path,
                Vec::new(),
                &product_key,
            ))
            .await?;
        ensure!(response.status() == StatusCode::NOT_FOUND);

        // Pausing a customer is an operator action.
        let pause_path = format!(
            "/v1/admin/accounts/{}/customers/account-001/pause",
            product.public_id
        );
        let pause_body =
            serde_json::to_vec(&json!({"scopes": ["quotes", "settlement"], "livemode": true}))?;
        let response = app
            .clone()
            .oneshot(merchant_request(
                Method::POST,
                &pause_path,
                pause_body.clone(),
                &product_key,
            ))
            .await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &pause_path,
                pause_body,
                ADMIN_KID,
                &admin_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let paused = response_json(response).await?;
        ensure!(paused["paused_scopes"] == json!(["quotes", "settlement"]));
        let stored_scopes: Vec<String> =
            sqlx::query_scalar("SELECT paused_scopes FROM customers WHERE id = $1")
                .bind(customer_id)
                .fetch_one(&database.app_pool)
                .await?;
        ensure!(stored_scopes == ["quotes", "settlement"]);
        let audit_count: i64 = sqlx::query_scalar("SELECT count(*) FROM audit WHERE subject = $1")
            .bind(format!("customer:{customer_id}"))
            .fetch_one(&database.app_pool)
            .await?;
        ensure!(audit_count == 1);

        // The paused customer gets no quote.
        let paused_quote = app
            .clone()
            .oneshot(merchant_request(
                Method::POST,
                "/v1/quotes",
                serde_json::to_vec(&json!({
                    "account_id": "account-001", "amount": 1000, "currency": "usd",
                    "chain_id": 1, "asset": "pha",
                }))?,
                &product_key,
            ))
            .await?;
        ensure!(paused_quote.status() == StatusCode::CONFLICT);
        ensure!(response_json(paused_quote).await?["error"]["code"] == "paused");

        let admin_path = "/v1/admin/routes/phala-cloud-ethereum-pha-usd/pause";
        let admin_body = serde_json::to_vec(&json!({"scopes": ["refunds"]}))?;
        let product_signed = app
            .clone()
            .oneshot(merchant_request(
                Method::POST,
                admin_path,
                admin_body.clone(),
                &product_key,
            ))
            .await?;
        ensure!(product_signed.status() == StatusCode::UNAUTHORIZED);
        let admin_signed = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                admin_path,
                admin_body,
                ADMIN_KID,
                &admin_key,
                now,
            ))
            .await?;
        ensure!(admin_signed.status() == StatusCode::OK);
        // The service sends no transactions, so there is no `flush` scope to pause.
        let removed_scope = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                admin_path,
                serde_json::to_vec(&json!({"scopes": ["flush"]}))?,
                ADMIN_KID,
                &admin_key,
                now + 1,
            ))
            .await?;
        ensure!(removed_scope.status() == StatusCode::BAD_REQUEST);
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

/// Every merchant endpoint that names an object answers `404` to another account's key and to
/// the owning account's key in the other mode, exactly as for an object that does not exist, and
/// lists show neither (design D13: the scope comes from the credential, never the request).
#[tokio::test]
async fn every_merchant_endpoint_is_404_across_accounts_and_modes() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let pool = &database.app_pool;
        let admin_key = SigningKey::from_bytes(&[23; 32]);
        let (owner, owner_key) = seed_product(pool, "owner").await?;
        let (_, other_key) = seed_product(pool, "other").await?;
        // The owner's own key of the other mode.
        let owner_test_key = seed::create_api_key(pool, owner.id, false).await?;
        let owner_key_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM api_keys WHERE account_id = $1 AND livemode ORDER BY created_at LIMIT 1",
        )
        .bind(owner.id)
        .fetch_one(pool)
        .await?;
        let customer = seed_customer(pool, owner.id, "owned-customer").await?;
        let address = seed::insert_address(
            pool,
            &NewAddress {
                id: Uuid::new_v4(),
                customer_id: customer.id,
                chain_id: 1,
                route: "phala-cloud-ethereum-pha-usd".to_owned(),
                salt: B256::repeat_byte(0x61),
                address: Address::repeat_byte(0x62),
            },
        )
        .await?;
        let tx_hash = B256::repeat_byte(0x63);
        ensure!(
            topup::db::insert_deposit(
                pool,
                &NewDeposit {
                    chain_id: 1,
                    tx_hash,
                    log_index: 0,
                    receipt_log_index: 0,
                    tx_from: Address::ZERO,
                    tx_nonce: 0,
                    is_final: true,
                    block_number: 5,
                    block_hash: B256::repeat_byte(0x64),
                    block_time: Utc::now(),
                    address_id: address.id,
                    route: Some("phala-cloud-ethereum-pha-usd".to_owned()),
                    route_version: Some(1),
                    asset_contract: Address::repeat_byte(0x65),
                    from_address: Address::repeat_byte(0x66),
                    amount_atomic: AtomicAmount::new(U256::from(1_000_u64)),
                    state: DepositState::Rejected,
                    reason: Some(topup_core::deposit::RejectReason::OutOfBounds),
                    next_attempt_at: Utc::now() + Duration::hours(1),
                },
            )
            .await?
        );
        let deposit = deposit_id(1, tx_hash, 0);
        let refund = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO refunds (id, account_id, livemode, deposit_id, amount_atomic, to_address,
                                 route, status, requested_by)
            SELECT $1, account_id, livemode, id, 10, $3, route, 'requested', 'test'
            FROM deposits WHERE id = $2
            "#,
        )
        .bind(refund)
        .bind(deposit)
        .bind(format!("{:#x}", Address::repeat_byte(0x67)))
        .execute(pool)
        .await?;

        let quote = topup::ids::format(topup::ids::QUOTE, address.quote_id);
        let deposit = topup::ids::format(topup::ids::DEPOSIT, deposit);
        let refund = topup::ids::format(topup::ids::REFUND, refund);
        let api_key = topup::ids::format(topup::ids::API_KEY, owner_key_id);
        let refund_body = serde_json::to_vec(&json!({
            "deposit": deposit,
            "destination_address": format!("{:#x}", Address::repeat_byte(0x68)),
            "amount_atomic": "10",
        }))?;
        let object_requests = [
            (Method::GET, format!("/v1/quotes/{quote}"), Vec::new()),
            (
                Method::GET,
                format!("/v1/quotes/{quote}?expand[]=deposit"),
                Vec::new(),
            ),
            (
                Method::POST,
                format!("/v1/quotes/{quote}/cancel"),
                Vec::new(),
            ),
            (Method::GET, format!("/v1/deposits/{deposit}"), Vec::new()),
            (
                Method::GET,
                format!("/v1/deposits/{deposit}?expand[]=quote"),
                Vec::new(),
            ),
            (Method::GET, format!("/v1/refunds/{refund}"), Vec::new()),
            (
                Method::GET,
                format!("/v1/refunds/{refund}?expand[]=deposit"),
                Vec::new(),
            ),
            (Method::POST, "/v1/refunds".to_owned(), refund_body),
            (Method::GET, format!("/v1/api_keys/{api_key}"), Vec::new()),
        ];
        // Key mutations the owner is not asked to make: another tenant must not reach them.
        let key_mutations = [
            (
                Method::POST,
                format!("/v1/api_keys/{api_key}/roll"),
                serde_json::to_vec(&json!({"expires_in": 60}))?,
            ),
            (Method::DELETE, format!("/v1/api_keys/{api_key}"), Vec::new()),
        ];
        let lists = [
            "/v1/deposits".to_owned(),
            format!("/v1/deposits?quote={quote}"),
            "/v1/deposits?account_id=owned-customer".to_owned(),
        ];
        let app = test_router(pool, &admin_key);
        let call = |method: Method, path: &str, body: Vec<u8>, key: &str| {
            let request = merchant_request(method, path, body, key);
            let app = app.clone();
            async move {
                let response = app.oneshot(request).await?;
                let status = response.status();
                anyhow::Ok((status, response_json(response).await?))
            }
        };

        // The owner reaches every one of its objects; its refund request succeeds.
        for (method, path, body) in &object_requests {
            let (status, answer) =
                call(method.clone(), path, body.clone(), &owner_key).await?;
            ensure!(
                status == StatusCode::OK,
                "owner {method} {path}: {status} {answer}"
            );
        }
        for path in &lists {
            let (status, answer) =
                call(Method::GET, path, Vec::new(), &owner_key).await?;
            ensure!(status == StatusCode::OK && answer["data"].as_array().map(Vec::len) == Some(1));
        }

        // Another account, and the owner's own key of the other mode, see nothing.
        for (who, key) in [
            ("other account", &other_key),
            ("other mode", &owner_test_key),
        ] {
            for (method, path, body) in object_requests.iter().chain(&key_mutations) {
                let (status, answer) = call(method.clone(), path, body.clone(), key).await?;
                ensure!(
                    status == StatusCode::NOT_FOUND
                        && answer["error"]["code"] == "resource_missing",
                    "{who} {method} {path}: {status} {answer}"
                );
            }
            for path in &lists {
                let (status, answer) = call(Method::GET, path, Vec::new(), key).await?;
                ensure!(
                    status == StatusCode::OK && answer["data"] == json!([]),
                    "{who} {path}: {status} {answer}"
                );
            }
            // A cursor naming a foreign deposit fails exactly like one naming no deposit.
            let (status, foreign) = call(
                Method::GET,
                &format!("/v1/deposits?starting_after={deposit}"),
                Vec::new(),
                key,
            )
            .await?;
            let unknown = topup::ids::format(topup::ids::DEPOSIT, Uuid::new_v4());
            let (unknown_status, missing) = call(
                Method::GET,
                &format!("/v1/deposits?starting_after={unknown}"),
                Vec::new(),
                key,
            )
            .await?;
            ensure!(
                status == unknown_status && foreign == missing,
                "{who}: {foreign}"
            );
        }
        // Nothing was written for the other tenants: the owner's objects are untouched.
        let refunds: i64 = sqlx::query_scalar("SELECT count(*) FROM refunds")
            .fetch_one(pool)
            .await?;
        ensure!(
            refunds == 2,
            "only the owner's own request created a refund"
        );
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

/// An instance restored from backup serves reads only, and its `/healthz` carries the boot-time
/// restore-check report (`deploy/RESTORE.md`).
#[tokio::test]
async fn read_only_router_refuses_writes_and_reports_the_restore_check() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let report = std::env::temp_dir().join(format!(
        "topup-api-restore-check-{}.json",
        std::process::id()
    ));
    let result = async {
        let admin_key = SigningKey::from_bytes(&[44; 32]);
        let app = topup::api::read_only_router(
            app_state(database.app_pool.clone(), &admin_key),
            Some(report.clone()),
        );
        let healthz = || -> Result<_> {
            Ok(axum::http::Request::builder()
                .uri("/healthz")
                .body(Body::empty())?)
        };

        let response = app.clone().oneshot(healthz()?).await?;
        ensure!(response.status() == StatusCode::OK);
        let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
        ensure!(
            body == json!({"mode": "read-only", "restore_check": null}),
            "{body}"
        );

        std::fs::write(&report, r#"{"status":"ok","rpo_basis":"unanchored"}"#)?;
        let response = app.clone().oneshot(healthz()?).await?;
        let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
        ensure!(body["restore_check"]["status"] == "ok", "{body}");

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                "/v1/admin/accounts",
                serde_json::to_vec(&json!({
                    "name": "Phala Cloud",
                    "contact": {"name": "Ops", "email": "ops@product.test"},
                    "due_diligence": {"reference": "DD-1", "reviewed_at": "2026-09-28",
                                      "reviewed_by": "operator"},
                    "reason": "onboarding",
                }))?,
                ADMIN_KID,
                &admin_key,
                Utc::now().timestamp(),
            ))
            .await?;
        ensure!(response.status() == StatusCode::SERVICE_UNAVAILABLE);
        let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
        ensure!(body["error"]["code"] == "unavailable", "{body}");
        let accounts: i64 = sqlx::query_scalar("SELECT count(*) FROM accounts")
            .fetch_one(&database.app_pool)
            .await?;
        ensure!(accounts == 0);
        Ok(())
    }
    .await;
    let _ = std::fs::remove_file(&report);
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn frozen_chain_refuses_quotes() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let admin_key = SigningKey::from_bytes(&[32; 32]);
        let (product, product_key) = seed_product(&database.app_pool, "phala-cloud").await?;
        seed_customer(&database.app_pool, product.id, "frozen-account").await?;
        sqlx::query(
            r#"
            INSERT INTO reconciliation_blocks (block_key, scope, chain_id, check_name, reason)
            VALUES ('chain:1', 'chain', 1, 'address_derivation', 'test freeze')
            "#,
        )
        .execute(&database.app_pool)
        .await?;

        let app = test_router(&database.app_pool, &admin_key);
        let lock_body = serde_json::to_vec(&json!({
            "account_id": "frozen-account", "amount": 1000, "currency": "usd",
            "chain_id": 1, "asset": "pha",
        }))?;
        let response = app
            .oneshot(merchant_request(
                Method::POST,
                "/v1/quotes",
                lock_body,
                &product_key,
            ))
            .await?;
        ensure!(response.status() == StatusCode::CONFLICT);
        ensure!(response_json(response).await?["error"]["code"] == "chain_frozen");
        // A refused creation leaves no lock or lock address behind.
        let leftovers: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM quotes) + (SELECT count(*) FROM addresses)",
        )
        .fetch_one(&database.app_pool)
        .await?;
        ensure!(leftovers == 0);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

/// `POST /v1/admin/reconciliation-blocks/{block_key}/lift` is admin-signed, needs a reason,
/// audits the lift with the block it removed, and answers a repeat with the first lift.
#[tokio::test]
async fn admin_lift_unfreezes_a_chain_once() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let admin_key = SigningKey::from_bytes(&[34; 32]);
        let (product, product_key) = seed_product(&database.app_pool, "phala-cloud").await?;
        seed_customer(&database.app_pool, product.id, "lift-account").await?;
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
        let admin = |method: Method, path: &str, body: Value, created: i64| -> Result<_> {
            Ok(signed_request(
                method,
                path,
                if body.is_null() {
                    Vec::new()
                } else {
                    serde_json::to_vec(&body)?
                },
                ADMIN_KID,
                &admin_key,
                created,
            ))
        };

        let response = app
            .clone()
            .oneshot(admin(
                Method::GET,
                "/v1/admin/report/daily",
                Value::Null,
                now,
            )?)
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let blocks = response_json(response).await?["reconciliation_blocks"].clone();
        ensure!(blocks.as_array().map(Vec::len) == Some(1));
        ensure!(blocks[0]["block_key"] == "chain:1");
        ensure!(blocks[0]["scope"] == "chain");
        ensure!(blocks[0]["check"] == "address_derivation");

        let lift = "/v1/admin/reconciliation-blocks/chain:1/lift";
        let reason = json!({"reason": "INC-7: factory confirmed, stored rows restored"});
        let response = app
            .clone()
            .oneshot(merchant_request(
                Method::POST,
                lift,
                serde_json::to_vec(&reason)?,
                &product_key,
            ))
            .await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);
        let response = app
            .clone()
            .oneshot(admin(Method::POST, lift, json!({"reason": " "}), now + 2)?)
            .await?;
        ensure!(response.status() == StatusCode::BAD_REQUEST);
        ensure!(response_json(response).await?["error"]["code"] == "parameter_invalid");
        let response = app
            .clone()
            .oneshot(admin(
                Method::POST,
                "/v1/admin/reconciliation-blocks/chain:2/lift",
                reason.clone(),
                now + 3,
            )?)
            .await?;
        ensure!(response.status() == StatusCode::NOT_FOUND);
        ensure!(response_json(response).await?["error"]["code"] == "resource_missing");

        let response = app
            .clone()
            .oneshot(admin(Method::POST, lift, reason.clone(), now + 4)?)
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let lifted = response_json(response).await?;
        ensure!(lifted["block_key"] == "chain:1");
        // A repeat answers with the first lift; generated clients percent-encode the key.
        let response = app
            .clone()
            .oneshot(admin(
                Method::POST,
                "/v1/admin/reconciliation-blocks/chain%3A1/lift",
                reason.clone(),
                now + 5,
            )?)
            .await?;
        ensure!(response.status() == StatusCode::OK);
        ensure!(response_json(response).await? == lifted);

        let audit = sqlx::query(
            "SELECT actor_type, actor_id, action, reason FROM audit \
             WHERE subject = 'reconciliation_block:chain:1'",
        )
        .fetch_all(&database.app_pool)
        .await?;
        ensure!(audit.len() == 1, "only the first lift is audited");
        ensure!(audit[0].try_get::<String, _>("actor_type")? == "admin");
        ensure!(audit[0].try_get::<String, _>("actor_id")? == ADMIN_KID);
        ensure!(audit[0].try_get::<String, _>("action")? == "reconciliation_block.lift");
        let evidence: Value = serde_json::from_str(&audit[0].try_get::<String, _>("reason")?)?;
        ensure!(evidence["reason"] == reason["reason"]);
        ensure!(evidence["block"]["check"] == "address_derivation");
        ensure!(evidence["block"]["reason"] == "test freeze");

        // The chain resumes without a restart.
        // Quote creation passes the frozen-chain check and reaches pricing, which the test
        // router does not configure.
        let response = app
            .clone()
            .oneshot(merchant_request(
                Method::POST,
                "/v1/quotes",
                serde_json::to_vec(&json!({
                    "account_id": "lift-account", "amount": 1000, "currency": "usd",
                    "chain_id": 1, "asset": "pha",
                }))?,
                &product_key,
            ))
            .await?;
        ensure!(response.status() == StatusCode::SERVICE_UNAVAILABLE);
        let response = app
            .oneshot(admin(
                Method::GET,
                "/v1/admin/report/daily",
                Value::Null,
                now + 7,
            )?)
            .await?;
        ensure!(response_json(response).await?["reconciliation_blocks"] == json!([]));
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

/// `POST /v1/admin/outbox/{event_id}/replay` requeues an existing event without touching its
/// payload; the operator finds the event id, `evt_…`, in the admin deposit view.
#[tokio::test]
async fn admin_replay_requeues_a_delivered_event_once() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let admin_key = SigningKey::from_bytes(&[36; 32]);
        let (product, product_key) = seed_product(&database.app_pool, "phala-cloud").await?;
        let deposit = seed_other_tenant_deposit(&database.app_pool, product.id).await?;
        let event_id = Uuid::new_v4();
        let payload = json!({"object": {"id": format!("dep_{}", deposit.simple())}});
        sqlx::query(
            r#"
            WITH event AS (
                INSERT INTO events (id, account_id, livemode, type, object_type, object_id, data,
                                    created, actor)
                VALUES ($1, $3, true, 'deposit.credited', 'deposit', $4, $2,
                        now() - interval '1 hour', 'system')
                RETURNING id, account_id
            )
            INSERT INTO webhook_deliveries (event_id, endpoint_id, next_attempt_at, delivered_at)
            SELECT event.id, endpoint.id, now() - interval '1 hour', now()
            FROM event
            JOIN webhook_endpoints AS endpoint ON endpoint.account_id = event.account_id
            "#,
        )
        .bind(event_id)
        .bind(&payload)
        .bind(product.id)
        .bind(deposit)
        .execute(&database.app_pool)
        .await?;
        let webhook_id = format!("evt_{}", event_id.simple());
        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::GET,
                &format!("/v1/admin/deposits/dep_{}", deposit.simple()),
                Vec::new(),
                ADMIN_KID,
                &admin_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let events = response_json(response).await?["events"].clone();
        ensure!(events.as_array().map(Vec::len) == Some(1), "{events}");
        ensure!(events[0]["id"] == webhook_id);
        ensure!(events[0]["event_type"] == "deposit.credited");
        ensure!(!events[0]["delivered_at"].is_null());

        let replay = format!("/v1/admin/outbox/{webhook_id}/replay");
        let reason = serde_json::to_vec(&json!({"reason": "product lost the event"}))?;
        let response = app
            .clone()
            .oneshot(merchant_request(
                Method::POST,
                &replay,
                reason.clone(),
                &product_key,
            ))
            .await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &replay,
                serde_json::to_vec(&json!({"reason": ""}))?,
                ADMIN_KID,
                &admin_key,
                now + 2,
            ))
            .await?;
        ensure!(response.status() == StatusCode::BAD_REQUEST);
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &format!("/v1/admin/outbox/{}/replay", Uuid::new_v4()),
                reason.clone(),
                ADMIN_KID,
                &admin_key,
                now + 3,
            ))
            .await?;
        ensure!(response.status() == StatusCode::NOT_FOUND);

        for created in [now + 4, now + 5] {
            let response = app
                .clone()
                .oneshot(signed_request(
                    Method::POST,
                    &replay,
                    reason.clone(),
                    ADMIN_KID,
                    &admin_key,
                    created,
                ))
                .await?;
            ensure!(response.status() == StatusCode::OK);
            let replayed = response_json(response).await?;
            ensure!(replayed["event_id"] == webhook_id);
            ensure!(replayed["event_type"] == "deposit.credited");
        }
        let row = sqlx::query(
            "SELECT event.data AS payload, \
             delivery.delivered_at IS NULL AND delivery.next_attempt_at <= now() AS due \
             FROM events AS event \
             JOIN webhook_deliveries AS delivery ON delivery.event_id = event.id \
             WHERE event.id = $1",
        )
        .bind(event_id)
        .fetch_one(&database.app_pool)
        .await?;
        ensure!(row.try_get::<bool, _>("due")?);
        ensure!(row.try_get::<Value, _>("payload")? == payload);
        let audit = sqlx::query(
            "SELECT actor_type, actor_id, action, reason FROM audit WHERE subject = $1",
        )
        .bind(format!("event:{event_id}"))
        .fetch_all(&database.app_pool)
        .await?;
        ensure!(
            audit.len() == 1,
            "a repeat while the event is due is not audited again"
        );
        ensure!(audit[0].try_get::<String, _>("actor_type")? == "admin");
        ensure!(audit[0].try_get::<String, _>("actor_id")? == ADMIN_KID);
        ensure!(audit[0].try_get::<String, _>("action")? == "outbox.replay");
        ensure!(audit[0].try_get::<String, _>("reason")? == "product lost the event");
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
    let seed = [1; 32];
    let attestor = Arc::new(DevHttpAttestor(seed));
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
    ensure!(response["quote"] == "");

    let settlement = DevSigner::derive(&SecretKey32::new(seed))
        .settlement_public_key()
        .await?;
    ensure!(response["settlement_pubkey"] == hex::encode(settlement.0));
    // The service sends no transactions, so no operator is attested.
    ensure!(response.get("operators").is_none());
    ensure!(response["report_data"] == hex::encode(report_data(&[0, 1, 2, 3], &settlement)));
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
    ensure!(document["info"]["title"] == "Phala Pay API");
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
    app_state_with_attestor(pool, admin_key, Arc::new(DstackAttestor::new()))
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
        routes: Arc::new(topup::routes::RouteSet::new(vec![route]).expect("route loads")),
        admin_key: VerificationKey::from_base64(
            ADMIN_KID.to_owned(),
            &public_key_base64(admin_key),
        )
        .expect("admin key is valid"),
        public_origin: PublicOrigin::parse(TEST_ORIGIN).expect("test origin is valid"),
        attestor,
        rate_lock_quotes: Arc::new(topup::locks::UnavailableQuoteProvider),
        client_reads: Arc::default(),
        rate_limits: Arc::default(),
    }
}

/// Development attestor deriving every key from one seed, as `topup attest --dev` does.
#[cfg(feature = "dev-signer")]
struct DevHttpAttestor([u8; 32]);

#[cfg(feature = "dev-signer")]
impl Attestor for DevHttpAttestor {
    fn attest<'a>(&'a self, nonce: &'a [u8]) -> AttestationFuture<'a> {
        Box::pin(async move {
            let public_key = DevSigner::derive(&SecretKey32::new(self.0))
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
    let cases = [(
        "/v1/deposits",
        &[
            "account_id",
            "quote",
            "status",
            "tx_hash",
            "created[gte]",
            "created[lte]",
            "limit",
            "starting_after",
            "ending_before",
            "expand[]",
        ][..],
    )];
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

/// A live account signing with `key`, with a webhook endpoint.
/// A live account and its live secret key.
async fn seed_product(pool: &sqlx::PgPool, name: &str) -> Result<(Account, String)> {
    let account = seed::create_account(
        pool,
        &NewAccount {
            webhook_url: "https://product.test/webhooks".to_owned(),
            ..NewAccount::named(name)
        },
    )
    .await?;
    let key = seed::create_api_key(pool, account.id, true).await?;
    Ok((account, key))
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

/// A live deposit of a new customer of `account_id`.
async fn seed_other_tenant_deposit(pool: &sqlx::PgPool, account_id: Uuid) -> Result<Uuid> {
    let customer = seed_customer(pool, account_id, "other-account").await?;
    let address = seed::insert_address(
        pool,
        &NewAddress {
            id: Uuid::new_v4(),
            customer_id: customer.id,
            chain_id: 1,
            route: "other-route".to_owned(),
            salt: B256::from_str(
                "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )?,
            address: Address::from_str("0x1111111111111111111111111111111111111111")?,
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
            receipt_log_index: 0,
            tx_from: alloy_primitives::Address::ZERO,
            tx_nonce: 0,
            is_final: true,
            block_number: 1,
            block_hash: B256::from_str(
                "0xcccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            )?,
            block_time: Utc::now(),
            address_id: address.id,
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

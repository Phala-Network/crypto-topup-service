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
use topup::api::models::{AttestationResponse, OperatorIdentity};
use topup::api::{AppState, Attestor, PublicOrigin, VerificationKey};
#[cfg(feature = "dev-signer")]
use topup::api::{AttestationError, AttestationFuture};
use topup::db::{AddressKind, NewDeposit};
use topup_adapters::attestation::DstackAttestor;
#[cfg(feature = "dev-signer")]
use topup_adapters::attestation::{AttestedOperator, OperatorKey, report_data};
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

use support::seed::{self, NewAccount, NewAddress, NewProduct};
use support::{
    SignatureOptions, SignatureParameter, TEST_ORIGIN, TestDatabase, public_key_base64,
    signed_request, signed_request_with_options,
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
        seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        let app = test_router(&database.app_pool, &admin_key);
        let path = "/v1/config".to_owned();
        let body = serde_json::to_vec(&json!({"external_id": "signed-account"}))?;
        let now = Utc::now().timestamp();

        let response = app
            .clone()
            .oneshot(signed_request_with_options(
                Method::GET,
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
                Method::GET,
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
                Method::GET,
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
                Method::GET,
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
                Method::GET,
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
                Method::GET,
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
                Method::GET,
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
                Method::GET,
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
                Method::GET,
                &path,
                serde_json::to_vec(&json!({"external_id": "future-created"}))?,
                PRODUCT_KID,
                &product_key,
                (Utc::now() + Duration::minutes(6)).timestamp(),
            ))
            .await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);

        let mut tampered = signed_request(
            Method::GET,
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
                Method::GET,
                &path,
                body.clone(),
                PRODUCT_KID,
                &product_key,
                (Utc::now() - Duration::minutes(6)).timestamp(),
            ))
            .await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);

        let mut missing_component = signed_request(
            Method::GET,
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
                Method::GET,
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
                Method::GET,
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

/// Behind the gateway `Host` and `X-Forwarded-*` describe the internal hop; only the configured
/// public origin determines `@target-uri`.
#[tokio::test]
async fn target_uri_uses_the_configured_public_origin() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[7; 32]);
        let admin_key = SigningKey::from_bytes(&[9; 32]);
        seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        let app = test_router(&database.app_pool, &admin_key);
        let path = "/v1/config".to_owned();
        let now = Utc::now().timestamp();
        let request = |external_id: &str, origin: &str| -> Result<_> {
            let mut request = signed_request_with_options(
                Method::GET,
                &path,
                serde_json::to_vec(&json!({"external_id": external_id}))?,
                PRODUCT_KID,
                &product_key,
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
        *tampered.uri_mut() = "/v1/deposits".parse()?;
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
        let product_key = SigningKey::from_bytes(&[17; 32]);
        let other_key = SigningKey::from_bytes(&[18; 32]);
        let admin_key = SigningKey::from_bytes(&[19; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        let other = seed_product(&database.app_pool, "builder", &other_key).await?;
        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();

        let account_id = seed::create_account(
            &database.app_pool,
            &NewAccount {
                id: Uuid::new_v4(),
                product_id: product.id,
                external_id: "account-001".to_owned(),
                paused_scopes: Vec::new(),
            },
        )
        .await?
        .id;

        let other_deposit = seed_other_tenant_deposit(&database.app_pool, other.id).await?;
        let cross_tenant_path = format!("/v1/deposits/dep_{}", other_deposit.simple());
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::GET,
                &cross_tenant_path,
                Vec::new(),
                PRODUCT_KID,
                &product_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::NOT_FOUND);

        // Pausing an account is an operator action.
        let pause_path = format!(
            "/v1/admin/products/{}/accounts/account-001/pause",
            product.slug
        );
        let pause_body = serde_json::to_vec(&json!({"scopes": ["quotes", "settlement"]}))?;
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &pause_path,
                pause_body.clone(),
                PRODUCT_KID,
                &product_key,
                now,
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
            sqlx::query_scalar("SELECT paused_scopes FROM accounts WHERE id = $1")
                .bind(account_id)
                .fetch_one(&database.app_pool)
                .await?;
        ensure!(stored_scopes == ["quotes", "settlement"]);
        let audit_count: i64 = sqlx::query_scalar("SELECT count(*) FROM audit WHERE subject = $1")
            .bind(format!("account:{account_id}"))
            .fetch_one(&database.app_pool)
            .await?;
        ensure!(audit_count == 1);

        // The paused account gets no quote.
        let paused_quote = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                "/v1/quotes",
                serde_json::to_vec(&json!({
                    "account_id": "account-001", "amount": 1000, "currency": "usd",
                    "chain_id": 1, "asset": "pha",
                }))?,
                PRODUCT_KID,
                &product_key,
                now + 2,
            ))
            .await?;
        ensure!(paused_quote.status() == StatusCode::CONFLICT);
        ensure!(response_json(paused_quote).await?["error"]["code"] == "paused");

        let admin_path = "/v1/admin/routes/phala-cloud-ethereum-pha-usd/pause";
        let admin_body = serde_json::to_vec(&json!({"scopes": ["flush"]}))?;
        let product_signed = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                admin_path,
                admin_body.clone(),
                PRODUCT_KID,
                &product_key,
                now,
            ))
            .await?;
        ensure!(product_signed.status() == StatusCode::UNAUTHORIZED);
        let admin_signed = app
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
        let product_key = SigningKey::from_bytes(&[43; 32]);
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
                "/v1/admin/products",
                serde_json::to_vec(&json!({
                    "slug": "phala-cloud",
                    "public_key": public_key_base64(&product_key),
                    "webhook_url": "https://product.test/webhooks",
                }))?,
                ADMIN_KID,
                &admin_key,
                Utc::now().timestamp(),
            ))
            .await?;
        ensure!(response.status() == StatusCode::SERVICE_UNAVAILABLE);
        let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 4096).await?)?;
        ensure!(body["error"]["code"] == "unavailable", "{body}");
        let products: i64 = sqlx::query_scalar("SELECT count(*) FROM products")
            .fetch_one(&database.app_pool)
            .await?;
        ensure!(products == 0);
        Ok(())
    }
    .await;
    let _ = std::fs::remove_file(&report);
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

/// `POST /v1/admin/products` is the only way to issue product credentials: admin-signed,
/// validated at the boundary, idempotent for identical values, and audited once.
#[tokio::test]
async fn admin_product_registration() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[41; 32]);
        let admin_key = SigningKey::from_bytes(&[42; 32]);
        let app = test_router(&database.app_pool, &admin_key);
        let path = "/v1/admin/products";
        let now = Utc::now().timestamp();
        let register = |body: Value, created: i64, kid: &str, key: &SigningKey| -> Result<_> {
            Ok(signed_request(
                Method::POST,
                path,
                serde_json::to_vec(&body)?,
                kid,
                key,
                created,
            ))
        };
        let valid = json!({
            "slug": "phala-cloud",
            "public_key": public_key_base64(&product_key),
            "webhook_url": "https://product.test/webhooks",
        });

        let response = app
            .clone()
            .oneshot(register(valid.clone(), now, PRODUCT_KID, &product_key)?)
            .await?;
        ensure!(response.status() == StatusCode::UNAUTHORIZED);

        let invalid = [
            json!({"slug": "Phala", "public_key": public_key_base64(&product_key),
                   "webhook_url": "https://product.test/webhooks"}),
            json!({"slug": "phala-cloud", "public_key": "AAAA",
                   "webhook_url": "https://product.test/webhooks"}),
            // The test origin is `http`, so only non-HTTP schemes are refused here.
            json!({"slug": "phala-cloud", "public_key": public_key_base64(&product_key),
                   "webhook_url": "ftp://product.test/webhooks"}),
            json!({"slug": "phala-cloud", "public_key": public_key_base64(&product_key),
                   "webhook_url": "https://user:secret@product.test/webhooks"}),
            json!({"slug": "unrouted", "public_key": public_key_base64(&product_key),
                   "webhook_url": "https://product.test/webhooks"}),
        ];
        for (offset, body) in (1_i64..).zip(invalid) {
            let response = app
                .clone()
                .oneshot(register(body.clone(), now + offset, ADMIN_KID, &admin_key)?)
                .await?;
            ensure!(
                response.status() == StatusCode::BAD_REQUEST,
                "{body} must be rejected"
            );
            ensure!(response_json(response).await?["error"]["code"] == "parameter_invalid");
        }

        let response = app
            .clone()
            .oneshot(register(valid.clone(), now + 10, ADMIN_KID, &admin_key)?)
            .await?;
        ensure!(response.status() == StatusCode::OK);
        let created = response_json(response).await?;
        ensure!(created["slug"] == "phala-cloud");
        ensure!(created["public_key"] == valid["public_key"]);
        ensure!(created["webhook_url"] == "https://product.test/webhooks");

        let response = app
            .clone()
            .oneshot(register(valid.clone(), now + 11, ADMIN_KID, &admin_key)?)
            .await?;
        ensure!(response.status() == StatusCode::OK);
        ensure!(response_json(response).await?["id"] == created["id"]);

        let mut changed = valid.clone();
        changed["webhook_url"] = json!("https://product.test/other");
        let response = app
            .clone()
            .oneshot(register(changed, now + 12, ADMIN_KID, &admin_key)?)
            .await?;
        ensure!(response.status() == StatusCode::CONFLICT);
        ensure!(response_json(response).await?["error"]["code"] == "conflict");

        let audit =
            sqlx::query("SELECT actor, action FROM audit WHERE subject = 'product:phala-cloud'")
                .fetch_all(&database.app_pool)
                .await?;
        ensure!(audit.len() == 1, "only the first registration is audited");
        ensure!(audit[0].try_get::<String, _>("actor")? == format!("admin:{ADMIN_KID}"));
        ensure!(audit[0].try_get::<String, _>("action")? == "product.issue");

        // The registered key authenticates product requests under the key id `{slug}/v1`.
        let response = app
            .oneshot(signed_request(
                Method::GET,
                "/v1/config",
                Vec::new(),
                PRODUCT_KID,
                &product_key,
                now,
            ))
            .await?;
        ensure!(response.status() == StatusCode::OK);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

/// `PUT /v1/admin/products/{slug}` replaces an issued product's key and webhook URL: a hard cut
/// from the old key to the new one under the route's key id, audited once with the replaced values.
#[tokio::test]
async fn admin_product_key_replacement() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let old_key = SigningKey::from_bytes(&[45; 32]);
        let new_key = SigningKey::from_bytes(&[46; 32]);
        let admin_key = SigningKey::from_bytes(&[47; 32]);
        let app = test_router(&database.app_pool, &admin_key);
        let now = Utc::now().timestamp();
        let update = |slug: &str, body: &Value, created: i64| -> Result<_> {
            Ok(signed_request(
                Method::PUT,
                &format!("/v1/admin/products/{slug}"),
                serde_json::to_vec(body)?,
                ADMIN_KID,
                &admin_key,
                created,
            ))
        };
        let register_account = |key: &SigningKey, created: i64| -> Result<_> {
            Ok(signed_request(
                Method::GET,
                &format!("/v1/config?request={created}"),
                Vec::new(),
                PRODUCT_KID,
                key,
                created,
            ))
        };
        let valid = json!({
            "public_key": public_key_base64(&new_key),
            "webhook_url": "https://product.test/rotated",
            "reason": "scheduled product key rotation",
        });

        let response = app
            .clone()
            .oneshot(update("phala-cloud", &valid, now - 1)?)
            .await?;
        ensure!(response.status() == StatusCode::NOT_FOUND, "not issued yet");
        let product = seed_product(&database.app_pool, "phala-cloud", &old_key).await?;

        let response = app
            .clone()
            .oneshot(signed_request(
                Method::PUT,
                "/v1/admin/products/phala-cloud",
                serde_json::to_vec(&valid)?,
                PRODUCT_KID,
                &old_key,
                now,
            ))
            .await?;
        ensure!(
            response.status() == StatusCode::UNAUTHORIZED,
            "only the admin key"
        );

        let mut invalid = Vec::new();
        for (field, value) in [
            ("public_key", json!("AAAA")),
            ("webhook_url", json!("ftp://product.test/rotated")),
            ("reason", json!(" ")),
        ] {
            let mut body = valid.clone();
            body[field] = value;
            invalid.push(("phala-cloud", body));
        }
        invalid.push(("unrouted", valid.clone()));
        for (offset, (slug, body)) in (1_i64..).zip(invalid) {
            let response = app
                .clone()
                .oneshot(update(slug, &body, now + offset)?)
                .await?;
            ensure!(
                response.status() == StatusCode::BAD_REQUEST,
                "{slug} {body} must be rejected"
            );
        }
        let response = app
            .clone()
            .oneshot(register_account(&old_key, now + 10)?)
            .await?;
        ensure!(
            response.status() == StatusCode::OK,
            "the old key works before"
        );

        for offset in [11, 12] {
            let response = app
                .clone()
                .oneshot(update("phala-cloud", &valid, now + offset)?)
                .await?;
            ensure!(response.status() == StatusCode::OK);
            let updated = response_json(response).await?;
            ensure!(updated["id"] == json!(product.id));
            ensure!(updated["public_key"] == valid["public_key"]);
            ensure!(updated["webhook_url"] == "https://product.test/rotated");
        }

        let response = app
            .clone()
            .oneshot(register_account(&old_key, now + 13)?)
            .await?;
        ensure!(
            response.status() == StatusCode::UNAUTHORIZED,
            "the old key is cut"
        );
        let response = app
            .clone()
            .oneshot(register_account(&new_key, now + 14)?)
            .await?;
        ensure!(response.status() == StatusCode::OK, "the new key verifies");

        let audit = sqlx::query(
            "SELECT actor, action, reason FROM audit WHERE subject = 'product:phala-cloud'",
        )
        .fetch_all(&database.app_pool)
        .await?;
        ensure!(
            audit.len() == 1,
            "a repeat with the stored values is not audited again"
        );
        ensure!(audit[0].try_get::<String, _>("actor")? == format!("admin:{ADMIN_KID}"));
        ensure!(audit[0].try_get::<String, _>("action")? == "product.update");
        let evidence: Value = serde_json::from_str(&audit[0].try_get::<String, _>("reason")?)?;
        ensure!(
            evidence["reason"] == "scheduled product key rotation",
            "{evidence}"
        );
        ensure!(
            evidence["replaced"]["public_key"] == json!(public_key_base64(&old_key)),
            "{evidence}"
        );
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn frozen_chain_refuses_quotes() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[31; 32]);
        let admin_key = SigningKey::from_bytes(&[32; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        seed::create_account(
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
        let lock_body = serde_json::to_vec(&json!({
            "account_id": "frozen-account", "amount": 1000, "currency": "usd",
            "chain_id": 1, "asset": "pha",
        }))?;
        let response = app
            .oneshot(signed_request(
                Method::POST,
                "/v1/quotes",
                lock_body,
                PRODUCT_KID,
                &product_key,
                now + 2,
            ))
            .await?;
        ensure!(response.status() == StatusCode::CONFLICT);
        ensure!(response_json(response).await?["error"]["code"] == "chain_frozen");
        // A refused creation leaves no lock or lock address behind.
        let leftovers: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM rate_locks) + (SELECT count(*) FROM addresses)",
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
        let product_key = SigningKey::from_bytes(&[33; 32]);
        let admin_key = SigningKey::from_bytes(&[34; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        seed::create_account(
            &database.app_pool,
            &NewAccount {
                id: Uuid::new_v4(),
                product_id: product.id,
                external_id: "lift-account".to_owned(),
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
        let admin = |method: Method, path: &str, body: Value, created: i64| -> Result<_> {
            Ok(signed_request(
                method,
                path,
                if body.is_null() { Vec::new() } else { serde_json::to_vec(&body)? },
                ADMIN_KID,
                &admin_key,
                created,
            ))
        };

        let response = app
            .clone()
            .oneshot(admin(Method::GET, "/v1/admin/report/daily", Value::Null, now)?)
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
            .oneshot(signed_request(
                Method::POST,
                lift,
                serde_json::to_vec(&reason)?,
                PRODUCT_KID,
                &product_key,
                now + 1,
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
            "SELECT actor, action, reason FROM audit WHERE subject = 'reconciliation_block:chain:1'",
        )
        .fetch_all(&database.app_pool)
        .await?;
        ensure!(audit.len() == 1, "only the first lift is audited");
        ensure!(audit[0].try_get::<String, _>("actor")? == format!("admin:{ADMIN_KID}"));
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
            .oneshot(signed_request(
                Method::POST,
                "/v1/quotes",
                serde_json::to_vec(&json!({
                    "account_id": "lift-account", "amount": 1000, "currency": "usd",
                    "chain_id": 1, "asset": "pha",
                }))?,
                PRODUCT_KID,
                &product_key,
                now + 6,
            ))
            .await?;
        ensure!(response.status() == StatusCode::SERVICE_UNAVAILABLE);
        let response = app
            .oneshot(admin(Method::GET, "/v1/admin/report/daily", Value::Null, now + 7)?)
            .await?;
        ensure!(response_json(response).await?["reconciliation_blocks"] == json!([]));
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

/// `POST /v1/admin/outbox/{event_id}/replay` requeues an existing event without touching its
/// payload; the operator finds the event id, `evt_…` or an older event's UUID, in the admin
/// deposit view.
#[tokio::test]
async fn admin_replay_requeues_a_delivered_event_once() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[35; 32]);
        let admin_key = SigningKey::from_bytes(&[36; 32]);
        let product = seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        let deposit = seed_other_tenant_deposit(&database.app_pool, product.id).await?;
        let legacy_event = Uuid::new_v4();
        sqlx::query(
            r#"
            INSERT INTO outbox (id, event_type, payload, next_attempt_at, delivered_at, format,
                                created_at)
            VALUES ($1, 'deposit.pending', $2, now() - interval '2 hours', now(), 1,
                    now() - interval '2 hours')
            "#,
        )
        .bind(legacy_event)
        .bind(json!({"product_id": product.id, "deposit_id": deposit}))
        .execute(&database.app_pool)
        .await?;
        let event_id = Uuid::new_v4();
        let payload = json!({"object": {"id": format!("dep_{}", deposit.simple())}});
        sqlx::query(
            r#"
            INSERT INTO outbox (id, event_type, payload, next_attempt_at, delivered_at,
                                product_id, object_type, object_id)
            VALUES ($1, 'deposit.credited', $2, now() - interval '1 hour', now(), $3, 'deposit',
                    $4)
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
        ensure!(events.as_array().map(Vec::len) == Some(2), "{events}");
        ensure!(events[0]["id"] == legacy_event.to_string());
        ensure!(events[1]["id"] == webhook_id);
        ensure!(events[1]["event_type"] == "deposit.credited");
        ensure!(!events[1]["delivered_at"].is_null());

        let replay = format!("/v1/admin/outbox/{webhook_id}/replay");
        let reason = serde_json::to_vec(&json!({"reason": "product lost the event"}))?;
        let response = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                &replay,
                reason.clone(),
                PRODUCT_KID,
                &product_key,
                now + 1,
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
            "SELECT payload, delivered_at IS NULL AND next_attempt_at <= now() AS due FROM outbox WHERE id = $1",
        )
        .bind(event_id)
        .fetch_one(&database.app_pool)
        .await?;
        ensure!(row.try_get::<bool, _>("due")?);
        ensure!(row.try_get::<Value, _>("payload")? == payload);
        let audit = sqlx::query("SELECT actor, action, reason FROM audit WHERE subject = $1")
            .bind(format!("event:{event_id}"))
            .fetch_all(&database.app_pool)
            .await?;
        ensure!(audit.len() == 1, "a repeat while the event is due is not audited again");
        ensure!(audit[0].try_get::<String, _>("actor")? == format!("admin:{ADMIN_KID}"));
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

    // The fixture route runs on chain 1 with operator key version 1.
    let dev = DevSigner::derive(&SecretKey32::new(seed), std::num::NonZeroU32::MIN);
    let settlement = dev.settlement_public_key().await?;
    let operator = AttestedOperator {
        chain_id: 1,
        key_version: std::num::NonZeroU32::MIN,
        address: dev.operator_address().await?,
    };
    ensure!(response["settlement_pubkey"] == hex::encode(settlement.0));
    ensure!(
        response["operators"]
            == json!([{
                "chain_id": 1,
                "operator_key_version": 1,
                "keyid": "operator/v1",
                "address": format!("{:#x}", operator.address),
            }])
    );
    ensure!(
        response["report_data"]
            == hex::encode(report_data(&[0, 1, 2, 3], &settlement, &[operator]))
    );
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
    }
}

/// Development attestor deriving every key from one seed, as `topup attest --dev` does.
#[cfg(feature = "dev-signer")]
struct DevHttpAttestor([u8; 32]);

#[cfg(feature = "dev-signer")]
impl Attestor for DevHttpAttestor {
    fn attest<'a>(
        &'a self,
        nonce: &'a [u8],
        operator_keys: &'a [OperatorKey],
    ) -> AttestationFuture<'a> {
        Box::pin(async move {
            let seed = SecretKey32::new(self.0);
            let public_key = DevSigner::derive(&seed, std::num::NonZeroU32::MIN)
                .settlement_public_key()
                .await
                .map_err(|_| AttestationError::Unavailable)?;
            let mut operators = Vec::with_capacity(operator_keys.len());
            for key in operator_keys {
                operators.push(AttestedOperator {
                    chain_id: key.chain_id,
                    key_version: key.key_version,
                    address: DevSigner::derive(&seed, key.key_version)
                        .operator_address()
                        .await
                        .map_err(|_| AttestationError::Unavailable)?,
                });
            }
            Ok(AttestationResponse {
                keyid: SETTLEMENT_KEY_DOMAIN.to_owned(),
                settlement_pubkey: hex::encode(public_key.0),
                operators: operators.iter().map(OperatorIdentity::from).collect(),
                report_data: hex::encode(report_data(nonce, &public_key, &operators)),
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

async fn seed_product(
    pool: &sqlx::PgPool,
    slug: &str,
    key: &SigningKey,
) -> Result<topup::db::Product> {
    Ok(seed::create_product(
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

async fn seed_other_tenant_deposit(pool: &sqlx::PgPool, product_id: Uuid) -> Result<Uuid> {
    let account = seed::create_account(
        pool,
        &NewAccount {
            id: Uuid::new_v4(),
            product_id,
            external_id: "other-account".to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    let address = seed::insert_address(
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

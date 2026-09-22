//! PostgreSQL-backed C10 rate-lock API and lifecycle tests.

mod support;

use std::sync::Arc;

use alloy_primitives::U256;
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use axum::body::{Body, to_bytes};
use axum::http::{Method, StatusCode};
use chrono::Utc;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use topup::api::{AppState, UnavailableAttestor, VerificationKey};
use topup::db::{NewAccount, NewProduct};
use topup::locks::pricing::ValidatedQuote;
use topup::locks::{self, QuoteProvider, RateLockError, RequestedAmount};
use topup_adapters::pricing::Observation;
use topup_core::money::{AtomicAmount, PRICE_SCALE, ScaledPrice};
use topup_core::route::RouteFile;
use topup_core::valuation::{SourceId, UnixSeconds};
use tower::ServiceExt;
use uuid::Uuid;

use support::{TestDatabase, public_key_base64, signed_request};

const PRODUCT_KID: &str = "phala-cloud/v1";
const ADMIN_KID: &str = "admin/v1";

struct FixedQuote;

#[async_trait]
impl QuoteProvider for FixedQuote {
    async fn quote(&self, _route: &RouteFile) -> Result<ValidatedQuote, Value> {
        Ok(ValidatedQuote {
            price: ScaledPrice::new(100_000_000, PRICE_SCALE).expect("fixed quote"),
            evidence: json!({
                "mode": "spot",
                "primary": Observation {
                    source: SourceId::new("test"),
                    price: ScaledPrice::new(100_000_000, PRICE_SCALE).expect("fixed quote"),
                    observed_at: UnixSeconds::new(
                        u64::try_from(Utc::now().timestamp()).expect("non-negative timestamp")
                    ),
                }.price.value().to_string()
            }),
        })
    }
}

#[tokio::test]
async fn api_is_idempotent_rate_limited_paused_tenant_safe_and_emits_eip681() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product_key = SigningKey::from_bytes(&[41; 32]);
        let other_key = SigningKey::from_bytes(&[42; 32]);
        let admin_key = SigningKey::from_bytes(&[43; 32]);
        let product =
            seed_product(&database.app_pool, "phala-cloud", PRODUCT_KID, &product_key).await?;
        let other = seed_product(&database.app_pool, "builder", "builder/v1", &other_key).await?;
        let account = seed_account(&database.app_pool, product.id, "account-rl").await?;
        let other_account = seed_account(&database.app_pool, other.id, "account-rl").await?;
        let mut route = test_route();
        route.rate_lock.max_creations_per_minute = 1;
        let mut other_route = route.clone();
        other_route.destination.product = other.slug.clone();
        other_route.route = "builder-ethereum-pha-usd".to_owned();
        let app = topup::api::router(AppState {
            pool: database.app_pool.clone(),
            routes: Arc::new(vec![route.clone(), other_route]),
            admin_key: VerificationKey::from_base64(
                ADMIN_KID.to_owned(),
                &public_key_base64(&admin_key),
            )
            .map_err(anyhow::Error::msg)?,
            attestor: Arc::new(UnavailableAttestor),
            rate_lock_quotes: Arc::new(FixedQuote),
        })
        .0;
        let path = "/v1/products/phala-cloud/accounts/account-rl/rate-locks";
        let body = serde_json::to_vec(&json!({
            "amount_atomic": "100",
            "product_lock_ref": "checkout-1"
        }))?;
        let now = Utc::now().timestamp();
        let created = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                path,
                body.clone(),
                PRODUCT_KID,
                &product_key,
                now,
            ))
            .await?;
        ensure!(created.status() == StatusCode::OK);
        let created = response_json(created).await?;
        ensure!(created["status"] == "open");
        ensure!(created["credit_minor"] == "100");
        ensure!(created["amount_atomic"] == "100");
        ensure!(
            created["eip681_uri"]
                == format!(
                    "ethereum:{:#x}@1/transfer?address={}&uint256=100",
                    route.asset.contract,
                    created["address"].as_str().context("address")?
                )
        );
        ensure!(created["salt_inputs"]["lock_ref"] == "checkout-1");

        let retried = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                path,
                body,
                PRODUCT_KID,
                &product_key,
                now + 1,
            ))
            .await?;
        ensure!(retried.status() == StatusCode::OK);
        ensure!(response_json(retried).await?["address"] == created["address"]);
        let lock_count: i64 = sqlx::query_scalar("SELECT count(*) FROM rate_locks")
            .fetch_one(&database.app_pool)
            .await?;
        ensure!(lock_count == 1);

        let limited = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                path,
                serde_json::to_vec(&json!({
                    "amount_atomic": "100",
                    "product_lock_ref": "checkout-2"
                }))?,
                PRODUCT_KID,
                &product_key,
                now + 2,
            ))
            .await?;
        ensure!(limited.status() == StatusCode::TOO_MANY_REQUESTS);
        ensure!(response_json(limited).await?["error"]["code"] == "rate_limited");

        let cross_tenant = app
            .clone()
            .oneshot(signed_request(
                Method::GET,
                "/v1/products/builder/accounts/account-rl/rate-locks/checkout-1",
                Vec::new(),
                "builder/v1",
                &other_key,
                now,
            ))
            .await?;
        ensure!(cross_tenant.status() == StatusCode::NOT_FOUND);

        topup::db::set_account_paused_scopes(
            &database.app_pool,
            account.id,
            &["quotes".to_owned()],
        )
        .await?;
        let paused = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                path,
                serde_json::to_vec(&json!({
                    "amount_atomic": "100",
                    "product_lock_ref": "checkout-3"
                }))?,
                PRODUCT_KID,
                &product_key,
                now + 3,
            ))
            .await?;
        ensure!(paused.status() == StatusCode::LOCKED);

        topup::db::set_account_paused_scopes(&database.app_pool, account.id, &[]).await?;
        topup::db::set_product_paused_scopes(
            &database.app_pool,
            product.id,
            &["quotes".to_owned()],
        )
        .await?;
        let product_paused = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                path,
                serde_json::to_vec(&json!({
                    "amount_atomic": "100",
                    "product_lock_ref": "checkout-4"
                }))?,
                PRODUCT_KID,
                &product_key,
                now + 4,
            ))
            .await?;
        ensure!(product_paused.status() == StatusCode::LOCKED);

        topup::db::set_product_paused_scopes(&database.app_pool, product.id, &[]).await?;
        sqlx::query("INSERT INTO route_pauses (route, paused_scopes) VALUES ($1, ARRAY['quotes'])")
            .bind(&route.route)
            .execute(&database.app_pool)
            .await?;
        let route_paused = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                path,
                serde_json::to_vec(&json!({
                    "amount_atomic": "100",
                    "product_lock_ref": "checkout-5"
                }))?,
                PRODUCT_KID,
                &product_key,
                now + 5,
            ))
            .await?;
        ensure!(route_paused.status() == StatusCode::LOCKED);

        let get = app
            .clone()
            .oneshot(signed_request(
                Method::GET,
                "/v1/products/phala-cloud/accounts/account-rl/rate-locks/checkout-1",
                Vec::new(),
                PRODUCT_KID,
                &product_key,
                now + 6,
            ))
            .await?;
        ensure!(get.status() == StatusCode::OK);
        ensure!(response_json(get).await?["status"] == "open");

        let cancelled = app
            .oneshot(signed_request(
                Method::DELETE,
                "/v1/products/phala-cloud/accounts/account-rl/rate-locks/checkout-1",
                Vec::new(),
                PRODUCT_KID,
                &product_key,
                now + 7,
            ))
            .await?;
        ensure!(cancelled.status() == StatusCode::OK);
        ensure!(response_json(cancelled).await?["status"] == "cancelled");
        let audit_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM audit WHERE action = 'cancel_rate_lock'")
                .fetch_one(&database.app_pool)
                .await?;
        ensure!(audit_count == 1);
        let _ = other_account;
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn usd_stated_amount_rounds_token_amount_up() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        struct ThreeDollarQuote;

        #[async_trait]
        impl QuoteProvider for ThreeDollarQuote {
            async fn quote(&self, _route: &RouteFile) -> Result<ValidatedQuote, Value> {
                Ok(ValidatedQuote {
                    price: ScaledPrice::new(300_000_000, PRICE_SCALE).expect("fixed quote"),
                    evidence: json!({"mode": "test"}),
                })
            }
        }

        let product = seed_product_without_key(&database.app_pool, "phala-cloud").await?;
        let account = seed_account(&database.app_pool, product.id, "round-up").await?;
        let mut route = test_route();
        route.asset.decimals = 2;
        let quotes: Arc<dyn QuoteProvider> = Arc::new(ThreeDollarQuote);
        let lock = locks::create(
            &database.app_pool,
            &quotes,
            &product,
            &account,
            &route,
            "round-up-1",
            RequestedAmount::Minor(topup_core::money::MinorAmount::new(1)),
        )
        .await?;
        ensure!(lock.amount_atomic.value() == U256::from(34_u64));
        ensure!(lock.credit_minor.value() == 1);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn account_product_global_caps_and_expiry_release_are_atomic() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let first = seed_product_without_key(&database.app_pool, "phala-cloud").await?;
        let second = seed_product_without_key(&database.app_pool, "builder").await?;
        let first_account = seed_account(&database.app_pool, first.id, "first").await?;
        let second_account = seed_account(&database.app_pool, first.id, "second").await?;
        let other_account = seed_account(&database.app_pool, second.id, "other").await?;
        let quotes: Arc<dyn QuoteProvider> = Arc::new(FixedQuote);

        let mut account_route = test_route();
        account_route.rate_lock.max_open_minor.account = 100;
        account_route.rate_lock.max_open_minor.product = 1_000;
        account_route.rate_lock.max_open_minor.global = 1_000;
        let first_lock = locks::create(
            &database.app_pool,
            &quotes,
            &first,
            &first_account,
            &account_route,
            "account-cap-1",
            RequestedAmount::Atomic(AtomicAmount::new(U256::from(100_u64))),
        )
        .await?;
        ensure!(matches!(
            locks::create(
                &database.app_pool,
                &quotes,
                &first,
                &first_account,
                &account_route,
                "account-cap-2",
                RequestedAmount::Atomic(AtomicAmount::new(U256::from(1_u64))),
            )
            .await,
            Err(RateLockError::ExposureCap("account"))
        ));
        locks::cancel(&database.app_pool, &first, &first_account, "account-cap-1").await?;

        let mut product_route = account_route.clone();
        product_route.rate_lock.max_open_minor.account = 1_000;
        product_route.rate_lock.max_open_minor.product = 100;
        locks::create(
            &database.app_pool,
            &quotes,
            &first,
            &first_account,
            &product_route,
            "product-cap-1",
            RequestedAmount::Atomic(AtomicAmount::new(U256::from(100_u64))),
        )
        .await?;
        ensure!(matches!(
            locks::create(
                &database.app_pool,
                &quotes,
                &first,
                &second_account,
                &product_route,
                "product-cap-2",
                RequestedAmount::Atomic(AtomicAmount::new(U256::from(1_u64))),
            )
            .await,
            Err(RateLockError::ExposureCap("product"))
        ));
        locks::cancel(&database.app_pool, &first, &first_account, "product-cap-1").await?;

        let mut global_route = account_route.clone();
        global_route.rate_lock.max_open_minor.account = 1_000;
        global_route.rate_lock.max_open_minor.product = 1_000;
        global_route.rate_lock.max_open_minor.global = 100;
        let expiring = locks::create(
            &database.app_pool,
            &quotes,
            &first,
            &first_account,
            &global_route,
            "global-cap-1",
            RequestedAmount::Atomic(AtomicAmount::new(U256::from(100_u64))),
        )
        .await?;
        ensure!(matches!(
            locks::create(
                &database.app_pool,
                &quotes,
                &second,
                &other_account,
                &global_route,
                "global-cap-2",
                RequestedAmount::Atomic(AtomicAmount::new(U256::from(1_u64))),
            )
            .await,
            Err(RateLockError::ExposureCap("global"))
        ));
        sqlx::query(
            "UPDATE rate_locks SET expires_at = now() - interval '1 second' WHERE address_id = $1",
        )
        .bind(expiring.address_id)
        .execute(&database.app_pool)
        .await?;
        ensure!(locks::expire_once(&database.app_pool).await? == 1);
        let event: Value =
            sqlx::query_scalar("SELECT payload FROM outbox WHERE event_type = 'rate_lock.expired'")
                .fetch_one(&database.app_pool)
                .await?;
        ensure!(event["product_id"] == first.id.to_string());
        let global_open: String = sqlx::query_scalar(
            "SELECT open_minor::text FROM lock_exposure WHERE scope_key = 'global'",
        )
        .fetch_one(&database.app_pool)
        .await?;
        ensure!(global_open == "0");
        ensure!(first_lock.status.code() == "open");
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

fn test_route() -> RouteFile {
    let mut route: RouteFile =
        serde_saphyr::from_str(include_str!("fixtures/phala-cloud-pha.yaml"))
            .expect("route fixture");
    route.asset.decimals = 0;
    route.destination.unit_decimals = 0;
    route.rate_lock.spread_bps = topup_core::money::Bps::new(0).expect("zero bps");
    route.rate_lock.max_creations_per_minute = 100;
    route.screening.min_deposit_atomic = AtomicAmount::new(U256::from(1_u64));
    route.screening.max_deposit_atomic = AtomicAmount::new(U256::from(1_000_000_u64));
    route.screening.min_credit_minor = 1;
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

async fn seed_product_without_key(pool: &sqlx::PgPool, slug: &str) -> Result<topup::db::Product> {
    let key = SigningKey::from_bytes(&[51; 32]);
    seed_product(pool, slug, &format!("{slug}/v1"), &key).await
}

async fn seed_account(
    pool: &sqlx::PgPool,
    product_id: Uuid,
    external_id: &str,
) -> Result<topup::db::Account> {
    Ok(topup::db::create_account(
        pool,
        &NewAccount {
            id: Uuid::new_v4(),
            product_id,
            external_id: external_id.to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?)
}

async fn response_json(response: axum::response::Response) -> Result<Value> {
    let bytes = to_bytes(response.into_body(), 1_048_576).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[allow(dead_code)]
fn empty_body() -> Body {
    Body::empty()
}

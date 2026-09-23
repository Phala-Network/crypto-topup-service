//! PostgreSQL-backed C10 rate-lock API and lifecycle tests.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use axum::body::{Body, to_bytes};
use axum::http::{Method, StatusCode};
use chrono::Utc;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use topup::api::{AppState, PublicOrigin, VerificationKey};
use topup::db::{NewAccount, NewProduct};
use topup::locks::pricing::ValidatedQuote;
use topup::locks::{self, QuoteProvider, RateLockError, RequestedAmount};
use topup::reconciler::{CheckName, Reconciler, ReconciliationError, SettlementLookup};
use topup_adapters::attestation::DstackAttestor;
use topup_adapters::pricing::Observation;
use topup_adapters::settlement::http::SettlementAnswer;
use topup_core::deposit::{DepositState, RejectReason};
use topup_core::money::{AtomicAmount, PRICE_SCALE, ScaledPrice};
use topup_core::route::RouteFile;
use topup_core::valuation::{SourceId, UnixSeconds};
use tower::ServiceExt;
use uuid::Uuid;

use support::{TEST_ORIGIN, TestDatabase, public_key_base64, signed_request};

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
        let product = seed_product(&database.app_pool, "phala-cloud", &product_key).await?;
        let other = seed_product(&database.app_pool, "builder", &other_key).await?;
        let account = seed_account(&database.app_pool, product.id, "account-rl").await?;
        let other_account = seed_account(&database.app_pool, other.id, "account-rl").await?;
        let mut route = test_route();
        route.rate_lock.max_creations_per_minute = 1;
        let mut other_route = route.clone();
        other_route.destination.product = other.slug.clone();
        other_route.route = "builder-ethereum-pha-usd".to_owned();
        other_route.destination.product_kid = "builder/v1".to_owned();
        let app = topup::api::router(AppState {
            pool: database.app_pool.clone(),
            routes: Arc::new(vec![route.clone(), other_route]),
            admin_key: VerificationKey::from_base64(
                ADMIN_KID.to_owned(),
                &public_key_base64(&admin_key),
            )
            .map_err(anyhow::Error::msg)?,
            public_origin: PublicOrigin::parse(TEST_ORIGIN)?,
            attestor: Arc::new(DstackAttestor::new()),
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

        let mismatched = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                path,
                serde_json::to_vec(&json!({
                    "amount_atomic": "200",
                    "product_lock_ref": "checkout-1"
                }))?,
                PRODUCT_KID,
                &product_key,
                now + 1,
            ))
            .await?;
        ensure!(mismatched.status() == StatusCode::CONFLICT);
        ensure!(response_json(mismatched).await?["error"]["code"] == "idempotency_mismatch");

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
        let cross_tenant_cancel = app
            .clone()
            .oneshot(signed_request(
                Method::DELETE,
                "/v1/products/builder/accounts/account-rl/rate-locks/checkout-1",
                Vec::new(),
                "builder/v1",
                &other_key,
                now + 1,
            ))
            .await?;
        ensure!(cross_tenant_cancel.status() == StatusCode::NOT_FOUND);
        ensure!(lock_status_by_ref(&database.app_pool, account.id, "checkout-1").await? == "open");
        let other_locks: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM addresses WHERE account_id = $1 AND kind = 'lock'",
        )
        .bind(other_account.id)
        .fetch_one(&database.app_pool)
        .await?;
        ensure!(other_locks == 0);

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
        // A replay creates nothing, so the pause does not hide the lock the product showed.
        let paused_replay = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                path,
                serde_json::to_vec(&json!({
                    "amount_atomic": "100",
                    "product_lock_ref": "checkout-1"
                }))?,
                PRODUCT_KID,
                &product_key,
                now + 10,
            ))
            .await?;
        ensure!(paused_replay.status() == StatusCode::OK);
        ensure!(response_json(paused_replay).await?["address"] == created["address"]);
        let paused_mismatch = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                path,
                serde_json::to_vec(&json!({
                    "amount_atomic": "200",
                    "product_lock_ref": "checkout-1"
                }))?,
                PRODUCT_KID,
                &product_key,
                now + 11,
            ))
            .await?;
        ensure!(paused_mismatch.status() == StatusCode::CONFLICT);

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
            .clone()
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
        ensure!(
            lock_status_by_ref(&database.app_pool, account.id, "checkout-1").await? == "cancelled"
        );

        // A lock whose single-use address already received funds is no longer unpaid.
        sqlx::query("DELETE FROM route_pauses WHERE route = $1")
            .bind(&route.route)
            .execute(&database.app_pool)
            .await?;
        sqlx::query("UPDATE rate_locks SET created_at = now() - interval '2 minutes'")
            .execute(&database.app_pool)
            .await?;
        let paid = app
            .clone()
            .oneshot(signed_request(
                Method::POST,
                path,
                serde_json::to_vec(&json!({
                    "amount_atomic": "100",
                    "product_lock_ref": "checkout-6"
                }))?,
                PRODUCT_KID,
                &product_key,
                now + 8,
            ))
            .await?;
        ensure!(paid.status() == StatusCode::OK);
        let paid_address_id: Uuid = sqlx::query_scalar(
            "SELECT id FROM addresses WHERE account_id = $1 AND lock_ref = 'checkout-6'",
        )
        .bind(account.id)
        .fetch_one(&database.app_pool)
        .await?;
        insert_rejected_deposit(&database.app_pool, &route, account.id, paid_address_id).await?;
        let refused = app
            .oneshot(signed_request(
                Method::DELETE,
                "/v1/products/phala-cloud/accounts/account-rl/rate-locks/checkout-6",
                Vec::new(),
                PRODUCT_KID,
                &product_key,
                now + 9,
            ))
            .await?;
        ensure!(refused.status() == StatusCode::CONFLICT);
        ensure!(response_json(refused).await?["error"]["code"] == "pending_payment");
        ensure!(lock_status_by_ref(&database.app_pool, account.id, "checkout-6").await? == "open");
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
        finalize_chain_past_now(&database.app_pool).await?;
        ensure!(locks::expire_once(&database.app_pool).await? == 1);
        let event: Value =
            sqlx::query_scalar("SELECT payload FROM outbox WHERE event_type = 'rate_lock.expired'")
                .fetch_one(&database.app_pool)
                .await?;
        ensure!(event["product_id"] == first.id.to_string());
        ensure!(event["external_id"] == "first");
        ensure!(event.get("account_id").is_none());
        ensure!(event["product_lock_ref"] == "global-cap-1");
        ensure!(event["address"] == format!("{:#x}", expiring.address));
        ensure!(event["chain_id"] == 1);
        ensure!(event["amount_atomic"] == "100");
        ensure!(event["credit_minor"] == "100");
        let global_open: String = sqlx::query_scalar(
            "SELECT open_minor::text FROM lock_exposure WHERE scope_key = 'global'",
        )
        .fetch_one(&database.app_pool)
        .await?;
        ensure!(global_open == "0");
        let first_status: String =
            sqlx::query_scalar("SELECT status FROM rate_locks WHERE address_id = $1")
                .bind(first_lock.address_id)
                .fetch_one(&database.app_pool)
                .await?;
        ensure!(first_status == "cancelled");
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn unpaid_lock_expires_only_once_the_finalized_chain_passes_its_window() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product = seed_product_without_key(&database.app_pool, "phala-cloud").await?;
        let account = seed_account(&database.app_pool, product.id, "unpaid").await?;
        let quotes: Arc<dyn QuoteProvider> = Arc::new(FixedQuote);
        let lock = create_lock(
            &database,
            &quotes,
            &product,
            &account,
            &test_route(),
            "unpaid-1",
        )
        .await?;
        let expires_at = Utc::now() - chrono::Duration::minutes(5);
        sqlx::query("UPDATE rate_locks SET expires_at = $2 WHERE address_id = $1")
            .bind(lock.address_id)
            .bind(expires_at)
            .execute(&database.app_pool)
            .await?;
        let account_key = format!("account:{}", account.id);

        // The wall-clock window has closed, but the scanner has not committed a finalized block
        // past it (never, then stalled at the deadline itself): the lock stays open and reserved.
        ensure!(locks::expire_once(&database.app_pool).await? == 0);
        set_finalized_time(&database.app_pool, expires_at).await?;
        ensure!(locks::expire_once(&database.app_pool).await? == 0);
        ensure!(lock_status_by_ref(&database.app_pool, account.id, "unpaid-1").await? == "open");
        ensure!(matches!(
            locks::cancel(&database.app_pool, &product, &account, "unpaid-1").await,
            Err(RateLockError::WindowClosed)
        ));
        ensure!(exposure(&database.app_pool, &account_key).await? == 100);
        let events: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM outbox WHERE event_type = 'rate_lock.expired'",
        )
        .fetch_one(&database.app_pool)
        .await?;
        ensure!(events == 0);

        set_finalized_time(
            &database.app_pool,
            expires_at + chrono::Duration::seconds(12),
        )
        .await?;
        ensure!(locks::expire_once(&database.app_pool).await? == 1);
        ensure!(lock_status_by_ref(&database.app_pool, account.id, "unpaid-1").await? == "expired");
        ensure!(matches!(
            locks::cancel(&database.app_pool, &product, &account, "unpaid-1").await,
            Err(RateLockError::NotOpen)
        ));
        ensure!(exposure(&database.app_pool, &account_key).await? == 0);
        let events: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM outbox WHERE event_type = 'rate_lock.expired'",
        )
        .fetch_one(&database.app_pool)
        .await?;
        ensure!(events == 1);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn another_chains_cursor_never_expires_a_lock() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product = seed_product_without_key(&database.app_pool, "phala-cloud").await?;
        let account = seed_account(&database.app_pool, product.id, "cross-chain").await?;
        let quotes: Arc<dyn QuoteProvider> = Arc::new(FixedQuote);
        let lock = create_lock(
            &database,
            &quotes,
            &product,
            &account,
            &test_route(),
            "chain-a",
        )
        .await?;
        let expires_at = Utc::now() - chrono::Duration::minutes(5);
        sqlx::query("UPDATE rate_locks SET expires_at = $2 WHERE address_id = $1")
            .bind(lock.address_id)
            .bind(expires_at)
            .execute(&database.app_pool)
            .await?;

        // The lock is on chain 1, whose cursor is still at the deadline; chain 2 is far past it.
        set_finalized_time(&database.app_pool, expires_at).await?;
        sqlx::query(
            "INSERT INTO cursors (chain_id, scanned_block, scanned_block_time) VALUES (2, 0, $1)",
        )
        .bind(Utc::now())
        .execute(&database.app_pool)
        .await?;
        ensure!(locks::expire_once(&database.app_pool).await? == 0);
        ensure!(lock_status_by_ref(&database.app_pool, account.id, "chain-a").await? == "open");
        ensure!(exposure(&database.app_pool, &format!("account:{}", account.id)).await? == 100);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn new_lock_addresses_start_scanning_at_the_chain_cursor() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product = seed_product_without_key(&database.app_pool, "phala-cloud").await?;
        let account = seed_account(&database.app_pool, product.id, "cursor").await?;
        let route = test_route();
        let quotes: Arc<dyn QuoteProvider> = Arc::new(FixedQuote);
        let before_cursor =
            create_lock(&database, &quotes, &product, &account, &route, "c-1").await?;
        sqlx::query("INSERT INTO cursors (chain_id, scanned_block) VALUES (1, 1234)")
            .execute(&database.app_pool)
            .await?;
        let after_cursor =
            create_lock(&database, &quotes, &product, &account, &route, "c-2").await?;
        for (address_id, expected) in [
            (before_cursor.address_id, 0),
            (after_cursor.address_id, 1234),
        ] {
            let (created_block, backfilled): (i64, bool) =
                sqlx::query_as("SELECT created_block, backfilled FROM addresses WHERE id = $1")
                    .bind(address_id)
                    .fetch_one(&database.app_pool)
                    .await?;
            ensure!(created_block == expected);
            ensure!(!backfilled);
        }
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn rate_limited_creation_does_not_fetch_a_price() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        struct CountingQuote(AtomicUsize);

        #[async_trait]
        impl QuoteProvider for CountingQuote {
            async fn quote(&self, route: &RouteFile) -> Result<ValidatedQuote, Value> {
                self.0.fetch_add(1, Ordering::SeqCst);
                FixedQuote.quote(route).await
            }
        }

        let product = seed_product_without_key(&database.app_pool, "phala-cloud").await?;
        let account = seed_account(&database.app_pool, product.id, "limited").await?;
        let mut route = test_route();
        route.rate_lock.max_creations_per_minute = 1;
        let counter = Arc::new(CountingQuote(AtomicUsize::new(0)));
        let quotes: Arc<dyn QuoteProvider> = counter.clone();
        create_lock(&database, &quotes, &product, &account, &route, "limited-1").await?;
        ensure!(matches!(
            locks::create(
                &database.app_pool,
                &quotes,
                &product,
                &account,
                &route,
                "limited-2",
                RequestedAmount::Atomic(AtomicAmount::new(U256::from(100_u64))),
            )
            .await,
            Err(RateLockError::RateLimited)
        ));
        ensure!(counter.0.load(Ordering::SeqCst) == 1);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn cancel_refuses_a_lock_whose_address_received_any_deposit() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product = seed_product_without_key(&database.app_pool, "phala-cloud").await?;
        let account = seed_account(&database.app_pool, product.id, "paid").await?;
        let route = test_route();
        let quotes: Arc<dyn QuoteProvider> = Arc::new(FixedQuote);
        let lock = create_lock(&database, &quotes, &product, &account, &route, "paid-1").await?;
        insert_rejected_deposit(&database.app_pool, &route, account.id, lock.address_id).await?;
        ensure!(matches!(
            locks::cancel(&database.app_pool, &product, &account, "paid-1").await,
            Err(RateLockError::PendingPayment)
        ));
        ensure!(lock_status_by_ref(&database.app_pool, account.id, "paid-1").await? == "open");
        ensure!(exposure(&database.app_pool, &format!("account:{}", account.id)).await? == 100);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn database_collector_exports_product_and_global_lock_exposure() -> Result<()> {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    // The current-thread test runtime polls the spawned collector on this thread.
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product = seed_product_without_key(&database.app_pool, "phala-cloud").await?;
        let account = seed_account(&database.app_pool, product.id, "exposed").await?;
        let route = test_route();
        let quotes: Arc<dyn QuoteProvider> = Arc::new(FixedQuote);
        create_lock(&database, &quotes, &product, &account, &route, "exposed-1").await?;

        let caps = topup::observability::LockExposureCaps::from_routes(std::slice::from_ref(&route));
        let cancellation = tokio_util::sync::CancellationToken::new();
        let collector = tokio::spawn(topup::observability::collect_database_metrics(
            database.app_pool.clone(),
            caps,
            cancellation.clone(),
        ));
        let product_exposure = format!(
            "topup_open_lock_exposure_minor{{scope=\"product\",id=\"{}\",producer_enabled=\"true\"}} 100",
            route.destination.product
        );
        let mut rendered = String::new();
        for _ in 0..200 {
            rendered = handle.render();
            if rendered.contains(&product_exposure) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        cancellation.cancel();
        collector.await?;

        ensure!(rendered.contains(&product_exposure), "{rendered}");
        ensure!(
            rendered.contains(
                "topup_open_lock_exposure_minor{scope=\"global\",id=\"global\",producer_enabled=\"true\"} 100"
            ),
            "{rendered}"
        );
        ensure!(rendered.contains(&format!(
            "topup_open_lock_exposure_cap_minor{{scope=\"global\",id=\"global\",producer_enabled=\"true\"}} {}",
            route.rate_lock.max_open_minor.global
        )));
        ensure!(!rendered.contains("scope=\"account\""), "{rendered}");
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn concurrent_creations_never_exceed_the_shared_exposure_cap() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        const ATTEMPTS: usize = 12;
        let product = seed_product_without_key(&database.app_pool, "phala-cloud").await?;
        let mut route = test_route();
        route.rate_lock.max_open_minor.account = 1_000;
        route.rate_lock.max_open_minor.product = 500;
        route.rate_lock.max_open_minor.global = 1_000;
        let route = Arc::new(route);
        let quotes: Arc<dyn QuoteProvider> = Arc::new(FixedQuote);
        let mut tasks = tokio::task::JoinSet::new();
        for index in 0..ATTEMPTS {
            let account =
                seed_account(&database.app_pool, product.id, &format!("racer-{index}")).await?;
            let (pool, quotes, product, route) = (
                database.app_pool.clone(),
                Arc::clone(&quotes),
                product.clone(),
                Arc::clone(&route),
            );
            tasks.spawn(async move {
                locks::create(
                    &pool,
                    &quotes,
                    &product,
                    &account,
                    &route,
                    "race",
                    RequestedAmount::Atomic(AtomicAmount::new(U256::from(100_u64))),
                )
                .await
            });
        }
        let mut successes = 0_u64;
        while let Some(joined) = tasks.join_next().await {
            match joined? {
                Ok(_) => successes += 1,
                Err(RateLockError::ExposureCap("product")) => {}
                Err(error) => anyhow::bail!("unexpected creation failure: {error}"),
            }
        }
        ensure!(successes * 100 <= route.rate_lock.max_open_minor.product);
        ensure!(successes == 5);
        let open: String = sqlx::query_scalar(
            "SELECT coalesce(sum(credit_minor), 0)::text FROM rate_locks WHERE status = 'open'",
        )
        .fetch_one(&database.app_pool)
        .await?;
        let open = open.parse::<u64>()?;
        ensure!(open == successes * 100);
        ensure!(exposure(&database.app_pool, &format!("product:{}", product.id)).await? == open);
        ensure!(exposure(&database.app_pool, "global").await? == open);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expiring_two_accounts_does_not_deadlock_with_a_concurrent_creation() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product = seed_product_without_key(&database.app_pool, "phala-cloud").await?;
        let first = seed_account(&database.app_pool, product.id, "expiring-a").await?;
        let second = seed_account(&database.app_pool, product.id, "expiring-b").await?;
        let route = test_route();
        let quotes: Arc<dyn QuoteProvider> = Arc::new(FixedQuote);
        let first_lock = create_lock(&database, &quotes, &product, &first, &route, "a-1").await?;
        let second_lock = create_lock(&database, &quotes, &product, &second, &route, "b-1").await?;
        for (address_id, seconds) in [(first_lock.address_id, 20), (second_lock.address_id, 10)] {
            sqlx::query(
                "UPDATE rate_locks SET expires_at = now() - make_interval(secs => $2) WHERE address_id = $1",
            )
            .bind(address_id)
            .bind(f64::from(seconds))
            .execute(&database.app_pool)
            .await?;
        }
        finalize_chain_past_now(&database.app_pool).await?;

        // Hold the second account's scope row and then request `global`, exactly as a creation
        // for that account does, while the expiry batch releases both accounts.
        let second_key = format!("account:{}", second.id);
        let mut creation = database.app_pool.begin().await?;
        sqlx::query("SELECT open_minor FROM lock_exposure WHERE scope_key = $1 FOR UPDATE")
            .bind(&second_key)
            .execute(&mut *creation)
            .await?;
        let pool = database.app_pool.clone();
        let expiry = tokio::spawn(async move { locks::expire_once(&pool).await });
        wait_for_lock_waiter(&database.app_pool).await?;
        sqlx::query("SELECT open_minor FROM lock_exposure WHERE scope_key = 'global' FOR UPDATE")
            .execute(&mut *creation)
            .await
            .context("creation-ordered lock must not deadlock with expiry")?;
        creation.commit().await?;
        ensure!(expiry.await?? == 2);

        // A real creation racing a second expiry batch also completes.
        let third_lock = create_lock(&database, &quotes, &product, &first, &route, "a-2").await?;
        sqlx::query("UPDATE rate_locks SET expires_at = now() - interval '1 second' WHERE address_id = $1")
            .bind(third_lock.address_id)
            .execute(&database.app_pool)
            .await?;
        finalize_chain_past_now(&database.app_pool).await?;
        let (expired, created) = tokio::join!(
            locks::expire_once(&database.app_pool),
            create_lock(&database, &quotes, &product, &second, &route, "b-2"),
        );
        ensure!(expired? == 1);
        created?;

        for key in [
            format!("account:{}", first.id),
            "global".to_owned(),
            format!("product:{}", product.id),
        ] {
            let expected = if key == format!("account:{}", first.id) { 0 } else { 100 };
            ensure!(exposure(&database.app_pool, &key).await? == expected);
        }
        ensure!(exposure(&database.app_pool, &second_key).await? == 100);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn disabled_route_still_replays_an_existing_lock() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product = seed_product_without_key(&database.app_pool, "phala-cloud").await?;
        let account = seed_account(&database.app_pool, product.id, "disabled").await?;
        let mut route = test_route();
        let quotes: Arc<dyn QuoteProvider> = Arc::new(FixedQuote);
        let lock = create_lock(&database, &quotes, &product, &account, &route, "d-1").await?;
        route.rate_lock.enabled = false;
        let replayed = create_lock(&database, &quotes, &product, &account, &route, "d-1").await?;
        ensure!(replayed.address_id == lock.address_id && replayed.address == lock.address);
        ensure!(matches!(
            create_lock(&database, &quotes, &product, &account, &route, "d-2").await,
            Err(error) if matches!(error.downcast_ref::<RateLockError>(), Some(RateLockError::Disabled))
        ));
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_waits_for_an_uncommitted_deposit_to_the_lock_address() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product = seed_product_without_key(&database.app_pool, "phala-cloud").await?;
        let account = seed_account(&database.app_pool, product.id, "racing").await?;
        let route = test_route();
        let quotes: Arc<dyn QuoteProvider> = Arc::new(FixedQuote);
        let lock = create_lock(&database, &quotes, &product, &account, &route, "race-1").await?;

        // A scanner transaction has inserted the payment but not committed yet.
        let mut scanner = database.app_pool.begin().await?;
        insert_deposit_in(&mut scanner, account.id, lock.address_id, 0x81).await?;
        let pool = database.app_pool.clone();
        let (cancel_product, cancel_account) = (product.clone(), account.clone());
        let cancel = tokio::spawn(async move {
            locks::cancel(&pool, &cancel_product, &cancel_account, "race-1").await
        });
        for _ in 0..200 {
            if cancel.is_finished() || lock_waiters(&database.app_pool).await? > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        scanner.commit().await?;
        ensure!(matches!(cancel.await?, Err(RateLockError::PendingPayment)));
        ensure!(lock_status_by_ref(&database.app_pool, account.id, "race-1").await? == "open");
        ensure!(exposure(&database.app_pool, &format!("account:{}", account.id)).await? == 100);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn failing_expiry_scans_are_counted_and_recover_after_exposure_repair() -> Result<()> {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    // The current-thread test runtime polls the spawned worker on this thread.
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let product = seed_product_without_key(&database.app_pool, "phala-cloud").await?;
        let account = seed_account(&database.app_pool, product.id, "drifted").await?;
        let route = test_route();
        let quotes: Arc<dyn QuoteProvider> = Arc::new(FixedQuote);
        let lock = create_lock(&database, &quotes, &product, &account, &route, "drift-1").await?;
        sqlx::query(
            "UPDATE rate_locks SET expires_at = now() - interval '1 second' WHERE address_id = $1",
        )
        .bind(lock.address_id)
        .execute(&database.app_pool)
        .await?;
        sqlx::query("UPDATE lock_exposure SET open_minor = 40 WHERE scope_key = 'global'")
            .execute(&database.app_pool)
            .await?;
        finalize_chain_past_now(&database.app_pool).await?;

        let cancellation = tokio_util::sync::CancellationToken::new();
        let worker = locks::ExpiryWorker::new(
            database.app_pool.clone(),
            std::time::Duration::from_millis(10),
        );
        let worker_cancellation = cancellation.clone();
        let running = tokio::spawn(async move { worker.run(worker_cancellation).await });
        let failures = || {
            handle
                .render()
                .lines()
                .find_map(|line| {
                    line.strip_prefix(
                        "topup_lock_expiry_failures_total{producer_enabled=\"true\"} ",
                    )
                    .and_then(|value| value.parse::<u64>().ok())
                })
                .unwrap_or(0)
        };
        for _ in 0..400 {
            if failures() >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        ensure!(
            failures() >= 2,
            "expiry never failed on the drifted counter"
        );
        ensure!(lock_status_by_ref(&database.app_pool, account.id, "drift-1").await? == "open");

        let repairs = locks::repair_exposure(&database.app_pool).await?;
        ensure!(
            repairs
                .iter()
                .map(|repair| (
                    repair.scope_key.as_str(),
                    repair.before_minor,
                    repair.after_minor
                ))
                .eq([("global", 40, 100)]),
            "{repairs:?}"
        );
        for _ in 0..400 {
            if lock_status_by_ref(&database.app_pool, account.id, "drift-1").await? == "expired" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        cancellation.cancel();
        running.await?;
        ensure!(lock_status_by_ref(&database.app_pool, account.id, "drift-1").await? == "expired");
        ensure!(exposure(&database.app_pool, "global").await? == 0);
        ensure!(handle.render().contains(
            "topup_lock_exposure_drift_total{scope=\"global\",producer_enabled=\"true\"} 1"
        ));
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exposure_repair_converges_under_concurrent_create_consume_cancel_and_expire() -> Result<()>
{
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        const ACCOUNTS: usize = 4;
        let product = seed_product_without_key(&database.app_pool, "phala-cloud").await?;
        let mut route = test_route();
        route.rate_lock.max_open_minor.account = 1_000_000;
        route.rate_lock.max_open_minor.product = 1_000_000;
        route.rate_lock.max_open_minor.global = 1_000_000;
        let route = Arc::new(route);
        let quotes: Arc<dyn QuoteProvider> = Arc::new(FixedQuote);
        let mut accounts = Vec::new();
        for index in 0..ACCOUNTS {
            let account =
                seed_account(&database.app_pool, product.id, &format!("c-{index}")).await?;
            for lock in ["consume", "cancel", "expire", "keep"] {
                let lock =
                    create_lock(&database, &quotes, &product, &account, &route, lock).await?;
                if lock.lock_ref == "expire" {
                    sqlx::query("UPDATE rate_locks SET expires_at = now() WHERE address_id = $1")
                        .bind(lock.address_id)
                        .execute(&database.app_pool)
                        .await?;
                }
            }
            accounts.push(account);
        }

        // Drift in every direction: too low, too high, and a missing row.
        sqlx::query("UPDATE lock_exposure SET open_minor = 50 WHERE scope_key = 'global'")
            .execute(&database.app_pool)
            .await?;
        sqlx::query("UPDATE lock_exposure SET open_minor = open_minor + 700 WHERE scope_key = $1")
            .bind(format!("product:{}", product.id))
            .execute(&database.app_pool)
            .await?;
        sqlx::query("UPDATE lock_exposure SET open_minor = 0 WHERE scope_key = $1")
            .bind(format!("account:{}", accounts[1].id))
            .execute(&database.app_pool)
            .await?;
        sqlx::query("DELETE FROM lock_exposure WHERE scope_key = $1")
            .bind(format!("account:{}", accounts[2].id))
            .execute(&database.app_pool)
            .await?;
        ensure!(!drifted_scopes(&database.app_pool).await?.is_empty());
        finalize_chain_past_now(&database.app_pool).await?;

        let done = tokio_util::sync::CancellationToken::new();
        let repairer = {
            let (pool, done) = (database.app_pool.clone(), done.clone());
            tokio::spawn(async move {
                let mut repaired = 0_usize;
                while !done.is_cancelled() {
                    repaired += locks::repair_exposure(&pool).await?.len();
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                anyhow::Ok(repaired)
            })
        };
        let mut tasks = tokio::task::JoinSet::new();
        for (index, account) in accounts.iter().cloned().enumerate() {
            let (pool, quotes, product, route) = (
                database.app_pool.clone(),
                Arc::clone(&quotes),
                product.clone(),
                Arc::clone(&route),
            );
            tasks.spawn(async move {
                for round in 0..3 {
                    let lock_ref = format!("new-{round}");
                    retry_invariant(|| {
                        locks::create(
                            &pool,
                            &quotes,
                            &product,
                            &account,
                            &route,
                            &lock_ref,
                            RequestedAmount::Atomic(AtomicAmount::new(U256::from(100_u64))),
                        )
                    })
                    .await?;
                }
                retry_invariant(|| locks::cancel(&pool, &product, &account, "cancel")).await?;
                consume_lock(&pool, account.id, "consume", u8::try_from(index)?).await?;
                anyhow::Ok(())
            });
        }
        {
            let pool = database.app_pool.clone();
            tasks.spawn(async move {
                let mut expired = 0;
                while expired < ACCOUNTS as u64 {
                    expired += retry_invariant(|| locks::expire_once(&pool)).await?;
                }
                anyhow::Ok(())
            });
        }
        let joined = tokio::time::timeout(std::time::Duration::from_secs(60), async {
            while let Some(task) = tasks.join_next().await {
                task??;
            }
            anyhow::Ok(())
        })
        .await;
        done.cancel();
        let repaired = repairer.await??;
        joined.context("lifecycle operations never converged")??;

        ensure!(repaired > 0);
        ensure!(drifted_scopes(&database.app_pool).await?.is_empty());
        ensure!(locks::repair_exposure(&database.app_pool).await?.is_empty());
        let open = open_reserved(&database.app_pool).await?;
        // Per account: "keep" plus three new locks remain reserved.
        ensure!(open == 4 * 100 * ACCOUNTS as u64, "{open}");
        ensure!(exposure(&database.app_pool, "global").await? == open);
        ensure!(exposure(&database.app_pool, &format!("product:{}", product.id)).await? == open);
        for account in &accounts {
            ensure!(exposure(&database.app_pool, &format!("account:{}", account.id)).await? == 400);
        }
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn every_lock_exposure_repair_leaves_a_finding_and_an_audit_row() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        struct NoSettlement;

        #[async_trait]
        impl SettlementLookup for NoSettlement {
            async fn get_by_key(
                &self,
                _settlement_url: &str,
                _key: &str,
            ) -> Result<Option<SettlementAnswer>, ReconciliationError> {
                Ok(None)
            }
        }

        let product = seed_product_without_key(&database.app_pool, "phala-cloud").await?;
        let account = seed_account(&database.app_pool, product.id, "reconciled").await?;
        let route = test_route();
        let quotes: Arc<dyn QuoteProvider> = Arc::new(FixedQuote);
        create_lock(&database, &quotes, &product, &account, &route, "r-1").await?;
        let product_key = format!("product:{}", product.id);

        let reconciler = Reconciler::with_dependencies(
            database.app_pool.clone(),
            vec![route],
            std::collections::BTreeMap::new(),
            Arc::new(NoSettlement),
        )?;
        // The same drift twice: every repair must leave its own finding and audit row.
        for _ in 0..2 {
            sqlx::query("UPDATE lock_exposure SET open_minor = 350 WHERE scope_key = $1")
                .bind(&product_key)
                .execute(&database.app_pool)
                .await?;
            let report = reconciler.run_once().await?;
            ensure!(!report.failed_checks.contains(&CheckName::LockExposure));
            let repaired = report
                .findings
                .iter()
                .filter(|finding| finding.check == CheckName::LockExposure)
                .collect::<Vec<_>>();
            ensure!(repaired.len() == 1, "{repaired:?}");
            ensure!(repaired[0].repair_applied);
            ensure!(repaired[0].subjects.get("scope_key") == Some(&product_key));
            ensure!(repaired[0].observed == json!({"open_minor": "350"}));
            ensure!(repaired[0].expected == json!({"open_minor": "100"}));
            ensure!(exposure(&database.app_pool, &product_key).await? == 100);
        }
        let findings: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM reconciliation_findings WHERE check_name = 'lock_exposure'",
        )
        .fetch_one(&database.app_pool)
        .await?;
        ensure!(findings == 2);
        let audits: Vec<String> = sqlx::query_scalar(
            "SELECT reason FROM audit WHERE action = 'repair_lock_exposure' AND subject = $1",
        )
        .bind(format!("lock_exposure:{product_key}"))
        .fetch_all(&database.app_pool)
        .await?;
        ensure!(audits.len() == 2, "{audits:?}");
        for audit in audits {
            let audit: Value = serde_json::from_str(&audit)?;
            ensure!(audit["before_minor"] == "350");
            ensure!(audit["after_minor"] == "100");
        }
        ensure!(reconciler.check(CheckName::LockExposure).await?.is_empty());
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

/// Commits the test chain's scanner cursor through a finalized block just past every lock already
/// overdue by wall clock, as the scanner does on reaching the finalized head.
async fn finalize_chain_past_now(pool: &sqlx::PgPool) -> Result<()> {
    set_finalized_time(pool, Utc::now() + chrono::Duration::seconds(1)).await
}

async fn set_finalized_time(pool: &sqlx::PgPool, time: chrono::DateTime<Utc>) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO cursors (chain_id, scanned_block, scanned_block_time)
        VALUES (1, 0, $1)
        ON CONFLICT (chain_id) DO UPDATE SET scanned_block_time = EXCLUDED.scanned_block_time
        "#,
    )
    .bind(time)
    .execute(pool)
    .await?;
    Ok(())
}

async fn retry_invariant<T, F, Fut>(mut operation: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, RateLockError>>,
{
    // A drifted counter fails a release with the invariant error until the repair runs.
    for _ in 0..2_000 {
        match operation().await {
            Err(RateLockError::DatabaseInvariant) => {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            other => return Ok(other?),
        }
    }
    anyhow::bail!("operation kept failing on a drifted exposure counter")
}

/// Consumes a lock the way the confirm step does: a leased deposit transition with consumption.
async fn consume_lock(
    pool: &sqlx::PgPool,
    account_id: Uuid,
    lock_ref: &str,
    number: u8,
) -> Result<()> {
    let address_id: Uuid =
        sqlx::query_scalar("SELECT id FROM addresses WHERE account_id = $1 AND lock_ref = $2")
            .bind(account_id)
            .bind(lock_ref)
            .fetch_one(pool)
            .await?;
    let mut transaction = pool.begin().await?;
    let deposit_id = insert_deposit_in(&mut transaction, account_id, address_id, number).await?;
    let lease_token = Uuid::new_v4();
    sqlx::query(
        "UPDATE deposits SET state = 'detected', reason = NULL, lease_token = $2, lease_until = now() + interval '5 minutes' WHERE id = $1",
    )
    .bind(deposit_id)
    .bind(lease_token)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    let advance = topup_core::deposit::next(
        DepositState::Detected,
        &topup_core::deposit::StepOutcome::Advance,
    )?;
    let effects = topup::db::TransitionEffects {
        lock_consumption: Some(topup::db::LockConsumption {
            address_id,
            idempotent: false,
        }),
        ..topup::db::TransitionEffects::default()
    };
    for _ in 0..2_000 {
        let mut transaction = pool.begin().await?;
        let applied = topup::db::apply_transition(
            &mut transaction,
            deposit_id,
            DepositState::Detected,
            lease_token,
            topup::db::TransitionUpdate {
                transition: advance,
                rejection_reason: None,
                attempt: 0,
                next_attempt_at: Utc::now(),
            },
            topup::db::TransitionWrites {
                evidence: &json!({"test": "consume"}),
                effects: &effects,
                outbox_events: &[],
            },
        )
        .await;
        match applied {
            Ok(topup::db::ApplyTransitionResult::Applied) => {
                transaction.commit().await?;
                let status: String =
                    sqlx::query_scalar("SELECT status FROM rate_locks WHERE address_id = $1")
                        .bind(address_id)
                        .fetch_one(pool)
                        .await?;
                ensure!(status == "consumed");
                return Ok(());
            }
            Ok(other) => anyhow::bail!("consumption was not applied: {other:?}"),
            // A drifted counter fails the release until the repair runs.
            Err(_) => {
                transaction.rollback().await?;
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        }
    }
    anyhow::bail!("consumption kept failing on a drifted exposure counter")
}

async fn insert_deposit_in(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    account_id: Uuid,
    address_id: Uuid,
    number: u8,
) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO deposits (
            id, chain_id, tx_hash, log_index, block_number, block_hash, block_time,
            address_id, account_id, asset_contract, from_address, amount_atomic, state, reason,
            next_attempt_at
        )
        VALUES ($1, 1, $2, 0, 10, $3, now(), $4, $5, $6, $7, 100, 'rejected',
                'unsupported_asset', now())
        "#,
    )
    .bind(id)
    .bind(format!("{:#x}", B256::repeat_byte(number)))
    .bind(format!("{:#x}", B256::repeat_byte(number.wrapping_add(1))))
    .bind(address_id)
    .bind(account_id)
    .bind(format!("{:#x}", Address::repeat_byte(0x73)))
    .bind(format!("{:#x}", Address::repeat_byte(0x74)))
    .execute(&mut **transaction)
    .await?;
    Ok(id)
}

async fn lock_waiters(pool: &sqlx::PgPool) -> Result<i64> {
    Ok(sqlx::query_scalar(
        r#"
        SELECT count(*)
        FROM pg_stat_activity
        WHERE datname = current_database()
          AND wait_event_type = 'Lock'
          AND pid <> pg_backend_pid()
        "#,
    )
    .fetch_one(pool)
    .await?)
}

async fn open_reserved(pool: &sqlx::PgPool) -> Result<u64> {
    let open: String = sqlx::query_scalar(
        r#"
        SELECT coalesce(sum(credit_minor), 0)::text
        FROM rate_locks
        WHERE status = 'open' AND consumed_by IS NULL AND exposure_reserved
        "#,
    )
    .fetch_one(pool)
    .await?;
    Ok(open.parse()?)
}

/// Scope keys whose counter differs from the per-scope sum of open reserved lock credit.
async fn drifted_scopes(pool: &sqlx::PgPool) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar(
        r#"
        WITH expected AS (
            SELECT scope.scope_key, sum(rate_lock.credit_minor) AS open_minor
            FROM rate_locks AS rate_lock
            JOIN addresses AS address ON address.id = rate_lock.address_id
            JOIN accounts AS account ON account.id = address.account_id
            CROSS JOIN LATERAL (
                VALUES ('account:' || account.id::text), ('product:' || account.product_id::text),
                       ('global')
            ) AS scope (scope_key)
            WHERE rate_lock.status = 'open' AND rate_lock.consumed_by IS NULL
              AND rate_lock.exposure_reserved
            GROUP BY scope.scope_key
        )
        SELECT coalesce(expected.scope_key, counter.scope_key)
        FROM expected
        FULL JOIN lock_exposure AS counter ON counter.scope_key = expected.scope_key
        WHERE coalesce(expected.open_minor, 0) <> coalesce(counter.open_minor, 0)
        "#,
    )
    .fetch_all(pool)
    .await?)
}

async fn create_lock(
    database: &TestDatabase,
    quotes: &Arc<dyn QuoteProvider>,
    product: &topup::db::Product,
    account: &topup::db::Account,
    route: &RouteFile,
    lock_ref: &str,
) -> Result<locks::RateLock> {
    Ok(locks::create(
        &database.app_pool,
        quotes,
        product,
        account,
        route,
        lock_ref,
        RequestedAmount::Atomic(AtomicAmount::new(U256::from(100_u64))),
    )
    .await?)
}

async fn wait_for_lock_waiter(pool: &sqlx::PgPool) -> Result<()> {
    for _ in 0..200 {
        let waiting: i64 = sqlx::query_scalar(
            r#"
            SELECT count(*)
            FROM pg_stat_activity
            WHERE datname = current_database()
              AND wait_event_type = 'Lock'
              AND pid <> pg_backend_pid()
            "#,
        )
        .fetch_one(pool)
        .await?;
        if waiting > 0 {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    anyhow::bail!("expiry never waited for the held scope row")
}

async fn exposure(pool: &sqlx::PgPool, key: &str) -> Result<u64> {
    let open: Option<String> =
        sqlx::query_scalar("SELECT open_minor::text FROM lock_exposure WHERE scope_key = $1")
            .bind(key)
            .fetch_optional(pool)
            .await?;
    Ok(open.as_deref().unwrap_or("0").parse()?)
}

async fn lock_status_by_ref(
    pool: &sqlx::PgPool,
    account_id: Uuid,
    lock_ref: &str,
) -> Result<String> {
    Ok(sqlx::query_scalar(
        r#"
        SELECT rate_lock.status
        FROM rate_locks AS rate_lock
        JOIN addresses AS address ON address.id = rate_lock.address_id
        WHERE address.account_id = $1 AND address.lock_ref = $2
        "#,
    )
    .bind(account_id)
    .bind(lock_ref)
    .fetch_one(pool)
    .await?)
}

async fn insert_rejected_deposit(
    pool: &sqlx::PgPool,
    route: &RouteFile,
    account_id: Uuid,
    address_id: Uuid,
) -> Result<()> {
    let inserted = topup::db::insert_deposit(
        pool,
        &topup::db::NewDeposit {
            chain_id: route.chain.chain_id,
            tx_hash: B256::repeat_byte(0x71),
            log_index: 0,
            block_number: 10,
            block_hash: B256::repeat_byte(0x72),
            block_time: Utc::now(),
            address_id,
            account_id,
            route: None,
            route_version: None,
            asset_contract: Address::repeat_byte(0x73),
            from_address: Address::repeat_byte(0x74),
            amount_atomic: AtomicAmount::new(U256::from(100_u64)),
            state: DepositState::Rejected,
            reason: Some(RejectReason::UnsupportedAsset),
            next_attempt_at: Utc::now(),
        },
    )
    .await?;
    ensure!(inserted);
    Ok(())
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

async fn seed_product_without_key(pool: &sqlx::PgPool, slug: &str) -> Result<topup::db::Product> {
    let key = SigningKey::from_bytes(&[51; 32]);
    seed_product(pool, slug, &key).await
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

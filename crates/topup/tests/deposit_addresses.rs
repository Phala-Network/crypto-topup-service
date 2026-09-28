//! Deposit addresses on PostgreSQL (docs/design/multi-tenant.md "Deposit addresses"): creation
//! is idempotent per customer, chain, asset, and mode; rotation retires and issues; the address
//! is the one the merchant recomputes; caps, the rotation limit, and tenancy hold; a deposit to
//! one names it.

mod support;

use std::str::FromStr;
use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use axum::Router;
use axum::body::to_bytes;
use axum::http::{Method, StatusCode};
use chrono::Utc;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use topup::api::{AppState, PublicOrigin, VerificationKey};
use topup::db::{self, NewDeposit};
use topup::deposit_addresses::MAX_ROTATIONS_PER_HOUR;
use topup_adapters::attestation::DstackAttestor;
use topup_core::address::{deposit_address_salt, forwarder_address};
use topup_core::deposit::{DepositState, RejectReason};
use topup_core::identity::deposit_id;
use topup_core::money::AtomicAmount;
use topup_core::route::RouteFile;
use tower::ServiceExt;
use uuid::Uuid;

use support::seed::{self, NewAccount};
use support::{TEST_ORIGIN, merchant_request, public_key_base64, with_database};

const TEST_CHAIN: u64 = 11_155_111;

#[tokio::test]
async fn create_is_idempotent_per_customer_chain_asset_and_mode() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let pool = &database.app_pool;
            let fixture = Fixture::new(pool).await?;
            let first = fixture.create(&fixture.live_key, "team-42", 1).await?;
            ensure!(first["object"] == "deposit_address" && first["status"] == "active");
            ensure!(first["livemode"] == true && first["client_reference_id"] == "team-42");
            ensure!(first["chain_id"] == 1 && first["asset"] == "pha");
            ensure!(first["version"] == 1 && first["retired_at"].is_null());
            let id = first["id"].as_str().context("id")?;
            ensure!(id.starts_with("da_") && id.len() == 35);
            // EIP-681 without an amount: the payer chooses it.
            ensure!(
                first["payment_uri"]
                    == format!(
                        "ethereum:{:#x}@1/transfer?address={}",
                        fixture.live_route.asset.contract,
                        first["address"].as_str().context("address")?
                    )
            );

            // The same request returns the same address, without an idempotency key.
            let again = fixture.create(&fixture.live_key, "team-42", 1).await?;
            ensure!(again == first, "{again} != {first}");
            // Another customer, and the same customer in test mode, get their own.
            let other = fixture.create(&fixture.live_key, "team-43", 1).await?;
            ensure!(other["address"] != first["address"]);
            let test = fixture
                .create(&fixture.test_key, "team-42", TEST_CHAIN)
                .await?;
            ensure!(test["livemode"] == false && test["address"] != first["address"]);
            // No test route on the live chain, and no live route on the test chain.
            let (status, body) = fixture
                .request(
                    Method::POST,
                    "/v1/deposit_addresses",
                    &fixture.test_key,
                    json!({"client_reference_id": "team-42", "chain_id": 1, "asset": "pha"}),
                )
                .await?;
            ensure!(status == StatusCode::BAD_REQUEST, "{body}");
            ensure!(body["error"]["param"] == "asset");
            let (status, body) = fixture
                .request(
                    Method::POST,
                    "/v1/deposit_addresses",
                    &fixture.live_key,
                    json!({"client_reference_id": "", "chain_id": 1, "asset": "pha"}),
                )
                .await?;
            ensure!(status == StatusCode::BAD_REQUEST, "{body}");
            ensure!(body["error"]["param"] == "client_reference_id");

            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM deposit_addresses")
                .fetch_one(pool)
                .await?;
            ensure!(count == 3);
            // New addresses are scanned from the chain's committed cursor, like a quote's.
            let owners: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM addresses \
                 WHERE deposit_address_id IS NOT NULL AND quote_id IS NULL",
            )
            .fetch_one(pool)
            .await?;
            ensure!(owners == 3);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn the_address_is_the_one_the_merchant_recomputes() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let fixture = Fixture::new(&database.app_pool).await?;
            let created = fixture.create(&fixture.live_key, "客户 42", 1).await?;
            let rotated = fixture.rotate(&fixture.live_key, &created).await?;
            for (object, version) in [(&created, 1), (&rotated, 2)] {
                ensure!(object["version"] == version);
                let salt = deposit_address_salt(
                    &fixture.account.public_id,
                    true,
                    "客户 42",
                    1,
                    "pha",
                    version,
                );
                ensure!(object["salt"] == format!("{salt:#x}"));
                let contracts = &fixture.live_route.chain.contracts;
                ensure!(object["treasury"] == format!("{:#x}", contracts.treasury));
                let address = forwarder_address(
                    contracts.forwarder_factory,
                    contracts.implementation,
                    contracts.treasury,
                    salt,
                );
                ensure!(object["address"] == format!("{address:#x}"));
            }
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn rotate_retires_the_address_and_issues_the_next_version() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let pool = &database.app_pool;
            let fixture = Fixture::new(pool).await?;
            let first = fixture.create(&fixture.live_key, "team-42", 1).await?;
            let first_id = first["id"].as_str().context("id")?;
            let second = fixture.rotate(&fixture.live_key, &first).await?;
            ensure!(second["status"] == "active" && second["version"] == 2);
            ensure!(second["id"] != first["id"] && second["address"] != first["address"]);

            let (status, retired) = fixture
                .request(
                    Method::GET,
                    &format!("/v1/deposit_addresses/{first_id}"),
                    &fixture.live_key,
                    Value::Null,
                )
                .await?;
            ensure!(status == StatusCode::OK);
            ensure!(retired["status"] == "retired" && retired["retired_at"].is_i64());
            ensure!(retired["address"] == first["address"]);
            // Creation now returns the new address.
            ensure!(fixture.create(&fixture.live_key, "team-42", 1).await? == second);
            // A retired address cannot be rotated again.
            let (status, body) = fixture
                .request(
                    Method::POST,
                    &format!("/v1/deposit_addresses/{first_id}/rotate"),
                    &fixture.live_key,
                    Value::Null,
                )
                .await?;
            ensure!(status == StatusCode::CONFLICT, "{body}");
            ensure!(body["error"]["code"] == "deposit_address_retired");
            let audited: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM audit WHERE action = 'deposit_address.rotate' \
                 AND subject = $1",
            )
            .bind(format!("deposit_address:{first_id}"))
            .fetch_one(pool)
            .await?;
            ensure!(audited == 1);

            // Lists filter by customer and status, newest first.
            fixture.create(&fixture.live_key, "team-43", 1).await?;
            let (_, list) = fixture
                .request(
                    Method::GET,
                    "/v1/deposit_addresses?client_reference_id=team-42",
                    &fixture.live_key,
                    Value::Null,
                )
                .await?;
            ensure!(list["object"] == "list" && list["has_more"] == false);
            let data = list["data"].as_array().context("data")?;
            ensure!(data.len() == 2 && data[0]["id"] == second["id"], "{list}");
            let (_, list) = fixture
                .request(
                    Method::GET,
                    "/v1/deposit_addresses?status=active&limit=1",
                    &fixture.live_key,
                    Value::Null,
                )
                .await?;
            ensure!(
                list["has_more"] == true && list["data"][0]["client_reference_id"] == "team-43"
            );
            let after = list["data"][0]["id"].as_str().context("id")?;
            let (_, list) = fixture
                .request(
                    Method::GET,
                    &format!("/v1/deposit_addresses?status=active&starting_after={after}"),
                    &fixture.live_key,
                    Value::Null,
                )
                .await?;
            ensure!(list["has_more"] == false && list["data"][0]["id"] == second["id"]);
            let (status, _) = fixture
                .request(
                    Method::GET,
                    "/v1/deposit_addresses?status=open",
                    &fixture.live_key,
                    Value::Null,
                )
                .await?;
            ensure!(status == StatusCode::BAD_REQUEST);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn other_accounts_and_modes_see_no_deposit_address() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let pool = &database.app_pool;
            let fixture = Fixture::new(pool).await?;
            let created = fixture.create(&fixture.live_key, "team-42", 1).await?;
            let id = created["id"].as_str().context("id")?;
            let stranger = seed::create_account(pool, &NewAccount::named("stranger")).await?;
            let stranger_key = seed::create_api_key(pool, stranger.id, true).await?;
            for key in [&stranger_key, &fixture.test_key] {
                for (method, path) in [
                    (Method::GET, format!("/v1/deposit_addresses/{id}")),
                    (Method::POST, format!("/v1/deposit_addresses/{id}/rotate")),
                ] {
                    let (status, body) = fixture.request(method, &path, key, Value::Null).await?;
                    ensure!(status == StatusCode::NOT_FOUND, "{path}: {body}");
                }
                let (_, list) = fixture
                    .request(Method::GET, "/v1/deposit_addresses", key, Value::Null)
                    .await?;
                ensure!(list["data"] == json!([]), "{list}");
                let (status, _) = fixture
                    .request(
                        Method::GET,
                        &format!("/v1/deposit_addresses?starting_after={id}"),
                        key,
                        Value::Null,
                    )
                    .await?;
                ensure!(status == StatusCode::BAD_REQUEST);
            }
            // The same client_reference_id at another account is another customer.
            let theirs = fixture.create(&stranger_key, "team-42", 1).await?;
            ensure!(theirs["address"] != created["address"]);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn caps_rotation_limit_and_pauses_bound_issuance() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let pool = &database.app_pool;
            let fixture = Fixture::new(pool).await?;
            sqlx::query(
                "INSERT INTO account_limits (account_id, livemode, max_open_quotes, \
                 max_open_minor_account, max_open_minor_customer, max_active_deposit_addresses) \
                 VALUES ($1, true, 1000, 0, 0, 2)",
            )
            .bind(fixture.account.id)
            .execute(pool)
            .await?;
            let first = fixture.create(&fixture.live_key, "team-1", 1).await?;
            fixture.create(&fixture.live_key, "team-2", 1).await?;
            let (status, body) = fixture
                .request(
                    Method::POST,
                    "/v1/deposit_addresses",
                    &fixture.live_key,
                    json!({"client_reference_id": "team-3", "chain_id": 1, "asset": "pha"}),
                )
                .await?;
            ensure!(status == StatusCode::CONFLICT, "{body}");
            ensure!(body["error"]["code"] == "deposit_address_cap_exceeded");
            // At the cap, existing addresses are still returned and rotated: rotation keeps the
            // count of active addresses.
            ensure!(fixture.create(&fixture.live_key, "team-1", 1).await? == first);
            // Test mode has its own default cap.
            fixture
                .create(&fixture.test_key, "team-3", TEST_CHAIN)
                .await?;

            let mut current = first;
            for _ in 0..MAX_ROTATIONS_PER_HOUR {
                current = fixture.rotate(&fixture.live_key, &current).await?;
            }
            let id = current["id"].as_str().context("id")?;
            let (status, body) = fixture
                .request(
                    Method::POST,
                    &format!("/v1/deposit_addresses/{id}/rotate"),
                    &fixture.live_key,
                    Value::Null,
                )
                .await?;
            ensure!(status == StatusCode::TOO_MANY_REQUESTS, "{body}");
            ensure!(body["error"]["code"] == "rate_limit");
            // Another customer's rotations are not limited by this one's.
            let second = fixture.create(&fixture.live_key, "team-2", 1).await?;
            fixture.rotate(&fixture.live_key, &second).await?;

            // A `quotes` pause stops new addresses; reads keep working.
            seed::set_account_paused_scopes(pool, fixture.account.id, &["quotes".to_owned()])
                .await?;
            let (status, body) = fixture
                .request(
                    Method::POST,
                    "/v1/deposit_addresses",
                    &fixture.live_key,
                    json!({"client_reference_id": "team-1", "chain_id": 1, "asset": "pha"}),
                )
                .await?;
            ensure!(status == StatusCode::CONFLICT && body["error"]["code"] == "paused");
            let (status, _) = fixture
                .request(
                    Method::GET,
                    &format!("/v1/deposit_addresses/{id}"),
                    &fixture.live_key,
                    Value::Null,
                )
                .await?;
            ensure!(status == StatusCode::OK);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn metadata_is_set_merged_carried_by_rotation_and_scoped() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let pool = &database.app_pool;
            let fixture = Fixture::new(pool).await?;
            let create = |metadata: Value| {
                fixture.request(
                    Method::POST,
                    "/v1/deposit_addresses",
                    &fixture.live_key,
                    json!({"client_reference_id": "team-42", "chain_id": 1, "asset": "pha",
                           "metadata": metadata}),
                )
            };
            let (status, first) = create(json!({"team": "42", "plan": "pro"})).await?;
            ensure!(status == StatusCode::OK, "{first}");
            ensure!(first["metadata"] == json!({"team": "42", "plan": "pro"}));
            // Creation returns the active address with the request's metadata merged in.
            let (_, again) = create(json!({"plan": "", "region": "eu"})).await?;
            ensure!(again["id"] == first["id"]);
            ensure!(
                again["metadata"] == json!({"team": "42", "region": "eu"}),
                "{again}"
            );
            let (status, body) = create(json!({"bad[key]": "x"})).await?;
            ensure!(status == StatusCode::BAD_REQUEST, "{body}");
            ensure!(body["error"]["param"] == "metadata[bad[key]]");

            let id = first["id"].as_str().context("id")?;
            let (status, updated) = fixture
                .request(
                    Method::POST,
                    &format!("/v1/deposit_addresses/{id}"),
                    &fixture.live_key,
                    json!({"metadata": {"region": "", "tier": "1"}}),
                )
                .await?;
            ensure!(status == StatusCode::OK, "{updated}");
            ensure!(updated["metadata"] == json!({"team": "42", "tier": "1"}));
            // Rotation carries the metadata to the next version.
            let rotated = fixture.rotate(&fixture.live_key, &updated).await?;
            ensure!(rotated["metadata"] == updated["metadata"]);
            // A retired address is still updatable; `""` unsets every key.
            let (status, cleared) = fixture
                .request(
                    Method::POST,
                    &format!("/v1/deposit_addresses/{id}"),
                    &fixture.live_key,
                    json!({"metadata": ""}),
                )
                .await?;
            ensure!(
                status == StatusCode::OK && cleared["metadata"] == json!({}),
                "{cleared}"
            );
            ensure!(cleared["status"] == "retired");
            // Another mode or account cannot update it.
            let (status, _) = fixture
                .request(
                    Method::POST,
                    &format!("/v1/deposit_addresses/{id}"),
                    &fixture.test_key,
                    json!({"metadata": {"x": "y"}}),
                )
                .await?;
            ensure!(status == StatusCode::NOT_FOUND);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_changed_treasury_replaces_the_active_address_on_creation() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let pool = &database.app_pool;
            let fixture = Fixture::new(pool).await?;
            let old = fixture.create(&fixture.live_key, "team-42", 1).await?;
            let mut route = fixture.live_route.clone();
            route.chain.contracts.treasury = Address::repeat_byte(0x7e);
            let changed = Fixture {
                app: app(pool, vec![route.clone(), fixture.test_route.clone()])?,
                live_route: route,
                ..fixture
            };
            let new = changed.create(&changed.live_key, "team-42", 1).await?;
            ensure!(new["version"] == 2 && new["address"] != old["address"]);
            ensure!(new["treasury"] == format!("{:#x}", Address::repeat_byte(0x7e)));
            let old_id = old["id"].as_str().context("id")?;
            let (_, old) = changed
                .request(
                    Method::GET,
                    &format!("/v1/deposit_addresses/{old_id}"),
                    &changed.live_key,
                    Value::Null,
                )
                .await?;
            // The retired address keeps the treasury it was issued for.
            ensure!(old["status"] == "retired");
            ensure!(old["treasury"] == format!("{:#x}", changed.account_treasury()));
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn deposits_to_active_and_retired_addresses_name_the_deposit_address() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let pool = &database.app_pool;
            let fixture = Fixture::new(pool).await?;
            let first = fixture.create(&fixture.live_key, "team-42", 1).await?;
            let second = fixture.rotate(&fixture.live_key, &first).await?;
            for (number, object) in [(1_u8, &first), (2, &second)] {
                // Each deposit starts with a copy of its address's metadata.
                let id = object["id"].as_str().context("id")?;
                let (status, _) = fixture
                    .request(
                        Method::POST,
                        &format!("/v1/deposit_addresses/{id}"),
                        &fixture.live_key,
                        json!({"metadata": {"version": number.to_string()}}),
                    )
                    .await?;
                ensure!(status == StatusCode::OK);
                let da = topup::ids::parse(
                    topup::ids::DEPOSIT_ADDRESS,
                    object["id"].as_str().context("id")?,
                )
                .context("da id")?;
                let address_id: Uuid =
                    sqlx::query_scalar("SELECT id FROM addresses WHERE deposit_address_id = $1")
                        .bind(da)
                        .fetch_one(pool)
                        .await?;
                let tx_hash = B256::repeat_byte(number);
                ensure!(
                    db::insert_deposit(
                        pool,
                        &NewDeposit {
                            chain_id: 1,
                            tx_hash,
                            receipt_log_index: 0,
                            log_index: 0,
                            block_number: 10,
                            block_hash: B256::repeat_byte(0xbb),
                            block_time: Utc::now(),
                            address_id,
                            route: None,
                            route_version: None,
                            asset_contract: Address::repeat_byte(0x73),
                            from_address: Address::repeat_byte(0x74),
                            amount_atomic: AtomicAmount::new(U256::from(5_u64)),
                            state: DepositState::Rejected,
                            reason: Some(RejectReason::UnsupportedAsset),
                            next_attempt_at: Utc::now(),
                            tx_from: Address::repeat_byte(0x74),
                            tx_nonce: 0,
                            is_final: false,
                        },
                    )
                    .await?
                );
                let deposit = topup::ids::format(topup::ids::DEPOSIT, deposit_id(1, tx_hash, 0));
                let (status, body) = fixture
                    .request(
                        Method::GET,
                        &format!("/v1/deposits/{deposit}"),
                        &fixture.live_key,
                        Value::Null,
                    )
                    .await?;
                ensure!(status == StatusCode::OK, "{body}");
                ensure!(body["deposit_address"] == object["id"] && body["quote"].is_null());
                ensure!(
                    body["metadata"] == json!({"version": number.to_string()}),
                    "{body}"
                );
                // The copy is independent of the address afterwards.
                let (status, _) = fixture
                    .request(
                        Method::POST,
                        &format!("/v1/deposit_addresses/{id}"),
                        &fixture.live_key,
                        json!({"metadata": ""}),
                    )
                    .await?;
                ensure!(status == StatusCode::OK);
                let (_, body) = fixture
                    .request(
                        Method::GET,
                        &format!("/v1/deposits/{deposit}"),
                        &fixture.live_key,
                        Value::Null,
                    )
                    .await?;
                ensure!(body["metadata"] == json!({"version": number.to_string()}));
                ensure!(body["account_id"] == "team-42");
                let (_, list) = fixture
                    .request(
                        Method::GET,
                        &format!(
                            "/v1/deposits?deposit_address={}",
                            object["id"].as_str().context("id")?
                        ),
                        &fixture.live_key,
                        Value::Null,
                    )
                    .await?;
                let data = list["data"].as_array().context("data")?;
                ensure!(data.len() == 1 && data[0]["id"] == deposit, "{list}");
            }
            Ok(())
        })
    })
    .await
}

struct Fixture {
    app: Router,
    account: db::Account,
    live_key: String,
    test_key: String,
    live_route: RouteFile,
    test_route: RouteFile,
}

impl Fixture {
    async fn new(pool: &sqlx::PgPool) -> Result<Self> {
        let account = seed::create_account(pool, &NewAccount::named("merchant")).await?;
        let live_key = seed::create_api_key(pool, account.id, true).await?;
        let test_key = seed::create_api_key(pool, account.id, false).await?;
        let live_route: RouteFile =
            serde_saphyr::from_str(include_str!("fixtures/phala-cloud-pha.yaml"))?;
        let test_route: RouteFile = serde_saphyr::from_str(
            &include_str!("fixtures/phala-cloud-pha.yaml")
                .replace(
                    "route: phala-cloud-ethereum-pha-usd",
                    "route: phala-cloud-sepolia-pha",
                )
                .replace("chain_id: 1", &format!("chain_id: {TEST_CHAIN}"))
                .replace("livemode: true", "livemode: false"),
        )?;
        Ok(Self {
            app: app(pool, vec![live_route.clone(), test_route.clone()])?,
            account,
            live_key,
            test_key,
            live_route,
            test_route,
        })
    }

    fn account_treasury(&self) -> Address {
        Address::from_str("0x0000000000000000000000000000000000007EA5").expect("treasury")
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        key: &str,
        body: Value,
    ) -> Result<(StatusCode, Value)> {
        let body = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&body)?
        };
        let response = self
            .app
            .clone()
            .oneshot(merchant_request(method, path, body, key))
            .await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1_048_576).await?;
        Ok((status, serde_json::from_slice(&bytes)?))
    }

    async fn create(&self, key: &str, customer: &str, chain_id: u64) -> Result<Value> {
        let (status, body) = self
            .request(
                Method::POST,
                "/v1/deposit_addresses",
                key,
                json!({"client_reference_id": customer, "chain_id": chain_id, "asset": "pha"}),
            )
            .await?;
        ensure!(status == StatusCode::OK, "{status}: {body}");
        Ok(body)
    }

    async fn rotate(&self, key: &str, address: &Value) -> Result<Value> {
        let id = address["id"].as_str().context("id")?;
        let (status, body) = self
            .request(
                Method::POST,
                &format!("/v1/deposit_addresses/{id}/rotate"),
                key,
                Value::Null,
            )
            .await?;
        ensure!(status == StatusCode::OK, "{status}: {body}");
        Ok(body)
    }
}

fn app(pool: &sqlx::PgPool, routes: Vec<RouteFile>) -> Result<Router> {
    let admin_key = SigningKey::from_bytes(&[47; 32]);
    Ok(topup::api::router(AppState {
        pool: pool.clone(),
        routes: Arc::new(topup::routes::RouteSet::new(routes).map_err(anyhow::Error::msg)?),
        admin_key: VerificationKey::from_base64(
            "admin/v1".to_owned(),
            &public_key_base64(&admin_key),
        )
        .map_err(anyhow::Error::msg)?,
        public_origin: PublicOrigin::parse(TEST_ORIGIN)?,
        attestor: Arc::new(DstackAttestor::new()),
        rate_lock_quotes: Arc::new(topup::locks::UnavailableQuoteProvider),
        client_reads: Arc::default(),
        rate_limits: Arc::default(),
        refund_screening: Arc::new(topup::refunds::UnavailableDestinationScreener),
    })
    .0)
}

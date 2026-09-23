//! Anvil and PostgreSQL coverage for the display-only pending view (architecture §8, §12): the
//! head scan, reorg removal, the finalized hand-off, the lock `payment` object, the
//! `pending-deposits` endpoint, and the `deposit.pending` event.

mod support;

use std::str::FromStr;
use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use axum::body::to_bytes;
use axum::http::{Method, StatusCode};
use chrono::{DateTime, Utc};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use topup::api::{AppState, PublicOrigin, VerificationKey};
use topup::db::{NewAccount, NewPendingTransfer, NewProduct};
use topup::locks::QuoteProvider;
use topup::locks::pricing::ValidatedQuote;
use topup::routes::RouteSet;
use topup::scanner::{chain_routes, head_scan_once, scan_once};
use topup_adapters::attestation::DstackAttestor;
use topup_adapters::chain::evm::{EvmClient, FinalizedReader};
use topup_core::money::{AtomicAmount, PRICE_SCALE, ScaledPrice};
use topup_core::route::RouteFile;
use tower::ServiceExt;
use uuid::Uuid;

use support::chain::{ANVIL_PRIVATE_KEY, Anvil, CHAIN_ID, forge_create, run_checked};
use support::{TEST_ORIGIN, TestDatabase, public_key_base64, signed_request, with_database};

const ANVIL_DEPLOYER: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
const PRODUCT_KID: &str = "phala-cloud/v1";
/// With 32-slot epochs anvil reports `finalized = latest - 64`.
const FINALITY_LAG: u64 = 64;

struct FixedQuote;

#[async_trait]
impl QuoteProvider for FixedQuote {
    async fn quote(&self, _route: &RouteFile) -> Result<ValidatedQuote, Value> {
        Ok(ValidatedQuote {
            price: ScaledPrice::new(100_000_000, PRICE_SCALE).expect("fixed quote"),
            evidence: json!({"mode": "spot"}),
        })
    }
}

#[tokio::test]
async fn pending_transfers_are_display_only_until_final() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            // 32-slot epochs keep transfers above `finalized` for 64 blocks.
            let Some(anvil) = Anvil::start_if_available(&["--slots-in-an-epoch", "32"]).await?
            else {
                return Ok(());
            };
            run_scenario(database, &anvil).await
        })
    })
    .await
}

async fn run_scenario(database: &TestDatabase, anvil: &Anvil) -> Result<()> {
    let pool = &database.app_pool;
    let token = forge_create(&anvil.rpc_url, MOCK_ERC20, &[])?;
    let other_token = forge_create(&anvil.rpc_url, MOCK_ERC20, &[])?;
    anvil.mine(FINALITY_LAG + 2)?;

    let route = test_route(token);
    let route_set = Arc::new(RouteSet::new(vec![route.clone()]).map_err(anyhow::Error::msg)?);
    let chain_routes = chain_routes(&route_set)
        .into_iter()
        .next()
        .context("one chain")?;
    let product_key = SigningKey::from_bytes(&[61; 32]);
    let admin_key = SigningKey::from_bytes(&[62; 32]);
    seed_account(pool, &product_key).await?;
    let app = topup::api::router(AppState {
        pool: pool.clone(),
        routes: Arc::clone(&route_set),
        admin_key: VerificationKey::from_base64(
            "admin/v1".to_owned(),
            &public_key_base64(&admin_key),
        )
        .map_err(anyhow::Error::msg)?,
        public_origin: PublicOrigin::parse(TEST_ORIGIN)?,
        attestor: Arc::new(DstackAttestor::new()),
        rate_lock_quotes: Arc::new(FixedQuote),
    })
    .0;
    let api = Api {
        app,
        key: product_key,
        created: Utc::now().timestamp().into(),
    };
    let reader = FinalizedReader::new(Arc::new(EvmClient::new(&anvil.rpc_url)?));
    scan_once(pool, &reader, &chain_routes).await?;

    let lock = api
        .call(
            Method::POST,
            "/v1/products/phala-cloud/accounts/ws-pending/rate-locks",
            json!({"amount_atomic": "100", "product_lock_ref": "checkout-1"}),
        )
        .await?;
    ensure!(lock.get("payment").is_none(), "unpaid lock has a payment");
    let lock_address = Address::from_str(lock["address"].as_str().context("lock address")?)?;
    let persistent = api
        .call(
            Method::POST,
            "/v1/products/phala-cloud/accounts/ws-pending/deposit-address",
            Value::Null,
        )
        .await?;
    let persistent_address = Address::from_str(
        persistent["address"]
            .as_str()
            .context("persistent address")?,
    )?;
    let before = Ledger::read(pool).await?;

    let snapshot = snapshot(anvil)?;
    transfer(&anvil.rpc_url, token, lock_address, 100)?;
    transfer(&anvil.rpc_url, other_token, persistent_address, 5)?;
    transfer(&anvil.rpc_url, token, persistent_address, 7)?;
    transfer(&anvil.rpc_url, token, persistent_address, 0)?;

    let scan = head_scan_once(pool, &reader, &chain_routes)
        .await?
        .context("chain is not frozen")?;
    // Only non-zero transfers of the routed token are requested and stored.
    ensure!(scan.commit.seen == 2, "unexpected head scan {scan:?}");
    ensure!(scan.commit.announced == 2, "unexpected head scan {scan:?}");
    ensure!(
        Ledger::read(pool).await? == before,
        "a pending transfer changed deposits, locks, exposure, or transitions"
    );

    let lock = api
        .call(
            Method::GET,
            "/v1/products/phala-cloud/accounts/ws-pending/rate-locks/checkout-1",
            Value::Null,
        )
        .await?;
    ensure!(
        lock["status"] == "open",
        "pending payment consumed the lock"
    );
    let payment = &lock["payment"];
    ensure!(payment["status"] == "seen", "unexpected payment {payment}");
    ensure!(payment["amount_atomic"] == "100");
    ensure!(payment["supported"] == true);
    ensure!(payment["amount_within_tolerance"] == true);
    ensure!(payment["in_time"] == true);
    ensure!(
        payment["confirmations"]
            .as_u64()
            .is_some_and(|value| value >= 1)
    );
    let block_time = pending_block_time(pool, payment["tx_hash"].as_str().context("hash")?).await?;
    ensure!(
        payment["estimated_final_at"]
            .as_str()
            .map(parse_time)
            .transpose()?
            == Some(block_time + chrono::TimeDelta::minutes(15)),
        "estimated_final_at is not block time plus 15 minutes: {payment}"
    );

    let pending = api
        .call(
            Method::GET,
            "/v1/products/phala-cloud/accounts/ws-pending/pending-deposits",
            Value::Null,
        )
        .await?;
    let items = pending["pending_deposits"]
        .as_array()
        .context("pending list")?;
    ensure!(items.len() == 1, "unexpected pending deposits {pending}");
    let supported = &items[0];
    ensure!(supported["amount_atomic"] == "7");
    ensure!(supported["supported"] == true);
    ensure!(supported["address"] == format!("{persistent_address:#x}"));

    let again = head_scan_once(pool, &reader, &chain_routes)
        .await?
        .context("chain is not frozen")?;
    ensure!(again.commit.seen == 2 && again.commit.announced == 0);
    ensure!(pending_events(pool).await? == 2);
    let unannounced: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox WHERE event_type = 'deposit.pending' \
         AND (payload->>'provisional')::boolean IS NOT TRUE",
    )
    .fetch_one(pool)
    .await?;
    ensure!(
        unannounced == 0,
        "deposit.pending is not marked provisional"
    );

    // A reorg that drops the transfers removes them from the pending view.
    revert(anvil, &snapshot)?;
    anvil.mine(8)?;
    let reorged = head_scan_once(pool, &reader, &chain_routes)
        .await?
        .context("chain is not frozen")?;
    ensure!(
        reorged.commit.removed == 2,
        "unexpected reorg scan {reorged:?}"
    );
    ensure!(pending_rows(pool).await? == 0);
    let lock = api
        .call(
            Method::GET,
            "/v1/products/phala-cloud/accounts/ws-pending/rate-locks/checkout-1",
            Value::Null,
        )
        .await?;
    ensure!(lock.get("payment").is_none(), "reorged payment still shown");
    ensure!(pending_events(pool).await? == 2);

    // Once final, the finalized scanner records the deposit and clears its pending row in the same
    // transaction, and the lock shows the finalized payment.
    transfer(&anvil.rpc_url, token, lock_address, 100)?;
    head_scan_once(pool, &reader, &chain_routes).await?;
    ensure!(pending_rows(pool).await? == 1);
    ensure!(pending_events(pool).await? == 3);
    anvil.mine(FINALITY_LAG)?;
    let finalized = scan_once(pool, &reader, &chain_routes).await?;
    ensure!(
        finalized.inserted == 1,
        "unexpected finalized scan {finalized:?}"
    );
    ensure!(pending_rows(pool).await? == 0, "finalized row was kept");
    let lock = api
        .call(
            Method::GET,
            "/v1/products/phala-cloud/accounts/ws-pending/rate-locks/checkout-1",
            Value::Null,
        )
        .await?;
    ensure!(
        lock["payment"]["status"] == "finalized",
        "unexpected {lock}"
    );
    ensure!(lock["payment"]["confirmations"].is_null());

    // Underpay, then pay in full: the finalized underpayment does not consume the lock, so the
    // later exact payment is the one shown while it is still pending.
    let underpaid = api.lock("checkout-2").await?;
    transfer(&anvil.rpc_url, token, underpaid, 50)?;
    anvil.mine(FINALITY_LAG)?;
    scan_once(pool, &reader, &chain_routes).await?;
    transfer(&anvil.rpc_url, token, underpaid, 100)?;
    // Dust first, then the exact amount, both still pending.
    let dusted = api.lock("checkout-3").await?;
    transfer(&anvil.rpc_url, token, dusted, 1)?;
    transfer(&anvil.rpc_url, token, dusted, 100)?;
    head_scan_once(pool, &reader, &chain_routes).await?;
    for lock_ref in ["checkout-2", "checkout-3"] {
        let payment = api.payment(lock_ref).await?;
        ensure!(
            payment["status"] == "seen"
                && payment["amount_atomic"] == "100"
                && payment["amount_within_tolerance"] == true
                && payment["in_time"] == true,
            "{lock_ref} does not show the payment that consumes it: {payment}"
        );
    }

    // Once a deposit consumed the lock, that deposit is shown, whatever else arrived. The
    // underpayment is marked as the consumer by hand: an artificial state (the pump would consume
    // with the exact payment) used only to show the consuming deposit wins over the first
    // qualifying one.
    anvil.mine(FINALITY_LAG)?;
    scan_once(pool, &reader, &chain_routes).await?;
    let underpayment: Uuid = sqlx::query_scalar(
        "SELECT deposit.id FROM deposits AS deposit JOIN addresses AS address \
         ON address.id = deposit.address_id WHERE address.lock_ref = 'checkout-2' \
         AND deposit.amount_atomic = 50",
    )
    .fetch_one(pool)
    .await?;
    sqlx::query(
        "UPDATE rate_locks SET status = 'consumed', consumed_by = $1, exposure_reserved = false, \
         closed_at = now() WHERE address_id = (SELECT address_id FROM deposits WHERE id = $1)",
    )
    .bind(underpayment)
    .execute(pool)
    .await?;
    let payment = api.payment("checkout-2").await?;
    ensure!(
        payment["status"] == "finalized" && payment["deposit_id"] == underpayment.to_string(),
        "consumed lock does not show its consuming deposit: {payment}"
    );

    // A cancelled lock credits every payment at spot, so none is in time or within tolerance.
    let cancelled = api.lock("checkout-4").await?;
    api.call(
        Method::DELETE,
        "/v1/products/phala-cloud/accounts/ws-pending/rate-locks/checkout-4",
        Value::Null,
    )
    .await?;
    transfer(&anvil.rpc_url, token, cancelled, 100)?;
    anvil.mine(FINALITY_LAG)?;
    scan_once(pool, &reader, &chain_routes).await?;
    let lock = api
        .call(
            Method::GET,
            "/v1/products/phala-cloud/accounts/ws-pending/rate-locks/checkout-4",
            Value::Null,
        )
        .await?;
    ensure!(lock["status"] == "cancelled", "unexpected {lock}");
    ensure!(
        lock["payment"]["amount_atomic"] == "100"
            && lock["payment"]["in_time"] == false
            && lock["payment"]["amount_within_tolerance"] == false,
        "cancelled lock shows its payment as applying: {lock}"
    );
    ensure!(
        pending_events(pool).await? == 6,
        "unsupported or zero transfers were announced"
    );
    Ok(())
}

#[tokio::test]
async fn head_scan_watches_open_locks_and_recently_requested_persistent_addresses() -> Result<()> {
    with_database(|database| Box::pin(run_watched_scenario(&database.app_pool))).await
}

async fn run_watched_scenario(pool: &sqlx::PgPool) -> Result<()> {
    let product_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO products (id, slug, webhook_url, pubkey) \
         VALUES ($1, 'watch', 'https://product.test/w', 'k')",
    )
    .bind(product_id)
    .execute(pool)
    .await?;
    // 1 001 persistent addresses exceed one log request; only the one requested in the last
    // 24 hours stays watched. Addresses are `0x…<index>`; index 0 is the recent one.
    sqlx::query(
        r#"
        WITH account AS (
            INSERT INTO accounts (id, product_id, external_id)
            SELECT gen_random_uuid(), $1, 'ws-' || index FROM generate_series(0, 1000) AS index
            RETURNING id, external_id
        )
        INSERT INTO addresses (id, account_id, chain_id, kind, version, salt, address, requested_at)
        SELECT gen_random_uuid(), account.id, $2, 'persistent', 1, '0x' || repeat('0', 64),
               '0x' || lpad(to_hex(substr(account.external_id, 4)::int), 40, '0'),
               CASE WHEN account.external_id = 'ws-0' THEN now()
                    ELSE now() - interval '2 days' END
        FROM account
        "#,
    )
    .bind(product_id)
    .bind(i64::try_from(CHAIN_ID)?)
    .execute(pool)
    .await?;
    let account_id: Uuid = sqlx::query_scalar("SELECT id FROM accounts WHERE external_id = 'ws-0'")
        .fetch_one(pool)
        .await?;
    for (index, status, expires, watched) in [
        (
            0xffff_00a1_u64,
            "open",
            "now() + interval '10 minutes'",
            true,
        ),
        (
            0xffff_00a2,
            "expired",
            "now() - interval '30 minutes'",
            true,
        ),
        (0xffff_00a3, "expired", "now() - interval '2 hours'", false),
        (
            0xffff_00a4,
            "cancelled",
            "now() + interval '10 minutes'",
            false,
        ),
    ] {
        let address = format!("0x{index:040x}");
        let address_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO addresses (id, account_id, chain_id, kind, version, lock_ref, salt, address) \
             VALUES ($1, $2, $3, 'lock', 0, $4, '0x' || repeat('0', 64), $4)",
        )
        .bind(address_id)
        .bind(account_id)
        .bind(i64::try_from(CHAIN_ID)?)
        .bind(&address)
        .execute(pool)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO rate_locks (address_id, route, amount_atomic, price_scaled, credit_minor, \
             expires_at, status, closed_at) VALUES ($1, 'r', 1, 1, 1, {expires}, $2, \
             CASE WHEN $2 = 'open' THEN NULL ELSE now() END)"
        ))
        .bind(address_id)
        .bind(status)
        .execute(pool)
        .await?;
        let listed = topup::db::list_watched_addresses(pool, CHAIN_ID)
            .await?
            .iter()
            .any(|watched| format!("{:#x}", watched.address) == address);
        ensure!(
            listed == watched,
            "lock {status} expiring {expires}: watched = {listed}"
        );
    }
    let persistent = topup::db::list_watched_addresses(pool, CHAIN_ID)
        .await?
        .into_iter()
        .map(|watched| watched.address)
        .filter(|address| U256::from_be_slice(address.as_slice()) <= U256::from(1_000_u64))
        .collect::<Vec<_>>();
    ensure!(
        persistent == [Address::ZERO],
        "expected only the recently requested persistent address, got {} addresses",
        persistent.len()
    );

    // A head scan from a provider that lags behind a stored row leaves the row alone; the next
    // scan whose range covers it and does not see it removes it.
    let address_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM addresses WHERE address = '0x0000000000000000000000000000000000000000'",
    )
    .fetch_one(pool)
    .await?;
    let row = NewPendingTransfer {
        chain_id: CHAIN_ID,
        tx_hash: B256::repeat_byte(0x51),
        log_index: 0,
        block_number: 50,
        block_hash: B256::repeat_byte(0x52),
        block_time: Utc::now(),
        address_id,
        asset_contract: Address::repeat_byte(0x53),
        from_address: Address::repeat_byte(0x54),
        amount_atomic: AtomicAmount::new(U256::from(1_u64)),
    };
    topup::db::commit_head_scan(pool, CHAIN_ID, 10, 60, std::slice::from_ref(&row)).await?;
    let lagging = topup::db::commit_head_scan(pool, CHAIN_ID, 10, 40, &[]).await?;
    ensure!(
        lagging.removed == 0,
        "a lagging head deleted a row above it"
    );
    let covering = topup::db::commit_head_scan(pool, CHAIN_ID, 10, 60, &[]).await?;
    ensure!(covering.removed == 1, "an unseen row in range was kept");
    Ok(())
}

/// Every table a pending row must never touch.
#[derive(Debug, Eq, PartialEq)]
struct Ledger {
    deposits: i64,
    transitions: i64,
    locks: Vec<(String, Option<Uuid>, bool)>,
    exposure: Vec<(String, String)>,
}

impl Ledger {
    async fn read(pool: &sqlx::PgPool) -> Result<Self> {
        Ok(Self {
            deposits: sqlx::query_scalar("SELECT count(*) FROM deposits")
                .fetch_one(pool)
                .await?,
            transitions: sqlx::query_scalar("SELECT count(*) FROM transitions")
                .fetch_one(pool)
                .await?,
            locks: sqlx::query_as(
                "SELECT status, consumed_by, exposure_reserved FROM rate_locks ORDER BY address_id",
            )
            .fetch_all(pool)
            .await?,
            exposure: sqlx::query_as(
                "SELECT scope_key, open_minor::text FROM lock_exposure ORDER BY scope_key",
            )
            .fetch_all(pool)
            .await?,
        })
    }
}

struct Api {
    app: axum::Router,
    key: SigningKey,
    /// Distinct `created` per request: identical requests signed in the same second would carry
    /// the same single-use signature.
    created: std::sync::atomic::AtomicI64,
}

impl Api {
    async fn lock(&self, lock_ref: &str) -> Result<Address> {
        let lock = self
            .call(
                Method::POST,
                "/v1/products/phala-cloud/accounts/ws-pending/rate-locks",
                json!({"amount_atomic": "100", "product_lock_ref": lock_ref}),
            )
            .await?;
        Ok(Address::from_str(
            lock["address"].as_str().context("lock address")?,
        )?)
    }

    async fn payment(&self, lock_ref: &str) -> Result<Value> {
        let lock = self
            .call(
                Method::GET,
                &format!("/v1/products/phala-cloud/accounts/ws-pending/rate-locks/{lock_ref}"),
                Value::Null,
            )
            .await?;
        Ok(lock["payment"].clone())
    }

    async fn call(&self, method: Method, path: &str, body: Value) -> Result<Value> {
        let body = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&body)?
        };
        let response = self
            .app
            .clone()
            .oneshot(signed_request(
                method,
                path,
                body,
                PRODUCT_KID,
                &self.key,
                self.created
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            ))
            .await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1_048_576).await?;
        ensure!(
            status == StatusCode::OK,
            "{path} returned {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
        Ok(serde_json::from_slice(&bytes)?)
    }
}

async fn pending_rows(pool: &sqlx::PgPool) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM pending_transfers")
        .fetch_one(pool)
        .await?)
}

async fn pending_events(pool: &sqlx::PgPool) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM outbox WHERE event_type = 'deposit.pending'")
            .fetch_one(pool)
            .await?,
    )
}

async fn pending_block_time(pool: &sqlx::PgPool, tx_hash: &str) -> Result<DateTime<Utc>> {
    Ok(
        sqlx::query_scalar("SELECT block_time FROM pending_transfers WHERE tx_hash = $1")
            .bind(tx_hash)
            .fetch_one(pool)
            .await?,
    )
}

fn parse_time(value: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

async fn seed_account(pool: &sqlx::PgPool, key: &SigningKey) -> Result<()> {
    let product = topup::db::create_product(
        pool,
        &NewProduct {
            id: Uuid::new_v4(),
            slug: "phala-cloud".to_owned(),
            webhook_url: "https://product.test/webhooks".to_owned(),
            pubkey: public_key_base64(key),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    topup::db::create_account(
        pool,
        &NewAccount {
            id: Uuid::new_v4(),
            product_id: product.id,
            external_id: "ws-pending".to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    Ok(())
}

fn test_route(token: Address) -> RouteFile {
    let yaml = include_str!("fixtures/phala-cloud-pha.yaml")
        .replace("chain_id: 1", &format!("chain_id: {CHAIN_ID}"))
        .replace(
            "0x6c5bA91642F10282b576d91922Ae6448C9d52f4E",
            &format!("{token:#x}"),
        );
    let mut route: RouteFile = serde_saphyr::from_str(&yaml).expect("route fixture");
    route.asset.decimals = 0;
    route.destination.unit_decimals = 0;
    route.rate_lock.spread_bps = topup_core::money::Bps::new(0).expect("zero bps");
    route.screening.min_deposit_atomic = AtomicAmount::new(U256::from(1_u64));
    route.screening.min_credit_minor = 1;
    route
}

const MOCK_ERC20: &str = "test/mocks/MockTokens.sol:MockERC20";

fn rpc(anvil: &Anvil, method: &str, params: &[&str]) -> Result<String> {
    let mut arguments = vec!["rpc", "--rpc-url", &anvil.rpc_url, method];
    arguments.extend_from_slice(params);
    let output = run_checked("cast", &arguments, None)?;
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn snapshot(anvil: &Anvil) -> Result<String> {
    Ok(rpc(anvil, "evm_snapshot", &[])?
        .trim_matches('"')
        .to_owned())
}

fn revert(anvil: &Anvil, snapshot: &str) -> Result<()> {
    ensure!(
        rpc(anvil, "evm_revert", &[snapshot])? == "true",
        "revert failed"
    );
    Ok(())
}

fn transfer(rpc_url: &str, token: Address, recipient: Address, amount: u64) -> Result<()> {
    for (signature, target) in [
        ("mint(address,uint256)", ANVIL_DEPLOYER.to_owned()),
        ("transfer(address,uint256)", format!("{recipient:#x}")),
    ] {
        run_checked(
            "cast",
            &[
                "send",
                "--rpc-url",
                rpc_url,
                "--private-key",
                ANVIL_PRIVATE_KEY,
                &format!("{token:#x}"),
                signature,
                &target,
                &amount.to_string(),
            ],
            None,
        )?;
    }
    Ok(())
}

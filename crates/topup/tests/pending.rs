//! Anvil and PostgreSQL coverage for the display-only pending view (architecture §8, §12): the
//! head scan, reorg removal, the finalized hand-off, the lock `payment` object, the
//! `pending-deposits` endpoint, and the `deposit.pending` event.

mod support;

use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{Address, U256};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use axum::body::to_bytes;
use axum::http::{Method, StatusCode};
use chrono::{DateTime, Utc};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use topup::api::{AppState, PublicOrigin, UnavailableAttestor, VerificationKey};
use topup::db::{NewAccount, NewProduct};
use topup::locks::QuoteProvider;
use topup::locks::pricing::ValidatedQuote;
use topup::scanner::{configure_routes, head_scan_once, scan_once};
use topup_adapters::chain::evm::EvmChain;
use topup_core::money::{AtomicAmount, PRICE_SCALE, ScaledPrice};
use topup_core::route::RouteFile;
use tower::ServiceExt;
use uuid::Uuid;

use support::{TEST_ORIGIN, TestDatabase, public_key_base64, signed_request};

const ANVIL_PRIVATE_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const ANVIL_DEPLOYER: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
const CHAIN_ID: u64 = 31_337;
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
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let Some(anvil) = Anvil::start()? else {
        database.cleanup().await?;
        return Ok(());
    };
    let result = run_scenario(&database, &anvil).await;
    drop(anvil);
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

async fn run_scenario(database: &TestDatabase, anvil: &Anvil) -> Result<()> {
    let pool = &database.app_pool;
    let contracts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts");
    run_checked("forge", &["build"], Some(&contracts))?;
    let token = deploy_token(&contracts, &anvil.rpc_url)?;
    let other_token = deploy_token(&contracts, &anvil.rpc_url)?;
    anvil.mine(FINALITY_LAG + 2)?;

    let route = test_route(token);
    let chain_routes = configure_routes(std::slice::from_ref(&route))?
        .into_iter()
        .next()
        .context("one chain")?;
    let product_key = SigningKey::from_bytes(&[61; 32]);
    let admin_key = SigningKey::from_bytes(&[62; 32]);
    seed_account(pool, &product_key).await?;
    let app = topup::api::router(AppState {
        pool: pool.clone(),
        routes: Arc::new(vec![route.clone()]),
        admin_key: VerificationKey::from_base64(
            "admin/v1".to_owned(),
            &public_key_base64(&admin_key),
        )
        .map_err(anyhow::Error::msg)?,
        public_origin: PublicOrigin::parse(TEST_ORIGIN)?,
        attestor: Arc::new(UnavailableAttestor),
        rate_lock_quotes: Arc::new(FixedQuote),
    })
    .0;
    let api = Api {
        app,
        key: product_key,
        created: Utc::now().timestamp().into(),
    };
    let reader = EvmChain::new(&anvil.rpc_url)?;
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

    let snapshot = anvil.snapshot()?;
    transfer(&anvil.rpc_url, token, lock_address, 100)?;
    transfer(&anvil.rpc_url, other_token, persistent_address, 5)?;
    transfer(&anvil.rpc_url, token, persistent_address, 7)?;

    let scan = head_scan_once(pool, &reader, CHAIN_ID)
        .await?
        .context("chain is not frozen")?;
    ensure!(scan.commit.seen == 3, "unexpected head scan {scan:?}");
    ensure!(scan.commit.announced == 3, "unexpected head scan {scan:?}");
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
    ensure!(items.len() == 2, "unexpected pending deposits {pending}");
    let unsupported = items
        .iter()
        .find(|item| item["amount_atomic"] == "5")
        .context("unsupported transfer")?;
    ensure!(unsupported["supported"] == false);
    ensure!(unsupported["asset_contract"] == format!("{other_token:#x}"));
    let supported = items
        .iter()
        .find(|item| item["amount_atomic"] == "7")
        .context("supported transfer")?;
    ensure!(supported["supported"] == true);
    ensure!(supported["address"] == format!("{persistent_address:#x}"));

    let again = head_scan_once(pool, &reader, CHAIN_ID)
        .await?
        .context("chain is not frozen")?;
    ensure!(again.commit.seen == 3 && again.commit.announced == 0);
    ensure!(pending_events(pool).await? == 3);
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
    anvil.revert(&snapshot)?;
    anvil.mine(8)?;
    let reorged = head_scan_once(pool, &reader, CHAIN_ID)
        .await?
        .context("chain is not frozen")?;
    ensure!(
        reorged.commit.removed == 3,
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
    ensure!(pending_events(pool).await? == 3);

    // Once final, the finalized scanner records the deposit and clears its pending row in the same
    // transaction, and the lock shows the finalized payment.
    transfer(&anvil.rpc_url, token, lock_address, 100)?;
    head_scan_once(pool, &reader, CHAIN_ID).await?;
    ensure!(pending_rows(pool).await? == 1);
    ensure!(pending_events(pool).await? == 4);
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
    Ok(())
}

#[tokio::test]
async fn head_scan_watches_open_locks_and_recently_requested_persistent_addresses() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = run_watched_scenario(&database.app_pool).await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

async fn run_watched_scenario(pool: &sqlx::PgPool) -> Result<()> {
    let product_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO products (id, slug, settlement_url, webhook_url, pubkey, kid) \
         VALUES ($1, 'watch', 'https://product.test/s', 'https://product.test/w', 'k', 'watch/v1')",
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
            settlement_url: "https://product.test/settlements".to_owned(),
            webhook_url: "https://product.test/webhooks".to_owned(),
            pubkey: public_key_base64(key),
            kid: PRODUCT_KID.to_owned(),
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

struct Anvil {
    child: Child,
    rpc_url: String,
}

impl Anvil {
    fn start() -> Result<Option<Self>> {
        if !command_available("anvil") {
            eprintln!("skipping pending integration test: anvil is not on PATH");
            return Ok(None);
        }
        ensure!(command_available("forge") && command_available("cast"));
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let rpc_url = format!("http://127.0.0.1:{port}");
        let child = Command::new("anvil")
            .args([
                "--silent",
                "--port",
                &port.to_string(),
                "--chain-id",
                &CHAIN_ID.to_string(),
                "--slots-in-an-epoch",
                "32",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start anvil")?;
        let anvil = Self { child, rpc_url };
        for _ in 0..100 {
            if run_checked("cast", &["block-number", "--rpc-url", &anvil.rpc_url], None).is_ok() {
                return Ok(Some(anvil));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        anyhow::bail!("anvil did not become ready")
    }

    fn rpc(&self, method: &str, params: &[&str]) -> Result<String> {
        let mut arguments = vec!["rpc", "--rpc-url", &self.rpc_url, method];
        arguments.extend_from_slice(params);
        let output = run_checked("cast", &arguments, None)?;
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    fn mine(&self, count: u64) -> Result<()> {
        self.rpc("anvil_mine", &[&format!("0x{count:x}")])?;
        Ok(())
    }

    fn snapshot(&self) -> Result<String> {
        Ok(self.rpc("evm_snapshot", &[])?.trim_matches('"').to_owned())
    }

    fn revert(&self, snapshot: &str) -> Result<()> {
        ensure!(
            self.rpc("evm_revert", &[snapshot])? == "true",
            "revert failed"
        );
        Ok(())
    }
}

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn command_available(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn run_checked(command: &str, arguments: &[&str], directory: Option<&Path>) -> Result<Output> {
    let mut invocation = Command::new(command);
    invocation.args(arguments);
    if let Some(directory) = directory {
        invocation.current_dir(directory);
    }
    let output = invocation.output()?;
    ensure!(
        output.status.success(),
        "{command} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

fn deploy_token(contracts: &Path, rpc_url: &str) -> Result<Address> {
    let output = run_checked(
        "forge",
        &[
            "create",
            "--rpc-url",
            rpc_url,
            "--private-key",
            ANVIL_PRIVATE_KEY,
            "--broadcast",
            "--json",
            "test/mocks/MockTokens.sol:MockERC20",
        ],
        Some(contracts),
    )?;
    let result: Value = serde_json::from_slice(&output.stdout)?;
    let address = result
        .get("deployedTo")
        .and_then(Value::as_str)
        .context("forge create omitted deployedTo")?;
    Address::from_str(address).context("parse deployed token address")
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

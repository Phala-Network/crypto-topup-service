//! Anvil and PostgreSQL integration coverage for the C3 scanner.

mod support;

use std::env;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use chrono::DateTime;
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool, Row};
use topup::db::{self, AddressKind, NewAccount, NewAddress, NewProduct};
use topup::pump::{NoopStepSet, Pump, PumpConfig, RunOnceResult, Step};
use topup::scanner::{load_route_files, scan_once};
use topup::steps::confirm::{ConfirmStep, ProductAnswer, ProductLookup, ProductLookupError};
use topup_adapters::chain::evm::{ChainError, ChainReader, EvmChain, TransferLog};
use topup_adapters::pricing::{Observation, PriceError, PriceSource};
use topup_core::deposit::{StepOutcome, WaitReason};
use topup_core::money::{AtomicAmount, PRICE_SCALE, ScaledPrice};
use topup_core::route::RouteFile;
use topup_core::valuation::{SourceId, UnixSeconds};
use url::Url;
use uuid::Uuid;

/// Waits out transient `max_connections` exhaustion when many test databases share one
/// server under load; sqlx's 30 s default turns that into spurious `PoolTimedOut` failures.
const DB_ACQUIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

const ANVIL_PRIVATE_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const ANVIL_DEPLOYER: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
const CHAIN_ID: u64 = 31_337;

struct MissingProductAnswer;

#[async_trait]
impl ProductLookup for MissingProductAnswer {
    async fn get_by_key(&self, _key: &str) -> Result<Option<ProductAnswer>, ProductLookupError> {
        Ok(None)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RecordedRequest {
    addresses: Vec<Address>,
    from_block: u64,
    to_block: u64,
}

struct RecordingReader {
    finalized: u64,
    requests: Mutex<Vec<RecordedRequest>>,
}

impl RecordingReader {
    fn new(finalized: u64) -> Self {
        Self {
            finalized,
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("request lock").clone()
    }
}

impl ChainReader for RecordingReader {
    async fn finalized_head(&self) -> Result<u64, ChainError> {
        Ok(self.finalized)
    }

    async fn transfer_logs_to(
        &self,
        addresses: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<TransferLog>, ChainError> {
        self.requests
            .lock()
            .map_err(|_| ChainError::HealthStateUnavailable)?
            .push(RecordedRequest {
                addresses: addresses.to_vec(),
                from_block,
                to_block,
            });
        Ok(Vec::new())
    }

    async fn transfer_log_by_identity(
        &self,
        _tx_hash: B256,
        _log_index: u64,
    ) -> Result<Option<TransferLog>, ChainError> {
        Ok(None)
    }
}

struct BackfillReader {
    recipient: Address,
    token: Address,
    fail_request: Mutex<Option<usize>>,
    requests: Mutex<Vec<RecordedRequest>>,
}

impl BackfillReader {
    fn new(recipient: Address, token: Address, fail_request: usize) -> Self {
        Self {
            recipient,
            token,
            fail_request: Mutex::new(Some(fail_request)),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn request_count(&self) -> usize {
        self.requests.lock().expect("request lock").len()
    }
}

impl ChainReader for BackfillReader {
    async fn finalized_head(&self) -> Result<u64, ChainError> {
        Ok(4_000)
    }

    async fn transfer_logs_to(
        &self,
        addresses: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<TransferLog>, ChainError> {
        let request_number = {
            let mut requests = self
                .requests
                .lock()
                .map_err(|_| ChainError::HealthStateUnavailable)?;
            requests.push(RecordedRequest {
                addresses: addresses.to_vec(),
                from_block,
                to_block,
            });
            requests.len()
        };
        let should_fail = {
            let mut fail_request = self
                .fail_request
                .lock()
                .map_err(|_| ChainError::HealthStateUnavailable)?;
            if *fail_request == Some(request_number) {
                *fail_request = None;
                true
            } else {
                false
            }
        };
        if should_fail {
            return Err(ChainError::Rpc("scripted backfill failure"));
        }
        if !addresses.contains(&self.recipient) {
            return Ok(Vec::new());
        }

        let mut logs = Vec::new();
        if from_block <= 100 && 100 <= to_block {
            logs.push(mock_transfer_log(self.token, self.recipient, 100, 1));
        }
        if from_block <= 3_000 && 3_000 <= to_block {
            logs.push(mock_transfer_log(self.token, self.recipient, 3_000, 2));
        }
        Ok(logs)
    }

    async fn transfer_log_by_identity(
        &self,
        _tx_hash: B256,
        _log_index: u64,
    ) -> Result<Option<TransferLog>, ChainError> {
        Ok(None)
    }
}

struct TestDatabase {
    admin_pool: PgPool,
    owner_pool: PgPool,
    app_pool: PgPool,
    database_name: String,
    app_role: String,
}

impl TestDatabase {
    async fn create() -> Result<Option<Self>> {
        let Some(owner_template) = required_url("MIGRATE_DATABASE_URL") else {
            return Ok(None);
        };
        let Some(app_template) = required_url("DATABASE_URL") else {
            return Ok(None);
        };
        let mut admin_url = Url::parse(&owner_template)?;
        admin_url.set_path("/postgres");
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(DB_ACQUIRE_TIMEOUT)
            .connect(admin_url.as_str())
            .await?;
        support::ensure_app_role(&admin_pool).await?;
        sqlx::query("SELECT pg_advisory_lock(704_203_001)")
            .execute(&admin_pool)
            .await?;

        let suffix = Uuid::new_v4().simple().to_string();
        let database_name = format!("topup_c3_{suffix}");
        let app_role = format!("topup_c3_app_{suffix}");
        let password = format!("c3_{suffix}");
        admin_pool
            .execute(format!("CREATE DATABASE \"{database_name}\"").as_str())
            .await?;

        let mut owner_url = Url::parse(&owner_template)?;
        owner_url.set_path(&format!("/{database_name}"));
        let owner_pool = PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(DB_ACQUIRE_TIMEOUT)
            .connect(owner_url.as_str())
            .await?;
        db::migrate(&owner_pool).await?;
        admin_pool
            .execute(
                format!("CREATE ROLE \"{app_role}\" LOGIN PASSWORD '{password}' IN ROLE topup_app")
                    .as_str(),
            )
            .await?;

        let mut app_url = Url::parse(&app_template)?;
        app_url
            .set_username(&app_role)
            .map_err(|()| anyhow::anyhow!("DATABASE_URL cannot accept a username"))?;
        app_url
            .set_password(Some(&password))
            .map_err(|()| anyhow::anyhow!("DATABASE_URL cannot accept a password"))?;
        app_url.set_path(&format!("/{database_name}"));
        let app_pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(DB_ACQUIRE_TIMEOUT)
            .connect(app_url.as_str())
            .await?;
        sqlx::query("SELECT pg_advisory_unlock(704_203_001)")
            .execute(&admin_pool)
            .await?;

        Ok(Some(Self {
            admin_pool,
            owner_pool,
            app_pool,
            database_name,
            app_role,
        }))
    }

    async fn cleanup(self) -> Result<()> {
        self.app_pool.close().await;
        self.owner_pool.close().await;
        self.admin_pool
            .execute(format!("DROP DATABASE \"{}\" WITH (FORCE)", self.database_name).as_str())
            .await?;
        self.admin_pool
            .execute(format!("DROP ROLE \"{}\"", self.app_role).as_str())
            .await?;
        self.admin_pool.close().await;
        Ok(())
    }
}

struct Anvil {
    child: Child,
    rpc_url: String,
}

impl Anvil {
    fn start() -> Result<Option<Self>> {
        if !command_available("anvil") {
            eprintln!("skipping scanner integration test: anvil is not on PATH");
            return Ok(None);
        }
        ensure!(
            command_available("forge"),
            "forge is required when anvil is available"
        );
        ensure!(
            command_available("cast"),
            "cast is required when anvil is available"
        );

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
                "1",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start anvil")?;
        let anvil = Self { child, rpc_url };
        for _ in 0..100 {
            if command_success("cast", &["block-number", "--rpc-url", &anvil.rpc_url]) {
                return Ok(Some(anvil));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        anyhow::bail!("anvil did not become ready")
    }

    fn mine(&self, count: u64) -> Result<()> {
        run_checked(
            "cast",
            &[
                "rpc",
                "--rpc-url",
                &self.rpc_url,
                "anvil_mine",
                &format!("0x{count:x}"),
            ],
            None,
        )?;
        Ok(())
    }

    fn reset(&self) -> Result<()> {
        run_checked(
            "cast",
            &["rpc", "--rpc-url", &self.rpc_url, "anvil_reset"],
            None,
        )?;
        Ok(())
    }
}

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct RouteFixture {
    path: PathBuf,
}

impl RouteFixture {
    fn create(token: Address) -> Result<Self> {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        std::fs::create_dir_all(&directory)?;
        let path = directory.join(format!("c3-route-{}.yaml", Uuid::new_v4()));
        let yaml = include_str!("fixtures/phala-cloud-pha.yaml")
            .replace("chain_id: 1", &format!("chain_id: {CHAIN_ID}"))
            .replace(
                "0x6c5bA91642F10282b576d91922Ae6448C9d52f4E",
                &format!("{token:#x}"),
            );
        std::fs::write(&path, yaml)?;
        Ok(Self { path })
    }
}

impl Drop for RouteFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[tokio::test]
async fn finalized_scanner_is_idempotent_atomic_and_backfills_new_addresses() -> Result<()> {
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

#[tokio::test]
async fn backfill_failure_keeps_marker_unset_then_retries_without_duplicates() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };

    let result = run_backfill_retry_scenario(&database).await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn scanner_shards_actual_requests_and_includes_lock_addresses() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };

    let result = run_sharding_scenario(&database).await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn scanned_transfer_confirms_with_two_providers_and_waits_for_a_lagging_provider()
-> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let Some(primary_anvil) = Anvil::start()? else {
        database.cleanup().await?;
        return Ok(());
    };
    let Some(lagging_anvil) = Anvil::start()? else {
        drop(primary_anvil);
        database.cleanup().await?;
        return Ok(());
    };

    let result = run_confirm_scenario(&database, &primary_anvil, &lagging_anvil).await;
    drop(lagging_anvil);
    drop(primary_anvil);
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

async fn run_scenario(database: &TestDatabase, anvil: &Anvil) -> Result<()> {
    let contracts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts");
    run_checked("forge", &["build"], Some(&contracts))?;
    let supported_token = deploy_token(&contracts, &anvil.rpc_url)?;
    let unsupported_token = deploy_token(&contracts, &anvil.rpc_url)?;
    let nft = deploy_contract(
        &contracts,
        &anvil.rpc_url,
        "test/mocks/MockTokens.sol:MockERC721Transfer",
    )?;
    let tracked_one = Address::from([0x11_u8; 20]);
    let tracked_two = Address::from([0x22_u8; 20]);
    let tracked_later = Address::from([0x33_u8; 20]);

    let account_id = seed_account(&database.app_pool).await?;
    insert_address(&database.app_pool, account_id, tracked_one, 1).await?;
    insert_address(&database.app_pool, account_id, tracked_two, 2).await?;
    transfer(&anvil.rpc_url, supported_token, tracked_one, 101)?;
    transfer(&anvil.rpc_url, supported_token, tracked_two, 202)?;
    transfer(&anvil.rpc_url, unsupported_token, tracked_one, 303)?;
    mint_nft(&anvil.rpc_url, nft, tracked_one, 404)?;
    anvil.mine(2)?;

    let route_fixture = RouteFixture::create(supported_token)?;
    let routes = load_route_files(std::slice::from_ref(&route_fixture.path))?
        .into_iter()
        .next()
        .context("one chain route")?;
    let reader = EvmChain::new(&anvil.rpc_url)?;
    let expected_cursor = reader.finalized_head().await?;
    let first = scan_once(&database.app_pool, &reader, &routes).await?;
    ensure!(
        first.inserted == 3,
        "expected three deposits, got {first:?}"
    );
    assert_deposit_counts(&database.app_pool, 3, 1).await?;
    assert_unsupported_asset_events(&database.app_pool, account_id).await?;
    ensure!(
        db::get_cursor(&database.app_pool, CHAIN_ID).await? == Some(expected_cursor),
        "cursor did not advance past the skipped ERC-721 Transfer"
    );

    sqlx::query("UPDATE cursors SET scanned_block = 0 WHERE chain_id = $1")
        .bind(i64::try_from(CHAIN_ID)?)
        .execute(&database.app_pool)
        .await?;
    let duplicate = scan_once(&database.app_pool, &reader, &routes).await?;
    ensure!(duplicate.inserted == 0, "duplicate logs inserted again");
    assert_deposit_counts(&database.app_pool, 3, 1).await?;
    assert_unsupported_asset_events(&database.app_pool, account_id).await?;

    let cursor_before_failure = db::get_cursor(&database.app_pool, CHAIN_ID)
        .await?
        .context("cursor after first scan")?;
    transfer(&anvil.rpc_url, supported_token, tracked_one, 404)?;
    anvil.mine(2)?;
    install_insert_failure(&database.owner_pool).await?;
    ensure!(
        scan_once(&database.app_pool, &reader, &routes)
            .await
            .is_err(),
        "forced insert failure must fail the scan"
    );
    ensure!(
        db::get_cursor(&database.app_pool, CHAIN_ID).await? == Some(cursor_before_failure),
        "cursor advanced despite a rolled-back deposit insert"
    );
    remove_insert_failure(&database.owner_pool).await?;
    let recovered = scan_once(&database.app_pool, &reader, &routes).await?;
    ensure!(recovered.inserted == 1);

    transfer(&anvil.rpc_url, supported_token, tracked_later, 505)?;
    let transfer_block = current_block(&anvil.rpc_url)?;
    anvil.mine(2)?;
    let before_address = scan_once(&database.app_pool, &reader, &routes).await?;
    ensure!(before_address.inserted == 0);
    let later_id = insert_address(&database.app_pool, account_id, tracked_later, 3).await?;
    sqlx::query("UPDATE addresses SET created_block = $2 WHERE id = $1")
        .bind(later_id)
        .bind(i64::try_from(transfer_block)?)
        .execute(&database.app_pool)
        .await?;
    let backfill = scan_once(&database.app_pool, &reader, &routes).await?;
    ensure!(backfill.inserted == 1, "new address was not backfilled");
    let backfilled: bool = sqlx::query("SELECT backfilled FROM addresses WHERE id = $1")
        .bind(later_id)
        .fetch_one(&database.app_pool)
        .await?
        .try_get(0)?;
    ensure!(backfilled, "new address backfill marker was not committed");
    assert_deposit_counts(&database.app_pool, 5, 1).await?;

    let previous = reader.finalized_head().await?;
    ensure!(previous > 0);
    anvil.reset()?;
    ensure!(
        matches!(
            reader.finalized_head().await,
            Err(ChainError::FinalizedHeadRegressed { current: 0, .. })
        ),
        "provider reset must be detected as a finalized regression"
    );
    ensure!(
        matches!(
            reader.finalized_head().await,
            Err(ChainError::ProviderUnhealthy)
        ),
        "provider must remain unhealthy after regression"
    );
    Ok(())
}

/// A deposit born `rejected(unsupported_asset)` emits exactly one `deposit.rejected` event.
async fn assert_unsupported_asset_events(pool: &PgPool, account_id: Uuid) -> Result<()> {
    let rows = sqlx::query(
        r#"
        SELECT event.payload
        FROM outbox AS event
        JOIN deposits AS deposit ON deposit.id = (event.payload->>'deposit_id')::uuid
        WHERE event.event_type = 'deposit.rejected'
        "#,
    )
    .fetch_all(pool)
    .await?;
    ensure!(
        rows.len() == 1,
        "expected one deposit.rejected event, got {}",
        rows.len()
    );
    let payload: Value = rows[0].try_get("payload")?;
    let product_id: Uuid = sqlx::query_scalar("SELECT product_id FROM accounts WHERE id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await?;
    ensure!(
        payload["reason"] == "unsupported_asset",
        "unexpected reason: {payload}"
    );
    ensure!(
        payload["product_id"] == product_id.to_string(),
        "event does not name the owning product: {payload}"
    );
    ensure!(
        payload["chain_id"].is_u64() && payload["state"] == "rejected",
        "event lacks the shared deposit event fields: {payload}"
    );
    ensure!(
        payload.get("route").is_some(),
        "event lacks the route field: {payload}"
    );
    Ok(())
}

async fn run_backfill_retry_scenario(database: &TestDatabase) -> Result<()> {
    let token = Address::from([0x44_u8; 20]);
    let recipient = Address::from([0x55_u8; 20]);
    let account_id = seed_account(&database.app_pool).await?;
    let address_id = insert_address(&database.app_pool, account_id, recipient, 1).await?;
    sqlx::query("UPDATE addresses SET created_block = 1 WHERE id = $1")
        .bind(address_id)
        .execute(&database.app_pool)
        .await?;
    sqlx::query("INSERT INTO cursors (chain_id, scanned_block) VALUES ($1, 4000)")
        .bind(i64::try_from(CHAIN_ID)?)
        .execute(&database.app_pool)
        .await?;

    let route_fixture = RouteFixture::create(token)?;
    let routes = load_route_files(std::slice::from_ref(&route_fixture.path))?
        .into_iter()
        .next()
        .context("one chain route")?;
    let reader = BackfillReader::new(recipient, token, 2);

    ensure!(
        matches!(
            scan_once(&database.app_pool, &reader, &routes).await,
            Err(topup::scanner::ScannerError::Chain(ChainError::Rpc(_)))
        ),
        "the scripted second backfill window must fail"
    );
    ensure!(!address_backfilled(&database.app_pool, address_id).await?);
    ensure!(deposit_count(&database.app_pool).await? == 1);

    let recovered = scan_once(&database.app_pool, &reader, &routes).await?;
    ensure!(
        recovered.inserted == 1,
        "only the missing window should insert"
    );
    ensure!(address_backfilled(&database.app_pool, address_id).await?);
    ensure!(deposit_count(&database.app_pool).await? == 2);

    let completed_request_count = reader.request_count();
    let completed = scan_once(&database.app_pool, &reader, &routes).await?;
    ensure!(completed.inserted == 0);
    ensure!(
        reader.request_count() == completed_request_count,
        "a completed address was backfilled again"
    );
    Ok(())
}

async fn run_sharding_scenario(database: &TestDatabase) -> Result<()> {
    let token = Address::from([0x66_u8; 20]);
    let account_id = seed_account(&database.app_pool).await?;
    insert_address(&database.app_pool, account_id, indexed_address(1), 1).await?;
    let lock_address = indexed_address(2);
    for index in 0..1_000_u64 {
        insert_lock_address(
            &database.app_pool,
            account_id,
            indexed_address(index.saturating_add(2)),
            index,
        )
        .await?;
    }
    sqlx::query("UPDATE addresses SET backfilled = true WHERE chain_id = $1")
        .bind(i64::try_from(CHAIN_ID)?)
        .execute(&database.app_pool)
        .await?;

    let route_fixture = RouteFixture::create(token)?;
    let routes = load_route_files(std::slice::from_ref(&route_fixture.path))?
        .into_iter()
        .next()
        .context("one chain route")?;
    let reader = RecordingReader::new(4_001);
    let stats = scan_once(&database.app_pool, &reader, &routes).await?;
    ensure!(stats.cursor == 4_001);

    let requests = reader.requests();
    ensure!(requests.len() == 6, "unexpected requests: {requests:?}");
    for request in &requests {
        ensure!(request.addresses.len() <= 1_000);
        ensure!(
            request
                .to_block
                .checked_sub(request.from_block)
                .and_then(|width| width.checked_add(1))
                .is_some_and(|width| width <= 2_000)
        );
    }
    ensure!(
        requests
            .iter()
            .any(|request| request.addresses.contains(&lock_address)),
        "lock address was omitted from the scanner filter"
    );
    let shapes = requests
        .iter()
        .map(|request| {
            (
                request.addresses.len(),
                request.from_block,
                request.to_block,
            )
        })
        .collect::<Vec<_>>();
    ensure!(
        shapes
            == vec![
                (1_000, 1, 2_000),
                (1, 1, 2_000),
                (1_000, 2_001, 4_000),
                (1, 2_001, 4_000),
                (1_000, 4_001, 4_001),
                (1, 4_001, 4_001),
            ],
        "unexpected request sharding: {shapes:?}"
    );
    Ok(())
}

async fn run_confirm_scenario(
    database: &TestDatabase,
    primary_anvil: &Anvil,
    lagging_anvil: &Anvil,
) -> Result<()> {
    let contracts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts");
    run_checked("forge", &["build"], Some(&contracts))?;
    let token = deploy_token(&contracts, &primary_anvil.rpc_url)?;
    let tracked = Address::from([0x91_u8; 20]);
    let account_id = seed_account(&database.app_pool).await?;
    insert_address(&database.app_pool, account_id, tracked, 1).await?;
    transfer(&primary_anvil.rpc_url, token, tracked, 1_000)?;
    primary_anvil.mine(2)?;

    let fixture = RouteFixture::create(token)?;
    let scanner_routes = load_route_files(std::slice::from_ref(&fixture.path))?
        .into_iter()
        .next()
        .context("one scanner route")?;
    let scanner_reader = EvmChain::new(&primary_anvil.rpc_url)?;
    ensure!(
        scan_once(&database.app_pool, &scanner_reader, &scanner_routes)
            .await?
            .inserted
            == 1
    );

    let mut route: RouteFile = serde_saphyr::from_str(&std::fs::read_to_string(&fixture.path)?)?;
    route.asset.decimals = 0;
    route.destination.unit_decimals = 0;
    route.screening.min_credit_minor = 1;
    let now = u64::try_from(chrono::Utc::now().timestamp())?;
    let primary_price: Arc<dyn PriceSource> =
        Arc::new(FixedPrice(observation("coinmetrics", 10_000_000, now)));
    let check_price: Arc<dyn PriceSource> =
        Arc::new(FixedPrice(observation("binance", 10_000_000, now)));
    let fx_price: Arc<dyn PriceSource> =
        Arc::new(FixedPrice(observation("kraken", 100_000_000, now)));
    let confirm = ConfirmStep::single(
        database.app_pool.clone(),
        route.clone(),
        EvmChain::new(&primary_anvil.rpc_url)?,
        EvmChain::new(&primary_anvil.rpc_url)?,
        Arc::clone(&primary_price),
        Some(Arc::clone(&check_price)),
        Some(Arc::clone(&fx_price)),
        Arc::new(MissingProductAnswer),
    );
    let pump = Pump::new(
        database.app_pool.clone(),
        Arc::new(NoopStepSet::build().with_detected(Box::new(confirm))),
        PumpConfig::default(),
    )?;
    let confirmed_id: Uuid = sqlx::query_scalar("SELECT id FROM deposits LIMIT 1")
        .fetch_one(&database.app_pool)
        .await?;
    ensure!(
        pump.run_once().await?
            == RunOnceResult::Applied {
                deposit_id: confirmed_id
            }
    );
    let confirmed = db::get_deposit(&database.app_pool, confirmed_id)
        .await?
        .context("confirmed deposit")?;
    ensure!(confirmed.state == topup_core::deposit::DepositState::Confirmed);
    ensure!(confirmed.price_scaled == Some(10_000_000));
    ensure!(confirmed.credit_minor == Some(topup_core::money::MinorAmount::new(100)));
    ensure!(confirmed.quote.is_some());
    let evidence: Value =
        sqlx::query_scalar("SELECT evidence FROM transitions WHERE deposit_id = $1")
            .bind(confirmed_id)
            .fetch_one(&database.app_pool)
            .await?;
    ensure!(evidence["stage"] == "confirmed");
    let outbox_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM outbox WHERE event_type = 'deposit.confirmed' AND payload->>'deposit_id' = $1",
    )
    .bind(confirmed_id.to_string())
    .fetch_one(&database.app_pool)
    .await?;
    ensure!(outbox_count == 1);

    transfer(&primary_anvil.rpc_url, token, tracked, 2_000)?;
    primary_anvil.mine(2)?;
    ensure!(
        scan_once(&database.app_pool, &scanner_reader, &scanner_routes)
            .await?
            .inserted
            == 1
    );
    let lagging_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM deposits WHERE state = 'detected' ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(&database.app_pool)
    .await?;
    let lagging_deposit = db::get_deposit(&database.app_pool, lagging_id)
        .await?
        .context("lagging deposit")?;
    let lagging_confirm = ConfirmStep::single(
        database.app_pool.clone(),
        route,
        EvmChain::new(&primary_anvil.rpc_url)?,
        EvmChain::new(&lagging_anvil.rpc_url)?,
        primary_price,
        Some(check_price),
        Some(fx_price),
        Arc::new(MissingProductAnswer),
    );
    let result = lagging_confirm.run(&lagging_deposit).await;
    ensure!(
        result.outcome
            == StepOutcome::Wait {
                reason: WaitReason::Finality
            }
    );
    Ok(())
}

struct FixedPrice(Observation);

#[async_trait]
impl PriceSource for FixedPrice {
    async fn observe(&self) -> Result<Observation, PriceError> {
        Ok(self.0.clone())
    }
}

fn observation(source: &str, price: u64, observed_at: u64) -> Observation {
    Observation {
        source: SourceId::new(source),
        price: ScaledPrice::new(price, PRICE_SCALE).expect("integration price"),
        observed_at: UnixSeconds::new(observed_at),
    }
}

fn required_url(name: &str) -> Option<String> {
    match env::var(name).ok().filter(|value| !value.is_empty()) {
        Some(value) => Some(value),
        None => {
            eprintln!("skipping scanner integration test: {name} is not set");
            None
        }
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

fn command_success(command: &str, arguments: &[&str]) -> bool {
    Command::new(command)
        .args(arguments)
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
    deploy_contract(contracts, rpc_url, "test/mocks/MockTokens.sol:MockERC20")
}

fn deploy_contract(contracts: &Path, rpc_url: &str, contract: &str) -> Result<Address> {
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
            contract,
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

fn mint_nft(rpc_url: &str, token: Address, recipient: Address, token_id: u64) -> Result<()> {
    run_checked(
        "cast",
        &[
            "send",
            "--rpc-url",
            rpc_url,
            "--private-key",
            ANVIL_PRIVATE_KEY,
            &format!("{token:#x}"),
            "mint(address,uint256)",
            &format!("{recipient:#x}"),
            &token_id.to_string(),
        ],
        None,
    )?;
    Ok(())
}

fn transfer(rpc_url: &str, token: Address, recipient: Address, amount: u64) -> Result<()> {
    run_checked(
        "cast",
        &[
            "send",
            "--rpc-url",
            rpc_url,
            "--private-key",
            ANVIL_PRIVATE_KEY,
            &format!("{token:#x}"),
            "mint(address,uint256)",
            ANVIL_DEPLOYER,
            &amount.to_string(),
        ],
        None,
    )?;
    run_checked(
        "cast",
        &[
            "send",
            "--rpc-url",
            rpc_url,
            "--private-key",
            ANVIL_PRIVATE_KEY,
            &format!("{token:#x}"),
            "transfer(address,uint256)",
            &format!("{recipient:#x}"),
            &amount.to_string(),
        ],
        None,
    )?;
    Ok(())
}

fn current_block(rpc_url: &str) -> Result<u64> {
    let output = run_checked("cast", &["block-number", "--rpc-url", rpc_url], None)?;
    String::from_utf8(output.stdout)?
        .trim()
        .parse::<u64>()
        .context("parse anvil block number")
}

async fn seed_account(pool: &PgPool) -> Result<Uuid> {
    let product_id = Uuid::new_v4();
    db::create_product(
        pool,
        &NewProduct {
            id: product_id,
            slug: "scanner-test".to_owned(),
            webhook_url: "https://product.test/webhooks".to_owned(),
            pubkey: "test-key".to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    let account_id = Uuid::new_v4();
    db::create_account(
        pool,
        &NewAccount {
            id: account_id,
            product_id,
            external_id: "workspace-scanner".to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    Ok(account_id)
}

async fn insert_address(
    pool: &PgPool,
    account_id: Uuid,
    address: Address,
    version: u64,
) -> Result<Uuid> {
    let id = Uuid::new_v4();
    db::insert_address(
        pool,
        &NewAddress {
            id,
            account_id,
            chain_id: CHAIN_ID,
            kind: AddressKind::Persistent,
            version,
            lock_ref: None,
            salt: B256::from([u8::try_from(version)?; 32]),
            address,
            retired_at: version
                .checked_sub(1)
                .filter(|value| *value > 0)
                .map(|_| chrono::Utc::now()),
        },
    )
    .await?;
    Ok(id)
}

async fn insert_lock_address(
    pool: &PgPool,
    account_id: Uuid,
    address: Address,
    index: u64,
) -> Result<Uuid> {
    let id = Uuid::new_v4();
    db::insert_address(
        pool,
        &NewAddress {
            id,
            account_id,
            chain_id: CHAIN_ID,
            kind: AddressKind::Lock,
            version: 0,
            lock_ref: Some(format!("lock-{index}")),
            salt: indexed_word(index),
            address,
            retired_at: None,
        },
    )
    .await?;
    Ok(id)
}

fn indexed_address(index: u64) -> Address {
    Address::from_word(indexed_word(index))
}

fn indexed_word(index: u64) -> B256 {
    let mut bytes = [0_u8; 32];
    bytes[24..].copy_from_slice(&index.to_be_bytes());
    B256::from(bytes)
}

fn mock_transfer_log(
    token: Address,
    recipient: Address,
    block_number: u64,
    marker: u8,
) -> TransferLog {
    TransferLog {
        tx_hash: B256::from([marker; 32]),
        log_index: 0,
        block_number,
        block_hash: B256::from([marker.saturating_add(10); 32]),
        block_time: DateTime::from_timestamp(i64::from(marker), 0).expect("test timestamp"),
        token,
        from: Address::from([0x77_u8; 20]),
        to: recipient,
        amount: AtomicAmount::new(U256::from(u64::from(marker))),
    }
}

async fn address_backfilled(pool: &PgPool, id: Uuid) -> Result<bool> {
    Ok(
        sqlx::query("SELECT backfilled FROM addresses WHERE id = $1")
            .bind(id)
            .fetch_one(pool)
            .await?
            .try_get(0)?,
    )
}

async fn deposit_count(pool: &PgPool) -> Result<i64> {
    Ok(sqlx::query("SELECT count(*) FROM deposits")
        .fetch_one(pool)
        .await?
        .try_get(0)?)
}

async fn assert_deposit_counts(pool: &PgPool, total: i64, unsupported: i64) -> Result<()> {
    let total_count: i64 = sqlx::query("SELECT count(*) FROM deposits")
        .fetch_one(pool)
        .await?
        .try_get(0)?;
    let unsupported_count: i64 = sqlx::query(
        "SELECT count(*) FROM deposits WHERE state = 'rejected' AND reason = 'unsupported_asset' AND route IS NULL AND route_version IS NULL",
    )
    .fetch_one(pool)
    .await?
    .try_get(0)?;
    ensure!(
        total_count == total,
        "expected {total} deposits, got {total_count}"
    );
    ensure!(
        unsupported_count == unsupported,
        "expected {unsupported} unsupported deposits, got {unsupported_count}"
    );
    Ok(())
}

async fn install_insert_failure(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql(
        r#"
        CREATE FUNCTION fail_c3_deposit_insert()
        RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            RAISE EXCEPTION 'forced scanner insert failure';
        END;
        $$;
        CREATE TRIGGER fail_c3_deposit_insert
        BEFORE INSERT ON deposits
        FOR EACH ROW EXECUTE FUNCTION fail_c3_deposit_insert();
        "#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn remove_insert_failure(pool: &PgPool) -> Result<()> {
    sqlx::raw_sql(
        r#"
        DROP TRIGGER fail_c3_deposit_insert ON deposits;
        DROP FUNCTION fail_c3_deposit_insert();
        "#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

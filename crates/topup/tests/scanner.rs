//! Anvil and PostgreSQL integration coverage for the C3 scanner.

mod support;

use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Row};
use topup::db::{self, AddressKind};
use topup::pump::{Pump, PumpConfig, RunOnceResult, Step, StepResult, StepSet};
use topup::routes::RouteSet;
use topup::scanner::{ChainRoutes, chain_routes, scan_once};
use topup::steps::confirm::ConfirmStep;
use topup_adapters::chain::evm::{
    ChainError, ChainReader, EvmClient, FinalizedHead, FinalizedReader, TransferLog,
};
use topup_adapters::pricing::{Observation, PriceError, PriceSource};
use topup_core::deposit::{StepOutcome, WaitReason};
use topup_core::money::{AtomicAmount, PRICE_SCALE, ScaledPrice};
use topup_core::route::RouteFile;
use topup_core::valuation::{SourceId, UnixSeconds};
use uuid::Uuid;

use support::TestDatabase;
use support::chain::{
    ANVIL_PRIVATE_KEY, Anvil, CHAIN_ID, contracts_dir, forge_create, run_checked,
};
use support::seed::{self, NewAccount, NewAddress, NewProduct};

/// One slot per epoch keeps anvil's finalized block close to the head for the finalized scanner.
const ANVIL_ARGS: &[&str] = &["--slots-in-an-epoch", "1"];
const ANVIL_DEPLOYER: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

#[derive(Clone, Debug, Eq, PartialEq)]
struct RecordedRequest {
    addresses: Vec<Address>,
    from_block: u64,
    to_block: u64,
}

struct RecordingReader {
    finalized: u64,
    finalized_time: DateTime<Utc>,
    requests: Mutex<Vec<RecordedRequest>>,
}

impl RecordingReader {
    fn new(finalized: u64, finalized_time: DateTime<Utc>) -> Self {
        Self {
            finalized,
            finalized_time,
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("request lock").clone()
    }
}

impl ChainReader for RecordingReader {
    async fn finalized_head(&self) -> Result<FinalizedHead, ChainError> {
        Ok(FinalizedHead {
            number: self.finalized,
            time: self.finalized_time,
        })
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
    async fn finalized_head(&self) -> Result<FinalizedHead, ChainError> {
        Ok(FinalizedHead {
            number: 4_000,
            time: DateTime::UNIX_EPOCH,
        })
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

/// Loads a route file through the same validation and grouping `topup run` uses.
fn scanner_route(path: &Path) -> Result<ChainRoutes> {
    let route: RouteFile = serde_saphyr::from_str(&std::fs::read_to_string(path)?)?;
    chain_routes(&RouteSet::new(vec![route]).map_err(anyhow::Error::msg)?)
        .into_iter()
        .next()
        .context("one chain route")
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
    let Some(anvil) = Anvil::start_if_available(ANVIL_ARGS).await? else {
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
    let Some(primary_anvil) = Anvil::start_if_available(ANVIL_ARGS).await? else {
        database.cleanup().await?;
        return Ok(());
    };
    let Some(lagging_anvil) = Anvil::start_if_available(ANVIL_ARGS).await? else {
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
    run_checked("forge", &["build"], Some(&contracts_dir()))?;
    let supported_token = deploy_token(&anvil.rpc_url)?;
    let unsupported_token = deploy_token(&anvil.rpc_url)?;
    let nft = forge_create(
        &anvil.rpc_url,
        "test/mocks/MockTokens.sol:MockERC721Transfer",
        &[],
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
    let routes = scanner_route(&route_fixture.path)?;
    let reader = reader(&anvil.rpc_url)?;
    let expected_cursor = reader.finalized_head().await?.number;
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
    ensure!(previous.number > 0);
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
        SELECT event.id, event.product_id, event.object_id, deposit.reason
        FROM outbox AS event
        JOIN deposits AS deposit ON deposit.id = event.object_id
        WHERE event.event_type = 'deposit.rejected' AND event.object_type = 'deposit'
        "#,
    )
    .fetch_all(pool)
    .await?;
    ensure!(
        rows.len() == 1,
        "expected one deposit.rejected event, got {}",
        rows.len()
    );
    let product_id: Uuid = sqlx::query_scalar("SELECT product_id FROM accounts WHERE id = $1")
        .bind(account_id)
        .fetch_one(pool)
        .await?;
    let deposit: Uuid = rows[0].try_get("object_id")?;
    ensure!(
        rows[0].try_get::<Option<String>, _>("reason")?.as_deref() == Some("unsupported_asset")
    );
    ensure!(
        rows[0].try_get::<Option<Uuid>, _>("product_id")? == Some(product_id),
        "event does not name the owning product"
    );
    ensure!(
        rows[0].try_get::<Uuid, _>("id")?
            == topup_core::identity::event_id("deposit.rejected", deposit),
        "event id is not derived from the deposit"
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
    let routes = scanner_route(&route_fixture.path)?;
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
    let routes = scanner_route(&route_fixture.path)?;
    let finalized_time = DateTime::from_timestamp(1_700_000_000, 0).context("finalized time")?;
    let reader = RecordingReader::new(4_001, finalized_time);
    let stats = scan_once(&database.app_pool, &reader, &routes).await?;
    ensure!(stats.cursor == 4_001);
    let scanned_block_time: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT scanned_block_time FROM cursors WHERE chain_id = $1")
            .bind(i64::try_from(CHAIN_ID)?)
            .fetch_one(&database.app_pool)
            .await?;
    ensure!(
        scanned_block_time == Some(finalized_time),
        "cursor must record the finalized head's block time, got {scanned_block_time:?}"
    );

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
    run_checked("forge", &["build"], Some(&contracts_dir()))?;
    let token = deploy_token(&primary_anvil.rpc_url)?;
    let tracked = Address::from([0x91_u8; 20]);
    let account_id = seed_account(&database.app_pool).await?;
    insert_address(&database.app_pool, account_id, tracked, 1).await?;
    transfer(&primary_anvil.rpc_url, token, tracked, 1_000)?;
    primary_anvil.mine(2)?;

    let fixture = RouteFixture::create(token)?;
    let scanner_routes = scanner_route(&fixture.path)?;
    let scanner_reader = reader(&primary_anvil.rpc_url)?;
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
        reader(&primary_anvil.rpc_url)?,
        reader(&primary_anvil.rpc_url)?,
        Arc::clone(&primary_price),
        Some(Arc::clone(&check_price)),
        Some(Arc::clone(&fx_price)),
    );
    let pump = Pump::new(
        database.app_pool.clone(),
        Arc::new(wait_steps().with_detected(Box::new(confirm))),
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
    // Confirmation announces nothing.
    let outbox_count: i64 = sqlx::query_scalar("SELECT count(*) FROM outbox WHERE object_id = $1")
        .bind(confirmed_id)
        .fetch_one(&database.app_pool)
        .await?;
    ensure!(outbox_count == 0);

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
        reader(&primary_anvil.rpc_url)?,
        reader(&lagging_anvil.rpc_url)?,
        primary_price,
        Some(check_price),
        Some(fx_price),
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

fn deploy_token(rpc_url: &str) -> Result<Address> {
    forge_create(rpc_url, "test/mocks/MockTokens.sol:MockERC20", &[])
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
    seed::create_product(
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
    seed::create_account(
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
    seed::insert_address(
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
    seed::insert_address(
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

/// Leaves every deposit waiting so only the step under test advances state.
struct WaitStep;

#[async_trait]
impl Step for WaitStep {
    async fn run(&self, _deposit: &db::Deposit) -> StepResult {
        StepResult::new(
            StepOutcome::Wait {
                reason: WaitReason::Paused,
            },
            serde_json::json!({"outcome": "wait"}),
        )
    }
}

fn wait_steps() -> StepSet {
    StepSet::new(Box::new(WaitStep), Box::new(WaitStep), Box::new(WaitStep))
}

fn reader(rpc_url: &str) -> Result<FinalizedReader> {
    Ok(FinalizedReader::new(Arc::new(EvmClient::new(rpc_url)?)))
}

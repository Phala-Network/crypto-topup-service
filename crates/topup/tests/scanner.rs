//! Anvil and PostgreSQL integration coverage for the C3 scanner.

use std::env;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::str::FromStr;
use std::time::Duration;

use alloy_primitives::{Address, B256};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool, Row};
use topup::db::{self, AddressKind, NewAccount, NewAddress, NewProduct};
use topup::scanner::{load_route_files, scan_once};
use topup_adapters::chain::evm::{ChainError, ChainReader, EvmChain};
use url::Url;
use uuid::Uuid;

const ANVIL_PRIVATE_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const ANVIL_DEPLOYER: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
const CHAIN_ID: u64 = 31_337;

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
            .connect(admin_url.as_str())
            .await?;
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

async fn run_scenario(database: &TestDatabase, anvil: &Anvil) -> Result<()> {
    let contracts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts");
    run_checked("forge", &["build"], Some(&contracts))?;
    let supported_token = deploy_token(&contracts, &anvil.rpc_url)?;
    let unsupported_token = deploy_token(&contracts, &anvil.rpc_url)?;
    let tracked_one = Address::from([0x11_u8; 20]);
    let tracked_two = Address::from([0x22_u8; 20]);
    let tracked_later = Address::from([0x33_u8; 20]);

    let account_id = seed_account(&database.app_pool).await?;
    insert_address(&database.app_pool, account_id, tracked_one, 1).await?;
    insert_address(&database.app_pool, account_id, tracked_two, 2).await?;
    transfer(&anvil.rpc_url, supported_token, tracked_one, 101)?;
    transfer(&anvil.rpc_url, supported_token, tracked_two, 202)?;
    transfer(&anvil.rpc_url, unsupported_token, tracked_one, 303)?;
    anvil.mine(2)?;

    let route_fixture = RouteFixture::create(supported_token)?;
    let routes = load_route_files(std::slice::from_ref(&route_fixture.path))?
        .into_iter()
        .next()
        .context("one chain route")?;
    let reader = EvmChain::new(&anvil.rpc_url)?;
    let first = scan_once(&database.app_pool, &reader, &routes).await?;
    ensure!(
        first.inserted == 3,
        "expected three deposits, got {first:?}"
    );
    assert_deposit_counts(&database.app_pool, 3, 1).await?;

    sqlx::query("UPDATE cursors SET scanned_block = 0 WHERE chain_id = $1")
        .bind(i64::try_from(CHAIN_ID)?)
        .execute(&database.app_pool)
        .await?;
    let duplicate = scan_once(&database.app_pool, &reader, &routes).await?;
    ensure!(duplicate.inserted == 0, "duplicate logs inserted again");
    assert_deposit_counts(&database.app_pool, 3, 1).await?;

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
            settlement_url: "https://product.test/settlements".to_owned(),
            webhook_url: "https://product.test/webhooks".to_owned(),
            pubkey: "test-key".to_owned(),
            kid: "test/v1".to_owned(),
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

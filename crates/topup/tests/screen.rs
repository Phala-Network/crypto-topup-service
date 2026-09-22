//! Anvil and PostgreSQL integration coverage for the C5 screen step.

use std::env;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::Command;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool, Row};
use topup::db::{self, AddressKind, NewAccount, NewAddress, NewDeposit, NewProduct};
use topup::pump::{NoopStep, Pump, PumpConfig, RunOnceResult, Step, StepSet};
use topup::steps::screen::{ScreenRoute, ScreenStep};
use topup_adapters::risk::oracle::SanctionsOracle;
use topup_core::deposit::{DepositState, RejectReason, RetryError, StepOutcome, WaitReason};
use topup_core::identity::deposit_id;
use topup_core::money::AtomicAmount;
use topup_core::screening::Bounds;
use url::Url;
use uuid::Uuid;

const ANVIL_PRIVATE_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";

type TestFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

struct TestContext {
    admin_pool: PgPool,
    owner_pool: PgPool,
    app_pool: PgPool,
    database_name: String,
    app_role: String,
}

impl TestContext {
    async fn create() -> Result<Option<Self>> {
        let Some(owner_template) = required_env("MIGRATE_DATABASE_URL") else {
            return Ok(None);
        };
        let Some(app_template) = required_env("DATABASE_URL") else {
            return Ok(None);
        };

        let mut admin_url = Url::parse(&owner_template)?;
        admin_url.set_path("/postgres");
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(admin_url.as_str())
            .await?;
        sqlx::query("SELECT pg_advisory_lock(704_205_001)")
            .execute(&admin_pool)
            .await?;

        let suffix = Uuid::new_v4().simple().to_string();
        let database_name = format!("topup_c5_{suffix}");
        let app_role = format!("topup_c5_app_{suffix}");
        let password = format!("c5_{suffix}");
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
            .map_err(|()| anyhow::anyhow!("DATABASE_URL cannot accept an application username"))?;
        app_url
            .set_password(Some(&password))
            .map_err(|()| anyhow::anyhow!("DATABASE_URL cannot accept an application password"))?;
        app_url.set_path(&format!("/{database_name}"));
        let app_pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(app_url.as_str())
            .await?;

        sqlx::query("SELECT pg_advisory_unlock(704_205_001)")
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

fn required_env(name: &str) -> Option<String> {
    match env::var(name).ok().filter(|value| !value.is_empty()) {
        Some(value) => Some(value),
        None => {
            eprintln!("skipping screen integration test: {name} is not set");
            None
        }
    }
}

async fn with_database<F>(test: F) -> Result<()>
where
    F: for<'a> FnOnce(&'a TestContext) -> TestFuture<'a>,
{
    let Some(context) = TestContext::create().await? else {
        return Ok(());
    };
    let result = test(&context).await;
    let cleanup = context.cleanup().await;
    result.and(cleanup)
}

#[tokio::test]
async fn anvil_oracle_and_postgres_pump_cover_clear_reject_and_outage() -> Result<()> {
    let Some(rpc_url) = required_env("ANVIL_RPC_URL") else {
        return Ok(());
    };
    let oracle = deploy_oracle(&rpc_url)?;
    let sanctioned = Address::repeat_byte(0x22);
    set_sanctioned(&rpc_url, oracle, sanctioned)?;
    let block_number = current_block(&rpc_url)?;

    with_database(|context| {
        Box::pin(async move {
            let seed = seed_account(&context.app_pool).await?;
            let clear_id = insert_confirmed(
                &context.app_pool,
                seed,
                1,
                Address::repeat_byte(0x11),
                block_number,
            )
            .await?;
            let sanctioned_id =
                insert_confirmed(&context.app_pool, seed, 2, sanctioned, block_number).await?;
            let step = screen_step(&context.app_pool, &rpc_url, oracle)?;

            let clear = db::get_deposit(&context.app_pool, clear_id)
                .await?
                .context("clear deposit")?;
            let clear_result = step.run(&clear).await;
            ensure!(clear_result.outcome == StepOutcome::Advance);
            ensure!(clear_result.events.is_empty());
            ensure!(clear_result.evidence["block_number"] == block_number);
            ensure!(clear_result.evidence["provider_a"] == "clear");
            ensure!(clear_result.evidence["provider_b"] == "clear");

            let sanctioned_deposit = db::get_deposit(&context.app_pool, sanctioned_id)
                .await?
                .context("sanctioned deposit")?;
            let rejected = step.run(&sanctioned_deposit).await;
            ensure!(
                rejected.outcome == StepOutcome::Reject(RejectReason::Sanctioned),
                "unexpected sanctions result: {:?}",
                rejected.outcome
            );
            ensure!(rejected.events.len() == 1);
            ensure!(rejected.events[0].event_type == "deposit.rejected");
            ensure!(rejected.events[0].payload["reason"] == "sanctioned");
            ensure!(rejected.evidence["provider_a"] == "sanctioned");
            ensure!(rejected.evidence["provider_b"] == "sanctioned");

            db::set_account_paused_scopes(
                &context.app_pool,
                seed.account_id,
                &["settlement".to_owned()],
            )
            .await?
            .context("pause account")?;
            let account_wait = step.run(&clear).await;
            ensure!(
                account_wait.outcome
                    == StepOutcome::Wait {
                        reason: WaitReason::Paused,
                    }
            );
            db::set_account_paused_scopes(&context.app_pool, seed.account_id, &[])
                .await?
                .context("resume account")?;
            db::set_product_paused_scopes(
                &context.app_pool,
                seed.product_id,
                &["settlement".to_owned()],
            )
            .await?
            .context("pause product")?;
            let product_wait = step.run(&clear).await;
            ensure!(
                product_wait.outcome
                    == StepOutcome::Wait {
                        reason: WaitReason::Paused,
                    }
            );
            db::set_product_paused_scopes(&context.app_pool, seed.product_id, &[])
                .await?
                .context("resume product")?;

            let down_source = Arc::new(SanctionsOracle::new(
                &rpc_url,
                "http://127.0.0.1:1",
                oracle,
                StdDuration::from_millis(200),
            )?);
            let down_step = ScreenStep::new(
                context.app_pool.clone(),
                [ScreenRoute::new("down", 1, oracle, bounds(), down_source)],
            )?;
            let mut down_deposit = clear.clone();
            down_deposit.route = Some("down".to_owned());
            let unavailable = down_step.run(&down_deposit).await;
            ensure!(
                unavailable.outcome
                    == StepOutcome::Retry {
                        error: RetryError::SanctionsInconclusive,
                    }
            );
            ensure!(unavailable.evidence["provider_a"] == "clear");
            ensure!(unavailable.evidence["provider_b"] == "unavailable");

            let pump_screen = screen_step(&context.app_pool, &rpc_url, oracle)?;
            let pump = Pump::new(
                context.app_pool.clone(),
                Arc::new(StepSet::new(
                    Box::new(NoopStep),
                    Box::new(pump_screen),
                    Box::new(NoopStep),
                    Box::new(NoopStep),
                )),
                PumpConfig::default(),
            )?;
            for expected_id in [clear_id, sanctioned_id] {
                ensure!(
                    pump.run_once().await?
                        == RunOnceResult::Applied {
                            deposit_id: expected_id,
                        }
                );
            }

            let clear = db::get_deposit(&context.app_pool, clear_id)
                .await?
                .context("clear deposit after pump")?;
            ensure!(clear.state == DepositState::Cleared);
            let rejected = db::get_deposit(&context.app_pool, sanctioned_id)
                .await?
                .context("rejected deposit after pump")?;
            ensure!(rejected.state == DepositState::Rejected);
            ensure!(rejected.reason == Some(RejectReason::Sanctioned));

            let clear_evidence = transition_evidence(&context.app_pool, clear_id).await?;
            ensure!(clear_evidence["provider_a"] == "clear");
            ensure!(clear_evidence["block_number"] == block_number);
            let rejected_evidence = transition_evidence(&context.app_pool, sanctioned_id).await?;
            ensure!(rejected_evidence["provider_a"] == "sanctioned");
            ensure!(rejected_evidence["oracle"] == format!("{oracle:#x}"));

            let outbox = sqlx::query(
                "SELECT event_type, payload FROM outbox WHERE payload->>'deposit_id' = $1",
            )
            .bind(sanctioned_id.to_string())
            .fetch_one(&context.app_pool)
            .await?;
            let event_type: String = outbox.try_get("event_type")?;
            let payload: Value = outbox.try_get("payload")?;
            ensure!(event_type == "deposit.rejected");
            ensure!(payload["reason"] == "sanctioned");
            ensure!(payload["product_id"] == seed.product_id.to_string());
            Ok(())
        })
    })
    .await
}

fn screen_step(pool: &PgPool, rpc_url: &str, oracle: Address) -> Result<ScreenStep> {
    let source = Arc::new(SanctionsOracle::new(
        rpc_url,
        rpc_url,
        oracle,
        StdDuration::from_secs(2),
    )?);
    Ok(ScreenStep::new(
        pool.clone(),
        [ScreenRoute::new("screen", 1, oracle, bounds(), source)],
    )?)
}

fn bounds() -> Bounds {
    Bounds {
        min_atomic: AtomicAmount::new(U256::from(10)),
        max_atomic: AtomicAmount::new(U256::from(20)),
    }
}

#[derive(Clone, Copy)]
struct Seed {
    product_id: Uuid,
    account_id: Uuid,
    address_id: Uuid,
}

async fn seed_account(pool: &PgPool) -> Result<Seed> {
    let product = NewProduct {
        id: Uuid::new_v4(),
        slug: "product-c5".to_owned(),
        settlement_url: "https://product.test/settlements".to_owned(),
        webhook_url: "https://product.test/webhooks".to_owned(),
        pubkey: "public-key-c5".to_owned(),
        kid: "product/c5".to_owned(),
        paused_scopes: Vec::new(),
    };
    db::create_product(pool, &product).await?;
    let account = NewAccount {
        id: Uuid::new_v4(),
        product_id: product.id,
        external_id: "workspace-c5".to_owned(),
        paused_scopes: Vec::new(),
    };
    db::create_account(pool, &account).await?;
    let address = NewAddress {
        id: Uuid::new_v4(),
        account_id: account.id,
        chain_id: 31_337,
        kind: AddressKind::Persistent,
        version: 1,
        lock_ref: None,
        salt: B256::repeat_byte(0x33),
        address: Address::repeat_byte(0x44),
        retired_at: None,
    };
    db::insert_address(pool, &address).await?;
    Ok(Seed {
        product_id: product.id,
        account_id: account.id,
        address_id: address.id,
    })
}

async fn insert_confirmed(
    pool: &PgPool,
    seed: Seed,
    number: u8,
    from_address: Address,
    block_number: u64,
) -> Result<Uuid> {
    let deposit = NewDeposit {
        chain_id: 31_337,
        tx_hash: B256::repeat_byte(number),
        log_index: 0,
        block_number,
        block_hash: B256::repeat_byte(number.wrapping_add(1)),
        block_time: Utc::now(),
        address_id: seed.address_id,
        account_id: seed.account_id,
        route: Some("screen".to_owned()),
        route_version: Some(1),
        asset_contract: Address::repeat_byte(0x55),
        from_address,
        amount_atomic: AtomicAmount::new(U256::from(15)),
        state: DepositState::Confirmed,
        reason: None,
        next_attempt_at: Utc::now() - Duration::seconds(1),
    };
    let id = deposit_id(deposit.chain_id, deposit.tx_hash, deposit.log_index);
    ensure!(db::insert_deposit(pool, &deposit).await?);
    Ok(id)
}

async fn transition_evidence(pool: &PgPool, deposit_id: Uuid) -> Result<Value> {
    Ok(
        sqlx::query("SELECT evidence FROM transitions WHERE deposit_id = $1")
            .bind(deposit_id)
            .fetch_one(pool)
            .await?
            .try_get("evidence")?,
    )
}

fn contracts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts")
}

fn deploy_oracle(rpc_url: &str) -> Result<Address> {
    let output = Command::new("forge")
        .current_dir(contracts_dir())
        .args([
            "create",
            "--rpc-url",
            rpc_url,
            "--private-key",
            ANVIL_PRIVATE_KEY,
            "--broadcast",
            "--json",
            "test/mocks/MockSanctionsOracle.sol:MockSanctionsOracle",
        ])
        .output()
        .context("deploy MockSanctionsOracle with forge")?;
    ensure!(
        output.status.success(),
        "forge create failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout)?;
    let deployed = response["deployedTo"]
        .as_str()
        .context("forge create omitted deployedTo")?;
    Address::from_str(deployed).context("forge returned an invalid deployment address")
}

fn set_sanctioned(rpc_url: &str, oracle: Address, account: Address) -> Result<()> {
    let output = Command::new("cast")
        .args([
            "send",
            "--rpc-url",
            rpc_url,
            "--private-key",
            ANVIL_PRIVATE_KEY,
            &format!("{oracle:#x}"),
            "setSanctioned(address,bool)",
            &format!("{account:#x}"),
            "true",
        ])
        .output()?;
    ensure!(
        output.status.success(),
        "cast send failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn current_block(rpc_url: &str) -> Result<u64> {
    let output = Command::new("cast")
        .args(["block-number", "--rpc-url", rpc_url])
        .output()?;
    ensure!(output.status.success(), "cast block-number failed");
    String::from_utf8(output.stdout)?
        .trim()
        .parse()
        .context("cast returned an invalid block number")
}

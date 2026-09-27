//! PostgreSQL and Anvil integration coverage for the C5 screen step.

mod support;

use std::process::Command;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::{PgPool, Row};
use topup::db::{self, AddressKind, NewDeposit};
use topup::pump::{Pump, PumpConfig, RunOnceResult, Step, StepResult, StepSet};
use topup::steps::screen::{ScreenRoute, ScreenStep};
use topup_adapters::chain::evm::EvmClient;
use topup_adapters::risk::oracle::{SanctionsOracle, SanctionsSource};
use topup_core::deposit::{DepositState, RejectReason, RetryError, StepOutcome, WaitReason};
use topup_core::identity::{credited_event_id, deposit_id, event_id};
use topup_core::money::AtomicAmount;
use topup_core::screening::{Bounds, SanctionsAnswer, SanctionsResult};
use uuid::Uuid;

use support::chain::{ANVIL_PRIVATE_KEY, Anvil, forge_create};
use support::seed::{self, NewAccount, NewAddress, NewProduct};
use support::with_database;

struct MockSanctionsSource {
    sanctioned: Address,
}

#[async_trait]
impl SanctionsSource for MockSanctionsSource {
    async fn sanctions(&self, address: Address, block_number: u64) -> SanctionsResult {
        let answer = if address == self.sanctioned {
            SanctionsAnswer::Sanctioned
        } else {
            SanctionsAnswer::Clear
        };
        SanctionsResult {
            provider_a: answer,
            provider_b: answer,
            block_number,
        }
    }
}

#[tokio::test]
async fn postgres_pump_persists_screening_transitions_pauses_and_outbox() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let seed = seed_account(&context.app_pool).await?;
            let block_number = 123;
            let sanctioned = Address::repeat_byte(0x22);
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
            let step = mock_screen_step(&context.app_pool, sanctioned)?;
            let clear = db::get_deposit(&context.app_pool, clear_id)
                .await?
                .context("clear deposit")?;

            set_route_pauses(&context.app_pool, &["settlement"]).await?;
            let route_wait = step.run(&clear).await;
            ensure!(
                route_wait.outcome
                    == StepOutcome::Wait {
                        reason: WaitReason::Paused,
                    }
            );
            ensure!(route_wait.evidence["pause_scopes"]["account"] == serde_json::json!([]));
            ensure!(route_wait.evidence["pause_scopes"]["product"] == serde_json::json!([]));
            ensure!(
                route_wait.evidence["pause_scopes"]["route"] == serde_json::json!(["settlement"])
            );

            set_route_pauses(&context.app_pool, &[]).await?;
            let resumed = step.run(&clear).await;
            ensure!(resumed.outcome == StepOutcome::Advance);

            set_route_pauses(&context.app_pool, &["flush"]).await?;
            let unrelated_pause = step.run(&clear).await;
            ensure!(unrelated_pause.outcome == StepOutcome::Advance);
            ensure!(
                unrelated_pause.evidence["pause_scopes"]["route"] == serde_json::json!(["flush"])
            );
            set_route_pauses(&context.app_pool, &[]).await?;

            let pump = Pump::new(
                context.app_pool.clone(),
                Arc::new(wait_steps().with_confirmed(Box::new(step))),
                PumpConfig::default(),
            )?;
            let mut applied = Vec::new();
            for _ in 0..2 {
                let RunOnceResult::Applied { deposit_id } = pump.run_once().await? else {
                    anyhow::bail!("screen pump did not apply a due deposit");
                };
                applied.push(deposit_id);
            }
            applied.sort_unstable();
            let mut expected = vec![clear_id, sanctioned_id];
            expected.sort_unstable();
            ensure!(applied == expected);

            let clear = db::get_deposit(&context.app_pool, clear_id)
                .await?
                .context("clear deposit after pump")?;
            ensure!(clear.state == DepositState::Credited);
            let rejected = db::get_deposit(&context.app_pool, sanctioned_id)
                .await?
                .context("rejected deposit after pump")?;
            ensure!(rejected.state == DepositState::Rejected);
            ensure!(rejected.reason == Some(RejectReason::Sanctioned));

            let clear_evidence = transition_evidence(&context.app_pool, clear_id).await?;
            ensure!(clear_evidence["provider_a"] == "clear");
            ensure!(clear_evidence["block_number"] == block_number);
            ensure!(clear_evidence["pause_scopes"]["route"] == serde_json::json!([]));
            let rejected_evidence = transition_evidence(&context.app_pool, sanctioned_id).await?;
            ensure!(rejected_evidence["provider_a"] == "sanctioned");
            ensure!(rejected_evidence["oracle"] == format!("{:#x}", Address::repeat_byte(9)));

            // Each event names its product and deposit; its data, the deposit's API
            // representation, is rendered when it is first delivered.
            for (deposit, event_type, id) in [
                (
                    sanctioned_id,
                    "deposit.rejected",
                    event_id("deposit.rejected", sanctioned_id),
                ),
                (clear_id, "deposit.credited", credited_event_id(clear_id)),
            ] {
                let event = sqlx::query(
                    "SELECT id, event_type, product_id, object_type, payload FROM outbox \
                     WHERE object_id = $1",
                )
                .bind(deposit)
                .fetch_one(&context.app_pool)
                .await?;
                ensure!(event.try_get::<Uuid, _>("id")? == id);
                ensure!(event.try_get::<String, _>("event_type")? == event_type);
                ensure!(event.try_get::<Option<Uuid>, _>("product_id")? == Some(seed.product_id));
                ensure!(
                    event
                        .try_get::<Option<String>, _>("object_type")?
                        .as_deref()
                        == Some("deposit")
                );
                ensure!(event.try_get::<Value, _>("payload")? == serde_json::json!({}));
            }
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn anvil_oracle_uses_recorded_blocks_and_maps_live_results() -> Result<()> {
    let Some(anvil) = Anvil::start_if_available(&[]).await? else {
        return Ok(());
    };
    let rpc_url = anvil.rpc_url.clone();
    let oracle = forge_create(
        &rpc_url,
        "test/mocks/MockSanctionsOracle.sol:MockSanctionsOracle",
        &[],
    )?;
    let account = Address::repeat_byte(0x22);
    let recorded_block = current_block(&rpc_url)?;
    let source = Arc::new(SanctionsOracle::new(
        client(&rpc_url, StdDuration::from_secs(2))?,
        client(&rpc_url, StdDuration::from_secs(2))?,
        oracle,
    )?);
    let before = source.sanctions(account, recorded_block).await;
    ensure!(before.provider_a == SanctionsAnswer::Clear);
    ensure!(before.provider_b == SanctionsAnswer::Clear);

    set_sanctioned(&rpc_url, oracle, account, true)?;
    let latest_block = current_block(&rpc_url)?;
    ensure!(latest_block > recorded_block);
    let historical = source.sanctions(account, recorded_block).await;
    let latest = source.sanctions(account, latest_block).await;
    ensure!(historical.provider_a == SanctionsAnswer::Clear);
    ensure!(historical.provider_b == SanctionsAnswer::Clear);
    ensure!(latest.provider_a == SanctionsAnswer::Sanctioned);
    ensure!(latest.provider_b == SanctionsAnswer::Sanctioned);

    with_database(|context| {
        let rpc_url = rpc_url.clone();
        let source = Arc::<SanctionsOracle>::clone(&source);
        Box::pin(async move {
            let seed = seed_account(&context.app_pool).await?;
            let old_id =
                insert_confirmed(&context.app_pool, seed, 3, account, recorded_block).await?;
            let latest_id =
                insert_confirmed(&context.app_pool, seed, 4, account, latest_block).await?;
            let step = ScreenStep::new(
                context.app_pool.clone(),
                [ScreenRoute::new("screen", 1, oracle, bounds(), source)],
            )?;

            let old_deposit = db::get_deposit(&context.app_pool, old_id)
                .await?
                .context("historical deposit")?;
            let old_result = step.run(&old_deposit).await;
            ensure!(old_result.outcome == StepOutcome::Advance);
            ensure!(old_result.evidence["block_number"] == recorded_block);
            ensure!(old_result.evidence["provider_a"] == "clear");

            let latest_deposit = db::get_deposit(&context.app_pool, latest_id)
                .await?
                .context("latest deposit")?;
            let rejected = step.run(&latest_deposit).await;
            ensure!(rejected.outcome == StepOutcome::Reject(RejectReason::Sanctioned));
            ensure!(rejected.events.len() == 1);
            ensure!(rejected.events[0].event_type == "deposit.rejected");
            ensure!(rejected.evidence["provider_a"] == "sanctioned");
            ensure!(rejected.evidence["provider_b"] == "sanctioned");

            let down_source = Arc::new(SanctionsOracle::new(
                client(&rpc_url, StdDuration::from_millis(200))?,
                client("http://127.0.0.1:1", StdDuration::from_millis(200))?,
                oracle,
            )?);
            let down_step = ScreenStep::new(
                context.app_pool.clone(),
                [ScreenRoute::new("down", 1, oracle, bounds(), down_source)],
            )?;
            let mut down_deposit = latest_deposit;
            down_deposit.route = Some("down".to_owned());
            down_deposit.from_address = Address::repeat_byte(0x11);
            let unavailable = down_step.run(&down_deposit).await;
            ensure!(
                unavailable.outcome
                    == StepOutcome::Retry {
                        error: RetryError::SanctionsInconclusive,
                    }
            );
            ensure!(unavailable.evidence["provider_a"] == "clear");
            ensure!(unavailable.evidence["provider_b"] == "unavailable");
            Ok(())
        })
    })
    .await
}

fn mock_screen_step(pool: &PgPool, sanctioned: Address) -> Result<ScreenStep> {
    Ok(ScreenStep::new(
        pool.clone(),
        [ScreenRoute::new(
            "screen",
            1,
            Address::repeat_byte(9),
            bounds(),
            Arc::new(MockSanctionsSource { sanctioned }),
        )],
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
        webhook_url: "https://product.test/webhooks".to_owned(),
        pubkey: "public-key-c5".to_owned(),
        paused_scopes: Vec::new(),
    };
    seed::create_product(pool, &product).await?;
    let account = NewAccount {
        id: Uuid::new_v4(),
        product_id: product.id,
        external_id: "workspace-c5".to_owned(),
        paused_scopes: Vec::new(),
    };
    seed::create_account(pool, &account).await?;
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
    seed::insert_address(pool, &address).await?;
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
    // The confirm step stores the valuation the credit carries.
    sqlx::query(
        r#"
        UPDATE deposits
        SET valuation_at = '2026-09-28T00:00:00Z', price_scaled = 250000000,
            price_source = 'spot', credit_minor = 37
        WHERE id = $1
        "#,
    )
    .bind(id)
    .execute(pool)
    .await?;
    Ok(id)
}

async fn set_route_pauses(pool: &PgPool, scopes: &[&str]) -> Result<()> {
    let scopes = scopes.iter().map(ToString::to_string).collect::<Vec<_>>();
    sqlx::query(
        r#"
        INSERT INTO route_pauses (route, paused_scopes)
        VALUES ('screen', $1)
        ON CONFLICT (route) DO UPDATE SET paused_scopes = EXCLUDED.paused_scopes
        "#,
    )
    .bind(scopes)
    .execute(pool)
    .await?;
    Ok(())
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

fn set_sanctioned(
    rpc_url: &str,
    oracle: Address,
    account: Address,
    sanctioned: bool,
) -> Result<()> {
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
            if sanctioned { "true" } else { "false" },
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

fn client(rpc_url: &str, timeout: StdDuration) -> Result<Arc<EvmClient>> {
    Ok(Arc::new(EvmClient::with_timeout(rpc_url, timeout)?))
}

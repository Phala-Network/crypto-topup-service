//! C7 end-to-end flusher coverage against disposable PostgreSQL and Anvil instances.

#![cfg(feature = "dev-signer")]

use std::env;
use std::future::Future;
use std::net::TcpListener;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use topup::db::{AddressKind, NewAccount, NewAddress, NewDeposit, NewProduct};
use topup::flusher::{
    AlertSink, AlloyChainClient, FlushAlert, Flusher, FlusherPolicy, Planner, PriceError,
    PriceSource, RunResult,
};
use topup_adapters::signer::DevSigner;
use topup_adapters::signer::actor::SignerHandle;
use topup_core::SecretKey32;
use topup_core::address::forwarder_address;
use topup_core::deposit::{DepositState, RejectReason};
use topup_core::identity::deposit_id;
use topup_core::money::{AtomicAmount, Bps, PRICE_SCALE, ScaledPrice};
use topup_core::route::RouteFile;
use url::Url;
use uuid::Uuid;

const ADMIN_ADDRESS: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";
const ADMIN_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const OPERATOR_ADDRESS: &str = "0x90f79bf6eb2c4f870365e785982e1f101e93b906";
const OPERATOR_KEY: &str = "7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6";
const ROTATED_ADDRESS: &str = "0x70997970c51812dc3a010c7d01b50e0d17dc79c8";
const ROTATED_KEY: &str = "59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";
const TREASURY: &str = "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc";
const TOKEN_AMOUNT: &str = "100000000000000000000";

type TestFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

struct Database {
    admin: PgPool,
    pool: PgPool,
    name: String,
}

impl Database {
    async fn create() -> Result<Option<Self>> {
        let Some(template) = env::var("MIGRATE_DATABASE_URL")
            .ok()
            .filter(|value| !value.is_empty())
        else {
            eprintln!("skipping flusher integration test: MIGRATE_DATABASE_URL is not set");
            return Ok(None);
        };
        let mut admin_url = Url::parse(&template)?;
        admin_url.set_path("/postgres");
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(admin_url.as_str())
            .await?;
        let name = format!("topup_c7_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE \"{name}\""))
            .execute(&admin)
            .await?;
        let mut database_url = Url::parse(&template)?;
        database_url.set_path(&format!("/{name}"));
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(database_url.as_str())
            .await?;
        topup::db::migrate(&pool).await?;
        Ok(Some(Self { admin, pool, name }))
    }

    async fn cleanup(self) -> Result<()> {
        self.pool.close().await;
        sqlx::query(&format!("DROP DATABASE \"{}\" WITH (FORCE)", self.name))
            .execute(&self.admin)
            .await?;
        self.admin.close().await;
        Ok(())
    }
}

struct Anvil {
    child: Child,
    rpc_url: String,
}

impl Anvil {
    fn start() -> Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let child = Command::new("anvil")
            .args([
                "--silent",
                "--port",
                &port.to_string(),
                "--chain-id",
                "31337",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start anvil")?;
        let rpc_url = format!("http://127.0.0.1:{port}");
        for _ in 0..100 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return Ok(Self { child, rpc_url });
            }
            std::thread::sleep(StdDuration::from_millis(25));
        }
        bail!("anvil did not start")
    }
}

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Clone)]
struct FixedPrice;

#[async_trait]
impl PriceSource for FixedPrice {
    async fn price_usd(&self, _asset: &str) -> Result<ScaledPrice, PriceError> {
        ScaledPrice::new(25_000_000, PRICE_SCALE).map_err(|error| PriceError(error.to_string()))
    }
}

#[derive(Default)]
struct Alerts(Mutex<Vec<FlushAlert>>);

impl AlertSink for Alerts {
    fn emit(&self, alert: FlushAlert) {
        self.0.lock().expect("alert mutex is available").push(alert);
    }
}

#[tokio::test]
async fn timed_out_rpc_does_not_hold_the_operator_lock() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let endpoint = format!("http://{}", listener.local_addr()?);
            let stalled_server = tokio::spawn(async move {
                if let Ok((_stream, _address)) = listener.accept().await {
                    std::future::pending::<()>().await;
                }
            });
            let route = test_route(Address::from([1; 20]), Address::from([2; 20]))?;
            let chain = Arc::new(AlloyChainClient::connect_http_with_policy(
                &endpoint,
                StdDuration::from_millis(50),
                10,
            )?);
            let signer = signer_handle(OPERATOR_KEY)?;
            let flusher = Flusher::new(
                database.pool.clone(),
                chain,
                signer,
                Arc::new(Alerts::default()),
                FlusherPolicy::default(),
            );
            ensure!(flusher.send_next(&route).await.is_err());

            let operator = Address::from_str(OPERATOR_ADDRESS)?;
            let mut transaction = database.pool.begin().await?;
            tokio::time::timeout(
                StdDuration::from_millis(200),
                topup::db::lock_operator(&mut transaction, route.chain.chain_id, operator),
            )
            .await
            .context("operator lock remained held after RPC timeout")??;
            transaction.rollback().await?;
            stalled_server.abort();
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn anvil_flush_lifecycle_covers_linkage_replacement_recovery_rotation_and_bisect()
-> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let anvil = Anvil::start()?;
            let root = repository_root();
            let factory = deploy(
                &root,
                &anvil.rpc_url,
                "src/ForwarderFactory.sol:ForwarderFactory",
                &[ADMIN_ADDRESS, TREASURY],
            )?;
            let token = deploy(
                &root,
                &anvil.rpc_url,
                "test/mocks/MockTokens.sol:MockERC20",
                &[],
            )?;
            grant_operator(&anvil.rpc_url, factory, OPERATOR_ADDRESS, ADMIN_KEY)?;
            let implementation =
                cast_call_address(&anvil.rpc_url, factory, "implementation()(address)", &[])?;
            let mut route = test_route(factory, token)?;
            let seeded = seed_addresses(&database.pool, factory, implementation).await?;
            for address in &seeded {
                cast_send(
                    &anvil.rpc_url,
                    token,
                    ADMIN_KEY,
                    "mint(address,uint256)",
                    &[&format!("{:#x}", address.physical), TOKEN_AMOUNT],
                )?;
            }
            let credited = insert_deposit(
                &database.pool,
                &seeded[0],
                token,
                DepositFixture {
                    state: DepositState::Credited,
                    reason: None,
                    block_number: 1,
                    log_index: 0,
                    number: 1,
                },
            )
            .await?;
            let rejected = insert_deposit(
                &database.pool,
                &seeded[1],
                token,
                DepositFixture {
                    state: DepositState::Rejected,
                    reason: Some(RejectReason::ProductRefused),
                    block_number: 1,
                    log_index: 1,
                    number: 2,
                },
            )
            .await?;
            let chain = Arc::new(AlloyChainClient::connect_http(&anvil.rpc_url)?);
            let alerts = Arc::new(Alerts::default());
            let signer = signer_handle(OPERATOR_KEY)?;
            let planner = Planner::new(
                database.pool.clone(),
                chain.clone(),
                signer.clone(),
                Arc::new(FixedPrice),
                alerts.clone(),
            );
            let flusher = Flusher::new(
                database.pool.clone(),
                chain.clone(),
                signer.clone(),
                alerts.clone(),
                FlusherPolicy::default(),
            );

            let first_flush = planner.plan(&route).await?.context("plan first flush")?;
            let mut changed_route = route.clone();
            changed_route.chain.contracts.forwarder_factory = Address::from([0x77; 20]);
            ensure!(
                flusher
                    .send_next(&changed_route)
                    .await
                    .context("send first flush")?
                    == RunResult::Sent {
                        flush_id: first_flush
                    }
            );
            finalize(&anvil.rpc_url)?;
            ensure!(
                flusher.maintain_sent(&changed_route).await?
                    == Some(RunResult::Confirmed {
                        flush_id: first_flush
                    })
            );
            assert_deposit(&database.pool, credited, "swept", Some(first_flush)).await?;
            assert_deposit(&database.pool, rejected, "rejected", Some(first_flush)).await?;
            let event_position = first_event_position(&database.pool, first_flush).await?;

            let backfilled = insert_deposit(
                &database.pool,
                &seeded[0],
                token,
                DepositFixture {
                    state: DepositState::Credited,
                    reason: None,
                    block_number: event_position.0,
                    log_index: event_position.1.saturating_sub(1),
                    number: 3,
                },
            )
            .await?;
            assert_deposit(&database.pool, backfilled, "swept", Some(first_flush)).await?;
            let after = insert_deposit(
                &database.pool,
                &seeded[0],
                token,
                DepositFixture {
                    state: DepositState::Credited,
                    reason: None,
                    block_number: event_position.0,
                    log_index: event_position.1 + 1,
                    number: 4,
                },
            )
            .await?;
            assert_deposit(&database.pool, after, "credited", None).await?;

            cast_send(
                &anvil.rpc_url,
                token,
                ADMIN_KEY,
                "mint(address,uint256)",
                &[&format!("{:#x}", seeded[0].physical), TOKEN_AMOUNT],
            )?;
            let replacement_flush = planner
                .plan(&route)
                .await?
                .context("plan replacement flush")?;
            cast_rpc(&anvil.rpc_url, "evm_setAutomine", &["false"])?;
            let replacement_flusher = Flusher::new(
                database.pool.clone(),
                chain.clone(),
                signer.clone(),
                alerts.clone(),
                FlusherPolicy {
                    replacement_after_blocks: 0,
                    ..FlusherPolicy::default()
                },
            );
            ensure!(
                replacement_flusher
                    .send_next(&route)
                    .await
                    .context("send replacement original")?
                    == RunResult::Sent {
                        flush_id: replacement_flush
                    }
            );
            let (replacement_a, replacement_b) = tokio::join!(
                replacement_flusher.maintain_sent(&route),
                replacement_flusher.maintain_sent(&route)
            );
            let replacement_a = replacement_a.context("first concurrent replacement")?;
            let replacement_b = replacement_b.context("second concurrent replacement")?;
            ensure!(
                matches!(replacement_a, Some(RunResult::Replaced { flush_id }) if flush_id == replacement_flush)
                    || matches!(replacement_b, Some(RunResult::Replaced { flush_id }) if flush_id == replacement_flush)
            );
            let signed_versions: i32 = sqlx::query(
                "SELECT jsonb_array_length(receipt->'signed') FROM flushes WHERE id = $1",
            )
            .bind(replacement_flush)
            .fetch_one(&database.pool)
            .await?
            .try_get(0)?;
            ensure!(signed_versions == 2);
            cast_rpc(&anvil.rpc_url, "anvil_mine", &["1"])?;
            cast_rpc(&anvil.rpc_url, "evm_setAutomine", &["true"])?;
            finalize(&anvil.rpc_url)?;
            let restarted = Flusher::new(
                database.pool.clone(),
                chain.clone(),
                signer.clone(),
                alerts.clone(),
                FlusherPolicy::default(),
            );
            ensure!(
                restarted
                    .maintain_sent(&route)
                    .await
                    .context("recover mined replacement")?
                    == Some(RunResult::Confirmed {
                        flush_id: replacement_flush
                    })
            );

            cast_send(
                &anvil.rpc_url,
                token,
                ADMIN_KEY,
                "mint(address,uint256)",
                &[&format!("{:#x}", seeded[0].physical), TOKEN_AMOUNT],
            )?;
            let capped_flush = planner.plan(&route).await?.context("plan fee-cap flush")?;
            cast_rpc(&anvil.rpc_url, "evm_setAutomine", &["false"])?;
            ensure!(
                flusher.send_next(&route).await?
                    == RunResult::Sent {
                        flush_id: capped_flush
                    }
            );
            let initial_fee: String = sqlx::query_scalar(
                "SELECT receipt->'signed'->0->>'max_fee_per_gas' FROM flushes WHERE id = $1",
            )
            .bind(capped_flush)
            .fetch_one(&database.pool)
            .await?;
            let initial_fee = initial_fee.parse::<u128>()?;
            let capped_flusher = Flusher::new(
                database.pool.clone(),
                chain.clone(),
                signer.clone(),
                alerts.clone(),
                FlusherPolicy {
                    replacement_after_blocks: 0,
                    max_fee_per_gas: initial_fee,
                    ..FlusherPolicy::default()
                },
            );
            ensure!(
                capped_flusher.maintain_sent(&route).await?
                    == Some(RunResult::Rebroadcast {
                        flush_id: capped_flush
                    })
            );
            let capped_versions: i32 = sqlx::query_scalar(
                "SELECT jsonb_array_length(receipt->'signed') FROM flushes WHERE id = $1",
            )
            .bind(capped_flush)
            .fetch_one(&database.pool)
            .await?;
            ensure!(capped_versions == 1);
            ensure!(
                alerts
                    .0
                    .lock()
                    .expect("alert mutex is available")
                    .iter()
                    .any(|alert| matches!(alert, FlushAlert::FeeCapReached { flush_id, .. } if *flush_id == capped_flush))
            );
            cast_rpc(&anvil.rpc_url, "anvil_mine", &["1"])?;
            cast_rpc(&anvil.rpc_url, "evm_setAutomine", &["true"])?;
            finalize(&anvil.rpc_url)?;
            ensure!(
                flusher.maintain_sent(&route).await?
                    == Some(RunResult::Confirmed {
                        flush_id: capped_flush
                    })
            );

            grant_operator(&anvil.rpc_url, factory, ROTATED_ADDRESS, ADMIN_KEY)?;
            cast_send(
                &anvil.rpc_url,
                token,
                ADMIN_KEY,
                "mint(address,uint256)",
                &[&format!("{:#x}", seeded[0].physical), TOKEN_AMOUNT],
            )?;
            let stale_plan = planner
                .plan(&route)
                .await?
                .context("plan stale old-operator flush")?;
            let rotated_signer = signer_handle(ROTATED_KEY)?;
            let rotated_planner = Planner::new(
                database.pool.clone(),
                chain.clone(),
                rotated_signer,
                Arc::new(FixedPrice),
                alerts.clone(),
            );
            let rotated_flush = rotated_planner
                .plan(&route)
                .await?
                .context("rebind rotated-operator flush")?;
            ensure!(rotated_flush == stale_plan);
            let rotated_row =
                sqlx::query("SELECT nonce::text, operator FROM flushes WHERE id = $1")
                    .bind(rotated_flush)
                    .fetch_one(&database.pool)
                    .await?;
            let rotated_nonce: String = rotated_row.try_get("nonce")?;
            let rotated_operator: String = rotated_row.try_get("operator")?;
            ensure!(rotated_nonce == "0");
            ensure!(rotated_operator == ROTATED_ADDRESS);
            let rebound_audit: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM audit WHERE action = 'flush.plan_operator_rebound' AND subject = $1)",
            )
            .bind(stale_plan.to_string())
            .fetch_one(&database.pool)
            .await?;
            ensure!(rebound_audit);

            ensure!(
                Flusher::new(
                    database.pool.clone(),
                    chain.clone(),
                    signer_handle(ROTATED_KEY)?,
                    alerts.clone(),
                    FlusherPolicy::default(),
                )
                .send_next(&route)
                .await?
                    == RunResult::Sent {
                        flush_id: rotated_flush
                    }
            );
            finalize(&anvil.rpc_url)?;
            ensure!(
                Flusher::new(
                    database.pool.clone(),
                    chain.clone(),
                    signer_handle(ROTATED_KEY)?,
                    alerts.clone(),
                    FlusherPolicy::default(),
                )
                .maintain_sent(&route)
                .await?
                    == Some(RunResult::Confirmed {
                        flush_id: rotated_flush
                    })
            );

            let reverting = deploy(
                &root,
                &anvil.rpc_url,
                "test/mocks/MockTokens.sol:SelectiveRevertingToken",
                &[],
            )?;
            route.asset.contract = reverting;
            for address in &seeded {
                cast_send(
                    &anvil.rpc_url,
                    reverting,
                    ADMIN_KEY,
                    "mint(address,uint256)",
                    &[&format!("{:#x}", address.physical), TOKEN_AMOUNT],
                )?;
            }
            cast_send(
                &anvil.rpc_url,
                reverting,
                ADMIN_KEY,
                "setBlockedForwarder(address)",
                &[&format!("{:#x}", seeded[1].physical)],
            )?;
            let estimated_plan = planner
                .plan(&route)
                .await?
                .context("plan around estimation revert")?;
            let estimated_count: i32 = sqlx::query_scalar(
                "SELECT jsonb_array_length(receipt->'plan') FROM flushes WHERE id = $1",
            )
            .bind(estimated_plan)
            .fetch_one(&database.pool)
            .await?;
            ensure!(estimated_count == 1);
            let exclusion_count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM flush_exclusions WHERE chain_id = 31337 AND token = $1",
            )
            .bind(format!("{reverting:#x}"))
            .fetch_one(&database.pool)
            .await?;
            ensure!(exclusion_count == 1);
            ensure!(alerts.0.lock().expect("alert mutex is available").iter().any(
                |alert| matches!(alert, FlushAlert::PlanningExcluded { address_id, .. } if *address_id == seeded[1].id)
            ));
            ensure!(
                flusher.send_next(&route).await?
                    == RunResult::Sent {
                        flush_id: estimated_plan
                    }
            );
            finalize(&anvil.rpc_url)?;
            ensure!(
                flusher.maintain_sent(&route).await?
                    == Some(RunResult::Confirmed {
                        flush_id: estimated_plan
                    })
            );
            cast_send(
                &anvil.rpc_url,
                reverting,
                ADMIN_KEY,
                "setBlockedForwarder(address)",
                &["0x0000000000000000000000000000000000000000"],
            )?;
            for address in &seeded {
                cast_send(
                    &anvil.rpc_url,
                    reverting,
                    ADMIN_KEY,
                    "mint(address,uint256)",
                    &[&format!("{:#x}", address.physical), TOKEN_AMOUNT],
                )?;
            }
            sqlx::query("DELETE FROM flush_exclusions WHERE chain_id = 31337 AND token = $1")
                .bind(format!("{reverting:#x}"))
                .execute(&database.pool)
                .await?;
            let revert_flush = planner.plan(&route).await?.context("plan reverting batch")?;
            cast_send(
                &anvil.rpc_url,
                reverting,
                ADMIN_KEY,
                "setBlockedForwarder(address)",
                &[&format!("{:#x}", seeded[1].physical)],
            )?;
            ensure!(
                flusher
                    .send_next(&route)
                    .await
                    .context("send reverting batch")?
                    == RunResult::Sent {
                        flush_id: revert_flush
                    }
            );
            finalize(&anvil.rpc_url)?;
            ensure!(
                flusher.maintain_sent(&route).await?
                    == Some(RunResult::Reverted {
                        flush_id: revert_flush
                    })
            );
            drive_bisection(&flusher, &route, &anvil.rpc_url, alerts.as_ref()).await?;
            Ok(())
        })
    })
    .await
}

async fn with_database<F>(test: F) -> Result<()>
where
    F: for<'a> FnOnce(&'a Database) -> TestFuture<'a>,
{
    let Some(database) = Database::create().await? else {
        return Ok(());
    };
    let result = test(&database).await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

async fn drive_bisection(
    flusher: &Flusher,
    route: &RouteFile,
    rpc_url: &str,
    alerts: &Alerts,
) -> Result<()> {
    for _ in 0..10 {
        match flusher.run_once(route).await? {
            RunResult::Sent { .. } => finalize(rpc_url)?,
            RunResult::Reverted { .. } | RunResult::Confirmed { .. } | RunResult::Idle => {}
            RunResult::Replaced { .. } | RunResult::Rebroadcast { .. } => {}
        }
        if alerts
            .0
            .lock()
            .expect("alert mutex is available")
            .iter()
            .any(|alert| matches!(alert, FlushAlert::IsolatedAddress { .. }))
        {
            return Ok(());
        }
    }
    bail!("bisection did not isolate the failing address")
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root exists")
}

fn deploy(root: &Path, rpc_url: &str, contract: &str, args: &[&str]) -> Result<Address> {
    let mut command = Command::new("forge");
    command.current_dir(root).args([
        "create",
        "--root",
        "contracts",
        "--rpc-url",
        rpc_url,
        "--private-key",
        &format!("0x{ADMIN_KEY}"),
        "--broadcast",
        "--json",
        contract,
    ]);
    if !args.is_empty() {
        command.arg("--constructor-args").args(args);
    }
    let output = command.output()?;
    ensure!(
        output.status.success(),
        "forge create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    Address::from_str(
        value["deployedTo"]
            .as_str()
            .context("forge output omitted deployedTo")?,
    )
    .map_err(Into::into)
}

fn cast_send(
    rpc_url: &str,
    contract: Address,
    private_key: &str,
    signature: &str,
    args: &[&str],
) -> Result<()> {
    let output = Command::new("cast")
        .args([
            "send",
            "--rpc-url",
            rpc_url,
            "--private-key",
            &format!("0x{private_key}"),
            &format!("{contract:#x}"),
            signature,
        ])
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "cast send failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn grant_operator(
    rpc_url: &str,
    factory: Address,
    operator: &str,
    private_key: &str,
) -> Result<()> {
    let role = cast_call_text(rpc_url, factory, "OPERATOR_ROLE()(bytes32)", &[])?;
    cast_send(
        rpc_url,
        factory,
        private_key,
        "grantRole(bytes32,address)",
        &[role.trim(), operator],
    )
}

fn cast_call_address(
    rpc_url: &str,
    contract: Address,
    signature: &str,
    args: &[&str],
) -> Result<Address> {
    Address::from_str(cast_call_text(rpc_url, contract, signature, args)?.trim())
        .map_err(Into::into)
}

fn cast_call_text(
    rpc_url: &str,
    contract: Address,
    signature: &str,
    args: &[&str],
) -> Result<String> {
    let output = Command::new("cast")
        .args([
            "call",
            "--rpc-url",
            rpc_url,
            &format!("{contract:#x}"),
            signature,
        ])
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "cast call failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).map_err(Into::into)
}

fn cast_rpc(rpc_url: &str, method: &str, args: &[&str]) -> Result<()> {
    let output = Command::new("cast")
        .args(["rpc", "--rpc-url", rpc_url, method])
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "cast rpc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn finalize(rpc_url: &str) -> Result<()> {
    cast_rpc(rpc_url, "anvil_mine", &["0x41"])
}

fn signer_handle(key: &str) -> Result<SignerHandle> {
    let bytes = hex::decode(key)?;
    let operator = SecretKey32::from_slice(&bytes).context("operator key has 32 bytes")?;
    let settlement = SecretKey32::new([9; 32]);
    SignerHandle::spawn(
        DevSigner::new(operator, settlement),
        NonZeroUsize::new(8).context("queue is non-zero")?,
        StdDuration::from_secs(5),
    )
    .map_err(Into::into)
}

fn test_route(factory: Address, token: Address) -> Result<RouteFile> {
    let mut route: RouteFile =
        serde_saphyr::from_str(include_str!("fixtures/phala-cloud-pha.yaml"))?;
    route.chain.chain_id = 31_337;
    route.chain.contracts.forwarder_factory = factory;
    route.chain.contracts.treasury = Address::from_str(TREASURY)?;
    route.chain.flush.max_gas_ratio_bps = Bps::new(10_000)?;
    route.asset.contract = token;
    route.asset.min_flush_atomic = AtomicAmount::new(U256::from(100_u64));
    Ok(route)
}

struct SeededAddress {
    id: Uuid,
    account_id: Uuid,
    physical: Address,
}

async fn seed_addresses(
    pool: &PgPool,
    factory: Address,
    implementation: Address,
) -> Result<Vec<SeededAddress>> {
    let product = NewProduct {
        id: Uuid::new_v4(),
        slug: "c7-product".to_owned(),
        settlement_url: "https://product.test/settlements".to_owned(),
        webhook_url: "https://product.test/webhooks".to_owned(),
        pubkey: "test-key".to_owned(),
        kid: "test/v1".to_owned(),
        paused_scopes: Vec::new(),
    };
    topup::db::create_product(pool, &product).await?;
    let mut result = Vec::new();
    for number in 1_u8..=2 {
        let account = NewAccount {
            id: Uuid::new_v4(),
            product_id: product.id,
            external_id: format!("account-{number}"),
            paused_scopes: Vec::new(),
        };
        topup::db::create_account(pool, &account).await?;
        let salt = B256::from([number; 32]);
        let physical = forwarder_address(factory, implementation, salt);
        let address = NewAddress {
            id: Uuid::new_v4(),
            account_id: account.id,
            chain_id: 31_337,
            kind: AddressKind::Persistent,
            version: 1,
            lock_ref: None,
            salt,
            address: physical,
            retired_at: (number == 2).then(Utc::now),
        };
        topup::db::insert_address(pool, &address).await?;
        result.push(SeededAddress {
            id: address.id,
            account_id: account.id,
            physical,
        });
    }
    Ok(result)
}

async fn insert_deposit(
    pool: &PgPool,
    address: &SeededAddress,
    token: Address,
    fixture: DepositFixture,
) -> Result<Uuid> {
    let DepositFixture {
        state,
        reason,
        block_number,
        log_index,
        number,
    } = fixture;
    let tx_hash = B256::from([number; 32]);
    let deposit = NewDeposit {
        chain_id: 31_337,
        tx_hash,
        log_index,
        block_number,
        block_hash: B256::from([number.wrapping_add(100); 32]),
        block_time: Utc::now(),
        address_id: address.id,
        account_id: address.account_id,
        route: Some("phala-cloud-ethereum-pha-usd".to_owned()),
        route_version: Some(1),
        asset_contract: token,
        from_address: Address::from([number.wrapping_add(10); 20]),
        amount_atomic: AtomicAmount::new(U256::from(1_000_u64)),
        state,
        reason,
        next_attempt_at: Utc::now() - Duration::seconds(1),
    };
    let committed = topup::db::commit_scan(pool, 31_337, &[deposit], &[], None).await?;
    ensure!(committed.inserted == 1);
    Ok(deposit_id(31_337, tx_hash, log_index))
}

struct DepositFixture {
    state: DepositState,
    reason: Option<RejectReason>,
    block_number: u64,
    log_index: u64,
    number: u8,
}

async fn assert_deposit(
    pool: &PgPool,
    id: Uuid,
    state: &str,
    flush_id: Option<Uuid>,
) -> Result<()> {
    let row = sqlx::query("SELECT state, flush_id FROM deposits WHERE id = $1")
        .bind(id)
        .fetch_one(pool)
        .await?;
    let actual_state: String = row.try_get("state")?;
    let actual_flush: Option<Uuid> = row.try_get("flush_id")?;
    ensure!(actual_state == state && actual_flush == flush_id);
    Ok(())
}

async fn first_event_position(pool: &PgPool, flush_id: Uuid) -> Result<(u64, u64)> {
    let row = sqlx::query(
        "SELECT block_number, log_index FROM flushed WHERE flush_id = $1 ORDER BY log_index LIMIT 1",
    )
    .bind(flush_id)
    .fetch_one(pool)
    .await?;
    let block: i64 = row.try_get("block_number")?;
    let index: i64 = row.try_get("log_index")?;
    Ok((u64::try_from(block)?, u64::try_from(index)?))
}

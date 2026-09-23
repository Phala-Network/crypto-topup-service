//! C7 end-to-end flusher coverage against disposable PostgreSQL and Anvil instances.

#![cfg(feature = "dev-signer")]

mod support;

use std::env;
use std::future::Future;
use std::net::TcpListener;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use alloy_primitives::{Address, B256, U256, keccak256};
use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgPool, Row};
use tokio_util::sync::CancellationToken;
use topup::db::{AddressKind, NewAccount, NewAddress, NewDeposit, NewProduct};
use topup::flusher::runtime::FlusherTask;
use topup::flusher::{
    AlertSink, AlloyChainClient, ChainClient, ChainError, ChainReceipt, FeeQuote, FlushAlert,
    Flusher, FlusherPolicy, NonceReceiptSearch, OperatorRole, Planner, PriceError, PriceSource,
    RunResult,
};
use topup_adapters::signer::DevSigner;
use topup_adapters::signer::actor::SignerHandle;
use topup_core::address::forwarder_address;
use topup_core::deposit::{DepositState, RejectReason};
use topup_core::identity::deposit_id;
use topup_core::money::{AtomicAmount, Bps, PRICE_SCALE, ScaledPrice};
use topup_core::route::RouteFile;
use topup_core::{
    Ed25519PublicKey, Ed25519Signature, SecretKey32, SignedTx, Signer, SignerError, TxRequest,
};
use url::Url;
use uuid::Uuid;

/// Waits out transient `max_connections` exhaustion when many test databases share one
/// server under load; sqlx's 30 s default turns that into spurious `PoolTimedOut` failures.
const DB_ACQUIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

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
            .acquire_timeout(DB_ACQUIRE_TIMEOUT)
            .connect(admin_url.as_str())
            .await?;
        support::ensure_app_role(&admin).await?;
        let name = format!("topup_c7_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE \"{name}\""))
            .execute(&admin)
            .await?;
        let mut database_url = Url::parse(&template)?;
        database_url.set_path(&format!("/{name}"));
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(DB_ACQUIRE_TIMEOUT)
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

/// Node-free chain double for planning and sending that records which addresses were read.
#[derive(Default)]
struct StubChain {
    calls: Mutex<usize>,
    balance_reads: Mutex<Vec<Address>>,
}

impl StubChain {
    fn record_call(&self) {
        *self.calls.lock().expect("call mutex is available") += 1;
    }
}

#[async_trait]
impl ChainClient for StubChain {
    async fn token_balances(
        &self,
        _token: Address,
        addresses: &[Address],
    ) -> Result<Vec<U256>, ChainError> {
        self.record_call();
        self.balance_reads
            .lock()
            .expect("read mutex is available")
            .extend_from_slice(addresses);
        Ok(vec![
            U256::from(10_u64).pow(U256::from(20_u8));
            addresses.len()
        ])
    }

    async fn native_balances(&self, addresses: &[Address]) -> Result<Vec<U256>, ChainError> {
        self.record_call();
        Ok(vec![U256::ZERO; addresses.len()])
    }

    async fn estimate_flush_gas(
        &self,
        _factory: Address,
        _operator: Address,
        _salts: &[B256],
        _token: Address,
    ) -> Result<u64, ChainError> {
        self.record_call();
        Ok(100_000)
    }

    async fn has_operator_role(
        &self,
        _factory: Address,
        _operator: Address,
    ) -> Result<bool, ChainError> {
        Err(ChainError::rpc("not used by the stub"))
    }

    async fn pending_nonce(&self, _operator: Address) -> Result<u64, ChainError> {
        self.record_call();
        Ok(0)
    }

    async fn confirmed_nonce(&self, _operator: Address) -> Result<u64, ChainError> {
        Err(ChainError::rpc("not used by the stub"))
    }

    async fn latest_block(&self) -> Result<u64, ChainError> {
        Ok(1)
    }

    async fn finalized_block(&self) -> Result<u64, ChainError> {
        Err(ChainError::rpc("not used by the stub"))
    }

    async fn fee_quote(&self) -> Result<FeeQuote, ChainError> {
        self.record_call();
        Ok(FeeQuote {
            max_fee_per_gas: 1_000_000_000,
            max_priority_fee_per_gas: 1_000_000,
        })
    }

    async fn send_raw_transaction(&self, raw: &[u8]) -> Result<B256, ChainError> {
        Ok(keccak256(raw))
    }

    async fn receipt(&self, _hash: B256) -> Result<Option<ChainReceipt>, ChainError> {
        Err(ChainError::rpc("not used by the stub"))
    }

    async fn receipt_by_sender_nonce(
        &self,
        _operator: Address,
        _nonce: u64,
        _from_block: u64,
        _max_blocks: u64,
    ) -> Result<NonceReceiptSearch, ChainError> {
        Err(ChainError::rpc("not used by the stub"))
    }
}

#[tokio::test]
async fn planner_skips_frozen_chains_and_blocked_addresses() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let factory = Address::from([0x51; 20]);
            let route = test_route(factory, Address::from([0x52; 20]))?;
            let seeded = seed_addresses(&database.pool, factory, Address::from([0x53; 20])).await?;
            let chain = Arc::new(StubChain::default());
            let planner = Planner::new(
                database.pool.clone(),
                chain.clone(),
                signer_handle(OPERATOR_KEY)?,
                Arc::new(FixedPrice),
                Arc::new(Alerts::default()),
            );

            sqlx::query(
                r#"
                INSERT INTO reconciliation_blocks (block_key, scope, chain_id, check_name, reason)
                VALUES ('chain:31337', 'chain', 31337, 'address_derivation', 'test freeze')
                "#,
            )
            .execute(&database.pool)
            .await?;
            ensure!(planner.plan(&route).await?.is_none());
            ensure!(*chain.calls.lock().expect("call mutex is available") == 0);
            let flushes: i64 = sqlx::query_scalar("SELECT count(*) FROM flushes")
                .fetch_one(&database.pool)
                .await?;
            ensure!(flushes == 0);

            // Unfreezing is the owner deleting the block row; planning resumes without restart.
            sqlx::query("DELETE FROM reconciliation_blocks WHERE block_key = 'chain:31337'")
                .execute(&database.pool)
                .await?;
            sqlx::query(
                r#"
                INSERT INTO reconciliation_blocks
                    (block_key, scope, chain_id, address_id, check_name, reason)
                VALUES ($1, 'address', 31337, $2, 'credit_recomputation', 'test block')
                "#,
            )
            .bind(format!("address:{}", seeded[0].id))
            .bind(seeded[0].id)
            .execute(&database.pool)
            .await?;
            let flush_id = planner.plan(&route).await?.context("plan unblocked address")?;
            let reads = chain
                .balance_reads
                .lock()
                .expect("read mutex is available")
                .clone();
            ensure!(reads == [seeded[1].physical]);
            let planned: Vec<String> = sqlx::query_scalar(
                "SELECT item->>'address_id' FROM flushes, jsonb_array_elements(receipt->'plan') AS item WHERE id = $1",
            )
            .bind(flush_id)
            .fetch_all(&database.pool)
            .await?;
            ensure!(planned == [seeded[1].id.to_string()]);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn paused_route_plan_is_voided_so_later_plans_on_the_chain_still_send() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let factory = Address::from([0x61; 20]);
            let paused = test_route(factory, Address::from([0x62; 20]))?;
            let mut other = test_route(factory, Address::from([0x63; 20]))?;
            other.route = "other-route".to_owned();
            seed_addresses(&database.pool, factory, Address::from([0x64; 20])).await?;
            let (planner, flusher) = stub_flusher(&database.pool, signer_handle(OPERATOR_KEY)?);
            let paused_plan = planner.plan(&paused).await?.context("plan paused route")?;
            let other_plan = planner.plan(&other).await?.context("plan other route")?;
            ensure!(flush_nonce(&database.pool, paused_plan).await? == 0);
            ensure!(flush_nonce(&database.pool, other_plan).await? == 1);

            set_route_flush_pause(&database.pool, &paused.route, true).await?;
            ensure!(
                flusher.send_next(&other).await?
                    == RunResult::Sent {
                        flush_id: other_plan
                    }
            );
            ensure!(flush_nonce(&database.pool, other_plan).await? == 0);
            ensure!(
                voided_reason(&database.pool, paused_plan)
                    .await?
                    .contains(&format!("route `{}`", paused.route))
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn voiding_the_first_plan_renumbers_later_plans_across_tokens_contiguously() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let factory = Address::from([0x65; 20]);
            let paused = test_route(factory, Address::from([0x66; 20]))?;
            let mut other = test_route(factory, Address::from([0x67; 20]))?;
            other.route = "other-route".to_owned();
            seed_addresses(&database.pool, factory, Address::from([0x68; 20])).await?;
            let (planner, flusher) = stub_flusher(&database.pool, signer_handle(OPERATOR_KEY)?);
            let paused_plan = planner.plan(&paused).await?.context("plan paused route")?;
            let other_plan = planner.plan(&other).await?.context("plan other route")?;
            // Interleave further unsigned plans of both tokens behind the first two.
            let other_later = copy_planned_flush(&database.pool, other_plan, 2).await?;
            let paused_later = copy_planned_flush(&database.pool, paused_plan, 3).await?;

            set_route_flush_pause(&database.pool, &paused.route, true).await?;
            ensure!(
                flusher.send_next(&other).await?
                    == RunResult::Sent {
                        flush_id: other_plan
                    }
            );
            voided_reason(&database.pool, paused_plan).await?;
            let nonces: Vec<(Uuid, String, String)> =
                sqlx::query_as("SELECT id, nonce::text, status FROM flushes ORDER BY nonce")
                    .fetch_all(&database.pool)
                    .await?;
            ensure!(
                nonces
                    == [
                        (other_plan, "0".to_owned(), "sent".to_owned()),
                        (other_later, "1".to_owned(), "planned".to_owned()),
                        (paused_later, "2".to_owned(), "planned".to_owned()),
                    ],
                "unexpected nonces after voiding: {nonces:?}"
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn paused_account_is_dropped_from_a_multi_account_batch_on_replan() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let factory = Address::from([0x71; 20]);
            let route = test_route(factory, Address::from([0x72; 20]))?;
            let seeded = seed_addresses(&database.pool, factory, Address::from([0x73; 20])).await?;
            let (planner, flusher) = stub_flusher(&database.pool, signer_handle(OPERATOR_KEY)?);
            let batch = planner.plan(&route).await?.context("plan batch")?;
            ensure!(planned_address_ids(&database.pool, batch).await?.len() == 2);

            set_account_flush_pause(&database.pool, seeded[0].account_id, true).await?;
            ensure!(flusher.send_next(&route).await? == RunResult::Idle);
            let reason = voided_reason(&database.pool, batch).await?;
            ensure!(reason.contains(&format!("account {}", seeded[0].account_id)));
            ensure!(reason.contains(&format!("token {:#x}", route.asset.contract)));
            let ids = reason
                .split_once("address ids [")
                .and_then(|(_, rest)| rest.split_once(']'))
                .map(|(ids, _)| ids)
                .context("reason lists the voided address ids")?;
            ensure!(ids.contains(&seeded[0].id.to_string()));
            ensure!(ids.contains(&seeded[1].id.to_string()));
            let replan = planner
                .plan(&route)
                .await?
                .context("replan without paused")?;
            ensure!(replan != batch);
            ensure!(planned_address_ids(&database.pool, replan).await? == [seeded[1].id]);
            ensure!(flush_nonce(&database.pool, replan).await? == 0);
            ensure!(flusher.send_next(&route).await? == RunResult::Sent { flush_id: replan });
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn slow_signing_does_not_hold_pause_row_locks() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let factory = Address::from([0x81; 20]);
            let route = test_route(factory, Address::from([0x82; 20]))?;
            let seeded = seed_addresses(&database.pool, factory, Address::from([0x83; 20])).await?;
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let gated = SignerHandle::spawn(
                GatedSigner {
                    inner: dev_signer(OPERATOR_KEY)?,
                    entered: entered.clone(),
                    release: release.clone(),
                },
                NonZeroUsize::new(8).context("queue is non-zero")?,
                StdDuration::from_secs(10),
            )?;
            let (planner, flusher) = stub_flusher(&database.pool, gated);
            let batch = planner.plan(&route).await?.context("plan batch")?;

            let pause_while_signing = async {
                entered.notified().await;
                let paused = tokio::time::timeout(
                    StdDuration::from_secs(2),
                    set_account_flush_pause(&database.pool, seeded[0].account_id, true),
                )
                .await;
                release.notify_one();
                paused.context("account pause blocked behind an in-flight signature")?
            };
            let (sent, paused) = tokio::join!(flusher.send_next(&route), pause_while_signing);
            paused?;
            // The pause committed while the signer ran is still honored before the plan is sent.
            ensure!(sent? == RunResult::Idle);
            ensure!(
                voided_reason(&database.pool, batch)
                    .await?
                    .contains(&format!("account {}", seeded[0].account_id))
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn first_route_pause_waits_for_a_send_that_passed_the_route_check() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let factory = Address::from([0x91; 20]);
            let route = test_route(factory, Address::from([0x92; 20]))?;
            let seeded = seed_addresses(&database.pool, factory, Address::from([0x93; 20])).await?;
            let (planner, flusher) = stub_flusher(&database.pool, signer_handle(OPERATOR_KEY)?);
            let batch = planner.plan(&route).await?.context("plan batch")?;
            let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM route_pauses")
                .fetch_one(&database.pool)
                .await?;
            ensure!(rows == 0, "the route must not have a pause row yet");

            // Hold the sender between its route check and its account check.
            let mut account_lock = database.pool.begin().await?;
            sqlx::query("SELECT 1 FROM accounts WHERE id = $1 FOR UPDATE")
                .bind(seeded[0].account_id)
                .execute(&mut *account_lock)
                .await?;
            let first_pause = async {
                wait_for_lock_wait(&database.pool, "FOR SHARE OF account, product").await?;
                let mut pause = database.pool.begin().await?;
                sqlx::query("SET LOCAL lock_timeout = '300ms'")
                    .execute(&mut *pause)
                    .await?;
                let paused = sqlx::query(
                    "INSERT INTO route_pauses (route, paused_scopes) VALUES ($1, '{flush}') \
                     ON CONFLICT (route) DO UPDATE SET paused_scopes = EXCLUDED.paused_scopes",
                )
                .bind(&route.route)
                .execute(&mut *pause)
                .await;
                pause.rollback().await?;
                account_lock.rollback().await?;
                anyhow::Ok(paused)
            };
            let (sent, paused) = tokio::join!(flusher.send_next(&route), first_pause);
            let error = paused?
                .err()
                .context("first route pause committed while a send was past its route check")?;
            ensure!(
                error
                    .as_database_error()
                    .and_then(|error| error.code())
                    .as_deref()
                    == Some("55P03"),
                "unexpected pause error: {error}"
            );
            ensure!(sent? == RunResult::Sent { flush_id: batch });
            Ok(())
        })
    })
    .await
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
async fn flush_pauses_gate_planning_and_void_unsent_plans_without_blocking_confirmation()
-> Result<()> {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let metrics = recorder.handle();
    // The current-thread test runtime polls every flusher call on this thread.
    let _recorder = metrics::set_default_local_recorder(&recorder);
    with_database(|database| {
        let metrics = metrics.clone();
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
            let route = test_route(factory, token)?;
            let seeded = seed_addresses(&database.pool, factory, implementation).await?;
            let address = &seeded[0];
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
                chain,
                signer,
                alerts,
                FlusherPolicy::default(),
            );

            mint(&anvil.rpc_url, token, address.physical)?;
            set_route_flush_pause(&database.pool, &route.route, true).await?;
            for _ in 0..2 {
                ensure!(planner.plan(&route).await?.is_none());
                ensure!(flusher.run_once(&route).await? == RunResult::Idle);
            }
            set_route_flush_pause(&database.pool, &route.route, false).await?;
            let route_plan = planner
                .plan(&route)
                .await?
                .context("plan before route pause")?;
            set_route_flush_pause(&database.pool, &route.route, true).await?;
            for _ in 0..2 {
                ensure!(flusher.run_once(&route).await? == RunResult::Idle);
                ensure!(planner.plan(&route).await?.is_none());
            }
            ensure!(
                voided_reason(&database.pool, route_plan)
                    .await?
                    .contains(&format!("route `{}`", route.route))
            );
            let send_paused = format!(
                "topup_flush_send_paused_total{{chain=\"{}\",producer_enabled=\"true\"}}",
                route.chain.chain_id
            );
            ensure!(metrics.render().contains(&format!("{send_paused} 1")));

            set_route_flush_pause(&database.pool, &route.route, false).await?;
            let route_plan = planner
                .plan(&route)
                .await?
                .context("plan after route resume")?;
            ensure!(
                flusher.run_once(&route).await?
                    == RunResult::Sent {
                        flush_id: route_plan
                    }
            );
            set_route_flush_pause(&database.pool, &route.route, true).await?;
            finalize(&anvil.rpc_url)?;
            ensure!(
                flusher.run_once(&route).await?
                    == RunResult::Confirmed {
                        flush_id: route_plan
                    }
            );
            set_route_flush_pause(&database.pool, &route.route, false).await?;

            mint(&anvil.rpc_url, token, address.physical)?;
            set_product_flush_pause(&database.pool, address.product_id, true).await?;
            let completed_before_product = completed_flush_count(&database.pool).await?;
            for _ in 0..2 {
                ensure!(planner.plan(&route).await?.is_none());
                ensure!(flusher.run_once(&route).await? == RunResult::Idle);
                ensure!(completed_flush_count(&database.pool).await? == completed_before_product);
            }
            set_product_flush_pause(&database.pool, address.product_id, false).await?;
            let product_plan = planner
                .plan(&route)
                .await?
                .context("plan after product resume")?;
            set_product_flush_pause(&database.pool, address.product_id, true).await?;
            for _ in 0..2 {
                ensure!(flusher.run_once(&route).await? == RunResult::Idle);
                ensure!(planner.plan(&route).await?.is_none());
            }
            ensure!(
                voided_reason(&database.pool, product_plan)
                    .await?
                    .contains(&format!("product {}", address.product_id))
            );
            set_product_flush_pause(&database.pool, address.product_id, false).await?;
            let product_plan = planner
                .plan(&route)
                .await?
                .context("plan after product resume")?;
            ensure!(
                flusher.run_once(&route).await?
                    == RunResult::Sent {
                        flush_id: product_plan
                    }
            );
            finalize(&anvil.rpc_url)?;
            ensure!(
                flusher.run_once(&route).await?
                    == RunResult::Confirmed {
                        flush_id: product_plan
                    }
            );

            mint(&anvil.rpc_url, token, address.physical)?;
            set_account_flush_pause(&database.pool, address.account_id, true).await?;
            let completed_before_account = completed_flush_count(&database.pool).await?;
            for _ in 0..2 {
                ensure!(planner.plan(&route).await?.is_none());
                ensure!(flusher.run_once(&route).await? == RunResult::Idle);
                ensure!(completed_flush_count(&database.pool).await? == completed_before_account);
            }
            set_account_flush_pause(&database.pool, address.account_id, false).await?;
            let account_plan = planner
                .plan(&route)
                .await?
                .context("plan after account resume")?;
            set_account_flush_pause(&database.pool, address.account_id, true).await?;
            for _ in 0..2 {
                ensure!(flusher.run_once(&route).await? == RunResult::Idle);
                ensure!(planner.plan(&route).await?.is_none());
            }
            ensure!(
                voided_reason(&database.pool, account_plan)
                    .await?
                    .contains(&format!("account {}", address.account_id))
            );
            set_account_flush_pause(&database.pool, address.account_id, false).await?;
            let account_plan = planner
                .plan(&route)
                .await?
                .context("plan after account resume")?;
            ensure!(
                flusher.run_once(&route).await?
                    == RunResult::Sent {
                        flush_id: account_plan
                    }
            );
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
            let stale_nonce: String =
                sqlx::query_scalar("SELECT nonce::text FROM flushes WHERE id = $1")
                    .bind(stale_plan)
                    .fetch_one(&database.pool)
                    .await?;
            let repeated_plan = planner
                .plan(&route)
                .await?
                .context("repeat current-operator planning")?;
            ensure!(repeated_plan == stale_plan);
            let repeated_nonce: String =
                sqlx::query_scalar("SELECT nonce::text FROM flushes WHERE id = $1")
                    .bind(repeated_plan)
                    .fetch_one(&database.pool)
                    .await?;
            ensure!(repeated_nonce == stale_nonce);
            let pre_rotation_audits: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM audit WHERE action = 'flush.plan_operator_rebound' AND subject = $1",
            )
            .bind(stale_plan.to_string())
            .fetch_one(&database.pool)
            .await?;
            ensure!(pre_rotation_audits == 0);
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
            let repeated_rotated = rotated_planner
                .plan(&route)
                .await?
                .context("repeat rotated-operator planning")?;
            ensure!(repeated_rotated == rotated_flush);
            let repeated_rotated_nonce: String =
                sqlx::query_scalar("SELECT nonce::text FROM flushes WHERE id = $1")
                    .bind(repeated_rotated)
                    .fetch_one(&database.pool)
                    .await?;
            ensure!(repeated_rotated_nonce == rotated_nonce);
            let rebound_audits: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM audit WHERE action = 'flush.plan_operator_rebound' AND subject = $1",
            )
            .bind(stale_plan.to_string())
            .fetch_one(&database.pool)
            .await?;
            ensure!(rebound_audits == 1);

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

#[tokio::test]
async fn anvil_operator_key_version_bump_gates_on_role_and_rebinds_stale_plans() -> Result<()> {
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
            let implementation =
                cast_call_address(&anvil.rpc_url, factory, "implementation()(address)", &[])?;
            let seeded = seed_addresses(&database.pool, factory, implementation).await?;
            let mint = |address: Address| {
                cast_send(
                    &anvil.rpc_url,
                    token,
                    ADMIN_KEY,
                    "mint(address,uint256)",
                    &[&format!("{address:#x}"), TOKEN_AMOUNT],
                )
            };
            let chain = Arc::new(AlloyChainClient::connect_http(&anvil.rpc_url)?);
            let alerts = Arc::new(Alerts::default());
            let components = |signer: &SignerHandle| {
                (
                    Planner::new(
                        database.pool.clone(),
                        chain.clone(),
                        signer.clone(),
                        Arc::new(FixedPrice),
                        alerts.clone(),
                    ),
                    Flusher::new(
                        database.pool.clone(),
                        chain.clone(),
                        signer.clone(),
                        alerts.clone(),
                        FlusherPolicy::default(),
                    ),
                )
            };
            let spawn_task = |route: &RouteFile, signer: &SignerHandle| {
                let (planner, flusher) = components(signer);
                let cancellation = CancellationToken::new();
                let task = FlusherTask::new(route.clone(), planner, flusher, alerts.clone())
                    .map_err(anyhow::Error::msg)?;
                let handle = tokio::spawn(task.run(cancellation.clone()));
                Ok::<_, anyhow::Error>((handle, cancellation))
            };
            let role_alerts = |operator: Address| {
                alerts
                    .0
                    .lock()
                    .expect("alert mutex is available")
                    .iter()
                    .filter(|alert| {
                        matches!(alert, FlushAlert::OperatorRoleMissing { operator: alerted, .. } if *alerted == operator)
                    })
                    .count()
            };
            let stop = |(handle, cancellation): (tokio::task::JoinHandle<()>, CancellationToken)| async move {
                ensure!(!handle.is_finished(), "flusher task must keep running");
                cancellation.cancel();
                tokio::time::timeout(StdDuration::from_secs(10), handle)
                    .await
                    .context("flusher task must stop when cancelled")??;
                Ok::<_, anyhow::Error>(())
            };
            // A missing role must not end the task: it re-checks and alerts at every interval.
            let waits_for_role = |route: &RouteFile, signer: &SignerHandle, operator: Address| {
                let task = spawn_task(route, signer);
                let before = role_alerts(operator);
                async move {
                    let task = task?;
                    tokio::time::sleep(StdDuration::from_millis(3_500)).await;
                    ensure!(
                        role_alerts(operator) >= before + 3,
                        "missing role must be re-checked and alerted"
                    );
                    Ok::<_, anyhow::Error>(task)
                }
            };

            let mut route_v1 = test_route(factory, token)?;
            route_v1.chain.flush.maintenance_interval_s = 1;
            ensure!(route_v1.chain.operator_key_version()? == NonZeroU32::MIN);
            let (signer_v1, operator_v1) = versioned_signer(&route_v1).await?;
            let mut route_v2 = route_v1.clone();
            route_v2.version = 2;
            route_v2.chain.operator_key_version = 2;
            let (signer_v2, operator_v2) = versioned_signer(&route_v2).await?;
            ensure!(operator_v1 != operator_v2);
            for operator in [operator_v1, operator_v2] {
                cast_rpc(
                    &anvil.rpc_url,
                    "anvil_setBalance",
                    &[&format!("{operator:#x}"), "0x56bc75e2d63100000"],
                )?;
            }
            let (planner_v1, flusher_v1) = components(&signer_v1);
            let (planner_v2, flusher_v2) = components(&signer_v2);

            ensure!(
                flusher_v1.operator_role(&route_v1).await?
                    == OperatorRole {
                        operator: operator_v1,
                        granted: false
                    }
            );
            stop(waits_for_role(&route_v1, &signer_v1, operator_v1).await?).await?;
            grant_operator(
                &anvil.rpc_url,
                factory,
                &format!("{operator_v1:#x}"),
                ADMIN_KEY,
            )?;
            ensure!(flusher_v1.operator_role(&route_v1).await?.granted);

            mint(seeded[0].physical)?;
            let first = planner_v1.plan(&route_v1).await?.context("plan v1 flush")?;
            ensure!(flusher_v1.send_next(&route_v1).await? == RunResult::Sent { flush_id: first });
            finalize(&anvil.rpc_url)?;
            ensure!(
                flusher_v1.maintain_sent(&route_v1).await?
                    == Some(RunResult::Confirmed { flush_id: first })
            );
            mint(seeded[0].physical)?;
            let stale = planner_v1
                .plan(&route_v1)
                .await?
                .context("plan unsigned v1 flush")?;
            let binding = |id: Uuid| async move {
                let row = sqlx::query(
                    "SELECT operator, nonce::text AS nonce, status::text AS status \
                     FROM flushes WHERE id = $1",
                )
                .bind(id)
                .fetch_one(&database.pool)
                .await?;
                Ok::<_, anyhow::Error>((
                    row.try_get::<String, _>("operator")?,
                    row.try_get::<String, _>("nonce")?,
                    row.try_get::<String, _>("status")?,
                ))
            };
            let wait_for_status = |id: Uuid, status: &'static str| {
                let binding = &binding;
                async move {
                    for _ in 0..100 {
                        if binding(id).await?.2 == status {
                            return Ok(());
                        }
                        tokio::time::sleep(StdDuration::from_millis(100)).await;
                    }
                    bail!("flush {id} did not become {status}")
                }
            };
            let v1_binding = (
                format!("{operator_v1:#x}"),
                "1".to_owned(),
                "planned".to_owned(),
            );
            ensure!(binding(stale).await? == v1_binding);

            // operator/v2 deployed before the admin Safe grants it waits and leaves the plan alone.
            ensure!(!flusher_v2.operator_role(&route_v2).await?.granted);
            let task_v2 = waits_for_role(&route_v2, &signer_v2, operator_v2).await?;
            ensure!(binding(stale).await? == v1_binding);

            // The running v2 task picks up the grant without a restart and flushes by itself.
            grant_operator(
                &anvil.rpc_url,
                factory,
                &format!("{operator_v2:#x}"),
                ADMIN_KEY,
            )?;
            ensure!(
                flusher_v2.operator_role(&route_v2).await?
                    == OperatorRole {
                        operator: operator_v2,
                        granted: true
                    }
            );
            ensure!(planner_v2.plan(&route_v2).await? == Some(stale));
            let (rebound_operator, rebound_nonce, _) = binding(stale).await?;
            ensure!(rebound_operator == format!("{operator_v2:#x}") && rebound_nonce == "0");
            wait_for_status(stale, "sent").await?;
            finalize(&anvil.rpc_url)?;
            wait_for_status(stale, "confirmed").await?;
            stop(task_v2).await?;
            ensure!(chain.confirmed_nonce(operator_v2).await? == 1);
            ensure!(chain.confirmed_nonce(operator_v1).await? == 1);
            let flushed: i64 =
                sqlx::query_scalar("SELECT count(*) FROM flushed WHERE flush_id = $1")
                    .bind(stale)
                    .fetch_one(&database.pool)
                    .await?;
            ensure!(flushed == 1);

            // After revocation a v1 task neither sends its own plan nor stops.
            mint(seeded[0].physical)?;
            let orphan = planner_v1
                .plan(&route_v1)
                .await?
                .context("plan before revoking v1")?;
            revoke_operator(
                &anvil.rpc_url,
                factory,
                &format!("{operator_v1:#x}"),
                ADMIN_KEY,
            )?;
            ensure!(!flusher_v1.operator_role(&route_v1).await?.granted);
            stop(waits_for_role(&route_v1, &signer_v1, operator_v1).await?).await?;
            ensure!(
                binding(orphan).await?
                    == (
                        format!("{operator_v1:#x}"),
                        "1".to_owned(),
                        "planned".to_owned()
                    )
            );

            // Revoking the operator of a running, authorized task stops its queued sends at the
            // next maintenance tick instead of letting them revert on chain.
            let mut slow_v2 = route_v2.clone();
            slow_v2.chain.flush.maintenance_interval_s = 3;
            ensure!(flusher_v2.operator_role(&slow_v2).await?.granted);
            let before = role_alerts(operator_v2);
            let task_v2 = spawn_task(&slow_v2, &signer_v2)?;
            tokio::time::sleep(StdDuration::from_millis(500)).await;
            ensure!(role_alerts(operator_v2) == before);
            ensure!(planner_v2.plan(&slow_v2).await? == Some(orphan));
            revoke_operator(
                &anvil.rpc_url,
                factory,
                &format!("{operator_v2:#x}"),
                ADMIN_KEY,
            )?;
            tokio::time::sleep(StdDuration::from_millis(4_000)).await;
            ensure!(
                role_alerts(operator_v2) > before,
                "revocation must be noticed by the running task"
            );
            stop(task_v2).await?;
            ensure!(
                binding(orphan).await?
                    == (
                        format!("{operator_v2:#x}"),
                        "1".to_owned(),
                        "planned".to_owned()
                    )
            );
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
    set_operator_role(rpc_url, factory, operator, private_key, "grantRole")
}

fn revoke_operator(
    rpc_url: &str,
    factory: Address,
    operator: &str,
    private_key: &str,
) -> Result<()> {
    set_operator_role(rpc_url, factory, operator, private_key, "revokeRole")
}

fn set_operator_role(
    rpc_url: &str,
    factory: Address,
    operator: &str,
    private_key: &str,
    function: &str,
) -> Result<()> {
    let role = cast_call_text(rpc_url, factory, "OPERATOR_ROLE()(bytes32)", &[])?;
    cast_send(
        rpc_url,
        factory,
        private_key,
        &format!("{function}(bytes32,address)"),
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

/// Mirrors `topup run`: one signer per attested operator key version, derived by domain.
async fn versioned_signer(route: &RouteFile) -> Result<(SignerHandle, Address)> {
    let version = route.chain.operator_key_version()?;
    let handle = SignerHandle::spawn(
        DevSigner::derive(&SecretKey32::new([0x5e; 32]), version),
        NonZeroUsize::new(8).context("queue is non-zero")?,
        StdDuration::from_secs(5),
    )?;
    let address = handle.operator_address().await?;
    Ok((handle, address))
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
    product_id: Uuid,
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
            product_id: product.id,
            physical,
        });
    }
    Ok(result)
}

fn mint(rpc_url: &str, token: Address, address: Address) -> Result<()> {
    cast_send(
        rpc_url,
        token,
        ADMIN_KEY,
        "mint(address,uint256)",
        &[&format!("{address:#x}"), TOKEN_AMOUNT],
    )
}

async fn set_route_flush_pause(pool: &PgPool, route: &str, paused: bool) -> Result<()> {
    let scopes = if paused { vec!["flush"] } else { Vec::new() };
    sqlx::query(
        r#"
        INSERT INTO route_pauses (route, paused_scopes)
        VALUES ($1, $2)
        ON CONFLICT (route) DO UPDATE SET paused_scopes = EXCLUDED.paused_scopes
        "#,
    )
    .bind(route)
    .bind(scopes)
    .execute(pool)
    .await?;
    Ok(())
}

async fn set_product_flush_pause(pool: &PgPool, product_id: Uuid, paused: bool) -> Result<()> {
    let scopes = if paused {
        vec!["flush".to_owned()]
    } else {
        Vec::new()
    };
    topup::db::set_product_paused_scopes(pool, product_id, &scopes).await?;
    Ok(())
}

async fn set_account_flush_pause(pool: &PgPool, account_id: Uuid, paused: bool) -> Result<()> {
    let scopes = if paused {
        vec!["flush".to_owned()]
    } else {
        Vec::new()
    };
    topup::db::set_account_paused_scopes(pool, account_id, &scopes).await?;
    Ok(())
}

/// Wraps the dev signer and blocks operator signing until the test releases it.
struct GatedSigner {
    inner: DevSigner,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

impl Signer for GatedSigner {
    async fn sign_operator_tx(&self, tx: TxRequest) -> Result<SignedTx, SignerError> {
        self.entered.notify_one();
        self.release.notified().await;
        self.inner.sign_operator_tx(tx).await
    }

    async fn sign_settlement(&self, payload: &[u8]) -> Result<Ed25519Signature, SignerError> {
        self.inner.sign_settlement(payload).await
    }

    async fn operator_address(&self) -> Result<Address, SignerError> {
        self.inner.operator_address().await
    }

    async fn settlement_public_key(&self) -> Result<Ed25519PublicKey, SignerError> {
        self.inner.settlement_public_key().await
    }
}

fn dev_signer(key: &str) -> Result<DevSigner> {
    let bytes = hex::decode(key)?;
    let operator = SecretKey32::from_slice(&bytes).context("operator key has 32 bytes")?;
    Ok(DevSigner::new(operator, SecretKey32::new([9; 32])))
}

fn stub_flusher(pool: &PgPool, signer: SignerHandle) -> (Planner, Flusher) {
    let chain = Arc::new(StubChain::default());
    let alerts = Arc::new(Alerts::default());
    let planner = Planner::new(
        pool.clone(),
        chain.clone(),
        signer.clone(),
        Arc::new(FixedPrice),
        alerts.clone(),
    );
    let flusher = Flusher::new(
        pool.clone(),
        chain,
        signer,
        alerts,
        FlusherPolicy::default(),
    );
    (planner, flusher)
}

/// Waits until another session of this test database blocks on a lock inside `statement`.
async fn wait_for_lock_wait(pool: &PgPool, statement: &str) -> Result<()> {
    for _ in 0..250 {
        let waiting: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS (
                SELECT 1 FROM pg_stat_activity
                WHERE datname = current_database()
                  AND wait_event_type = 'Lock'
                  AND strpos(query, $1) > 0
            )
            "#,
        )
        .bind(statement)
        .fetch_one(pool)
        .await?;
        if waiting {
            return Ok(());
        }
        tokio::time::sleep(StdDuration::from_millis(20)).await;
    }
    bail!("no session blocked inside `{statement}`")
}

async fn copy_planned_flush(pool: &PgPool, source: Uuid, nonce: u64) -> Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO flushes (id, chain_id, token, operator, nonce, status, receipt)
        SELECT $2, chain_id, token, operator, $3::text::numeric, status, receipt
        FROM flushes WHERE id = $1 AND status = 'planned'
        "#,
    )
    .bind(source)
    .bind(id)
    .bind(nonce.to_string())
    .execute(pool)
    .await?;
    Ok(id)
}

async fn flush_nonce(pool: &PgPool, flush_id: Uuid) -> Result<u64> {
    let nonce: String = sqlx::query_scalar("SELECT nonce::text FROM flushes WHERE id = $1")
        .bind(flush_id)
        .fetch_one(pool)
        .await?;
    Ok(nonce.parse()?)
}

async fn planned_address_ids(pool: &PgPool, flush_id: Uuid) -> Result<Vec<Uuid>> {
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT item->>'address_id' FROM flushes, jsonb_array_elements(receipt->'plan') AS item WHERE id = $1",
    )
    .bind(flush_id)
    .fetch_all(pool)
    .await?;
    ids.iter().map(|id| Ok(Uuid::parse_str(id)?)).collect()
}

/// Asserts that a plan was voided by a flush pause and returns the audit reason naming the level.
async fn voided_reason(pool: &PgPool, flush_id: Uuid) -> Result<String> {
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM flushes WHERE id = $1")
        .bind(flush_id)
        .fetch_one(pool)
        .await?;
    ensure!(remaining == 0, "paused plan {flush_id} was not voided");
    let reasons: Vec<String> = sqlx::query_scalar(
        "SELECT reason FROM audit WHERE action = 'flush.send_paused' AND subject = $1",
    )
    .bind(flush_id.to_string())
    .fetch_all(pool)
    .await?;
    ensure!(
        reasons.len() == 1,
        "expected one pause audit row for {flush_id}"
    );
    Ok(reasons.concat())
}

async fn completed_flush_count(pool: &PgPool) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "SELECT count(*) FROM flushes WHERE status IN ('sent', 'confirmed', 'reverted')",
    )
    .fetch_one(pool)
    .await?)
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
    let committed = topup::db::commit_scan(pool, 31_337, &[deposit], &[], None, None).await?;
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

//! Fast credit and reversal on Anvil and PostgreSQL (docs/design/multi-tenant.md §4, §16 PR 1):
//! a route crediting at depth 2, reorganized with `anvil_reorg`.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, bail, ensure};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use sqlx::{PgPool, Row};
use tokio_util::sync::CancellationToken;
use topup::audit::Actor;
use topup::db;
use topup::deposit_addresses;
use topup::finality::FinalityWatch;
use topup::pump::{Pump, PumpConfig, RunOnceResult, StepSet};
use topup::routes::RouteSet;
use topup::scanner::{
    ChainRoutes, FinalizedHeads, HeadScan, ScanConfig, chain_routes, head_scan_once, run_chain,
    scan_once,
};
use topup::steps::confirm::ConfirmStep;
use topup::steps::screen::{ScreenRoute, ScreenStep};
use topup::steps::sweep::SweepStep;
use topup::tenancy::Scope;
use topup_adapters::chain::evm::metrics::provider_call_counts;
use topup_adapters::chain::evm::{
    ChainError, ChainReader, EvmClient, FinalizedHead, FinalizedReader, ReceiptLookup, TransferLog,
};
use topup_adapters::pricing::{Observation, PriceError, PriceSource};
use topup_adapters::risk::oracle::SanctionsSource;
use topup_core::deposit::DepositState;
use topup_core::identity::{credited_event_id, deposit_id, reversed_event_id};
use topup_core::money::{AtomicAmount, PRICE_SCALE, ScaledPrice};
use topup_core::route::{ChainHeads, Confirmations, RouteFile};
use topup_core::screening::{Bounds, SanctionsAnswer, SanctionsResult};
use topup_core::valuation::{SourceId, UnixSeconds};
use uuid::Uuid;

use support::TestDatabase;
use support::chain::{ANVIL_PRIVATE_KEY, Anvil, CHAIN_ID, forge_create, run_checked};
use support::seed::{self, NewAccount, NewAddress};

/// Anvil's second and third default accounts: the payer, and a sender whose transaction shifts
/// the payer's log within a re-mined block.
const PAYER_KEY: &str = "59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d";
const PAYER: &str = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
const OTHER_KEY: &str = "5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a";
const OTHER: &str = "0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC";
const AMOUNT: u64 = 1_000;
/// Anvil's default 32-slot epochs keep `finalized` 64 blocks and `safe` 32 blocks behind the head,
/// so a reorg of a few blocks stays above both.
const FINALITY_DEPTH: u64 = 64;

#[tokio::test]
async fn a_depth_one_reorg_before_credit_changes_nothing() -> Result<()> {
    run(&[], |chain| {
        Box::pin(async move {
            let tx = chain.pay(AMOUNT)?;
            // Depth 1: not credited yet.
            ensure!(chain.scan().await? == 0, "a depth-1 transfer was recorded");
            let raw = chain.raw_transaction(tx)?;
            chain.reorg(1, &[(&raw, 0)])?;
            let (_, reorged_hash) = chain.receipt_block(tx)?.context("re-mined receipt")?;
            chain.anvil.mine(1)?;

            ensure!(chain.scan().await? == 1);
            chain.settle().await?;
            let deposit = chain.deposit(tx).await?;
            ensure!(deposit.state == DepositState::Credited);
            ensure!(
                deposit.block_hash == reorged_hash,
                "evidence is the re-mined block"
            );
            ensure!(chain.count("SELECT count(*) FROM deposits").await? == 1);
            ensure!(chain.events("deposit.credited").await? == vec![credited_event_id(deposit.id)]);
            ensure!(chain.events("deposit.reversed").await?.is_empty());
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_re_included_transaction_keeps_its_deposit_and_is_followed_not_reversed() -> Result<()> {
    run(&[], |chain| {
        Box::pin(async move {
            let tx = chain.pay(AMOUNT)?;
            chain.anvil.mine(1)?;
            ensure!(chain.scan().await? == 1);
            chain.settle().await?;
            let credited = chain.deposit(tx).await?;
            ensure!(credited.state == DepositState::Credited);
            ensure!(credited.final_at.is_none(), "a depth-2 credit is not final");

            // The transaction moves one block later, behind another sender's transfer in the
            // same block, so its block-wide log index changes and its receipt position does not.
            let raw = chain.raw_transaction(tx)?;
            let shift = chain.other_transfer_raw()?;
            chain.reorg(2, &[(&shift, 1), (&raw, 1)])?;
            let (block_number, block_hash) =
                chain.receipt_block(tx)?.context("re-included receipt")?;
            ensure!(block_number == credited.block_number + 1);

            // Nothing is read before the recorded block is final.
            let stats = chain.watch().await?;
            ensure!(stats.watched == 0, "{stats:?}");

            // At finality one receipt per provider shows the transfer in its new, final block.
            chain.anvil.mine(FINALITY_DEPTH + 2)?;
            let stats = chain.watch().await?;
            ensure!(stats.finalized == 1 && stats.reversed == 0, "{stats:?}");
            let final_deposit = chain.deposit(tx).await?;
            ensure!(final_deposit.id == credited.id);
            ensure!(final_deposit.final_at.is_some());
            ensure!(final_deposit.state == DepositState::Credited);
            ensure!(
                final_deposit.block_number == block_number
                    && final_deposit.block_hash == block_hash
            );
            ensure!(final_deposit.receipt_log_index == 0);
            ensure!(
                final_deposit.log_index != credited.log_index,
                "the re-mined block puts another log first"
            );
            ensure!(chain.count("SELECT count(*) FROM deposits").await? == 1);
            ensure!(chain.events("deposit.reversed").await?.is_empty());
            ensure!(chain.events("deposit.credited").await?.len() == 1);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_transaction_replaced_with_the_same_nonce_is_reversed_once() -> Result<()> {
    run(&[], |chain| {
        Box::pin(async move {
            let nonce = chain.payer_nonce()?;
            let tx = chain.pay(AMOUNT)?;
            chain.anvil.mine(1)?;
            ensure!(chain.scan().await? == 1);
            chain.settle().await?;
            let credited = chain.deposit(tx).await?;
            ensure!(credited.state == DepositState::Credited);
            ensure!(credited.price_source.as_deref() == Some("lock"));
            ensure!(chain.quote_status().await? == ("consumed".to_owned(), Some(credited.id)));

            // The API refunds only final deposits; a pending refund of this one is written
            // directly, to show a reversal cancels it.
            let refund = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO refunds (id, account_id, livemode, chain_id, deposit_id, \
                 amount_atomic, destination_address, status) \
                 SELECT $1, account_id, livemode, chain_id, id, 1, $3, 'pending' \
                 FROM deposits WHERE id = $2",
            )
            .bind(refund)
            .bind(credited.id)
            .bind(OTHER.to_lowercase())
            .execute(&chain.pool)
            .await?;

            // The payer's nonce is spent on another transaction instead.
            let replacement = chain.payer_replacement(nonce)?;
            chain.reorg(2, &[(&replacement, 0)])?;
            ensure!(chain.receipt_block(tx)?.is_none());

            // Not final yet: the deposit is not read, and it may still come back.
            let stats = chain.watch().await?;
            ensure!(stats.watched == 0, "{stats:?}");
            ensure!(chain.deposit(tx).await?.state == DepositState::Credited);

            chain.anvil.mine(FINALITY_DEPTH + 2)?;
            let stats = chain.watch().await?;
            ensure!(stats.reversed == 1, "{stats:?}");
            let reversed = chain.deposit(tx).await?;
            ensure!(reversed.state == DepositState::Reversed);
            ensure!(reversed.final_at.is_none());
            ensure!(
                chain.events("deposit.reversed").await? == vec![reversed_event_id(credited.id)]
            );
            let evidence: Value = sqlx::query_scalar(
                "SELECT evidence FROM transitions \
                 WHERE deposit_id = $1 AND from_state = 'credited' AND to_state = 'reversed'",
            )
            .bind(credited.id)
            .fetch_one(&chain.pool)
            .await?;
            ensure!(evidence["result"] == "dropped_nonce_consumed");
            let refund_status: String =
                sqlx::query_scalar("SELECT status FROM refunds WHERE id = $1")
                    .bind(refund)
                    .fetch_one(&chain.pool)
                    .await?;
            ensure!(refund_status == "canceled");
            // The quote's window is still open, so it opens again with its reservation.
            ensure!(chain.quote_status().await? == ("open".to_owned(), None));

            // Terminal: later passes and pumps leave it alone, and the event stays single.
            let again = chain.watch().await?;
            ensure!(again.watched == 0, "{again:?}");
            ensure!(chain.pump.run_once().await? == RunOnceResult::Idle);
            ensure!(chain.events("deposit.reversed").await?.len() == 1);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn payments_to_active_and_retired_deposit_addresses_are_credited_at_spot() -> Result<()> {
    run(&[], |chain| {
        Box::pin(async move {
            let (account, customer) = seed::create_account_and_customer(
                &chain.pool,
                &NewAccount {
                    livemode: false,
                    ..NewAccount::named("deposit-address-merchant")
                },
                "team-da",
            )
            .await?;
            let chains = [deposit_addresses::Chain::of(&chain.route)];
            let (retired, created) =
                deposit_addresses::create(&chain.pool, &account, &customer, &chains, None).await?;
            ensure!(created);
            let active = deposit_addresses::rotate(
                &chain.pool,
                &account,
                Scope::new(account.id, false),
                &Actor::system("test"),
                retired.id,
                &chains,
            )
            .await?;
            let [retired_network] = retired.networks.as_slice() else {
                bail!("one network: {:?}", retired.networks);
            };
            let [active_network] = active.networks.as_slice() else {
                bail!("one network: {:?}", active.networks);
            };

            let to_retired = chain.pay_to(retired_network.address, AMOUNT)?;
            let to_active = chain.pay_to(active_network.address, AMOUNT)?;
            chain.anvil.mine(1)?;
            // The per-block scan covers deposit addresses, active and retired, like every address.
            ensure!(chain.scan().await? == 2);
            chain.settle().await?;
            for (tx, network) in [(to_retired, retired_network), (to_active, active_network)] {
                let deposit = chain.deposit(tx).await?;
                ensure!(
                    deposit.state == DepositState::Credited,
                    "{:?}",
                    deposit.state
                );
                ensure!(deposit.price_source.as_deref() == Some("spot"));
                ensure!(deposit.address_id == network.address_id);
                ensure!(deposit.customer_id == customer.id);
                ensure!(deposit.account_id == account.id && !deposit.livemode);
                ensure!(
                    chain
                        .events("deposit.credited")
                        .await?
                        .contains(&credited_event_id(deposit.id))
                );
            }
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_lagging_provider_b_delays_the_credit_until_it_reaches_the_depth() -> Result<()> {
    let lag = Arc::new(AtomicU64::new(1));
    let lagging = Arc::clone(&lag);
    run_with(
        &[],
        move |rpc_url| {
            Ok(LaggingReader {
                inner: reader(rpc_url)?,
                lag: Arc::clone(&lagging),
            })
        },
        |chain| {
            Box::pin(async move {
                let tx = chain.pay(AMOUNT)?;
                chain.anvil.mine(1)?;
                ensure!(chain.scan().await? == 1);
                let id = chain.deposit(tx).await?.id;
                ensure!(chain.pump.run_once().await? == RunOnceResult::Applied { deposit_id: id });
                let waiting = chain.deposit(tx).await?;
                ensure!(waiting.state == DepositState::Detected);
                let evidence: Value = sqlx::query_scalar(
                    "SELECT evidence FROM transitions WHERE deposit_id = $1 \
                     ORDER BY created_at DESC LIMIT 1",
                )
                .bind(id)
                .fetch_one(&chain.pool)
                .await?;
                ensure!(evidence["result"] == "not_confirmed", "{evidence}");
                // Retried within a few seconds, not a full wait interval.
                let delay = waiting.next_attempt_at - Utc::now();
                ensure!(delay <= chrono::Duration::seconds(3), "{delay}");

                lag.store(0, Ordering::SeqCst);
                sqlx::query("UPDATE deposits SET next_attempt_at = now() WHERE id = $1")
                    .bind(id)
                    .execute(&chain.pool)
                    .await?;
                chain.settle().await?;
                ensure!(chain.deposit(tx).await?.state == DepositState::Credited);
                Ok(())
            })
        },
    )
    .await
}

#[tokio::test]
async fn a_payment_is_credited_within_thirty_seconds_of_inclusion_on_twelve_second_blocks()
-> Result<()> {
    run(&["--block-time", "12"], |chain| {
        Box::pin(async move {
            let label = "latency-provider-a";
            let cancellation = CancellationToken::new();
            let pump = chain.pump.clone();
            let pump_task = tokio::spawn({
                let cancellation = cancellation.clone();
                async move { pump.run(cancellation).await }
            });
            // The production loop at its production cadence: a head poll per block time.
            let scanner_task = tokio::spawn(run_chain(
                chain.pool.clone(),
                labeled_reader(&chain.anvil.rpc_url, label)?,
                chain.routes.clone(),
                ScanConfig::default(),
                FinalizedHeads::default(),
                cancellation.clone(),
            ));
            let started = chain.anvil.block_number()?;
            let tx = chain.send_payment(AMOUNT)?;
            let result = async {
                let mut included_at = None;
                let deadline = Instant::now() + Duration::from_secs(90);
                loop {
                    ensure!(Instant::now() < deadline, "no credit within 90 s");
                    if included_at.is_none() && chain.receipt_block(tx)?.is_some() {
                        included_at = Some(Utc::now());
                    }
                    if let Some(included_at) = included_at {
                        let created = sqlx::query_scalar::<_, chrono::DateTime<Utc>>(
                            "SELECT created FROM events WHERE type = 'deposit.credited'",
                        )
                        .fetch_optional(&chain.pool)
                        .await?;
                        if let Some(created) = created {
                            let elapsed = created - included_at;
                            eprintln!("deposit.credited written {elapsed} after inclusion");
                            ensure!(
                                elapsed < chrono::Duration::seconds(30),
                                "credited {elapsed} after inclusion"
                            );
                            return Ok(());
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
            .await;
            cancellation.cancel();
            pump_task.await?;
            scanner_task.await??;
            result?;

            // Provider A's calls over the run: about one head poll and one log request per block,
            // and one receipt, block, and transaction read for the one transfer.
            let blocks = chain.anvil.block_number()?.saturating_sub(started).max(1);
            let calls = provider_call_counts(label);
            eprintln!("provider A calls over {blocks} blocks: {calls:?}");
            let count = |method| calls.get(method).copied().unwrap_or_default();
            ensure!(count("eth_blockNumber") <= 2 * blocks + 4, "{calls:?}");
            ensure!(count("eth_getLogs") <= blocks + 1, "{calls:?}");
            ensure!(count("eth_getTransactionReceipt") <= 2, "{calls:?}");
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn an_idle_chain_polls_only_the_head_and_a_new_block_costs_one_log_request() -> Result<()> {
    run(&[], |chain| {
        Box::pin(async move {
            let label = "idle-provider-a";
            let cancellation = CancellationToken::new();
            let config = ScanConfig {
                head_poll_interval: Some(Duration::from_millis(200)),
                finalized_poll_interval: Duration::from_secs(3_600),
            };
            let scanner_task = tokio::spawn(run_chain(
                chain.pool.clone(),
                labeled_reader(&chain.anvil.rpc_url, label)?,
                chain.routes.clone(),
                config,
                FinalizedHeads::default(),
                cancellation.clone(),
            ));
            let count = |method| {
                provider_call_counts(label)
                    .get(method)
                    .copied()
                    .unwrap_or_default()
            };
            let result = async {
                // The first poll reads `finalized` and scans the blocks above the cursor once.
                let deadline = Instant::now() + Duration::from_secs(10);
                while count("eth_getLogs") == 0 {
                    ensure!(Instant::now() < deadline, "the first poll never scanned");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                let before = provider_call_counts(label);
                tokio::time::sleep(Duration::from_secs(3)).await;
                let idle = provider_call_counts(label);
                let polls = idle["eth_blockNumber"] - before["eth_blockNumber"];
                ensure!(polls >= 5, "only {polls} head polls in 3 s");
                for (method, calls) in &idle {
                    ensure!(
                        *method == "eth_blockNumber" || before.get(method) == Some(calls),
                        "an idle chain called {method}: {before:?} -> {idle:?}"
                    );
                }

                chain.anvil.mine(1)?;
                let logs = count("eth_getLogs");
                let deadline = Instant::now() + Duration::from_secs(10);
                while count("eth_getLogs") == logs {
                    ensure!(Instant::now() < deadline, "a new block was not scanned");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
                ensure!(
                    count("eth_getLogs") == logs + 1,
                    "one new block costs one log request, whatever the address count"
                );
                Ok(())
            }
            .await;
            cancellation.cancel();
            scanner_task.await??;
            result
        })
    })
    .await
}

#[tokio::test]
async fn a_newly_issued_address_is_scanned_from_the_next_block_without_a_gap() -> Result<()> {
    run(&[], |chain| {
        Box::pin(async move {
            let first = chain.head_scan().await?;
            chain.anvil.mine(2)?;
            let second = chain.head_scan().await?;
            let horizon = |scan: &HeadScan| scan.horizon.context("a depth route has a horizon");
            ensure!(
                second.from_block == horizon(&first)? + 1,
                "{first:?} {second:?}"
            );

            // An address issued now, with no open quote, is paid in the next block.
            let (address_id, address) = issue_address(&chain.pool, chain.customer_id, 0x6b).await?;
            let tx = chain.pay_to(address, AMOUNT)?;
            chain.anvil.mine(1)?;
            let third = chain.head_scan().await?;
            ensure!(
                third.from_block == horizon(&second)? + 1,
                "{second:?} {third:?}"
            );
            ensure!(third.inserted == 1, "{third:?}");
            let deposit = chain.deposit(tx).await?;
            ensure!(deposit.address_id == address_id);
            ensure!(deposit.state == DepositState::Detected);
            Ok(())
        })
    })
    .await
}

async fn run<S>(anvil_args: &[&str], scenario: S) -> Result<()>
where
    S: for<'a> FnOnce(&'a FastChain) -> support::TestFuture<'a>,
{
    run_with(anvil_args, reader, scenario).await
}

async fn run_with<B, F, S>(anvil_args: &[&str], secondary: F, scenario: S) -> Result<()>
where
    B: ChainReader + Send + Sync + 'static,
    F: Fn(&str) -> Result<B>,
    S: for<'a> FnOnce(&'a FastChain) -> support::TestFuture<'a>,
{
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let Some(anvil) = Anvil::start_if_available(anvil_args).await? else {
        database.cleanup().await?;
        return Ok(());
    };
    let result = async {
        let chain = FastChain::setup(&database, anvil, secondary).await?;
        scenario(&chain).await
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

struct FastChain {
    anvil: Anvil,
    pool: PgPool,
    token: Address,
    customer_id: Uuid,
    address: Address,
    address_id: Uuid,
    route: RouteFile,
    routes: ChainRoutes,
    reader: FinalizedReader,
    pump: Pump,
    watch: FinalityWatch,
}

impl FastChain {
    async fn setup<B, F>(database: &TestDatabase, anvil: Anvil, secondary: F) -> Result<Self>
    where
        B: ChainReader + Send + Sync + 'static,
        F: Fn(&str) -> Result<B>,
    {
        let pool = database.app_pool.clone();
        // History below `finalized` for the finalized scanner's first cursor.
        anvil.mine(2 * FINALITY_DEPTH)?;
        let token = forge_create(&anvil.rpc_url, "test/mocks/MockTokens.sol:MockERC20", &[])?;
        for (account, amount) in [(PAYER, 1_000_000_u64), (OTHER, 1_000_000)] {
            send(
                &anvil,
                ANVIL_PRIVATE_KEY,
                &[
                    &format!("{token:#x}"),
                    "mint(address,uint256)",
                    account,
                    &amount.to_string(),
                ],
            )?;
        }
        anvil.mine(4)?;

        let (customer_id, address_id, address) = seed_address(&pool).await?;
        let mut route: RouteFile = serde_saphyr::from_str(
            &include_str!("fixtures/phala-cloud-pha.yaml")
                .replace("chain_id: 1", &format!("chain_id: {CHAIN_ID}"))
                .replace("livemode: true", "livemode: false")
                .replace(
                    "0x6c5bA91642F10282b576d91922Ae6448C9d52f4E",
                    &format!("{token:#x}"),
                ),
        )?;
        route.chain.confirmations = Confirmations::Depth(2);
        route.asset.decimals = 0;
        route.rate_lock.amount_decimals = 0;
        route.destination.unit_decimals = 0;
        route.screening.min_credit_minor = 1;
        route.screening.min_deposit_atomic = AtomicAmount::new(U256::ZERO);
        route.validate()?;
        let routes = chain_routes(&RouteSet::new(vec![route.clone()]).map_err(anyhow::Error::msg)?)
            .into_iter()
            .next()
            .context("one chain route")?;

        let now = u64::try_from(Utc::now().timestamp())?;
        let price = |source: &str, value: u64| -> Arc<dyn PriceSource> {
            Arc::new(FixedPrice(Observation {
                source: SourceId::new(source),
                price: ScaledPrice::new(value, PRICE_SCALE).expect("fixture price"),
                observed_at: UnixSeconds::new(now),
            }))
        };
        let confirm = ConfirmStep::single(
            pool.clone(),
            route.clone(),
            reader(&anvil.rpc_url)?,
            secondary(&anvil.rpc_url)?,
            price("coinmetrics", 10_000_000),
            Some(price("binance", 10_000_000)),
            Some(price("kraken", 100_000_000)),
        );
        let screen = ScreenStep::new(
            pool.clone(),
            [ScreenRoute::new(
                route.route.clone(),
                route.version,
                route.screening.sanctions_oracle,
                Bounds::from(&route.screening),
                Arc::new(ClearSanctions),
            )],
        )?;
        let pump = Pump::new(
            pool.clone(),
            Arc::new(StepSet::new(
                Box::new(confirm),
                Box::new(screen),
                Box::new(SweepStep),
            )),
            PumpConfig::default(),
        )?;
        let watch = FinalityWatch::single(
            pool.clone(),
            CHAIN_ID,
            reader(&anvil.rpc_url)?,
            secondary(&anvil.rpc_url)?,
        );
        let reader = reader(&anvil.rpc_url)?;
        // The finalized scanner's first pass writes the cursor the fast scan starts above.
        scan_once(&pool, &reader, &routes).await?;
        let chain = Self {
            anvil,
            pool,
            token,
            customer_id,
            address,
            address_id,
            route,
            routes,
            reader,
            pump,
            watch,
        };
        // Every address is a quote's; the fast scan watches open quotes.
        chain.open_quote().await?;
        Ok(chain)
    }

    /// Transfers `amount` from the payer to the quote's address and waits for its receipt.
    fn pay(&self, amount: u64) -> Result<B256> {
        self.pay_to(self.address, amount)
    }

    /// Transfers `amount` from the payer to `to` and waits for its receipt.
    fn pay_to(&self, to: Address, amount: u64) -> Result<B256> {
        let output = send(
            &self.anvil,
            PAYER_KEY,
            &[
                &format!("{:#x}", self.token),
                "transfer(address,uint256)",
                &format!("{to:#x}"),
                &amount.to_string(),
            ],
        )?;
        let receipt: Value = serde_json::from_slice(&output.stdout)?;
        receipt["transactionHash"]
            .as_str()
            .context("transaction hash")?
            .parse()
            .context("parse transaction hash")
    }

    /// Sends the payment without waiting for a block.
    fn send_payment(&self, amount: u64) -> Result<B256> {
        let output = run_checked(
            "cast",
            &[
                "send",
                "--async",
                "--rpc-url",
                &self.anvil.rpc_url,
                "--private-key",
                PAYER_KEY,
                &format!("{:#x}", self.token),
                "transfer(address,uint256)",
                &format!("{:#x}", self.address),
                &amount.to_string(),
            ],
            None,
        )?;
        String::from_utf8(output.stdout)?
            .trim()
            .parse()
            .context("parse transaction hash")
    }

    fn raw_transaction(&self, tx: B256) -> Result<String> {
        let output = rpc(
            &self.anvil,
            "eth_getRawTransactionByHash",
            &[&format!("{tx:#x}")],
        )?;
        Ok(String::from_utf8(output.stdout)?
            .trim()
            .trim_matches('"')
            .to_owned())
    }

    /// Another sender's token transfer, tipped above the payer's so it comes first in a block.
    fn other_transfer_raw(&self) -> Result<String> {
        let output = run_checked(
            "cast",
            &[
                "mktx",
                "--rpc-url",
                &self.anvil.rpc_url,
                "--private-key",
                OTHER_KEY,
                "--gas-price",
                "100gwei",
                "--priority-gas-price",
                "50gwei",
                &format!("{:#x}", self.token),
                "transfer(address,uint256)",
                OTHER,
                "1",
            ],
            None,
        )?;
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    /// A payer transaction with `nonce` that pays someone else.
    fn payer_replacement(&self, nonce: u64) -> Result<String> {
        let output = run_checked(
            "cast",
            &[
                "mktx",
                "--rpc-url",
                &self.anvil.rpc_url,
                "--private-key",
                PAYER_KEY,
                "--nonce",
                &nonce.to_string(),
                &format!("{:#x}", self.token),
                "transfer(address,uint256)",
                OTHER,
                "1",
            ],
            None,
        )?;
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }

    fn payer_nonce(&self) -> Result<u64> {
        let output = run_checked(
            "cast",
            &["nonce", "--rpc-url", &self.anvil.rpc_url, PAYER],
            None,
        )?;
        Ok(String::from_utf8(output.stdout)?.trim().parse()?)
    }

    /// Replaces the last `depth` blocks with as many new blocks, carrying `transactions` at their
    /// block offsets; transactions of the replaced blocks are dropped.
    fn reorg(&self, depth: u64, transactions: &[(&str, u64)]) -> Result<()> {
        let pairs = serde_json::to_string(
            &transactions
                .iter()
                .map(|(raw, offset)| serde_json::json!([raw, offset]))
                .collect::<Vec<_>>(),
        )?;
        rpc(&self.anvil, "anvil_reorg", &[&depth.to_string(), &pairs])?;
        Ok(())
    }

    fn receipt_block(&self, tx: B256) -> Result<Option<(u64, B256)>> {
        let output = rpc(
            &self.anvil,
            "eth_getTransactionReceipt",
            &[&format!("{tx:#x}")],
        )?;
        let receipt: Value = serde_json::from_slice(&output.stdout)?;
        if receipt.is_null() {
            return Ok(None);
        }
        let number = receipt["blockNumber"].as_str().context("block number")?;
        let number = u64::from_str_radix(number.trim_start_matches("0x"), 16)?;
        let hash = receipt["blockHash"]
            .as_str()
            .context("block hash")?
            .parse()?;
        Ok(Some((number, hash)))
    }

    /// One per-block scan, as the head loop runs on each new head.
    async fn head_scan(&self) -> Result<HeadScan> {
        head_scan_once(&self.pool, &self.reader, &self.routes)
            .await?
            .context("a new block is scanned")
    }

    /// Deposits one per-block scan records at the route's depth.
    async fn scan(&self) -> Result<u64> {
        Ok(self.head_scan().await?.inserted)
    }

    /// Runs the pump until nothing is due.
    async fn settle(&self) -> Result<()> {
        for _ in 0..10 {
            if self.pump.run_once().await? == RunOnceResult::Idle {
                return Ok(());
            }
        }
        bail!("the pump did not settle")
    }

    async fn watch(&self) -> Result<topup::finality::WatchStats> {
        Ok(self.watch.watch_once(CHAIN_ID).await?)
    }

    async fn deposit(&self, tx: B256) -> Result<db::Deposit> {
        db::get_deposit(&self.pool, deposit_id(CHAIN_ID, tx, 0))
            .await?
            .context("deposit by receipt position")
    }

    async fn count(&self, query: &'static str) -> Result<i64> {
        Ok(sqlx::query_scalar(query).fetch_one(&self.pool).await?)
    }

    async fn events(&self, event_type: &str) -> Result<Vec<Uuid>> {
        Ok(
            sqlx::query_scalar("SELECT id FROM events WHERE type = $1 ORDER BY id")
                .bind(event_type)
                .fetch_all(&self.pool)
                .await?,
        )
    }

    /// Opens a quote on the address for exactly [`AMOUNT`], its window an hour long.
    async fn open_quote(&self) -> Result<()> {
        sqlx::query(
            r#"
            UPDATE quotes
            SET route = $2, amount_atomic = $3::text::numeric, price_scaled = 9000000,
                expires_at = now() + interval '1 hour', credit_minor = 90, status = 'open',
                exposure_reserved = true, closed_at = NULL
            WHERE id = (SELECT quote_id FROM addresses WHERE id = $1)
            "#,
        )
        .bind(self.address_id)
        .bind(&self.route.route)
        .bind(AMOUNT.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn quote_status(&self) -> Result<(String, Option<Uuid>)> {
        let row = sqlx::query(
            "SELECT quote.status, quote.consumed_by, quote.exposure_reserved FROM quotes AS quote \
             JOIN addresses AS address ON address.quote_id = quote.id WHERE address.id = $1",
        )
        .bind(self.address_id)
        .fetch_one(&self.pool)
        .await?;
        let status: String = row.try_get("status")?;
        let reserved: bool = row.try_get("exposure_reserved")?;
        ensure!(
            reserved == (status == "open"),
            "an open quote reserves its exposure"
        );
        Ok((status, row.try_get("consumed_by")?))
    }
}

fn send(anvil: &Anvil, key: &str, call: &[&str]) -> Result<std::process::Output> {
    let mut arguments = vec![
        "send",
        "--json",
        "--rpc-url",
        &anvil.rpc_url,
        "--private-key",
        key,
    ];
    arguments.extend_from_slice(call);
    run_checked("cast", &arguments, None)
}

fn rpc(anvil: &Anvil, method: &str, params: &[&str]) -> Result<std::process::Output> {
    let mut arguments = vec!["rpc", "--rpc-url", &anvil.rpc_url, method];
    arguments.extend_from_slice(params);
    run_checked("cast", &arguments, None)
}

fn reader(rpc_url: &str) -> Result<FinalizedReader> {
    Ok(FinalizedReader::new(Arc::new(EvmClient::new(rpc_url)?)))
}

/// A reader whose calls are counted under `label`, unique to one test.
fn labeled_reader(rpc_url: &str, label: &str) -> Result<FinalizedReader> {
    Ok(FinalizedReader::new(Arc::new(
        EvmClient::new(rpc_url)?
            .with_provider(label)
            .with_chain_id(CHAIN_ID),
    )))
}

async fn seed_address(pool: &PgPool) -> Result<(Uuid, Uuid, Address)> {
    let (_, customer) = seed::create_account_and_customer(
        pool,
        &NewAccount {
            livemode: false,
            webhook_url: "https://product.test/webhooks".to_owned(),
            ..NewAccount::named("phala-cloud")
        },
        "workspace-fast",
    )
    .await?;
    let (address_id, address) = issue_address(pool, customer.id, 0x5a).await?;
    Ok((customer.id, address_id, address))
}

/// Issues the customer's address `0x<byte>…`, created at the current finalized cursor.
async fn issue_address(pool: &PgPool, customer_id: Uuid, byte: u8) -> Result<(Uuid, Address)> {
    let address = Address::repeat_byte(byte);
    let address_id = Uuid::new_v4();
    seed::insert_address(
        pool,
        &NewAddress {
            id: address_id,
            customer_id,
            chain_id: CHAIN_ID,
            route: "phala-cloud-ethereum-pha-usd".to_owned(),
            salt: B256::repeat_byte(byte),
            address,
        },
    )
    .await?;
    Ok((address_id, address))
}

/// Provider B, `lag` blocks behind provider A's head.
struct LaggingReader {
    inner: FinalizedReader,
    lag: Arc<AtomicU64>,
}

impl ChainReader for LaggingReader {
    async fn factory_logs(
        &self,
        factory: Address,
        forwarders: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<topup_adapters::chain::evm::FactoryLog>, ChainError> {
        self.inner
            .factory_logs(factory, forwarders, from_block, to_block)
            .await
    }

    async fn finalized_head(&self) -> Result<FinalizedHead, ChainError> {
        self.inner.finalized_head().await
    }

    async fn confirmation_heads(
        &self,
        confirmations: Confirmations,
    ) -> Result<ChainHeads, ChainError> {
        let heads = self.inner.confirmation_heads(confirmations).await?;
        let lag = self.lag.load(Ordering::SeqCst);
        Ok(ChainHeads {
            latest: heads.latest.map(|latest| latest.saturating_sub(lag)),
            ..heads
        })
    }

    async fn transfer_logs_to(
        &self,
        addresses: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<TransferLog>, ChainError> {
        self.inner
            .transfer_logs_to(addresses, from_block, to_block)
            .await
    }

    async fn receipt_transfer(
        &self,
        tx_hash: B256,
        receipt_log_index: u64,
    ) -> Result<ReceiptLookup, ChainError> {
        self.inner
            .receipt_transfer(tx_hash, receipt_log_index)
            .await
    }

    async fn nonce_at(&self, account: Address, block: u64) -> Result<u64, ChainError> {
        self.inner.nonce_at(account, block).await
    }
}

struct FixedPrice(Observation);

#[async_trait]
impl PriceSource for FixedPrice {
    async fn observe(&self) -> Result<Observation, PriceError> {
        Ok(self.0.clone())
    }
}

struct ClearSanctions;

#[async_trait]
impl SanctionsSource for ClearSanctions {
    async fn sanctions(&self, _address: Address, block_number: u64) -> SanctionsResult {
        SanctionsResult {
            provider_a: SanctionsAnswer::Clear,
            provider_b: SanctionsAnswer::Clear,
            block_number,
        }
    }
}

//! PostgreSQL-backed reconciliation checks and cancellation behavior.

mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use sqlx::PgPool;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use topup::db::{
    self, AddressKind, FlushedEvent, NewAccount, NewAddress, NewDeposit, NewFlush, NewProduct,
    SettlementIntent,
};
use topup::reconciler::{
    CheckName, Reconciler, ReconciliationChain, ReconciliationError, ReconciliationMetrics,
    SettlementLookup,
};
use topup_adapters::chain::evm::TransferLog;
use topup_adapters::settlement::http::SettlementAnswer;
use topup_core::deposit::DepositState;
use topup_core::identity::deposit_id;
use topup_core::money::AtomicAmount;
use topup_core::route::RouteFile;
use uuid::Uuid;

use support::TestDatabase;

#[derive(Default)]
struct MockChain {
    finalized: u64,
    finalized_delay: StdDuration,
    finalized_started: Notify,
    logs: Mutex<Vec<TransferLog>>,
    balances: Mutex<BTreeMap<Address, U256>>,
    flushed_total: Mutex<U256>,
    derived: Mutex<BTreeMap<B256, Address>>,
}

#[async_trait]
impl ReconciliationChain for MockChain {
    async fn finalized_head(&self) -> Result<u64, ReconciliationError> {
        self.finalized_started.notify_one();
        tokio::time::sleep(self.finalized_delay).await;
        Ok(self.finalized)
    }

    async fn transfer_logs_to(
        &self,
        addresses: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<TransferLog>, ReconciliationError> {
        Ok(self
            .logs
            .lock()
            .map_err(|_| ReconciliationError::Chain("mock log lock poisoned".to_owned()))?
            .iter()
            .filter(|log| {
                addresses.contains(&log.to)
                    && log.block_number >= from_block
                    && log.block_number <= to_block
            })
            .cloned()
            .collect())
    }

    async fn token_balances(
        &self,
        _token: Address,
        addresses: &[Address],
    ) -> Result<Vec<U256>, ReconciliationError> {
        let balances = self
            .balances
            .lock()
            .map_err(|_| ReconciliationError::Chain("mock balance lock poisoned".to_owned()))?;
        Ok(addresses
            .iter()
            .map(|address| balances.get(address).copied().unwrap_or(U256::ZERO))
            .collect())
    }

    async fn flushed_total(
        &self,
        _factory: Address,
        _token: Address,
        _from_block: u64,
        _to_block: u64,
    ) -> Result<U256, ReconciliationError> {
        self.flushed_total
            .lock()
            .map(|value| *value)
            .map_err(|_| ReconciliationError::Chain("mock flushed lock poisoned".to_owned()))
    }

    async fn factory_addresses(
        &self,
        _factory: Address,
        salts: &[B256],
    ) -> Result<Vec<Address>, ReconciliationError> {
        let derived = self
            .derived
            .lock()
            .map_err(|_| ReconciliationError::Chain("mock derivation lock poisoned".to_owned()))?;
        Ok(salts
            .iter()
            .map(|salt| derived.get(salt).copied().unwrap_or(Address::ZERO))
            .collect())
    }
}

#[derive(Default)]
struct MockSettlement {
    answers: Mutex<BTreeMap<String, Option<SettlementAnswer>>>,
}

#[async_trait]
impl SettlementLookup for MockSettlement {
    async fn get_by_key(
        &self,
        _settlement_url: &str,
        key: &str,
    ) -> Result<Option<SettlementAnswer>, ReconciliationError> {
        Ok(self
            .answers
            .lock()
            .map_err(|_| ReconciliationError::Settlement("mock answer lock poisoned".to_owned()))?
            .get(key)
            .cloned()
            .flatten())
    }
}

struct Seed {
    account_id: Uuid,
    address_id: Uuid,
    salt: B256,
    address: Address,
}

#[tokio::test]
async fn repairs_missing_deposit_and_missing_flush_link() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let route = route()?;
        let seed = seed_identity(&context.app_pool, &route, 1).await?;
        let chain = Arc::new(MockChain {
            finalized: 5,
            ..MockChain::default()
        });
        chain
            .derived
            .lock()
            .unwrap()
            .insert(seed.salt, seed.address);
        chain.logs.lock().unwrap().push(TransferLog {
            tx_hash: B256::from([11; 32]),
            log_index: 2,
            block_number: 3,
            block_hash: B256::from([12; 32]),
            block_time: Utc::now(),
            token: route.asset.contract,
            from: Address::from([13; 20]),
            to: seed.address,
            amount: AtomicAmount::new(U256::from(500_u64)),
        });
        let reconciler = reconciler(&context.app_pool, route.clone(), chain, Arc::default())?;

        let missing = reconciler.check_missing_deposits().await?;
        ensure!(missing.len() == 1 && missing[0].repair_applied);
        ensure!(reconciler.check_missing_deposits().await?.is_empty());
        let deposit_id = deposit_id(31_337, B256::from([11; 32]), 2);
        ensure!(
            db::get_deposit(&context.app_pool, deposit_id)
                .await?
                .context("repaired deposit")?
                .state
                == DepositState::Detected
        );

        let linked_id = seed_deposit(
            &context.app_pool,
            &route,
            &seed,
            21,
            DepositState::Credited,
            U256::from(1_000_u64),
            100,
        )
        .await?;
        let flush_id = Uuid::new_v4();
        db::insert_flush(
            &context.app_pool,
            &NewFlush {
                id: flush_id,
                chain_id: route.chain.chain_id,
                token: route.asset.contract,
                operator: Address::from([22; 20]),
                nonce: 1,
                tx_hash: Some(B256::from([23; 32])),
                block_number: Some(101),
                status: "confirmed".to_owned(),
                receipt: Some(serde_json::json!({})),
            },
        )
        .await?;
        db::insert_flushed(
            &context.app_pool,
            &FlushedEvent {
                flush_id,
                address_id: seed.address_id,
                amount_atomic: AtomicAmount::new(U256::from(1_500_u64)),
                block_number: 101,
                log_index: 1,
            },
        )
        .await?;
        let links = reconciler.check_missing_flush_links().await?;
        ensure!(links.iter().any(|finding| finding.repair_applied));
        ensure!(reconciler.check_missing_flush_links().await?.is_empty());
        let linked = db::get_deposit(&context.app_pool, linked_id)
            .await?
            .context("linked deposit")?;
        ensure!(linked.flush_id == Some(flush_id) && linked.state == DepositState::Swept);
        Ok(())
    }
    .await;
    context.cleanup().await?;
    result
}

#[tokio::test]
async fn sent_answers_are_adopted_and_post_restore_stays_incomplete_for_unknown_keys() -> Result<()>
{
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let route = route()?;
        let first = seed_identity(&context.app_pool, &route, 31).await?;
        let second = seed_identity(&context.app_pool, &route, 32).await?;
        let third = seed_identity(&context.app_pool, &route, 33).await?;
        let first_id = seed_deposit(
            &context.app_pool,
            &route,
            &first,
            31,
            DepositState::Cleared,
            U256::from(10_u64).pow(U256::from(18_u8)),
            100,
        )
        .await?;
        let second_id = seed_deposit(
            &context.app_pool,
            &route,
            &second,
            32,
            DepositState::Cleared,
            U256::from(10_u64).pow(U256::from(18_u8)),
            100,
        )
        .await?;
        let third_id = seed_deposit(
            &context.app_pool,
            &route,
            &third,
            33,
            DepositState::Cleared,
            U256::from(10_u64).pow(U256::from(18_u8)),
            100,
        )
        .await?;
        persist_sent(&context.app_pool, first_id).await?;
        persist_sent(&context.app_pool, second_id).await?;
        persist_sent(&context.app_pool, third_id).await?;
        let settlement = Arc::new(MockSettlement::default());
        let first_payload = settlement_payload(&context.app_pool, first_id).await?;
        settlement.answers.lock().unwrap().insert(
            format!("deposit:{first_id}"),
            Some(SettlementAnswer::Accepted {
                destination_tx_id: "credit-31".to_owned(),
                payload: first_payload,
            }),
        );
        settlement
            .answers
            .lock()
            .unwrap()
            .insert(format!("deposit:{second_id}"), None);
        let mut invalid_payload = settlement_payload(&context.app_pool, third_id).await?;
        invalid_payload["account_id"] = serde_json::json!("wrong-workspace");
        settlement.answers.lock().unwrap().insert(
            format!("deposit:{third_id}"),
            Some(SettlementAnswer::Accepted {
                destination_tx_id: "credit-33".to_owned(),
                payload: invalid_payload,
            }),
        );
        let chain = Arc::new(MockChain {
            finalized: 0,
            ..MockChain::default()
        });
        {
            let mut derived = chain.derived.lock().unwrap();
            derived.insert(first.salt, first.address);
            derived.insert(second.salt, second.address);
            derived.insert(third.salt, third.address);
        }
        {
            let amount = U256::from(10_u64).pow(U256::from(18_u8));
            let mut balances = chain.balances.lock().unwrap();
            balances.insert(first.address, amount);
            balances.insert(second.address, amount);
            balances.insert(third.address, amount);
        }
        let reconciler = reconciler(&context.app_pool, route, chain, settlement)?;
        let report = reconciler.post_restore_once().await?;
        ensure!(report.incomplete);
        ensure!(report.findings.iter().any(|finding| {
            finding.check == CheckName::SentSettlement && finding.repair_applied
        }));
        ensure!(report.findings.iter().any(|finding| {
            finding.check == CheckName::SentSettlement && !finding.repair_applied
        }));
        ensure!(report.findings.iter().any(|finding| {
            finding.subjects.get("deposit_id") == Some(&third_id.to_string())
                && finding.observed.get("error")
                    == Some(&serde_json::json!("settlement_product_payload_invalid"))
        }));
        ensure!(report.findings.iter().any(|finding| {
            finding.check == CheckName::PostRestoreSettlement && finding.incomplete
        }));
        ensure!(
            db::get_deposit(&context.app_pool, first_id)
                .await?
                .context("first deposit")?
                .state
                == DepositState::Credited
        );
        ensure!(
            db::get_deposit(&context.app_pool, third_id)
                .await?
                .context("third deposit")?
                .state
                == DepositState::Cleared
        );
        Ok(())
    }
    .await;
    context.cleanup().await?;
    result
}

#[tokio::test]
async fn mismatches_block_only_required_scopes_and_findings_are_idempotent() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let route = route()?;
        let seed = seed_identity(&context.app_pool, &route, 41).await?;
        let amount = U256::from(10_u64).pow(U256::from(18_u8));
        seed_deposit(
            &context.app_pool,
            &route,
            &seed,
            41,
            DepositState::Confirmed,
            amount,
            101,
        )
        .await?;
        let chain = Arc::new(MockChain {
            finalized: 0,
            ..MockChain::default()
        });
        chain
            .derived
            .lock()
            .unwrap()
            .insert(seed.salt, Address::from([99; 20]));
        *chain.flushed_total.lock().unwrap() = U256::from(7_u8);
        let metrics = Arc::new(ReconciliationMetrics::default());
        let reconciler = Reconciler::with_dependencies(
            context.app_pool.clone(),
            vec![route],
            BTreeMap::from([(31_337, Arc::clone(&chain) as Arc<dyn ReconciliationChain>)]),
            Arc::new(MockSettlement::default()),
            Arc::clone(&metrics),
        )?;
        let first = reconciler.run_once().await?;
        ensure!(first.findings.iter().any(|finding| {
            finding.check == CheckName::CreditRecomputation && !finding.repair_applied
        }));
        ensure!(first.findings.iter().any(|finding| {
            finding.check == CheckName::CustodyBalance && !finding.repair_applied
        }));
        ensure!(first.findings.iter().any(|finding| {
            finding.check == CheckName::CustodyBalance
                && finding.subjects.contains_key("treasury")
                && finding.expected.get("flushed_event_total").is_some()
                && finding.observed.get("treasury_inflow_total").is_some()
        }));
        ensure!(first.findings.iter().any(|finding| {
            finding.check == CheckName::AddressDerivation && !finding.repair_applied
        }));
        let count_after_first: i64 =
            sqlx::query_scalar("SELECT count(*) FROM reconciliation_findings")
                .fetch_one(&context.app_pool)
                .await?;
        let audit_after_first: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit WHERE action = 'reconciliation_mismatch'",
        )
        .fetch_one(&context.app_pool)
        .await?;
        let _ = reconciler.run_once().await?;
        let count_after_second: i64 =
            sqlx::query_scalar("SELECT count(*) FROM reconciliation_findings")
                .fetch_one(&context.app_pool)
                .await?;
        let audit_after_second: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit WHERE action = 'reconciliation_mismatch'",
        )
        .fetch_one(&context.app_pool)
        .await?;
        ensure!(count_after_first == count_after_second);
        ensure!(audit_after_first == audit_after_second);
        let scopes: Vec<String> =
            sqlx::query_scalar("SELECT scope FROM reconciliation_blocks ORDER BY scope")
                .fetch_all(&context.app_pool)
                .await?;
        ensure!(scopes == ["address", "chain"]);
        ensure!(metrics.mismatch_count(CheckName::CreditRecomputation) == 1);
        Ok(())
    }
    .await;
    context.cleanup().await?;
    result
}

#[tokio::test]
async fn loop_respects_cancellation() -> Result<()> {
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let route = route()?;
        seed_product(&context.app_pool).await?;
        let chain = Arc::new(MockChain {
            finalized_delay: StdDuration::from_secs(10),
            ..MockChain::default()
        });
        let metrics = Arc::new(ReconciliationMetrics::default());
        let reconciler = Arc::new(Reconciler::with_dependencies(
            context.app_pool.clone(),
            vec![route],
            BTreeMap::from([(31_337, Arc::clone(&chain) as Arc<dyn ReconciliationChain>)]),
            Arc::new(MockSettlement::default()),
            Arc::clone(&metrics),
        )?);
        let cancellation = CancellationToken::new();
        let task = tokio::spawn({
            let reconciler = Arc::clone(&reconciler);
            let cancellation = cancellation.clone();
            async move {
                reconciler
                    .run_loop(StdDuration::from_secs(60), cancellation)
                    .await;
            }
        });
        tokio::time::timeout(
            StdDuration::from_secs(1),
            chain.finalized_started.notified(),
        )
        .await?;
        cancellation.cancel();
        tokio::time::timeout(StdDuration::from_secs(1), task).await??;
        ensure!(metrics.last_heartbeat_unix() == 0);
        Ok(())
    }
    .await;
    context.cleanup().await?;
    result
}

fn route() -> Result<RouteFile> {
    let mut route: RouteFile =
        serde_saphyr::from_str(include_str!("fixtures/phala-cloud-pha.yaml"))?;
    route.chain.chain_id = 31_337;
    route.chain.rpc_providers = vec![
        "http://127.0.0.1:8546".to_owned(),
        "http://localhost:8546".to_owned(),
    ];
    route.asset.contract = Address::from([200; 20]);
    Ok(route)
}

fn reconciler(
    pool: &PgPool,
    route: RouteFile,
    chain: Arc<MockChain>,
    settlement: Arc<MockSettlement>,
) -> Result<Reconciler> {
    Ok(Reconciler::with_dependencies(
        pool.clone(),
        vec![route],
        BTreeMap::from([(31_337, chain as Arc<dyn ReconciliationChain>)]),
        settlement,
        Arc::new(ReconciliationMetrics::default()),
    )?)
}

async fn seed_product(pool: &PgPool) -> Result<Uuid> {
    let id = Uuid::new_v4();
    db::create_product(
        pool,
        &NewProduct {
            id,
            slug: format!("product-{id}"),
            settlement_url: "http://product.test/settlements".to_owned(),
            webhook_url: "http://product.test/webhooks".to_owned(),
            pubkey: "test".to_owned(),
            kid: "product/v1".to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    Ok(id)
}

async fn seed_identity(pool: &PgPool, route: &RouteFile, number: u8) -> Result<Seed> {
    let product_id = seed_product(pool).await?;
    let account_id = Uuid::new_v4();
    db::create_account(
        pool,
        &NewAccount {
            id: account_id,
            product_id,
            external_id: format!("workspace-{number}"),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    let address_id = Uuid::new_v4();
    let salt = B256::from([number; 32]);
    let address = Address::from([number; 20]);
    db::insert_address(
        pool,
        &NewAddress {
            id: address_id,
            account_id,
            chain_id: route.chain.chain_id,
            kind: AddressKind::Persistent,
            version: 1,
            lock_ref: None,
            salt,
            address,
            retired_at: None,
        },
    )
    .await?;
    Ok(Seed {
        account_id,
        address_id,
        salt,
        address,
    })
}

async fn seed_deposit(
    pool: &PgPool,
    route: &RouteFile,
    seed: &Seed,
    number: u8,
    state: DepositState,
    amount: U256,
    credit_minor: u64,
) -> Result<Uuid> {
    let tx_hash = B256::from([number; 32]);
    let deposit = NewDeposit {
        chain_id: route.chain.chain_id,
        tx_hash,
        log_index: u64::from(number),
        block_number: 100,
        block_hash: B256::from([number.wrapping_add(1); 32]),
        block_time: Utc::now(),
        address_id: seed.address_id,
        account_id: seed.account_id,
        route: Some(route.route.clone()),
        route_version: Some(route.version),
        asset_contract: route.asset.contract,
        from_address: Address::from([201; 20]),
        amount_atomic: AtomicAmount::new(amount),
        state,
        reason: None,
        next_attempt_at: Utc::now() - Duration::seconds(1),
    };
    let id = deposit_id(deposit.chain_id, deposit.tx_hash, deposit.log_index);
    ensure!(db::insert_deposit(pool, &deposit).await?);
    sqlx::query(
        r#"
        UPDATE deposits
        SET valuation_at = $2, price_scaled = 100000000,
            price_source = 'spot', credit_minor = $3::text::numeric
        WHERE id = $1
        "#,
    )
    .bind(id)
    .bind(Utc::now())
    .bind(credit_minor.to_string())
    .execute(pool)
    .await?;
    Ok(id)
}

async fn settlement_payload(pool: &PgPool, id: Uuid) -> Result<serde_json::Value> {
    let deposit = db::get_deposit(pool, id)
        .await?
        .context("deposit must exist")?;
    let account = db::get_account(pool, deposit.account_id)
        .await?
        .context("account must exist")?;
    let address = db::get_address(pool, deposit.address_id)
        .await?
        .context("address must exist")?;
    Ok(serde_json::json!({
        "version": 1,
        "idempotency_key": format!("deposit:{id}"),
        "account_id": account.external_id,
        "unit": "USD",
        "amount_minor": deposit.credit_minor.context("credit")?.value().to_string(),
        "source": "crypto_deposit",
        "evidence": {
            "chain_id": deposit.chain_id,
            "asset_contract": format!("{:#x}", deposit.asset_contract),
            "route": deposit.route.context("route")?,
            "route_version": deposit.route_version.context("route version")?,
            "tx_hash": format!("{:#x}", deposit.tx_hash),
            "log_index": deposit.log_index,
            "to": format!("{:#x}", address.address),
            "amount_atomic": deposit.amount_atomic.value().to_string(),
            "price_scaled": deposit.price_scaled.context("price")?.to_string(),
            "price_scale": 8,
            "valuation_at": deposit.valuation_at.context("valuation")?,
            "lock_ref": null
        }
    }))
}

async fn persist_sent(pool: &PgPool, id: Uuid) -> Result<()> {
    let deposit = db::get_deposit(pool, id)
        .await?
        .context("deposit must exist")?;
    let account = db::get_account(pool, deposit.account_id)
        .await?
        .context("account must exist")?;
    db::upsert_intent(
        pool,
        &SettlementIntent {
            deposit_id: id,
            product_id: account.product_id,
            key: format!("deposit:{id}"),
            payload: settlement_payload(pool, id).await?,
        },
    )
    .await?;
    db::mark_sent(pool, id).await?;
    Ok(())
}

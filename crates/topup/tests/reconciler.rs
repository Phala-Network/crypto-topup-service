//! PostgreSQL-backed reconciliation checks, repairs, freezes, and cancellation behavior.

mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use topup::db::{
    self, AddressKind, Deposit, FlushedEvent, NewAccount, NewAddress, NewDeposit, NewFlush,
    NewProduct, SettlementIntent,
};
use topup::pump::{Pump, PumpConfig, RunOnceResult, Step, StepResult, StepSet};
use topup::reconciler::{
    CheckName, Reconciler, ReconciliationChain, ReconciliationError, ReconciliationReport,
    SettlementLookup, frozen_chains, hold_lease_owner_lock,
};
use topup_adapters::chain::evm::{ChainError, ChainReader, TransferLog};
use topup_adapters::settlement::http::SettlementAnswer;
use topup_core::deposit::{DepositState, RejectReason, StepOutcome};
use topup_core::identity::deposit_id;
use topup_core::money::AtomicAmount;
use topup_core::route::RouteFile;
use uuid::Uuid;

use support::TestDatabase;

const CHAIN_ID: u64 = 31_337;

#[derive(Default)]
struct MockChain {
    finalized: AtomicU64,
    finalized_delay: StdDuration,
    finalized_started: Notify,
    fail_derivation: AtomicBool,
    logs: Mutex<Vec<TransferLog>>,
    log_requests: Mutex<Vec<(u64, u64)>>,
    balances: Mutex<BTreeMap<Address, U256>>,
    balance_blocks: Mutex<Vec<u64>>,
    flushed_events: Mutex<Vec<(u64, U256)>>,
    derived: Mutex<BTreeMap<B256, Address>>,
}

impl MockChain {
    fn at(finalized: u64) -> Self {
        Self {
            finalized: AtomicU64::new(finalized),
            ..Self::default()
        }
    }

    fn derive(&self, seeds: &[&Seed]) {
        let mut derived = self.derived.lock().unwrap();
        for seed in seeds {
            derived.insert(seed.salt, seed.address);
        }
    }
}

#[async_trait]
impl ReconciliationChain for MockChain {
    async fn finalized_head(&self) -> Result<u64, ReconciliationError> {
        self.finalized_started.notify_one();
        tokio::time::sleep(self.finalized_delay).await;
        Ok(self.finalized.load(Ordering::SeqCst))
    }

    async fn transfer_logs_to(
        &self,
        addresses: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<TransferLog>, ReconciliationError> {
        self.log_requests
            .lock()
            .unwrap()
            .push((from_block, to_block));
        Ok(self
            .logs
            .lock()
            .unwrap()
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
        block: u64,
    ) -> Result<Vec<U256>, ReconciliationError> {
        self.balance_blocks.lock().unwrap().push(block);
        let balances = self.balances.lock().unwrap();
        Ok(addresses
            .iter()
            .map(|address| balances.get(address).copied().unwrap_or(U256::ZERO))
            .collect())
    }

    async fn flushed_total(
        &self,
        _factory: Address,
        _token: Address,
        from_block: u64,
        to_block: u64,
    ) -> Result<U256, ReconciliationError> {
        Ok(self
            .flushed_events
            .lock()
            .unwrap()
            .iter()
            .filter(|(block, _)| *block >= from_block && *block <= to_block)
            .fold(U256::ZERO, |total, (_, amount)| total + *amount))
    }

    async fn factory_addresses(
        &self,
        _factory: Address,
        salts: &[B256],
    ) -> Result<Vec<Address>, ReconciliationError> {
        if self.fail_derivation.load(Ordering::SeqCst) {
            return Err(ReconciliationError::Chain("addressOf timed out".to_owned()));
        }
        let derived = self.derived.lock().unwrap();
        Ok(salts
            .iter()
            .map(|salt| derived.get(salt).copied().unwrap_or(Address::ZERO))
            .collect())
    }
}

#[derive(Default)]
struct MockSettlement {
    answers: Mutex<BTreeMap<String, Option<SettlementAnswer>>>,
    failing: Mutex<BTreeSet<String>>,
}

impl MockSettlement {
    fn answer(&self, deposit_id: Uuid, answer: Option<SettlementAnswer>) {
        self.answers
            .lock()
            .unwrap()
            .insert(format!("deposit:{deposit_id}"), answer);
    }

    fn fail(&self, deposit_id: Uuid) {
        self.failing
            .lock()
            .unwrap()
            .insert(format!("deposit:{deposit_id}"));
    }
}

#[async_trait]
impl SettlementLookup for MockSettlement {
    async fn get_by_key(
        &self,
        _settlement_url: &str,
        key: &str,
    ) -> Result<Option<SettlementAnswer>, ReconciliationError> {
        if self.failing.lock().unwrap().contains(key) {
            return Err(ReconciliationError::Settlement("GET timed out".to_owned()));
        }
        Ok(self.answers.lock().unwrap().get(key).cloned().flatten())
    }
}

struct Seed {
    account_id: Uuid,
    address_id: Uuid,
    salt: B256,
    address: Address,
}

#[tokio::test]
async fn repairs_missing_deposits_incrementally_and_links_flushes_with_audit() -> Result<()> {
    with_database(|pool| async move {
        let route = route()?;
        let seed = seed_identity(&pool, &route, 1).await?;
        let chain = Arc::new(MockChain::at(5));
        chain.derive(&[&seed]);
        chain.logs.lock().unwrap().push(transfer(
            11,
            2,
            3,
            route.asset.contract,
            Address::from([13; 20]),
            seed.address,
            500,
        ));
        let reconciler = reconciler(&pool, route.clone(), chain.clone(), Arc::default())?;
        scanned_through(&pool, 5).await?;

        let missing = reconciler.check(CheckName::MissingDeposit).await?;
        ensure!(missing.len() == 1 && missing[0].repair_applied);
        let requests = chain.log_requests.lock().unwrap().clone();
        ensure!(requests == [(0, 5)]);
        ensure!(reconciler.check(CheckName::MissingDeposit).await?.is_empty());
        ensure!(chain.log_requests.lock().unwrap().len() == 1);
        chain.finalized.store(9, Ordering::SeqCst);
        scanned_through(&pool, 9).await?;
        ensure!(reconciler.check(CheckName::MissingDeposit).await?.is_empty());
        ensure!(chain.log_requests.lock().unwrap().last() == Some(&(6, 9)));
        let repaired = deposit(&pool, deposit_id(CHAIN_ID, B256::from([11; 32]), 2)).await?;
        ensure!(repaired.state == DepositState::Detected);

        let linked_id = seed_deposit(
            &pool,
            &route,
            &seed,
            DepositSeed::new(21, DepositState::Credited).block(4),
        )
        .await?;
        let flush_id = seed_confirmed_flush(&pool, &route, &seed, 101, 1, 1_500).await?;
        let report = reconciler.run_once().await?;
        ensure!(report.succeeded());
        ensure!(report.findings.iter().any(|finding| {
            finding.check == CheckName::MissingFlushLink && finding.repair_applied
        }));
        let linked = deposit(&pool, linked_id).await?;
        ensure!(linked.flush_id == Some(flush_id) && linked.state == DepositState::Swept);
        ensure!(reconciler.check(CheckName::MissingFlushLink).await?.is_empty());
        let repairs: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit WHERE actor = 'reconciler' AND action = 'reconciliation_repair'",
        )
        .fetch_one(&pool)
        .await?;
        ensure!(repairs >= 1);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn missing_deposit_scan_never_passes_the_scanner() -> Result<()> {
    with_database(|pool| async move {
        let route = route()?;
        let seed = seed_identity(&pool, &route, 2).await?;
        let chain = Arc::new(MockChain::at(9));
        chain.derive(&[&seed]);
        chain.logs.lock().unwrap().push(transfer(
            12,
            0,
            7,
            route.asset.contract,
            Address::from([13; 20]),
            seed.address,
            500,
        ));
        let reconciler = reconciler(&pool, route, chain.clone(), Arc::default())?;

        // Nothing is compared before the scanner commits a range.
        ensure!(
            reconciler
                .check(CheckName::MissingDeposit)
                .await?
                .is_empty()
        );
        ensure!(chain.log_requests.lock().unwrap().is_empty());

        // The scanner cursor does not cover an address still awaiting its backfill.
        db::commit_scan(&pool, CHAIN_ID, &[], &[], Some(6)).await?;
        ensure!(
            reconciler
                .check(CheckName::MissingDeposit)
                .await?
                .is_empty()
        );
        ensure!(chain.log_requests.lock().unwrap().is_empty());

        // A transfer the scanner has not reached yet is not reported as missing.
        db::commit_scan(&pool, CHAIN_ID, &[], &[seed.address_id], None).await?;
        ensure!(
            reconciler
                .check(CheckName::MissingDeposit)
                .await?
                .is_empty()
        );
        ensure!(chain.log_requests.lock().unwrap().as_slice() == [(0, 6)]);

        db::commit_scan(&pool, CHAIN_ID, &[], &[], Some(9)).await?;
        let missing = reconciler.check(CheckName::MissingDeposit).await?;
        ensure!(missing.len() == 1 && missing[0].repair_applied);
        ensure!(chain.log_requests.lock().unwrap().last() == Some(&(7, 9)));
        Ok(())
    })
    .await
}

#[tokio::test]
async fn flush_linkage_keeps_a_state_advanced_after_the_scan() -> Result<()> {
    with_database(|pool| async move {
        let route = route()?;
        let seed = seed_identity(&pool, &route, 2).await?;
        let deposit_id = seed_deposit(
            &pool,
            &route,
            &seed,
            DepositSeed::new(22, DepositState::Cleared),
        )
        .await?;
        let flush_id = seed_confirmed_flush(&pool, &route, &seed, 101, 1, 1_000).await?;
        let reconciler = Arc::new(reconciler(
            &pool,
            route,
            Arc::new(MockChain::at(0)),
            Arc::default(),
        )?);

        // A settle step commits `cleared → credited` while linkage is waiting on the row.
        let mut advance = pool.begin().await?;
        sqlx::query("UPDATE deposits SET state = 'credited' WHERE id = $1")
            .bind(deposit_id)
            .execute(&mut *advance)
            .await?;
        let linking = tokio::spawn({
            let reconciler = Arc::clone(&reconciler);
            async move { reconciler.check(CheckName::MissingFlushLink).await }
        });
        tokio::time::sleep(StdDuration::from_millis(300)).await;
        advance.commit().await?;
        let findings = tokio::time::timeout(StdDuration::from_secs(5), linking).await???;
        ensure!(findings.len() == 1 && findings[0].repair_applied);

        let linked = deposit(&pool, deposit_id).await?;
        ensure!(linked.state == DepositState::Swept && linked.flush_id == Some(flush_id));
        let swept: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM transitions WHERE deposit_id = $1 AND from_state = 'credited' AND to_state = 'swept'",
        )
        .bind(deposit_id)
        .fetch_one(&pool)
        .await?;
        ensure!(swept == 1);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn sent_settlements_are_adopted_under_a_lease_and_waits_are_quiet() -> Result<()> {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    // The current-thread test runtime polls every reconciler future on this thread.
    let _recorder = metrics::set_default_local_recorder(&recorder);
    with_database(|pool| async move {
        let route = route()?;
        let accepted = seed_sent(&pool, &route, 31).await?;
        let processing = seed_sent(&pool, &route, 32).await?;
        let conflict = seed_sent(&pool, &route, 33).await?;
        let leased = seed_sent(&pool, &route, 34).await?;
        let foreign = seed_sent(&pool, &route, 35).await?;
        let settlement = Arc::new(MockSettlement::default());
        settlement.answer(accepted, Some(accepted_answer(&pool, accepted).await?));
        settlement.answer(
            processing,
            Some(SettlementAnswer::Processing {
                payload: settlement_payload(&pool, processing).await?,
            }),
        );
        settlement.answer(conflict, Some(SettlementAnswer::Conflict409));
        settlement.answer(leased, Some(accepted_answer(&pool, leased).await?));
        let mut wrong_account = settlement_payload(&pool, foreign).await?;
        wrong_account["account_id"] = json!("another-workspace");
        settlement.answer(
            foreign,
            Some(SettlementAnswer::Accepted {
                destination_tx_id: "credit-35".to_owned(),
                payload: wrong_account,
            }),
        );
        let pump_lease = Uuid::new_v4();
        sqlx::query(
            "UPDATE deposits SET lease_token = $2, lease_until = now() + interval '5 minutes' WHERE id = $1",
        )
        .bind(leased)
        .bind(pump_lease)
        .execute(&pool)
        .await?;
        let reconciler = Reconciler::with_dependencies(
            pool.clone(),
            vec![route],
            BTreeMap::from([(
                CHAIN_ID,
                Arc::new(MockChain::at(0)) as Arc<dyn ReconciliationChain>,
            )]),
            settlement,
        )?;

        let findings = reconciler.check(CheckName::SentSettlement).await?;
        ensure!(findings.len() == 2, "unexpected findings: {findings:?}");
        ensure!(findings.iter().any(|finding| {
            finding.subjects["deposit_id"] == accepted.to_string() && finding.repair_applied
        }));
        ensure!(findings.iter().any(|finding| {
            finding.subjects["deposit_id"] == foreign.to_string()
                && !finding.repair_applied
                && finding.observed["error"] == json!("settlement_product_payload_invalid")
        }));

        let credited = deposit(&pool, accepted).await?;
        ensure!(credited.state == DepositState::Credited && credited.lease_token.is_none());
        let transitions: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM transitions WHERE deposit_id = $1 AND from_state = 'cleared' AND to_state = 'credited'",
        )
        .bind(accepted)
        .fetch_one(&pool)
        .await?;
        ensure!(transitions == 1);
        let credited_events: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM outbox WHERE event_type = 'deposit.credited' AND payload->>'deposit_id' = $1",
        )
        .bind(accepted.to_string())
        .fetch_one(&pool)
        .await?;
        ensure!(credited_events == 1);

        for waiting in [processing, conflict] {
            let row = deposit(&pool, waiting).await?;
            ensure!(row.state == DepositState::Cleared && row.lease_token.is_none());
        }
        let untouched = deposit(&pool, leased).await?;
        ensure!(untouched.state == DepositState::Cleared);
        ensure!(untouched.lease_token == Some(pump_lease));
        let leased_status: String =
            sqlx::query_scalar("SELECT status FROM settlements WHERE deposit_id = $1")
                .bind(leased)
                .fetch_one(&pool)
                .await?;
        ensure!(leased_status == "sent");

        let report = reconciler.run_once().await?;
        ensure!(
            !report
                .findings
                .iter()
                .any(|finding| finding.check == CheckName::SentSettlement
                    && [processing, conflict, leased]
                        .iter()
                        .any(|id| finding.subjects["deposit_id"] == id.to_string()))
        );
        ensure!(handle.render().contains(
            "topup_reconciliation_mismatches_total{check=\"sent_settlement\",producer_enabled=\"true\"} 1"
        ));
        Ok(())
    })
    .await
}

#[tokio::test]
async fn post_restore_completes_when_product_truth_is_adopted() -> Result<()> {
    with_database(|pool| async move {
        let route = route()?;
        let unsent = seed_identity(&pool, &route, 41).await?;
        let refused = seed_identity(&pool, &route, 42).await?;
        let swept = seed_identity(&pool, &route, 43).await?;
        let mispriced = seed_identity(&pool, &route, 44).await?;
        let unsent_id = seed_deposit(
            &pool,
            &route,
            &unsent,
            DepositSeed::new(41, DepositState::Cleared),
        )
        .await?;
        let refused_id = seed_deposit(
            &pool,
            &route,
            &refused,
            DepositSeed::new(42, DepositState::Credited),
        )
        .await?;
        let swept_id = seed_deposit(
            &pool,
            &route,
            &swept,
            DepositSeed::new(43, DepositState::Swept),
        )
        .await?;
        seed_deposit(
            &pool,
            &route,
            &mispriced,
            DepositSeed::new(44, DepositState::Confirmed).credit(101),
        )
        .await?;
        let settlement = Arc::new(MockSettlement::default());
        settlement.answer(unsent_id, None);
        settlement.answer(
            refused_id,
            Some(SettlementAnswer::Rejected {
                reason: "account_closed".to_owned(),
                payload: settlement_payload(&pool, refused_id).await?,
            }),
        );
        settlement.answer(swept_id, Some(accepted_answer(&pool, swept_id).await?));
        let chain = Arc::new(MockChain::at(150));
        chain.derive(&[&unsent, &refused, &swept, &mispriced]);
        let reconciler = reconciler(&pool, route, chain, settlement)?;

        let report = reconciler.post_restore_once().await?;
        ensure!(!report.incomplete, "restore must complete: {report:?}");
        ensure!(report.succeeded());
        // Alert-only findings are present but never gate the restore.
        ensure!(has_check(&report, CheckName::CreditRecomputation));
        ensure!(has_check(&report, CheckName::CustodyBalance));
        ensure!(!report.findings.iter().any(|finding| {
            finding.subjects.get("deposit_id") == Some(&unsent_id.to_string())
                && finding.check == CheckName::PostRestoreSettlement
        }));
        ensure!(deposit(&pool, unsent_id).await?.state == DepositState::Cleared);

        let rejected = deposit(&pool, refused_id).await?;
        ensure!(rejected.state == DepositState::Rejected);
        ensure!(rejected.reason == Some(RejectReason::ProductRefused));
        let product_wins: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM transitions WHERE deposit_id = $1 AND from_state = 'credited' AND to_state = 'rejected' AND evidence->>'source' = 'post_restore_product_answer'",
        )
        .bind(refused_id)
        .fetch_one(&pool)
        .await?;
        ensure!(product_wins == 1);

        ensure!(deposit(&pool, swept_id).await?.state == DepositState::Swept);
        let swept_settlement: String =
            sqlx::query_scalar("SELECT status FROM settlements WHERE deposit_id = $1")
                .bind(swept_id)
                .fetch_one(&pool)
                .await?;
        ensure!(swept_settlement == "accepted");
        Ok(())
    })
    .await
}

#[tokio::test]
async fn post_restore_refuses_to_preempt_a_running_lease_owner() -> Result<()> {
    with_database(|pool| async move {
        let route = route()?;
        let seed = seed_identity(&pool, &route, 45).await?;
        let deposit_id = seed_deposit(
            &pool,
            &route,
            &seed,
            DepositSeed::new(45, DepositState::Credited),
        )
        .await?;
        let lease_token = Uuid::new_v4();
        sqlx::query(
            "UPDATE deposits SET lease_token = $2, lease_until = now() + interval '5 minutes' WHERE id = $1",
        )
        .bind(deposit_id)
        .bind(lease_token)
        .execute(&pool)
        .await?;
        let settlement = Arc::new(MockSettlement::default());
        settlement.answer(deposit_id, Some(accepted_answer(&pool, deposit_id).await?));
        let chain = Arc::new(MockChain::at(150));
        chain.derive(&[&seed]);
        let reconciler = reconciler(&pool, route, chain, settlement)?;

        let service = hold_lease_owner_lock(&pool).await?;
        let second_service = hold_lease_owner_lock(&pool).await?;
        let refused = reconciler.post_restore_once().await;
        ensure!(matches!(
            refused,
            Err(ReconciliationError::LeaseOwnerLock(_))
        ));
        ensure!(deposit(&pool, deposit_id).await?.lease_token == Some(lease_token));
        second_service.release().await?;
        service.release().await?;

        let report = reconciler.post_restore_once().await?;
        ensure!(!report.incomplete, "restore must complete: {report:?}");
        ensure!(deposit(&pool, deposit_id).await?.lease_token.is_none());
        hold_lease_owner_lock(&pool).await?.release().await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn losing_the_lease_owner_connection_stops_guarded_pumps() -> Result<()> {
    with_database(|pool| async move {
        let shutdown = CancellationToken::new();
        let lock = hold_lease_owner_lock(&pool).await?;
        let watch = tokio::spawn(lock.watch(StdDuration::from_millis(50), shutdown.clone()));
        let calls = Arc::new(AtomicUsize::new(0));
        let step = || Box::new(AdvanceStep(Arc::clone(&calls))) as Box<dyn Step>;
        let pump = Pump::new(
            pool.clone(),
            Arc::new(StepSet::new(step(), step(), step(), step())),
            PumpConfig::default(),
        )?;
        let pump_shutdown = shutdown.child_token();
        let pump_task = tokio::spawn(async move { pump.run(pump_shutdown).await });

        let terminated: bool = sqlx::query_scalar(
            r#"
            SELECT pg_terminate_backend(pid) FROM pg_locks
            WHERE locktype = 'advisory' AND mode = 'ShareLock'
              AND database = (SELECT oid FROM pg_database WHERE datname = current_database())
            "#,
        )
        .fetch_one(&pool)
        .await?;
        ensure!(terminated);
        let watched = tokio::time::timeout(StdDuration::from_secs(10), watch).await??;
        ensure!(watched.is_err());
        ensure!(shutdown.is_cancelled());
        tokio::time::timeout(StdDuration::from_secs(10), pump_task).await??;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn post_restore_stays_incomplete_without_verified_product_truth() -> Result<()> {
    with_database(|pool| async move {
        let route = route()?;
        let unknown = seed_identity(&pool, &route, 51).await?;
        let unreachable = seed_identity(&pool, &route, 52).await?;
        let foreign = seed_identity(&pool, &route, 53).await?;
        let unknown_id = seed_deposit(
            &pool,
            &route,
            &unknown,
            DepositSeed::new(51, DepositState::Swept),
        )
        .await?;
        let unreachable_id = seed_deposit(
            &pool,
            &route,
            &unreachable,
            DepositSeed::new(52, DepositState::Credited),
        )
        .await?;
        let foreign_id = seed_deposit(
            &pool,
            &route,
            &foreign,
            DepositSeed::new(53, DepositState::Cleared),
        )
        .await?;
        let settlement = Arc::new(MockSettlement::default());
        settlement.answer(unknown_id, None);
        settlement.fail(unreachable_id);
        let mut wrong_account = settlement_payload(&pool, foreign_id).await?;
        wrong_account["account_id"] = json!("another-workspace");
        settlement.answer(
            foreign_id,
            Some(SettlementAnswer::Accepted {
                destination_tx_id: "credit-53".to_owned(),
                payload: wrong_account,
            }),
        );
        let chain = Arc::new(MockChain::at(0));
        chain.derive(&[&unknown, &unreachable, &foreign]);
        let reconciler = reconciler(&pool, route, chain, settlement)?;

        let report = reconciler.post_restore_once().await?;
        ensure!(report.incomplete && report.succeeded());
        for id in [unknown_id, unreachable_id, foreign_id] {
            ensure!(
                report.findings.iter().any(|finding| {
                    finding.check == CheckName::PostRestoreSettlement
                        && finding.incomplete
                        && finding.subjects.get("deposit_id") == Some(&id.to_string())
                }),
                "deposit {id} must gate the restore"
            );
        }
        ensure!(deposit(&pool, unknown_id).await?.state == DepositState::Swept);
        ensure!(deposit(&pool, foreign_id).await?.state == DepositState::Cleared);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn checks_are_independent_and_heartbeat_requires_a_successful_round() -> Result<()> {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    // The current-thread test runtime polls every reconciler future on this thread.
    let _recorder = metrics::set_default_local_recorder(&recorder);
    with_database(|pool| async move {
        let route = route()?;
        let seed = seed_identity(&pool, &route, 61).await?;
        let mispriced_id = seed_deposit(
            &pool,
            &route,
            &seed,
            DepositSeed::new(61, DepositState::Confirmed).credit(101),
        )
        .await?;
        let historical_id = seed_deposit(
            &pool,
            &route,
            &seed,
            DepositSeed::new(62, DepositState::Confirmed),
        )
        .await?;
        sqlx::query("UPDATE deposits SET route_version = 99 WHERE id = $1")
            .bind(historical_id)
            .execute(&pool)
            .await?;
        let chain = Arc::new(MockChain::at(150));
        chain.derive(&[&seed]);
        chain.fail_derivation.store(true, Ordering::SeqCst);
        let reconciler = Reconciler::with_dependencies(
            pool.clone(),
            vec![route],
            BTreeMap::from([(CHAIN_ID, Arc::clone(&chain) as Arc<dyn ReconciliationChain>)]),
            Arc::new(MockSettlement::default()),
        )?;

        let failed = reconciler.run_once().await?;
        ensure!(failed.failed_checks == [CheckName::AddressDerivation]);
        ensure!(failed.findings.iter().any(|finding| {
            finding.subjects.get("deposit_id") == Some(&mispriced_id.to_string())
                && finding.expected["credit_minor"] == json!("100")
        }));
        ensure!(failed.findings.iter().any(|finding| {
            finding.subjects.get("deposit_id") == Some(&historical_id.to_string())
                && finding.observed["error"] == json!("route_version_unavailable")
        }));
        ensure!(has_check(&failed, CheckName::CustodyBalance));
        ensure!(!handle.render().contains(RECONCILER_PROGRESS));

        chain.fail_derivation.store(false, Ordering::SeqCst);
        let recovered = reconciler.run_once().await?;
        ensure!(recovered.succeeded());
        ensure!(handle.render().contains(RECONCILER_PROGRESS));
        Ok(())
    })
    .await
}

#[tokio::test]
async fn custody_balances_use_the_finalized_block_and_incremental_totals() -> Result<()> {
    with_database(|pool| async move {
        let route = route()?;
        let custody = seed_identity(&pool, &route, 71).await?;
        let flushing = seed_identity(&pool, &route, 72).await?;
        seed_deposit(
            &pool,
            &route,
            &custody,
            DepositSeed::new(71, DepositState::Swept)
                .block(100)
                .amount(1_000),
        )
        .await?;
        // Still inside the finality window at block 150.
        seed_deposit(
            &pool,
            &route,
            &custody,
            DepositSeed::new(72, DepositState::Detected)
                .block(200)
                .amount(500),
        )
        .await?;
        seed_confirmed_flush(&pool, &route, &custody, 120, 3, 400).await?;
        seed_deposit(
            &pool,
            &route,
            &flushing,
            DepositSeed::new(73, DepositState::Credited).amount(700),
        )
        .await?;
        db::insert_flush(
            &pool,
            &NewFlush {
                id: Uuid::new_v4(),
                chain_id: CHAIN_ID,
                token: route.asset.contract,
                operator: Address::from([22; 20]),
                nonce: 9,
                tx_hash: Some(B256::from([24; 32])),
                block_number: None,
                status: "sent".to_owned(),
                receipt: Some(json!({"plan": [{"address_id": flushing.address_id}]})),
            },
        )
        .await?;

        let treasury = route.chain.contracts.treasury;
        let chain = Arc::new(MockChain::at(150));
        chain.derive(&[&custody, &flushing]);
        chain
            .balances
            .lock()
            .unwrap()
            .insert(custody.address, U256::from(600_u64));
        chain
            .flushed_events
            .lock()
            .unwrap()
            .push((120, U256::from(400_u64)));
        {
            let mut logs = chain.logs.lock().unwrap();
            logs.push(transfer(
                81,
                3,
                120,
                route.asset.contract,
                custody.address,
                treasury,
                400,
            ));
            // Finance top-ups to the treasury are not flush inflows.
            logs.push(transfer(
                82,
                0,
                130,
                route.asset.contract,
                Address::from([90; 20]),
                treasury,
                999,
            ));
        }
        let reconciler = reconciler(&pool, route.clone(), chain.clone(), Arc::default())?;

        ensure!(
            reconciler
                .check(CheckName::CustodyBalance)
                .await?
                .is_empty()
        );
        ensure!(chain.balance_blocks.lock().unwrap().as_slice() == [150]);
        ensure!(
            reconciler
                .check(CheckName::CustodyBalance)
                .await?
                .is_empty()
        );

        chain.finalized.store(300, Ordering::SeqCst);
        chain
            .balances
            .lock()
            .unwrap()
            .insert(custody.address, U256::from(1_100_u64));
        chain
            .flushed_events
            .lock()
            .unwrap()
            .push((250, U256::from(50_u64)));
        let requests_before = chain.log_requests.lock().unwrap().len();
        let findings = reconciler.check(CheckName::CustodyBalance).await?;
        ensure!(findings.len() == 1, "unexpected findings: {findings:?}");
        ensure!(findings[0].expected["flushed_event_total"] == json!("450"));
        ensure!(findings[0].observed["treasury_inflow_total"] == json!("400"));
        let requests = chain.log_requests.lock().unwrap().clone();
        ensure!(requests[requests_before..] == [(151, 300)]);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn mismatches_block_only_required_scopes_and_findings_are_idempotent() -> Result<()> {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    // The current-thread test runtime polls every reconciler future on this thread.
    let _recorder = metrics::set_default_local_recorder(&recorder);
    with_database(|pool| async move {
        let route = route()?;
        let seed = seed_identity(&pool, &route, 91).await?;
        seed_deposit(
            &pool,
            &route,
            &seed,
            DepositSeed::new(91, DepositState::Confirmed).credit(101),
        )
        .await?;
        let chain = Arc::new(MockChain::at(0));
        chain
            .derived
            .lock()
            .unwrap()
            .insert(seed.salt, Address::from([99; 20]));
        chain
            .flushed_events
            .lock()
            .unwrap()
            .push((0, U256::from(7_u8)));
        let reconciler = Reconciler::with_dependencies(
            pool.clone(),
            vec![route],
            BTreeMap::from([(CHAIN_ID, Arc::clone(&chain) as Arc<dyn ReconciliationChain>)]),
            Arc::new(MockSettlement::default()),
        )?;
        let first = reconciler.run_once().await?;
        ensure!(has_check(&first, CheckName::CreditRecomputation));
        ensure!(first.findings.iter().any(|finding| {
            finding.check == CheckName::CustodyBalance
                && finding.subjects.contains_key("treasury")
                && finding.expected["flushed_event_total"] == json!("7")
        }));
        ensure!(has_check(&first, CheckName::AddressDerivation));
        ensure!(!first.incomplete);
        let count_after_first =
            count(&pool, "SELECT count(*) FROM reconciliation_findings").await?;
        let audit_after_first = count(
            &pool,
            "SELECT count(*) FROM audit WHERE action = 'reconciliation_mismatch'",
        )
        .await?;
        let _ = reconciler.run_once().await?;
        ensure!(
            count(&pool, "SELECT count(*) FROM reconciliation_findings").await?
                == count_after_first
        );
        ensure!(
            count(
                &pool,
                "SELECT count(*) FROM audit WHERE action = 'reconciliation_mismatch'",
            )
            .await?
                == audit_after_first
        );
        let scopes: Vec<String> =
            sqlx::query_scalar("SELECT scope FROM reconciliation_blocks ORDER BY scope")
                .fetch_all(&pool)
                .await?;
        ensure!(scopes == ["address", "chain"]);
        ensure!(handle.render().contains(
            "topup_reconciliation_mismatches_total{check=\"credit_recomputation\",producer_enabled=\"true\"} 1"
        ));
        Ok(())
    })
    .await
}

struct AdvanceStep(Arc<AtomicUsize>);

#[async_trait]
impl Step for AdvanceStep {
    async fn run(&self, _deposit: &Deposit) -> StepResult {
        self.0.fetch_add(1, Ordering::SeqCst);
        StepResult::new(StepOutcome::Advance, json!({"outcome": "advance"}))
    }
}

struct UnreachableReader;

impl ChainReader for UnreachableReader {
    async fn finalized_head(&self) -> Result<u64, ChainError> {
        Err(ChainError::ProviderUnhealthy)
    }

    async fn transfer_logs_to(
        &self,
        _addresses: &[Address],
        _from_block: u64,
        _to_block: u64,
    ) -> Result<Vec<TransferLog>, ChainError> {
        Err(ChainError::ProviderUnhealthy)
    }

    async fn transfer_log_by_identity(
        &self,
        _tx_hash: B256,
        _log_index: u64,
    ) -> Result<Option<TransferLog>, ChainError> {
        Err(ChainError::ProviderUnhealthy)
    }
}

#[tokio::test]
async fn frozen_chain_gates_startup_pumps_and_scanner() -> Result<()> {
    with_database(|pool| async move {
        let route = route()?;
        let seed = seed_identity(&pool, &route, 101).await?;
        let deposit_id = seed_deposit(
            &pool,
            &route,
            &seed,
            DepositSeed::new(101, DepositState::Detected),
        )
        .await?;
        ensure!(frozen_chains(&pool, std::slice::from_ref(&route)).await?.is_empty());
        sqlx::query(
            r#"
            INSERT INTO reconciliation_blocks (block_key, scope, chain_id, check_name, reason)
            VALUES ('chain:31337', 'chain', 31337, 'address_derivation', 'test freeze')
            "#,
        )
        .execute(&pool)
        .await?;
        ensure!(
            frozen_chains(&pool, std::slice::from_ref(&route)).await?
                == BTreeSet::from([CHAIN_ID])
        );

        let calls = Arc::new(AtomicUsize::new(0));
        let step = || Box::new(AdvanceStep(Arc::clone(&calls))) as Box<dyn Step>;
        let pump = Pump::new(
            pool.clone(),
            Arc::new(StepSet::new(step(), step(), step(), step())),
            PumpConfig::default(),
        )?;
        ensure!(pump.run_once().await? == RunOnceResult::Applied { deposit_id });
        ensure!(calls.load(Ordering::SeqCst) == 0);
        let waiting = deposit(&pool, deposit_id).await?;
        ensure!(waiting.state == DepositState::Detected);
        ensure!(waiting.next_attempt_at > Utc::now());
        let reason: Option<String> = sqlx::query_scalar(
            "SELECT evidence->>'reason' FROM transitions WHERE deposit_id = $1 ORDER BY created_at DESC LIMIT 1",
        )
        .bind(deposit_id)
        .fetch_one(&pool)
        .await?;
        ensure!(reason.as_deref() == Some("chain_frozen"));

        let scanner_routes = topup::scanner::configure_routes(std::slice::from_ref(&route))?;
        let stats =
            topup::scanner::scan_once(&pool, &UnreachableReader, &scanner_routes[0]).await?;
        ensure!(stats.inserted == 0);
        Ok(())
    })
    .await
}

#[tokio::test]
async fn application_role_cannot_rewrite_findings_or_delete_blocks() -> Result<()> {
    with_database(|pool| async move {
        let checks = [
            ("reconciliation_findings", "SELECT", true),
            ("reconciliation_findings", "INSERT", true),
            ("reconciliation_findings", "UPDATE", false),
            ("reconciliation_findings", "DELETE", false),
            ("reconciliation_findings", "TRUNCATE", false),
            ("reconciliation_blocks", "INSERT", true),
            ("reconciliation_blocks", "UPDATE", false),
            ("reconciliation_blocks", "DELETE", false),
            ("reconciliation_blocks", "TRUNCATE", false),
            ("reconciliation_deposit_cursors", "UPDATE", true),
            ("reconciliation_deposit_cursors", "DELETE", false),
            ("reconciliation_custody_cursors", "UPDATE", true),
            ("reconciliation_custody_cursors", "DELETE", false),
        ];
        for (table, privilege, expected) in checks {
            let granted: bool =
                sqlx::query_scalar("SELECT has_table_privilege('topup_app', $1, $2)")
                    .bind(table)
                    .bind(privilege)
                    .fetch_one(&pool)
                    .await?;
            ensure!(
                granted == expected,
                "topup_app {privilege} on {table} should be {expected}"
            );
        }
        sqlx::query(
            r#"
            INSERT INTO reconciliation_blocks (block_key, scope, chain_id, check_name, reason)
            VALUES ('chain:31337', 'chain', 31337, 'address_derivation', 'test freeze')
            "#,
        )
        .execute(&pool)
        .await?;
        for statement in [
            "DELETE FROM reconciliation_blocks",
            "UPDATE reconciliation_blocks SET chain_id = 1",
        ] {
            let denied = sqlx::query(statement)
                .execute(&pool)
                .await
                .err()
                .and_then(|error| {
                    error
                        .as_database_error()
                        .and_then(|error| error.code())
                        .map(|code| code.into_owned())
                });
            ensure!(
                denied.as_deref() == Some("42501"),
                "{statement} must be denied"
            );
        }
        ensure!(frozen_chains(&pool, &[route()?]).await? == BTreeSet::from([CHAIN_ID]));
        Ok(())
    })
    .await
}

#[tokio::test]
async fn loop_respects_cancellation() -> Result<()> {
    let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    // The current-thread test runtime polls every reconciler future on this thread.
    let _recorder = metrics::set_default_local_recorder(&recorder);
    with_database(|pool| async move {
        let route = route()?;
        seed_product(&pool, &route.destination.product).await?;
        let chain = Arc::new(MockChain {
            finalized_delay: StdDuration::from_secs(10),
            ..MockChain::default()
        });
        let reconciler = Arc::new(Reconciler::with_dependencies(
            pool.clone(),
            vec![route],
            BTreeMap::from([(CHAIN_ID, Arc::clone(&chain) as Arc<dyn ReconciliationChain>)]),
            Arc::new(MockSettlement::default()),
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
        let rendered = handle.render();
        // Positive control: the recorder captured this loop's metrics, so the absent progress
        // gauge means the cancelled round never completed rather than that nothing was recorded.
        ensure!(
            rendered.contains("topup_loop_heartbeat_unixtime_seconds{loop=\"reconciler\""),
            "{rendered}"
        );
        ensure!(!rendered.contains(RECONCILER_PROGRESS), "{rendered}");
        Ok(())
    })
    .await
}

/// Progress gauge the reconciler sets only after a round in which every check completed.
const RECONCILER_PROGRESS: &str = "topup_loop_progress_unixtime_seconds{loop=\"reconciler\"";

async fn with_database<F, Fut>(test: F) -> Result<()>
where
    F: FnOnce(PgPool) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let Some(context) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = test(context.app_pool.clone()).await;
    let cleanup = context.cleanup().await;
    result.and(cleanup)
}

fn route() -> Result<RouteFile> {
    let mut route: RouteFile =
        serde_saphyr::from_str(include_str!("fixtures/phala-cloud-pha.yaml"))?;
    route.chain.chain_id = CHAIN_ID;
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
        BTreeMap::from([(CHAIN_ID, chain as Arc<dyn ReconciliationChain>)]),
        settlement,
    )?)
}

fn has_check(report: &ReconciliationReport, check: CheckName) -> bool {
    report
        .findings
        .iter()
        .any(|finding| finding.check == check && !finding.repair_applied && !finding.incomplete)
}

/// Records that the scanner has backfilled every address and committed through `block`.
async fn scanned_through(pool: &PgPool, block: u64) -> Result<()> {
    let ids = db::list_scan_addresses(pool, CHAIN_ID)
        .await?
        .into_iter()
        .map(|address| address.id)
        .collect::<Vec<_>>();
    db::commit_scan(pool, CHAIN_ID, &[], &ids, Some(block)).await?;
    Ok(())
}

async fn count(pool: &PgPool, query: &str) -> Result<i64> {
    Ok(sqlx::query_scalar(query).fetch_one(pool).await?)
}

async fn deposit(pool: &PgPool, id: Uuid) -> Result<Deposit> {
    db::get_deposit(pool, id)
        .await?
        .context("deposit must exist")
}

fn transfer(
    number: u8,
    log_index: u64,
    block_number: u64,
    token: Address,
    from: Address,
    to: Address,
    amount: u64,
) -> TransferLog {
    TransferLog {
        tx_hash: B256::from([number; 32]),
        log_index,
        block_number,
        block_hash: B256::from([number.wrapping_add(1); 32]),
        block_time: Utc::now(),
        token,
        from,
        to,
        amount: AtomicAmount::new(U256::from(amount)),
    }
}

/// Returns the route's product, creating it once: settlement calls resolve its attested destination.
async fn seed_product(pool: &PgPool, slug: &str) -> Result<Uuid> {
    let existing = sqlx::query_scalar::<_, Uuid>("SELECT id FROM products WHERE slug = $1")
        .bind(slug)
        .fetch_optional(pool)
        .await?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = Uuid::new_v4();
    db::create_product(
        pool,
        &NewProduct {
            id,
            slug: slug.to_owned(),
            webhook_url: "http://product.test/webhooks".to_owned(),
            pubkey: "test".to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    Ok(id)
}

async fn seed_identity(pool: &PgPool, route: &RouteFile, number: u8) -> Result<Seed> {
    let product_id = seed_product(pool, &route.destination.product).await?;
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

struct DepositSeed {
    number: u8,
    state: DepositState,
    block_number: u64,
    amount: U256,
    credit_minor: u64,
}

impl DepositSeed {
    fn new(number: u8, state: DepositState) -> Self {
        Self {
            number,
            state,
            block_number: 100,
            amount: U256::from(10_u64).pow(U256::from(18_u8)),
            credit_minor: 100,
        }
    }

    fn block(self, block_number: u64) -> Self {
        Self {
            block_number,
            ..self
        }
    }

    fn amount(self, amount: u64) -> Self {
        Self {
            amount: U256::from(amount),
            ..self
        }
    }

    fn credit(self, credit_minor: u64) -> Self {
        Self {
            credit_minor,
            ..self
        }
    }
}

async fn seed_deposit(
    pool: &PgPool,
    route: &RouteFile,
    seed: &Seed,
    fixture: DepositSeed,
) -> Result<Uuid> {
    let number = fixture.number;
    let deposit = NewDeposit {
        chain_id: route.chain.chain_id,
        tx_hash: B256::from([number; 32]),
        log_index: u64::from(number),
        block_number: fixture.block_number,
        block_hash: B256::from([number.wrapping_add(1); 32]),
        block_time: Utc::now(),
        address_id: seed.address_id,
        account_id: seed.account_id,
        route: Some(route.route.clone()),
        route_version: Some(route.version),
        asset_contract: route.asset.contract,
        from_address: Address::from([201; 20]),
        amount_atomic: AtomicAmount::new(fixture.amount),
        state: fixture.state,
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
    .bind(fixture.credit_minor.to_string())
    .execute(pool)
    .await?;
    Ok(id)
}

async fn seed_confirmed_flush(
    pool: &PgPool,
    route: &RouteFile,
    seed: &Seed,
    block_number: u64,
    log_index: u64,
    amount: u64,
) -> Result<Uuid> {
    let flush_id = Uuid::new_v4();
    db::insert_flush(
        pool,
        &NewFlush {
            id: flush_id,
            chain_id: route.chain.chain_id,
            token: route.asset.contract,
            operator: Address::from([22; 20]),
            nonce: block_number,
            tx_hash: Some(B256::from([23; 32])),
            block_number: Some(block_number),
            status: "confirmed".to_owned(),
            receipt: Some(json!({})),
        },
    )
    .await?;
    db::insert_flushed(
        pool,
        &FlushedEvent {
            flush_id,
            address_id: seed.address_id,
            amount_atomic: AtomicAmount::new(U256::from(amount)),
            block_number,
            log_index,
        },
    )
    .await?;
    Ok(flush_id)
}

async fn seed_sent(pool: &PgPool, route: &RouteFile, number: u8) -> Result<Uuid> {
    let seed = seed_identity(pool, route, number).await?;
    let id = seed_deposit(
        pool,
        route,
        &seed,
        DepositSeed::new(number, DepositState::Cleared),
    )
    .await?;
    let account = db::get_account(pool, seed.account_id)
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
    Ok(id)
}

async fn accepted_answer(pool: &PgPool, id: Uuid) -> Result<SettlementAnswer> {
    Ok(SettlementAnswer::Accepted {
        destination_tx_id: format!("credit-{id}"),
        payload: settlement_payload(pool, id).await?,
    })
}

async fn settlement_payload(pool: &PgPool, id: Uuid) -> Result<Value> {
    let deposit = deposit(pool, id).await?;
    let account = db::get_account(pool, deposit.account_id)
        .await?
        .context("account must exist")?;
    let address = db::get_address(pool, deposit.address_id)
        .await?
        .context("address must exist")?;
    Ok(json!({
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

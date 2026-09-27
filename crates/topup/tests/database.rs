//! PostgreSQL integration tests for the C1 database boundary.

mod support;

use std::collections::BTreeMap;
use std::env;
use std::process::Command;
use std::str::FromStr;
use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::json;
use sqlx::{AssertSqlSafe, PgPool, Row};
use topup::db::{
    self, AddressKind, ApplyTransitionResult, EventObject, FlushedEvent, NewDeposit, NewFlush,
    OutboxEvent, TransitionUpdate,
};
use topup::reconciler::{CheckName, Reconciler, ReconciliationChain, ReconciliationError};
use topup::{heartbeat, restore};
use topup_adapters::chain::evm::TransferLog;
use topup_core::deposit::{DepositState, StepOutcome, WaitReason, next};
use topup_core::identity::deposit_id;
use topup_core::money::AtomicAmount;
use topup_core::route::RouteFile;
use uuid::Uuid;

use support::seed::{self, NewAccount, NewAddress, NewProduct};
use support::with_database;

#[tokio::test]
async fn migrations_apply_from_scratch_and_are_idempotent() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            db::migrate(&context.owner_pool).await?;
            let output = Command::new(env!("CARGO_BIN_EXE_topup"))
                .arg("migrate")
                .env("MIGRATE_DATABASE_URL", &context.owner_url)
                .env_remove("DATABASE_URL")
                .output()
                .context("run topup migrate")?;
            ensure!(
                output.status.success(),
                "topup migrate failed: {}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn restore_check_accepts_a_current_schema_and_fresh_heartbeat() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let heartbeat = heartbeat::record(&context.app_pool).await?;

            let reconciler = restore_reconciler(&context.owner_pool)?;
            let expectations = restore_expectations(&heartbeat);
            let report = restore::check(&context.owner_pool, &expectations, &reconciler)
                .await
                .map_err(anyhow::Error::msg)?;
            ensure!(report.status == "ok");
            let latest = db::MIGRATOR
                .iter()
                .map(|migration| migration.version)
                .max()
                .context("embedded migrations")?;
            ensure!(report.latest_migration == latest);
            ensure!(report.measured_rpo_seconds == Some(0));
            ensure!(report.rpo_basis == "heartbeat_and_lsn");
            ensure!(report.wal_bytes_behind == Some(0));
            ensure!(report.expected_lsn.as_deref() == Some(heartbeat.wal_lsn.as_str()));
            ensure!(report.row_counts.get("heartbeat") == Some(&1));
            ensure!(report.post_restore_reconciliation.status == "complete");
            ensure!(
                !report
                    .post_restore_reconciliation
                    .findings
                    .iter()
                    .any(|finding| finding.incomplete)
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn restore_check_without_a_source_lsn_flags_heartbeat_only_rpo() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let heartbeat = heartbeat::record(&context.app_pool).await?;
            let reconciler = restore_reconciler(&context.owner_pool)?;
            let expectations = restore::RestoreExpectations {
                expected_heartbeat_at: Some(heartbeat.recorded_at),
                expected_lsn: None,
            };
            let report = restore::check(&context.owner_pool, &expectations, &reconciler)
                .await
                .map_err(anyhow::Error::msg)?;
            ensure!(report.status == "ok");
            ensure!(report.rpo_basis == "heartbeat_only");
            ensure!(report.expected_lsn.is_none());
            ensure!(report.wal_bytes_behind.is_none());
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn restore_check_at_boot_reports_an_unanchored_rpo() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let heartbeat = heartbeat::record(&context.app_pool).await?;
            let reconciler = restore_reconciler(&context.owner_pool)?;
            let expectations = restore::RestoreExpectations {
                expected_heartbeat_at: None,
                expected_lsn: None,
            };
            let report = restore::check(&context.owner_pool, &expectations, &reconciler)
                .await
                .map_err(anyhow::Error::msg)?;
            ensure!(report.status == "ok");
            ensure!(report.rpo_basis == "unanchored");
            ensure!(report.measured_rpo_seconds.is_none());
            ensure!(report.restored_heartbeat_at == heartbeat.recorded_at);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn application_role_can_only_append_heartbeats() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let heartbeat = heartbeat::record(&context.app_pool).await?;
            ensure!(heartbeat.wal_lsn.contains('/'));
            let update = sqlx::query("UPDATE heartbeat SET recorded_at = now() WHERE id = $1")
                .bind(heartbeat.id)
                .execute(&context.app_pool)
                .await
                .err();
            assert_sqlstate(update, "42501")?;
            let delete = sqlx::query("DELETE FROM heartbeat WHERE id = $1")
                .bind(heartbeat.id)
                .execute(&context.app_pool)
                .await
                .err();
            assert_sqlstate(delete, "42501")?;
            let truncate = sqlx::query("TRUNCATE heartbeat")
                .execute(&context.app_pool)
                .await
                .err();
            assert_sqlstate(truncate, "42501")?;
            Ok(())
        })
    })
    .await
}

/// Chain double for a restore check run without chain access; only alert-only checks use it.
struct UnavailableChain;

impl UnavailableChain {
    fn error<T>() -> Result<T, ReconciliationError> {
        Err(ReconciliationError::Chain("chain unavailable".to_owned()))
    }
}

#[async_trait]
impl ReconciliationChain for UnavailableChain {
    async fn finalized_head(&self) -> Result<u64, ReconciliationError> {
        Self::error()
    }

    async fn transfer_logs_to(
        &self,
        _addresses: &[Address],
        _from_block: u64,
        _to_block: u64,
    ) -> Result<Vec<TransferLog>, ReconciliationError> {
        Self::error()
    }

    async fn token_balances(
        &self,
        _token: Address,
        _addresses: &[Address],
        _block: u64,
    ) -> Result<Vec<U256>, ReconciliationError> {
        Self::error()
    }

    async fn flushed_total(
        &self,
        _factory: Address,
        _treasury: Address,
        _token: Address,
        _from_block: u64,
        _to_block: u64,
    ) -> Result<U256, ReconciliationError> {
        Self::error()
    }

    async fn factory_addresses(
        &self,
        _factory: Address,
        _treasury: Address,
        _salts: &[B256],
    ) -> Result<Vec<Address>, ReconciliationError> {
        Self::error()
    }
}

fn restore_reconciler(pool: &PgPool) -> Result<Reconciler> {
    let mut route: RouteFile =
        serde_saphyr::from_str(include_str!("fixtures/phala-cloud-pha.yaml"))?;
    route.chain.rpc_providers = vec![
        "http://127.0.0.1:8546".to_owned(),
        "http://localhost:8546".to_owned(),
    ];
    let chain_id = route.chain.chain_id;
    Ok(Reconciler::with_dependencies(
        pool.clone(),
        Arc::new(topup::routes::RouteSet::new(vec![route]).map_err(anyhow::Error::msg)?),
        BTreeMap::from([(
            chain_id,
            Arc::new(UnavailableChain) as Arc<dyn ReconciliationChain>,
        )]),
    ))
}

#[tokio::test]
async fn restore_check_asks_the_product_nothing_and_keeps_recorded_credits() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let heartbeat = heartbeat::record(&context.app_pool).await?;
            let seed = seed_account(&context.app_pool, 3).await?;
            let credited = insert_numbered_deposit(&context.app_pool, &seed, 3).await?;
            sqlx::query(
                "UPDATE deposits SET state = 'credited', valuation_at = now(), \
                 price_scaled = 25000000, price_source = 'spot', credit_minor = 250 WHERE id = $1",
            )
            .bind(credited)
            .execute(&context.owner_pool)
            .await?;
            let reconciler = restore_reconciler(&context.owner_pool)?;
            let expectations = restore_expectations(&heartbeat);

            let report = restore::check(&context.owner_pool, &expectations, &reconciler)
                .await
                .map_err(anyhow::Error::msg)?;
            ensure!(
                report.status == "ok",
                "restore must pass: {:?}",
                report.failures
            );
            ensure!(report.post_restore_reconciliation.status == "complete");
            // Chain checks are alert-only; their failure is reported but does not gate resume.
            ensure!(
                report
                    .post_restore_reconciliation
                    .failed_checks
                    .contains(&CheckName::CustodyBalance)
            );
            let deposit = db::get_deposit(&context.app_pool, credited)
                .await?
                .context("credited deposit")?;
            ensure!(deposit.state == DepositState::Credited);
            ensure!(deposit.credit_minor.map(|value| value.value()) == Some(250));
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn fulfillment_migration_returns_cleared_deposits_to_confirmed_with_history() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            // Recreate the pre-migration schema, where `cleared` still existed.
            sqlx::raw_sql(include_str!(
                "../migrations/20260928000000_webhook_fulfillment.down.sql"
            ))
            .execute(&context.owner_pool)
            .await?;
            let seed = seed_account(&context.app_pool, 5).await?;
            let cleared = insert_numbered_deposit(&context.app_pool, &seed, 5).await?;
            sqlx::query(
                "UPDATE deposits SET state = 'cleared', attempt = 3, valuation_at = now(), \
                 price_scaled = 25000000, price_source = 'lock', credit_minor = 250 WHERE id = $1",
            )
            .bind(cleared)
            .execute(&context.owner_pool)
            .await?;

            sqlx::raw_sql(include_str!(
                "../migrations/20260928000000_webhook_fulfillment.up.sql"
            ))
            .execute(&context.owner_pool)
            .await?;

            let deposit = db::get_deposit(&context.app_pool, cleared)
                .await?
                .context("migrated deposit")?;
            ensure!(deposit.state == DepositState::Confirmed);
            ensure!(deposit.attempt == 0);
            ensure!(deposit.credit_minor.map(|value| value.value()) == Some(250));
            ensure!(deposit.price_source.as_deref() == Some("lock"));
            let transition = sqlx::query(
                "SELECT from_state, to_state, evidence FROM transitions WHERE deposit_id = $1",
            )
            .bind(cleared)
            .fetch_one(&context.app_pool)
            .await?;
            ensure!(transition.try_get::<String, _>("from_state")? == "cleared");
            ensure!(transition.try_get::<String, _>("to_state")? == "confirmed");
            ensure!(
                transition.try_get::<serde_json::Value, _>("evidence")?
                    == json!({"migration": "webhook_fulfillment"})
            );
            assert_sqlstate(
                sqlx::query("UPDATE deposits SET state = 'cleared' WHERE id = $1")
                    .bind(cleared)
                    .execute(&context.owner_pool)
                    .await
                    .err(),
                "23514",
            )?;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn application_role_can_append_and_read_history_but_cannot_mutate_it() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let seed = seed_account(&context.app_pool, 1).await?;
            let deposit = new_deposit(seed.address_id, seed.account_id, 1, 1, 0);
            let deposit_id = deposit_id(deposit.chain_id, deposit.tx_hash, deposit.log_index);
            ensure!(db::insert_deposit(&context.app_pool, &deposit).await?);

            let transition_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO transitions (id, deposit_id, from_state, to_state, attempt, evidence) VALUES ($1, $2, 'detected', 'detected', 0, '{}'::jsonb)",
            )
            .bind(transition_id)
            .bind(deposit_id)
            .execute(&context.app_pool)
            .await?;
            let audit_id = Uuid::new_v4();
            seed::insert_audit(
                &context.app_pool,
                audit_id,
                "admin:test",
                "pause",
                "product:test",
                "permission test",
            )
            .await?;

            let transition_count: i64 = sqlx::query("SELECT count(*) FROM transitions")
                .fetch_one(&context.app_pool)
                .await?
                .try_get(0)?;
            let audit_count: i64 = sqlx::query("SELECT count(*) FROM audit")
                .fetch_one(&context.app_pool)
                .await?
                .try_get(0)?;
            ensure!(transition_count == 1 && audit_count == 1);

            for statement in [
                "UPDATE transitions SET evidence = '{}'::jsonb",
                "DELETE FROM transitions",
                "TRUNCATE transitions",
                "UPDATE audit SET reason = 'changed'",
                "DELETE FROM audit",
                "TRUNCATE audit",
            ] {
                assert_sqlstate(
                    sqlx::query(statement)
                        .execute(&context.app_pool)
                        .await
                        .err(),
                    "42501",
                )?;
            }
            assert_sqlstate(
                sqlx::query("TRUNCATE outbox")
                    .execute(&context.app_pool)
                    .await
                    .err(),
                "42501",
            )?;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn application_role_can_read_but_cannot_mutate_migration_history() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let versions: Vec<i64> =
                sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
                    .fetch_all(&context.app_pool)
                    .await?;
            let expected: Vec<i64> = db::MIGRATOR
                .iter()
                .filter(|migration| migration.migration_type.is_up_migration())
                .map(|migration| migration.version)
                .collect();
            ensure!(versions == expected);

            for statement in [
                "UPDATE _sqlx_migrations SET version = version",
                "DELETE FROM _sqlx_migrations",
            ] {
                assert_sqlstate(
                    sqlx::query(statement)
                        .execute(&context.app_pool)
                        .await
                        .err(),
                    "42501",
                )?;
            }
            Ok(())
        })
    })
    .await
}

/// Mirrors the grants table in `crates/topup/migrations/README.md`; grants come from default
/// privileges, so a new table must be listed here with its intended privileges.
const DOCUMENTED_GRANTS: &[(&str, &[&str])] = &[
    ("transitions", &["SELECT", "INSERT"]),
    ("audit", &["SELECT", "INSERT"]),
    ("reconciliation_findings", &["SELECT", "INSERT"]),
    ("heartbeat", &["SELECT", "INSERT"]),
    ("reconciliation_blocks", &["SELECT", "INSERT", "DELETE"]),
    (
        "reconciliation_deposit_cursors",
        &["SELECT", "INSERT", "UPDATE"],
    ),
    (
        "reconciliation_custody_cursors",
        &["SELECT", "INSERT", "UPDATE"],
    ),
    ("_sqlx_migrations", &["SELECT"]),
    ("settlements", &["SELECT"]),
    ("products", OPERATIONAL),
    ("accounts", OPERATIONAL),
    ("route_pauses", OPERATIONAL),
    ("seen_signatures", OPERATIONAL),
    ("addresses", OPERATIONAL),
    ("cursors", OPERATIONAL),
    ("pending_transfers", OPERATIONAL),
    ("flushes", OPERATIONAL),
    ("flushed", OPERATIONAL),
    ("flush_exclusions", OPERATIONAL),
    ("deposits", OPERATIONAL),
    ("rate_locks", OPERATIONAL),
    ("outbox", OPERATIONAL),
    ("refunds", OPERATIONAL),
    ("refund_payment_claims", OPERATIONAL),
];
const OPERATIONAL: &[&str] = &["SELECT", "INSERT", "UPDATE", "DELETE"];

#[tokio::test]
async fn application_role_privileges_match_the_documented_grants() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let tables: Vec<String> = sqlx::query_scalar(
                "SELECT tablename::text FROM pg_tables WHERE schemaname = 'public' ORDER BY 1",
            )
            .fetch_all(&context.owner_pool)
            .await?;
            let documented: BTreeMap<&str, &[&str]> = DOCUMENTED_GRANTS.iter().copied().collect();
            ensure!(documented.len() == DOCUMENTED_GRANTS.len(), "duplicate documented table");
            let mut listed: Vec<&str> = documented.keys().copied().collect();
            listed.sort_unstable();
            ensure!(
                tables == listed,
                "public tables differ from the documented grants: tables={tables:?} documented={listed:?}"
            );

            for table in &tables {
                let expected = documented
                    .get(table.as_str())
                    .context("documented table")?;
                for privilege in [
                    "SELECT",
                    "INSERT",
                    "UPDATE",
                    "DELETE",
                    "TRUNCATE",
                    "REFERENCES",
                    "TRIGGER",
                ] {
                    let granted: bool =
                        sqlx::query_scalar("SELECT has_table_privilege('topup_app', $1, $2)")
                            .bind(format!("public.{table}"))
                            .bind(privilege)
                            .fetch_one(&context.owner_pool)
                            .await?;
                    ensure!(
                        granted == expected.contains(&privilege),
                        "topup_app {privilege} on {table}: granted={granted}"
                    );
                }
            }
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn owner_side_history_mutation_is_rejected_by_defense_in_depth_triggers() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let seed = seed_account(&context.app_pool, 2).await?;
            let deposit = new_deposit(seed.address_id, seed.account_id, 1, 2, 0);
            let deposit_id = deposit_id(deposit.chain_id, deposit.tx_hash, deposit.log_index);
            db::insert_deposit(&context.app_pool, &deposit).await?;
            let transition_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO transitions (id, deposit_id, from_state, to_state, attempt, evidence) VALUES ($1, $2, 'detected', 'detected', 0, '{}'::jsonb)",
            )
            .bind(transition_id)
            .bind(deposit_id)
            .execute(&context.app_pool)
            .await?;
            let audit_id = Uuid::new_v4();
            seed::insert_audit(
                &context.app_pool,
                audit_id,
                "admin:test",
                "pause",
                "product:test",
                "trigger test",
            )
            .await?;
            // Only the owner can still write the retired settlement protocol's history.
            sqlx::query(
                "INSERT INTO settlements (deposit_id, product_id, key, payload, status) \
                 VALUES ($1, $2, $3, '{}'::jsonb, 'accepted')",
            )
            .bind(deposit_id)
            .bind(seed.product_id)
            .bind(format!("deposit:{deposit_id}"))
            .execute(&context.owner_pool)
            .await?;

            for (statement, id) in [
                ("UPDATE transitions SET evidence = '{}'::jsonb WHERE id = $1", transition_id),
                ("DELETE FROM transitions WHERE id = $1", transition_id),
                ("UPDATE audit SET reason = 'changed' WHERE id = $1", audit_id),
                ("DELETE FROM audit WHERE id = $1", audit_id),
                ("UPDATE settlements SET status = 'rejected' WHERE deposit_id = $1", deposit_id),
                ("DELETE FROM settlements WHERE deposit_id = $1", deposit_id),
            ] {
                assert_sqlstate(
                    sqlx::query(statement)
                        .bind(id)
                        .execute(&context.owner_pool)
                        .await
                        .err(),
                    "55000",
                )?;
            }
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn products_and_accounts_enforce_identity_uniqueness_and_only_pause_mutates() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let first_product = new_product(10, "product-a");
            let second_product = new_product(11, "product-b");
            seed::create_product(&context.app_pool, &first_product).await?;
            seed::create_product(&context.app_pool, &second_product).await?;

            seed::set_product_paused_scopes(
                &context.app_pool,
                first_product.id,
                &["quotes".to_owned()],
            )
            .await?;
            let stored_product = db::get_product(&context.app_pool, first_product.id)
                .await?
                .context("product must exist")?;
            ensure!(stored_product.slug == first_product.slug);
            ensure!(stored_product.paused_scopes == ["quotes"]);

            let first_account = NewAccount {
                id: Uuid::new_v4(),
                product_id: first_product.id,
                external_id: "workspace".to_owned(),
                paused_scopes: Vec::new(),
            };
            seed::create_account(&context.app_pool, &first_account).await?;
            assert_unique(
                seed::create_account(
                    &context.app_pool,
                    &NewAccount {
                        id: Uuid::new_v4(),
                        ..first_account.clone()
                    },
                )
                .await
                .err(),
            )?;
            seed::create_account(
                &context.app_pool,
                &NewAccount {
                    id: Uuid::new_v4(),
                    product_id: second_product.id,
                    ..first_account.clone()
                },
            )
            .await?;
            seed::set_account_paused_scopes(
                &context.app_pool,
                first_account.id,
                &["settlement".to_owned()],
            )
            .await?;
            let stored_account = db::get_account(&context.app_pool, first_account.id)
                .await?
                .context("account must exist")?;
            ensure!(stored_account.product_id == first_account.product_id);
            ensure!(stored_account.external_id == first_account.external_id);
            ensure!(stored_account.paused_scopes == ["settlement"]);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn addresses_are_canonical_and_unique_per_chain() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let first_account = seed_account_without_address(&context.app_pool, 20).await?;
            let second = seed_account_without_address(&context.app_pool, 21).await?;
            let checksum = Address::from_str("0x52908400098527886E0F7030069857D2E4169EE7")?;
            let lowercase = Address::from_str("0x52908400098527886e0f7030069857d2e4169ee7")?;
            ensure!(checksum == lowercase);
            let first = new_address(first_account.account_id, 1, 1, checksum, 20);
            seed::insert_address(&context.app_pool, &first).await?;
            assert_unique(
                seed::insert_address(
                    &context.app_pool,
                    &new_address(second.account_id, 1, 1, lowercase, 22),
                )
                .await
                .err(),
            )?;

            // One account may hold several addresses on a chain.
            seed::insert_address(
                &context.app_pool,
                &new_address(first_account.account_id, 1, 2, evm_address(23), 23),
            )
            .await?;
            seed::insert_address(
                &context.app_pool,
                &new_address(second.account_id, 2, 1, lowercase, 24),
            )
            .await?;

            let before: i64 = sqlx::query("SELECT count(*) FROM addresses")
                .fetch_one(&context.app_pool)
                .await?
                .try_get(0)?;
            ensure!(Address::from_str("not-an-address").is_err());
            ensure!(B256::from_str("not-a-hash").is_err());
            let after: i64 = sqlx::query("SELECT count(*) FROM addresses")
                .fetch_one(&context.app_pool)
                .await?
                .try_get(0)?;
            ensure!(before == after, "malformed input reached the database");
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn deposits_are_idempotent_and_concurrent_claimers_get_different_rows() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let seed = seed_account(&context.app_pool, 30).await?;
            let first = new_deposit(seed.address_id, seed.account_id, 1, 30, 0);
            ensure!(db::insert_deposit(&context.app_pool, &first).await?);
            ensure!(!db::insert_deposit(&context.app_pool, &first).await?);
            ensure!(
                db::insert_deposit(
                    &context.app_pool,
                    &NewDeposit {
                        chain_id: 2,
                        ..first.clone()
                    },
                )
                .await?
            );
            db::insert_deposit(
                &context.app_pool,
                &new_deposit(seed.address_id, seed.account_id, 1, 31, 0),
            )
            .await?;

            let (first_claim, second_claim) = tokio::join!(
                db::claim_deposit(&context.app_pool, Uuid::new_v4()),
                db::claim_deposit(&context.app_pool, Uuid::new_v4())
            );
            let first_claim = first_claim?.context("first claim must return a row")?;
            let second_claim = second_claim?.context("second claim must return a row")?;
            ensure!(first_claim.id != second_claim.id);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn attempts_survive_claim_and_wait_then_reset_on_advance() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let seed = seed_account(&context.app_pool, 40).await?;
            let deposit = new_deposit(seed.address_id, seed.account_id, 1, 40, 0);
            let id = deposit_id(deposit.chain_id, deposit.tx_hash, deposit.log_index);
            db::insert_deposit(&context.app_pool, &deposit).await?;
            sqlx::query("UPDATE deposits SET attempt = 3 WHERE id = $1")
                .bind(id)
                .execute(&context.app_pool)
                .await?;

            let claimed = db::claim_deposit(&context.app_pool, Uuid::new_v4())
                .await?
                .context("deposit must be claimable")?;
            ensure!(claimed.attempt == 3);
            let wait = next(
                DepositState::Detected,
                &StepOutcome::Wait {
                    reason: WaitReason::Paused,
                },
            )?;
            let mut transaction = context.app_pool.begin().await?;
            let result = db::apply_transition(
                &mut transaction,
                id,
                DepositState::Detected,
                claimed.lease_token.context("claim must have a token")?,
                TransitionUpdate {
                    transition: wait,
                    rejection_reason: None,
                    attempt: 3,
                    next_attempt_at: Utc::now() - Duration::seconds(1),
                },
                db::TransitionWrites {
                    evidence: &json!({"wait": "paused"}),
                    effects: &db::TransitionEffects::default(),
                    outbox_events: &[],
                },
            )
            .await?;
            ensure!(result == ApplyTransitionResult::Applied);
            transaction.commit().await?;
            ensure!(
                db::get_deposit(&context.app_pool, id)
                    .await?
                    .context("deposit")?
                    .attempt
                    == 3
            );

            let claimed = db::claim_deposit(&context.app_pool, Uuid::new_v4())
                .await?
                .context("deposit must be claimable again")?;
            let advance = next(DepositState::Detected, &StepOutcome::Advance)?;
            let mut transaction = context.app_pool.begin().await?;
            db::apply_transition(
                &mut transaction,
                id,
                DepositState::Detected,
                claimed.lease_token.context("claim must have a token")?,
                TransitionUpdate {
                    transition: advance,
                    rejection_reason: None,
                    attempt: 0,
                    next_attempt_at: Utc::now(),
                },
                db::TransitionWrites {
                    evidence: &json!({"advance": true}),
                    effects: &db::TransitionEffects::default(),
                    outbox_events: &[],
                },
            )
            .await?;
            transaction.commit().await?;
            let stored = db::get_deposit(&context.app_pool, id)
                .await?
                .context("deposit must exist")?;
            ensure!(stored.state == DepositState::Confirmed && stored.attempt == 0);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn transition_cas_and_outbox_are_atomic() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let seed = seed_account(&context.app_pool, 50).await?;
            let deposit = new_deposit(seed.address_id, seed.account_id, 1, 50, 0);
            let id = deposit_id(deposit.chain_id, deposit.tx_hash, deposit.log_index);
            db::insert_deposit(&context.app_pool, &deposit).await?;
            let claimed = db::claim_deposit(&context.app_pool, Uuid::new_v4())
                .await?
                .context("deposit must be claimable")?;
            let transition = next(DepositState::Detected, &StepOutcome::Advance)?;
            let update = TransitionUpdate {
                transition,
                rejection_reason: None,
                attempt: 0,
                next_attempt_at: Utc::now(),
            };

            let mut transaction = context.app_pool.begin().await?;
            ensure!(
                db::apply_transition(
                    &mut transaction,
                    id,
                    DepositState::Detected,
                    Uuid::new_v4(),
                    update,
                    db::TransitionWrites {
                        evidence: &json!({}),
                        effects: &db::TransitionEffects::default(),
                        outbox_events: &[],
                    },
                )
                .await?
                    == ApplyTransitionResult::Stale
            );
            transaction.commit().await?;

            // The second event cannot be stored (its product does not exist), after the state,
            // transition, and first event were written: none of them may survive.
            let first_event = Uuid::new_v4();
            let events = [
                OutboxEvent {
                    id: first_event,
                    event_type: "deposit.rejected".to_owned(),
                    product_id: seed.product_id,
                    object: EventObject::Deposit(id),
                    next_attempt_at: Utc::now(),
                },
                OutboxEvent {
                    id: Uuid::new_v4(),
                    event_type: "deposit.rejected".to_owned(),
                    product_id: Uuid::new_v4(),
                    object: EventObject::Deposit(id),
                    next_attempt_at: Utc::now(),
                },
            ];
            let mut transaction = context.app_pool.begin().await?;
            ensure!(
                db::apply_transition(
                    &mut transaction,
                    id,
                    DepositState::Detected,
                    claimed.lease_token.context("claim must have a token")?,
                    update,
                    db::TransitionWrites {
                        evidence: &json!({"atomic": true}),
                        effects: &db::TransitionEffects::default(),
                        outbox_events: &events,
                    },
                )
                .await
                .is_err()
            );
            transaction.rollback().await?;
            let stored = db::get_deposit(&context.app_pool, id)
                .await?
                .context("deposit must exist")?;
            ensure!(stored.state == DepositState::Detected);
            ensure!(count_where(&context.app_pool, "transitions", "deposit_id", id).await? == 0);
            ensure!(count_where(&context.app_pool, "outbox", "id", first_event).await? == 0);

            // A repeated event id is written once: deterministic ids make re-emission a no-op.
            let repeated = Uuid::new_v4();
            let events = [id, Uuid::new_v4()].map(|object| OutboxEvent {
                id: repeated,
                event_type: "deposit.credited".to_owned(),
                product_id: seed.product_id,
                object: EventObject::Deposit(object),
                next_attempt_at: Utc::now(),
            });
            let mut transaction = context.app_pool.begin().await?;
            ensure!(
                db::apply_transition(
                    &mut transaction,
                    id,
                    DepositState::Detected,
                    claimed.lease_token.context("claim must have a token")?,
                    update,
                    db::TransitionWrites {
                        evidence: &json!({}),
                        effects: &db::TransitionEffects::default(),
                        outbox_events: &events,
                    },
                )
                .await?
                    == ApplyTransitionResult::Applied
            );
            transaction.commit().await?;
            let objects: Vec<Uuid> =
                sqlx::query_scalar("SELECT object_id FROM outbox WHERE id = $1")
                    .bind(repeated)
                    .fetch_all(&context.app_pool)
                    .await?;
            ensure!(objects == [id]);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn flush_and_flushed_uniqueness_allow_independent_operators_and_addresses() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let first = seed_account(&context.app_pool, 70).await?;
            let second = seed_account(&context.app_pool, 71).await?;
            let flush = NewFlush {
                id: Uuid::new_v4(),
                chain_id: 1,
                token: evm_address(70),
                operator: evm_address(71),
                nonce: 7,
                tx_hash: None,
                block_number: None,
                status: "planned".to_owned(),
                receipt: None,
            };
            db::insert_flush(&context.app_pool, &flush).await?;
            assert_unique(
                db::insert_flush(
                    &context.app_pool,
                    &NewFlush {
                        id: Uuid::new_v4(),
                        ..flush.clone()
                    },
                )
                .await
                .err(),
            )?;
            db::insert_flush(
                &context.app_pool,
                &NewFlush {
                    id: Uuid::new_v4(),
                    operator: evm_address(72),
                    ..flush.clone()
                },
            )
            .await?;

            let event = FlushedEvent {
                flush_id: flush.id,
                address_id: first.address_id,
                amount_atomic: atomic(1_000),
                block_number: 100,
                log_index: 4,
            };
            db::insert_flushed(&context.app_pool, &event).await?;
            assert_unique(db::insert_flushed(&context.app_pool, &event).await.err())?;
            db::insert_flushed(
                &context.app_pool,
                &FlushedEvent {
                    address_id: second.address_id,
                    log_index: 5,
                    ..event
                },
            )
            .await?;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn rate_lock_consumption_is_unique_but_unconsumed_locks_are_independent() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let seed = seed_account(&context.app_pool, 80).await?;
            let first_deposit = insert_numbered_deposit(&context.app_pool, &seed, 80).await?;
            let second_deposit = insert_numbered_deposit(&context.app_pool, &seed, 81).await?;
            let first_lock = insert_lock_address(&context.app_pool, seed.account_id, 80).await?;
            let second_lock = insert_lock_address(&context.app_pool, seed.account_id, 81).await?;
            let third_lock = insert_lock_address(&context.app_pool, seed.account_id, 82).await?;
            let fourth_lock = insert_lock_address(&context.app_pool, seed.account_id, 83).await?;

            insert_rate_lock(&context.app_pool, first_lock, Some(first_deposit)).await?;
            assert_unique(
                insert_rate_lock(&context.app_pool, second_lock, Some(first_deposit))
                    .await
                    .err(),
            )?;
            insert_rate_lock(&context.app_pool, third_lock, None).await?;
            insert_rate_lock(&context.app_pool, fourth_lock, Some(second_deposit)).await?;
            Ok(())
        })
    })
    .await
}

#[derive(Clone, Copy)]
struct Seed {
    product_id: Uuid,
    account_id: Uuid,
    address_id: Uuid,
}

#[derive(Clone, Copy)]
struct AccountSeed {
    product_id: Uuid,
    account_id: Uuid,
}

async fn seed_account(pool: &PgPool, number: u8) -> Result<Seed> {
    let account = seed_account_without_address(pool, number).await?;
    let address = new_address(account.account_id, 1, 1, evm_address(number), number);
    seed::insert_address(pool, &address).await?;
    Ok(Seed {
        product_id: account.product_id,
        account_id: account.account_id,
        address_id: address.id,
    })
}

async fn seed_account_without_address(pool: &PgPool, number: u8) -> Result<AccountSeed> {
    let product = new_product(number, &format!("product-{number}"));
    seed::create_product(pool, &product).await?;
    let account = NewAccount {
        id: Uuid::new_v4(),
        product_id: product.id,
        external_id: format!("workspace-{number}"),
        paused_scopes: Vec::new(),
    };
    seed::create_account(pool, &account).await?;
    Ok(AccountSeed {
        product_id: product.id,
        account_id: account.id,
    })
}

fn new_product(number: u8, slug: &str) -> NewProduct {
    NewProduct {
        id: Uuid::new_v4(),
        slug: slug.to_owned(),
        webhook_url: format!("https://product-{number}.test/webhooks"),
        pubkey: format!("public-key-{number}"),
        paused_scopes: Vec::new(),
    }
}

fn new_address(
    account_id: Uuid,
    chain_id: u64,
    version: u64,
    address: Address,
    salt_byte: u8,
) -> NewAddress {
    NewAddress {
        id: Uuid::new_v4(),
        account_id,
        chain_id,
        kind: AddressKind::Persistent,
        version,
        lock_ref: None,
        salt: b256(salt_byte),
        address,
        retired_at: None,
    }
}

fn new_deposit(
    address_id: Uuid,
    account_id: Uuid,
    chain_id: u64,
    number: u8,
    log_index: u64,
) -> NewDeposit {
    NewDeposit {
        chain_id,
        tx_hash: b256(number),
        log_index,
        block_number: 100 + u64::from(number),
        block_hash: b256(number.wrapping_add(1)),
        block_time: Utc::now(),
        address_id,
        account_id,
        route: Some("ethereum-pha".to_owned()),
        route_version: Some(1),
        asset_contract: evm_address(200),
        from_address: evm_address(number.wrapping_add(100)),
        amount_atomic: atomic(1_000),
        state: DepositState::Detected,
        reason: None,
        next_attempt_at: Utc::now() - Duration::seconds(1),
    }
}

async fn insert_numbered_deposit(pool: &PgPool, seed: &Seed, number: u8) -> Result<Uuid> {
    let deposit = new_deposit(seed.address_id, seed.account_id, 1, number, 0);
    let id = deposit_id(deposit.chain_id, deposit.tx_hash, deposit.log_index);
    ensure!(db::insert_deposit(pool, &deposit).await?);
    Ok(id)
}

/// Failure point exactly as an operator reads it from the last heartbeat log line.
fn restore_expectations(heartbeat: &heartbeat::Heartbeat) -> restore::RestoreExpectations {
    restore::RestoreExpectations {
        expected_heartbeat_at: Some(heartbeat.recorded_at),
        expected_lsn: Some(heartbeat.wal_lsn.clone()),
    }
}

async fn insert_lock_address(pool: &PgPool, account_id: Uuid, number: u8) -> Result<Uuid> {
    let id = Uuid::new_v4();
    seed::insert_address(
        pool,
        &NewAddress {
            id,
            account_id,
            chain_id: 1,
            kind: AddressKind::Lock,
            version: 0,
            lock_ref: Some(format!("lock-{number}")),
            salt: b256(number),
            address: evm_address(number.wrapping_add(100)),
            retired_at: None,
        },
    )
    .await?;
    Ok(id)
}

async fn insert_rate_lock(
    pool: &PgPool,
    address_id: Uuid,
    consumed_by: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    let (status, closed_at) = if consumed_by.is_some() {
        ("consumed", Some(Utc::now()))
    } else {
        ("open", None)
    };
    sqlx::query(
        "INSERT INTO rate_locks (address_id, route, amount_atomic, price_scaled, credit_minor, expires_at, consumed_by, status, closed_at) VALUES ($1, 'ethereum-pha', 1000, 25000000, 250, now() + interval '15 minutes', $2, $3, $4)",
    )
    .bind(address_id)
    .bind(consumed_by)
    .bind(status)
    .bind(closed_at)
    .execute(pool)
    .await?;
    Ok(())
}

async fn count_where(pool: &PgPool, table: &str, column: &str, id: Uuid) -> Result<i64> {
    let row = sqlx::query(AssertSqlSafe(format!(
        "SELECT count(*) FROM {table} WHERE {column} = $1"
    )))
    .bind(id)
    .fetch_one(pool)
    .await?;
    Ok(row.try_get(0)?)
}

fn atomic(value: u64) -> AtomicAmount {
    AtomicAmount::new(U256::from(value))
}

fn evm_address(byte: u8) -> Address {
    Address::from([byte; 20])
}

fn b256(byte: u8) -> B256 {
    B256::from([byte; 32])
}

fn assert_unique(error: Option<sqlx::Error>) -> Result<()> {
    assert_sqlstate(error, "23505")
}

fn assert_sqlstate(error: Option<sqlx::Error>, expected: &str) -> Result<()> {
    let error = error.context("expected a database error")?;
    let database_error = error
        .as_database_error()
        .context("expected a database error")?;
    ensure!(
        database_error.code().as_deref() == Some(expected),
        "expected SQLSTATE {expected}, got {error}"
    );
    Ok(())
}

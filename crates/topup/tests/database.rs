//! PostgreSQL integration tests for the C1 database boundary.

use std::env;
use std::future::Future;
use std::pin::Pin;
use std::process::Command;
use std::str::FromStr;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use chrono::{Duration, Utc};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool, Row};
use topup::db::{
    self, AddressKind, ApplyTransitionResult, FlushedEvent, NewAccount, NewAddress, NewDeposit,
    NewFlush, NewProduct, OutboxEvent, SettlementIntent, TransitionUpdate,
};
use topup_core::deposit::{DepositState, StepOutcome, WaitReason, next};
use topup_core::identity::deposit_id;
use topup_core::money::AtomicAmount;
use url::Url;
use uuid::Uuid;

type TestFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

struct TestContext {
    admin_pool: PgPool,
    owner_pool: PgPool,
    app_pool: PgPool,
    database_name: String,
    app_role: String,
    owner_url: String,
}

impl TestContext {
    async fn create() -> Result<Option<Self>> {
        let Some(owner_template) = required_url("MIGRATE_DATABASE_URL") else {
            return Ok(None);
        };
        let Some(app_template) = required_url("DATABASE_URL") else {
            return Ok(None);
        };

        let mut admin_url =
            Url::parse(&owner_template).context("MIGRATE_DATABASE_URL must be a PostgreSQL URL")?;
        admin_url.set_path("/postgres");
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(admin_url.as_str())
            .await
            .context("connect to the PostgreSQL maintenance database")?;

        sqlx::query("SELECT pg_advisory_lock(704_201_001)")
            .execute(&admin_pool)
            .await?;

        let suffix = Uuid::new_v4().simple().to_string();
        let database_name = format!("topup_c1_{suffix}");
        let app_role = format!("topup_c1_app_{suffix}");
        let password = format!("c1_{suffix}");
        admin_pool
            .execute(format!("CREATE DATABASE \"{database_name}\"").as_str())
            .await
            .context("create isolated test database")?;

        let mut owner_url = Url::parse(&owner_template)?;
        owner_url.set_path(&format!("/{database_name}"));
        let owner_pool = PgPoolOptions::new()
            .max_connections(4)
            .connect(owner_url.as_str())
            .await
            .context("connect to isolated test database as owner")?;
        db::migrate(&owner_pool).await.context("apply migrations")?;

        admin_pool
            .execute(
                format!("CREATE ROLE \"{app_role}\" LOGIN PASSWORD '{password}' IN ROLE topup_app")
                    .as_str(),
            )
            .await
            .context("create isolated application login role")?;

        let mut app_url = Url::parse(&app_template).context("DATABASE_URL must be a URL")?;
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
            .await
            .context("connect to isolated test database as application role")?;

        sqlx::query("SELECT pg_advisory_unlock(704_201_001)")
            .execute(&admin_pool)
            .await?;

        Ok(Some(Self {
            admin_pool,
            owner_pool,
            app_pool,
            database_name,
            app_role,
            owner_url: owner_url.to_string(),
        }))
    }

    async fn cleanup(self) -> Result<()> {
        self.app_pool.close().await;
        self.owner_pool.close().await;
        self.admin_pool
            .execute(format!("DROP DATABASE \"{}\" WITH (FORCE)", self.database_name).as_str())
            .await
            .context("drop isolated test database")?;
        self.admin_pool
            .execute(format!("DROP ROLE \"{}\"", self.app_role).as_str())
            .await
            .context("drop isolated application login role")?;
        self.admin_pool.close().await;
        Ok(())
    }
}

fn required_url(name: &str) -> Option<String> {
    match env::var(name).ok().filter(|value| !value.is_empty()) {
        Some(value) => Some(value),
        None => {
            eprintln!("skipping database integration test: {name} is not set");
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
async fn rate_lock_credit_migration_upgrades_preexisting_rows() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            sqlx::query("ALTER TABLE rate_locks DROP COLUMN credit_minor")
                .execute(&context.owner_pool)
                .await?;
            sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 20260922000008")
                .execute(&context.owner_pool)
                .await?;

            let seed = seed_account(&context.app_pool, 61).await?;
            let deposit_id = insert_numbered_deposit(&context.app_pool, &seed, 61).await?;
            let lock_address = insert_lock_address(&context.app_pool, seed.account_id, 61).await?;
            sqlx::query(
                "INSERT INTO rate_locks (address_id, route, amount_atomic, price_scaled, expires_at, consumed_by) VALUES ($1, 'ethereum-pha', 1000, 25000000, now() + interval '15 minutes', $2)",
            )
            .bind(lock_address)
            .bind(deposit_id)
            .execute(&context.app_pool)
            .await?;

            db::migrate(&context.owner_pool).await?;
            db::migrate(&context.owner_pool).await?;
            let (data_type, precision, nullable): (String, Option<i32>, String) =
                sqlx::query_as(
                    r#"
                    SELECT data_type, numeric_precision, is_nullable
                    FROM information_schema.columns
                    WHERE table_schema = 'public'
                      AND table_name = 'rate_locks'
                      AND column_name = 'credit_minor'
                    "#,
                )
                .fetch_one(&context.owner_pool)
                .await?;
            ensure!(data_type == "numeric");
            ensure!(precision == Some(78));
            ensure!(nullable == "NO");
            let credit_minor: String = sqlx::query_scalar(
                "SELECT credit_minor::text FROM rate_locks WHERE address_id = $1",
            )
            .bind(lock_address)
            .fetch_one(&context.owner_pool)
            .await?;
            ensure!(credit_minor == "0");
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn rate_lock_credit_migration_rejects_preexisting_open_locks() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            sqlx::query("ALTER TABLE rate_locks DROP COLUMN credit_minor")
                .execute(&context.owner_pool)
                .await?;
            sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 20260922000008")
                .execute(&context.owner_pool)
                .await?;

            let seed = seed_account(&context.app_pool, 62).await?;
            let lock_address = insert_lock_address(&context.app_pool, seed.account_id, 62).await?;
            sqlx::query(
                "INSERT INTO rate_locks (address_id, route, amount_atomic, price_scaled, expires_at) VALUES ($1, 'ethereum-pha', 1000, 25000000, now() + interval '15 minutes')",
            )
            .bind(lock_address)
            .execute(&context.app_pool)
            .await?;

            let error = db::migrate(&context.owner_pool)
                .await
                .expect_err("open pre-upgrade lock must block migration");
            ensure!(
                error.to_string().contains(
                    "migration 20260922000008 requires every pre-existing rate lock to be consumed or removed"
                ),
                "unexpected migration error: {error}"
            );
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
            db::insert_audit(
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
            ensure!(versions.contains(&20_260_922_000_015));

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
            db::insert_audit(
                &context.app_pool,
                audit_id,
                "admin:test",
                "pause",
                "product:test",
                "trigger test",
            )
            .await?;

            for (statement, id) in [
                ("UPDATE transitions SET evidence = '{}'::jsonb WHERE id = $1", transition_id),
                ("DELETE FROM transitions WHERE id = $1", transition_id),
                ("UPDATE audit SET reason = 'changed' WHERE id = $1", audit_id),
                ("DELETE FROM audit WHERE id = $1", audit_id),
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
            db::create_product(&context.app_pool, &first_product).await?;
            db::create_product(&context.app_pool, &second_product).await?;

            db::set_product_paused_scopes(
                &context.app_pool,
                first_product.id,
                &["quotes".to_owned()],
            )
            .await?
            .context("product must exist")?;
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
            db::create_account(&context.app_pool, &first_account).await?;
            assert_unique(
                db::create_account(
                    &context.app_pool,
                    &NewAccount {
                        id: Uuid::new_v4(),
                        ..first_account.clone()
                    },
                )
                .await
                .err(),
            )?;
            db::create_account(
                &context.app_pool,
                &NewAccount {
                    id: Uuid::new_v4(),
                    product_id: second_product.id,
                    ..first_account.clone()
                },
            )
            .await?;
            db::set_account_paused_scopes(
                &context.app_pool,
                first_account.id,
                &["settlement".to_owned()],
            )
            .await?
            .context("account must exist")?;
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
async fn addresses_are_canonical_and_enforce_both_unique_keys() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let first_account = seed_account_without_address(&context.app_pool, 20).await?;
            let second = seed_account_without_address(&context.app_pool, 21).await?;
            let checksum = Address::from_str("0x52908400098527886E0F7030069857D2E4169EE7")?;
            let lowercase = Address::from_str("0x52908400098527886e0f7030069857d2e4169ee7")?;
            ensure!(checksum == lowercase);
            let first = new_address(first_account.account_id, 1, 1, checksum, 20);
            db::insert_address(&context.app_pool, &first).await?;
            assert_unique(
                db::insert_address(
                    &context.app_pool,
                    &new_address(second.account_id, 1, 1, lowercase, 22),
                )
                .await
                .err(),
            )?;

            assert_unique(
                db::insert_address(
                    &context.app_pool,
                    &new_address(first_account.account_id, 1, 2, evm_address(23), 23),
                )
                .await
                .err(),
            )?;

            sqlx::query("UPDATE addresses SET retired_at = now() WHERE id = $1")
                .bind(first.id)
                .execute(&context.app_pool)
                .await?;
            db::insert_address(
                &context.app_pool,
                &new_address(first_account.account_id, 1, 2, evm_address(23), 23),
            )
            .await?;
            db::insert_address(
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

            let duplicate_event = Uuid::new_v4();
            let events = [
                OutboxEvent {
                    id: duplicate_event,
                    event_type: "deposit.confirmed".to_owned(),
                    payload: json!({"sequence": 1}),
                    next_attempt_at: Utc::now(),
                },
                OutboxEvent {
                    id: duplicate_event,
                    event_type: "deposit.confirmed".to_owned(),
                    payload: json!({"sequence": 2}),
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
            ensure!(count_where(&context.app_pool, "outbox", "id", duplicate_event).await? == 0);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn settlement_destination_ids_are_unique_per_product() -> Result<()> {
    with_database(|context| {
        Box::pin(async move {
            let first = seed_account(&context.app_pool, 60).await?;
            let second = seed_account(&context.app_pool, 61).await?;
            let third = seed_account(&context.app_pool, 62).await?;
            let first_id = insert_numbered_deposit(&context.app_pool, &first, 60).await?;
            let second_id = insert_numbered_deposit(&context.app_pool, &first, 61).await?;
            let third_id = insert_numbered_deposit(&context.app_pool, &second, 62).await?;
            let fourth_id = insert_numbered_deposit(&context.app_pool, &third, 63).await?;

            for (deposit_id, product_id) in [
                (first_id, first.product_id),
                (second_id, first.product_id),
                (third_id, second.product_id),
                (fourth_id, third.product_id),
            ] {
                db::upsert_intent(
                    &context.app_pool,
                    &SettlementIntent {
                        deposit_id,
                        product_id,
                        key: format!("deposit:{deposit_id}"),
                        payload: json!({"deposit_id": deposit_id}),
                    },
                )
                .await?;
            }
            set_destination(&context.app_pool, first_id, "credit-1").await?;
            assert_unique(
                set_destination(&context.app_pool, second_id, "credit-1")
                    .await
                    .err(),
            )?;
            set_destination(&context.app_pool, third_id, "credit-1").await?;
            set_destination(&context.app_pool, fourth_id, "credit-1").await?;
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
    db::insert_address(pool, &address).await?;
    Ok(Seed {
        product_id: account.product_id,
        account_id: account.account_id,
        address_id: address.id,
    })
}

async fn seed_account_without_address(pool: &PgPool, number: u8) -> Result<AccountSeed> {
    let product = new_product(number, &format!("product-{number}"));
    db::create_product(pool, &product).await?;
    let account = NewAccount {
        id: Uuid::new_v4(),
        product_id: product.id,
        external_id: format!("workspace-{number}"),
        paused_scopes: Vec::new(),
    };
    db::create_account(pool, &account).await?;
    Ok(AccountSeed {
        product_id: product.id,
        account_id: account.id,
    })
}

fn new_product(number: u8, slug: &str) -> NewProduct {
    NewProduct {
        id: Uuid::new_v4(),
        slug: slug.to_owned(),
        settlement_url: format!("https://product-{number}.test/settlements"),
        webhook_url: format!("https://product-{number}.test/webhooks"),
        pubkey: format!("public-key-{number}"),
        kid: format!("product/{number}"),
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

async fn insert_lock_address(pool: &PgPool, account_id: Uuid, number: u8) -> Result<Uuid> {
    let id = Uuid::new_v4();
    db::insert_address(
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
    sqlx::query(
        "INSERT INTO rate_locks (address_id, route, amount_atomic, price_scaled, credit_minor, expires_at, consumed_by) VALUES ($1, 'ethereum-pha', 1000, 25000000, 250, now() + interval '15 minutes', $2)",
    )
    .bind(address_id)
    .bind(consumed_by)
    .execute(pool)
    .await?;
    Ok(())
}

async fn set_destination(
    pool: &PgPool,
    deposit_id: Uuid,
    destination: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE settlements SET destination_tx_id = $2 WHERE deposit_id = $1")
        .bind(deposit_id)
        .bind(destination)
        .execute(pool)
        .await?;
    Ok(())
}

async fn count_where(pool: &PgPool, table: &str, column: &str, id: Uuid) -> Result<i64> {
    let row = sqlx::query(format!("SELECT count(*) FROM {table} WHERE {column} = $1").as_str())
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

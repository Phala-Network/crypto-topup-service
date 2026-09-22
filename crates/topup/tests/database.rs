//! PostgreSQL integration tests for the C1 database boundary.

use std::env;
use std::process::Command;

use anyhow::{Context, Result, ensure};
use chrono::{Duration, Utc};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use topup::db::{
    self, AddressKind, ApplyTransitionResult, AuditEntry, FlushedEvent, NewAccount, NewAddress,
    NewDeposit, NewFlush, NewOutboxEvent, NewProduct, OutboxEvent, SettlementIntent,
    TransitionUpdate,
};
use topup_core::deposit::{DepositState, StepOutcome, next};
use url::Url;
use uuid::Uuid;

#[tokio::test]
async fn database_contracts_hold() -> Result<()> {
    let Some(database_url) = env::var("DATABASE_URL")
        .ok()
        .filter(|value| !value.is_empty())
    else {
        eprintln!("skipping database integration test: DATABASE_URL is not set");
        return Ok(());
    };

    let mut admin_url = Url::parse(&database_url).context("DATABASE_URL must be a URL")?;
    let database_name = format!("topup_c1_{}", Uuid::new_v4().simple());
    admin_url.set_path("/postgres");
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(admin_url.as_str())
        .await
        .context("connect to the PostgreSQL maintenance database")?;
    admin_pool
        .execute(format!("CREATE DATABASE \"{database_name}\"").as_str())
        .await
        .context("create isolated test database")?;

    let mut test_url = Url::parse(&database_url)?;
    test_url.set_path(&format!("/{database_name}"));
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(test_url.as_str())
        .await
        .context("connect to isolated test database")?;

    let result = run_contracts(&pool, test_url.as_str()).await;
    pool.close().await;
    admin_pool
        .execute(format!("DROP DATABASE \"{database_name}\" WITH (FORCE)").as_str())
        .await
        .context("drop isolated test database")?;
    admin_pool.close().await;
    result
}

async fn run_contracts(pool: &PgPool, database_url: &str) -> Result<()> {
    db::migrate(pool).await.context("apply migrations")?;
    db::migrate(pool)
        .await
        .context("apply migrations idempotently")?;

    let cli = Command::new(env!("CARGO_BIN_EXE_topup"))
        .arg("migrate")
        .env("DATABASE_URL", database_url)
        .output()
        .context("run topup migrate")?;
    ensure!(
        cli.status.success(),
        "topup migrate failed: {}",
        String::from_utf8_lossy(&cli.stderr)
    );

    let product_id = Uuid::new_v4();
    let product = NewProduct {
        id: product_id,
        slug: "phala-cloud".to_owned(),
        settlement_url: "https://product.test/settlements".to_owned(),
        webhook_url: "https://product.test/webhooks".to_owned(),
        pubkey: "test-public-key".to_owned(),
        kid: "product/v1".to_owned(),
        paused_scopes: Vec::new(),
    };
    let created_product = db::create_product(pool, &product).await?;
    ensure!(
        created_product.id == product_id,
        "product create returned wrong row"
    );
    let mut updated_product = product.clone();
    updated_product.paused_scopes = vec!["quotes".to_owned()];
    ensure!(
        db::update_product(pool, &updated_product).await?.is_some(),
        "product update missed existing row"
    );
    ensure!(
        db::get_product(pool, product_id).await?.is_some(),
        "product read missed existing row"
    );

    let account_id = Uuid::new_v4();
    let account = NewAccount {
        id: account_id,
        product_id,
        external_id: "workspace-1".to_owned(),
        paused_scopes: Vec::new(),
    };
    db::create_account(pool, &account).await?;
    let mut updated_account = account.clone();
    updated_account.paused_scopes = vec!["settlement".to_owned()];
    ensure!(
        db::update_account(pool, &updated_account).await?.is_some(),
        "account update missed existing row"
    );
    ensure!(
        db::get_account(pool, account_id).await?.is_some(),
        "account read missed existing row"
    );

    let address_id = Uuid::new_v4();
    let address = NewAddress {
        id: address_id,
        account_id,
        chain_id: 1,
        kind: AddressKind::Persistent,
        version: 1,
        lock_ref: None,
        salt: "0x01".to_owned(),
        address: "0x1111111111111111111111111111111111111111".to_owned(),
        retired_at: None,
    };
    db::insert_address(pool, &address).await?;
    ensure!(
        db::find_active_persistent(pool, account_id, 1)
            .await?
            .is_some(),
        "active persistent address was not found"
    );
    ensure!(
        db::find_address_by_chain(pool, 1, &address.address)
            .await?
            .is_some(),
        "chain address lookup missed existing row"
    );

    let duplicate_address = NewAddress {
        id: Uuid::new_v4(),
        version: 2,
        salt: "0x02".to_owned(),
        address: "0x2222222222222222222222222222222222222222".to_owned(),
        ..address.clone()
    };
    assert_unique_violation(db::insert_address(pool, &duplicate_address).await.err())?;

    let first = new_deposit(address_id, account_id, 0);
    let second = new_deposit(address_id, account_id, 1);
    let atomic = new_deposit(address_id, account_id, 2);
    ensure!(
        db::insert_deposit(pool, &first).await?,
        "first deposit was not inserted"
    );
    ensure!(
        db::insert_deposit(pool, &second).await?,
        "second deposit was not inserted"
    );
    ensure!(
        db::insert_deposit(pool, &atomic).await?,
        "atomic-test deposit was not inserted"
    );
    ensure!(
        !db::insert_deposit(
            pool,
            &NewDeposit {
                id: Uuid::new_v4(),
                ..first.clone()
            }
        )
        .await?,
        "deposit identity conflict was not ignored"
    );

    let raw_duplicate = sqlx::query!(
        r#"
        INSERT INTO deposits (
            id, chain_id, tx_hash, log_index, block_number, block_hash, block_time,
            address_id, account_id, route, route_version, asset_contract, from_address,
            amount_atomic, state, reason, attempt, next_attempt_at, created_at, updated_at
        )
        SELECT $1, chain_id, tx_hash, log_index, block_number, block_hash, block_time,
               address_id, account_id, route, route_version, asset_contract, from_address,
               amount_atomic, state, reason, attempt, next_attempt_at, created_at, updated_at
        FROM deposits WHERE id = $2
        "#,
        Uuid::new_v4(),
        first.id
    )
    .execute(pool)
    .await
    .err();
    assert_unique_violation(raw_duplicate)?;

    let token_one = Uuid::new_v4();
    let token_two = Uuid::new_v4();
    let (claim_one, claim_two) = tokio::join!(
        db::claim_deposit(pool, token_one),
        db::claim_deposit(pool, token_two)
    );
    let claim_one = claim_one?.context("first claimer should receive a row")?;
    let claim_two = claim_two?.context("second claimer should receive a row")?;
    ensure!(
        claim_one.id != claim_two.id,
        "concurrent claimers received the same row"
    );

    let transition = next(DepositState::Detected, &StepOutcome::Advance)?;
    let transition_update = TransitionUpdate {
        transition,
        rejection_reason: None,
        attempt: 0,
        next_attempt_at: Utc::now(),
    };
    let mut stale_transaction = pool.begin().await?;
    let stale = db::apply_transition(
        &mut stale_transaction,
        claim_one.id,
        DepositState::Detected,
        Uuid::new_v4(),
        transition_update,
        &json!({"provider": "test"}),
        &[],
    )
    .await?;
    ensure!(
        stale == ApplyTransitionResult::Stale,
        "stale lease token was accepted"
    );
    stale_transaction.commit().await?;

    let matching_token = claim_one
        .lease_token
        .context("claim should have a lease token")?;
    let event_id = Uuid::new_v4();
    let mut transition_transaction = pool.begin().await?;
    let applied = db::apply_transition(
        &mut transition_transaction,
        claim_one.id,
        DepositState::Detected,
        matching_token,
        transition_update,
        &json!({"provider": "test"}),
        &[OutboxEvent {
            id: event_id,
            event_type: "deposit.confirmed".to_owned(),
            payload: json!({"deposit_id": claim_one.id}),
            next_attempt_at: Utc::now(),
        }],
    )
    .await?;
    ensure!(
        applied == ApplyTransitionResult::Applied,
        "valid transition was not applied"
    );
    transition_transaction.commit().await?;
    let stored_state =
        sqlx::query_scalar!("SELECT state FROM deposits WHERE id = $1", claim_one.id)
            .fetch_one(pool)
            .await?;
    ensure!(stored_state == "confirmed", "deposit state did not advance");
    let stored_event = sqlx::query_scalar!("SELECT count(*) FROM outbox WHERE id = $1", event_id)
        .fetch_one(pool)
        .await?;
    ensure!(
        stored_event == Some(1),
        "transition outbox event was not committed"
    );

    let atomic_claim = if claim_two.id == atomic.id {
        claim_two
    } else {
        db::claim_deposit(pool, Uuid::new_v4())
            .await?
            .context("atomic-test deposit should be claimable")?
    };
    let atomic_token = atomic_claim
        .lease_token
        .context("atomic claim should have a token")?;
    let duplicate_event_id = Uuid::new_v4();
    let duplicate_events = [
        OutboxEvent {
            id: duplicate_event_id,
            event_type: "deposit.confirmed".to_owned(),
            payload: json!({"sequence": 1}),
            next_attempt_at: Utc::now(),
        },
        OutboxEvent {
            id: duplicate_event_id,
            event_type: "deposit.confirmed".to_owned(),
            payload: json!({"sequence": 2}),
            next_attempt_at: Utc::now(),
        },
    ];
    let mut atomic_transaction = pool.begin().await?;
    ensure!(
        db::apply_transition(
            &mut atomic_transaction,
            atomic_claim.id,
            DepositState::Detected,
            atomic_token,
            transition_update,
            &json!({"atomic": true}),
            &duplicate_events,
        )
        .await
        .is_err(),
        "duplicate outbox event should fail the transaction"
    );
    atomic_transaction.rollback().await?;
    let atomic_state =
        sqlx::query_scalar!("SELECT state FROM deposits WHERE id = $1", atomic_claim.id)
            .fetch_one(pool)
            .await?;
    ensure!(
        atomic_state == "detected",
        "failed transition update was not rolled back"
    );
    let atomic_transitions = sqlx::query_scalar!(
        "SELECT count(*) FROM transitions WHERE deposit_id = $1",
        atomic_claim.id
    )
    .fetch_one(pool)
    .await?;
    ensure!(
        atomic_transitions == Some(0),
        "failed timeline insert was not rolled back"
    );
    let atomic_events = sqlx::query_scalar!(
        "SELECT count(*) FROM outbox WHERE id = $1",
        duplicate_event_id
    )
    .fetch_one(pool)
    .await?;
    ensure!(
        atomic_events == Some(0),
        "failed outbox insert was not rolled back"
    );

    let transition_id = sqlx::query_scalar!(
        "SELECT id FROM transitions WHERE deposit_id = $1 LIMIT 1",
        claim_one.id
    )
    .fetch_one(pool)
    .await?;
    assert_append_only_error(
        sqlx::query!(
            "UPDATE transitions SET evidence = '{}'::jsonb WHERE id = $1",
            transition_id
        )
        .execute(pool)
        .await
        .err(),
    )?;
    assert_append_only_error(
        sqlx::query!("DELETE FROM transitions WHERE id = $1", transition_id)
            .execute(pool)
            .await
            .err(),
    )?;

    let audit_id = Uuid::new_v4();
    let audit: AuditEntry = db::insert_audit(
        pool,
        audit_id,
        "admin:test",
        "pause",
        "product:phala-cloud",
        "integration test",
    )
    .await?;
    ensure!(audit.id == audit_id, "audit insert returned wrong row");
    assert_append_only_error(
        sqlx::query!(
            "UPDATE audit SET reason = 'changed' WHERE id = $1",
            audit_id
        )
        .execute(pool)
        .await
        .err(),
    )?;
    assert_append_only_error(
        sqlx::query!("DELETE FROM audit WHERE id = $1", audit_id)
            .execute(pool)
            .await
            .err(),
    )?;

    let destination_one = new_deposit(address_id, account_id, 10);
    let destination_two = new_deposit(address_id, account_id, 11);
    db::insert_deposit(pool, &destination_one).await?;
    db::insert_deposit(pool, &destination_two).await?;
    for deposit in [&destination_one, &destination_two] {
        db::upsert_intent(
            pool,
            &SettlementIntent {
                deposit_id: deposit.id,
                product_id,
                key: format!("deposit:{}", deposit.id),
                payload: json!({"deposit_id": deposit.id}),
            },
        )
        .await?;
    }
    sqlx::query!(
        "UPDATE settlements SET destination_tx_id = 'credit-1' WHERE deposit_id = $1",
        destination_one.id
    )
    .execute(pool)
    .await?;
    assert_unique_violation(
        sqlx::query!(
            "UPDATE settlements SET destination_tx_id = 'credit-1' WHERE deposit_id = $1",
            destination_two.id
        )
        .execute(pool)
        .await
        .err(),
    )?;

    let flush = NewFlush {
        id: Uuid::new_v4(),
        chain_id: 1,
        token: "0xtoken".to_owned(),
        operator: "0xoperator".to_owned(),
        nonce: "7".to_owned(),
        tx_hash: None,
        block_number: None,
        status: "planned".to_owned(),
        receipt: None,
    };
    db::insert_flush(pool, &flush).await?;
    assert_unique_violation(
        db::insert_flush(
            pool,
            &NewFlush {
                id: Uuid::new_v4(),
                ..flush.clone()
            },
        )
        .await
        .err(),
    )?;
    db::insert_flushed(
        pool,
        &FlushedEvent {
            flush_id: flush.id,
            address_id,
            amount_atomic: "1000".to_owned(),
            block_number: 100,
            log_index: 4,
        },
    )
    .await?;

    let standalone_event_id = Uuid::new_v4();
    db::enqueue(
        pool,
        &NewOutboxEvent {
            id: standalone_event_id,
            event_type: "deposit.credited".to_owned(),
            payload: json!({"test": true}),
            next_attempt_at: Utc::now() - Duration::seconds(1),
        },
    )
    .await?;
    let claimed_event = db::claim_outbox(pool)
        .await?
        .context("outbox event should be claimable")?;
    ensure!(
        db::mark_delivered(pool, claimed_event.id, &json!({"status": 200})).await?,
        "claimed outbox event was not marked delivered"
    );

    let deletable_account_id = Uuid::new_v4();
    let deletable_product_id = Uuid::new_v4();
    db::create_product(
        pool,
        &NewProduct {
            id: deletable_product_id,
            slug: "deletable".to_owned(),
            settlement_url: "https://delete.test/settlements".to_owned(),
            webhook_url: "https://delete.test/webhooks".to_owned(),
            pubkey: "delete-key".to_owned(),
            kid: "delete/v1".to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    db::create_account(
        pool,
        &NewAccount {
            id: deletable_account_id,
            product_id: deletable_product_id,
            external_id: "delete-me".to_owned(),
            paused_scopes: Vec::new(),
        },
    )
    .await?;
    ensure!(
        db::delete_account(pool, deletable_account_id).await?,
        "account delete failed"
    );
    ensure!(
        db::delete_product(pool, deletable_product_id).await?,
        "product delete failed"
    );

    Ok(())
}

fn new_deposit(address_id: Uuid, account_id: Uuid, log_index: i64) -> NewDeposit {
    NewDeposit {
        id: Uuid::new_v4(),
        chain_id: 1,
        tx_hash: format!("0x{log_index:064x}"),
        log_index,
        block_number: 100 + log_index,
        block_hash: format!("0x{:064x}", 100 + log_index),
        block_time: Utc::now(),
        address_id,
        account_id,
        route: Some("ethereum-pha".to_owned()),
        route_version: Some(1),
        asset_contract: "0xtoken".to_owned(),
        from_address: "0xsender".to_owned(),
        amount_atomic: "1000".to_owned(),
        state: DepositState::Detected,
        reason: None,
        next_attempt_at: Utc::now() - Duration::seconds(1),
    }
}

fn assert_unique_violation(error: Option<sqlx::Error>) -> Result<()> {
    let error = error.context("expected a unique constraint violation")?;
    let database_error = error
        .as_database_error()
        .context("expected a database error")?;
    ensure!(
        database_error.code().as_deref() == Some("23505"),
        "expected SQLSTATE 23505, got {error}"
    );
    Ok(())
}

fn assert_append_only_error(error: Option<sqlx::Error>) -> Result<()> {
    let error = error.context("expected append-only enforcement to reject the mutation")?;
    let database_error = error
        .as_database_error()
        .context("expected a database error")?;
    ensure!(
        database_error.code().as_deref() == Some("55000"),
        "expected SQLSTATE 55000, got {error}"
    );
    Ok(())
}

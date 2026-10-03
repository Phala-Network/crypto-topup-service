//! Reproducible RPC queue plans and concurrent migration round trips on an isolated database.

mod support;

use anyhow::{Result, ensure};
use sqlx::{Executor, PgPool};
use support::with_database;

const REVIEW: &str = "EXPLAIN (ANALYZE, BUFFERS, FORMAT TEXT) SELECT id,request,answering_member FROM rpc_window_reviews WHERE chain_id=$1 AND epoch=COALESCE((SELECT epoch FROM rpc_chain_state WHERE chain_id=$1),0) AND reviewed_at IS NULL ORDER BY COALESCE(replayed_at,created_at),from_block,id LIMIT 16";
const REORG: &str = "EXPLAIN (ANALYZE, BUFFERS, FORMAT TEXT) UPDATE rpc_reorg_ranges SET replayed_through=LEAST(to_block,$2) WHERE chain_id=$1 AND epoch=COALESCE((SELECT epoch FROM rpc_chain_state WHERE chain_id=$1),0) AND COALESCE(replayed_through+1,from_block)>=$3 AND COALESCE(replayed_through+1,from_block)<=$2 AND COALESCE(replayed_through,from_block-1)<to_block";

async fn plans(pool: &PgPool) -> Result<(String, String)> {
    let review = sqlx::query_scalar::<_, String>(REVIEW)
        .bind(1_i64)
        .fetch_all(pool)
        .await?
        .join("\n");
    // EXPLAIN ANALYZE executes the update; roll it back to keep before/after data identical.
    let mut tx = pool.begin().await?;
    let reorg = sqlx::query_scalar::<_, String>(REORG)
        .bind(1_i64)
        .bind(3_000_000_i64)
        .bind(1_000_000_i64)
        .fetch_all(&mut *tx)
        .await?
        .join("\n");
    tx.rollback().await?;
    Ok((review, reorg))
}

#[tokio::test]
async fn rpc_queue_plans_and_concurrent_migration_round_trip() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
        let pool = &database.owner_pool;
        pool.execute("INSERT INTO rpc_chain_state(chain_id,epoch) SELECT g,1 FROM generate_series(1,4) g").await?;
        pool.execute(r#"
            INSERT INTO rpc_window_reviews(chain_id,group_id,epoch,from_block,to_block,request,
                request_digest,answering_member,end_hash,reviewed_at,created_at,replayed_at)
            SELECT g % 4 + 1,'a',g % 3,g * 100,g * 100 + 99,'{}'::jsonb,g::text,'member','hash',
                CASE WHEN g % 10 <> 0 THEN '2026-01-01'::timestamptz END,
                '2026-01-01'::timestamptz + g * interval '1 second',
                CASE WHEN g % 20 = 0 THEN '2026-01-03'::timestamptz + g * interval '1 second' END
            FROM generate_series(1,120000) g
        "#).await?;
        pool.execute(r#"
            INSERT INTO rpc_reorg_ranges(chain_id,group_id,epoch,from_block,to_block,replayed_through)
            SELECT g % 4 + 1,'a',g % 3,g * 100,g * 100 + 99,
                CASE WHEN g % 10 <> 0 THEN g * 100 + 99
                     WHEN g % 20 = 0 THEN g * 100 + 40 END
            FROM generate_series(1,120000) g
        "#).await?;
        pool.execute("ANALYZE rpc_window_reviews").await?;
        pool.execute("ANALYZE rpc_reorg_ranges").await?;
        topup::db::MIGRATOR.undo(pool,20261027000000).await?;
        let (review_before,reorg_before) = plans(pool).await?;
        // Run through SQLx itself: a missing no-transaction directive fails CREATE INDEX here.
        topup::db::migrate(pool).await?;
        let (review_after,reorg_after) = plans(pool).await?;
        println!("REVIEW BEFORE\n{review_before}\nREVIEW AFTER\n{review_after}\nREORG BEFORE\n{reorg_before}\nREORG AFTER\n{reorg_after}");
        ensure!(review_after.contains("Index Scan using rpc_window_reviews_due_idx"), "{review_after}");
        ensure!(!review_after.contains("Sort"), "review queue still sorts: {review_after}");
        ensure!(reorg_after.contains("rpc_reorg_ranges_pending_idx"), "{reorg_after}");
        let indexes: Vec<(String,bool)> = sqlx::query_as(
            "SELECT c.relname,i.indisvalid FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid WHERE c.relname IN ('rpc_window_reviews_due_idx','rpc_reorg_ranges_pending_idx','rpc_window_reviews_pending')"
        ).fetch_all(pool).await?;
        ensure!(indexes.len()==2 && indexes.iter().all(|(_,valid)| *valid), "{indexes:?}");
        Ok(())
        })
    })
    .await
}

fn migrate_cli(url: &str) -> Result<std::process::Output> {
    Ok(std::process::Command::new(env!("CARGO_BIN_EXE_topup"))
        .arg("migrate")
        .env("DATABASE_URL", url)
        .output()?)
}

fn cli_text(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Normal Deploy's topup migrate command repairs only a matching invalid pending queue index,
/// then runs the original migration. Both concurrent builds are exercised through the CLI.
#[tokio::test]
async fn interrupted_concurrent_builds_recover_through_the_migrate_cli() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let pool = &database.owner_pool;
            for (target,version,name,write) in [
                (20261027000000,20261028000000_i64,"rpc_window_reviews_due_idx",
                 "INSERT INTO rpc_window_reviews(chain_id,group_id,from_block,to_block,request,request_digest,answering_member,end_hash) VALUES(1,'a',1,2,'{}','blocked','member','hash')"),
                (20261028000001,20261028000002_i64,"rpc_reorg_ranges_pending_idx",
                 "INSERT INTO rpc_reorg_ranges(chain_id,group_id,epoch,from_block,to_block) VALUES(1,'a',0,1,2)"),
            ] {
                topup::db::MIGRATOR.undo(pool,target).await?;
                let mut writer = pool.begin().await?;
                sqlx::query(write).execute(&mut *writer).await?;
                // Exercise the production migrate budget, which overrides URL timeout options.
                let failed = migrate_cli(&database.owner_url)?;
                ensure!(!failed.status.success() && cli_text(&failed).contains("lock timeout"),"{}",cli_text(&failed));
                let (valid,oid): (bool,i64) = sqlx::query_as(
                    "SELECT i.indisvalid,c.oid::bigint FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relname=$1"
                ).bind(name).fetch_one(pool).await?;
                ensure!(!valid,"failed build did not leave invalid {name}");
                let recorded: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations WHERE version=$1")
                    .bind(version).fetch_one(pool).await?;
                ensure!(recorded==0,"failed migration was recorded");
                writer.rollback().await?;
                let retry = migrate_cli(&database.owner_url)?;
                ensure!(retry.status.success(),"{}",cli_text(&retry));
                ensure!(cli_text(&retry).contains("removing invalid index"),"cleanup was not reported");
                let (valid,new_oid): (bool,i64) = sqlx::query_as(
                    "SELECT i.indisvalid,c.oid::bigint FROM pg_index i JOIN pg_class c ON c.oid=i.indexrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='public' AND c.relname=$1"
                ).bind(name).fetch_one(pool).await?;
                ensure!(valid && new_oid!=oid,"{name} was skipped instead of rebuilt");
                let retry = migrate_cli(&database.owner_url)?;
                ensure!(retry.status.success(),"idempotent retry failed: {}",cli_text(&retry));
            }
            Ok(())
        })
    }).await
}

/// Valid completed builds retain their OID; unrelated objects with a reserved name fail closed.
#[tokio::test]
async fn migration_recovery_preserves_valid_and_unrelated_indexes() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let pool = &database.owner_pool;
            topup::db::MIGRATOR.undo(pool,20261027000000).await?;
            pool.execute(include_str!("../migrations/20261028000000_rpc_review_queue.up.sql")).await?;
            let oid: i64 = sqlx::query_scalar("SELECT 'public.rpc_window_reviews_due_idx'::regclass::oid::bigint").fetch_one(pool).await?;
            let completed = migrate_cli(&database.owner_url)?;
            ensure!(completed.status.success(),"{}",cli_text(&completed));
            let kept: i64 = sqlx::query_scalar("SELECT 'public.rpc_window_reviews_due_idx'::regclass::oid::bigint").fetch_one(pool).await?;
            ensure!(kept==oid,"valid index was replaced");
            topup::db::MIGRATOR.undo(pool,20261027000000).await?;
            pool.execute("CREATE INDEX rpc_window_reviews_due_idx ON rpc_reorg_ranges(chain_id)").await?;
            let oid: i64 = sqlx::query_scalar("SELECT 'public.rpc_window_reviews_due_idx'::regclass::oid::bigint").fetch_one(pool).await?;
            let unrelated = migrate_cli(&database.owner_url)?;
            ensure!(!unrelated.status.success() && cli_text(&unrelated).contains("refusing recovery"),"{}",cli_text(&unrelated));
            let kept: i64 = sqlx::query_scalar("SELECT 'public.rpc_window_reviews_due_idx'::regclass::oid::bigint").fetch_one(pool).await?;
            ensure!(kept==oid,"unrelated index was replaced");
            pool.execute("DROP INDEX public.rpc_window_reviews_due_idx").await?;
            let mut writer = pool.begin().await?;
            writer.execute("INSERT INTO rpc_reorg_ranges(chain_id,group_id,epoch,from_block,to_block) VALUES(1,'a',0,1,2)").await?;
            let mut connection = pool.acquire().await?;
            connection.execute("SET lock_timeout='100ms'").await?;
            let failed = connection.execute("CREATE INDEX CONCURRENTLY rpc_window_reviews_due_idx ON rpc_reorg_ranges(chain_id)").await;
            ensure!(failed.is_err());
            writer.rollback().await?;
            drop(connection);
            let unrelated = migrate_cli(&database.owner_url)?;
            ensure!(!unrelated.status.success() && cli_text(&unrelated).contains("refusing recovery"),"{}",cli_text(&unrelated));
            let valid: bool = sqlx::query_scalar("SELECT indisvalid FROM pg_index WHERE indexrelid='public.rpc_window_reviews_due_idx'::regclass").fetch_one(pool).await?;
            ensure!(!valid,"unrelated invalid index was changed");
            // Missing bookkeeping must not allow CREATE IF NOT EXISTS to bypass inspection.
            let oid: i64 = sqlx::query_scalar("SELECT 'public.rpc_window_reviews_due_idx'::regclass::oid::bigint").fetch_one(pool).await?;
            pool.execute("DROP TABLE public._sqlx_migrations").await?;
            let missing_history = topup::db::migrate(pool).await;
            ensure!(missing_history.is_err_and(|error| error.to_string().contains("inconsistent migration history")));
            let kept: i64 = sqlx::query_scalar("SELECT 'public.rpc_window_reviews_due_idx'::regclass::oid::bigint").fetch_one(pool).await?;
            ensure!(kept==oid,"index without migration history was changed");
            Ok(())
        })
    }).await
}

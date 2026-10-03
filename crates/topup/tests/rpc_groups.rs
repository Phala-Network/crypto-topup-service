//! Durable coverage, atomic progress and wrong-watermark recovery acceptance tests.
mod support;
use alloy_primitives::{Address, B256};
use anyhow::{Result, ensure};
use sqlx::Row;
use support::{TestDatabase, seed};
use topup::{db, routes::RouteSet};
use topup_adapters::chain::evm::{
    group::{HeadAnchor, WatermarkStore},
    window::{WindowProof, WindowRequest},
};
use uuid::Uuid;
fn proof(member: &str, from: u64, to: u64) -> WindowProof {
    WindowProof {
        group: "a".into(),
        member: member.into(),
        request: WindowRequest {
            from,
            to,
            recipients: Vec::new(),
            tokens: Vec::new(),
            factory: None,
            finalized: true,
            exclude_member: None,
        },
        end_hash: format!("0x{}", "11".repeat(32)),
    }
}
#[tokio::test]
async fn historical_coverage_survives_a_moving_tail_and_progress_is_atomic() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let pool = &database.app_pool;
        let old = proof("old", 1, 2000);
        let tail = proof("new", 100_001, 102_000);
        db::rpc::commit_window(
            pool,
            1,
            &[],
            &[],
            Some(&old),
            db::rpc::WindowProgress {
                scanned: Some((2000, None)),
                reconciliation: Some((None, 2001)),
                ..Default::default()
            },
        )
        .await?;
        db::rpc::commit_window(
            pool,
            1,
            &[],
            &[],
            Some(&tail),
            db::rpc::WindowProgress {
                scanned: Some((102_000, None)),
                ..Default::default()
            },
        )
        .await?;
        let pending = db::rpc::due_reviews(pool, 1).await?;
        ensure!(
            pending.iter().any(|(_, r, m)| r.from == 1 && m == "old"),
            "old catch-up window lost review coverage"
        );
        let id = pending
            .iter()
            .find(|(_, r, _)| r.from == 1)
            .expect("old coverage")
            .0;
        let mut reviewed = old.clone();
        reviewed.member = "independent".into();
        reviewed.request.exclude_member = Some("old".into());
        db::rpc::commit_window(
            pool,
            1,
            &[],
            &[],
            Some(&reviewed),
            db::rpc::WindowProgress {
                reviewed: Some(id),
                ..Default::default()
            },
        )
        .await?;
        ensure!(
            !db::rpc::due_reviews(pool, 1)
                .await?
                .iter()
                .any(|(i, _, _)| i == &id)
        );
        let cursor = db::get_cursor(pool, 1).await?;
        let refused = db::rpc::commit_window(
            pool,
            1,
            &[],
            &[],
            Some(&proof("new", 102_001, 104_000)),
            db::rpc::WindowProgress {
                scanned: Some((104_000, None)),
                reconciliation: Some((Some(99), 104_001)),
                ..Default::default()
            },
        )
        .await;
        ensure!(refused.is_err());
        ensure!(
            db::get_cursor(pool, 1).await? == cursor,
            "failed atomic window advanced scanner"
        );
        let recon: i64 = sqlx::query_scalar(
            "SELECT next_block FROM reconciliation_deposit_cursors WHERE chain_id=1",
        )
        .fetch_one(pool)
        .await?;
        ensure!(recon == 2001, "failed atomic window advanced reconciler");
        Ok(())
    }
    .await;
    database.cleanup().await?;
    result
}
#[tokio::test]
async fn recovery_repairs_poisoned_address_progress_and_preserves_audit() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result=async {
        let pool=&database.app_pool;
        let (_,customer)=seed::create_account_and_customer(pool,&seed::NewAccount::named("rpc recovery"),"customer").await?;
        let id=Uuid::new_v4();seed::insert_address(pool,&seed::NewAddress {id,customer_id:customer.id,chain_id:1,route:"phala-cloud-pha-usd".into(),salt:B256::repeat_byte(3),address:Address::repeat_byte(3)}).await?;
        sqlx::query("UPDATE addresses SET created_block=9000,backfilled=true,backfilled_through=10000 WHERE id=$1").bind(id).execute(pool).await?;
        db::initialize_cursor(pool,1,10_000,chrono::Utc::now()).await?;
        let state=db::rpc::state(pool,"config".into());let wrong=HeadAnchor {number:10_000,hash:format!("0x{}","22".repeat(32)),parent_hash:format!("0x{}","33".repeat(32))};
        state.accept(1,"a","finalized","bad",&wrong).await?;state.freeze(1).await?;
        let correct=HeadAnchor {number:100,hash:format!("0x{}","11".repeat(32)),parent_hash:format!("0x{}","00".repeat(32))};
        db::rpc::recover(pool,1,&correct,"reviewed operator","wrong accepted head").await?;
        let row=sqlx::query("SELECT created_block,backfilled,backfilled_through FROM addresses WHERE id=$1").bind(id).fetch_one(pool).await?;
        ensure!(row.try_get::<i64,_>("created_block")?==0);ensure!(!row.try_get::<bool,_>("backfilled")?);ensure!(row.try_get::<Option<i64>,_>("backfilled_through")?.is_none());
        ensure!(db::get_confirmed_cursor(pool,1).await?.is_none());ensure!(state.blocked(1).await.is_err());
        let evidence:serde_json::Value=sqlx::query_scalar("SELECT evidence FROM rpc_recoveries WHERE chain_id=1").fetch_one(pool).await?;
        ensure!(evidence.to_string().contains("9000"));
        // Runtime cannot mutate/delete the recovery audit.
        ensure!(sqlx::query("DELETE FROM rpc_recoveries").execute(pool).await.is_err());
        Ok(())
    }.await;
    database.cleanup().await?;
    result
}
#[test]
fn singleton_staging_migration_preserves_roles_and_rejects_old_schema() -> Result<()> {
    let config = topup::config::Config::parse(include_str!(
        "../../../deploy/environments/phala-network/staging/topup/topup.yaml"
    ))
    .map_err(anyhow::Error::msg)?;
    for route in &config.routes {
        for (role, id) in route.chain.rpc_providers.iter().enumerate() {
            let group = &config.rpc_groups[id];
            ensure!(group.members.len() == 1);
            ensure!(group.members[0].company == if role == 0 { "tenderly" } else { "publicnode" });
        }
    }
    let route: topup_core::route::RouteFile =
        serde_saphyr::from_str(include_str!("fixtures/phala-cloud-pha.yaml"))?;
    let mut bad = route.clone();
    bad.chain.rpc_providers.push("silently-dropped".into());
    ensure!(RouteSet::new(vec![bad]).is_err());
    let legacy = include_str!("fixtures/phala-cloud-pha.yaml").replace(
        "rpc_groups: { a: alchemy, b: quicknode }",
        "rpc_providers: [alchemy, quicknode, spare]",
    );
    ensure!(serde_saphyr::from_str::<topup_core::route::RouteFile>(&legacy).is_err());
    Ok(())
}

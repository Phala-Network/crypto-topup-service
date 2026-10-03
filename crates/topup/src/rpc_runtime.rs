//! Executable per-member acceptance, recovery probing and height-only cursor anchoring.
use crate::{db, routes::RouteSet};
use serde_json::json;
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use topup_adapters::chain::evm::{
    EvmClient,
    group::{HeadAnchor, RpcGroup, WatermarkStore},
};
use topup_core::route::RouteFile;

async fn probe(
    group: &Arc<RpcGroup>,
    index: usize,
    routes: &[&RouteFile],
) -> Result<String, String> {
    tokio::time::timeout(
        Duration::from_millis(group.policy.total_deadline_ms),
        probe_inner(group, index, routes),
    )
    .await
    .map_err(|_| "RPC preflight deadline".to_owned())?
}
async fn probe_inner(
    group: &Arc<RpcGroup>,
    index: usize,
    routes: &[&RouteFile],
) -> Result<String, String> {
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(group.policy.total_deadline_ms))
        .unwrap_or_else(Instant::now);
    let chain = group
        .send(
            index,
            &json!({"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}),
            deadline,
        )
        .await
        .map_err(|e| e.to_string())?;
    if chain.get("result").and_then(serde_json::Value::as_str)
        != Some(format!("0x{:x}", group.chain).as_str())
    {
        return Err("RPC chain identity mismatch".to_owned());
    }
    let genesis = block(group, index, 0, deadline).await?;
    let client = EvmClient::from_group(group.clone(), Some(index)).map_err(|e| e.to_string())?;
    for route in routes {
        crate::contracts::verify_on(&client, route).await?;
    }
    group
        .head(index, "latest", deadline)
        .await
        .map_err(|e| e.to_string())?;
    group
        .head(index, "finalized", deadline)
        .await
        .map_err(|e| e.to_string())?;
    let head = group
        .head(index, "finalized", deadline)
        .await
        .map_err(|e| e.to_string())?;
    let logs = json!({"jsonrpc":"2.0","id":1,"method":"eth_getLogs","params":[{"fromBlock":format!("0x{:x}",head.number.saturating_sub(1999)),"toBlock":format!("0x{:x}",head.number),"topics":["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",null,[format!("0x{}","00".repeat(32))]]}]});
    group
        .send_logs(index, &logs, deadline)
        .await
        .map_err(|e| e.to_string())?;
    for route in routes {
        for contract in [route.asset.contract, route.screening.sanctions_oracle] {
            if client
                .code_at(contract)
                .await
                .map_err(|e| e.to_string())?
                .is_empty()
            {
                return Err("RPC route token/oracle code unavailable".to_owned());
            }
        }
    }
    Ok(genesis.hash)
}
async fn block(
    group: &RpcGroup,
    index: usize,
    number: u64,
    deadline: Instant,
) -> Result<HeadAnchor, String> {
    let v=group.send(index,&json!({"jsonrpc":"2.0","id":1,"method":"eth_getBlockByNumber","params":[format!("0x{number:x}"),false]}),deadline).await.map_err(|e|e.to_string())?;
    HeadAnchor::parse(v.get("result").ok_or("missing block")?).map_err(|e| e.to_string())
}
type Groups<'a> = BTreeMap<String, (Arc<RpcGroup>, Vec<&'a RouteFile>)>;
fn groups(routes: &RouteSet) -> Result<Groups<'_>, String> {
    let mut result: BTreeMap<String, (Arc<RpcGroup>, Vec<&RouteFile>)> = BTreeMap::new();
    for route in routes.routes() {
        for role in 0..2 {
            let client = routes
                .provider(route.chain.chain_id, role)
                .map_err(|e| e.to_string())?;
            if let Some(group) = client.group() {
                result
                    .entry(group.id.clone())
                    .or_insert_with(|| (group.clone(), Vec::new()))
                    .1
                    .push(route);
            }
        }
    }
    Ok(result)
}
/// First acceptance needs one fully verified member in each group; offline backups do not veto it.
/// The exact accepted digest may restart with no serving members, without issuing or crediting on
/// unavailable evidence. Every returning member still undergoes complete verification.
pub async fn accept(pool: &PgPool, routes: &RouteSet, public_config: &str) -> Result<(), String> {
    let digest = db::rpc::digest(public_config);
    let accepted: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM rpc_config_acceptances WHERE config_digest=$1)",
    )
    .bind(&digest)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;
    let groups = groups(routes)?;
    let mut genesis = BTreeMap::new();
    let mut validations = Vec::new();
    for (group, route_files) in groups.values() {
        for index in 0..group.members.len() {
            match probe(group, index, route_files).await {
                Ok(hash) => {
                    let previous: Option<String> = sqlx::query_scalar(
                        "SELECT genesis_hash FROM rpc_member_validations WHERE chain_id=$1 LIMIT 1",
                    )
                    .bind(i64::try_from(group.chain).map_err(|e| e.to_string())?)
                    .fetch_optional(pool)
                    .await
                    .map_err(|e| e.to_string())?;
                    if previous.as_ref().is_some_and(|h| h != &hash)
                        || genesis
                            .insert(group.chain, hash.clone())
                            .is_some_and(|h| h != hash)
                    {
                        return Err("RPC A/B or persisted genesis mismatch".to_owned());
                    }
                    group.verified(index, true);
                    validations.push((group.clone(), index, hash));
                }
                Err(error) => {
                    group.verified(index, false);
                    tracing::warn!(group=%group.id,member=%group.members.get(index).map(|m|m.id.as_str()).unwrap_or("unknown"),%error,"RPC member preflight failed");
                }
            }
        }
        if !accepted && group.eligible() == 0 {
            return Err(format!(
                "RPC group {} has no verified member for first acceptance",
                group.id
            ));
        }
    }
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query(
        "INSERT INTO rpc_config_acceptances(config_digest) VALUES($1) ON CONFLICT DO NOTHING",
    )
    .bind(&digest)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    for (group, index, hash) in validations {
        let member = group.members.get(index).ok_or("missing RPC member")?;
        sqlx::query("INSERT INTO rpc_member_validations(config_digest,group_id,member_id,chain_id,genesis_hash) VALUES($1,$2,$3,$4,$5) ON CONFLICT(config_digest,group_id,member_id) DO UPDATE SET validated_at=now()").bind(&digest).bind(&group.id).bind(&member.id).bind(i64::try_from(group.chain).map_err(|e|e.to_string())?).bind(hash).execute(&mut *tx).await.map_err(|e|e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    let state = db::rpc::state(pool, digest);
    for (group, _) in groups.values() {
        group.set_store(state.clone());
    }
    anchor_cursors(pool, routes, &state).await
}
async fn anchor_cursors(
    pool: &PgPool,
    routes: &RouteSet,
    state: &Arc<db::rpc::RpcState>,
) -> Result<(), String> {
    for chain in routes.chain_ids() {
        let a = routes
            .provider(chain, 0)
            .map_err(|e| e.to_string())?
            .group()
            .ok_or("RPC A group missing")?;
        let b = routes
            .provider(chain, 1)
            .map_err(|e| e.to_string())?
            .group()
            .ok_or("RPC B group missing")?;
        if state
            .load(chain, &a.id, "cursor")
            .await
            .map_err(|e| e.to_string())?
            .is_some()
        {
            continue;
        }
        let Some(height) = db::get_cursor(pool, chain)
            .await
            .map_err(|e| e.to_string())?
        else {
            continue;
        };
        let (Ok(ai), Ok(bi)) = (
            a.select(&Default::default(), None),
            b.select(&Default::default(), None),
        ) else {
            continue;
        };
        let deadline = Instant::now()
            + Duration::from_millis(a.policy.total_deadline_ms.min(b.policy.total_deadline_ms));
        if a.head(ai, "finalized", deadline)
            .await
            .map_err(|e| e.to_string())?
            .number
            < height
            || b.head(bi, "finalized", deadline)
                .await
                .map_err(|e| e.to_string())?
                .number
                < height
        {
            state.freeze(chain).await.map_err(|e| e.to_string())?;
            return Err(
                "legacy cursor exceeds agreed finalized evidence; audited recovery required"
                    .to_owned(),
            );
        }
        let ah = block(a, ai, height, deadline).await?;
        let bh = block(b, bi, height, deadline).await?;
        if ah != bh {
            state.freeze(chain).await.map_err(|e| e.to_string())?;
            return Err("height-only cursor has no agreed A/B hash anchor".to_owned());
        }
        for (group, index) in [(a, ai), (b, bi)] {
            state
                .accept(
                    chain,
                    &group.id,
                    "cursor",
                    &group.members.get(index).ok_or("RPC member missing")?.id,
                    &ah,
                )
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
/// Cooldown expiry only schedules probes; full identity/contracts/heads are checked before readmit.
pub async fn recover_members(
    pool: PgPool,
    routes: Arc<RouteSet>,
    cancellation: CancellationToken,
) -> Result<(), String> {
    let groups = groups(&routes)?;
    loop {
        tokio::select! {()=cancellation.cancelled()=>return Ok(()),()=tokio::time::sleep(Duration::from_secs(5))=>{}}
        db::rpc::refresh_metrics(&pool)
            .await
            .map_err(|e| e.to_string())?;
        for (group, route_files) in groups.values() {
            for index in 0..group.members.len() {
                if group.probe_due(index) {
                    let result = probe(group, index, route_files).await;
                    let known: Option<String> = sqlx::query_scalar(
                        "SELECT genesis_hash FROM rpc_member_validations WHERE chain_id=$1 LIMIT 1",
                    )
                    .bind(i64::try_from(group.chain).map_err(|e| e.to_string())?)
                    .fetch_optional(&pool)
                    .await
                    .map_err(|e| e.to_string())?;
                    group.probe_result(
                        index,
                        result.is_ok_and(|hash| known.as_ref() == Some(&hash)),
                    );
                }
            }
        }
    }
}

/// Per-member network preflight, returning only stable ids. An offline backup is reported and
/// left unverified; at least one fully validated member must serve every group.
pub async fn preflight(routes: &RouteSet) -> Result<Vec<String>, String> {
    let mut healthy = Vec::new();
    let mut genesis = BTreeMap::new();
    for (group, files) in groups(routes)?.values() {
        for index in 0..group.members.len() {
            if let Ok(hash) = probe(group, index, files).await {
                if genesis
                    .insert(group.chain, hash.clone())
                    .is_some_and(|old| old != hash)
                {
                    return Err("RPC A/B genesis disagreement".into());
                }
                group.verified(index, true);
                healthy.push(group.members.get(index).ok_or("missing member")?.id.clone());
            }
        }
        if group.eligible() == 0 {
            return Err(format!("RPC group {} has no verified member", group.id));
        }
    }
    Ok(healthy)
}
/// Audited owner recovery verifies an agreed lower finalized anchor and repairs derived floors.
/// No runtime operation can lower a persisted watermark; this entry requires stopped writers.
pub async fn recover_watermark(
    pool: &PgPool,
    routes: &RouteSet,
    chain: u64,
    height: u64,
    actor: &str,
    reason: &str,
) -> Result<(), String> {
    let lock = crate::reconciler::store::exclusive_lease_owner_lock(pool)
        .await
        .map_err(|e| e.to_string())?;
    let result = async {
        preflight(routes).await?;
        let a = routes
            .provider(chain, 0)
            .map_err(|e| e.to_string())?
            .group()
            .ok_or("A group missing")?;
        let b = routes
            .provider(chain, 1)
            .map_err(|e| e.to_string())?
            .group()
            .ok_or("B group missing")?;
        let ai = a
            .select(&Default::default(), None)
            .map_err(|e| e.to_string())?;
        let bi = b
            .select(&Default::default(), None)
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(
                a.policy.total_deadline_ms.min(b.policy.total_deadline_ms),
            ))
            .unwrap_or_else(Instant::now);
        if a.head(ai, "finalized", deadline)
            .await
            .map_err(|e| e.to_string())?
            .number
            < height
            || b.head(bi, "finalized", deadline)
                .await
                .map_err(|e| e.to_string())?
                .number
                < height
        {
            return Err("recovery anchor above finalized evidence".into());
        }
        let ah = block(a, ai, height, deadline).await?;
        let bh = block(b, bi, height, deadline).await?;
        if ah != bh || ah.number != height {
            return Err("RPC recovery requires A/B anchor agreement".into());
        }
        db::rpc::recover(pool, chain, &ah, actor, reason)
            .await
            .map_err(|e| e.to_string())
    }
    .await;
    lock.release().await.map_err(|e| e.to_string())?;
    result
}
/// Bounded stopped-service replay. Repeated invocations retain atomic address progress. Unfreeze
/// occurs only after all address backfills and every credited deposit's canonical block check.
pub async fn resume_recovery(
    pool: &PgPool,
    routes: &RouteSet,
    chain: u64,
    max_windows: u32,
) -> Result<bool, String> {
    use sqlx::Row;
    use topup_adapters::chain::evm::{ChainReader, FinalizedReader};
    let lock = crate::reconciler::store::exclusive_lease_owner_lock(pool)
        .await
        .map_err(|e| e.to_string())?;
    let result=async {
        let pending:bool=sqlx::query_scalar("SELECT frozen AND recovery_pending FROM rpc_chain_state WHERE chain_id=$1").bind(i64::try_from(chain).map_err(|e|e.to_string())?).fetch_one(pool).await.map_err(|e|e.to_string())?;
        if !pending {return Err("an audited recovery must be pending before replay/resume".to_owned());}
        preflight(routes).await?;
        let a=routes.provider(chain,0).map_err(|e|e.to_string())?.group().ok_or("A group missing")?;
        let b=routes.provider(chain,1).map_err(|e|e.to_string())?.group().ok_or("B group missing")?;
        let ai=a.select(&Default::default(),None).map_err(|e|e.to_string())?;let bi=b.select(&Default::default(),None).map_err(|e|e.to_string())?;
        let chain_db=i64::try_from(chain).map_err(|e|e.to_string())?;
        let evidence:serde_json::Value=sqlx::query_scalar("SELECT evidence FROM rpc_recoveries WHERE chain_id=$1 ORDER BY epoch DESC LIMIT 1").bind(chain_db).fetch_one(pool).await.map_err(|e|e.to_string())?;
        let anchor:HeadAnchor=serde_json::from_value(evidence.get("anchor").ok_or("missing audited recovery anchor")?.clone()).map_err(|e|e.to_string())?;
        let deadline=Instant::now().checked_add(Duration::from_millis(a.policy.total_deadline_ms.min(b.policy.total_deadline_ms))).unwrap_or_else(Instant::now);
        if block(a,ai,anchor.number,deadline).await?!=anchor||block(b,bi,anchor.number,deadline).await?!=anchor {return Err("audited anchor no longer agrees with A/B".into());}
        let reader=FinalizedReader::new(routes.provider(chain,0).map_err(|e|e.to_string())?.clone());
        let chain_routes=crate::scanner::chain_routes(routes).into_iter().find(|r|r.chain.chain_id==chain).ok_or("chain routes missing")?;
        let addresses=db::list_scan_addresses(pool,chain).await.map_err(|e|e.to_string())?;
        let from=addresses.iter().filter(|a|!a.backfilled).map(crate::db::ScanAddress::backfill_start).min();
        if let Some(mut from)=from {for _ in 0..max_windows {
            if from>anchor.number {break;}
            let to=from.saturating_add(1999).min(anchor.number);
            let selected=addresses.iter().filter(|a|!a.backfilled&&a.backfill_start()<=to).cloned().collect::<Vec<_>>();
            let request=crate::scanner::window_request(&chain_routes,&selected,from,to,true);
            let window=reader.read_window(&request).await.map_err(|e|e.to_string())?;
            let deposits=crate::scanner::resolve_logs_for_reconciliation(window.transfers,&selected,&chain_routes).map_err(|e|e.to_string())?;
            db::rpc::recovery_window(pool,chain,&deposits,&window.factory_logs,window.proof.as_ref().ok_or("missing replay provenance")?,&selected.iter().map(|a|a.id).collect::<Vec<_>>(),to).await.map_err(|e|e.to_string())?;
            from=to.checked_add(1).ok_or("replay block overflow")?;
        }}
        let incomplete:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM addresses WHERE chain_id=$1 AND NOT backfilled AND (backfilled_through IS NULL OR backfilled_through<$2))").bind(chain_db).bind(i64::try_from(anchor.number).map_err(|e|e.to_string())?).fetch_one(pool).await.map_err(|e|e.to_string())?;
        if incomplete {return Ok(false);}
        for row in sqlx::query("SELECT block_number,block_hash FROM deposits WHERE chain_id=$1 AND credit_minor IS NOT NULL AND state<>'reversed'").bind(chain_db).fetch_all(pool).await.map_err(|e|e.to_string())? {
            let number=u64::try_from(row.try_get::<i64,_>("block_number").map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
            let hash:String=row.try_get("block_hash").map_err(|e|e.to_string())?;
            let deadline=Instant::now().checked_add(Duration::from_millis(a.policy.total_deadline_ms.min(b.policy.total_deadline_ms))).unwrap_or_else(Instant::now);
            let ah=block(a,ai,number,deadline).await?;let bh=block(b,bi,number,deadline).await?;
            if ah!=bh||ah.hash!=hash {return Err("credited deposit branch requires explicit ledger reconciliation; chain stays frozen".into());}
        }
        let mut tx=pool.begin().await.map_err(|e|e.to_string())?;
        sqlx::query("UPDATE addresses SET backfilled=true WHERE chain_id=$1").bind(chain_db).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        sqlx::query("UPDATE rpc_chain_state SET frozen=false,recovery_pending=false,reason=NULL WHERE chain_id=$1 AND frozen AND recovery_pending").bind(chain_db).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        tx.commit().await.map_err(|e|e.to_string())?;Ok(true)
    }.await;
    lock.release().await.map_err(|e| e.to_string())?;
    result
}

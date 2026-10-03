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
    let copy = group.probe_copy().map_err(|e| e.to_string())?;
    let result = tokio::time::timeout(
        Duration::from_millis(group.policy.total_deadline_ms),
        async {
            let hash = probe_inner(&copy, index, routes).await?;
            group
                .validate_probe(index, &copy)
                .await
                .map_err(|e| e.to_string())?;
            Ok(hash)
        },
    )
    .await
    .map_err(|_| "RPC preflight deadline".to_owned())?;
    if copy.quarantined(index) {
        group.failed(index, topup_adapters::chain::evm::group::Failure::Identity);
    }
    result
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
        group.failed(index, topup_adapters::chain::evm::group::Failure::Identity);
        return Err("RPC chain identity mismatch".to_owned());
    }
    let genesis = block(group, index, 0, deadline).await?;
    let client = EvmClient::from_group(group.clone(), Some(index)).map_err(|e| e.to_string())?;
    let mut contracts = std::collections::BTreeSet::new();
    for route in routes {
        if contracts.insert((
            route.chain.contracts.forwarder_factory,
            route.chain.contracts.implementation,
        )) {
            crate::contracts::verify_on(&client, route).await?;
        }
    }
    group
        .head(index, "latest", deadline)
        .await
        .map_err(|e| e.to_string())?;
    group
        .head(index, "finalized", deadline)
        .await
        .map_err(|e| e.to_string())?;
    group
        .head(index, "safe", deadline)
        .await
        .map_err(|e| e.to_string())?;
    group.send(index,&json!({"jsonrpc":"2.0","id":1,"method":"eth_getTransactionReceipt","params":[format!("0x{}","00".repeat(32))]}),deadline).await.map_err(|e|e.to_string())?;
    let head = group
        .head(index, "finalized", deadline)
        .await
        .map_err(|e| e.to_string())?;
    if routes
        .iter()
        .any(|r| r.chain.rpc_providers.first() == Some(&group.id))
    {
        let logs = json!({"jsonrpc":"2.0","id":1,"method":"eth_getLogs","params":[{"fromBlock":format!("0x{:x}",head.number.saturating_sub(1999)),"toBlock":format!("0x{:x}",head.number),"topics":["0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",null,[format!("0x{}","00".repeat(32))]]}]});
        group
            .send_logs(index, &logs, deadline)
            .await
            .map_err(|e| e.to_string())?;
    }
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
    for route in routes {
        let decimals = client
            .call(
                "RPC token capability",
                route.asset.contract,
                alloy_primitives::Bytes::from_static(&[0x31, 0x3c, 0xe5, 0x67]),
                Some(head.number.into()),
            )
            .await
            .map_err(|e| e.to_string())?;
        if decimals.len() != 32
            || alloy_primitives::U256::from_be_slice(&decimals)
                != alloy_primitives::U256::from(route.asset.decimals)
        {
            return Err("RPC token decimals mismatch".to_owned());
        }
        let hash = alloy_primitives::keccak256("isSanctioned(address)");
        let mut data = hash
            .as_slice()
            .get(..4)
            .ok_or("oracle selector missing")?
            .to_vec();
        data.extend_from_slice(&[0u8; 32]);
        let answer = client
            .call(
                "RPC oracle capability",
                route.screening.sanctions_oracle,
                data.into(),
                Some(head.number.into()),
            )
            .await
            .map_err(|e| e.to_string())?;
        if answer.len() != 32
            || alloy_primitives::U256::from_be_slice(&answer) > alloy_primitives::U256::from(1)
        {
            return Err("RPC oracle capability malformed".to_owned());
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
    let anchor =
        HeadAnchor::parse(v.get("result").ok_or("missing block")?).map_err(|e| e.to_string())?;
    if anchor.number != number {
        return Err("RPC returned a different numeric block".into());
    }
    Ok(anchor)
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
    let state = db::rpc::state(pool, digest.clone());
    for (group, _) in groups.values() {
        group.set_store(state.clone());
    }
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
                        || genesis.get(&group.chain).is_some_and(|h| h != &hash)
                    {
                        group.failed(index, topup_adapters::chain::evm::group::Failure::Identity);
                        tracing::warn!(group=%group.id,member=%group.members.get(index).map(|m|m.id.as_str()).unwrap_or("unknown"), "RPC genesis mismatch; member quarantined");
                        continue;
                    }
                    genesis.insert(group.chain, hash.clone());
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
    anchor_cursors(pool, routes, state.as_ref()).await?;
    for (group, _) in groups.values() {
        group.set_store(state.clone());
    }
    Ok(())
}
/// Standalone reconciliation/restore must establish the same trusted A/B cursor floors.
pub async fn ensure_anchors(pool: &PgPool, routes: &RouteSet) -> Result<(), String> {
    if !routes.has_rpc_groups() {
        return Ok(());
    }
    let configured = groups(routes)?;
    let Some((group, _)) = configured.values().next() else {
        return Ok(());
    };
    let state = group
        .watermark_store()
        .ok_or("RPC durable safety store missing")?;
    anchor_cursors(pool, routes, state.as_ref()).await
}
async fn anchor_cursors(
    pool: &PgPool,
    routes: &RouteSet,
    state: &dyn WatermarkStore,
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
        let saved_a = state.load(chain, &a.id, "cursor").await;
        let saved_b = state.load(chain, &b.id, "cursor").await;
        match (saved_a, saved_b) {
            (Ok(Some(a)), Ok(Some(b))) if a == b => {
                sqlx::query("UPDATE rpc_chain_state SET awaiting_anchor=false WHERE chain_id=$1")
                    .bind(i64::try_from(chain).map_err(|e| e.to_string())?)
                    .execute(pool)
                    .await
                    .map_err(|e| e.to_string())?;
                continue;
            }
            (Err(topup_adapters::chain::evm::group::Failure::Fork), _)
            | (_, Err(topup_adapters::chain::evm::group::Failure::Fork)) => continue,
            (Err(e), _) | (_, Err(e)) => return Err(e.to_string()),
            _ => {}
        }
        sqlx::query("INSERT INTO rpc_chain_state(chain_id,awaiting_anchor) VALUES($1,true) ON CONFLICT(chain_id) DO UPDATE SET awaiting_anchor=true").bind(i64::try_from(chain).map_err(|e|e.to_string())?).execute(pool).await.map_err(|e|e.to_string())?;
        let (Ok(ai), Ok(bi)) = (
            a.select(&Default::default(), None),
            b.select(&Default::default(), None),
        ) else {
            sqlx::query("INSERT INTO rpc_chain_state(chain_id,awaiting_anchor) VALUES($1,true) ON CONFLICT(chain_id) DO UPDATE SET awaiting_anchor=true").bind(i64::try_from(chain).map_err(|e|e.to_string())?).execute(pool).await.map_err(|e|e.to_string())?;
            continue;
        };
        let ap = a.probe_copy().map_err(|e| e.to_string())?;
        let bp = b.probe_copy().map_err(|e| e.to_string())?;
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(
                a.policy.total_deadline_ms.min(b.policy.total_deadline_ms),
            ))
            .unwrap_or_else(Instant::now);
        let ahead = ap
            .head(ai, "finalized", deadline)
            .await
            .map_err(|e| e.to_string())?;
        let bhead = bp
            .head(bi, "finalized", deadline)
            .await
            .map_err(|e| e.to_string())?;
        let old = db::get_cursor(pool, chain)
            .await
            .map_err(|e| e.to_string())?;
        let restoring = crate::restore_mode::detect(pool)
            .await
            .map_err(|e| e.to_string())?
            .is_some();
        let height = old.unwrap_or(if restoring {
            0
        } else {
            ahead.number.min(bhead.number)
        });
        if height > ahead.number.min(bhead.number) {
            state.freeze(chain).await.map_err(|e| e.to_string())?;
            return Err(
                "legacy cursor exceeds agreed finalized evidence; audited recovery required"
                    .to_owned(),
            );
        }
        let ah = block(&ap, ai, height, deadline).await?;
        let bh = block(&bp, bi, height, deadline).await?;
        if ah != bh || ah.number != height {
            state.freeze(chain).await.map_err(|e| e.to_string())?;
            return Err("height-only cursor has no agreed A/B hash anchor".to_owned());
        }
        if old.is_none() {
            let v=ap.send(ai,&json!({"jsonrpc":"2.0","id":1,"method":"eth_getBlockByNumber","params":[format!("0x{height:x}"),false]}),deadline).await.map_err(|e|e.to_string())?;
            let time = v
                .get("result")
                .and_then(|v| v.get("timestamp"))
                .and_then(serde_json::Value::as_str)
                .and_then(|s| u64::from_str_radix(s.strip_prefix("0x")?, 16).ok())
                .and_then(|n| i64::try_from(n).ok())
                .and_then(|n| chrono::DateTime::from_timestamp(n, 0))
                .ok_or("malformed cursor timestamp")?;
            db::initialize_cursor(pool, chain, height, time)
                .await
                .map_err(|e| e.to_string())?;
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
        sqlx::query("UPDATE rpc_chain_state SET awaiting_anchor=false WHERE chain_id=$1")
            .bind(i64::try_from(chain).map_err(|e| e.to_string())?)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}
/// Cooldown expiry only schedules probes; full identity/contracts/heads are checked before readmit.
pub async fn recover_members(
    pool: PgPool,
    routes: Arc<RouteSet>,
    cancellation: CancellationToken,
    config_digest: String,
) -> Result<(), String> {
    let state = db::rpc::state(&pool, config_digest);
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
        if let Err(error) = anchor_cursors(&pool, &routes, state.as_ref()).await {
            tracing::warn!(%error, "RPC cursor anchor unavailable; safety gate remains closed");
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
            group.verified(index, false);
            if let Ok(hash) = probe(group, index, files).await {
                if genesis.get(&group.chain).is_some_and(|old| old != &hash) {
                    group.failed(index, topup_adapters::chain::evm::group::Failure::Identity);
                    continue;
                }
                genesis.insert(group.chain, hash.clone());
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
/// Pins a selected member's genesis to the database identity before standalone/recovery reads.
pub async fn verify_persisted_genesis(pool: &PgPool, routes: &RouteSet) -> Result<(), String> {
    for (group, _) in groups(routes)?.values() {
        let known: Option<String> = sqlx::query_scalar(
            "SELECT genesis_hash FROM rpc_member_validations WHERE chain_id=$1 LIMIT 1",
        )
        .bind(i64::try_from(group.chain).map_err(|e| e.to_string())?)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;
        if let Some(known) = known {
            let index = group
                .select(&Default::default(), None)
                .map_err(|e| e.to_string())?;
            let deadline = Instant::now()
                .checked_add(Duration::from_millis(group.policy.total_deadline_ms))
                .unwrap_or_else(Instant::now);
            if block(group, index, 0, deadline).await?.hash != known {
                group.failed(index, topup_adapters::chain::evm::group::Failure::Identity);
                return Err("RPC genesis differs from the persisted chain identity".into());
            }
        }
    }
    Ok(())
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
        verify_persisted_genesis(pool, routes).await?;
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
        verify_persisted_genesis(pool, routes).await?;
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
        for row in sqlx::query("SELECT d.block_number,d.block_hash,d.block_time,d.tx_hash,d.receipt_log_index,d.tx_nonce,d.asset_contract,d.from_address,d.amount_atomic::text AS amount_atomic,a.address FROM deposits d JOIN addresses a ON a.id=d.address_id WHERE d.chain_id=$1 AND d.credit_minor IS NOT NULL AND d.state<>'reversed'").bind(chain_db).fetch_all(pool).await.map_err(|e|e.to_string())? {
            let number=u64::try_from(row.try_get::<i64,_>("block_number").map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
            let hash:String=row.try_get("block_hash").map_err(|e|e.to_string())?;
            let deadline=Instant::now().checked_add(Duration::from_millis(a.policy.total_deadline_ms.min(b.policy.total_deadline_ms))).unwrap_or_else(Instant::now);
            let ah=block(a,ai,number,deadline).await?;let bh=block(b,bi,number,deadline).await?;
            if ah!=bh||ah.hash!=hash {return Err("credited deposit branch requires explicit ledger reconciliation; chain stays frozen".into());}
            let tx_hash: alloy_primitives::B256 = row.try_get::<String,_>("tx_hash").map_err(|e|e.to_string())?.parse().map_err(|_|"invalid stored transaction hash")?;
            let receipt_index=u64::try_from(row.try_get::<i64,_>("receipt_log_index").map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
            let known=topup_adapters::chain::evm::KnownTransfer {block_hash:hash.parse().map_err(|_|"invalid stored block hash")?,block_time:row.try_get("block_time").map_err(|e|e.to_string())?,tx_nonce:row.try_get::<String,_>("tx_nonce").map_err(|e|e.to_string())?.parse().map_err(|_|"invalid stored transaction nonce")?};
            let mut evidence=None;
            for (group,index) in [(a,ai),(b,bi)] {
                if group.head(index,"finalized",deadline).await.map_err(|e|e.to_string())?.number<number {return Err("credited deposit not finalized on both groups".into());}
                let client=Arc::new(EvmClient::from_group(group.clone(),Some(index)).map_err(|e|e.to_string())?);
                let lookup=FinalizedReader::new(client).receipt_transfer_known(tx_hash,receipt_index,known).await.map_err(|e|e.to_string())?;
                let transfer=lookup.transfer().ok_or("credited deposit receipt transfer missing; chain stays frozen")?;
                if transfer.block_number!=number || transfer.block_hash!=known.block_hash
                    || format!("{:#x}",transfer.token)!=row.try_get::<String,_>("asset_contract").map_err(|e|e.to_string())?
                    || format!("{:#x}",transfer.from)!=row.try_get::<String,_>("from_address").map_err(|e|e.to_string())?
                    || format!("{:#x}",transfer.to)!=row.try_get::<String,_>("address").map_err(|e|e.to_string())?
                    || transfer.amount.value().to_string()!=row.try_get::<String,_>("amount_atomic").map_err(|e|e.to_string())?
                    || evidence.as_ref().is_some_and(|old| old!=transfer) { return Err("credited deposit receipt disagrees; chain stays frozen".into()); }
                evidence=Some(transfer.clone());
            }

        }
        for row in sqlx::query("SELECT block_number,block_hash FROM flushed WHERE chain_id=$1 UNION SELECT block_number,block_hash FROM flush_failures WHERE chain_id=$1").bind(chain_db).fetch_all(pool).await.map_err(|e|e.to_string())? {
            let number=u64::try_from(row.try_get::<i64,_>("block_number").map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
            let hash:String=row.try_get("block_hash").map_err(|e|e.to_string())?;
            let deadline=Instant::now().checked_add(Duration::from_millis(a.policy.total_deadline_ms.min(b.policy.total_deadline_ms))).unwrap_or_else(Instant::now);
            let ah=block(a,ai,number,deadline).await?;let bh=block(b,bi,number,deadline).await?;
            if ah!=bh||ah.hash!=hash {return Err("factory ledger branch requires explicit reconciliation; chain stays frozen".into());}
        }
        let mut tx=pool.begin().await.map_err(|e|e.to_string())?;
        sqlx::query("UPDATE addresses SET backfilled=true WHERE chain_id=$1").bind(chain_db).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        for (group,index) in [(a,ai),(b,bi)] {
            sqlx::query("INSERT INTO rpc_watermarks(chain_id,group_id,tag,number,hash,parent_hash,member_id,config_digest,epoch) SELECT $1,$2,'cursor',$3,$4,$5,$6,$7,epoch FROM rpc_chain_state WHERE chain_id=$1")
                .bind(chain_db).bind(&group.id).bind(i64::try_from(anchor.number).map_err(|e|e.to_string())?).bind(&anchor.hash).bind(&anchor.parent_hash)
                .bind(&group.members.get(index).ok_or("missing recovery member")?.id).bind("audited-recovery").execute(&mut *tx).await.map_err(|e|e.to_string())?;
        }
        sqlx::query("UPDATE rpc_chain_state SET frozen=false,recovery_pending=false,awaiting_anchor=false,reason=NULL WHERE chain_id=$1 AND frozen AND recovery_pending").bind(chain_db).execute(&mut *tx).await.map_err(|e|e.to_string())?;
        tx.commit().await.map_err(|e|e.to_string())?;Ok(true)
    }.await;
    lock.release().await.map_err(|e| e.to_string())?;
    result
}

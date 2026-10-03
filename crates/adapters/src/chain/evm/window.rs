//! Complete numeric log windows with a fixed member and review evidence.
use super::{
    ChainError, ChainReader, EvmClient, FactoryLog, FinalizedReader, TransferLog,
    group::{Failure, HeadAnchor},
};
use alloy::primitives::Address;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{Instant, timeout};

/// Immutable selectors for one complete window, also persisted for independent historical review.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WindowRequest {
    /// Inclusive first block.
    pub from: u64,
    /// Inclusive last block.
    pub to: u64,
    /// Recipients fixed for every batch.
    pub recipients: Vec<Address>,
    /// Token-wide scan when nonempty; otherwise recipient-filtered scans.
    pub tokens: Vec<Address>,
    /// Optional factory whose events belong to the same window.
    pub factory: Option<Address>,
    /// Finalized work rather than latest-head scanning.
    pub finalized: bool,
    /// Different member required for independent review, when set.
    #[serde(default)]
    pub exclude_member: Option<String>,
}
/// Validated coverage, never a cached RPC answer.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct WindowProof {
    /// Answering group.
    pub group: String,
    /// Answering member.
    pub member: String,
    /// Complete request for durable review.
    pub request: WindowRequest,
    /// Hash anchor at the window end.
    pub end_hash: String,
}
/// All evidence returned together; failure produces no partial result.
#[derive(Debug)]
pub struct WindowResult {
    /// Transfers from every filter batch.
    pub transfers: Vec<TransferLog>,
    /// Factory events from the same member.
    pub factory_logs: Vec<FactoryLog>,
    /// Production member provenance; test readers may omit it.
    pub proof: Option<WindowProof>,
}
/// Default whole-window implementation for generic readers and deterministic test doubles.
pub async fn read<R: ChainReader + ?Sized>(
    reader: &R,
    request: &WindowRequest,
) -> Result<WindowResult, ChainError> {
    let transfers = if request.tokens.is_empty() {
        let mut logs = Vec::new();
        for recipients in request.recipients.chunks(super::MAX_ADDRESSES_PER_REQUEST) {
            logs.extend(
                reader
                    .transfer_logs_to(recipients, request.from, request.to)
                    .await?,
            );
        }
        logs
    } else {
        reader
            .token_transfers(
                &request.tokens,
                &request.recipients.iter().copied().collect::<BTreeSet<_>>(),
                request.from,
                request.to,
            )
            .await?
    };
    let factory_logs = if let Some(factory) = request.factory {
        reader
            .factory_logs(factory, &request.recipients, request.from, request.to)
            .await?
    } else {
        Vec::new()
    };
    if transfers
        .iter()
        .any(|l| l.block_number < request.from || l.block_number > request.to)
        || factory_logs
            .iter()
            .any(|l| l.block_number < request.from || l.block_number > request.to)
    {
        return Err(ChainError::InvalidResponse(
            "logs outside requested window".to_owned(),
        ));
    }
    Ok(WindowResult {
        transfers,
        factory_logs,
        proof: None,
    })
}
impl FinalizedReader {
    /// Pins every RPC and validates heads both before and after all batches.
    pub async fn group_window(&self, request: &WindowRequest) -> Result<WindowResult, ChainError> {
        let Some(group) = self.client().group() else {
            return read(self, request).await;
        };
        if request.from > request.to
            || request.to.saturating_sub(request.from) >= super::MAX_BLOCKS_PER_REQUEST
        {
            return Err(ChainError::InvalidResponse(
                "invalid numeric log window".to_owned(),
            ));
        }
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(group.policy.total_deadline_ms))
            .unwrap_or_else(Instant::now);
        let operation = async {
            let mut tried = BTreeSet::new();
            let mut last = Failure::Unavailable;
            for _ in 0..group.policy.max_attempts {
                let index = match group.select_for(
                    &tried,
                    request.exclude_member.as_deref(),
                    Some("eth_getLogs"),
                ) {
                    Ok(i) => i,
                    Err(_) => break,
                };
                tried.insert(index);
                let result=async {
                    let tag=if request.finalized {"finalized"} else {"latest"};
                    if group.head(index,tag,deadline).await?.number<request.to {return Err(Failure::Stale);}
                    let anchor_request=json!({"jsonrpc":"2.0","id":1,"method":"eth_getBlockByNumber","params":[format!("0x{:x}",request.to),false]});
                    let anchor=group.send(index,&anchor_request,deadline).await?;
                    let before=HeadAnchor::parse(anchor.get("result").ok_or(Failure::Malformed)?)?;
                    if before.number!=request.to {return Err(Failure::Malformed);}
                    let pinned=EvmClient::from_group(group.clone(),Some(index)).map_err(|_|Failure::Malformed)?;
                    let reader=FinalizedReader::new(Arc::new(pinned));
                    let mut result=read(&reader,request).await.map_err(|e| match e {
                        ChainError::Group(e)=>e,
                        ChainError::Transport(_)=>Failure::Malformed,
                        _=>Failure::Malformed,
                    })?;
                    if group.head(index,tag,deadline).await?.number<request.to {return Err(Failure::Stale);}
                    let blocks = result.transfers.iter().map(|log| (log.block_number, log.block_hash))
                        .chain(result.factory_logs.iter().map(|log| (log.block_number, log.block_hash)))
                        .collect::<BTreeSet<_>>();
                    for (number, hash) in blocks {
                        let value=group.send(index,&json!({"jsonrpc":"2.0","id":1,"method":"eth_getBlockByNumber","params":[format!("0x{number:x}"),false]}),deadline).await?;
                        let canonical=HeadAnchor::parse(value.get("result").ok_or(Failure::Malformed)?)?;
                        if canonical.number!=number||canonical.hash!=format!("{hash:#x}") {return Err(if request.finalized {Failure::Fork} else {Failure::Stale});}
                    }
                    let after=group.send(index,&anchor_request,deadline).await?;
                    let after=HeadAnchor::parse(after.get("result").ok_or(Failure::Malformed)?)?;
                    if before!=after {return Err(if request.finalized {Failure::Fork} else {Failure::Stale});}
                    let member=group.members.get(index).ok_or(Failure::Unavailable)?;
                    result.proof=Some(WindowProof {group:group.id.clone(),member:member.id.clone(),request:request.clone(),end_hash:before.hash});
                    Ok(result)
                }.await;
                match result {
                    Ok(result) => {
                        group.succeeded(index);
                        return Ok(result);
                    }
                    Err(error) => {
                        if error == Failure::Fork {
                            group.freeze().await?;
                        }
                        group.failed(index, error);
                        last = error;
                        if !error.retryable() {
                            break;
                        }
                    }
                }
            }
            Err(last)
        };
        timeout(
            deadline.saturating_duration_since(Instant::now()),
            operation,
        )
        .await
        .map_err(|_| ChainError::Group(Failure::Deadline))?
        .map_err(ChainError::Group)
    }
}

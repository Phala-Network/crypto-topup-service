use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use alloy_primitives::{Address, B256, LogData, U256, keccak256};
use serde_json::{Value, json, to_value};
use sqlx::PgPool;
use topup_adapters::chain::flush::{decode_flushed, encode_flush, flushed_signature};
use topup_adapters::signer::actor::SignerHandle;
use topup_core::route::RouteFile;
use topup_core::{Signer, TxRequest};
use uuid::Uuid;

use crate::db::{self, Flush, FlushStatus, FlushedEvent};
use crate::pause;

use super::planner::{parse_address, parse_salt, parse_u256};
use super::{
    AlertSink, ChainClient, ChainReceipt, FeeQuote, FlushAlert, FlushEvidence, FlusherError,
    FlusherPolicy, PlannedAddress, SignedVersion, map_chain, map_signer,
};

/// Result of one flusher lifecycle action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunResult {
    /// No durable row changed and no transaction was rebroadcast.
    Idle,
    /// A planned transaction was signed and broadcast.
    Sent {
        /// Flush row identifier.
        flush_id: Uuid,
    },
    /// An unmined transaction was replaced on the same nonce.
    Replaced {
        /// Flush row identifier.
        flush_id: Uuid,
    },
    /// A stored raw transaction was rebroadcast during recovery.
    Rebroadcast {
        /// Flush row identifier.
        flush_id: Uuid,
    },
    /// A finalized successful receipt was persisted.
    Confirmed {
        /// Flush row identifier.
        flush_id: Uuid,
    },
    /// A finalized failed receipt was persisted and optionally replanned.
    Reverted {
        /// Flush row identifier.
        flush_id: Uuid,
    },
}

/// The flusher's operator and whether it may call `ForwarderFactory.flush`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OperatorRole {
    /// Address of the configured operator key.
    pub operator: Address,
    /// Whether the operator holds `OPERATOR_ROLE` on the route's factory.
    pub granted: bool,
}

/// Flush transaction lifecycle coordinator.
pub struct Flusher {
    pool: PgPool,
    chain: Arc<dyn ChainClient>,
    signer: SignerHandle,
    alerts: Arc<dyn AlertSink>,
    policy: FlusherPolicy,
}

impl Flusher {
    /// Creates a flusher for one chain client.
    #[must_use]
    pub fn new(
        pool: PgPool,
        chain: Arc<dyn ChainClient>,
        signer: SignerHandle,
        alerts: Arc<dyn AlertSink>,
        policy: FlusherPolicy,
    ) -> Self {
        Self {
            pool,
            chain,
            signer,
            alerts,
            policy,
        }
    }

    /// Reads whether the configured operator holds `OPERATOR_ROLE` on the route's factory.
    pub async fn operator_role(&self, route: &RouteFile) -> Result<OperatorRole, FlusherError> {
        let operator = self.signer.operator_address().await.map_err(map_signer)?;
        let granted = self
            .chain
            .has_operator_role(route.chain.contracts.forwarder_factory, operator)
            .await
            .map_err(map_chain)?;
        Ok(OperatorRole { operator, granted })
    }

    /// Maintains existing sent rows first, then sends one queued plan for the current operator.
    pub async fn run_once(&self, route: &RouteFile) -> Result<RunResult, FlusherError> {
        if let Some(result) = self.maintain_sent(route).await? {
            return Ok(result);
        }
        self.send_next(route).await
    }

    /// Signs and broadcasts the oldest planned row if this operator has no in-flight flush.
    ///
    /// A plan whose route, product, or account has the `flush` scope paused is voided instead,
    /// and the next plan takes over its nonce, so one pause never stalls the chain's queue.
    pub async fn send_next(&self, route: &RouteFile) -> Result<RunResult, FlusherError> {
        let operator = self.signer.operator_address().await.map_err(map_signer)?;
        let latest = self.chain.latest_block().await.map_err(map_chain)?;
        let fees = self.chain.fee_quote().await.map_err(map_chain)?;
        loop {
            let mut transaction = self.pool.begin().await?;
            db::lock_operator(&mut transaction, route.chain.chain_id, operator).await?;
            let Some(flush) =
                db::next_planned_flush(&mut transaction, route.chain.chain_id, operator).await?
            else {
                transaction.commit().await?;
                return Ok(RunResult::Idle);
            };
            let mut evidence = parse_evidence(&flush)?;
            let (factory, token, salts) = bound_call(&flush, &evidence)?;
            let max_fee = fees.max_fee_per_gas.min(self.policy.max_fee_per_gas);
            let priority = fees.max_priority_fee_per_gas.min(max_fee);
            let gas_limit = buffered_gas(evidence.estimated_gas, self.policy.gas_limit_bps)?;
            let request = TxRequest {
                chain_id: flush.chain_id,
                nonce: flush.nonce,
                to: factory,
                value: U256::ZERO,
                data: encode_flush(salts, token),
                gas_limit,
                max_fee_per_gas: max_fee,
                max_priority_fee_per_gas: priority,
            };
            // Sign under the operator nonce lock only; the pause rows are share-locked afterwards
            // so a slow signer never blocks account, product, or route writes.
            let signed = self
                .signer
                .sign_operator_tx(request)
                .await
                .map_err(map_signer)?;
            let address_ids = evidence
                .plan
                .iter()
                .map(|address| address.address_id)
                .collect::<Vec<_>>();
            if let Some(paused) = pause::flush_pause_for_addresses_locked(
                &mut transaction,
                &evidence.binding.route,
                &address_ids,
            )
            .await?
            {
                db::void_paused_plan(&mut transaction, &flush, &address_ids, &paused).await?;
                transaction.commit().await?;
                crate::observability::record_flush_send_paused(route.chain.chain_id);
                tracing::info!(
                    flush_id = %flush.id,
                    route = %evidence.binding.route,
                    paused = %paused,
                    outcome = "voided",
                    "planned flush voided by a flush pause"
                );
                continue;
            }
            let hash = keccak256(&signed.raw_signed_bytes);
            evidence.signed.push(SignedVersion {
                hash: format!("{hash:#x}"),
                raw: hex::encode(&signed.raw_signed_bytes),
                signed_at_block: latest,
                max_fee_per_gas: max_fee,
                max_priority_fee_per_gas: priority,
            });
            db::mark_flush_sent(&mut transaction, flush.id, hash, &to_value(&evidence)?).await?;
            transaction.commit().await?;
            let broadcast = self
                .chain
                .send_raw_transaction(&signed.raw_signed_bytes)
                .await
                .map_err(map_chain)?;
            if broadcast != hash {
                return Err(FlusherError::Invariant(
                    "RPC returned a hash different from the signed transaction",
                ));
            }
            return Ok(RunResult::Sent { flush_id: flush.id });
        }
    }

    /// Recovers, replaces, or confirms one sent flush for this route.
    pub async fn maintain_sent(
        &self,
        route: &RouteFile,
    ) -> Result<Option<RunResult>, FlusherError> {
        let sent = db::list_flushes(&self.pool, FlushStatus::Sent).await?;
        for flush in sent.into_iter().filter(|flush| {
            flush.chain_id == route.chain.chain_id && flush.token == route.asset.contract
        }) {
            if let Some(receipt) = self.known_receipt(&flush).await? {
                if let Some(result) = self.process_receipt(&flush, receipt).await? {
                    return Ok(Some(result));
                }
                continue;
            }
            let confirmed_nonce = self
                .chain
                .confirmed_nonce(flush.operator)
                .await
                .map_err(map_chain)?;
            if confirmed_nonce > flush.nonce {
                let evidence = parse_evidence(&flush)?;
                let from_block = evidence
                    .recovery_from_block
                    .or_else(|| {
                        evidence
                            .signed
                            .first()
                            .map(|version| version.signed_at_block)
                    })
                    .ok_or(FlusherError::StoredEvidence(
                        "sent flush has no raw transaction",
                    ))?;
                let search = self
                    .chain
                    .receipt_by_sender_nonce(
                        flush.operator,
                        flush.nonce,
                        from_block,
                        self.policy.recovery_scan_blocks,
                    )
                    .await
                    .map_err(map_chain)?;
                if let Some(receipt) = search.receipt {
                    if let Some(result) = self.process_receipt(&flush, receipt).await? {
                        return Ok(Some(result));
                    }
                } else if let Some(next_block) = search.next_block {
                    let mut progressed = evidence;
                    progressed.recovery_from_block = Some(next_block);
                    let expected = flush.tx_hash.ok_or(FlusherError::StoredEvidence(
                        "sent flush has no transaction hash",
                    ))?;
                    db::update_sent_evidence(
                        &self.pool,
                        flush.id,
                        expected,
                        &to_value(progressed)?,
                    )
                    .await?;
                } else {
                    self.alerts.emit(FlushAlert::MissingConsumedReceipt {
                        chain_id: flush.chain_id,
                        operator: flush.operator,
                        nonce: flush.nonce,
                    });
                }
                continue;
            }

            let evidence = parse_evidence(&flush)?;
            let current_operator = self.signer.operator_address().await.map_err(map_signer)?;
            let latest = self.chain.latest_block().await.map_err(map_chain)?;
            let last = evidence.signed.last().ok_or(FlusherError::StoredEvidence(
                "sent flush has no raw transaction",
            ))?;
            let stale_at = last
                .signed_at_block
                .checked_add(self.policy.replacement_after_blocks)
                .ok_or(FlusherError::Arithmetic)?;
            if current_operator == flush.operator && latest >= stale_at {
                let suggested = self.chain.fee_quote().await.map_err(map_chain)?;
                return self
                    .replace(flush, evidence, latest, suggested)
                    .await
                    .map(Some);
            }
            let raw = decode_raw(last)?;
            let hash = self
                .chain
                .send_raw_transaction(&raw)
                .await
                .map_err(map_chain)?;
            let expected = parse_hash(&last.hash)?;
            if hash != expected {
                return Err(FlusherError::Invariant(
                    "rebroadcast returned a different transaction hash",
                ));
            }
            return Ok(Some(RunResult::Rebroadcast { flush_id: flush.id }));
        }
        Ok(None)
    }

    async fn known_receipt(&self, flush: &Flush) -> Result<Option<ChainReceipt>, FlusherError> {
        let evidence = parse_evidence(flush)?;
        for version in evidence.signed.iter().rev() {
            let hash = parse_hash(&version.hash)?;
            if let Some(receipt) = self.chain.receipt(hash).await.map_err(map_chain)? {
                return Ok(Some(receipt));
            }
        }
        Ok(None)
    }

    async fn replace(
        &self,
        flush: Flush,
        evidence: FlushEvidence,
        latest: u64,
        suggested: FeeQuote,
    ) -> Result<RunResult, FlusherError> {
        let previous = evidence.signed.last().ok_or(FlusherError::StoredEvidence(
            "sent flush has no signed version",
        ))?;
        let required_max_fee = required_bumped_fee(
            previous.max_fee_per_gas,
            suggested.max_fee_per_gas,
            self.policy.replacement_bps,
        )?;
        let required_priority = required_bumped_fee(
            previous.max_priority_fee_per_gas,
            suggested.max_priority_fee_per_gas,
            self.policy.replacement_bps,
        )?;
        if required_max_fee > self.policy.max_fee_per_gas || required_priority > required_max_fee {
            self.alerts.emit(FlushAlert::FeeCapReached {
                flush_id: flush.id,
                required_max_fee_per_gas: required_max_fee,
                cap: self.policy.max_fee_per_gas,
            });
            let raw = decode_raw(previous)?;
            self.chain
                .send_raw_transaction(&raw)
                .await
                .map_err(map_chain)?;
            return Ok(RunResult::Rebroadcast { flush_id: flush.id });
        }
        let observed_hash = parse_hash(&previous.hash)?;
        let mut transaction = self.pool.begin().await?;
        db::lock_operator(&mut transaction, flush.chain_id, flush.operator).await?;
        let Some(current) = db::get_flush_locked(&mut transaction, flush.id).await? else {
            transaction.commit().await?;
            return Ok(RunResult::Idle);
        };
        if current.status != FlushStatus::Sent || current.tx_hash != Some(observed_hash) {
            transaction.commit().await?;
            return Ok(RunResult::Idle);
        }
        let mut evidence = parse_evidence(&current)?;
        let current_previous = evidence.signed.last().ok_or(FlusherError::StoredEvidence(
            "sent flush has no signed version",
        ))?;
        if parse_hash(&current_previous.hash)? != observed_hash {
            transaction.commit().await?;
            return Ok(RunResult::Idle);
        }
        let (factory, token, salts) = bound_call(&current, &evidence)?;
        let request = TxRequest {
            chain_id: flush.chain_id,
            nonce: flush.nonce,
            to: factory,
            value: U256::ZERO,
            data: encode_flush(salts, token),
            gas_limit: buffered_gas(evidence.estimated_gas, self.policy.gas_limit_bps)?,
            max_fee_per_gas: required_max_fee,
            max_priority_fee_per_gas: required_priority,
        };
        let signed = self
            .signer
            .sign_operator_tx(request)
            .await
            .map_err(map_signer)?;
        let hash = keccak256(&signed.raw_signed_bytes);
        evidence.signed.push(SignedVersion {
            hash: format!("{hash:#x}"),
            raw: hex::encode(&signed.raw_signed_bytes),
            signed_at_block: latest,
            max_fee_per_gas: required_max_fee,
            max_priority_fee_per_gas: required_priority,
        });
        if !db::store_flush_replacement_cas(
            &mut transaction,
            flush.id,
            observed_hash,
            hash,
            &to_value(&evidence)?,
        )
        .await?
        {
            transaction.commit().await?;
            return Ok(RunResult::Idle);
        }
        transaction.commit().await?;
        let broadcast = self
            .chain
            .send_raw_transaction(&signed.raw_signed_bytes)
            .await
            .map_err(map_chain)?;
        if broadcast != hash {
            return Err(FlusherError::Invariant(
                "replacement returned a different transaction hash",
            ));
        }
        Ok(RunResult::Replaced { flush_id: flush.id })
    }

    async fn process_receipt(
        &self,
        flush: &Flush,
        receipt: ChainReceipt,
    ) -> Result<Option<RunResult>, FlusherError> {
        let finalized = self.chain.finalized_block().await.map_err(map_chain)?;
        if receipt.block_number > finalized {
            return Ok(None);
        }
        if !receipt.success {
            self.handle_revert(flush, receipt).await?;
            return Ok(Some(RunResult::Reverted { flush_id: flush.id }));
        }
        let evidence = parse_evidence(flush)?;
        let factory = parse_address(&evidence.binding.factory)?;
        let token = parse_address(&evidence.binding.token)?;
        if token != flush.token {
            return Err(FlusherError::StoredEvidence(
                "bound token differs from flush row",
            ));
        }
        let planned = planned_by_salt(&evidence.plan)?;
        let mut seen = BTreeSet::new();
        let mut events = Vec::new();
        for log in &receipt.logs {
            if log.address != factory || log.topics.first() != Some(&flushed_signature()) {
                continue;
            }
            let log_data = LogData::new(log.topics.clone(), log.data.clone())
                .ok_or(FlusherError::StoredEvidence("invalid Flushed topic count"))?;
            let decoded = decode_flushed(&log_data)
                .map_err(|_| FlusherError::StoredEvidence("invalid Flushed event"))?;
            if decoded.token != flush.token {
                return Err(FlusherError::Invariant(
                    "Flushed event token differs from the flush row",
                ));
            }
            let item = planned.get(&decoded.salt).ok_or(FlusherError::Invariant(
                "Flushed event salt was not planned",
            ))?;
            if parse_address(&item.address)? != decoded.forwarder {
                return Err(FlusherError::Invariant(
                    "Flushed event forwarder differs from the planned address",
                ));
            }
            if !seen.insert(decoded.salt) {
                return Err(FlusherError::Invariant("duplicate Flushed event salt"));
            }
            events.push(FlushedEvent {
                flush_id: flush.id,
                address_id: item.address_id,
                amount_atomic: topup_core::money::AtomicAmount::new(decoded.amount),
                block_number: log.block_number,
                log_index: log.log_index,
            });
        }
        if events.len() != evidence.plan.len() {
            return Err(FlusherError::Invariant(
                "successful flush receipt did not contain one event per planned salt",
            ));
        }
        let mut confirmed = evidence;
        confirmed.chain_receipt = Some(receipt_json(&receipt));
        db::confirm_flush(
            &self.pool,
            flush.id,
            receipt.block_number,
            &to_value(confirmed)?,
            &events,
        )
        .await?;
        Ok(Some(RunResult::Confirmed { flush_id: flush.id }))
    }

    async fn handle_revert(
        &self,
        flush: &Flush,
        receipt: ChainReceipt,
    ) -> Result<(), FlusherError> {
        let mut evidence = parse_evidence(flush)?;
        evidence.chain_receipt = Some(receipt_json(&receipt));
        let groups = failed_groups(&evidence);
        let operator = self.signer.operator_address().await.map_err(map_signer)?;
        let mut replans = Vec::new();
        for group in groups {
            let mut binding = evidence.binding.clone();
            binding.salts = group.iter().map(|item| item.salt.clone()).collect();
            let mut next = FlushEvidence::planned(binding, group, evidence.estimated_gas);
            next.failure_attempt = evidence
                .failure_attempt
                .checked_add(1)
                .ok_or(FlusherError::Arithmetic)?;
            next.parent_flush_id = Some(flush.id);
            replans.push(next);
        }
        let pending = self
            .chain
            .pending_nonce(operator)
            .await
            .map_err(map_chain)?;
        let mut transaction = self.pool.begin().await?;
        db::lock_operator(&mut transaction, flush.chain_id, operator).await?;
        db::mark_flush_reverted_locked(
            &mut transaction,
            flush.id,
            receipt.block_number,
            &to_value(&evidence)?,
        )
        .await?;
        for plan in replans {
            let nonce =
                db::next_flush_nonce(&mut transaction, flush.chain_id, operator, pending).await?;
            db::insert_planned_flush(
                &mut transaction,
                Uuid::new_v4(),
                flush.chain_id,
                flush.token,
                operator,
                nonce,
                &to_value(plan)?,
            )
            .await?;
        }
        transaction.commit().await?;
        self.alerts.emit(FlushAlert::Reverted {
            flush_id: flush.id,
            nonce: flush.nonce,
        });
        if evidence.failure_attempt > 0 && evidence.plan.len() == 1 {
            let item = evidence
                .plan
                .first()
                .ok_or(FlusherError::StoredEvidence("singleton plan is empty"))?;
            self.alerts.emit(FlushAlert::IsolatedAddress {
                chain_id: flush.chain_id,
                token: flush.token,
                address_id: item.address_id,
                address: parse_address(&item.address)?,
                salt: parse_salt(&item.salt)?,
            });
        }
        Ok(())
    }
}

fn parse_evidence(flush: &Flush) -> Result<FlushEvidence, FlusherError> {
    serde_json::from_value(flush.receipt.clone()).map_err(Into::into)
}

fn plan_salts(plan: &[PlannedAddress]) -> Result<Vec<B256>, FlusherError> {
    plan.iter().map(|item| parse_salt(&item.salt)).collect()
}

fn bound_call(
    flush: &Flush,
    evidence: &FlushEvidence,
) -> Result<(Address, Address, Vec<B256>), FlusherError> {
    let factory = parse_address(&evidence.binding.factory)?;
    let token = parse_address(&evidence.binding.token)?;
    if token != flush.token {
        return Err(FlusherError::StoredEvidence(
            "bound token differs from flush row",
        ));
    }
    let salts = evidence
        .binding
        .salts
        .iter()
        .map(|salt| parse_salt(salt))
        .collect::<Result<Vec<_>, _>>()?;
    if salts != plan_salts(&evidence.plan)? {
        return Err(FlusherError::StoredEvidence(
            "bound salts differ from planned addresses",
        ));
    }
    Ok((factory, token, salts))
}

fn planned_by_salt(
    plan: &[PlannedAddress],
) -> Result<BTreeMap<B256, &PlannedAddress>, FlusherError> {
    let mut result = BTreeMap::new();
    for item in plan {
        let salt = parse_salt(&item.salt)?;
        parse_u256(&item.balance_atomic)?;
        if result.insert(salt, item).is_some() {
            return Err(FlusherError::StoredEvidence("duplicate planned salt"));
        }
    }
    Ok(result)
}

fn buffered_gas(estimate: u64, multiplier_bps: u16) -> Result<u64, FlusherError> {
    let numerator = u128::from(estimate)
        .checked_mul(u128::from(multiplier_bps))
        .and_then(|value| value.checked_add(9_999))
        .ok_or(FlusherError::Arithmetic)?;
    let result = numerator
        .checked_div(10_000)
        .ok_or(FlusherError::Arithmetic)?;
    u64::try_from(result).map_err(|_| FlusherError::Arithmetic)
}

fn required_bumped_fee(
    previous: u128,
    suggested: u128,
    multiplier_bps: u16,
) -> Result<u128, FlusherError> {
    let bumped = previous
        .checked_mul(u128::from(multiplier_bps))
        .and_then(|value| value.checked_add(9_999))
        .and_then(|value| value.checked_div(10_000))
        .and_then(|value| value.checked_add(1))
        .ok_or(FlusherError::Arithmetic)?;
    Ok(bumped.max(suggested))
}

fn decode_raw(version: &SignedVersion) -> Result<Vec<u8>, FlusherError> {
    hex::decode(&version.raw).map_err(|_| FlusherError::StoredEvidence("invalid raw tx hex"))
}

fn parse_hash(value: &str) -> Result<B256, FlusherError> {
    value
        .parse()
        .map_err(|_| FlusherError::StoredEvidence("invalid transaction hash"))
}

fn receipt_json(receipt: &ChainReceipt) -> Value {
    json!({
        "transaction_hash": format!("{:#x}", receipt.transaction_hash),
        "block_number": receipt.block_number,
        "success": receipt.success,
        "logs": receipt.logs.iter().map(|log| json!({
            "address": format!("{:#x}", log.address),
            "topics": log.topics.iter().map(|topic| format!("{topic:#x}")).collect::<Vec<_>>(),
            "data": format!("0x{}", hex::encode(&log.data)),
            "block_number": log.block_number,
            "log_index": log.log_index,
        })).collect::<Vec<_>>(),
    })
}

fn failed_groups(evidence: &FlushEvidence) -> Vec<Vec<PlannedAddress>> {
    if evidence.failure_attempt == 0 {
        return vec![evidence.plan.clone()];
    }
    if evidence.plan.len() <= 1 {
        return Vec::new();
    }
    let middle = evidence.plan.len() / 2;
    vec![
        evidence.plan[..middle].to_vec(),
        evidence.plan[middle..].to_vec(),
    ]
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, B256};
    use uuid::Uuid;

    use super::*;

    fn binding(items: &[PlannedAddress]) -> super::super::FlushCallBinding {
        super::super::FlushCallBinding {
            route: "test".to_owned(),
            config_version: 1,
            factory: format!("{:#x}", Address::from([9; 20])),
            token: format!("{:#x}", Address::from([8; 20])),
            salts: items.iter().map(|item| item.salt.clone()).collect(),
        }
    }

    fn item(number: u8) -> PlannedAddress {
        PlannedAddress {
            address_id: Uuid::from_u128(u128::from(number)),
            salt: format!("{:#x}", B256::from([number; 32])),
            address: format!("{:#x}", Address::from([number; 20])),
            balance_atomic: "1000".to_owned(),
        }
    }

    #[test]
    fn first_failure_retries_the_batch_then_bisects_persistent_failure() {
        let items = vec![item(1), item(2), item(3)];
        let mut evidence = FlushEvidence::planned(binding(&items), items, 100);
        assert_eq!(failed_groups(&evidence), vec![evidence.plan.clone()]);
        evidence.failure_attempt = 1;
        let groups = failed_groups(&evidence);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0], vec![item(1)]);
        assert_eq!(groups[1], vec![item(2), item(3)]);
        evidence.plan = vec![item(2)];
        assert!(failed_groups(&evidence).is_empty());
    }

    #[test]
    fn replacement_fee_is_strictly_higher_and_not_silently_capped() {
        assert_eq!(
            required_bumped_fee(100, 90, 12_500).expect("fee should fit"),
            126
        );
        assert_eq!(
            required_bumped_fee(999, 900, 12_500).expect("fee should fit"),
            1_250
        );
        assert_eq!(
            required_bumped_fee(1_000, 900, 12_500).expect("fee should fit"),
            1_251
        );
    }
}

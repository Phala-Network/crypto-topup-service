//! Settlement endpoint conformance runner.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Output};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use alloy_primitives::{Address, B256};
use anyhow::{Context, Result, anyhow, bail, ensure};
use chrono::Utc;
use reqwest::header::HeaderValue;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::task::JoinSet;
use topup_adapters::settlement::http::{
    SettlementAnswer, SettlementApi as _, SettlementClient, SettlementRequest,
};
use topup_adapters::signer::actor::SignerHandle;
use topup_core::address::{forwarder_address, persistent_salt};
use topup_core::identity::deposit_id;
use url::Url;

use crate::report::{Report, TestResult, TestStatus};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const SYNTHETIC_ASSET: &str = "0x1111111111111111111111111111111111111111";
const SYNTHETIC_FACTORY: &str = "0x2222222222222222222222222222222222222222";
const SYNTHETIC_IMPLEMENTATION: &str = "0x3333333333333333333333333333333333333333";

/// Inputs controlled by a product team when running the suite.
#[derive(Clone)]
pub struct SuiteConfig {
    /// Product settlement POST URL.
    pub settlement_url: String,
    /// Signer using the product's pinned test seed.
    pub signer: SignerHandle,
    /// Pinned key identifier paired with the public key.
    pub keyid: String,
    /// Product-configured independent per-deposit cap in minor units.
    pub per_deposit_cap: u64,
    /// Account configured to accept conformance credits.
    pub accepted_account_id: String,
    /// Account configured to return a business refusal.
    pub refused_account_id: String,
    /// Account configured to retain a processing response.
    pub processing_account_id: String,
    /// Optional live Anvil fixture provider.
    pub evidence: Arc<dyn EvidenceProvider>,
    /// Whether obligation 5 should be asserted instead of reported as skipped.
    pub check_chain_evidence: bool,
}

/// One Transfer-log evidence object embedded in a settlement payload.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Evidence {
    /// EVM chain id.
    pub chain_id: u64,
    /// Approved ERC-20 contract.
    pub asset_contract: String,
    /// Product route slug.
    pub route: String,
    /// Product route version.
    pub route_version: u64,
    /// Transaction hash containing the Transfer log.
    pub tx_hash: String,
    /// Receipt-global log index.
    pub log_index: u64,
    /// Product-computed forwarder address.
    pub to: String,
    /// Atomic token amount.
    pub amount_atomic: String,
    /// Scaled USD price used by the service.
    pub price_scaled: String,
    /// Price decimal scale.
    pub price_scale: u32,
    /// UTC valuation time.
    pub valuation_at: String,
    /// Optional product rate-lock reference.
    pub lock_ref: Option<String>,
}

/// Supplies valid, independently identifiable evidence for test requests.
#[async_trait::async_trait]
pub trait EvidenceProvider: Send + Sync {
    /// Returns evidence for one account and atomic amount.
    async fn evidence(&self, account_id: &str, amount_atomic: u64) -> Result<Evidence>;

    /// Returns a real or simulated log which has not reached finality.
    async fn unfinalized_evidence(&self, account_id: &str, amount_atomic: u64) -> Result<Evidence> {
        let mut evidence = self.evidence(account_id, amount_atomic).await?;
        evidence.tx_hash = format!("0x{}", "fe".repeat(32));
        Ok(evidence)
    }
}

/// Deterministic evidence provider used by the in-process reference tests.
pub struct SyntheticEvidence {
    chain_id: u64,
    counter: AtomicU64,
}

impl SyntheticEvidence {
    /// Creates a synthetic provider for a chain id.
    #[must_use]
    pub fn new(chain_id: u64) -> Self {
        Self {
            chain_id,
            counter: AtomicU64::new(1),
        }
    }
}

#[async_trait::async_trait]
impl EvidenceProvider for SyntheticEvidence {
    async fn evidence(&self, account_id: &str, amount_atomic: u64) -> Result<Evidence> {
        let sequence = self.counter.fetch_add(1, Ordering::Relaxed);
        let marker = u8::try_from(sequence % 251).unwrap_or(1);
        let mut tx_hash_bytes = [marker; 32];
        if let Some(amount_bytes) = tx_hash_bytes.get_mut(24..) {
            amount_bytes.copy_from_slice(&amount_atomic.to_be_bytes());
        }
        let tx_hash = B256::from(tx_hash_bytes);
        let factory = Address::from_str(SYNTHETIC_FACTORY)?;
        let implementation = Address::from_str(SYNTHETIC_IMPLEMENTATION)?;
        let to = forwarder_address(
            factory,
            implementation,
            persistent_salt("conformance", account_id, 1),
        );
        Ok(Evidence {
            chain_id: self.chain_id,
            asset_contract: SYNTHETIC_ASSET.to_owned(),
            route: "conformance".to_owned(),
            route_version: 1,
            tx_hash: format!("{tx_hash:#x}"),
            log_index: 0,
            to: format!("{to:#x}"),
            amount_atomic: amount_atomic.to_string(),
            price_scaled: "100000000".to_owned(),
            price_scale: 8,
            valuation_at: Utc::now().to_rfc3339(),
            lock_ref: None,
        })
    }
}

/// Foundry-backed evidence provider which deploys the A1 factory and mock token on Anvil.
pub struct AnvilEvidence {
    rpc_url: String,
    chain_id: u64,
    factory: Address,
    implementation: Address,
    token: Address,
}

impl AnvilEvidence {
    /// Deploys a fresh factory and `MockERC20` using Anvil's first development account.
    pub fn prepare(rpc_url: String, chain_id: u64) -> Result<Self> {
        for command in ["forge", "cast"] {
            ensure!(
                command_available(command),
                "{command} is required with --anvil-rpc"
            );
        }
        let contracts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts");
        let actual_chain_id = run_checked("cast", &["chain-id", "--rpc-url", &rpc_url], None)?;
        let actual_chain_id = String::from_utf8(actual_chain_id.stdout)?
            .trim()
            .parse::<u64>()?;
        ensure!(
            actual_chain_id == chain_id,
            "Anvil chain id is {actual_chain_id}, expected {chain_id}"
        );
        let factory = deploy_contract(
            &contracts,
            &rpc_url,
            "src/ForwarderFactory.sol:ForwarderFactory",
            &[ANVIL_DEPLOYER, ANVIL_DEPLOYER],
        )?;
        let implementation =
            cast_call_address(&rpc_url, factory, "implementation()(address)", &[])?;
        let token = deploy_contract(
            &contracts,
            &rpc_url,
            "test/mocks/MockTokens.sol:MockERC20",
            &[],
        )?;
        eprintln!(
            "conformance Anvil fixture: factory={factory:#x} implementation={implementation:#x} asset={token:#x}"
        );
        Ok(Self {
            rpc_url,
            chain_id,
            factory,
            implementation,
            token,
        })
    }

    /// Returns the addresses a product must configure for its conformance route.
    #[must_use]
    pub fn route_addresses(&self) -> (Address, Address, Address) {
        (self.factory, self.implementation, self.token)
    }

    fn create_evidence(
        &self,
        account_id: &str,
        amount_atomic: u64,
        finalize: bool,
    ) -> Result<Evidence> {
        let salt = persistent_salt("conformance", account_id, 1);
        let forwarder = forwarder_address(self.factory, self.implementation, salt);
        let transaction = cast_send_json(
            &self.rpc_url,
            self.token,
            "mint(address,uint256)",
            &[&format!("{forwarder:#x}"), &amount_atomic.to_string()],
        )?;
        let tx_hash = transaction
            .get("transactionHash")
            .or_else(|| transaction.get("transaction_hash"))
            .and_then(Value::as_str)
            .context("cast send omitted transaction hash")?;
        if finalize {
            run_checked(
                "cast",
                &["rpc", "--rpc-url", &self.rpc_url, "anvil_mine", "0x41"],
                None,
            )?;
        }
        Ok(Evidence {
            chain_id: self.chain_id,
            asset_contract: format!("{:#x}", self.token),
            route: "conformance".to_owned(),
            route_version: 1,
            tx_hash: tx_hash.to_owned(),
            log_index: 0,
            to: format!("{forwarder:#x}"),
            amount_atomic: amount_atomic.to_string(),
            price_scaled: "100000000".to_owned(),
            price_scale: 8,
            valuation_at: Utc::now().to_rfc3339(),
            lock_ref: None,
        })
    }
}

#[async_trait::async_trait]
impl EvidenceProvider for AnvilEvidence {
    async fn evidence(&self, account_id: &str, amount_atomic: u64) -> Result<Evidence> {
        self.create_evidence(account_id, amount_atomic, true)
    }

    async fn unfinalized_evidence(&self, account_id: &str, amount_atomic: u64) -> Result<Evidence> {
        self.create_evidence(account_id, amount_atomic, false)
    }
}

/// Runs all mandatory protocol tests and returns a machine-readable report.
pub async fn run(config: SuiteConfig) -> Result<Report> {
    let started_at = Utc::now();
    let mut runner = Runner::new(config)?;
    runner.run_all().await;
    Ok(Report::complete(
        runner.config.settlement_url.clone(),
        started_at,
        runner.tests,
    ))
}

struct Runner {
    config: SuiteConfig,
    client: SettlementClient,
    raw_client: reqwest::Client,
    tests: Vec<TestResult>,
    accepted: Option<(SettlementRequest, String)>,
}

impl Runner {
    fn new(config: SuiteConfig) -> Result<Self> {
        let client = SettlementClient::new_with_keyid(
            &config.settlement_url,
            config.signer.clone(),
            REQUEST_TIMEOUT,
            config.keyid.clone(),
        )?;
        let raw_client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        Ok(Self {
            config,
            client,
            raw_client,
            tests: Vec::new(),
            accepted: None,
        })
    }

    async fn run_all(&mut self) {
        let result = self.valid_accepted().await;
        self.capture("accepted", "valid request is durably accepted", result);
        let result = self.exact_replay().await;
        self.capture("replay", "exact replay is idempotent", result);
        let result = self.payload_mismatch().await;
        self.capture(
            "payload_mismatch",
            "same key with a different payload returns 422",
            result,
        );
        let result = self.concurrent_identical().await;
        self.capture(
            "concurrency",
            "parallel identical requests mutate once",
            result,
        );
        let result = self.get_original().await;
        self.capture(
            "get_original",
            "GET returns status, destination id, and original payload",
            result,
        );
        let result = self.invalid_signatures().await;
        self.capture(
            "authentication",
            "invalid signature profiles are rejected",
            result,
        );
        let result = self.missing_idempotency_coverage().await;
        self.capture(
            "idempotency_coverage",
            "idempotency-key must be covered by the signature",
            result,
        );
        let result = self.over_cap().await;
        self.capture(
            "per_deposit_cap",
            "product enforces its configured per-deposit cap",
            result,
        );
        if self.config.check_chain_evidence {
            let result = self.chain_evidence().await;
            self.capture(
                "chain_evidence",
                "product validates log existence, emitter, recipient, amount, and finality",
                result,
            );
        } else {
            self.tests.push(TestResult {
                id: "chain_evidence".to_owned(),
                name: "product validates log existence, emitter, recipient, amount, and finality"
                    .to_owned(),
                status: TestStatus::Skip,
                evidence: json!({"reason": "--anvil-rpc was not provided"}),
            });
        }
        let result = self.wrong_deposit_id().await;
        self.capture(
            "deposit_identity",
            "idempotency key must match the recomputed deposit id",
            result,
        );
        let result = self.business_refusal().await;
        self.capture(
            "business_refusal",
            "business refusal is a typed 200 rejected answer",
            result,
        );
        let result = self.processing().await;
        self.capture(
            "processing",
            "processing is a typed 200 answer retained by GET",
            result,
        );
        let result = self.unknown_get().await;
        self.capture(
            "unknown_get",
            "GET of an unknown key is 404 or status unknown",
            result,
        );
    }

    fn capture(&mut self, id: &str, name: &str, result: Result<Value>) {
        let (status, evidence) = match result {
            Ok(evidence) => (TestStatus::Pass, evidence),
            Err(error) => (TestStatus::Fail, json!({"error": error.to_string()})),
        };
        self.tests.push(TestResult {
            id: id.to_owned(),
            name: name.to_owned(),
            status,
            evidence,
        });
    }

    async fn valid_accepted(&mut self) -> Result<Value> {
        let request = self
            .request(&self.config.accepted_account_id, 100, 1_000)
            .await?;
        let answer = self.client.post(&request).await?;
        let SettlementAnswer::Accepted {
            destination_tx_id, ..
        } = answer
        else {
            bail!("expected accepted, received {answer:?}");
        };
        let fetched = self.client.get_by_key(&request.idempotency_key).await?;
        ensure!(
            fetched.is_some(),
            "accepted answer was not committed before response"
        );
        self.accepted = Some((request, destination_tx_id.clone()));
        Ok(json!({"destination_tx_id": destination_tx_id, "committed_before_response": true}))
    }

    async fn exact_replay(&self) -> Result<Value> {
        let (request, expected) = self.accepted()?;
        let answer = self.client.post(request).await?;
        let SettlementAnswer::Accepted {
            destination_tx_id, ..
        } = answer
        else {
            bail!("expected accepted replay, received {answer:?}");
        };
        ensure!(
            &destination_tx_id == expected,
            "replay changed destination_tx_id"
        );
        let ledger_mutations = self.ledger_probe(&request.idempotency_key).await?;
        if let Some(count) = ledger_mutations {
            ensure!(count == 1, "ledger probe reported {count} mutations");
        }
        Ok(json!({"destination_tx_id": destination_tx_id, "ledger_mutations": ledger_mutations}))
    }

    async fn payload_mismatch(&self) -> Result<Value> {
        let (original, _) = self.accepted()?;
        let mut changed = original.clone();
        changed.payload["amount_minor"] = json!("101");
        let answer = self.client.post(&changed).await?;
        ensure!(
            answer == SettlementAnswer::PayloadMismatch422,
            "expected 422 payload mismatch, received {answer:?}"
        );
        Ok(json!({"http_status": 422}))
    }

    async fn concurrent_identical(&self) -> Result<Value> {
        let request = self
            .request(&self.config.accepted_account_id, 120, 1_200)
            .await?;
        let mut tasks = JoinSet::new();
        for _ in 0..8 {
            let client = self.client.clone();
            let request = request.clone();
            tasks.spawn(async move { client.post(&request).await });
        }
        let mut destinations = BTreeSet::new();
        let mut conflicts = 0_u64;
        let mut answers = 0_u64;
        while let Some(joined) = tasks.join_next().await {
            let answer = joined.context("concurrent request task failed")??;
            answers = answers.saturating_add(1);
            match answer {
                SettlementAnswer::Accepted {
                    destination_tx_id, ..
                } => {
                    destinations.insert(destination_tx_id);
                }
                SettlementAnswer::Conflict409 => conflicts = conflicts.saturating_add(1),
                other => bail!("unexpected concurrent answer {other:?}"),
            }
        }
        ensure!(
            !destinations.is_empty(),
            "no concurrent request was accepted"
        );
        ensure!(
            destinations.len() == 1,
            "concurrent requests returned multiple credits"
        );
        if let Some(count) = self.ledger_probe(&request.idempotency_key).await? {
            ensure!(count == 1, "concurrent ledger mutation count was {count}");
        }
        Ok(json!({"requests": answers, "conflicts": conflicts, "destination_tx_ids": destinations}))
    }

    async fn get_original(&self) -> Result<Value> {
        let (request, expected_destination) = self.accepted()?;
        let answer = self
            .client
            .get_by_key(&request.idempotency_key)
            .await?
            .context("GET returned unknown")?;
        let SettlementAnswer::Accepted {
            destination_tx_id,
            payload,
        } = answer
        else {
            bail!("GET returned {answer:?}");
        };
        ensure!(
            &destination_tx_id == expected_destination,
            "GET destination id changed"
        );
        let expected = serde_json::to_vec(&request.payload)?;
        let actual = serde_json::to_vec(&payload)?;
        ensure!(
            expected == actual,
            "GET payload was not byte-equal after JSON encoding"
        );
        Ok(json!({"destination_tx_id": destination_tx_id, "payload_byte_equal": true}))
    }

    async fn invalid_signatures(&self) -> Result<Value> {
        let request = self
            .request(&self.config.accepted_account_id, 130, 1_300)
            .await?;
        let mut statuses = Vec::new();

        let mut invalid = self.client.signed_post_request(&request).await?;
        invalid.headers_mut().insert(
            "signature",
            HeaderValue::from_static("sig1=:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==:"),
        );
        statuses.push(self.send_rejected(invalid).await?);

        let wrong_client = SettlementClient::new_with_keyid(
            &self.config.settlement_url,
            self.config.signer.clone(),
            REQUEST_TIMEOUT,
            format!("{}-wrong", self.config.keyid),
        )?;
        statuses.push(
            self.send_rejected(wrong_client.signed_post_request(&request).await?)
                .await?,
        );

        let expired = unix_timestamp()?.saturating_sub(600);
        statuses.push(
            self.send_rejected(
                self.client
                    .signed_post_request_at(&request, expired, true)
                    .await?,
            )
            .await?,
        );

        let mut tampered = self.client.signed_post_request(&request).await?;
        let body = serde_json::to_vec(&request.payload)?;
        let mut changed = body;
        changed.push(b' ');
        *tampered.body_mut() = Some(reqwest::Body::from(changed));
        statuses.push(self.send_rejected(tampered).await?);

        Ok(json!({"statuses": statuses}))
    }

    async fn missing_idempotency_coverage(&self) -> Result<Value> {
        let request = self
            .request(&self.config.accepted_account_id, 140, 1_400)
            .await?;
        let signed = self
            .client
            .signed_post_request_at(&request, unix_timestamp()?, false)
            .await?;
        let status = self.send_rejected(signed).await?;
        Ok(json!({"http_status": status}))
    }

    async fn over_cap(&self) -> Result<Value> {
        let amount = self.config.per_deposit_cap.saturating_add(1);
        let request = self
            .request(&self.config.accepted_account_id, amount, 1_500)
            .await?;
        let answer = self.client.post(&request).await?;
        let SettlementAnswer::Rejected { reason, .. } = answer else {
            bail!("over-cap request was not rejected: {answer:?}");
        };
        Ok(json!({"cap": self.config.per_deposit_cap, "amount_minor": amount, "reason": reason}))
    }

    async fn chain_evidence(&self) -> Result<Value> {
        let mut cases = Vec::new();
        for (name, mutation) in [
            ("missing_log", EvidenceMutation::MissingLog),
            ("wrong_contract", EvidenceMutation::WrongContract),
            ("wrong_to", EvidenceMutation::WrongTo),
            ("wrong_amount", EvidenceMutation::WrongAmount),
            ("not_finalized", EvidenceMutation::NotFinalized),
        ] {
            let mut invalid = if matches!(mutation, EvidenceMutation::NotFinalized) {
                self.request_with_evidence(
                    &self.config.accepted_account_id,
                    150,
                    self.config
                        .evidence
                        .unfinalized_evidence(&self.config.accepted_account_id, 1_600)
                        .await?,
                )?
            } else {
                self.request(&self.config.accepted_account_id, 150, 1_600)
                    .await?
            };
            if !matches!(mutation, EvidenceMutation::NotFinalized) {
                mutate_evidence(&mut invalid.payload, mutation)?;
            }
            invalid.idempotency_key = key_from_payload(&invalid.payload)?;
            invalid.payload["idempotency_key"] = json!(invalid.idempotency_key);
            let answer = self.client.post(&invalid).await?;
            ensure!(
                matches!(answer, SettlementAnswer::Rejected { .. }),
                "{name} was accepted"
            );
            cases.push(name);
        }
        let request = self
            .request(&self.config.accepted_account_id, 150, 1_600)
            .await?;
        let answer = self.client.post(&request).await?;
        ensure!(
            matches!(answer, SettlementAnswer::Accepted { .. }),
            "valid evidence was rejected"
        );
        Ok(json!({"rejected_invalid_cases": cases, "valid_accepted": true}))
    }

    async fn wrong_deposit_id(&self) -> Result<Value> {
        let mut request = self
            .request(&self.config.accepted_account_id, 160, 1_700)
            .await?;
        request.idempotency_key = format!("deposit:{}", uuid::Uuid::new_v4());
        request.payload["idempotency_key"] = json!(request.idempotency_key);
        let answer = self.client.post(&request).await?;
        ensure!(
            matches!(answer, SettlementAnswer::Rejected { .. }),
            "wrong deposit id was accepted"
        );
        Ok(json!({"rejected": true}))
    }

    async fn business_refusal(&self) -> Result<Value> {
        let request = self
            .request(&self.config.refused_account_id, 170, 1_800)
            .await?;
        let answer = self.client.post(&request).await?;
        let SettlementAnswer::Rejected { reason, .. } = answer else {
            bail!("expected business rejection, received {answer:?}");
        };
        Ok(json!({"reason": reason}))
    }

    async fn processing(&self) -> Result<Value> {
        let request = self
            .request(&self.config.processing_account_id, 180, 1_900)
            .await?;
        let answer = self.client.post(&request).await?;
        ensure!(
            matches!(answer, SettlementAnswer::Processing { .. }),
            "expected processing"
        );
        let fetched = self.client.get_by_key(&request.idempotency_key).await?;
        ensure!(
            matches!(fetched, Some(SettlementAnswer::Processing { .. })),
            "GET lost processing state"
        );
        Ok(json!({"status": "processing"}))
    }

    async fn unknown_get(&self) -> Result<Value> {
        let key = format!("deposit:{}", uuid::Uuid::new_v4());
        match self.client.get_by_key(&key).await? {
            None => Ok(json!({"http_status": 404})),
            Some(SettlementAnswer::Unknown { status: 200, body }) => {
                let value: Value = serde_json::from_str(&body)?;
                ensure!(
                    value.get("status") == Some(&json!("unknown")),
                    "unexpected 200 body"
                );
                Ok(json!({"http_status": 200, "status": "unknown"}))
            }
            other => bail!("unknown GET returned {other:?}"),
        }
    }

    async fn request(
        &self,
        account_id: &str,
        amount_minor: u64,
        amount_atomic: u64,
    ) -> Result<SettlementRequest> {
        let evidence = self
            .config
            .evidence
            .evidence(account_id, amount_atomic)
            .await?;
        self.request_with_evidence(account_id, amount_minor, evidence)
    }

    fn request_with_evidence(
        &self,
        account_id: &str,
        amount_minor: u64,
        evidence: Evidence,
    ) -> Result<SettlementRequest> {
        let tx_hash = B256::from_str(&evidence.tx_hash).context("parse evidence tx_hash")?;
        let id = deposit_id(evidence.chain_id, tx_hash, evidence.log_index);
        let key = format!("deposit:{id}");
        Ok(SettlementRequest {
            idempotency_key: key.clone(),
            payload: json!({
                "version": 1,
                "idempotency_key": key,
                "account_id": account_id,
                "unit": "USD",
                "amount_minor": amount_minor.to_string(),
                "source": "crypto_deposit",
                "evidence": evidence,
            }),
        })
    }

    fn accepted(&self) -> Result<(&SettlementRequest, &String)> {
        self.accepted
            .as_ref()
            .map(|(request, destination)| (request, destination))
            .context("accepted fixture unavailable")
    }

    async fn send_rejected(&self, request: reqwest::Request) -> Result<u16> {
        let response = self.raw_client.execute(request).await?;
        let status = response.status().as_u16();
        ensure!(
            status == 401 || status == 403,
            "expected 401/403, received {status}"
        );
        Ok(status)
    }

    async fn ledger_probe(&self, key: &str) -> Result<Option<u64>> {
        let mut url = Url::parse(&self.config.settlement_url)?;
        url.path_segments_mut()
            .map_err(|()| anyhow!("settlement URL cannot be a base"))?
            .pop_if_empty()
            .pop()
            .push("__conformance")
            .push("ledger")
            .push(key);
        let response = self.raw_client.get(url).send().await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            response.status().is_success(),
            "ledger probe returned {}",
            response.status()
        );
        let value: Value = response.json().await?;
        Ok(value.get("mutations").and_then(Value::as_u64))
    }
}

#[derive(Clone, Copy)]
enum EvidenceMutation {
    MissingLog,
    WrongContract,
    WrongTo,
    WrongAmount,
    NotFinalized,
}

fn mutate_evidence(payload: &mut Value, mutation: EvidenceMutation) -> Result<()> {
    let evidence = payload
        .get_mut("evidence")
        .and_then(Value::as_object_mut)
        .context("payload evidence object is missing")?;
    match mutation {
        EvidenceMutation::MissingLog => {
            evidence.insert("log_index".to_owned(), json!(9_999_999_u64));
        }
        EvidenceMutation::WrongContract => {
            evidence.insert(
                "asset_contract".to_owned(),
                json!("0x9999999999999999999999999999999999999999"),
            );
        }
        EvidenceMutation::WrongTo => {
            evidence.insert(
                "to".to_owned(),
                json!("0x9999999999999999999999999999999999999999"),
            );
        }
        EvidenceMutation::WrongAmount => {
            evidence.insert("amount_atomic".to_owned(), json!("999999999"));
        }
        EvidenceMutation::NotFinalized => {
            evidence.insert(
                "tx_hash".to_owned(),
                json!(format!("0x{}", "fe".repeat(32))),
            );
        }
    }
    Ok(())
}

fn key_from_payload(payload: &Value) -> Result<String> {
    let evidence: Evidence = serde_json::from_value(
        payload
            .get("evidence")
            .cloned()
            .context("evidence is missing")?,
    )?;
    let tx_hash = B256::from_str(&evidence.tx_hash)?;
    Ok(format!(
        "deposit:{}",
        deposit_id(evidence.chain_id, tx_hash, evidence.log_index)
    ))
}

fn unix_timestamp() -> Result<i64> {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    i64::try_from(seconds).context("system clock does not fit i64")
}

const ANVIL_PRIVATE_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const ANVIL_DEPLOYER: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";

fn command_available(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn run_checked(command: &str, arguments: &[&str], directory: Option<&Path>) -> Result<Output> {
    let mut invocation = Command::new(command);
    invocation.args(arguments);
    if let Some(directory) = directory {
        invocation.current_dir(directory);
    }
    let output = invocation.output()?;
    ensure!(
        output.status.success(),
        "{command} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

fn deploy_contract(
    contracts: &Path,
    rpc_url: &str,
    contract: &str,
    constructor_arguments: &[&str],
) -> Result<Address> {
    let mut arguments = vec![
        "create",
        "--rpc-url",
        rpc_url,
        "--private-key",
        ANVIL_PRIVATE_KEY,
        "--broadcast",
        "--json",
        contract,
    ];
    if !constructor_arguments.is_empty() {
        arguments.push("--constructor-args");
        arguments.extend_from_slice(constructor_arguments);
    }
    let output = run_checked("forge", &arguments, Some(contracts))?;
    let result: Value = serde_json::from_slice(&output.stdout)?;
    Address::from_str(
        result
            .get("deployedTo")
            .and_then(Value::as_str)
            .context("forge create omitted deployedTo")?,
    )
    .context("parse deployed contract address")
}

fn cast_call_address(
    rpc_url: &str,
    contract: Address,
    signature: &str,
    arguments: &[&str],
) -> Result<Address> {
    let output = Command::new("cast")
        .args([
            "call",
            "--rpc-url",
            rpc_url,
            &format!("{contract:#x}"),
            signature,
        ])
        .args(arguments)
        .output()?;
    ensure!(
        output.status.success(),
        "cast call failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Address::from_str(String::from_utf8(output.stdout)?.trim()).context("parse cast call address")
}

fn cast_send_json(
    rpc_url: &str,
    contract: Address,
    signature: &str,
    arguments: &[&str],
) -> Result<Value> {
    let output = Command::new("cast")
        .args([
            "send",
            "--rpc-url",
            rpc_url,
            "--private-key",
            ANVIL_PRIVATE_KEY,
            "--json",
            &format!("{contract:#x}"),
            signature,
        ])
        .args(arguments)
        .output()?;
    ensure!(
        output.status.success(),
        "cast send failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).context("parse cast send JSON")
}

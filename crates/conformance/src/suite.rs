//! Settlement endpoint conformance runner.

use std::collections::BTreeSet;
use std::fmt::{self, Display, Formatter};
use std::process::Command;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use alloy_primitives::{Address, B256};
use anyhow::{Context, Result, anyhow, bail, ensure};
use chrono::Utc;
use reqwest::header::HeaderValue;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::task::JoinSet;
use topup_adapters::settlement::http::{
    SettlementAnswer, SettlementApi as _, SettlementClient, SettlementRequest,
};
use topup_adapters::signer::actor::SignerHandle;
use topup_core::identity::deposit_id;
use url::Url;

use crate::chain::ChainFixture;
use crate::report::{Report, TestResult, TestStatus};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const READY_TIMEOUT: Duration = Duration::from_secs(60);
const CONCURRENT_REQUESTS: usize = 8;
const FIXTURE_AMOUNT_ATOMIC: u64 = 1_000_000;
/// Smallest per-deposit cap; the suite's ordinary requests use up to 200 minor units.
pub const MIN_PER_DEPOSIT_CAP: u64 = 201;
/// Smallest per-period cap; covers every credit the suite makes to the accepted account.
pub const MIN_PER_PERIOD_CAP: u64 = 2_000;
/// Upper bound on sequential requests needed to fill the per-period cap.
pub const MAX_PERIOD_FILL_REQUESTS: u64 = 32;
/// Shortest supported product period.
pub const MIN_PERIOD: Duration = Duration::from_secs(30);

/// Product-configured caps, in minor units.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    /// Independent per-deposit cap.
    pub per_deposit: u64,
    /// Independent cumulative cap per account and period.
    pub per_period: u64,
    /// Period length.
    pub period: Duration,
}

impl Caps {
    /// Rejects cap settings the suite cannot exercise deterministically.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (MIN_PER_DEPOSIT_CAP..u64::MAX).contains(&self.per_deposit),
            "--per-deposit-cap must be at least {MIN_PER_DEPOSIT_CAP} and below u64::MAX"
        );
        ensure!(
            self.per_period >= MIN_PER_PERIOD_CAP,
            "--per-period-cap must be at least {MIN_PER_PERIOD_CAP}"
        );
        ensure!(
            self.per_period.div_ceil(self.per_deposit) <= MAX_PERIOD_FILL_REQUESTS,
            "--per-period-cap may be at most {MAX_PERIOD_FILL_REQUESTS} per-deposit caps"
        );
        ensure!(
            self.period >= MIN_PERIOD,
            "--period-seconds must be at least {}",
            MIN_PERIOD.as_secs()
        );
        Ok(())
    }
}

/// Product accounts configured for conformance behavior.
#[derive(Clone, Debug)]
pub struct Accounts {
    /// Creditable account used by most cases.
    pub accepted: String,
    /// Account whose settlements the product refuses.
    pub refused: String,
    /// Account whose settlements stay in processing.
    pub processing: String,
    /// Creditable account used only to fill the per-period cap; must start empty.
    pub period: String,
}

impl Default for Accounts {
    fn default() -> Self {
        Self {
            accepted: "conformance-accepted".to_owned(),
            refused: "conformance-refused".to_owned(),
            processing: "conformance-processing".to_owned(),
            period: "conformance-period".to_owned(),
        }
    }
}

/// Restarts the product under test and returns once the restart was initiated.
#[async_trait::async_trait]
pub trait Restart: Send + Sync {
    /// Restarts the product process; the suite then waits for readiness.
    async fn restart(&self) -> Result<()>;
}

/// Restarts the product by running a shell command.
pub struct CommandRestart {
    command: String,
}

impl CommandRestart {
    /// Wraps a `sh -c` command line.
    #[must_use]
    pub fn new(command: String) -> Self {
        Self { command }
    }
}

#[async_trait::async_trait]
impl Restart for CommandRestart {
    async fn restart(&self) -> Result<()> {
        let command = self.command.clone();
        let status = tokio::task::spawn_blocking(move || {
            Command::new("sh").arg("-c").arg(&command).status()
        })
        .await?
        .context("run --restart-command")?;
        ensure!(status.success(), "--restart-command exited with {status}");
        Ok(())
    }
}

/// Inputs controlled by a product team when running the suite.
#[derive(Clone)]
pub struct SuiteConfig {
    /// Product settlement POST URL.
    pub settlement_url: String,
    /// Signer using the product's pinned test seed.
    pub signer: SignerHandle,
    /// Pinned key identifier paired with the public key.
    pub keyid: String,
    /// Product-configured caps.
    pub caps: Caps,
    /// Product accounts configured for conformance behavior.
    pub accounts: Accounts,
    /// Chain fixture read from the `prepare` manifest.
    pub chain: ChainFixture,
    /// Restart hook; without it the retention case is incomplete.
    pub restart: Option<Arc<dyn Restart>>,
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

/// Body of the required test-only ledger observation hook.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LedgerBody {
    /// Sum of all credits to the account, in minor units, as a decimal string.
    pub balance_minor: String,
    /// Number of ledger mutations (credits) applied to the account.
    pub mutations: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct Ledger {
    balance_minor: u64,
    mutations: u64,
}

impl Ledger {
    fn ensure_delta(self, after: Self, amount: u64, mutations: u64) -> Result<()> {
        ensure!(
            after.balance_minor.checked_sub(self.balance_minor) == Some(amount)
                && after.mutations.checked_sub(self.mutations) == Some(mutations),
            "expected ledger delta of {amount} minor units in {mutations} mutation(s), \
             observed {self:?} -> {after:?}"
        );
        Ok(())
    }
}

/// Marks a case whose required observation is unavailable; reported as `incomplete`.
#[derive(Debug)]
pub struct Incomplete(pub String);

impl Display for Incomplete {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Incomplete {}

/// Runs every case and returns a machine-readable report.
pub async fn run(config: SuiteConfig) -> Result<Report> {
    config.caps.validate()?;
    let started_at = Utc::now();
    let mut runner = Runner::new(config)?;
    runner.run_all().await;
    Ok(Report::complete(
        runner.config.settlement_url.clone(),
        runner.config.chain.manifest().clone(),
        started_at,
        runner.tests,
    ))
}

struct AcceptedFixture {
    request: SettlementRequest,
    destination_tx_id: String,
    sent_body: Vec<u8>,
}

/// A real Transfer log plus the (possibly false) claims a request makes about it.
#[derive(Clone, Copy)]
struct Emission {
    token: Address,
    recipient: Address,
    amount_atomic: u64,
    finalize: bool,
    claimed_amount_atomic: u64,
    claimed_log_offset: u64,
}

struct Runner {
    config: SuiteConfig,
    client: SettlementClient,
    raw_client: reqwest::Client,
    tests: Vec<TestResult>,
    accepted: Option<AcceptedFixture>,
    retained: Vec<SettlementRequest>,
}

impl Runner {
    fn new(config: SuiteConfig) -> Result<Self> {
        let (client, raw_client) = clients(&config)?;
        Ok(Self {
            config,
            client,
            raw_client,
            tests: Vec::new(),
            accepted: None,
            retained: Vec::new(),
        })
    }

    async fn run_all(&mut self) {
        let result = self.valid_accepted().await;
        self.capture(
            "accepted",
            Some(3),
            "valid request is committed before accepted and credits the ledger once",
            result,
        );
        let result = self.exact_replay().await;
        self.capture(
            "replay",
            Some(2),
            "exact replay returns the stored answer without a ledger mutation",
            result,
        );
        let result = self.payload_mismatch().await;
        self.capture(
            "payload_mismatch",
            Some(2),
            "same key with a different payload returns 422",
            result,
        );
        let result = self.concurrent_identical().await;
        self.capture(
            "concurrency",
            Some(3),
            "parallel identical requests mutate the ledger once",
            result,
        );
        let result = self.get_original().await;
        self.capture(
            "get_original",
            None,
            "GET returns status, destination id, and the original payload",
            result,
        );
        let result = self.invalid_signatures().await;
        self.capture(
            "authentication",
            Some(1),
            "invalid signature profiles are rejected",
            result,
        );
        let result = self.missing_idempotency_coverage().await;
        self.capture(
            "idempotency_coverage",
            Some(1),
            "idempotency-key must be covered by the signature",
            result,
        );
        let result = self.over_deposit_cap().await;
        self.capture(
            "per_deposit_cap",
            Some(4),
            "product enforces its configured per-deposit cap",
            result,
        );
        let result = self.per_period_cap().await;
        self.capture(
            "per_period_cap",
            Some(4),
            "product enforces its cumulative per-period cap atomically with the credit",
            result,
        );
        let result = self.chain_evidence().await;
        self.capture(
            "chain_evidence",
            Some(5),
            "product verifies log existence, emitter, recipient, amount, and finality on-chain",
            result,
        );
        let result = self.wrong_deposit_id().await;
        self.capture(
            "deposit_identity",
            Some(6),
            "idempotency key must match the recomputed deposit id",
            result,
        );
        let result = self.business_refusal().await;
        self.capture(
            "business_refusal",
            None,
            "business refusal is a typed 200 rejected answer without a credit",
            result,
        );
        let result = self.processing().await;
        self.capture(
            "processing",
            None,
            "processing is a typed 200 answer retained by GET",
            result,
        );
        let result = self.unknown_get().await;
        self.capture(
            "unknown_get",
            None,
            "GET of an unknown key is 404 or status unknown",
            result,
        );
        let result = self.restart_retention().await;
        self.capture(
            "restart_retention",
            Some(2),
            "records and ledger survive a restart; the oldest record still replays idempotently",
            result,
        );
    }

    fn capture(&mut self, id: &str, obligation: Option<u8>, name: &str, result: Result<Value>) {
        let (status, evidence) = match result {
            Ok(evidence) => (TestStatus::Pass, evidence),
            Err(error) => match error.downcast_ref::<Incomplete>() {
                Some(incomplete) => (
                    TestStatus::Incomplete,
                    json!({"reason": incomplete.to_string()}),
                ),
                None => (TestStatus::Fail, json!({"error": format!("{error:#}")})),
            },
        };
        self.tests.push(TestResult {
            id: id.to_owned(),
            name: name.to_owned(),
            obligation,
            status,
            evidence,
        });
    }

    async fn valid_accepted(&mut self) -> Result<Value> {
        let account = self.config.accounts.accepted.clone();
        let before = self.ledger(&account).await;
        let request = self.request(&account, 100).await?;
        let signed = self.client.signed_post_request(&request).await?;
        let sent_body = signed
            .body()
            .and_then(reqwest::Body::as_bytes)
            .context("signed request body must be buffered")?
            .to_vec();
        let response = self.raw_client.execute(signed).await?;
        let status = response.status().as_u16();
        let body = response.bytes().await?;
        ensure!(status == 200, "expected 200 accepted, received {status}");
        let answer: Value = serde_json::from_slice(&body).context("answer is not JSON")?;
        ensure!(
            answer.get("status") == Some(&json!("accepted")),
            "expected accepted, received {answer}"
        );
        let destination_tx_id = answer
            .get("destination_tx_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .context("accepted answer omitted destination_tx_id")?
            .to_owned();
        let fetched = self.client.get_by_key(&request.idempotency_key).await?;
        ensure!(
            matches!(
                &fetched,
                Some(SettlementAnswer::Accepted { destination_tx_id: fetched, .. })
                    if *fetched == destination_tx_id
            ),
            "accepted answer was not committed before the response: GET returned {fetched:?}"
        );
        self.retained.push(request.clone());
        self.accepted = Some(AcceptedFixture {
            request,
            destination_tx_id: destination_tx_id.clone(),
            sent_body,
        });
        let before = before?;
        let after = self.ledger(&account).await?;
        before.ensure_delta(after, 100, 1)?;
        Ok(json!({
            "destination_tx_id": destination_tx_id,
            "committed_before_response": true,
            "ledger_before": before,
            "ledger_after": after,
        }))
    }

    async fn exact_replay(&self) -> Result<Value> {
        let fixture = self.accepted()?;
        let account = &self.config.accounts.accepted;
        let before = self.ledger(account).await;
        let answer = self.client.post(&fixture.request).await?;
        let SettlementAnswer::Accepted {
            destination_tx_id, ..
        } = answer
        else {
            bail!("expected accepted replay, received {answer:?}");
        };
        ensure!(
            destination_tx_id == fixture.destination_tx_id,
            "replay changed destination_tx_id"
        );
        let before = before?;
        let after = self.ledger(account).await?;
        before.ensure_delta(after, 0, 0)?;
        Ok(json!({"destination_tx_id": destination_tx_id, "ledger_after": after}))
    }

    async fn payload_mismatch(&self) -> Result<Value> {
        let mut changed = self.accepted()?.request.clone();
        changed.payload["amount_minor"] = json!("101");
        let answer = self.client.post(&changed).await?;
        ensure!(
            answer == SettlementAnswer::PayloadMismatch422,
            "expected 422 payload mismatch, received {answer:?}"
        );
        Ok(json!({"http_status": 422}))
    }

    async fn concurrent_identical(&mut self) -> Result<Value> {
        let account = self.config.accounts.accepted.clone();
        let before = self.ledger(&account).await;
        let request = self.request(&account, 120).await?;
        let mut tasks = JoinSet::new();
        for _ in 0..CONCURRENT_REQUESTS {
            let client = self.client.clone();
            let request = request.clone();
            tasks.spawn(async move { client.post(&request).await });
        }
        let mut destinations = BTreeSet::new();
        let mut conflicts = 0_u64;
        while let Some(joined) = tasks.join_next().await {
            match joined.context("concurrent request task failed")?? {
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
            destinations.len() == 1,
            "concurrent requests returned {} destination ids",
            destinations.len()
        );
        self.retained.push(request);
        let before = before?;
        let after = self.ledger(&account).await?;
        before.ensure_delta(after, 120, 1)?;
        Ok(json!({
            "requests": CONCURRENT_REQUESTS,
            "conflicts": conflicts,
            "destination_tx_ids": destinations,
            "ledger_after": after,
        }))
    }

    async fn get_original(&self) -> Result<Value> {
        let fixture = self.accepted()?;
        let signed = self
            .client
            .signed_get_request(&fixture.request.idempotency_key)
            .await?;
        let response = self.raw_client.execute(signed).await?;
        let status = response.status().as_u16();
        let body = response.bytes().await?;
        ensure!(status == 200, "GET returned {status}");
        let answer: RawAnswer<'_> =
            serde_json::from_slice(&body).context("GET body is not a settlement answer")?;
        ensure!(
            answer.status == "accepted",
            "GET status is {}",
            answer.status
        );
        ensure!(
            answer.destination_tx_id.as_deref() == Some(fixture.destination_tx_id.as_str()),
            "GET destination id changed"
        );
        let returned = answer.payload.get().as_bytes();
        let semantically_equal =
            serde_json::from_slice::<Value>(returned)? == fixture.request.payload;
        ensure!(
            semantically_equal,
            "GET payload is not semantically equal to the payload sent"
        );
        Ok(json!({
            "destination_tx_id": fixture.destination_tx_id,
            "payload_semantically_equal": semantically_equal,
            "payload_byte_equal": returned == fixture.sent_body.as_slice(),
            "sent_sha256": hex::encode(Sha256::digest(&fixture.sent_body)),
            "returned_sha256": hex::encode(Sha256::digest(returned)),
        }))
    }

    async fn invalid_signatures(&self) -> Result<Value> {
        let request = self.request(&self.config.accounts.accepted, 130).await?;
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
        let mut changed = serde_json::to_vec(&request.payload)?;
        changed.push(b' ');
        *tampered.body_mut() = Some(reqwest::Body::from(changed));
        statuses.push(self.send_rejected(tampered).await?);

        Ok(json!({"statuses": statuses}))
    }

    async fn missing_idempotency_coverage(&self) -> Result<Value> {
        let request = self.request(&self.config.accounts.accepted, 140).await?;
        let signed = self
            .client
            .signed_post_request_at(&request, unix_timestamp()?, false)
            .await?;
        let status = self.send_rejected(signed).await?;
        Ok(json!({"http_status": status}))
    }

    async fn over_deposit_cap(&self) -> Result<Value> {
        let amount = self.config.caps.per_deposit.saturating_add(1);
        let request = self.request(&self.config.accounts.accepted, amount).await?;
        let answer = self.client.post(&request).await?;
        let SettlementAnswer::Rejected { reason, .. } = answer else {
            bail!("over-cap request was not rejected: {answer:?}");
        };
        Ok(json!({"cap": self.config.caps.per_deposit, "amount_minor": amount, "reason": reason}))
    }

    async fn per_period_cap(&mut self) -> Result<Value> {
        let caps = self.config.caps;
        let account = self.config.accounts.period.clone();
        let start = Instant::now();
        let before = self.ledger(&account).await?;
        ensure!(
            before
                == Ledger {
                    balance_minor: 0,
                    mutations: 0
                },
            "{account} must start with an empty ledger; use a fresh conformance namespace"
        );
        let result = self.fill_period(&account, caps).await;
        if start.elapsed() >= caps.period {
            return Err(Incomplete(format!(
                "the case took {}s, longer than the {}s period; rerun with a longer period",
                start.elapsed().as_secs(),
                caps.period.as_secs()
            ))
            .into());
        }
        result
    }

    async fn fill_period(&mut self, account: &str, caps: Caps) -> Result<Value> {
        let reserve = caps.per_deposit.min(caps.per_period);
        let fill_target = caps.per_period.saturating_sub(reserve);
        let mut credited = 0_u64;
        let mut fills = 0_u64;
        while credited < fill_target {
            let amount = caps.per_deposit.min(fill_target.saturating_sub(credited));
            let request = self.request(account, amount).await?;
            let answer = self.client.post(&request).await?;
            ensure!(
                matches!(answer, SettlementAnswer::Accepted { .. }),
                "credit {fills} of {amount} within the period cap was not accepted: {answer:?}"
            );
            if fills == 0 {
                self.retained.push(request);
            }
            credited = credited.saturating_add(amount);
            fills = fills.saturating_add(1);
        }

        let mut racing = Vec::with_capacity(CONCURRENT_REQUESTS);
        for _ in 0..CONCURRENT_REQUESTS {
            racing.push(self.request(account, reserve).await?);
        }
        let mut tasks = JoinSet::new();
        for request in racing {
            let client = self.client.clone();
            tasks.spawn(async move { post_until_terminal(&client, &request).await });
        }
        let mut accepted = 0_u64;
        let mut refused = 0_u64;
        while let Some(joined) = tasks.join_next().await {
            match joined.context("concurrent cap task failed")?? {
                SettlementAnswer::Accepted { .. } => accepted = accepted.saturating_add(1),
                SettlementAnswer::Rejected { .. } => refused = refused.saturating_add(1),
                other => bail!("unexpected answer while racing the period cap: {other:?}"),
            }
        }
        ensure!(
            accepted == 1,
            "{accepted} of {CONCURRENT_REQUESTS} concurrent requests for the last {reserve} \
             minor units were accepted; exactly one fits under the per-period cap"
        );

        let next = self.request(account, 1).await?;
        let answer = self.client.post(&next).await?;
        ensure!(
            matches!(answer, SettlementAnswer::Rejected { .. }),
            "request beyond the exhausted per-period cap was not refused: {answer:?}"
        );

        let after = self.ledger(account).await?;
        ensure!(
            after
                == Ledger {
                    balance_minor: caps.per_period,
                    mutations: fills.saturating_add(1)
                },
            "ledger must hold exactly the cap after the race, observed {after:?}"
        );
        Ok(json!({
            "per_period_cap": caps.per_period,
            "period_seconds": caps.period.as_secs(),
            "sequential_credits": fills,
            "concurrent_requests": CONCURRENT_REQUESTS,
            "concurrent_accepted": accepted,
            "concurrent_refused": refused,
            "next_request_refused": true,
            "ledger_after": after,
        }))
    }

    async fn chain_evidence(&self) -> Result<Value> {
        let account = &self.config.accounts.accepted;
        let valid = self.valid_emission(account);
        let manifest = self.config.chain.manifest();
        let mut cases = Vec::new();
        for (name, emission) in [
            (
                "missing_log",
                Emission {
                    claimed_log_offset: 1,
                    ..valid
                },
            ),
            (
                "unapproved_emitter",
                Emission {
                    token: manifest.unapproved_asset_contract,
                    ..valid
                },
            ),
            (
                "wrong_recipient",
                Emission {
                    recipient: manifest.funder,
                    ..valid
                },
            ),
            (
                "wrong_amount",
                Emission {
                    claimed_amount_atomic: valid.amount_atomic.saturating_add(1),
                    ..valid
                },
            ),
            (
                "not_finalized",
                Emission {
                    finalize: false,
                    ..valid
                },
            ),
        ] {
            let request = self.emit_request(account, 150, emission).await?;
            let answer = self.client.post(&request).await?;
            ensure!(
                matches!(answer, SettlementAnswer::Rejected { .. }),
                "{name}: request fields claim the approved token and the account forwarder, \
                 but the on-chain log does not match; expected rejected, received {answer:?}"
            );
            cases.push(json!({"case": name, "tx_hash": request.payload["evidence"]["tx_hash"]}));
        }
        let request = self.request(account, 150).await?;
        let answer = self.client.post(&request).await?;
        ensure!(
            matches!(answer, SettlementAnswer::Accepted { .. }),
            "valid finalized evidence was not accepted: {answer:?}"
        );
        Ok(json!({"rejected_counter_examples": cases, "valid_accepted": true}))
    }

    async fn wrong_deposit_id(&self) -> Result<Value> {
        let mut request = self.request(&self.config.accounts.accepted, 160).await?;
        request.idempotency_key = format!("deposit:{}", uuid::Uuid::new_v4());
        request.payload["idempotency_key"] = json!(request.idempotency_key);
        let answer = self.client.post(&request).await?;
        ensure!(
            matches!(answer, SettlementAnswer::Rejected { .. }),
            "wrong deposit id was accepted: {answer:?}"
        );
        Ok(json!({"rejected": true}))
    }

    async fn business_refusal(&mut self) -> Result<Value> {
        let account = self.config.accounts.refused.clone();
        let before = self.ledger(&account).await;
        let request = self.request(&account, 170).await?;
        let answer = self.client.post(&request).await?;
        let SettlementAnswer::Rejected { reason, .. } = answer else {
            bail!("expected business rejection, received {answer:?}");
        };
        self.retained.push(request);
        let before = before?;
        before.ensure_delta(self.ledger(&account).await?, 0, 0)?;
        Ok(json!({"reason": reason, "ledger_unchanged": true}))
    }

    async fn processing(&mut self) -> Result<Value> {
        let account = self.config.accounts.processing.clone();
        let before = self.ledger(&account).await;
        let request = self.request(&account, 180).await?;
        let answer = self.client.post(&request).await?;
        ensure!(
            matches!(answer, SettlementAnswer::Processing { .. }),
            "expected processing, received {answer:?}"
        );
        let fetched = self.client.get_by_key(&request.idempotency_key).await?;
        ensure!(
            matches!(fetched, Some(SettlementAnswer::Processing { .. })),
            "GET lost processing state: {fetched:?}"
        );
        self.retained.push(request);
        let before = before?;
        before.ensure_delta(self.ledger(&account).await?, 0, 0)?;
        Ok(json!({"status": "processing", "ledger_unchanged": true}))
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

    async fn restart_retention(&mut self) -> Result<Value> {
        let Some(restart) = self.config.restart.clone() else {
            return Err(Incomplete(
                "no restart hook: pass --restart-command so records can be re-read after a \
                 product restart"
                    .to_owned(),
            )
            .into());
        };
        let fixture = self.accepted()?;
        let (oldest, destination) = (fixture.request.clone(), fixture.destination_tx_id.clone());
        let accounts = self.account_ids();
        let mut ledgers = Vec::with_capacity(accounts.len());
        for account in &accounts {
            ledgers.push(self.ledger(account).await?);
        }
        let mut answers = Vec::with_capacity(self.retained.len());
        for request in &self.retained {
            answers.push(
                self.client
                    .get_by_key(&request.idempotency_key)
                    .await?
                    .with_context(|| {
                        format!("{} is unknown before restart", request.idempotency_key)
                    })?,
            );
        }

        restart.restart().await?;
        let (client, raw_client) = clients(&self.config)?;
        self.client = client;
        self.raw_client = raw_client;
        self.wait_ready().await?;

        for (request, before) in self.retained.iter().zip(&answers) {
            let after = self.client.get_by_key(&request.idempotency_key).await?;
            ensure!(
                after.as_ref() == Some(before),
                "{} changed across restart: {before:?} -> {after:?}",
                request.idempotency_key
            );
        }
        let replay = self.client.post(&oldest).await?;
        ensure!(
            matches!(
                &replay,
                SettlementAnswer::Accepted { destination_tx_id, .. } if *destination_tx_id == destination
            ),
            "replaying the oldest record after restart returned {replay:?}"
        );
        for (account, before) in accounts.iter().zip(&ledgers) {
            before.ensure_delta(self.ledger(account).await?, 0, 0)?;
        }
        Ok(json!({
            "records_rechecked": self.retained.len(),
            "oldest_replayed": oldest.idempotency_key,
            "ledgers_unchanged": accounts,
        }))
    }

    async fn wait_ready(&self) -> Result<()> {
        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            let probe = format!("deposit:{}", uuid::Uuid::new_v4());
            let not_ready = match self.client.get_by_key(&probe).await {
                Ok(Some(SettlementAnswer::Unknown { status, .. })) if status >= 500 => {
                    format!("GET returned {status}")
                }
                Ok(_) => return Ok(()),
                Err(error) => error.to_string(),
            };
            ensure!(
                Instant::now() < deadline,
                "product was not ready after restart: {not_ready}"
            );
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    fn account_ids(&self) -> Vec<String> {
        let accounts = &self.config.accounts;
        vec![
            accounts.accepted.clone(),
            accounts.refused.clone(),
            accounts.processing.clone(),
            accounts.period.clone(),
        ]
    }

    fn valid_emission(&self, account_id: &str) -> Emission {
        let manifest = self.config.chain.manifest();
        Emission {
            token: manifest.asset_contract,
            recipient: manifest.forwarder(account_id),
            amount_atomic: FIXTURE_AMOUNT_ATOMIC,
            finalize: true,
            claimed_amount_atomic: FIXTURE_AMOUNT_ATOMIC,
            claimed_log_offset: 0,
        }
    }

    async fn request(&self, account_id: &str, amount_minor: u64) -> Result<SettlementRequest> {
        self.emit_request(account_id, amount_minor, self.valid_emission(account_id))
            .await
    }

    /// Emits a real Transfer log and builds a request which always claims the approved token
    /// and the account's forwarder, whatever was actually emitted.
    async fn emit_request(
        &self,
        account_id: &str,
        amount_minor: u64,
        emission: Emission,
    ) -> Result<SettlementRequest> {
        let chain = &self.config.chain;
        let manifest = chain.manifest();
        let log = chain
            .mint(
                emission.token,
                emission.recipient,
                emission.amount_atomic,
                emission.finalize,
            )
            .await?;
        let log_index = log
            .log_index
            .checked_add(emission.claimed_log_offset)
            .context("log index overflow")?;
        let evidence = Evidence {
            chain_id: manifest.chain_id,
            asset_contract: format!("{:#x}", manifest.asset_contract),
            route: manifest.route.clone(),
            route_version: manifest.route_version,
            tx_hash: format!("{:#x}", log.tx_hash),
            log_index,
            to: format!("{:#x}", manifest.forwarder(account_id)),
            amount_atomic: emission.claimed_amount_atomic.to_string(),
            price_scaled: "100000000".to_owned(),
            price_scale: 8,
            valuation_at: Utc::now().to_rfc3339(),
            lock_ref: None,
        };
        let key = format!(
            "deposit:{}",
            deposit_id(manifest.chain_id, log.tx_hash, log_index)
        );
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

    fn accepted(&self) -> Result<&AcceptedFixture> {
        self.accepted
            .as_ref()
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

    async fn ledger(&self, account_id: &str) -> Result<Ledger> {
        let url = ledger_url(&self.config.settlement_url, account_id)?;
        let response = self
            .raw_client
            .get(url)
            .send()
            .await
            .context("ledger observation hook request failed")?;
        let status = response.status();
        if matches!(status.as_u16(), 404 | 405 | 501) {
            return Err(Incomplete(format!(
                "ledger observation hook GET {{settlement_url}}/_conformance/ledger/{{account_id}} \
                 returned {status}; it is required for a pass"
            ))
            .into());
        }
        ensure!(status.is_success(), "ledger hook returned {status}");
        let body: LedgerBody = response.json().await.context("parse ledger hook body")?;
        Ok(Ledger {
            balance_minor: body
                .balance_minor
                .parse()
                .context("balance_minor must be a decimal string")?,
            mutations: body.mutations,
        })
    }
}

#[derive(Deserialize)]
struct RawAnswer<'a> {
    status: String,
    destination_tx_id: Option<String>,
    #[serde(borrow)]
    payload: &'a RawValue,
}

async fn post_until_terminal(
    client: &SettlementClient,
    request: &SettlementRequest,
) -> Result<SettlementAnswer> {
    for _ in 0..50 {
        match client.post(request).await? {
            SettlementAnswer::Conflict409 | SettlementAnswer::Processing { .. } => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            answer => return Ok(answer),
        }
    }
    bail!(
        "{} never reached a terminal answer",
        request.idempotency_key
    )
}

fn clients(config: &SuiteConfig) -> Result<(SettlementClient, reqwest::Client)> {
    let client = SettlementClient::new_with_keyid(
        &config.settlement_url,
        config.signer.clone(),
        REQUEST_TIMEOUT,
        config.keyid.clone(),
    )?;
    let raw_client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    Ok((client, raw_client))
}

/// Returns `{settlement_url}/_conformance/ledger/{account_id}`.
pub fn ledger_url(settlement_url: &str, account_id: &str) -> Result<Url> {
    let mut url = Url::parse(settlement_url)?;
    url.path_segments_mut()
        .map_err(|()| anyhow!("settlement URL cannot be a base"))?
        .pop_if_empty()
        .push("_conformance")
        .push("ledger")
        .push(account_id);
    Ok(url)
}

/// Recomputes `deposit:<uuid>` from a payload's evidence.
pub fn key_from_evidence(evidence: &Evidence) -> Result<String> {
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

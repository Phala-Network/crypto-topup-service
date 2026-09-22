//! Axum reference implementation of the product-side settlement contract.
//!
//! The reference is runnable documentation for product teams: it verifies signatures with the
//! service's shared RFC 9421 verifier, keeps idempotency records forever, enforces both caps in
//! the same critical section as the credit, verifies the cited log against its own RPC, and
//! recomputes the deposit id. Each [`BrokenVariant`] removes exactly one of those obligations.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alloy_primitives::{Address, B256, U256, keccak256};
use anyhow::{Context, Result, ensure};
use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Path, Request, State};
use axum::http::header::HOST;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::Utc;
use ed25519_dalek::VerifyingKey;
use serde::Serialize;
use serde_json::Value;
use serde_json::value::RawValue;
use tokio::sync::Mutex;
use topup_adapters::http_signature::{self, SignedMessage};

use crate::chain::{Manifest, Rpc, parse_quantity};
use crate::suite::{Evidence, LedgerBody, key_from_evidence};

#[cfg(feature = "postgres")]
use sqlx::{PgPool, Row};

const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Window which makes check-then-act defects in broken variants observable.
const RACE_WINDOW: Duration = Duration::from_millis(100);

/// Deliberate single-obligation failures used to prove the suite is sensitive.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BrokenVariant {
    /// Fully conforming behavior.
    #[default]
    None,
    /// Obligation 1: accepts requests without verifying the pinned signature.
    Signature,
    /// Obligation 2: answers a reused key from its record without comparing payloads.
    Idempotency,
    /// Obligation 2: idempotency records live in process memory and vanish on restart.
    Retention,
    /// Obligation 3: checks for an existing record outside the critical section of the credit.
    Concurrency,
    /// Obligation 4: enforces neither the per-deposit nor the per-period cap.
    Caps,
    /// Obligation 4: checks the per-period cap outside the critical section of the credit.
    PeriodCapRace,
    /// Obligation 5: trusts the request's evidence fields without consulting its RPC.
    Evidence,
    /// Obligation 6: does not recompute the deterministic deposit id.
    DepositIdentity,
}

impl BrokenVariant {
    /// Every deliberately broken variant.
    pub const BROKEN: [Self; 8] = [
        Self::Signature,
        Self::Idempotency,
        Self::Retention,
        Self::Concurrency,
        Self::Caps,
        Self::PeriodCapRace,
        Self::Evidence,
        Self::DepositIdentity,
    ];

    /// Architecture section 11 obligation this variant violates.
    #[must_use]
    pub fn obligation(self) -> Option<u8> {
        match self {
            Self::None => None,
            Self::Signature => Some(1),
            Self::Idempotency | Self::Retention => Some(2),
            Self::Concurrency => Some(3),
            Self::Caps | Self::PeriodCapRace => Some(4),
            Self::Evidence => Some(5),
            Self::DepositIdentity => Some(6),
        }
    }
}

impl FromStr for BrokenVariant {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "none" => Ok(Self::None),
            "signature" => Ok(Self::Signature),
            "idempotency" => Ok(Self::Idempotency),
            "retention" => Ok(Self::Retention),
            "concurrency" => Ok(Self::Concurrency),
            "caps" => Ok(Self::Caps),
            "period-cap-race" => Ok(Self::PeriodCapRace),
            "evidence" => Ok(Self::Evidence),
            "deposit-identity" => Ok(Self::DepositIdentity),
            _ => anyhow::bail!("unknown broken variant {value}"),
        }
    }
}

/// Reference endpoint configuration.
#[derive(Clone)]
pub struct ReferenceConfig {
    /// Public key paired with `keyid`.
    pub verifying_key: VerifyingKey,
    /// Required RFC 9421 key identifier.
    pub keyid: String,
    /// Independent per-deposit cap in minor units.
    pub per_deposit_cap: u64,
    /// Independent cumulative cap per account and rolling period, in minor units.
    pub per_period_cap: u64,
    /// Rolling period length.
    pub period: Duration,
    /// Account id which returns a business refusal.
    pub refused_account_id: String,
    /// Account id which remains in processing.
    pub processing_account_id: String,
    /// Deliberate failure mode.
    pub broken: BrokenVariant,
    /// Approved route and chain, with the product's own RPC URL in `rpc_url`.
    pub manifest: Manifest,
}

/// Shared reference state.
#[derive(Clone)]
pub struct ReferenceState {
    config: Arc<ReferenceConfig>,
    storage: Arc<Storage>,
    rpc: Rpc,
}

impl ReferenceState {
    /// Creates a reference backed by in-memory storage.
    pub fn new(config: ReferenceConfig) -> Result<Self> {
        let rpc = Rpc::new(&config.manifest.rpc_url)?;
        Ok(Self {
            config: Arc::new(config),
            storage: Arc::new(Storage::Memory(Mutex::new(MemoryStore::default()))),
            rpc,
        })
    }

    /// Creates the same reference backed by PostgreSQL tables; broken variants are memory-only.
    #[cfg(feature = "postgres")]
    pub async fn new_postgres(config: ReferenceConfig, pool: PgPool) -> Result<Self> {
        ensure!(
            config.broken == BrokenVariant::None,
            "broken variants require in-memory storage"
        );
        for statement in [
            "CREATE TABLE IF NOT EXISTS conformance_settlements (\
             key text PRIMARY KEY, payload text NOT NULL, status text NOT NULL, \
             destination_tx_id text, reason text)",
            "CREATE TABLE IF NOT EXISTS conformance_accounts (\
             account_id text PRIMARY KEY, balance_minor bigint NOT NULL, \
             mutations bigint NOT NULL)",
            "CREATE TABLE IF NOT EXISTS conformance_credits (\
             key text PRIMARY KEY REFERENCES conformance_settlements (key), \
             account_id text NOT NULL, amount_minor bigint NOT NULL, \
             credited_at timestamptz NOT NULL DEFAULT now())",
        ] {
            sqlx::query(statement).execute(&pool).await?;
        }
        let rpc = Rpc::new(&config.manifest.rpc_url)?;
        Ok(Self {
            config: Arc::new(config),
            storage: Arc::new(Storage::Postgres(pool)),
            rpc,
        })
    }

    /// Simulates a process restart over the same durable storage.
    ///
    /// The [`BrokenVariant::Retention`] variant keeps idempotency records only in process memory,
    /// so they are lost here while the ledger survives.
    pub async fn restarted(&self) -> Self {
        if self.config.broken == BrokenVariant::Retention
            && let Storage::Memory(store) = self.storage.as_ref()
        {
            store.lock().await.records.clear();
        }
        self.clone()
    }
}

/// Builds the reference product router.
pub fn router(state: ReferenceState) -> Router {
    Router::new()
        .route("/settlements", post(post_settlement))
        .route("/settlements/{key}", get(get_settlement))
        .route(
            "/settlements/_conformance/ledger/{account_id}",
            get(ledger_hook),
        )
        .with_state(state)
}

#[derive(Clone)]
struct Record {
    raw_payload: Box<RawValue>,
    payload: Value,
    status: StoredStatus,
}

#[derive(Clone)]
enum StoredStatus {
    Accepted { destination_tx_id: String },
    Processing,
    Rejected { reason: String },
}

/// Outcome of the stateless checks, applied atomically by storage.
enum Decision {
    Credit { account_id: String, amount: u64 },
    Processing,
    Rejected(&'static str),
}

enum Resolution {
    Record(Record),
    Mismatch,
}

#[derive(Default)]
struct MemoryStore {
    records: HashMap<String, Record>,
    accounts: HashMap<String, Account>,
}

#[derive(Default)]
struct Account {
    balance_minor: u64,
    mutations: u64,
    credits: Vec<(Instant, u64)>,
}

impl MemoryStore {
    fn existing(&self, key: &str, payload: &Value, compare: bool) -> Option<Resolution> {
        let record = self.records.get(key)?;
        Some(if !compare || record.payload == *payload {
            Resolution::Record(record.clone())
        } else {
            Resolution::Mismatch
        })
    }

    fn period_total(&self, account_id: &str, period: Duration) -> u64 {
        self.accounts.get(account_id).map_or(0, |account| {
            account
                .credits
                .iter()
                .filter(|(at, _)| at.elapsed() < period)
                .fold(0_u64, |total, (_, amount)| total.saturating_add(*amount))
        })
    }

    /// Applies a decision; `period_cap` is `None` when the cap was already decided elsewhere.
    fn apply(
        &mut self,
        key: &str,
        record: (Box<RawValue>, Value),
        decision: Decision,
        period_cap: Option<(u64, Duration)>,
    ) -> Record {
        let status = match decision {
            Decision::Credit { account_id, amount } => {
                let within = period_cap.is_none_or(|(cap, period)| {
                    self.period_total(&account_id, period)
                        .checked_add(amount)
                        .is_some_and(|total| total <= cap)
                });
                if within {
                    let account = self.accounts.entry(account_id).or_default();
                    account.balance_minor = account.balance_minor.saturating_add(amount);
                    account.mutations = account.mutations.saturating_add(1);
                    account.credits.push((Instant::now(), amount));
                    StoredStatus::Accepted {
                        destination_tx_id: format!("credit-{}", uuid::Uuid::new_v4()),
                    }
                } else {
                    StoredStatus::Rejected {
                        reason: "per_period_cap".to_owned(),
                    }
                }
            }
            Decision::Processing => StoredStatus::Processing,
            Decision::Rejected(reason) => StoredStatus::Rejected {
                reason: reason.to_owned(),
            },
        };
        let record = Record {
            raw_payload: record.0,
            payload: record.1,
            status,
        };
        self.records.insert(key.to_owned(), record.clone());
        record
    }
}

enum Storage {
    Memory(Mutex<MemoryStore>),
    #[cfg(feature = "postgres")]
    Postgres(PgPool),
}

impl Storage {
    /// Atomically finds or creates the record for `key`, applying the credit and the
    /// per-period cap in the same critical section.
    async fn settle(
        &self,
        config: &ReferenceConfig,
        key: &str,
        raw_payload: Box<RawValue>,
        payload: Value,
        decision: Decision,
    ) -> Result<Resolution> {
        let broken = config.broken;
        let period_cap =
            (broken != BrokenVariant::Caps).then_some((config.per_period_cap, config.period));
        match self {
            Self::Memory(store) => {
                let compare = broken != BrokenVariant::Idempotency;
                match broken {
                    BrokenVariant::Concurrency => {
                        if let Some(existing) = store.lock().await.existing(key, &payload, compare)
                        {
                            return Ok(existing);
                        }
                        tokio::time::sleep(RACE_WINDOW).await;
                        let mut store = store.lock().await;
                        Ok(Resolution::Record(store.apply(
                            key,
                            (raw_payload, payload),
                            decision,
                            period_cap,
                        )))
                    }
                    BrokenVariant::PeriodCapRace => {
                        let decision = {
                            let store = store.lock().await;
                            if let Some(existing) = store.existing(key, &payload, compare) {
                                return Ok(existing);
                            }
                            match decision {
                                Decision::Credit { account_id, amount }
                                    if store
                                        .period_total(&account_id, config.period)
                                        .saturating_add(amount)
                                        > config.per_period_cap =>
                                {
                                    Decision::Rejected("per_period_cap")
                                }
                                other => other,
                            }
                        };
                        tokio::time::sleep(RACE_WINDOW).await;
                        let mut store = store.lock().await;
                        if let Some(existing) = store.existing(key, &payload, compare) {
                            return Ok(existing);
                        }
                        Ok(Resolution::Record(store.apply(
                            key,
                            (raw_payload, payload),
                            decision,
                            None,
                        )))
                    }
                    _ => {
                        let mut store = store.lock().await;
                        if let Some(existing) = store.existing(key, &payload, compare) {
                            return Ok(existing);
                        }
                        Ok(Resolution::Record(store.apply(
                            key,
                            (raw_payload, payload),
                            decision,
                            period_cap,
                        )))
                    }
                }
            }
            #[cfg(feature = "postgres")]
            Self::Postgres(pool) => {
                settle_postgres(pool, config, key, raw_payload, payload, decision).await
            }
        }
    }

    async fn get(&self, key: &str) -> Result<Option<Record>> {
        match self {
            Self::Memory(store) => Ok(store.lock().await.records.get(key).cloned()),
            #[cfg(feature = "postgres")]
            Self::Postgres(pool) => get_postgres(pool, key).await,
        }
    }

    async fn ledger(&self, account_id: &str) -> Result<(u64, u64)> {
        match self {
            Self::Memory(store) => Ok(store
                .lock()
                .await
                .accounts
                .get(account_id)
                .map_or((0, 0), |account| (account.balance_minor, account.mutations))),
            #[cfg(feature = "postgres")]
            Self::Postgres(pool) => {
                let row = sqlx::query(
                    "SELECT balance_minor, mutations FROM conformance_accounts \
                     WHERE account_id = $1",
                )
                .bind(account_id)
                .fetch_optional(pool)
                .await?;
                match row {
                    Some(row) => Ok((
                        u64::try_from(row.get::<i64, _>("balance_minor"))?,
                        u64::try_from(row.get::<i64, _>("mutations"))?,
                    )),
                    None => Ok((0, 0)),
                }
            }
        }
    }
}

async fn post_settlement(State(state): State<ReferenceState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let Ok(body) = to_bytes(body, MAX_BODY_BYTES).await else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if state.config.broken != BrokenVariant::Signature
        && verify_signature(&state.config, "POST", &parts.uri, &parts.headers, &body).is_err()
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(raw_payload) = std::str::from_utf8(&body)
        .map_err(|_| ())
        .and_then(|text| RawValue::from_string(text.to_owned()).map_err(|_| ()))
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(payload) = serde_json::from_str::<Value>(raw_payload.get()) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(key) = idempotency_key(&parts.headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if payload.get("idempotency_key").and_then(Value::as_str) != Some(key.as_str()) {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }

    let decision = decide(&state, &key, &payload).await;
    match state
        .storage
        .settle(&state.config, &key, raw_payload, payload, decision)
        .await
    {
        Ok(Resolution::Record(record)) => record_response(&record),
        Ok(Resolution::Mismatch) => StatusCode::UNPROCESSABLE_ENTITY.into_response(),
        Err(error) => {
            tracing::error!(%error, "reference settlement storage failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn decide(state: &ReferenceState, key: &str, payload: &Value) -> Decision {
    let config = &state.config;
    let account_id = payload
        .get("account_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if account_id == config.refused_account_id {
        return Decision::Rejected("business_refusal");
    }
    if account_id == config.processing_account_id {
        return Decision::Processing;
    }
    let Some(amount) = payload
        .get("amount_minor")
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|amount| *amount > 0)
    else {
        return Decision::Rejected("invalid_amount");
    };
    if config.broken != BrokenVariant::Caps && amount > config.per_deposit_cap {
        return Decision::Rejected("per_deposit_cap");
    }
    let Ok(evidence) = payload
        .get("evidence")
        .cloned()
        .context("missing evidence")
        .and_then(|evidence| serde_json::from_value::<Evidence>(evidence).map_err(Into::into))
    else {
        return Decision::Rejected("invalid_chain_evidence");
    };
    if let Err(error) = validate_evidence(state, account_id, &evidence).await {
        tracing::info!(%error, "reference rejected chain evidence");
        return Decision::Rejected("invalid_chain_evidence");
    }
    if config.broken != BrokenVariant::DepositIdentity
        && key_from_evidence(&evidence).ok().as_deref() != Some(key)
    {
        return Decision::Rejected("deposit_identity_mismatch");
    }
    Decision::Credit {
        account_id: account_id.to_owned(),
        amount,
    }
}

async fn get_settlement(
    State(state): State<ReferenceState>,
    Path(key): Path<String>,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();
    let Ok(body) = to_bytes(body, MAX_BODY_BYTES).await else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if state.config.broken != BrokenVariant::Signature
        && verify_signature(&state.config, "GET", &parts.uri, &parts.headers, &body).is_err()
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match state.storage.get(&key).await {
        Ok(Some(record)) => record_response(&record),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!(%error, "reference settlement lookup failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

async fn ledger_hook(
    State(state): State<ReferenceState>,
    Path(account_id): Path<String>,
) -> Response {
    match state.storage.ledger(&account_id).await {
        Ok((balance_minor, mutations)) => axum::Json(LedgerBody {
            balance_minor: balance_minor.to_string(),
            mutations,
        })
        .into_response(),
        Err(error) => {
            tracing::error!(%error, "reference ledger lookup failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[derive(Serialize)]
struct AnswerBody<'a> {
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    destination_tx_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
    payload: &'a RawValue,
}

/// Answers with the stored payload bytes exactly as they were received.
fn record_response(record: &Record) -> Response {
    let (status, destination_tx_id, reason) = match &record.status {
        StoredStatus::Accepted { destination_tx_id } => {
            ("accepted", Some(destination_tx_id.as_str()), None)
        }
        StoredStatus::Processing => ("processing", None, None),
        StoredStatus::Rejected { reason } => ("rejected", None, Some(reason.as_str())),
    };
    axum::Json(AnswerBody {
        status,
        destination_tx_id,
        reason,
        payload: &record.raw_payload,
    })
    .into_response()
}

#[cfg(feature = "postgres")]
async fn settle_postgres(
    pool: &PgPool,
    config: &ReferenceConfig,
    key: &str,
    raw_payload: Box<RawValue>,
    payload: Value,
    decision: Decision,
) -> Result<Resolution> {
    let mut transaction = pool.begin().await?;
    if let Some(record) = select_record(&mut transaction, key).await? {
        transaction.commit().await?;
        return Ok(resolution(record, &payload));
    }
    let (status, destination_tx_id, reason, credit) = match decision {
        Decision::Credit { account_id, amount } => {
            let amount_i64 = i64::try_from(amount)?;
            sqlx::query(
                "INSERT INTO conformance_accounts (account_id, balance_minor, mutations) \
                 VALUES ($1, 0, 0) ON CONFLICT (account_id) DO NOTHING",
            )
            .bind(&account_id)
            .execute(&mut *transaction)
            .await?;
            // The row lock serializes every credit to the account, so the period total read
            // below cannot change before this transaction's credit commits.
            sqlx::query("SELECT 1 FROM conformance_accounts WHERE account_id = $1 FOR UPDATE")
                .bind(&account_id)
                .execute(&mut *transaction)
                .await?;
            let total: i64 = sqlx::query_scalar(
                "SELECT COALESCE(SUM(amount_minor), 0)::bigint FROM conformance_credits \
                 WHERE account_id = $1 AND credited_at > now() - $2 * interval '1 second'",
            )
            .bind(&account_id)
            .bind(i64::try_from(config.period.as_secs())?)
            .fetch_one(&mut *transaction)
            .await?;
            if u64::try_from(total)?.saturating_add(amount) > config.per_period_cap {
                ("rejected", None, Some("per_period_cap".to_owned()), None)
            } else {
                (
                    "accepted",
                    Some(format!("credit-{}", uuid::Uuid::new_v4())),
                    None,
                    Some((account_id, amount_i64)),
                )
            }
        }
        Decision::Processing => ("processing", None, None, None),
        Decision::Rejected(reason) => ("rejected", None, Some(reason.to_owned()), None),
    };
    let inserted = sqlx::query(
        "INSERT INTO conformance_settlements \
         (key, payload, status, destination_tx_id, reason) VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (key) DO NOTHING",
    )
    .bind(key)
    .bind(raw_payload.get())
    .bind(status)
    .bind(&destination_tx_id)
    .bind(&reason)
    .execute(&mut *transaction)
    .await?
    .rows_affected()
        == 1;
    if !inserted {
        // A concurrent request with the same key committed first; adopt its record.
        transaction.rollback().await?;
        let record = get_postgres(pool, key)
            .await?
            .context("conflicting settlement disappeared")?;
        return Ok(resolution(record, &payload));
    }
    if let Some((account_id, amount)) = credit {
        sqlx::query(
            "INSERT INTO conformance_credits (key, account_id, amount_minor) VALUES ($1, $2, $3)",
        )
        .bind(key)
        .bind(&account_id)
        .bind(amount)
        .execute(&mut *transaction)
        .await?;
        sqlx::query(
            "UPDATE conformance_accounts SET balance_minor = balance_minor + $2, \
             mutations = mutations + 1 WHERE account_id = $1",
        )
        .bind(&account_id)
        .bind(amount)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    let status = match (status, destination_tx_id, reason) {
        ("accepted", Some(destination_tx_id), _) => StoredStatus::Accepted { destination_tx_id },
        ("rejected", _, Some(reason)) => StoredStatus::Rejected { reason },
        _ => StoredStatus::Processing,
    };
    Ok(Resolution::Record(Record {
        raw_payload,
        payload,
        status,
    }))
}

#[cfg(feature = "postgres")]
fn resolution(record: Record, payload: &Value) -> Resolution {
    if record.payload == *payload {
        Resolution::Record(record)
    } else {
        Resolution::Mismatch
    }
}

#[cfg(feature = "postgres")]
async fn select_record(transaction: &mut sqlx::PgConnection, key: &str) -> Result<Option<Record>> {
    sqlx::query(
        "SELECT payload, status, destination_tx_id, reason \
         FROM conformance_settlements WHERE key = $1",
    )
    .bind(key)
    .fetch_optional(transaction)
    .await?
    .map(|row| record_from_row(&row))
    .transpose()
}

#[cfg(feature = "postgres")]
async fn get_postgres(pool: &PgPool, key: &str) -> Result<Option<Record>> {
    let mut connection = pool.acquire().await?;
    select_record(&mut connection, key).await
}

#[cfg(feature = "postgres")]
fn record_from_row(row: &sqlx::postgres::PgRow) -> Result<Record> {
    let status = match row.get::<String, _>("status").as_str() {
        "accepted" => StoredStatus::Accepted {
            destination_tx_id: row
                .get::<Option<String>, _>("destination_tx_id")
                .context("accepted row omitted destination_tx_id")?,
        },
        "processing" => StoredStatus::Processing,
        "rejected" => StoredStatus::Rejected {
            reason: row
                .get::<Option<String>, _>("reason")
                .context("rejected row omitted reason")?,
        },
        other => anyhow::bail!("unknown stored status {other}"),
    };
    let raw_payload = RawValue::from_string(row.get::<String, _>("payload"))?;
    let payload = serde_json::from_str(raw_payload.get())?;
    Ok(Record {
        raw_payload,
        payload,
        status,
    })
}

async fn validate_evidence(
    state: &ReferenceState,
    account_id: &str,
    evidence: &Evidence,
) -> Result<()> {
    let manifest = &state.config.manifest;
    ensure!(
        evidence.route == manifest.route && evidence.route_version == manifest.route_version,
        "wrong route"
    );
    ensure!(evidence.chain_id == manifest.chain_id, "wrong chain id");
    ensure!(
        Address::from_str(&evidence.asset_contract)? == manifest.asset_contract,
        "asset is not approved for the route"
    );
    let expected_to = manifest.forwarder(account_id);
    ensure!(
        Address::from_str(&evidence.to)? == expected_to,
        "to is not the account's forwarder"
    );
    if state.config.broken == BrokenVariant::Evidence {
        return Ok(());
    }

    let rpc = &state.rpc;
    let receipt = rpc
        .receipt(B256::from_str(&evidence.tx_hash)?)
        .await?
        .context("transaction receipt does not exist")?;
    let log = receipt
        .get("logs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|log| {
            log.get("logIndex")
                .and_then(|value| parse_quantity(value).ok())
                == Some(evidence.log_index)
        })
        .context("cited log does not exist")?;
    ensure!(
        log.get("address")
            .and_then(Value::as_str)
            .map(Address::from_str)
            .transpose()?
            == Some(manifest.asset_contract),
        "log was not emitted by the approved asset contract"
    );
    let transfer_topic = format!("{:#x}", keccak256("Transfer(address,address,uint256)"));
    ensure!(
        log.pointer("/topics/0").and_then(Value::as_str) == Some(transfer_topic.as_str()),
        "log is not an ERC-20 Transfer"
    );
    let recipient = B256::from_str(
        log.pointer("/topics/2")
            .and_then(Value::as_str)
            .context("Transfer log omitted recipient")?,
    )?;
    ensure!(
        recipient.as_slice().get(12..) == Some(expected_to.as_slice()),
        "Transfer recipient is not the account's forwarder"
    );
    let amount = log
        .get("data")
        .and_then(Value::as_str)
        .context("Transfer log omitted amount")?;
    ensure!(
        U256::from_str(amount)? == U256::from_str(&evidence.amount_atomic)?,
        "Transfer amount differs from amount_atomic"
    );
    let receipt_block =
        parse_quantity(receipt.get("blockNumber").context("receipt is not mined")?)?;
    ensure!(
        receipt_block <= rpc.finalized_block().await?,
        "block is not finalized"
    );
    Ok(())
}

fn verify_signature(
    config: &ReferenceConfig,
    method: &str,
    uri: &axum::http::Uri,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<()> {
    let host = header(headers, HOST.as_str())?;
    let path = uri.path_and_query().map_or("/", |value| value.as_str());
    let target_uri = format!("http://{host}{path}");
    let verified = http_signature::verify(
        &SignedMessage {
            method,
            target_uri: &target_uri,
            content_digest: header(headers, "content-digest")?,
            idempotency_key: Some(header(headers, "idempotency-key")?),
            signature_input: header(headers, "signature-input")?,
            signature: header(headers, "signature")?,
            body,
        },
        &config.keyid,
        &config.verifying_key,
        Utc::now().timestamp(),
    )?;
    ensure!(
        verified.covers_idempotency_key,
        "settlement signatures must cover idempotency-key"
    );
    Ok(())
}

/// Parses the `Idempotency-Key` Structured Field string.
fn idempotency_key(headers: &HeaderMap) -> Result<String> {
    let value = header(headers, "idempotency-key")?.trim();
    let inner = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .context("idempotency-key must be a structured field string")?;
    let mut key = String::with_capacity(inner.len());
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        match character {
            '\\' => match characters.next() {
                Some(escaped @ ('"' | '\\')) => key.push(escaped),
                _ => anyhow::bail!("invalid escape in idempotency-key"),
            },
            '"' => anyhow::bail!("unescaped quote in idempotency-key"),
            character => key.push(character),
        }
    }
    Ok(key)
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str> {
    headers
        .get(name)
        .context("required header is missing")?
        .to_str()
        .context("header is not ASCII")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotency_key_unescapes_structured_field_strings() -> Result<()> {
        let mut headers = HeaderMap::new();
        headers.insert("idempotency-key", r#""deposit:\"a\\b""#.parse()?);
        assert_eq!(idempotency_key(&headers)?, r#"deposit:"a\b"#);
        headers.insert("idempotency-key", r#""bad\x""#.parse()?);
        assert!(idempotency_key(&headers).is_err());
        Ok(())
    }
}

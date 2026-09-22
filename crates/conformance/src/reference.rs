//! Axum reference implementation of the product-side settlement contract.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use alloy_primitives::{Address, B256, U256, keccak256};
use anyhow::{Context, Result, ensure};
use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Path, Request, State};
use axum::http::header::HOST;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::Utc;
use ed25519_dalek::{Signature, VerifyingKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;
use topup_core::address::{forwarder_address, persistent_salt};
use topup_core::identity::deposit_id;

use crate::suite::Evidence;

#[cfg(feature = "postgres")]
use sqlx::{PgPool, Row};

const MAX_BODY_BYTES: usize = 1024 * 1024;
const SYNTHETIC_ASSET: &str = "0x1111111111111111111111111111111111111111";
const SYNTHETIC_FACTORY: &str = "0x2222222222222222222222222222222222222222";
const SYNTHETIC_IMPLEMENTATION: &str = "0x3333333333333333333333333333333333333333";

/// Deliberate single-obligation failures used to prove the suite is sensitive.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum BrokenVariant {
    /// Fully conforming behavior.
    #[default]
    None,
    /// Accepts requests without validating the pinned signature profile.
    Signature,
    /// Discards idempotency records and credits exact replays again.
    Idempotency,
    /// Mutates the ledger for every concurrent replay.
    Concurrency,
    /// Does not enforce the product-owned cap.
    Caps,
    /// Trusts supplied chain evidence.
    Evidence,
    /// Does not recompute the deterministic deposit id.
    DepositIdentity,
}

impl FromStr for BrokenVariant {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "none" => Ok(Self::None),
            "signature" => Ok(Self::Signature),
            "idempotency" => Ok(Self::Idempotency),
            "concurrency" => Ok(Self::Concurrency),
            "caps" => Ok(Self::Caps),
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
    /// Independent product cap in minor units.
    pub per_deposit_cap: u64,
    /// Account id which returns a business refusal.
    pub refused_account_id: String,
    /// Account id which remains in processing.
    pub processing_account_id: String,
    /// Deliberate failure mode.
    pub broken: BrokenVariant,
    /// Synthetic or RPC-backed evidence verification.
    pub evidence_policy: EvidencePolicy,
}

/// Product route configuration used to verify chain evidence.
#[derive(Clone)]
pub enum EvidencePolicy {
    /// Deterministic no-network fixture used by unit and integration tests.
    Synthetic,
    /// Independent verification against an Anvil JSON-RPC endpoint.
    Rpc(RpcEvidenceConfig),
}

/// Approved route values for RPC-backed verification.
#[derive(Clone)]
pub struct RpcEvidenceConfig {
    /// JSON-RPC endpoint queried by the product.
    pub rpc_url: String,
    /// Approved EVM chain id.
    pub chain_id: u64,
    /// Approved mock token contract.
    pub asset_contract: Address,
    /// A1 forwarder factory.
    pub factory: Address,
    /// A1 forwarder implementation.
    pub implementation: Address,
}

/// Shared in-memory reference state.
#[derive(Clone)]
pub struct ReferenceState {
    config: ReferenceConfig,
    storage: Arc<Storage>,
    inflight: Arc<Mutex<HashSet<String>>>,
}

impl ReferenceState {
    /// Creates an empty reference state.
    #[must_use]
    pub fn new(config: ReferenceConfig) -> Self {
        Self {
            config,
            storage: Arc::new(Storage::Memory {
                records: Mutex::new(HashMap::new()),
                ledger: Mutex::new(HashMap::new()),
            }),
            inflight: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Creates the same reference contract backed by PostgreSQL tables.
    #[cfg(feature = "postgres")]
    pub async fn new_postgres(config: ReferenceConfig, pool: PgPool) -> Result<Self> {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS conformance_settlements (\
             key text PRIMARY KEY, payload jsonb NOT NULL, status text NOT NULL, \
             destination_tx_id text, reason text)",
        )
        .execute(&pool)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS conformance_ledger (\
             key text PRIMARY KEY, mutations bigint NOT NULL CHECK (mutations > 0))",
        )
        .execute(&pool)
        .await?;
        Ok(Self {
            config,
            storage: Arc::new(Storage::Postgres(pool)),
            inflight: Arc::new(Mutex::new(HashSet::new())),
        })
    }
}

enum Storage {
    Memory {
        records: Mutex<HashMap<String, Record>>,
        ledger: Mutex<HashMap<String, u64>>,
    },
    #[cfg(feature = "postgres")]
    Postgres(PgPool),
}

enum Resolution {
    Record(Record),
    Mismatch,
}

impl Storage {
    async fn resolve(
        &self,
        key: &str,
        payload: Value,
        proposed: StoredStatus,
    ) -> Result<Resolution> {
        match self {
            Self::Memory { records, ledger } => {
                let mut records = records.lock().await;
                if let Some(record) = records.get(key) {
                    return Ok(if record.payload == payload {
                        Resolution::Record(record.clone())
                    } else {
                        Resolution::Mismatch
                    });
                }
                let record = Record {
                    payload,
                    status: proposed,
                };
                if matches!(record.status, StoredStatus::Accepted { .. }) {
                    ledger.lock().await.insert(key.to_owned(), 1);
                }
                records.insert(key.to_owned(), record.clone());
                Ok(Resolution::Record(record))
            }
            #[cfg(feature = "postgres")]
            Self::Postgres(pool) => resolve_postgres(pool, key, payload, proposed).await,
        }
    }

    async fn get(&self, key: &str) -> Result<Option<Record>> {
        match self {
            Self::Memory { records, .. } => Ok(records.lock().await.get(key).cloned()),
            #[cfg(feature = "postgres")]
            Self::Postgres(pool) => get_postgres(pool, key).await,
        }
    }

    async fn mutate_without_idempotency(&self, key: &str) -> Result<()> {
        match self {
            Self::Memory { ledger, .. } => {
                let mut ledger = ledger.lock().await;
                let count = ledger.entry(key.to_owned()).or_default();
                *count = count.saturating_add(1);
                Ok(())
            }
            #[cfg(feature = "postgres")]
            Self::Postgres(pool) => {
                sqlx::query(
                    "INSERT INTO conformance_ledger (key, mutations) VALUES ($1, 1) \
                     ON CONFLICT (key) DO UPDATE SET mutations = conformance_ledger.mutations + 1",
                )
                .bind(key)
                .execute(pool)
                .await?;
                Ok(())
            }
        }
    }

    async fn ledger_count(&self, key: &str) -> Result<Option<u64>> {
        match self {
            Self::Memory { ledger, .. } => Ok(ledger.lock().await.get(key).copied()),
            #[cfg(feature = "postgres")]
            Self::Postgres(pool) => {
                let row = sqlx::query("SELECT mutations FROM conformance_ledger WHERE key = $1")
                    .bind(key)
                    .fetch_optional(pool)
                    .await?;
                row.map(|row| u64::try_from(row.get::<i64, _>("mutations")))
                    .transpose()
                    .map_err(Into::into)
            }
        }
    }
}

#[derive(Clone)]
struct Record {
    payload: Value,
    status: StoredStatus,
}

#[derive(Clone)]
enum StoredStatus {
    Accepted { destination_tx_id: String },
    Processing,
    Rejected { reason: String },
}

/// Builds the reference product router.
pub fn router(state: ReferenceState) -> Router {
    Router::new()
        .route("/settlements", post(post_settlement))
        .route("/settlements/{key}", get(get_settlement))
        .route("/__conformance/ledger/{key}", get(ledger_probe))
        .with_state(state)
}

async fn post_settlement(State(state): State<ReferenceState>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if state.config.broken != BrokenVariant::Signature
        && verify_signature(
            &state.config.verifying_key,
            &state.config.keyid,
            "POST",
            parts
                .uri
                .path_and_query()
                .map_or("/", |value| value.as_str()),
            &parts.headers,
            &body,
        )
        .is_err()
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let payload: Value = match serde_json::from_slice(&body) {
        Ok(payload) => payload,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    let key = match unquote_header(&parts.headers, "idempotency-key") {
        Ok(key) => key,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    if payload.get("idempotency_key").and_then(Value::as_str) != Some(key.as_str()) {
        return StatusCode::UNPROCESSABLE_ENTITY.into_response();
    }

    if state.config.broken != BrokenVariant::Idempotency
        && state.config.broken != BrokenVariant::Concurrency
    {
        if !state.inflight.lock().await.insert(key.clone()) {
            return StatusCode::CONFLICT.into_response();
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let status = decide(&state, &key, &payload).await;
        let response = match state.storage.resolve(&key, payload, status).await {
            Ok(Resolution::Record(record)) => record_response(&record),
            Ok(Resolution::Mismatch) => StatusCode::UNPROCESSABLE_ENTITY.into_response(),
            Err(error) => {
                tracing::error!(%error, "reference settlement storage failed");
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        };
        state.inflight.lock().await.remove(&key);
        return response;
    }

    let status = decide(&state, &key, &payload).await;
    if matches!(status, StoredStatus::Accepted { .. })
        && let Err(error) = state.storage.mutate_without_idempotency(&key).await
    {
        tracing::error!(%error, "reference ledger mutation failed");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    record_response(&Record { payload, status })
}

async fn decide(state: &ReferenceState, key: &str, payload: &Value) -> StoredStatus {
    let account_id = payload
        .get("account_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if account_id == state.config.refused_account_id {
        return StoredStatus::Rejected {
            reason: "business_refusal".to_owned(),
        };
    }
    if account_id == state.config.processing_account_id {
        return StoredStatus::Processing;
    }
    if state.config.broken != BrokenVariant::Caps
        && payload
            .get("amount_minor")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<u64>().ok())
            .is_none_or(|amount| amount > state.config.per_deposit_cap)
    {
        return StoredStatus::Rejected {
            reason: "per_deposit_cap".to_owned(),
        };
    }
    if state.config.broken != BrokenVariant::Evidence
        && validate_evidence(payload, &state.config.evidence_policy)
            .await
            .is_err()
    {
        return StoredStatus::Rejected {
            reason: "invalid_chain_evidence".to_owned(),
        };
    }
    if state.config.broken != BrokenVariant::DepositIdentity
        && validate_deposit_identity(key, payload).is_err()
    {
        return StoredStatus::Rejected {
            reason: "deposit_identity_mismatch".to_owned(),
        };
    }

    let destination_tx_id = format!("credit-{}", uuid::Uuid::new_v4());
    StoredStatus::Accepted { destination_tx_id }
}

async fn get_settlement(
    State(state): State<ReferenceState>,
    Path(key): Path<String>,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();
    let body = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if state.config.broken != BrokenVariant::Signature
        && verify_signature(
            &state.config.verifying_key,
            &state.config.keyid,
            "GET",
            parts
                .uri
                .path_and_query()
                .map_or("/", |value| value.as_str()),
            &parts.headers,
            &body,
        )
        .is_err()
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

async fn ledger_probe(State(state): State<ReferenceState>, Path(key): Path<String>) -> Response {
    match state.storage.ledger_count(&key).await {
        Ok(Some(count)) => axum::Json(json!({"mutations": count})).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => {
            tracing::error!(%error, "reference ledger lookup failed");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(feature = "postgres")]
async fn resolve_postgres(
    pool: &PgPool,
    key: &str,
    payload: Value,
    proposed: StoredStatus,
) -> Result<Resolution> {
    let (status, destination_tx_id, reason) = stored_status_columns(&proposed);
    let mut transaction = pool.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO conformance_settlements \
         (key, payload, status, destination_tx_id, reason) VALUES ($1, $2, $3, $4, $5) \
         ON CONFLICT (key) DO NOTHING",
    )
    .bind(key)
    .bind(&payload)
    .bind(status)
    .bind(destination_tx_id)
    .bind(reason)
    .execute(&mut *transaction)
    .await?
    .rows_affected()
        == 1;
    if inserted && matches!(proposed, StoredStatus::Accepted { .. }) {
        sqlx::query("INSERT INTO conformance_ledger (key, mutations) VALUES ($1, 1)")
            .bind(key)
            .execute(&mut *transaction)
            .await?;
    }
    let row = sqlx::query(
        "SELECT payload, status, destination_tx_id, reason \
         FROM conformance_settlements WHERE key = $1 FOR UPDATE",
    )
    .bind(key)
    .fetch_one(&mut *transaction)
    .await?;
    let record = record_from_row(&row)?;
    transaction.commit().await?;
    Ok(if record.payload == payload {
        Resolution::Record(record)
    } else {
        Resolution::Mismatch
    })
}

#[cfg(feature = "postgres")]
async fn get_postgres(pool: &PgPool, key: &str) -> Result<Option<Record>> {
    sqlx::query(
        "SELECT payload, status, destination_tx_id, reason \
         FROM conformance_settlements WHERE key = $1",
    )
    .bind(key)
    .fetch_optional(pool)
    .await?
    .map(|row| record_from_row(&row))
    .transpose()
}

#[cfg(feature = "postgres")]
fn stored_status_columns(status: &StoredStatus) -> (&'static str, Option<&str>, Option<&str>) {
    match status {
        StoredStatus::Accepted { destination_tx_id } => {
            ("accepted", Some(destination_tx_id.as_str()), None)
        }
        StoredStatus::Processing => ("processing", None, None),
        StoredStatus::Rejected { reason } => ("rejected", None, Some(reason.as_str())),
    }
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
    Ok(Record {
        payload: row.get("payload"),
        status,
    })
}

fn record_response(record: &Record) -> Response {
    let body = match &record.status {
        StoredStatus::Accepted { destination_tx_id } => json!({
            "status": "accepted",
            "destination_tx_id": destination_tx_id,
            "payload": record.payload,
        }),
        StoredStatus::Processing => json!({
            "status": "processing",
            "payload": record.payload,
        }),
        StoredStatus::Rejected { reason } => json!({
            "status": "rejected",
            "reason": reason,
            "payload": record.payload,
        }),
    };
    (StatusCode::OK, axum::Json(body)).into_response()
}

fn validate_deposit_identity(key: &str, payload: &Value) -> Result<()> {
    let evidence: Evidence = serde_json::from_value(
        payload
            .get("evidence")
            .cloned()
            .context("missing evidence")?,
    )?;
    let tx_hash = B256::from_str(&evidence.tx_hash)?;
    ensure!(
        key == format!(
            "deposit:{}",
            deposit_id(evidence.chain_id, tx_hash, evidence.log_index)
        ),
        "deposit id mismatch"
    );
    Ok(())
}

async fn validate_evidence(payload: &Value, policy: &EvidencePolicy) -> Result<()> {
    let account_id = payload
        .get("account_id")
        .and_then(Value::as_str)
        .context("missing account_id")?;
    let evidence: Evidence = serde_json::from_value(
        payload
            .get("evidence")
            .cloned()
            .context("missing evidence")?,
    )?;
    ensure!(
        evidence.route == "conformance" && evidence.route_version == 1,
        "wrong route"
    );
    match policy {
        EvidencePolicy::Synthetic => validate_synthetic_evidence(account_id, &evidence),
        EvidencePolicy::Rpc(config) => validate_rpc_evidence(account_id, &evidence, config).await,
    }
}

fn validate_synthetic_evidence(account_id: &str, evidence: &Evidence) -> Result<()> {
    ensure!(
        evidence.asset_contract == SYNTHETIC_ASSET,
        "wrong emitting contract"
    );
    ensure!(evidence.log_index == 0, "log does not exist");
    let factory = Address::from_str(SYNTHETIC_FACTORY)?;
    let implementation = Address::from_str(SYNTHETIC_IMPLEMENTATION)?;
    let expected_to = forwarder_address(
        factory,
        implementation,
        persistent_salt("conformance", account_id, 1),
    );
    ensure!(
        Address::from_str(&evidence.to)? == expected_to,
        "wrong recipient"
    );
    let tx_hash = B256::from_str(&evidence.tx_hash)?;
    let bytes = tx_hash.as_slice();
    ensure!(bytes.first() != Some(&0xfe), "block is not finalized");
    let amount_bytes: [u8; 8] = bytes
        .get(24..)
        .context("tx hash is too short")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("tx hash amount marker is invalid"))?;
    let expected_amount = u64::from_be_bytes(amount_bytes);
    ensure!(
        evidence.amount_atomic.parse::<u64>()? == expected_amount,
        "wrong atomic amount"
    );
    Ok(())
}

async fn validate_rpc_evidence(
    account_id: &str,
    evidence: &Evidence,
    config: &RpcEvidenceConfig,
) -> Result<()> {
    ensure!(evidence.chain_id == config.chain_id, "wrong chain id");
    let asset_contract = Address::from_str(&evidence.asset_contract)?;
    ensure!(
        asset_contract == config.asset_contract,
        "asset is not approved"
    );
    let expected_to = forwarder_address(
        config.factory,
        config.implementation,
        persistent_salt("conformance", account_id, 1),
    );
    ensure!(
        Address::from_str(&evidence.to)? == expected_to,
        "wrong recipient"
    );

    let receipt = rpc_call(
        &config.rpc_url,
        "eth_getTransactionReceipt",
        json!([evidence.tx_hash]),
    )
    .await?
    .context("transaction receipt does not exist")?;
    let logs = receipt
        .get("logs")
        .and_then(Value::as_array)
        .context("receipt omitted logs")?;
    let log = logs
        .iter()
        .find(|log| {
            log.get("logIndex")
                .and_then(Value::as_str)
                .and_then(|value| parse_hex_u64(value).ok())
                == Some(evidence.log_index)
        })
        .context("cited log does not exist")?;
    ensure!(
        log.get("address")
            .and_then(Value::as_str)
            .map(Address::from_str)
            .transpose()?
            == Some(config.asset_contract),
        "wrong emitting contract"
    );
    let topics = log
        .get("topics")
        .and_then(Value::as_array)
        .context("log omitted topics")?;
    let transfer_topic = format!("{:#x}", keccak256("Transfer(address,address,uint256)"));
    ensure!(
        topics.first().and_then(Value::as_str) == Some(transfer_topic.as_str()),
        "log is not an ERC-20 Transfer"
    );
    let recipient_topic = topics
        .get(2)
        .and_then(Value::as_str)
        .context("Transfer log omitted recipient")?;
    let recipient_word = B256::from_str(recipient_topic)?;
    let recipient = Address::from_slice(
        recipient_word
            .as_slice()
            .get(12..)
            .context("recipient topic is malformed")?,
    );
    ensure!(recipient == expected_to, "Transfer recipient is wrong");
    let amount = log
        .get("data")
        .and_then(Value::as_str)
        .context("Transfer log omitted amount")?;
    ensure!(
        U256::from_str(amount)? == U256::from_str(&evidence.amount_atomic)?,
        "Transfer amount is wrong"
    );
    let receipt_block = receipt
        .get("blockNumber")
        .and_then(Value::as_str)
        .map(parse_hex_u64)
        .transpose()?
        .context("receipt is not mined")?;
    let finalized = rpc_call(
        &config.rpc_url,
        "eth_getBlockByNumber",
        json!(["finalized", false]),
    )
    .await?
    .context("finalized block is unavailable")?;
    let finalized_number = finalized
        .get("number")
        .and_then(Value::as_str)
        .map(parse_hex_u64)
        .transpose()?
        .context("finalized block omitted number")?;
    ensure!(receipt_block <= finalized_number, "block is not finalized");
    Ok(())
}

async fn rpc_call(rpc_url: &str, method: &str, params: Value) -> Result<Option<Value>> {
    let response = reqwest::Client::new()
        .post(rpc_url)
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        }))
        .send()
        .await?
        .error_for_status()?;
    let body: Value = response.json().await?;
    ensure!(body.get("error").is_none(), "RPC returned an error: {body}");
    Ok(body.get("result").filter(|value| !value.is_null()).cloned())
}

fn parse_hex_u64(value: &str) -> Result<u64> {
    u64::from_str_radix(value.strip_prefix("0x").context("hex value needs 0x")?, 16)
        .map_err(Into::into)
}

fn verify_signature(
    key: &VerifyingKey,
    keyid: &str,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<()> {
    let digest = header(headers, "content-digest")?;
    ensure!(
        digest == format!("sha-256=:{}:", STANDARD.encode(Sha256::digest(body))),
        "content digest mismatch"
    );
    let idempotency_key = header(headers, "idempotency-key")?;
    let signature_input = header(headers, "signature-input")?;
    let parameters = signature_input
        .strip_prefix("sig1=")
        .context("signature label must be sig1")?;
    let prefix = "(\"@method\" \"@target-uri\" \"content-digest\" \"idempotency-key\");created=";
    let remainder = parameters
        .strip_prefix(prefix)
        .context("required signature components are missing")?;
    let (created, supplied_keyid) = remainder
        .split_once(";keyid=\"")
        .context("created or keyid is missing")?;
    let supplied_keyid = supplied_keyid
        .strip_suffix('"')
        .context("keyid is malformed")?;
    ensure!(supplied_keyid == keyid, "keyid mismatch");
    let created = created.parse::<i64>()?;
    ensure!(
        Utc::now().timestamp().abs_diff(created) <= 300,
        "signature expired"
    );
    let host = header(headers, HOST.as_str())?;
    let target_uri = format!("http://{host}{path}");
    let base = format!(
        "\"@method\": {method}\n\"@target-uri\": {target_uri}\n\"content-digest\": {digest}\n\"idempotency-key\": {idempotency_key}\n\"@signature-params\": {parameters}"
    );
    let encoded = header(headers, "signature")?
        .strip_prefix("sig1=:")
        .and_then(|value| value.strip_suffix(':'))
        .context("signature is malformed")?;
    let signature = Signature::from_slice(&STANDARD.decode(encoded)?)?;
    key.verify_strict(base.as_bytes(), &signature)?;
    Ok(())
}

fn unquote_header(headers: &HeaderMap, name: &'static str) -> Result<String> {
    let value = header(headers, name)?;
    let value = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .context("structured field string is required")?;
    ensure!(
        !value.contains(['"', '\\']),
        "escaped idempotency keys are unsupported"
    );
    Ok(value.to_owned())
}

fn header<'a>(headers: &'a HeaderMap, name: &'static str) -> Result<&'a str> {
    headers
        .get(name)
        .context("required header is missing")?
        .to_str()
        .context("header is not ASCII")
}

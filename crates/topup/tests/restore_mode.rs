//! Restore mode (docs/architecture.md §14, docs/design/multi-tenant.md §13) on PostgreSQL: a
//! restore freezes merchant requests and the crediting tasks; the operator re-applies lost
//! security changes, re-issues lost deposit addresses and quotes identically, imports the signed
//! deliveries of events so none is sent again with another body and each delivered credit stands,
//! and unfreezes once the chains are rescanned, audited.

mod support;

use std::str::FromStr as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, TimeDelta, Utc};
use ed25519_dalek::{Signer as _, SigningKey};
use hmac::{Hmac, KeyInit, Mac as _};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use sqlx::PgPool;
use topup::api::{
    AppState, AttestationError, AttestationFuture, AttestationRequest, Attestor, PublicOrigin,
    VerificationKey, WebhookKeysFuture,
};
use topup::db::{self, NewDeposit};
use topup::pump::{Pump, PumpConfig, RunOnceResult, StepSet};
use topup::restore_mode;
use topup::steps::confirm::ConfirmStep;
use topup_adapters::attestation::AttestedWebhookKey;
use topup_adapters::chain::evm::{
    ChainError, ChainReader, FactoryLog, FinalizedHead, ReceiptLookup, TransferLog,
};
use topup_adapters::pricing::{Observation, PriceError, PriceSource};
use topup_core::Ed25519PublicKey;
use topup_core::address::{deposit_address_salt, forwarder_address, quote_salt};
use topup_core::deposit::DepositState;
use topup_core::identity::{credited_event_id, deposit_id};
use topup_core::money::{AtomicAmount, PRICE_SCALE, ScaledPrice};
use topup_core::route::{ChainHeads, Confirmations, RouteFile};
use topup_core::valuation::{SourceId, UnixSeconds};
use tower::ServiceExt;
use uuid::Uuid;

use support::seed::{self, NewAccount};
use support::{TEST_ORIGIN, TestDatabase, merchant_request, public_key_base64, signed_request};

const ADMIN_KID: &str = "admin/v1";
/// The key the harness issues and checks client secrets with.
const CLIENT_SECRET_KEY: [u8; 32] = [92; 32];

struct Harness {
    app: Router,
    read_only: Router,
    pool: PgPool,
    owner: PgPool,
    route: RouteFile,
    admin_key: SigningKey,
    /// Admin signatures are single-use, so each admin request is signed at a distinct second.
    created: AtomicI64,
    account: db::Account,
    key: String,
    /// Issues client secrets with the key the API checks them with.
    client_reads: Arc<topup::api::ClientReadLimiter>,
}

struct Answer {
    status: StatusCode,
    retry_after: Option<String>,
    body: Value,
}

/// A client secret of `id` as issued before owner tags: a random nonce under the harness's key,
/// so a read accepts its tag but it proves no account.
fn legacy_secret(id: &str) -> String {
    let signed = format!("{id}_secret_{}", hex::encode(Uuid::new_v4().as_bytes()));
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&CLIENT_SECRET_KEY)
        .expect("HMAC takes a key of any length");
    mac.update(signed.as_bytes());
    format!(
        "{signed}{}",
        hex::encode(&mac.finalize().into_bytes()[..16])
    )
}

impl Harness {
    async fn new(database: &TestDatabase) -> Result<Self> {
        let pool = database.app_pool.clone();
        let admin_key = SigningKey::from_bytes(&[91; 32]);
        let client_reads = Arc::new(topup::api::ClientReadLimiter::new(
            topup::client_secret::ClientSecretKey::new(topup_core::SecretKey32::new(
                CLIENT_SECRET_KEY,
            )),
            8,
        ));
        let route: RouteFile =
            serde_saphyr::from_str(include_str!("fixtures/phala-cloud-pha.yaml"))?;
        let state = AppState {
            pool: pool.clone(),
            routes: Arc::new(
                topup::routes::RouteSet::new(vec![route.clone()]).map_err(anyhow::Error::msg)?,
            ),
            admin_key: VerificationKey::from_base64(
                ADMIN_KID.to_owned(),
                &public_key_base64(&admin_key),
            )
            .map_err(anyhow::Error::msg)?,
            public_origin: PublicOrigin::parse(TEST_ORIGIN)?,
            attestor: Arc::new(KeyAttestor),
            rate_lock_quotes: Arc::new(topup::locks::UnavailableQuoteProvider),
            client_reads: Arc::clone(&client_reads),
            rate_limits: Arc::default(),
            screening: Arc::new(topup::refunds::UnavailableDestinationScreener),
            contract_signatures: Arc::new(topup::treasuries::UnavailableContractSignatures),
        };
        let account = seed::create_account(
            &pool,
            &NewAccount {
                webhook_url: "https://merchant.example/webhooks".to_owned(),
                ..NewAccount::named("merchant")
            },
        )
        .await?;
        let key = seed::create_api_key(&pool, account.id, true).await?;
        seed::set_treasury(&pool, account.id, true, 1, seed::FIXTURE_TREASURY).await?;
        Ok(Self {
            app: topup::api::router(state.clone()).0,
            read_only: topup::api::read_only_router(state, None),
            pool,
            owner: database.owner_pool.clone(),
            route,
            admin_key,
            created: AtomicI64::new(Utc::now().timestamp()),
            account,
            key,
            client_reads,
        })
    }

    async fn admin(&self, method: Method, path: &str, body: &Value) -> Result<Answer> {
        self.admin_on(&self.app, method, path, body).await
    }

    async fn admin_on(
        &self,
        app: &Router,
        method: Method,
        path: &str,
        body: &Value,
    ) -> Result<Answer> {
        let created = self.created.fetch_sub(1, Ordering::Relaxed);
        let body = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(body)?
        };
        let request = signed_request(method, path, body, ADMIN_KID, &self.admin_key, created);
        answer(app.clone().oneshot(request).await?).await
    }

    async fn merchant(&self, method: Method, path: &str, body: &Value) -> Result<Answer> {
        self.merchant_with(&self.app, method, path, body, &self.key)
            .await
    }

    async fn merchant_with(
        &self,
        app: &Router,
        method: Method,
        path: &str,
        body: &Value,
        key: &str,
    ) -> Result<Answer> {
        let body = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(body)?
        };
        answer(
            app.clone()
                .oneshot(merchant_request(method, path, body, key))
                .await?,
        )
        .await
    }

    /// Records a restore as `restore-check` does after a restore on boot.
    async fn restore(&self) -> Result<restore_mode::Restore> {
        Ok(restore_mode::freeze_after_restore(&self.owner).await?)
    }

    /// Sets the chain's cursor as the finalized scanner commits it.
    async fn scan_to(&self, block: i64, block_time: chrono::DateTime<Utc>) -> Result<()> {
        sqlx::query(
            "INSERT INTO cursors (chain_id, scanned_block, scanned_block_time) VALUES (1, $1, $2) \
             ON CONFLICT (chain_id) DO UPDATE \
             SET scanned_block = EXCLUDED.scanned_block, \
                 scanned_block_time = EXCLUDED.scanned_block_time",
        )
        .bind(block)
        .bind(block_time)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    fn account_id(&self) -> &str {
        &self.account.public_id
    }

    /// The delivery of `event` signed with the account's live webhook key.
    fn delivered(&self, event: &Value) -> Value {
        delivery(event, &webhook_key(self.account_id(), true, 1))
    }

    /// Lifts the freeze once the chain is rescanned past it with every address backfilled.
    async fn unfreeze(&self, restore: &restore_mode::Restore) -> Result<()> {
        self.scan_to(1_000, restore.detected_at + TimeDelta::seconds(1))
            .await?;
        sqlx::query("UPDATE addresses SET backfilled = true")
            .execute(&self.pool)
            .await?;
        let unfrozen = self
            .admin(
                Method::POST,
                "/v1/admin/restore/unfreeze",
                &checklist("reconciled"),
            )
            .await?;
        ensure!(unfrozen.status == StatusCode::OK, "{}", unfrozen.body);
        Ok(())
    }

    /// The customer's deposit address of `version`, as the merchant recomputes it.
    fn derived_address(&self, customer: &str, version: u64) -> Address {
        let contracts = &self.route.chain.contracts;
        forwarder_address(
            contracts.forwarder_factory,
            contracts.implementation,
            seed::FIXTURE_TREASURY,
            deposit_address_salt(&self.account.public_id, true, customer, version),
        )
    }
}

/// The service's webhook keys in these tests: one per account, mode, and version, from its
/// dstack path.
fn webhook_key(account: &str, livemode: bool, version: u32) -> SigningKey {
    let id = topup_core::WebhookKeyId::new(account, livemode, version).expect("an acct_ id");
    key_at(&id)
}

fn key_at(id: &topup_core::WebhookKeyId) -> SigningKey {
    SigningKey::from_bytes(&Sha256::digest(id.domain().as_bytes()).into())
}

/// Signs deliveries with [`webhook_key`]s, as the service signs them with dstack's.
struct KeySigner;

impl topup_core::Signer for KeySigner {
    async fn sign_webhook(
        &self,
        key: &topup_core::WebhookKeyId,
        payload: &[u8],
    ) -> Result<topup_core::Ed25519Signature, topup_core::SignerError> {
        Ok(topup_core::Ed25519Signature(
            key_at(key).sign(payload).to_bytes(),
        ))
    }

    async fn webhook_public_key(
        &self,
        key: &topup_core::WebhookKeyId,
    ) -> Result<Ed25519PublicKey, topup_core::SignerError> {
        Ok(Ed25519PublicKey(key_at(key).verifying_key().to_bytes()))
    }
}

/// A merchant's webhook receiver that records each delivery as it got it: the Standard Webhooks
/// headers and the raw body.
#[derive(Clone, Default)]
struct Receiver(Arc<std::sync::Mutex<Vec<Value>>>);

impl Receiver {
    /// Serves on a local port; returns its URL.
    async fn serve(&self) -> Result<String> {
        let received = self.clone();
        let app = Router::new().route(
            "/webhooks",
            axum::routing::post(
                move |headers: axum::http::HeaderMap, body: axum::body::Bytes| async move {
                    let header = |name: &str| {
                        headers
                            .get(name)
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_owned()
                    };
                    received.0.lock().expect("receiver lock").push(json!({
                        "webhook_id": header("webhook-id"),
                        "webhook_timestamp": header("webhook-timestamp"),
                        "webhook_signature": header("webhook-signature"),
                        "body": String::from_utf8_lossy(&body),
                    }));
                    StatusCode::NO_CONTENT
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(format!("http://{address}/webhooks"))
    }

    fn deliveries(&self) -> Vec<Value> {
        self.0.lock().expect("receiver lock").clone()
    }
}

/// Screens every sender clear.
struct ClearSanctions;

#[async_trait]
impl topup_adapters::risk::oracle::SanctionsSource for ClearSanctions {
    async fn sanctions(
        &self,
        _address: Address,
        block_number: u64,
    ) -> topup_core::screening::SanctionsResult {
        topup_core::screening::SanctionsResult {
            provider_a: topup_core::screening::SanctionsAnswer::Clear,
            provider_b: topup_core::screening::SanctionsAnswer::Clear,
            block_number,
        }
    }
}

/// Derives [`webhook_key`]s as dstack derives the service's; it attests nothing.
struct KeyAttestor;

impl Attestor for KeyAttestor {
    fn attest<'a>(&'a self, request: AttestationRequest<'a>) -> AttestationFuture<'a> {
        Box::pin(async move {
            let webhook_keys = self
                .webhook_keys(request.account, request.livemode, request.versions)
                .await?;
            let report_data = topup_adapters::attestation::report_data(
                request.nonce,
                request.account,
                request.livemode,
                &webhook_keys,
            )
            .ok_or(AttestationError::Unavailable)?;
            Ok(topup::api::AttestationEvidence {
                webhook_keys,
                report_data,
                quote: vec![0x7d],
            })
        })
    }

    fn webhook_keys<'a>(
        &'a self,
        account: &'a str,
        livemode: bool,
        versions: &'a [u32],
    ) -> WebhookKeysFuture<'a> {
        Box::pin(async move {
            Ok(versions
                .iter()
                .map(|&version| AttestedWebhookKey {
                    version,
                    public_key: Ed25519PublicKey(
                        webhook_key(account, livemode, version)
                            .verifying_key()
                            .to_bytes(),
                    ),
                })
                .collect())
        })
    }
}

/// The delivery of `event` as the merchant's receiver recorded it, signed with `key`.
fn delivery(event: &Value, key: &SigningKey) -> Value {
    let id = event["id"].as_str().unwrap_or_default().to_owned();
    let body = event.to_string();
    let timestamp = "1790000005";
    let signature = key.sign(format!("{id}.{timestamp}.{body}").as_bytes());
    json!({
        "webhook_id": id,
        "webhook_timestamp": timestamp,
        "webhook_signature": format!("v1a,{}", STANDARD.encode(signature.to_bytes())),
        "body": body,
    })
}

/// A delivered `deposit.credited` of `deposit`, for a transfer of `amount_atomic` to `address`.
fn credited_event(
    harness: &Harness,
    deposit: Uuid,
    tx_hash: B256,
    address: &str,
    amount_atomic: &str,
    valuation: (u64, &str, &str),
) -> Value {
    let (amount, exchange_rate, price_source) = valuation;
    json!({
        "id": topup::ids::format(topup::ids::EVENT, credited_event_id(deposit)),
        "object": "event",
        "account": harness.account_id(),
        "livemode": true,
        "type": "deposit.credited",
        "created": 1_790_000_000,
        "actor": "system",
        "request": null,
        "data": {"object": {
            "id": topup::ids::format(topup::ids::DEPOSIT, deposit),
            "object": "deposit",
            "livemode": true,
            "status": "credited",
            "chain_id": 1,
            "tx_hash": format!("{tx_hash:#x}"),
            "address": address,
            "asset_contract": format!("{:#x}", harness.route.asset.contract),
            "from_address": format!("{:#x}", Address::repeat_byte(0x74)),
            "amount_atomic": amount_atomic,
            "amount": amount,
            "currency": "usd",
            "exchange_rate": exchange_rate,
            "price_source": price_source,
            "valued_at": 1_790_000_000,
        }},
    })
}

/// A detected deposit of `amount_atomic` to `address_id`, as the rescan records it.
async fn record_deposit(
    harness: &Harness,
    tx_hash: B256,
    address_id: Uuid,
    amount_atomic: U256,
) -> Result<Uuid> {
    ensure!(
        db::insert_deposit(
            &harness.pool,
            &NewDeposit {
                chain_id: 1,
                tx_hash,
                receipt_log_index: 0,
                log_index: 0,
                block_number: 120,
                block_hash: B256::repeat_byte(0xb1),
                block_time: Utc::now(),
                address_id,
                route: Some(harness.route.route.clone()),
                route_version: Some(harness.route.version),
                asset_contract: harness.route.asset.contract,
                from_address: Address::repeat_byte(0x74),
                amount_atomic: AtomicAmount::new(amount_atomic),
                state: DepositState::Detected,
                reason: None,
                next_attempt_at: Utc::now(),
                tx_from: Address::repeat_byte(0x74),
                tx_nonce: 0,
                is_final: false,
            },
        )
        .await?
    );
    Ok(deposit_id(1, tx_hash, 0))
}

/// Runs the confirm step once on the recorded deposit, the chain showing its transfer to
/// `recipient` and spot at `spot` scaled dollars.
async fn confirm(harness: &Harness, deposit: Uuid, recipient: Address, spot: u64) -> Result<()> {
    let step = confirm_step(harness, deposit, recipient, spot).await?;
    run_pump(
        harness,
        deposit,
        StepSet::new(Box::new(step), Box::new(Unreached)),
    )
    .await
}

/// The confirm step, the chain showing the recorded deposit's transfer to `recipient` and spot at
/// `spot` scaled dollars.
async fn confirm_step(
    harness: &Harness,
    deposit: Uuid,
    recipient: Address,
    spot: u64,
) -> Result<ConfirmStep> {
    let recorded = db::get_deposit(&harness.pool, deposit)
        .await?
        .context("recorded deposit")?;
    let chain = FinalChain(TransferLog {
        tx_hash: recorded.tx_hash,
        receipt_log_index: recorded.receipt_log_index,
        log_index: recorded.log_index,
        block_number: recorded.block_number,
        block_hash: recorded.block_hash,
        block_time: recorded.block_time,
        tx_from: Address::repeat_byte(0x74),
        tx_nonce: 0,
        token: recorded.asset_contract,
        from: recorded.from_address,
        to: recipient,
        amount: recorded.amount_atomic,
    });
    let price = |source: &str, value| {
        Arc::new(FixedPrice(Observation {
            source: SourceId::new(source),
            price: ScaledPrice::new(value, PRICE_SCALE).expect("test price"),
            observed_at: UnixSeconds::new(
                u64::try_from(Utc::now().timestamp()).expect("current time"),
            ),
        })) as Arc<dyn PriceSource>
    };
    Ok(ConfirmStep::single(
        harness.pool.clone(),
        harness.route.clone(),
        chain.clone(),
        chain,
        price("primary", spot),
        Some(price("check", spot)),
        Some(price("fx", 100_000_000)),
    ))
}

/// Runs one step of the pump on `deposit`, whose events render over the harness's route.
async fn run_pump(harness: &Harness, deposit: Uuid, steps: StepSet) -> Result<()> {
    let pump = Pump::new(
        harness.pool.clone(),
        Arc::new(
            topup::routes::RouteSet::new(vec![harness.route.clone()])
                .map_err(anyhow::Error::msg)?,
        ),
        Arc::new(steps),
        PumpConfig::default(),
    )?;
    // Another due deposit may be claimed first; the held steps park it.
    for _ in 0..3 {
        if pump.run_once().await?
            == (RunOnceResult::Applied {
                deposit_id: deposit,
            })
        {
            return Ok(());
        }
    }
    anyhow::bail!("the pump did not run a step on {deposit}")
}

/// A chain final past every block, holding one transfer.
#[derive(Clone)]
struct FinalChain(TransferLog);

impl ChainReader for FinalChain {
    async fn factory_logs(
        &self,
        _factory: Address,
        _forwarders: &[Address],
        _from_block: u64,
        _to_block: u64,
    ) -> Result<Vec<FactoryLog>, ChainError> {
        Ok(Vec::new())
    }

    async fn finalized_head(&self) -> Result<FinalizedHead, ChainError> {
        Ok(FinalizedHead {
            number: u64::MAX,
            time: DateTime::UNIX_EPOCH,
        })
    }

    async fn transfer_logs_to(
        &self,
        _addresses: &[Address],
        _from_block: u64,
        _to_block: u64,
    ) -> Result<Vec<TransferLog>, ChainError> {
        Ok(vec![self.0.clone()])
    }

    async fn confirmation_heads(
        &self,
        _confirmations: Confirmations,
    ) -> Result<ChainHeads, ChainError> {
        Ok(ChainHeads {
            latest: Some(u64::MAX),
            safe: Some(u64::MAX),
            finalized: u64::MAX,
        })
    }

    async fn receipt_transfer(
        &self,
        _tx_hash: B256,
        _receipt_log_index: u64,
    ) -> Result<ReceiptLookup, ChainError> {
        Ok(ReceiptLookup::Included {
            block_number: self.0.block_number,
            block_hash: self.0.block_hash,
            transfer: Some(Box::new(self.0.clone())),
        })
    }

    async fn nonce_at(&self, _account: Address, _block: u64) -> Result<u64, ChainError> {
        Ok(1)
    }
}

struct FixedPrice(Observation);

#[async_trait]
impl PriceSource for FixedPrice {
    async fn observe(&self) -> Result<Observation, PriceError> {
        Ok(self.0.clone())
    }
}

/// The steps after confirmation hold a confirmed deposit, so only the confirm step runs.
struct Unreached;

#[async_trait]
impl topup::pump::Step for Unreached {
    async fn run(&self, _deposit: &db::Deposit) -> topup::pump::StepResult {
        topup::pump::StepResult::new(
            topup_core::deposit::StepOutcome::Wait {
                reason: topup_core::deposit::WaitReason::Paused,
            },
            json!({"stage": "held by the test"}),
        )
    }
}

/// The stored valuation of a deposit: state, price source, price, credit.
async fn valuation(harness: &Harness, deposit: Uuid) -> Result<(String, String, String, String)> {
    Ok(sqlx::query_as(
        "SELECT state, price_source, price_scaled::text, credit_minor::text \
         FROM deposits WHERE id = $1",
    )
    .bind(deposit)
    .fetch_one(&harness.pool)
    .await?)
}

/// A `GET` without credentials, as a payer's page reads a `client_secret` view.
async fn anonymous(app: &Router, path: &str) -> Result<Answer> {
    answer(
        app.clone()
            .oneshot(Request::get(path).body(Body::empty())?)
            .await?,
    )
    .await
}

async fn answer(response: axum::response::Response) -> Result<Answer> {
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .map(|value| value.to_str().map(str::to_owned))
        .transpose()?;
    let bytes = to_bytes(response.into_body(), 1_048_576).await?;
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    Ok(Answer {
        status,
        retry_after,
        body,
    })
}

fn checklist(reason: &str) -> Value {
    json!({
        "reason": reason,
        "security_changes_reapplied": true,
        "deposit_addresses_reissued": true,
        "quotes_reissued": true,
        "delivered_events_imported": true,
    })
}

#[tokio::test]
async fn a_new_timeline_freezes_the_service_once() -> Result<()> {
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            // The migration acknowledged the timeline it ran on: no restore.
            ensure!(restore_mode::detect(&harness.pool).await?.is_none());
            ensure!(!restore_mode::is_frozen(&harness.pool).await?);
            harness.scan_to(40, Utc::now()).await?;
            sqlx::query("INSERT INTO heartbeat DEFAULT VALUES")
                .execute(&harness.pool)
                .await?;

            // A promotion out of archive recovery starts a newer timeline than the acknowledged
            // one; emulate it by acknowledging an older one.
            sqlx::query(
                "ALTER TABLE restore_timeline DROP CONSTRAINT restore_timeline_timeline_id_check",
            )
            .execute(&harness.owner)
            .await?;
            sqlx::query("UPDATE restore_timeline SET timeline_id = 0")
                .execute(&harness.owner)
                .await?;
            let restore = restore_mode::detect(&harness.pool)
                .await?
                .context("a newer timeline freezes the service")?;
            ensure!(restore.detected_by == "timeline" && restore.timeline_id >= 1);
            ensure!(restore.restored_cursors.get(&1) == Some(&40));
            ensure!(restore.restore_point.is_some());
            ensure!(restore_mode::is_frozen(&harness.pool).await?);
            // The timeline is acknowledged: a restart finds the same freeze, not another.
            ensure!(restore_mode::detect(&harness.pool).await? == Some(restore.clone()));
            // restore-check's marker keeps the one freeze too.
            ensure!(harness.restore().await?.id == restore.id);
            let restores: i64 = sqlx::query_scalar("SELECT count(*) FROM restores")
                .fetch_one(&harness.pool)
                .await?;
            ensure!(restores == 1);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn frozen_merchant_requests_answer_service_restoring_and_admin_and_health_work() -> Result<()>
{
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            let body = json!({"name": "ci"});
            ensure!(
                harness
                    .merchant(Method::POST, "/v1/api_keys", &body)
                    .await?
                    .status
                    == StatusCode::OK
            );
            harness.restore().await?;

            for (method, path, body) in [
                (Method::POST, "/v1/api_keys", json!({"name": "ci"})),
                (
                    Method::POST,
                    "/v1/deposit_addresses",
                    json!({"client_reference_id": "team-1"}),
                ),
                (Method::POST, "/v1/account/pause", Value::Null),
                (
                    Method::POST,
                    "/v1/api_keys",
                    json!({"name": "worker", "type": "restricted", "permissions": ["quotes.read"]}),
                ),
                (Method::POST, "/v1/treasuries/trs_0/pause", Value::Null),
                (Method::POST, "/v1/treasuries/trs_0/resume", Value::Null),
                (
                    Method::POST,
                    "/v1/account/webhook_keys/roll",
                    json!({"expires_in": 0}),
                ),
                (Method::DELETE, "/v1/webhook_endpoints/we_0", Value::Null),
            ] {
                let refused = harness.merchant(method, path, &body).await?;
                ensure!(
                    refused.status == StatusCode::SERVICE_UNAVAILABLE,
                    "{path}: {}",
                    refused.status
                );
                ensure!(refused.body["error"]["code"] == "service_restoring");
                ensure!(refused.retry_after.as_deref() == Some("300"));
            }
            // Refused before authentication, so no idempotency key stores the refusal.
            let anonymous = harness
                .merchant_with(
                    &harness.app,
                    Method::POST,
                    "/v1/api_keys",
                    &body,
                    "ppay_sk_live_invalid",
                )
                .await?;
            ensure!(anonymous.body["error"]["code"] == "service_restoring");
            let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM idempotency_keys")
                .fetch_one(&harness.pool)
                .await?;
            ensure!(stored == 0);

            // Reads too: the restored database may hold a key revoked after the restore point as
            // valid, so no key authenticates until the operator has revoked such keys again and
            // unfrozen the service. A client secret's public read uses no key.
            for path in ["/v1/account", "/v1/quotes", "/v1/deposits", "/v1/api_keys"] {
                let refused = harness.merchant(Method::GET, path, &Value::Null).await?;
                ensure!(
                    refused.status == StatusCode::SERVICE_UNAVAILABLE,
                    "{path}: {}",
                    refused.status
                );
                ensure!(refused.body["error"]["code"] == "service_restoring");
                ensure!(refused.retry_after.as_deref() == Some("300"));
            }
            let quote_by_key = harness
                .merchant(Method::GET, "/v1/quotes/qt_0", &Value::Null)
                .await?;
            ensure!(quote_by_key.body["error"]["code"] == "service_restoring");
            let health = answer(
                harness
                    .app
                    .clone()
                    .oneshot(Request::get("/healthz").body(Body::empty())?)
                    .await?,
            )
            .await?;
            ensure!(health.status == StatusCode::OK);
            let status = harness
                .admin(Method::GET, "/v1/admin/restore", &Value::Null)
                .await?;
            ensure!(status.status == StatusCode::OK);
            ensure!(status.body["frozen"] == true);
            ensure!(status.body["restore"]["detected_by"] == "restore_check");
            ensure!(status.body["rescan"][0]["chain_id"] == 1);
            ensure!(status.body["rescan"][0]["complete"] == false);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn unfreeze_needs_the_rescan_and_the_checklist_and_is_audited() -> Result<()> {
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            let not_frozen = harness
                .admin(
                    Method::POST,
                    "/v1/admin/restore/unfreeze",
                    &checklist("drill"),
                )
                .await?;
            ensure!(not_frozen.status == StatusCode::BAD_REQUEST);
            ensure!(not_frozen.body["error"]["code"] == "restore_not_frozen");

            harness
                .scan_to(100, Utc::now() - TimeDelta::hours(1))
                .await?;
            let restore = harness.restore().await?;
            let held = tokio::spawn({
                let pool = harness.pool.clone();
                async move {
                    restore_mode::wait_until_unfrozen(
                        &pool,
                        std::time::Duration::from_millis(50),
                        &tokio_util::sync::CancellationToken::new(),
                    )
                    .await
                }
            });

            let mut unchecked = checklist("drill");
            unchecked["deposit_addresses_reissued"] = json!(false);
            let refused = harness
                .admin(Method::POST, "/v1/admin/restore/unfreeze", &unchecked)
                .await?;
            ensure!(refused.status == StatusCode::BAD_REQUEST);
            ensure!(refused.body["error"]["param"] == "deposit_addresses_reissued");

            // The chain has not finalized past the moment the restore was detected.
            let incomplete = harness
                .admin(
                    Method::POST,
                    "/v1/admin/restore/unfreeze",
                    &checklist("drill"),
                )
                .await?;
            ensure!(incomplete.status == StatusCode::BAD_REQUEST);
            ensure!(incomplete.body["error"]["code"] == "restore_rescan_incomplete");

            // Caught up, but an issued address's history is not read yet.
            harness
                .scan_to(200, restore.detected_at + TimeDelta::seconds(1))
                .await?;
            let customer = seed::create_customer(
                &harness.pool,
                &seed::NewCustomer {
                    id: Uuid::new_v4(),
                    account_id: harness.account.id,
                    livemode: true,
                    client_reference_id: "team-7".to_owned(),
                    paused_scopes: Vec::new(),
                },
            )
            .await?;
            let address = seed::insert_address(
                &harness.pool,
                &seed::NewAddress {
                    id: Uuid::new_v4(),
                    customer_id: customer.id,
                    chain_id: 1,
                    route: harness.route.route.clone(),
                    salt: B256::repeat_byte(0x11),
                    address: Address::repeat_byte(0x12),
                },
            )
            .await?;
            let status = harness
                .admin(Method::GET, "/v1/admin/restore", &Value::Null)
                .await?;
            ensure!(status.body["rescan"][0]["pending_backfills"] == 1);
            ensure!(status.body["rescan"][0]["complete"] == false);
            sqlx::query("UPDATE addresses SET backfilled = true WHERE id = $1")
                .bind(address.id)
                .execute(&harness.pool)
                .await?;
            ensure!(!held.is_finished());

            let unfrozen = harness
                .admin(
                    Method::POST,
                    "/v1/admin/restore/unfreeze",
                    &checklist("INC-42 reconciled"),
                )
                .await?;
            ensure!(unfrozen.status == StatusCode::OK, "{}", unfrozen.body);
            ensure!(unfrozen.body["id"] == restore.id.to_string());
            ensure!(unfrozen.body["unfrozen_by"] == format!("admin:{ADMIN_KID}"));
            let reason = unfrozen.body["unfreeze_reason"]
                .as_str()
                .context("reason")?;
            ensure!(reason.starts_with("INC-42 reconciled; checklist: "));
            let audit: (String, String, String) = sqlx::query_as(
                "SELECT actor_id, subject, reason FROM audit WHERE action = 'restore.unfreeze'",
            )
            .fetch_one(&harness.pool)
            .await?;
            ensure!(
                audit
                    == (
                        ADMIN_KID.to_owned(),
                        format!("restore:{}", restore.id),
                        reason.to_owned()
                    )
            );
            ensure!(tokio::time::timeout(std::time::Duration::from_secs(5), held).await??);

            // Merchant writes work again; the restore stays on record.
            let created = harness
                .merchant(Method::POST, "/v1/api_keys", &json!({"name": "after"}))
                .await?;
            ensure!(created.status == StatusCode::OK);
            let again = harness
                .admin(
                    Method::POST,
                    "/v1/admin/restore/unfreeze",
                    &checklist("again"),
                )
                .await?;
            ensure!(again.body["error"]["code"] == "restore_not_frozen");
            let status = harness
                .admin(Method::GET, "/v1/admin/restore", &Value::Null)
                .await?;
            ensure!(status.body["frozen"] == false);
            ensure!(status.body["restore"]["unfrozen_at"].is_i64());
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_key_revoked_after_the_restore_point_is_revoked_again_before_the_unfreeze() -> Result<()>
{
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            // The merchant's second key, revoked after the restore point: the restore made it
            // valid again.
            let leaked = seed::create_api_key(&harness.pool, harness.account.id, true).await?;
            let restore_path = "/v1/admin/restore/api_keys/revoke";
            let by_prefix = json!({
                "account": harness.account_id(),
                "prefix": "ppay_sk_live_",
                "last4": &leaked[leaked.len() - 4..],
                "reason": "merchant revoked it at 10:02 PDT",
            });
            let not_frozen = harness
                .admin(Method::POST, restore_path, &by_prefix)
                .await?;
            ensure!(not_frozen.body["error"]["code"] == "restore_not_frozen");
            let restore = harness.restore().await?;
            // The restore made the key valid again, but while frozen no key reads anything.
            let account = |key: String| {
                let harness = &harness;
                async move {
                    harness
                        .merchant_with(&harness.app, Method::GET, "/v1/account", &Value::Null, &key)
                        .await
                }
            };
            let held = account(leaked.clone()).await?;
            ensure!(held.status == StatusCode::SERVICE_UNAVAILABLE);
            ensure!(held.body["error"]["code"] == "service_restoring");

            let revoked = harness
                .admin(Method::POST, restore_path, &by_prefix)
                .await?;
            ensure!(revoked.status == StatusCode::OK, "{}", revoked.body);
            ensure!(revoked.body["status"] == "revoked");
            // Repeating it is harmless.
            let repeated = harness
                .admin(Method::POST, restore_path, &by_prefix)
                .await?;
            ensure!(repeated.status == StatusCode::OK && repeated.body["id"] == revoked.body["id"]);
            ensure!(account(harness.key.clone()).await?.status == StatusCode::SERVICE_UNAVAILABLE);

            // Neither selector, or an unknown one.
            let neither = harness
                .admin(
                    Method::POST,
                    restore_path,
                    &json!({"account": harness.account_id(), "reason": "x"}),
                )
                .await?;
            ensure!(neither.status == StatusCode::BAD_REQUEST);
            let unknown = harness
                .admin(
                    Method::POST,
                    restore_path,
                    &json!({
                        "account": harness.account_id(),
                        "prefix": "ppay_sk_live_",
                        "last4": "zzzz",
                        "reason": "x",
                    }),
                )
                .await?;
            ensure!(unknown.status == StatusCode::NOT_FOUND);
            // Once unfrozen, the key revoked again is refused and the other key works.
            harness.unfreeze(&restore).await?;
            ensure!(account(leaked.clone()).await?.status == StatusCode::UNAUTHORIZED);
            ensure!(account(harness.key.clone()).await?.status == StatusCode::OK);
            let audited: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM audit WHERE action = 'restore.api_key_revoke'",
            )
            .fetch_one(&harness.pool)
            .await?;
            ensure!(audited == 2);
            let announced: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM events WHERE type = 'api_key.revoked' AND account_id = $1",
            )
            .bind(harness.account.id)
            .fetch_one(&harness.pool)
            .await?;
            ensure!(announced == 1);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_treasury_cancellation_lost_in_the_restore_is_applied_again() -> Result<()> {
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            let pending = seed::schedule_treasury(
                &harness.pool,
                harness.account.id,
                true,
                1,
                Address::repeat_byte(0x42),
                Utc::now() + TimeDelta::hours(48),
            )
            .await?;
            harness.restore().await?;
            let pending_id = topup::ids::format(topup::ids::TREASURY, pending);
            let missing_id = topup::ids::format(topup::ids::TREASURY, Uuid::new_v4());
            let received = |reapply: bool| {
                json!({
                    "account": harness.account_id(),
                    "livemode": true,
                    "treasuries": [
                        {"id": pending_id, "object": "treasury", "status": "canceled", "chain_id": 1,
                         "address": format!("{:#x}", Address::repeat_byte(0x42))},
                        {"id": missing_id, "status": "pending", "chain_id": 1,
                         "address": format!("{:#x}", Address::repeat_byte(0x43))},
                    ],
                    "reapply": reapply,
                    "reason": "treasury events the merchant received",
                })
            };
            let path = "/v1/admin/restore/treasuries/verify";
            let verified = harness.admin(Method::POST, path, &received(false)).await?;
            ensure!(verified.status == StatusCode::OK, "{}", verified.body);
            ensure!(verified.body["data"][0]["result"] == "cancellation_lost");
            ensure!(verified.body["data"][0]["status"] == "pending");
            ensure!(verified.body["data"][1]["result"] == "missing");
            ensure!(verified.body["data"][1]["status"].is_null());

            let reapplied = harness.admin(Method::POST, path, &received(true)).await?;
            ensure!(reapplied.body["data"][0]["result"] == "canceled");
            ensure!(reapplied.body["data"][0]["status"] == "canceled");
            let again = harness.admin(Method::POST, path, &received(true)).await?;
            ensure!(again.body["data"][0]["result"] == "matches");
            let canceled: Option<chrono::DateTime<Utc>> =
                sqlx::query_scalar("SELECT canceled_at FROM treasuries WHERE id = $1")
                    .bind(pending)
                    .fetch_one(&harness.pool)
                    .await?;
            ensure!(canceled.is_some());
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_treasury_crediting_pause_lost_in_the_restore_is_applied_again() -> Result<()> {
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            let current: Uuid = sqlx::query_scalar(
                "SELECT id FROM treasuries WHERE account_id = $1 AND replaced_at IS NULL",
            )
            .bind(harness.account.id)
            .fetch_one(&harness.pool)
            .await?;
            harness.restore().await?;
            let paused_by = || async {
                let owners: Vec<String> =
                    sqlx::query_scalar("SELECT crediting_paused_by FROM treasuries WHERE id = $1")
                        .bind(current)
                        .fetch_one(&harness.pool)
                        .await?;
                anyhow::Ok(owners)
            };
            let received = |owners: Value, reapply: bool| {
                json!({
                    "account": harness.account_id(),
                    "livemode": true,
                    "treasuries": [{
                        "id": topup::ids::format(topup::ids::TREASURY, current),
                        "status": "active",
                        "chain_id": 1,
                        "address": format!("{:#x}", seed::FIXTURE_TREASURY),
                        "crediting_paused": true,
                        "crediting_paused_by": owners,
                    }],
                    "reapply": reapply,
                    "reason": "treasury.updated the merchant received",
                })
            };
            let path = "/v1/admin/restore/treasuries/verify";
            // The merchant paused crediting to the treasury after the restore point.
            let lost = harness
                .admin(Method::POST, path, &received(json!(["merchant"]), false))
                .await?;
            ensure!(lost.status == StatusCode::OK, "{}", lost.body);
            ensure!(lost.body["data"][0]["result"] == "matches");
            ensure!(lost.body["data"][0]["crediting"] == "pause_lost");
            ensure!(paused_by().await?.is_empty());
            let paused = harness
                .admin(Method::POST, path, &received(json!(["merchant"]), true))
                .await?;
            ensure!(paused.body["data"][0]["crediting"] == "paused");
            ensure!(paused_by().await? == vec!["merchant".to_owned()]);
            let announced: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM events WHERE type = 'treasury.updated' AND object_id = $1",
            )
            .bind(current)
            .fetch_one(&harness.pool)
            .await?;
            ensure!(announced == 1);

            // A resume lost in the window is applied again; the operator's pause is not the
            // merchant's to lift.
            sqlx::query(
                "UPDATE treasuries SET crediting_paused_by = ARRAY['merchant', 'operator'] \
                 WHERE id = $1",
            )
            .bind(current)
            .execute(&harness.pool)
            .await?;
            let resumed = harness
                .admin(Method::POST, path, &received(json!(["operator"]), true))
                .await?;
            ensure!(resumed.body["data"][0]["crediting"] == "resumed");
            ensure!(paused_by().await? == vec!["operator".to_owned()]);
            let matches = harness
                .admin(Method::POST, path, &received(json!(["operator"]), true))
                .await?;
            ensure!(matches.body["data"][0]["crediting"] == "matches");
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_deposit_address_given_out_after_the_restore_point_is_reissued_identically() -> Result<()>
{
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            let first = harness
                .merchant(
                    Method::POST,
                    "/v1/deposit_addresses",
                    &json!({"client_reference_id": "team-42", "metadata": {"plan": "pro"}}),
                )
                .await?;
            ensure!(first.status == StatusCode::OK && first.body["version"] == 1);
            harness.scan_to(100, Utc::now()).await?;
            let restore = harness.restore().await?;
            // The scanner moved on while frozen; the re-issued address is still read from block 100.
            harness.scan_to(250, Utc::now()).await?;

            // After the restore point the merchant rotated the address twice and holds version 3.
            let held = harness.derived_address("team-42", 3);
            let held_id = topup::ids::format(topup::ids::DEPOSIT_ADDRESS, Uuid::new_v4());
            let held_secret = harness
                .client_reads
                .key()
                .issue(harness.account_id(), &held_id)?;
            let request = json!({
                "account": harness.account_id(),
                "livemode": true,
                "client_reference_id": "team-42",
                "address": format!("{held:#x}"),
                "id": held_id,
                "client_secret": held_secret,
                "reason": "the merchant's export",
            });
            let path = "/v1/admin/restore/deposit_addresses";
            // A version that disagrees with the address is refused before anything is issued.
            let mut disagreeing = request.clone();
            disagreeing["version"] = json!(2);
            let refused = harness.admin(Method::POST, path, &disagreeing).await?;
            ensure!(
                refused.status == StatusCode::BAD_REQUEST,
                "{}",
                refused.body
            );
            let versions: i64 = sqlx::query_scalar("SELECT max(version) FROM deposit_addresses")
                .fetch_one(&harness.pool)
                .await?;
            ensure!(versions == 1);
            // A client secret the service did not issue for the id, issued for it to another
            // account (another merchant's record of the id), or issued before owner tags (it
            // reads, but proves no account), is refused.
            let key = harness.client_reads.key();
            for secret in [
                key.issue(
                    harness.account_id(),
                    &topup::ids::format(topup::ids::DEPOSIT_ADDRESS, Uuid::new_v4()),
                )?,
                key.issue("acct_other", &held_id)?,
                legacy_secret(&held_id),
            ] {
                let mut other_secret = request.clone();
                other_secret["client_secret"] = json!(secret);
                let refused = harness.admin(Method::POST, path, &other_secret).await?;
                ensure!(
                    refused.body["error"]["param"] == "client_secret",
                    "{}",
                    refused.body
                );
            }
            let reissued = harness.admin(Method::POST, path, &request).await?;
            ensure!(reissued.status == StatusCode::OK, "{}", reissued.body);
            ensure!(reissued.body["reissued"] == true);
            // The payer's page reads the address again with the secret it holds.
            let public = anonymous(
                &harness.app,
                &format!("/v1/deposit_addresses/{held_id}?client_secret={held_secret}"),
            )
            .await?;
            ensure!(public.status == StatusCode::OK, "{}", public.body);
            let object = &reissued.body["deposit_address"];
            ensure!(object["id"] == held_id && object["version"] == 3);
            ensure!(object["status"] == "active" && object["address"] == format!("{held:#x}"));
            ensure!(object["metadata"]["plan"] == "pro");
            let versions: Vec<(i64, String)> = sqlx::query_as(
                "SELECT deposit_address.version, deposit_address.status \
                 FROM deposit_addresses AS deposit_address \
                 JOIN customers AS customer ON customer.id = deposit_address.customer_id \
                 WHERE customer.client_reference_id = 'team-42' ORDER BY 1",
            )
            .fetch_all(&harness.pool)
            .await?;
            ensure!(
                versions
                    == vec![
                        (1, "retired".to_owned()),
                        (2, "retired".to_owned()),
                        (3, "active".to_owned())
                    ],
                "{versions:?}"
            );
            // Version 2's and 3's forwarders are read from the restored cursor.
            let backfill: Vec<(i64, i64, bool)> = sqlx::query_as(
                "SELECT deposit_address.version, address.created_block, address.backfilled \
                 FROM addresses AS address \
                 JOIN deposit_addresses AS deposit_address \
                   ON deposit_address.id = address.deposit_address_id \
                 WHERE deposit_address.version > 1 ORDER BY 1",
            )
            .fetch_all(&harness.pool)
            .await?;
            ensure!(
                backfill == vec![(2, 100, false), (3, 100, false)],
                "{backfill:?}"
            );
            ensure!(restore.restored_cursors.get(&1) == Some(&100));

            // Repeating it returns the same address; a version by number works too.
            let repeated = harness.admin(Method::POST, path, &request).await?;
            ensure!(repeated.body["reissued"] == false);
            ensure!(repeated.body["deposit_address"]["id"] == held_id);
            // A repeat with a secret of the id issued to another account, or without an owner
            // tag, is refused and adds nothing: the secret held was kept once, audited.
            for secret in [key.issue("acct_other", &held_id)?, legacy_secret(&held_id)] {
                let mut other_secret = request.clone();
                other_secret["client_secret"] = json!(secret);
                let refused = harness.admin(Method::POST, path, &other_secret).await?;
                ensure!(
                    refused.status == StatusCode::BAD_REQUEST
                        && refused.body["error"]["param"] == "client_secret",
                    "{}",
                    refused.body
                );
                let read = anonymous(
                    &harness.app,
                    &format!("/v1/deposit_addresses/{held_id}?client_secret={secret}"),
                )
                .await?;
                ensure!(read.status == StatusCode::NOT_FOUND, "{}", read.body);
            }
            let (secrets, audited): (i64, i64) = sqlx::query_as(
                "SELECT (SELECT count(*) FROM deposit_address_client_secrets \
                         WHERE deposit_address_id = $1), \
                        (SELECT count(*) FROM audit \
                         WHERE action = 'deposit_address.client_secret_restore')",
            )
            .bind(topup::ids::parse(topup::ids::DEPOSIT_ADDRESS, &held_id).context("a da_ id")?)
            .fetch_one(&harness.pool)
            .await?;
            ensure!(
                (secrets, audited) == (1, 1),
                "{secrets} secrets, {audited} audited"
            );
            let new_customer = harness
                .admin(
                    Method::POST,
                    path,
                    &json!({
                        "account": harness.account_id(),
                        "livemode": true,
                        "client_reference_id": "team-new",
                        "version": 1,
                        "reason": "the merchant's export",
                    }),
                )
                .await?;
            ensure!(
                new_customer.body["deposit_address"]["address"]
                    == format!("{:#x}", harness.derived_address("team-new", 1))
            );
            // An address that is not the customer's is refused.
            let mut foreign = request.clone();
            foreign["address"] = json!(format!("{:#x}", Address::repeat_byte(0x99)));
            foreign["id"] = Value::Null;
            foreign["client_secret"] = Value::Null;
            let refused = harness.admin(Method::POST, path, &foreign).await?;
            ensure!(
                refused.status == StatusCode::BAD_REQUEST,
                "{}",
                refused.body
            );
            // Refused for a customer the account never had, it leaves no customer behind.
            foreign["client_reference_id"] = json!("team-ghost");
            let refused = harness.admin(Method::POST, path, &foreign).await?;
            ensure!(refused.status == StatusCode::BAD_REQUEST);
            let ghosts: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM customers WHERE client_reference_id = 'team-ghost'",
            )
            .fetch_one(&harness.pool)
            .await?;
            ensure!(ghosts == 0);
            let audited: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM audit WHERE action = 'deposit_address.reissue'",
            )
            .fetch_one(&harness.pool)
            .await?;
            ensure!(audited == 2);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_delivered_event_is_kept_as_delivered_and_never_sent_again() -> Result<()> {
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            let address = harness
                .merchant(
                    Method::POST,
                    "/v1/deposit_addresses",
                    &json!({"client_reference_id": "team-9"}),
                )
                .await?;
            let forwarder = address.body["address"]
                .as_str()
                .context("address")?
                .to_owned();
            let address_id: Uuid =
                sqlx::query_scalar("SELECT id FROM addresses WHERE address = $1 AND chain_id = 1")
                    .bind(&forwarder)
                    .fetch_one(&harness.pool)
                    .await?;
            harness.restore().await?;

            // The merchant received deposit.credited for a deposit credited after the restore
            // point; the rescan has not re-derived it yet.
            let tx_hash = B256::repeat_byte(0x5a);
            let deposit = deposit_id(1, tx_hash, 0);
            let event_id = credited_event_id(deposit);
            let amount_atomic = "100000000000000000000";
            let delivered = credited_event(
                &harness,
                deposit,
                tx_hash,
                &forwarder,
                amount_atomic,
                (2_500, "0.25000000", "spot"),
            );
            let path = "/v1/admin/restore/events";
            let import =
                |deliveries: Vec<Value>| json!({"deliveries": deliveries, "reason": "merchant log"});
            let imported = harness
                .admin(Method::POST, path, &import(vec![harness.delivered(&delivered)]))
                .await?;
            ensure!(imported.status == StatusCode::OK, "{}", imported.body);
            ensure!(imported.body["data"][0]["result"] == "imported");
            let stored = |id: Uuid| {
                let pool = harness.pool.clone();
                async move {
                    let stored: (Value, i64, i64) = sqlx::query_as(
                        "SELECT data, extract(epoch FROM created)::bigint, \
                                (SELECT count(*) FROM webhook_deliveries WHERE event_id = $1) \
                         FROM events WHERE id = $1",
                    )
                    .bind(id)
                    .fetch_one(&pool)
                    .await?;
                    anyhow::Ok(stored)
                }
            };
            ensure!(stored(event_id).await? == (delivered["data"].clone(), 1_790_000_000, 0));

            // After the unfreeze the rescan credits the deposit again: its event is recorded
            // already, so nothing is delivered and the delivered body stays.
            let mut transaction = harness.pool.begin().await?;
            db::enqueue_in(
                &mut transaction,
                &topup::routes::RouteSet::default(),
                &db::NewOutboxEvent {
                    id: event_id,
                    event_type: "deposit.credited".to_owned(),
                    account_id: harness.account.id,
                    livemode: true,
                    object: db::EventObject::Deposit(deposit),
                    next_attempt_at: Utc::now(),
                    actor: db::SYSTEM_ACTOR.to_owned(),
                    request: None,
                    signing_key_version: None,
                },
                None,
            )
            .await?;
            transaction.commit().await?;
            ensure!(stored(event_id).await? == (delivered["data"].clone(), 1_790_000_000, 0));

            // Importing it again matches.
            let again = harness
                .admin(Method::POST, path, &import(vec![harness.delivered(&delivered)]))
                .await?;
            ensure!(again.body["data"][0]["result"] == "matches");

            // Only what the service signed is imported: a body changed after signing, a body
            // signed by another key, and a signature of another account are refused, and
            // nothing of the request is imported.
            let mut altered = harness.delivered(&delivered);
            let mut body = delivered.clone();
            body["data"]["object"]["amount"] = json!(9_999);
            altered["body"] = json!(body.to_string());
            let forged = delivery(&body, &SigningKey::from_bytes(&[5; 32]));
            let other_account = delivery(&body, &webhook_key("acct_other", true, 1));
            let mut other_id = harness.delivered(&delivered);
            other_id["webhook_id"] = json!(topup::ids::format(topup::ids::EVENT, Uuid::new_v4()));
            for refused in [altered, forged, other_account, other_id] {
                let answer = harness
                    .admin(Method::POST, path, &import(vec![refused]))
                    .await?;
                ensure!(answer.status == StatusCode::BAD_REQUEST, "{}", answer.body);
                ensure!(answer.body["error"]["param"] == "deliveries");
            }
            ensure!(stored(event_id).await? == (delivered["data"].clone(), 1_790_000_000, 0));
            // A key rolled after the restore point, lost with it, still verifies.
            let mut rolled = delivered.clone();
            rolled["id"] = json!(topup::ids::format(
                topup::ids::EVENT,
                topup_core::identity::event_id("deposit.reversed", deposit)
            ));
            rolled["type"] = json!("deposit.reversed");
            let answer = harness
                .admin(
                    Method::POST,
                    path,
                    &import(vec![delivery(
                        &rolled,
                        &webhook_key(harness.account_id(), true, 2),
                    )]),
                )
                .await?;
            ensure!(answer.body["data"][0]["result"] == "imported", "{}", answer.body);
            // Up to four rolls lost with the restore are tried, and no more.
            for (version, verifies) in [(5, true), (6, false)] {
                let answer = harness
                    .admin(
                        Method::POST,
                        path,
                        &import(vec![delivery(
                            &rolled,
                            &webhook_key(harness.account_id(), true, version),
                        )]),
                    )
                    .await?;
                ensure!(
                    (answer.status == StatusCode::OK) == verifies,
                    "v{version}: {}",
                    answer.body
                );
            }

            // Compared with the ledger: pending until the rescan values the deposit.
            let findings = |body: &Value| body["delivered_events"]["findings"].clone();
            let status = harness
                .admin(Method::GET, "/v1/admin/restore", &Value::Null)
                .await?;
            ensure!(status.body["delivered_events"]["imported"] == 2);
            ensure!(findings(&status.body)[0]["status"] == "pending");
            record_deposit(&harness, tx_hash, address_id, U256::from(10_u64).pow(U256::from(20)))
                .await?;
            sqlx::query("UPDATE deposits SET credit_minor = 2600 WHERE id = $1")
                .bind(deposit)
                .execute(&harness.owner)
                .await?;
            let status = harness
                .admin(Method::GET, "/v1/admin/restore", &Value::Null)
                .await?;
            let finding = &findings(&status.body)[0];
            ensure!(finding["status"] == "mismatch", "{finding}");
            ensure!(finding["delivered_amount"] == "2500" && finding["ledger_amount"] == "2600");
            sqlx::query("UPDATE deposits SET credit_minor = 2500 WHERE id = $1")
                .bind(deposit)
                .execute(&harness.owner)
                .await?;
            let status = harness
                .admin(Method::GET, "/v1/admin/restore", &Value::Null)
                .await?;
            ensure!(findings(&status.body) == json!([]));

            // An event whose id is not the one its type and deposit derive is refused.
            let mut forged = delivered.clone();
            forged["id"] = json!(topup::ids::format(topup::ids::EVENT, Uuid::new_v4()));
            let refused = harness
                .admin(Method::POST, path, &import(vec![harness.delivered(&forged)]))
                .await?;
            ensure!(refused.status == StatusCode::BAD_REQUEST);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_restored_deposit_keeps_its_delivered_credit_for_its_refunds_and_reversal() -> Result<()>
{
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            let address = harness
                .merchant(
                    Method::POST,
                    "/v1/deposit_addresses",
                    &json!({"client_reference_id": "team-5"}),
                )
                .await?;
            let forwarder = address.body["address"]
                .as_str()
                .context("address")?
                .to_owned();
            let address_id: Uuid =
                sqlx::query_scalar("SELECT id FROM addresses WHERE address = $1 AND chain_id = 1")
                    .bind(&forwarder)
                    .fetch_one(&harness.pool)
                    .await?;
            let restore = harness.restore().await?;

            // The merchant was told 100 PHA credited $25.00 at $0.25, after the restore point.
            let tx_hash = B256::repeat_byte(0x6b);
            let deposit = deposit_id(1, tx_hash, 0);
            let delivered = credited_event(
                &harness,
                deposit,
                tx_hash,
                &forwarder,
                "100000000000000000000",
                (2_500, "0.25000000", "spot"),
            );
            let imported = harness
                .admin(
                    Method::POST,
                    "/v1/admin/restore/events",
                    &json!({"deliveries": [harness.delivered(&delivered)], "reason": "log"}),
                )
                .await?;
            ensure!(
                imported.body["data"][0]["result"] == "imported",
                "{}",
                imported.body
            );

            // The rescan re-derives it; spot is now $0.20, which would credit $20.00.
            record_deposit(
                &harness,
                tx_hash,
                address_id,
                U256::from(10_u64).pow(U256::from(20)),
            )
            .await?;
            let recipient = Address::from_str(&forwarder)?;
            confirm(&harness, deposit, recipient, 20_000_000).await?;
            ensure!(
                valuation(&harness, deposit).await?
                    == (
                        "confirmed".to_owned(),
                        "spot".to_owned(),
                        "25000000".to_owned(),
                        "2500".to_owned()
                    )
            );
            let valued_at: i64 = sqlx::query_scalar(
                "SELECT extract(epoch FROM valuation_at)::bigint FROM deposits WHERE id = $1",
            )
            .bind(deposit)
            .fetch_one(&harness.pool)
            .await?;
            ensure!(valued_at == 1_790_000_000);
            let status = harness
                .admin(Method::GET, "/v1/admin/restore", &Value::Null)
                .await?;
            ensure!(status.body["delivered_events"]["findings"] == json!([]));

            // Its refunds and its reversal reference the delivered credit.
            sqlx::query("UPDATE deposits SET state = 'credited', final_at = now() WHERE id = $1")
                .bind(deposit)
                .execute(&harness.owner)
                .await?;
            sqlx::query(
                "INSERT INTO refunds (id, account_id, livemode, chain_id, deposit_id, \
                 amount_atomic, destination_address, status, tx_hash, receipt_log_index, paid_at) \
                 SELECT $1, account_id, livemode, chain_id, id, 25000000000000000000, $3, \
                        'succeeded', $4, 0, now() \
                 FROM deposits WHERE id = $2",
            )
            .bind(Uuid::new_v4())
            .bind(deposit)
            .bind(format!("{:#x}", Address::repeat_byte(0x75)))
            .bind(format!("{:#x}", B256::repeat_byte(0x76)))
            .execute(&harness.owner)
            .await?;
            harness.unfreeze(&restore).await?;
            let path = format!(
                "/v1/deposits/{}",
                topup::ids::format(topup::ids::DEPOSIT, deposit)
            );
            let object = harness.merchant(Method::GET, &path, &Value::Null).await?;
            ensure!(object.status == StatusCode::OK, "{}", object.body);
            ensure!(object.body["amount"] == 2_500 && object.body["exchange_rate"] == "0.25000000");
            ensure!(object.body["amount_refunded"] == 625, "{}", object.body);
            sqlx::query("DELETE FROM refunds WHERE deposit_id = $1")
                .bind(deposit)
                .execute(&harness.owner)
                .await?;
            sqlx::query("UPDATE deposits SET state = 'reversed', final_at = NULL WHERE id = $1")
                .bind(deposit)
                .execute(&harness.owner)
                .await?;
            let object = harness.merchant(Method::GET, &path, &Value::Null).await?;
            ensure!(object.body["amount_reversed"] == 2_500, "{}", object.body);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_delivered_credit_the_chain_contradicts_holds_the_deposit_until_discarded() -> Result<()>
{
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            let address = harness
                .merchant(
                    Method::POST,
                    "/v1/deposit_addresses",
                    &json!({"client_reference_id": "team-6"}),
                )
                .await?;
            let forwarder = address.body["address"]
                .as_str()
                .context("address")?
                .to_owned();
            let address_id: Uuid =
                sqlx::query_scalar("SELECT id FROM addresses WHERE address = $1 AND chain_id = 1")
                    .bind(&forwarder)
                    .fetch_one(&harness.pool)
                    .await?;
            harness.restore().await?;

            // A delivered credit of 100 PHA, but the chain's transfer is 50 PHA.
            let tx_hash = B256::repeat_byte(0x7c);
            let deposit = deposit_id(1, tx_hash, 0);
            let delivered = credited_event(
                &harness,
                deposit,
                tx_hash,
                &forwarder,
                "100000000000000000000",
                (2_500, "0.25000000", "spot"),
            );
            harness
                .admin(
                    Method::POST,
                    "/v1/admin/restore/events",
                    &json!({"deliveries": [harness.delivered(&delivered)], "reason": "log"}),
                )
                .await?;
            record_deposit(
                &harness,
                tx_hash,
                address_id,
                U256::from(5_u64) * U256::from(10_u64).pow(U256::from(19)),
            )
            .await?;
            let finding = harness
                .admin(Method::GET, "/v1/admin/restore", &Value::Null)
                .await?
                .body["delivered_events"]["findings"][0]
                .clone();
            ensure!(finding["status"] == "contradicted", "{finding}");

            // The confirm step holds it: not valued, not credited.
            let recipient = Address::from_str(&forwarder)?;
            confirm(&harness, deposit, recipient, 20_000_000).await?;
            let (state, attempt_error): (String, Option<String>) = sqlx::query_as(
                "SELECT state, (SELECT evidence ->> 'error' FROM transitions \
                                WHERE deposit_id = $1 ORDER BY created_at DESC LIMIT 1) \
                 FROM deposits WHERE id = $1",
            )
            .bind(deposit)
            .fetch_one(&harness.pool)
            .await?;
            ensure!(state == "detected", "{state}");
            ensure!(attempt_error.as_deref() == Some("delivered_event_contradicts_chain"));
            let credit: Option<String> =
                sqlx::query_scalar("SELECT credit_minor::text FROM deposits WHERE id = $1")
                    .bind(deposit)
                    .fetch_one(&harness.pool)
                    .await?;
            ensure!(credit.is_none());

            // The operator discards the delivered credit; the deposit is valued from the chain.
            let path = "/v1/admin/restore/delivered_credits/discard";
            let request = json!({
                "deposit": topup::ids::format(topup::ids::DEPOSIT, deposit),
                "reason": "INC-7: chain shows 50 PHA; settled with the merchant",
            });
            let discarded = harness.admin(Method::POST, path, &request).await?;
            ensure!(discarded.status == StatusCode::OK, "{}", discarded.body);
            ensure!(discarded.body["discarded"] == true);
            let again = harness.admin(Method::POST, path, &request).await?;
            ensure!(again.status == StatusCode::OK);
            let unknown = harness
                .admin(
                    Method::POST,
                    path,
                    &json!({
                        "deposit": topup::ids::format(topup::ids::DEPOSIT, Uuid::new_v4()),
                        "reason": "x",
                    }),
                )
                .await?;
            ensure!(unknown.status == StatusCode::NOT_FOUND);
            let audited: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM audit WHERE action = 'restore.delivered_credit_discard'",
            )
            .fetch_one(&harness.pool)
            .await?;
            ensure!(audited == 1);
            sqlx::query("UPDATE deposits SET next_attempt_at = now() WHERE id = $1")
                .bind(deposit)
                .execute(&harness.owner)
                .await?;
            confirm(&harness, deposit, recipient, 20_000_000).await?;
            ensure!(
                valuation(&harness, deposit).await?
                    == (
                        "confirmed".to_owned(),
                        "spot".to_owned(),
                        "20000000".to_owned(),
                        "1000".to_owned()
                    )
            );
            let finding = harness
                .admin(Method::GET, "/v1/admin/restore", &Value::Null)
                .await?
                .body["delivered_events"]["findings"][0]
                .clone();
            ensure!(finding["status"] == "mismatch", "{finding}");
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_quote_given_out_after_the_restore_point_is_reissued_and_credited() -> Result<()> {
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            harness.scan_to(100, Utc::now()).await?;
            let restore = harness.restore().await?;
            harness.scan_to(250, Utc::now()).await?;

            // The merchant's record of a quote created after the restore point: $10.00 at $0.10.
            let id = Uuid::new_v4();
            let qt = topup::ids::format(topup::ids::QUOTE, id);
            let contracts = &harness.route.chain.contracts;
            let address = forwarder_address(
                contracts.forwarder_factory,
                contracts.implementation,
                seed::FIXTURE_TREASURY,
                quote_salt(harness.account_id(), "team-q", &qt),
            );
            // Terms no route issues today (a window past the route's, another amount) are the
            // merchant's record all the same: the route may have changed since.
            let created = Utc::now().timestamp() - 60;
            let secret = harness
                .client_reads
                .key()
                .issue(harness.account_id(), &qt)?;
            let request = json!({
                "account": harness.account_id(),
                "livemode": true,
                "id": qt,
                "client_reference_id": "team-q",
                "chain_id": 1,
                "asset": "pha",
                "amount": 1_000,
                "amount_atomic": "100000000000000000000",
                "exchange_rate": "0.10000000",
                "address": format!("{address:#x}"),
                "created": created,
                "expires_at": created + 3_600,
                "metadata": {"order_id": "o-1"},
                "client_secret": secret,
                "reason": "the merchant's quote log",
            });
            let path = "/v1/admin/restore/quotes";
            // A forged address, a client secret the service did not issue for the quote, issued
            // for it to another account (another merchant's record of the id, or the payer's page
            // of the owner's quote), or issued before owner tags, or an asset no route has is
            // refused, and no customer is created.
            let other_quote = topup::ids::format(topup::ids::QUOTE, Uuid::new_v4());
            for (field, value) in [
                (
                    "address",
                    json!(format!("{:#x}", Address::repeat_byte(0x99))),
                ),
                (
                    "client_secret",
                    json!(
                        harness
                            .client_reads
                            .key()
                            .issue(harness.account_id(), &other_quote)?
                    ),
                ),
                (
                    "client_secret",
                    json!(harness.client_reads.key().issue("acct_other", &qt)?),
                ),
                ("client_secret", json!(legacy_secret(&qt))),
                (
                    "client_secret",
                    json!(format!("{qt}_secret_{}", "0".repeat(64))),
                ),
                ("asset", json!("usdc")),
            ] {
                let mut forged = request.clone();
                forged[field] = value;
                let refused = harness.admin(Method::POST, path, &forged).await?;
                ensure!(
                    refused.status == StatusCode::BAD_REQUEST,
                    "{field}: {}",
                    refused.body
                );
            }
            let (quotes, customers): (i64, i64) = sqlx::query_as(
                "SELECT (SELECT count(*) FROM quotes), \
                        (SELECT count(*) FROM customers WHERE client_reference_id = 'team-q')",
            )
            .fetch_one(&harness.pool)
            .await?;
            ensure!((quotes, customers) == (0, 0));

            // Re-issued first without its secret, found later in the merchant's records.
            let mut without_secret = request.clone();
            without_secret["client_secret"] = Value::Null;
            let reissued = harness.admin(Method::POST, path, &without_secret).await?;
            ensure!(reissued.status == StatusCode::OK, "{}", reissued.body);
            ensure!(reissued.body["reissued"] == true);
            let public_read = |secret: String| {
                let app = harness.app.clone();
                let path = format!("/v1/quotes/{qt}?client_secret={secret}");
                async move { anonymous(&app, &path).await }
            };
            ensure!(public_read(secret.clone()).await?.status == StatusCode::NOT_FOUND);
            let quote = &reissued.body["quote"];
            ensure!(quote["id"] == qt && quote["address"] == format!("{address:#x}"));
            ensure!(quote["amount"] == 1_000 && quote["exchange_rate"] == "0.10000000");
            ensure!(quote["metadata"] == json!({"order_id": "o-1"}));
            // Its lock is never honoured, so its window closes at the restore: its page shows it
            // expired instead of asking for a payment at the locked price.
            ensure!(
                quote["expires_at"] == restore.detected_at.timestamp(),
                "{quote}"
            );
            // A repeat with a secret of the quote issued to another account, or without an owner
            // tag, is refused and adds nothing, though a read accepts its tag.
            for other in [
                harness.client_reads.key().issue("acct_other", &qt)?,
                legacy_secret(&qt),
            ] {
                ensure!(harness.client_reads.key().verify(&qt, &other));
                let mut other_secret = request.clone();
                other_secret["client_secret"] = json!(other);
                let refused = harness.admin(Method::POST, path, &other_secret).await?;
                ensure!(
                    refused.status == StatusCode::BAD_REQUEST
                        && refused.body["error"]["param"] == "client_secret",
                    "{}",
                    refused.body
                );
                ensure!(public_read(other).await?.status == StatusCode::NOT_FOUND);
            }
            // A repeat with its secret adds it; the payer's page reads the quote again.
            let repeated = harness.admin(Method::POST, path, &request).await?;
            ensure!(repeated.body["reissued"] == false, "{}", repeated.body);
            let public = public_read(secret.clone()).await?;
            ensure!(public.status == StatusCode::OK, "{}", public.body);
            // Once it has one, another secret of the quote does not replace it.
            let other = harness
                .client_reads
                .key()
                .issue(harness.account_id(), &qt)?;
            let mut other_secret = request.clone();
            other_secret["client_secret"] = json!(other);
            let repeated = harness.admin(Method::POST, path, &other_secret).await?;
            ensure!(repeated.body["reissued"] == false);
            ensure!(public_read(other).await?.status == StatusCode::NOT_FOUND);
            ensure!(public_read(secret.clone()).await?.status == StatusCode::OK);
            let secret_restored: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM audit WHERE action = 'quote.client_secret_restore'",
            )
            .fetch_one(&harness.pool)
            .await?;
            ensure!(secret_restored == 1);

            // The scanner watches it from the restored cursor, so the rescan finds its payment.
            let scanned = db::list_scan_addresses(&harness.pool, 1).await?;
            let watched = scanned
                .iter()
                .find(|scan| scan.address == address)
                .context("the re-issued quote's address is scanned")?;
            ensure!(!watched.backfilled && watched.backfill_start() == 100);
            ensure!(restore.restored_cursors.get(&1) == Some(&100));
            let status = harness
                .admin(Method::GET, "/v1/admin/restore", &Value::Null)
                .await?;
            ensure!(status.body["rescan"][0]["pending_backfills"] == 1);

            // Its locked price is the merchant's record, never applied: at $0.08 spot the
            // payment credits $8.00, not the quote's $10.00.
            let address_id: Uuid =
                sqlx::query_scalar("SELECT id FROM addresses WHERE quote_id = $1")
                    .bind(id)
                    .fetch_one(&harness.pool)
                    .await?;
            let tx_hash = B256::repeat_byte(0x8d);
            let deposit = record_deposit(
                &harness,
                tx_hash,
                address_id,
                U256::from(10_u64).pow(U256::from(20)),
            )
            .await?;
            confirm(&harness, deposit, address, 8_000_000).await?;
            ensure!(
                valuation(&harness, deposit).await?
                    == (
                        "confirmed".to_owned(),
                        "spot".to_owned(),
                        "8000000".to_owned(),
                        "800".to_owned()
                    )
            );

            // A signed deposit.credited that carries the quote's credit is the evidence: a second
            // payment the merchant was told was credited at the quote is valued at it.
            let paid = B256::repeat_byte(0x8e);
            let second = deposit_id(1, paid, 0);
            let delivered = credited_event(
                &harness,
                second,
                paid,
                &format!("{address:#x}"),
                "100000000000000000000",
                (1_000, "0.10000000", "quote"),
            );
            harness
                .admin(
                    Method::POST,
                    "/v1/admin/restore/events",
                    &json!({"deliveries": [harness.delivered(&delivered)], "reason": "log"}),
                )
                .await?;
            record_deposit(
                &harness,
                paid,
                address_id,
                U256::from(10_u64).pow(U256::from(20)),
            )
            .await?;
            confirm(&harness, second, address, 8_000_000).await?;
            ensure!(
                valuation(&harness, second).await?
                    == (
                        "confirmed".to_owned(),
                        "lock".to_owned(),
                        "10000000".to_owned(),
                        "1000".to_owned()
                    )
            );
            let (status, consumed_by): (String, Option<Uuid>) =
                sqlx::query_as("SELECT status, consumed_by FROM quotes WHERE id = $1")
                    .bind(id)
                    .fetch_one(&harness.pool)
                    .await?;
            ensure!(status == "consumed" && consumed_by == Some(second));
            let audited: i64 =
                sqlx::query_scalar("SELECT count(*) FROM audit WHERE action = 'quote.reissue'")
                    .fetch_one(&harness.pool)
                    .await?;
            ensure!(audited == 1);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_credit_delivered_by_the_service_round_trips_through_a_restore() -> Result<()> {
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            let receiver = Receiver::default();
            sqlx::query("UPDATE webhook_endpoints SET url = $2 WHERE account_id = $1")
                .bind(harness.account.id)
                .bind(receiver.serve().await?)
                .execute(&harness.owner)
                .await?;
            let address = harness
                .merchant(
                    Method::POST,
                    "/v1/deposit_addresses",
                    &json!({"client_reference_id": "team-rt"}),
                )
                .await?;
            let forwarder =
                Address::from_str(address.body["address"].as_str().context("address")?)?;
            let address_id: Uuid =
                sqlx::query_scalar("SELECT id FROM addresses WHERE address = $1 AND chain_id = 1")
                    .bind(format!("{forwarder:#x}"))
                    .fetch_one(&harness.pool)
                    .await?;

            // The service credits 100 PHA at $0.25 through the real steps and delivers
            // deposit.credited, which the merchant's receiver records.
            let tx_hash = B256::repeat_byte(0x9a);
            let deposit = record_deposit(
                &harness,
                tx_hash,
                address_id,
                U256::from(10_u64).pow(U256::from(20)),
            )
            .await?;
            let steps = |confirm: ConfirmStep| -> Result<StepSet> {
                let screen = topup::steps::screen::ScreenStep::new(
                    harness.pool.clone(),
                    [topup::steps::screen::ScreenRoute::new(
                        harness.route.route.clone(),
                        harness.route.version,
                        harness.route.screening.sanctions_oracle,
                        topup_core::screening::Bounds::from(&harness.route.screening),
                        Arc::new(ClearSanctions),
                    )],
                )?;
                Ok(StepSet::new(Box::new(confirm), Box::new(screen)))
            };
            for _ in ["confirm", "screen and credit"] {
                let step = confirm_step(&harness, deposit, forwarder, 25_000_000).await?;
                run_pump(&harness, deposit, steps(step)?).await?;
            }
            let credited = valuation(&harness, deposit).await?;
            ensure!(credited.0 == "credited", "{credited:?}");
            // In whole seconds, as the API renders `valued_at` and the delivery carries it.
            let valued_at = |pool: PgPool| async move {
                let at: i64 = sqlx::query_scalar(
                    "SELECT floor(extract(epoch FROM valuation_at))::bigint FROM deposits \
                     WHERE id = $1",
                )
                .bind(deposit)
                .fetch_one(&pool)
                .await?;
                anyhow::Ok(at)
            };
            let first_valued_at = valued_at(harness.pool.clone()).await?;
            let worker = topup::outbox::DeliveryWorker::new(
                harness.pool.clone(),
                Arc::new(KeySigner),
                true,
                topup::outbox::DeliveryConfig {
                    proxy: None,
                    ..topup::outbox::DeliveryConfig::default()
                },
            )?;
            ensure!(worker.run_once().await? >= 1);
            let delivered = receiver
                .deliveries()
                .into_iter()
                .find(|delivery| {
                    delivery["webhook_id"]
                        == topup::ids::format(topup::ids::EVENT, credited_event_id(deposit))
                })
                .context("the receiver recorded deposit.credited")?;

            // The restore loses the credit and its event: the rescan finds the deposit detected.
            for statement in [
                "DELETE FROM webhook_deliveries WHERE event_id = $1",
                "DELETE FROM events WHERE id = $1",
            ] {
                sqlx::query(statement)
                    .bind(credited_event_id(deposit))
                    .execute(&harness.owner)
                    .await?;
            }
            sqlx::query(
                "UPDATE deposits SET state = 'detected', valuation_at = NULL, price_scaled = NULL, \
                 price_source = NULL, credit_minor = NULL, quote = NULL, next_attempt_at = now() \
                 WHERE id = $1",
            )
            .bind(deposit)
            .execute(&harness.owner)
            .await?;
            harness.restore().await?;

            // The delivery as recorded verifies and carries the delivered credit into the ledger,
            // though spot is now $0.20.
            let imported = harness
                .admin(
                    Method::POST,
                    "/v1/admin/restore/events",
                    &json!({"deliveries": [delivered], "reason": "the merchant's receiver log"}),
                )
                .await?;
            ensure!(
                imported.body["data"][0]["result"] == "imported",
                "{}",
                imported.body
            );
            confirm(&harness, deposit, forwarder, 20_000_000).await?;
            let restored = valuation(&harness, deposit).await?;
            ensure!(
                (
                    restored.1.as_str(),
                    restored.2.as_str(),
                    restored.3.as_str()
                ) == (
                    credited.1.as_str(),
                    credited.2.as_str(),
                    credited.3.as_str()
                ),
                "{restored:?} != {credited:?}"
            );
            ensure!(valued_at(harness.pool.clone()).await? == first_valued_at);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn the_operator_attests_a_frozen_instance_that_merchant_keys_cannot() -> Result<()> {
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            harness.restore().await?;
            let nonce = "00112233445566778899aabbccddeeff";
            let merchant = harness
                .merchant(
                    Method::GET,
                    &format!("/v1/attestation?nonce={nonce}"),
                    &Value::Null,
                )
                .await?;
            ensure!(merchant.body["error"]["code"] == "service_restoring");
            for app in [&harness.app, &harness.read_only] {
                let attested = harness
                    .admin_on(
                        app,
                        Method::GET,
                        &format!(
                            "/v1/admin/attestation?account={}&livemode=true&nonce={nonce}",
                            harness.account_id()
                        ),
                        &Value::Null,
                    )
                    .await?;
                ensure!(attested.status == StatusCode::OK, "{}", attested.body);
                ensure!(attested.body["account"] == harness.account_id());
                ensure!(attested.body["livemode"] == true);
                let key = webhook_key(harness.account_id(), true, 1);
                ensure!(
                    attested.body["webhook_keys"][0]["public_key"]
                        == hex::encode(key.verifying_key().to_bytes())
                );
                let expected = topup_adapters::attestation::report_data(
                    &hex::decode(nonce)?,
                    harness.account_id(),
                    true,
                    &[AttestedWebhookKey {
                        version: 1,
                        public_key: Ed25519PublicKey(key.verifying_key().to_bytes()),
                    }],
                )
                .context("report data")?;
                ensure!(attested.body["report_data"] == hex::encode(expected));
            }
            let unknown = harness
                .admin(
                    Method::GET,
                    &format!(
                        "/v1/admin/attestation?account={}&livemode=true&nonce={nonce}",
                        topup::ids::format(topup::ids::ACCOUNT, Uuid::new_v4())
                    ),
                    &Value::Null,
                )
                .await?;
            ensure!(unknown.status == StatusCode::NOT_FOUND);
            let bad_nonce = harness
                .admin(
                    Method::GET,
                    &format!(
                        "/v1/admin/attestation?account={}&livemode=true&nonce=zz",
                        harness.account_id()
                    ),
                    &Value::Null,
                )
                .await?;
            ensure!(bad_nonce.status == StatusCode::BAD_REQUEST);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn the_restore_check_instance_takes_only_reads_and_the_restore_reconciliation() -> Result<()>
{
    support::with_database(|database| {
        Box::pin(async move {
            let harness = Harness::new(database).await?;
            // restore-check records the freeze in parallel with the read-only API, and may fail
            // before it does: a merchant key reads nothing there even before the freeze exists.
            ensure!(!restore_mode::is_frozen(&harness.pool).await?);
            let early = harness
                .merchant_with(
                    &harness.read_only,
                    Method::GET,
                    "/v1/account",
                    &Value::Null,
                    &harness.key,
                )
                .await?;
            ensure!(
                early.body["error"]["code"] == "service_restoring",
                "{}",
                early.body
            );
            harness.restore().await?;
            let refused = harness
                .merchant_with(
                    &harness.read_only,
                    Method::POST,
                    "/v1/api_keys",
                    &json!({"name": "ci"}),
                    &harness.key,
                )
                .await?;
            ensure!(refused.status == StatusCode::SERVICE_UNAVAILABLE);
            ensure!(refused.body["error"]["code"] == "service_restoring");
            ensure!(refused.retry_after.as_deref() == Some("300"));
            let admin = harness
                .admin_on(
                    &harness.read_only,
                    Method::POST,
                    &format!("/v1/admin/accounts/{}/pause", harness.account_id()),
                    &json!({"scopes": ["quotes"], "reason": "x"}),
                )
                .await?;
            ensure!(admin.body["error"]["code"] == "service_restoring");
            // Frozen, a merchant read is refused too; the operator's reads work.
            let read = harness
                .merchant_with(
                    &harness.read_only,
                    Method::GET,
                    "/v1/account",
                    &Value::Null,
                    &harness.key,
                )
                .await?;
            ensure!(read.body["error"]["code"] == "service_restoring");
            let status = harness
                .admin_on(
                    &harness.read_only,
                    Method::GET,
                    "/v1/admin/restore",
                    &Value::Null,
                )
                .await?;
            ensure!(status.status == StatusCode::OK && status.body["frozen"] == true);
            // The restore reconciliation reaches its handler: nothing scans here, so the freeze
            // cannot be lifted, but a key can be revoked again.
            let unfreeze = harness
                .admin_on(
                    &harness.read_only,
                    Method::POST,
                    "/v1/admin/restore/unfreeze",
                    &checklist("drill"),
                )
                .await?;
            ensure!(unfreeze.body["error"]["code"] == "restore_rescan_incomplete");
            let leaked = seed::create_api_key(&harness.pool, harness.account.id, true).await?;
            let revoked = harness
                .admin_on(
                    &harness.read_only,
                    Method::POST,
                    "/v1/admin/restore/api_keys/revoke",
                    &json!({
                        "account": harness.account_id(),
                        "prefix": "ppay_sk_live_",
                        "last4": &leaked[leaked.len() - 4..],
                        "reason": "merchant revoked it",
                    }),
                )
                .await?;
            ensure!(revoked.status == StatusCode::OK, "{}", revoked.body);
            Ok(())
        })
    })
    .await
}

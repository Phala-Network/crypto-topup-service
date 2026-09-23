//! Proof that the suite passes the conforming reference and that every deliberately broken
//! variant fails exactly the cases of its obligation. Requires Foundry's `anvil` and `forge` on
//! `PATH`; the anvil-backed tests are skipped with a message otherwise, unless
//! `CONFORMANCE_REQUIRE_TOOLS=1`, which turns every skip into a failure.

use std::collections::{BTreeSet, VecDeque};
use std::future::Future;
use std::io::{BufRead as _, BufReader};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use topup_adapters::settlement::http::{
    SettlementAnswer, SettlementApi as _, SettlementClient, SettlementRequest,
};
use topup_conformance::DEV_SETTLEMENT_SEED;
use topup_conformance::chain::{ChainFixture, Manifest, PrepareOptions, Rpc};
use topup_conformance::reference::{BrokenVariant, ReferenceConfig, ReferenceState, router};
use topup_conformance::report::{Report, TestStatus};
use topup_conformance::signer_handle;
use topup_conformance::suite::{Accounts, Caps, Evidence, Restart, SuiteConfig, key_from_evidence};

const CAPS: Caps = Caps {
    per_deposit: 1_000,
    per_period: 5_000,
    period: Duration::from_secs(3_600),
};

/// Generous bound on anvil start-up under CPU contention; a healthy start takes well under 1 s.
const ANVIL_START: Duration = Duration::from_secs(60);

/// Serializes `forge create` so parallel tests do not race on the Foundry build cache.
static FORGE: Mutex<()> = Mutex::const_new(());

struct Anvil {
    child: Child,
    rpc_url: String,
}

impl Anvil {
    /// Starts anvil on a port it picks itself (no free-port race with parallel tests) and
    /// waits until it answers JSON-RPC, so a loaded host only slows the start down.
    async fn start() -> Result<Option<Self>> {
        if !command_available("anvil") || !command_available("forge") {
            skip("anvil or forge is not on PATH")?;
            return Ok(None);
        }
        let mut child = Command::new("anvil")
            .args(["--port", "0", "--chain-id", "31337"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("start anvil")?;
        let stdout = child.stdout.take().context("anvil stdout")?;
        let stderr = child.stderr.take().context("anvil stderr")?;
        let mut anvil = Self {
            child,
            rpc_url: String::new(),
        };
        // Both readers keep draining after start-up so anvil never blocks on a full pipe; the
        // stderr reader returns its last lines at EOF for the start-up failure message.
        let stderr_tail = std::thread::spawn(move || {
            let mut tail = VecDeque::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if tail.len() == 20 {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
            Vec::from(tail).join("\n")
        });
        let (address_tx, address_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(address) = line.strip_prefix("Listening on ") {
                    let _ = address_tx.send(address.trim().to_owned());
                }
            }
        });
        let address = tokio::task::spawn_blocking(move || address_rx.recv_timeout(ANVIL_START))
            .await?
            .context("anvil did not report its listening address");
        let error = match address {
            Ok(address) => {
                anvil.rpc_url = format!("http://{address}");
                match anvil.wait_for_rpc().await {
                    Ok(()) => return Ok(Some(anvil)),
                    Err(error) => error,
                }
            }
            Err(error) => error,
        };
        // Stopping anvil closes stderr, so the reader finishes with the complete tail.
        drop(anvil);
        let tail = stderr_tail.join().unwrap_or_default();
        Err(error.context(format!("anvil stderr tail:\n{tail}")))
    }

    async fn wait_for_rpc(&self) -> Result<()> {
        let rpc = Rpc::new(&self.rpc_url)?;
        let deadline = tokio::time::Instant::now() + ANVIL_START;
        loop {
            match rpc.chain_id().await {
                Ok(_) => return Ok(()),
                Err(error) if tokio::time::Instant::now() >= deadline => {
                    return Err(error.context("anvil did not answer eth_chainId"));
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    async fn prepare(&self) -> Result<Manifest> {
        let _guard = FORGE.lock().await;
        topup_conformance::chain::prepare(
            &self.rpc_url,
            PrepareOptions {
                chain_id: Some(31_337),
                contracts_dir: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts"),
                ..PrepareOptions::default()
            },
        )
        .await
    }
}

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Skips with a message, or fails when `CONFORMANCE_REQUIRE_TOOLS=1`.
fn skip(reason: &str) -> Result<()> {
    if std::env::var("CONFORMANCE_REQUIRE_TOOLS").is_ok_and(|value| value == "1") {
        bail!("CONFORMANCE_REQUIRE_TOOLS=1 but {reason}");
    }
    eprintln!("skipping conformance test: {reason}");
    Ok(())
}

fn command_available(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn config(manifest: Manifest, broken: BrokenVariant) -> ReferenceConfig {
    ReferenceConfig {
        verifying_key: SigningKey::from_bytes(&DEV_SETTLEMENT_SEED).verifying_key(),
        keyid: "settlement/v1".to_owned(),
        per_deposit_cap: CAPS.per_deposit,
        per_period_cap: CAPS.per_period,
        period: CAPS.period,
        refused_account_id: "conformance-refused".to_owned(),
        processing_account_id: "conformance-processing".to_owned(),
        broken,
        manifest,
    }
}

type Rebuild = Box<
    dyn Fn(ReferenceState) -> Pin<Box<dyn Future<Output = Result<ReferenceState>> + Send>>
        + Send
        + Sync,
>;

struct Running {
    state: ReferenceState,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

/// An in-process reference server which can be restarted on the same address.
struct Server {
    address: SocketAddr,
    app: fn(ReferenceState) -> Router,
    rebuild: Rebuild,
    current: Mutex<Option<Running>>,
}

impl Server {
    async fn start(
        state: ReferenceState,
        app: fn(ReferenceState) -> Router,
        rebuild: Rebuild,
    ) -> Result<Arc<Self>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let server = Arc::new(Self {
            address,
            app,
            rebuild,
            current: Mutex::new(None),
        });
        server.serve(state, listener).await;
        Ok(server)
    }

    async fn serve(&self, state: ReferenceState, listener: tokio::net::TcpListener) {
        let (shutdown, signal) = oneshot::channel();
        let app = (self.app)(state.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = signal.await;
                })
                .await;
        });
        *self.current.lock().await = Some(Running {
            state,
            shutdown,
            task,
        });
    }

    fn url(&self) -> String {
        format!("http://{}/settlements", self.address)
    }

    async fn stop(&self) -> Option<ReferenceState> {
        let Running {
            state,
            shutdown,
            mut task,
        } = self.current.lock().await.take()?;
        let _ = shutdown.send(());
        if tokio::time::timeout(Duration::from_secs(5), &mut task)
            .await
            .is_err()
        {
            task.abort();
        }
        Some(state)
    }
}

#[async_trait::async_trait]
impl Restart for Server {
    async fn restart(&self) -> Result<()> {
        let state = self.stop().await.context("server is not running")?;
        let state = (self.rebuild)(state).await?;
        let listener = tokio::net::TcpListener::bind(self.address).await?;
        self.serve(state, listener).await;
        Ok(())
    }
}

fn memory_restart() -> Rebuild {
    Box::new(|state| Box::pin(async move { Ok(state.restarted().await) }))
}

async fn run_suite(
    manifest: &Manifest,
    server: &Arc<Server>,
    restart: Option<Arc<dyn Restart>>,
) -> Result<Report> {
    topup_conformance::suite::run(SuiteConfig {
        settlement_url: server.url(),
        signer: signer_handle(DEV_SETTLEMENT_SEED)?,
        keyid: "settlement/v1".to_owned(),
        caps: CAPS,
        accounts: Accounts::default(),
        chain: ChainFixture::connect(manifest.clone()).await?,
        restart,
        resend_window: Duration::from_secs(30),
    })
    .await
}

async fn run_memory_variant(manifest: &Manifest, broken: BrokenVariant) -> Result<Report> {
    let state = ReferenceState::new(config(manifest.clone(), broken))?;
    let server = Server::start(state, router, memory_restart()).await?;
    let report = run_suite(manifest, &server, Some(server.clone())).await;
    server.stop().await;
    report
}

fn ids_with(report: &Report, status: TestStatus) -> BTreeSet<&str> {
    report
        .tests
        .iter()
        .filter(|test| test.status == status)
        .map(|test| test.id.as_str())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn conforming_reference_passes_every_case() -> Result<()> {
    let Some(anvil) = Anvil::start().await? else {
        return Ok(());
    };
    let manifest = anvil.prepare().await?;
    let report = run_memory_variant(&manifest, BrokenVariant::None).await?;
    assert!(report.passed, "{:#?}", report.tests);
    assert_eq!(report.summary.warnings, 0, "{:#?}", report.tests);
    assert_eq!(report.tests.len(), 15);
    assert_eq!(ids_with(&report, TestStatus::Pass).len(), 15);
    let get_original = report
        .tests
        .iter()
        .find(|test| test.id == "get_original")
        .context("get_original case")?;
    assert_eq!(get_original.evidence["payload_byte_equal"], json!(true));
    assert_eq!(
        get_original.evidence["payload_semantically_equal"],
        json!(true)
    );
    let chain_evidence = report
        .tests
        .iter()
        .find(|test| test.id == "chain_evidence")
        .context("chain_evidence case")?;
    assert_eq!(
        chain_evidence.evidence["not_finalized"]["first_answer"],
        json!("http 503"),
        "an unfinalized log is transient: no stored rejection"
    );
    assert_eq!(
        chain_evidence.evidence["not_finalized"]["resent_after_finality"],
        json!("accepted")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn each_broken_variant_fails_exactly_its_obligation() -> Result<()> {
    let Some(anvil) = Anvil::start().await? else {
        return Ok(());
    };
    let manifest = anvil.prepare().await?;
    for variant in BrokenVariant::BROKEN {
        let expected: BTreeSet<&str> = match variant {
            BrokenVariant::Signature => ["authentication", "idempotency_coverage"].into(),
            BrokenVariant::Idempotency => ["payload_mismatch"].into(),
            BrokenVariant::Retention => ["restart_retention"].into(),
            BrokenVariant::Concurrency => ["concurrency"].into(),
            BrokenVariant::Caps => ["per_deposit_cap", "per_period_cap"].into(),
            BrokenVariant::PeriodCapRace => ["per_period_cap"].into(),
            BrokenVariant::Evidence => ["chain_evidence"].into(),
            BrokenVariant::DepositIdentity => ["deposit_identity"].into(),
            BrokenVariant::None => bail!("None is not a broken variant"),
        };
        let report = run_memory_variant(&manifest, variant).await?;
        assert!(!report.passed, "{variant:?} unexpectedly passed");
        assert_eq!(
            ids_with(&report, TestStatus::Fail),
            expected,
            "{variant:?} failed the wrong cases: {:#?}",
            report.tests
        );
        assert!(ids_with(&report, TestStatus::Incomplete).is_empty());
        for test in &report.tests {
            if test.status == TestStatus::Fail {
                assert_eq!(test.obligation, variant.obligation(), "{}", test.id);
            }
        }
    }
    Ok(())
}

async fn hide_conformance_hooks(request: Request, next: Next) -> Response {
    if request.uri().path().contains("/_conformance/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(request).await
}

fn router_without_hooks(state: ReferenceState) -> Router {
    router(state).layer(middleware::from_fn(hide_conformance_hooks))
}

/// Answers the reference's transient `503` with a terminal rejection instead, as a product
/// which stores rejections for unfinalized logs would.
async fn reject_transient_reads(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    if response.status() != StatusCode::SERVICE_UNAVAILABLE {
        return response;
    }
    axum::Json(json!({"status": "rejected", "reason": "not_finalized"})).into_response()
}

fn router_rejecting_transient_reads(state: ReferenceState) -> Router {
    router(state).layer(middleware::from_fn(reject_transient_reads))
}

/// The suite does not enforce the transient-read rule, but must surface a violation.
#[tokio::test(flavor = "multi_thread")]
async fn stored_rejection_for_an_unfinalized_log_is_reported_as_a_warning() -> Result<()> {
    let Some(anvil) = Anvil::start().await? else {
        return Ok(());
    };
    let manifest = anvil.prepare().await?;
    let state = ReferenceState::new(config(manifest.clone(), BrokenVariant::None))?;
    let server = Server::start(state, router_rejecting_transient_reads, memory_restart()).await?;
    let report = run_suite(&manifest, &server, Some(server.clone())).await;
    server.stop().await;
    let report = report?;
    assert!(report.passed, "{:#?}", report.tests);
    assert_eq!(report.summary.warnings, 1);
    let chain_evidence = report
        .tests
        .iter()
        .find(|test| test.id == "chain_evidence")
        .context("chain_evidence case")?;
    assert_eq!(chain_evidence.warnings.len(), 1);
    assert_eq!(
        chain_evidence.evidence["not_finalized"]["first_answer"],
        json!("rejected: not_finalized")
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_ledger_hook_and_restart_are_incomplete_never_pass() -> Result<()> {
    let Some(anvil) = Anvil::start().await? else {
        return Ok(());
    };
    let manifest = anvil.prepare().await?;
    let state = ReferenceState::new(config(manifest.clone(), BrokenVariant::None))?;
    let server = Server::start(state, router_without_hooks, memory_restart()).await?;
    let report = run_suite(&manifest, &server, None).await;
    server.stop().await;
    let report = report?;
    assert!(!report.passed);
    assert_eq!(
        ids_with(&report, TestStatus::Incomplete),
        [
            "accepted",
            "business_refusal",
            "chain_evidence",
            "concurrency",
            "per_deposit_cap",
            "per_period_cap",
            "processing",
            "replay",
            "restart_retention",
        ]
        .into(),
        "{:#?}",
        report.tests
    );
    assert!(
        ids_with(&report, TestStatus::Fail).is_empty(),
        "{:#?}",
        report.tests
    );
    Ok(())
}

#[cfg(feature = "postgres")]
#[tokio::test(flavor = "multi_thread")]
async fn postgres_reference_passes_across_a_real_reconnect() -> Result<()> {
    let Ok(admin_url) = std::env::var("MIGRATE_DATABASE_URL") else {
        return skip("MIGRATE_DATABASE_URL is not set");
    };
    let Some(anvil) = Anvil::start().await? else {
        return Ok(());
    };
    let manifest = anvil.prepare().await?;
    let admin = sqlx::PgPool::connect(&admin_url).await?;
    let database = format!("conformance_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE DATABASE {database}")))
        .execute(&admin)
        .await?;
    let mut url = url::Url::parse(&admin_url)?;
    url.set_path(&database);
    let url = url.to_string();

    let result = async {
        let pool = sqlx::PgPool::connect(&url).await?;
        let state =
            ReferenceState::new_postgres(config(manifest.clone(), BrokenVariant::None), pool)
                .await?;
        let reconnect: Rebuild = {
            let url = url.clone();
            let manifest = manifest.clone();
            Box::new(move |_old| {
                let url = url.clone();
                let manifest = manifest.clone();
                Box::pin(async move {
                    let pool = sqlx::PgPool::connect(&url).await?;
                    ReferenceState::new_postgres(config(manifest, BrokenVariant::None), pool).await
                })
            })
        };
        let server = Server::start(state, router, reconnect).await?;
        let report = run_suite(&manifest, &server, Some(server.clone())).await;
        server.stop().await;
        report
    }
    .await;
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE {database} WITH (FORCE)"
    )))
    .execute(&admin)
    .await?;
    let report = result?;
    assert!(report.passed, "{:#?}", report.tests);
    Ok(())
}

/// A manifest whose RPC URL refuses connections.
fn unreachable_chain_manifest() -> Manifest {
    Manifest {
        version: 1,
        chain_id: 31_337,
        rpc_url: "http://127.0.0.1:9".to_owned(),
        product_slug: "conformance".to_owned(),
        route: "conformance".to_owned(),
        route_version: 1,
        address_version: 1,
        factory: alloy_primitives::Address::repeat_byte(1),
        implementation: alloy_primitives::Address::repeat_byte(2),
        asset_contract: alloy_primitives::Address::repeat_byte(3),
        unapproved_asset_contract: alloy_primitives::Address::repeat_byte(4),
        funder: alloy_primitives::Address::repeat_byte(5),
    }
}

/// A chain read that fails transiently must answer `503` and store nothing, so the service's
/// unchanged resend can still be credited once the product's RPC recovers.
#[tokio::test]
async fn transient_chain_read_failure_answers_503_without_a_record() -> Result<()> {
    let manifest = unreachable_chain_manifest();
    let account = "conformance-accepted";
    let evidence = Evidence {
        chain_id: manifest.chain_id,
        asset_contract: format!("{:#x}", manifest.asset_contract),
        route: manifest.route.clone(),
        route_version: manifest.route_version,
        tx_hash: format!("{:#x}", alloy_primitives::B256::repeat_byte(9)),
        log_index: 0,
        to: format!("{:#x}", manifest.forwarder(account)),
        amount_atomic: "1000000".to_owned(),
        price_scaled: "100000000".to_owned(),
        price_scale: 8,
        valuation_at: chrono::Utc::now().to_rfc3339(),
        lock_ref: None,
    };
    let key = key_from_evidence(&evidence)?;
    let request = SettlementRequest {
        idempotency_key: key.clone(),
        payload: json!({
            "version": 1,
            "idempotency_key": key,
            "account_id": account,
            "unit": "USD",
            "amount_minor": "100",
            "source": "crypto_deposit",
            "evidence": evidence,
        }),
    };
    let state = ReferenceState::new(config(manifest, BrokenVariant::None))?;
    let server = Server::start(state, router, memory_restart()).await?;
    let client = SettlementClient::new(
        &server.url(),
        signer_handle(DEV_SETTLEMENT_SEED)?,
        Duration::from_secs(10),
    )?;
    let answer = client.post(&request).await;
    let stored = client.get_by_key(&request.idempotency_key).await;
    server.stop().await;
    assert!(
        matches!(answer?, SettlementAnswer::Unknown { status: 503, .. }),
        "a transient chain-read failure must answer 503"
    );
    assert_eq!(stored?, None, "a transient failure must not store a record");
    Ok(())
}

/// The reference authenticates with the service's shared verifier, so any label, parameter
/// order, and the optional `alg` parameter are accepted.
#[tokio::test]
async fn reference_accepts_structured_field_signature_variants() -> Result<()> {
    let manifest = unreachable_chain_manifest();
    let state = ReferenceState::new(config(manifest, BrokenVariant::None))?;
    let server = Server::start(state, router, memory_restart()).await?;
    let client = reqwest::Client::new();
    let key = SigningKey::from_bytes(&DEV_SETTLEMENT_SEED);
    let created = chrono::Utc::now().timestamp();
    let components = "(\"@method\" \"@target-uri\" \"content-digest\" \"idempotency-key\")";
    let mut statuses = Vec::new();
    for (index, (label, parameters)) in [
        (
            "sig1",
            format!(";created={created};keyid=\"settlement/v1\""),
        ),
        (
            "service",
            format!(";keyid=\"settlement/v1\";created={created}"),
        ),
        (
            "x",
            format!(";alg=\"ed25519\";created={created};keyid=\"settlement/v1\""),
        ),
        (
            "sig1",
            format!(";created={created};keyid=\"settlement/v2\""),
        ),
        (
            "sig1",
            format!(";created={created};keyid=\"settlement/v1\";alg=\"hmac-sha256\""),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let idempotency_key = format!("\"deposit:variant-{index}\"");
        let body = serde_json::to_vec(&json!({
            "idempotency_key": format!("deposit:variant-{index}"),
            "account_id": "conformance-refused",
        }))?;
        let digest = format!("sha-256=:{}:", STANDARD.encode(Sha256::digest(&body)));
        let params = format!("{components}{parameters}");
        let base = format!(
            "\"@method\": POST\n\"@target-uri\": {}\n\"content-digest\": {digest}\n\
             \"idempotency-key\": {idempotency_key}\n\"@signature-params\": {params}",
            server.url()
        );
        let signature = STANDARD.encode(key.sign(base.as_bytes()).to_bytes());
        let response = client
            .post(server.url())
            .header("content-digest", digest)
            .header("idempotency-key", idempotency_key)
            .header("signature-input", format!("{label}={params}"))
            .header("signature", format!("{label}=:{signature}:"))
            .body(body)
            .send()
            .await?;
        statuses.push(response.status().as_u16());
    }
    server.stop().await;
    assert_eq!(statuses, [200, 200, 200, 401, 401]);
    Ok(())
}

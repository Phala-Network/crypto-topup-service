//! Command-line entry point for the Phala Pay service.

mod route;

use std::collections::HashMap;
use std::future::{Future, IntoFuture as _};
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, anyhow, bail, ensure};
use chrono::{DateTime, Utc};
use clap::{Args, Parser, Subcommand};
use serde_json::json;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use topup::pump::{AgeAlertConfig, AgeAlerter, Pump, PumpConfig, StepSet};
use topup::routes::RouteSet;
use topup::steps::confirm::ConfirmStep;
use topup::steps::screen::ScreenStep;
#[cfg(feature = "dev-signer")]
use topup_adapters::attestation::report_data;
use topup_adapters::attestation::{AttestedOperator, DstackAttestor};
#[cfg(feature = "dev-signer")]
use topup_adapters::signer::DevSigner;
use topup_adapters::signer::actor::SignerHandle;
use topup_adapters::signer::dstack::DstackSigner;
#[cfg(feature = "dev-signer")]
use topup_core::SecretKey32;
use topup_core::{
    DB_APP_KEY_DOMAIN, DB_OWNER_KEY_DOMAIN, SETTLEMENT_KEY_DOMAIN, Signer as _, operator_key_domain,
};
use tracing_subscriber::util::SubscriberInitExt as _;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "topup", version, about = "Phala Pay service")]
struct Cli {
    #[command(subcommand)]
    command: TopupCommand,
}

#[derive(Subcommand)]
enum TopupCommand {
    Run(RunArgs),
    Migrate,
    Route {
        #[command(subcommand)]
        command: RouteCommand,
    },
    Outbox {
        #[command(subcommand)]
        command: OutboxCommand,
    },
    /// Run one reconciliation pass and exit.
    Reconcile(ReconcileArgs),
    Attest(AttestArgs),
    /// Derive the backup key and database credentials from dstack into a shared tmpfs.
    Keys(KeysArgs),
    Heartbeat(HeartbeatArgs),
    /// Exit zero only when the local API answers `GET /healthz` with 200, for container health.
    Healthcheck(HealthcheckArgs),
    /// Validate a restored database and run post-restore reconciliation.
    ///
    /// Run only while the service, heartbeat, and backup processes are stopped: the post-restore
    /// round holds the lease-owner lock and may repair the restored ledger; it asks the product
    /// nothing. It does nothing while TOPUP_RESTORE_FROM_BACKUP=off, and writes its report to
    /// TOPUP_RESTORE_REPORT_FILE when that is set.
    RestoreCheck(RestoreCheckArgs),
}

#[derive(Args)]
struct RunArgs {
    /// API socket address; defaults to the deployment port on all interfaces.
    #[arg(long, default_value = "0.0.0.0:8080")]
    bind: std::net::SocketAddr,
    /// Validated route file; repeat for every enabled route version.
    #[arg(long = "route", required = true, value_name = "FILE")]
    routes: Vec<PathBuf>,
    /// Delay before retrying an expected wait outcome.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
    wait_interval_s: u64,
    /// Delay between scanner polls of each chain.
    #[arg(long, default_value_t = 15, value_parser = clap::value_parser!(u64).range(1..))]
    scanner_poll_interval_s: u64,
}

/// Concurrent deposit pumps in one service process.
const PUMPS: usize = 1;
/// Maximum duration of one step; shorter than the five-minute lease.
const STEP_TIMEOUT: Duration = Duration::from_secs(240);
/// Interval between deposit state-age scans.
const AGE_ALERT_INTERVAL: Duration = Duration::from_secs(60);
/// Interval between full reconciliation passes.
const RECONCILIATION_INTERVAL: Duration = Duration::from_secs(10 * 60);

#[derive(Args)]
struct ReconcileArgs {
    /// Validated route file; repeat for every enabled route version.
    #[arg(long = "route", required = true, value_name = "FILE")]
    routes: Vec<PathBuf>,
}

#[derive(Args)]
struct AttestArgs {
    #[arg(long, value_name = "HEX")]
    nonce: String,
    /// Operator key derivation version to report, as set by the chain's `operator_key_version`.
    #[arg(long, value_name = "N", default_value_t = NonZeroU32::MIN)]
    operator_key_version: NonZeroU32,
    /// Route file whose chain operator is listed in `operators` and bound into the report data,
    /// as `GET /v1/attestation` does; repeat for every enabled route version.
    #[arg(long = "route", value_name = "FILE")]
    routes: Vec<PathBuf>,
    #[cfg(feature = "dev-signer")]
    #[arg(long, help = "Use development keys without a hardware quote")]
    dev: bool,
}

#[derive(Args)]
struct KeysArgs {
    /// Directory (tmpfs) for backup.key, which WAL-G reads.
    #[arg(long, value_name = "DIR")]
    backup_dir: PathBuf,
    /// Directory (tmpfs) for the owner login's postgres.password and postgres.pgpass.
    #[arg(long, value_name = "DIR")]
    owner_dir: PathBuf,
    /// Directory (tmpfs) for the application login's topup_service.pgpass.
    #[arg(long, value_name = "DIR")]
    app_dir: PathBuf,
    /// Keep the process alive so the shared tmpfs remains mounted.
    #[arg(long, conflicts_with = "check")]
    hold: bool,
    /// Check only that the files were atomically published with safe metadata.
    #[arg(long, conflicts_with = "hold")]
    check: bool,
}

#[derive(Args)]
struct RestoreCheckArgs {
    /// Last source heartbeat committed before the recorded failure point. Omitted at boot after a
    /// restore from backup: the report is then `unanchored` and the operator compares its
    /// `restored_heartbeat_at` with their own external anchor.
    #[arg(long, value_name = "RFC3339")]
    expected_heartbeat_at: Option<DateTime<Utc>>,
    /// Source WAL location from the same heartbeat log line. Omit only as a declared incident
    /// exception; RPO is then proven by the heartbeat timestamp alone and flagged in the report.
    #[arg(long, value_name = "PG_LSN", requires = "expected_heartbeat_at")]
    expected_lsn: Option<String>,
    /// Validated route file; repeat for every enabled route version.
    #[arg(long = "route", required = true, value_name = "FILE")]
    routes: Vec<PathBuf>,
}

#[derive(Args)]
struct HeartbeatArgs {
    /// Seconds between persisted RPO heartbeats.
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..))]
    interval_s: u64,
}

#[derive(Args)]
struct HealthcheckArgs {
    /// Health endpoint of the API listener in this container.
    #[arg(long, default_value = "http://127.0.0.1:8080/healthz")]
    url: reqwest::Url,
}

#[derive(Subcommand)]
enum OutboxCommand {
    Replay {
        /// Replay one event by its `webhook-id`: `evt_…`, or the UUID of an older event.
        #[arg(long, conflicts_with = "since", required_unless_present = "since", value_parser = parse_event_id)]
        id: Option<Uuid>,
        /// Replay events created at or after this RFC 3339 timestamp.
        #[arg(long, conflicts_with = "id", required_unless_present = "id")]
        since: Option<String>,
        /// Redeliver events that were already marked delivered.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum RouteCommand {
    Validate {
        /// Permit zero factory and treasury placeholders in deployment templates.
        #[arg(long)]
        template: bool,
        file: PathBuf,
    },
    /// Print the resolved route as JSON (also a valid route file): every code default written out.
    Show {
        /// Permit zero factory and treasury placeholders in deployment templates.
        #[arg(long)]
        template: bool,
        file: PathBuf,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    // Before the subscriber, which adds the Sentry layer only when reporting is enabled. The
    // guard flushes queued events when `main` returns.
    let reporting = match topup::observability::init_reporting() {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = topup::observability::log_subscriber(std::io::stdout).try_init() {
        eprintln!("failed to initialize tracing: {error}");
        return ExitCode::FAILURE;
    }

    let cli = Cli::parse();

    let result = match cli.command {
        TopupCommand::Run(args) => {
            // Only here: other commands print their result on stdout, which the log shares.
            tracing::info!(
                sentry_enabled = reporting.is_some(),
                "error reporting configured"
            );
            run(&args).await
        }
        TopupCommand::Migrate => migrate().await,
        TopupCommand::Route {
            command: RouteCommand::Validate { template, file },
        } => return validate_route(&file, template),
        TopupCommand::Route {
            command: RouteCommand::Show { template, file },
        } => return show_route(&file, template),
        TopupCommand::Outbox {
            command: OutboxCommand::Replay { id, since, force },
        } => replay_outbox(id, since.as_deref(), force).await,
        TopupCommand::Reconcile(args) => reconcile(&args).await,
        TopupCommand::Attest(args) => {
            return match attest(&args).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(message) => {
                    eprintln!("{message}");
                    ExitCode::FAILURE
                }
            };
        }
        TopupCommand::Keys(args) => keys(&args).await,
        TopupCommand::Heartbeat(args) => heartbeat(&args).await,
        TopupCommand::Healthcheck(args) => return healthcheck(&args).await,
        TopupCommand::RestoreCheck(args) => restore_check(&args).await,
    };

    result.unwrap_or_else(|error| {
        // The outermost context is the message; the error it wraps, if any, the `error` field.
        match error.source() {
            Some(cause) => tracing::error!(error = %cause, "{error}"),
            None => tracing::error!("{error}"),
        }
        ExitCode::FAILURE
    })
}

async fn keys(args: &KeysArgs) -> anyhow::Result<ExitCode> {
    if args.check {
        let checked = topup::keys::check(&args.backup_dir, &args.owner_dir, &args.app_dir);
        return Ok(if checked.is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    let signer = DstackSigner::new();
    let (Ok(backup), Ok(owner), Ok(app)) = tokio::join!(
        signer.derive_backup_key(),
        signer.derive_secret(DB_OWNER_KEY_DOMAIN),
        signer.derive_secret(DB_APP_KEY_DOMAIN),
    ) else {
        bail!("failed to derive the backup key and database credentials");
    };
    let backup_key = args.backup_dir.join(topup::keys::BACKUP_KEY_FILE);
    topup::keys::write_backup_key(&backup_key, &backup)
        .with_context(|| format!("failed to write backup key file {}", backup_key.display()))?;
    topup::keys::write_database_credentials(&args.owner_dir, &args.app_dir, &owner, &app)
        .context("failed to write database credential files")?;
    tracing::info!("key files are ready");
    if args.hold {
        wait_for_shutdown_signal()
            .await
            .context("failed to listen for key holder shutdown signal")?;
        tracing::info!("key holder stopped");
    }
    Ok(ExitCode::SUCCESS)
}

/// `TOPUP_SERVICE_ENABLED` of a replacement CVM that boots for a restore (`deploy/RESTORE.md`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum ServiceMode {
    On,
    /// `topup run` serves only reads; nothing else runs.
    ReadOnly,
    Off,
}

impl ServiceMode {
    fn from_env() -> anyhow::Result<Self> {
        match std::env::var("TOPUP_SERVICE_ENABLED").as_deref() {
            Err(_) | Ok("on") => Ok(Self::On),
            Ok("read-only") => Ok(Self::ReadOnly),
            Ok("off") => Ok(Self::Off),
            Ok(_) => bail!("TOPUP_SERVICE_ENABLED must be on, read-only, or off"),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::ReadOnly => "read-only",
            Self::Off => "off",
        }
    }
}

/// Refuses a command in any `TOPUP_SERVICE_ENABLED` mode it does not run in.
fn service_enabled(command: &str, allowed: &[ServiceMode]) -> anyhow::Result<ServiceMode> {
    let mode = ServiceMode::from_env()?;
    ensure!(
        allowed.contains(&mode),
        "{command} is disabled while TOPUP_SERVICE_ENABLED={}",
        mode.name()
    );
    Ok(mode)
}

async fn heartbeat(args: &HeartbeatArgs) -> anyhow::Result<ExitCode> {
    service_enabled("heartbeat", &[ServiceMode::On]).context("heartbeat refused to start")?;
    let pool = connect("DATABASE_URL", "heartbeat", 1)
        .await
        .context("failed to connect to database")?;
    let mut interval = tokio::time::interval(Duration::from_secs(args.interval_s));
    let shutdown = wait_for_shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let record = topup::heartbeat::record(&pool)
                    .await
                    .context("failed to record restore heartbeat")?;
                tracing::info!(
                    heartbeat_id = record.id,
                    // RFC 3339, so the value can be passed to --expected-heartbeat-at as is.
                    recorded_at = %record
                        .recorded_at
                        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
                    rpo_seconds = record.rpo_seconds,
                    wal_lsn = %record.wal_lsn,
                    "restore heartbeat recorded"
                );
            }
            signal = &mut shutdown => {
                signal.context("failed to listen for heartbeat shutdown signal")?;
                tracing::info!("heartbeat stopped");
                return Ok(ExitCode::SUCCESS);
            }
        }
    }
}

// The exit status is the health signal; failures are logged below error level so a probe every
// 30 s during an outage does not duplicate the service's own error reports.
async fn healthcheck(args: &HealthcheckArgs) -> ExitCode {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!(%error, "failed to build the health check client");
            return ExitCode::FAILURE;
        }
    };
    match client.get(args.url.clone()).send().await {
        Ok(response) if response.status() == reqwest::StatusCode::OK => ExitCode::SUCCESS,
        Ok(response) => {
            tracing::warn!(status = %response.status(), "health check failed");
            ExitCode::FAILURE
        }
        Err(error) => {
            tracing::warn!(%error, "health check request failed");
            ExitCode::FAILURE
        }
    }
}

async fn restore_check(args: &RestoreCheckArgs) -> anyhow::Result<ExitCode> {
    // In the compose, restore-check starts with every boot; it runs only after a restore.
    match std::env::var("TOPUP_RESTORE_FROM_BACKUP").as_deref() {
        Err(_) | Ok("on") => {}
        Ok("off") => {
            tracing::info!("restore-check skipped: TOPUP_RESTORE_FROM_BACKUP=off");
            return Ok(ExitCode::SUCCESS);
        }
        Ok(_) => bail!("TOPUP_RESTORE_FROM_BACKUP must be on or off"),
    }
    let result = run_restore_check(args).await;
    let encoded = match &result {
        Ok(report) => {
            serde_json::to_value(report).context("failed to encode restore check report")?
        }
        Err(error) => json!({ "status": "failed", "failures": [error.to_string()] }),
    };
    if let Some(path) = std::env::var_os("TOPUP_RESTORE_REPORT_FILE") {
        let path = PathBuf::from(path);
        write_restore_report(&path, &encoded)
            .with_context(|| format!("failed to write restore check report {}", path.display()))?;
    }
    let report = result?;
    println!("{encoded}");
    Ok(if report.status == "ok" {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Runs the post-restore gate; each error's outermost context is the report's failure.
async fn run_restore_check(
    args: &RestoreCheckArgs,
) -> anyhow::Result<topup::restore::RestoreReport> {
    let routes = load_routes(&args.routes).context("failed to load route configuration")?;
    // The post-restore gate reads and repairs with owner credentials, never the service login.
    let pool = connect("MIGRATE_DATABASE_URL", "restore-check", 4)
        .await
        .context("failed to connect to the restored database")?;
    let reconciler = topup::reconciler::Reconciler::from_routes(pool.clone(), Arc::new(routes))
        .context("failed to configure the post-restore reconciler")?;
    let expectations = topup::restore::RestoreExpectations {
        expected_heartbeat_at: args.expected_heartbeat_at,
        expected_lsn: args.expected_lsn.clone(),
    };
    topup::restore::check(&pool, &expectations, &reconciler)
        .await
        .map_err(anyhow::Error::msg)
}

/// Publishes the report atomically, so a reader never sees a partial file.
fn write_restore_report(path: &Path, report: &serde_json::Value) -> std::io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".tmp.{}", std::process::id()));
    let temporary = PathBuf::from(temporary);
    std::fs::write(&temporary, report.to_string())?;
    std::fs::rename(&temporary, path)
}

async fn attest(args: &AttestArgs) -> Result<(), &'static str> {
    let nonce = parse_nonce(&args.nonce)?;
    let version = args.operator_key_version;
    let operator_keys = if args.routes.is_empty() {
        Vec::new()
    } else {
        load_routes(&args.routes)
            .and_then(|routes| routes.operator_keys().map_err(anyhow::Error::msg))
            .map_err(|error| {
                tracing::error!(%error, "invalid route configuration");
                "failed to load the route configuration"
            })?
    };

    #[cfg(feature = "dev-signer")]
    if args.dev {
        let seed = SecretKey32::new([1; 32]);
        let signer = DevSigner::derive(&seed, version);
        let public_key = signer
            .settlement_public_key()
            .await
            .map_err(|_| "development settlement key is invalid")?;
        let operator = signer
            .operator_address()
            .await
            .map_err(|_| "development operator key is invalid")?;
        let mut operators = Vec::with_capacity(operator_keys.len());
        for key in &operator_keys {
            operators.push(AttestedOperator {
                chain_id: key.chain_id,
                key_version: key.key_version,
                address: DevSigner::derive(&seed, key.key_version)
                    .operator_address()
                    .await
                    .map_err(|_| "development operator key is invalid")?,
            });
        }
        return print_attestation(
            &public_key.0,
            &operators,
            &report_data(&nonce, &public_key, &operators),
            &[],
            &[],
            &[],
            version,
            operator,
        );
    }

    let evidence = DstackAttestor::new()
        .attest(&nonce, &operator_keys)
        .await
        .map_err(|_| "failed to collect dstack attestation")?;
    let operator = DstackSigner::new()
        .with_operator_key_version(version)
        .operator_address()
        .await
        .map_err(|_| "failed to derive the dstack operator key")?;
    print_attestation(
        &evidence.settlement_public_key.0,
        &evidence.operators,
        &evidence.report_data,
        &evidence.quote,
        &evidence.info.app_id,
        &evidence.info.compose_hash,
        version,
        operator,
    )
}

fn parse_nonce(value: &str) -> Result<Vec<u8>, &'static str> {
    if value.is_empty() {
        return Err("nonce must be non-empty hexadecimal");
    }
    if value.len() > 64 {
        return Err("nonce must be at most 32 bytes (64 hexadecimal characters)");
    }
    hex::decode(value).map_err(|_| "nonce must be valid hexadecimal")
}

#[allow(clippy::too_many_arguments)]
fn print_attestation(
    settlement_public_key: &[u8; 32],
    operators: &[AttestedOperator],
    report_data: &[u8; 32],
    quote: &[u8],
    app_id: &[u8],
    compose_hash: &[u8],
    operator_key_version: NonZeroU32,
    operator: alloy_primitives::Address,
) -> Result<(), &'static str> {
    let operators = operators
        .iter()
        .map(topup::api::models::OperatorIdentity::from)
        .collect::<Vec<_>>();
    let output = json!({
        "keyid": SETTLEMENT_KEY_DOMAIN,
        "settlement_pubkey": hex::encode(settlement_public_key),
        "operators": operators,
        "report_data": hex::encode(report_data),
        "quote": hex::encode(quote),
        "app_id": if app_id.is_empty() { String::new() } else { format!("0x{}", hex::encode(app_id)) },
        "compose_hash": if compose_hash.is_empty() { String::new() } else { format!("0x{}", hex::encode(compose_hash)) },
        "operator_keyid": operator_key_domain(operator_key_version),
        "operator_address": format!("{operator:#x}"),
    });
    let encoded = serde_json::to_string(&output).map_err(|_| "failed to encode attestation")?;
    println!("{encoded}");
    Ok(())
}

async fn run(args: &RunArgs) -> anyhow::Result<ExitCode> {
    let routes = load_routes(&args.routes).context("failed to load route configuration")?;
    let age_config =
        AgeAlertConfig::from_routes(routes.routes()).context("invalid age alert configuration")?;
    let rate_lock_quotes: Arc<dyn topup::locks::QuoteProvider> = Arc::new(
        topup::locks::ConfiguredQuoteProvider::from_routes(routes.routes())
            .map_err(anyhow::Error::msg)
            .context("invalid rate-lock pricing configuration")?,
    );
    let pump_config = PumpConfig {
        step_timeout: STEP_TIMEOUT,
        wait_interval: Duration::from_secs(args.wait_interval_s),
        ..PumpConfig::default()
    };
    // Checked before the on-chain contract check; `connect` reads it again below.
    let mode = service_enabled("run", &[ServiceMode::On, ServiceMode::ReadOnly])
        .and_then(|mode| required_env("DATABASE_URL").map(|_| mode))
        .context("missing runtime configuration")?;
    let admin_kid = required_env("TOPUP_ADMIN_KID").context("missing runtime configuration")?;
    let admin_public_key =
        required_env("TOPUP_ADMIN_PUBLIC_KEY").context("missing runtime configuration")?;
    let admin_key = topup::api::VerificationKey::from_base64(admin_kid, &admin_public_key)
        .map_err(anyhow::Error::msg)
        .context("invalid administrative verification key")?;
    let public_origin = required_env("TOPUP_PUBLIC_ORIGIN")
        .and_then(|value| Ok(topup::api::PublicOrigin::parse(&value)?))
        .context("invalid TOPUP_PUBLIC_ORIGIN")?;
    if mode == ServiceMode::ReadOnly {
        let state = topup::api::AppState {
            pool: connect("DATABASE_URL", "run", 4)
                .await
                .context("failed to connect to database")?,
            routes: Arc::new(routes),
            admin_key,
            public_origin,
            attestor: Arc::new(DstackAttestor::new()),
            rate_lock_quotes,
            client_reads: Arc::default(),
        };
        return serve_read_only(args.bind, state).await;
    }
    // Architecture §4: before the database is touched, every provider must show the route's
    // factory, implementation, and treasury, so nothing issues addresses or moves funds otherwise.
    topup::contracts::verify_routes(&routes)
        .await
        .map_err(anyhow::Error::msg)
        .context("on-chain contract check failed")?;
    let routes = Arc::new(routes);
    let scanner_count = routes.chain_ids().count();
    let route_count = routes.routes().len();
    let connection_count = u32::try_from(PUMPS)
        .ok()
        .zip(u32::try_from(scanner_count).ok())
        .zip(u32::try_from(route_count).ok())
        .and_then(|((pumps, scanners), routes)| pumps.checked_add(scanners)?.checked_add(routes))
        // The outbox renders an event's object on a second connection while it holds the claim.
        .and_then(|count| count.checked_add(4))
        .context("route count is too large")?;
    let pool = connect("DATABASE_URL", "run", connection_count)
        .await
        .context("failed to connect to database")?;
    let Some(lease_owner) = wait_for_lease_owner_lock(&pool).await? else {
        return Ok(ExitCode::SUCCESS);
    };
    let signer = spawn_signer(None).context("failed to start signer actor")?;
    let delivery_worker = topup::outbox::DeliveryWorker::new(
        pool.clone(),
        Arc::clone(&routes),
        Arc::new(signer.clone()),
        topup::outbox::DeliveryConfig::default(),
    )
    .context("failed to configure webhook delivery")?;
    let frozen = topup::reconciler::frozen_chains(&pool, &routes)
        .await
        .context("failed to load reconciliation blocks")?;
    for chain_id in frozen {
        tracing::error!(
            chain_id,
            "reconciliation froze configured chain; its scanner, pumps, flusher, and \
             address issuance stay paused until the block is removed"
        );
    }
    let reconciler = Arc::new(
        topup::reconciler::Reconciler::from_routes(pool.clone(), Arc::clone(&routes))
            .context("failed to configure reconciler")?,
    );
    let flusher_tasks =
        topup::flusher::runtime::configure_tasks(pool.clone(), &routes, |version| {
            spawn_signer(Some(version))
        })
        .map_err(anyhow::Error::msg)
        .context("invalid flusher runtime configuration")?;
    let listener = tokio::net::TcpListener::bind(args.bind)
        .await
        .with_context(|| format!("failed to bind API listener on {}", args.bind))?;
    let confirm_step = ConfirmStep::from_routes(pool.clone(), &routes)
        .context("invalid confirm-step configuration")?;
    let screen_step = ScreenStep::from_routes(pool.clone(), &routes)
        .context("failed to configure screening step")?;
    let steps = Arc::new(StepSet::new(
        Box::new(confirm_step),
        Box::new(screen_step),
        Box::new(topup::flusher::SweepStep),
    ));
    let pump = Pump::new(pool.clone(), Arc::<StepSet>::clone(&steps), pump_config)
        .context("invalid pump configuration")?;
    let refund_config = topup::refunds::RefundConfirmationConfig::default();
    let refund_reader = topup::refunds::EvmRefundChainReader::from_routes(&routes)
        .context("failed to configure refund confirmation chain reader")?;
    let refund_worker = topup::refunds::RefundConfirmationWorker::new(
        pool.clone(),
        refund_reader,
        routes.routes(),
        refund_config,
    )
    .context("failed to configure refund confirmation worker")?;
    let mut tasks = ServiceTasks::new();
    let state = topup::api::AppState {
        pool: pool.clone(),
        routes: Arc::clone(&routes),
        admin_key,
        public_origin,
        attestor: Arc::new(DstackAttestor::new()),
        rate_lock_quotes,
        client_reads: Arc::default(),
    };
    let (application, _) = topup::api::router(state);
    tasks.spawn("API server", |cancellation| {
        axum::serve(listener, application)
            .with_graceful_shutdown(cancellation.cancelled_owned())
            .into_future()
    });
    tracing::info!(bind = %args.bind, "API listening");

    let scanner_pool = pool.clone();
    let scanner_routes = Arc::clone(&routes);
    let scanner_poll_interval = Duration::from_secs(args.scanner_poll_interval_s);
    tasks.spawn("scanner", |cancellation| async move {
        topup::scanner::run(
            scanner_pool,
            &scanner_routes,
            scanner_poll_interval,
            cancellation,
        )
        .await
    });
    tasks.spawn("refund confirmation worker", |cancellation| async move {
        refund_worker.run(cancellation).await;
    });
    for worker in 0..PUMPS {
        let worker_pump = pump.clone();
        tasks.spawn(
            format!("deposit pump {worker}"),
            |cancellation| async move {
                tracing::info!(worker, "deposit pump started");
                worker_pump
                    .run_with_instance(worker.to_string(), cancellation)
                    .await;
            },
        );
    }
    let age_alerter = AgeAlerter::new(pool.clone(), age_config, AGE_ALERT_INTERVAL);
    tasks.spawn("age alerter", |cancellation| async move {
        age_alerter.run(cancellation).await;
    });
    tasks.spawn("backup monitor", topup::observability::monitor_backup);
    let expiry_worker = topup::locks::ExpiryWorker::new(pool.clone(), Duration::from_secs(5));
    tasks.spawn("rate-lock expiry worker", |cancellation| async move {
        expiry_worker.run(cancellation).await;
    });
    tasks.spawn("webhook delivery worker", |cancellation| async move {
        delivery_worker.run(cancellation).await;
    });
    for task in flusher_tasks {
        tasks.spawn(format!("flusher {}", task.instance()), |cancellation| {
            task.run(cancellation)
        });
    }
    tasks.spawn("reconciler", |cancellation| async move {
        reconciler
            .run_loop(RECONCILIATION_INTERVAL, cancellation)
            .await;
    });

    tracing::info!(
        pumps = PUMPS,
        scanners = scanner_count,
        "topup service started"
    );
    // The watch cancels every task when the lock connection fails; it is reported below.
    let mut lease_owner_task = tokio::spawn(lease_owner.watch(LEASE_OWNER_PING, tasks.token()));
    let mut clean_shutdown = true;
    let mut lease_owner_finished = None;
    tokio::select! {
        signal = wait_for_shutdown_signal() => {
            if let Err(error) = signal {
                tracing::error!(%error, "failed to listen for shutdown signal");
                clean_shutdown = false;
            }
        }
        () = tasks.first_exit() => clean_shutdown = false,
        result = &mut lease_owner_task => {
            lease_owner_finished = Some(result);
        }
    }
    tracing::info!("shutdown requested; finishing in-flight service work");
    if !tasks.shutdown().await {
        clean_shutdown = false;
    }
    // Release the lease-owner lock only after every lease-holding task has stopped.
    let lease_owner = match lease_owner_finished {
        Some(result) => result,
        None => lease_owner_task.await,
    };
    match lease_owner {
        Ok(Ok(lock)) => {
            if let Err(error) = lock.release().await {
                tracing::error!(%error, "failed to release the lease-owner lock");
                clean_shutdown = false;
            }
        }
        Ok(Err(error)) => {
            tracing::error!(
                %error,
                "lease-owner lock connection failed; deposit processing was stopped"
            );
            clean_shutdown = false;
        }
        Err(error) => {
            tracing::error!(%error, "lease-owner lock task failed");
            clean_shutdown = false;
        }
    }
    pool.close().await;
    tracing::info!("topup service stopped");

    Ok(if clean_shutdown {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Serves only the read API of a database restored from backup (`deploy/RESTORE.md`), so the
/// operator can verify it through the product-signed lookups: no lease-owner lock, scanner, pump,
/// flusher, webhook delivery, reconciler, or signer runs, and every non-GET request is refused.
async fn serve_read_only(
    bind: std::net::SocketAddr,
    state: topup::api::AppState,
) -> anyhow::Result<ExitCode> {
    let pool = state.pool.clone();
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("failed to bind API listener on {bind}"))?;
    let report = std::env::var_os("TOPUP_RESTORE_REPORT_FILE").map(PathBuf::from);
    let application = topup::api::read_only_router(state, report);
    tracing::warn!(%bind, "API listening read-only while TOPUP_SERVICE_ENABLED=read-only");
    let served = axum::serve(listener, application)
        .with_graceful_shutdown(async {
            if let Err(error) = wait_for_shutdown_signal().await {
                tracing::error!(%error, "failed to listen for shutdown signal");
            }
        })
        .await;
    pool.close().await;
    served.context("read-only API failed")?;
    tracing::info!("read-only API stopped");
    Ok(ExitCode::SUCCESS)
}

async fn reconcile(args: &ReconcileArgs) -> anyhow::Result<ExitCode> {
    let routes = load_routes(&args.routes).context("failed to load route configuration")?;
    let pool = connect("DATABASE_URL", "reconcile", 4)
        .await
        .context("failed to connect to database")?;
    let reconciler = topup::reconciler::Reconciler::from_routes(pool.clone(), Arc::new(routes))
        .context("failed to configure reconciler")?;
    let result = match topup::reconciler::hold_lease_owner_lock(&pool).await {
        Ok(lease_owner) => {
            let result = reconciler.run_once().await;
            if let Err(error) = lease_owner.release().await {
                tracing::warn!(%error, "failed to release the lease-owner lock");
            }
            result
        }
        Err(error) => Err(error),
    };
    pool.close().await;
    let report = result.context("reconciliation failed")?;
    if !report.succeeded() {
        tracing::error!(
            findings = report.findings.len(),
            failed_checks = report.failed_checks.len(),
            "reconciliation completed with failed checks"
        );
        return Ok(ExitCode::FAILURE);
    }
    tracing::info!(findings = report.findings.len(), "reconciliation completed");
    Ok(ExitCode::SUCCESS)
}

/// Long-running service tasks sharing one cancellation token.
///
/// A task that exits before shutdown stops the service: `run` then cancels the rest.
struct ServiceTasks {
    set: JoinSet<Result<(), String>>,
    names: HashMap<tokio::task::Id, String>,
    cancellation: CancellationToken,
}

/// Result of a service task, normalized for reporting.
trait TaskOutcome {
    fn into_outcome(self) -> Result<(), String>;
}

impl TaskOutcome for () {
    fn into_outcome(self) -> Result<(), String> {
        Ok(())
    }
}

impl<E: std::fmt::Display> TaskOutcome for Result<(), E> {
    fn into_outcome(self) -> Result<(), String> {
        self.map_err(|error| error.to_string())
    }
}

impl ServiceTasks {
    fn new() -> Self {
        Self {
            set: JoinSet::new(),
            names: HashMap::new(),
            cancellation: CancellationToken::new(),
        }
    }

    fn token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    fn spawn<F>(&mut self, name: impl Into<String>, task: impl FnOnce(CancellationToken) -> F)
    where
        F: Future + Send + 'static,
        F::Output: TaskOutcome,
    {
        let task = task(self.cancellation.clone());
        let handle = self.set.spawn(async move { task.await.into_outcome() });
        self.names.insert(handle.id(), name.into());
    }

    fn name(&self, id: tokio::task::Id) -> &str {
        self.names.get(&id).map_or("unknown", String::as_str)
    }

    /// Waits for the first task to exit and reports it.
    async fn first_exit(&mut self) {
        let Some(result) = self.set.join_next_with_id().await else {
            return std::future::pending().await;
        };
        match result {
            // After a cancellation (a failed lease-owner lock) exits are expected; the cause is
            // reported separately.
            Ok((_, Ok(()))) if self.cancellation.is_cancelled() => {}
            Ok((id, Ok(()))) => {
                tracing::error!(task = self.name(id), "service task stopped before shutdown");
            }
            Ok((id, Err(error))) => {
                tracing::error!(task = self.name(id), %error, "service task failed");
            }
            Err(error) => {
                tracing::error!(task = self.name(error.id()), %error, "service task failed to join");
            }
        }
    }

    /// Cancels every task and waits for all of them; returns whether each stopped cleanly.
    async fn shutdown(mut self) -> bool {
        self.cancellation.cancel();
        let mut clean = true;
        while let Some(result) = self.set.join_next_with_id().await {
            match result {
                Ok((_, Ok(()))) => {}
                Ok((id, Err(error))) => {
                    tracing::error!(
                        task = self.name(id),
                        %error,
                        "service task failed during shutdown"
                    );
                    clean = false;
                }
                Err(error) => {
                    tracing::error!(
                        task = self.name(error.id()),
                        %error,
                        "service task failed to join during shutdown"
                    );
                    clean = false;
                }
            }
        }
        clean
    }
}

/// Interval between liveness pings on the lease-owner lock connection.
const LEASE_OWNER_PING: Duration = Duration::from_secs(5);

/// Takes the lease-owner lock, retrying with backoff while the post-restore gate holds it;
/// `None` means shutdown was requested first.
///
/// Waiting instead of exiting keeps a restart policy from crash-looping during a restore.
async fn wait_for_lease_owner_lock(
    pool: &sqlx::PgPool,
) -> anyhow::Result<Option<topup::reconciler::LeaseOwnerLock>> {
    let mut delay = Duration::from_secs(1);
    let shutdown = wait_for_shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        match topup::reconciler::hold_lease_owner_lock(pool).await {
            Ok(lock) => return Ok(Some(lock)),
            Err(topup::reconciler::ReconciliationError::LeaseOwnerLock(reason)) => {
                tracing::warn!(
                    reason,
                    retry_in_s = delay.as_secs(),
                    "waiting for the lease-owner lock before processing deposits"
                );
            }
            Err(error) => return Err(error).context("failed to take the lease-owner lock"),
        }
        tokio::select! {
            signal = &mut shutdown => {
                signal.context("failed to listen for shutdown signal")?;
                return Ok(None);
            }
            () = tokio::time::sleep(delay) => {}
        }
        delay = delay.saturating_mul(2).min(Duration::from_secs(60));
    }
}

#[cfg(unix)]
async fn wait_for_shutdown_signal() -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};

    use tokio::signal::unix::{SignalKind, signal};

    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        result = tokio::signal::ctrl_c() => result,
        received = terminate.recv() => received
            .ok_or_else(|| Error::new(ErrorKind::BrokenPipe, "SIGTERM listener closed")),
    }
}

#[cfg(not(unix))]
async fn wait_for_shutdown_signal() -> std::io::Result<()> {
    tokio::signal::ctrl_c().await
}

fn load_routes(paths: &[PathBuf]) -> anyhow::Result<RouteSet> {
    let routes = paths
        .iter()
        .map(|path| {
            let yaml = std::fs::read_to_string(path)
                .map_err(|error| anyhow!("failed to read `{}`: {error}", path.display()))?;
            route::parse_and_validate(&yaml, false)
                .map_err(|error| anyhow!("invalid route `{}`: {error}", path.display()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    RouteSet::new(routes).map_err(anyhow::Error::msg)
}

fn required_env(name: &'static str) -> anyhow::Result<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{name} is required for run"))
}

/// Connects a pool of at most `max_connections` to the database URL in `environment`.
async fn connect(
    environment: &'static str,
    command: &'static str,
    max_connections: u32,
) -> anyhow::Result<PgPool> {
    let url = std::env::var(environment)
        .ok()
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{environment} is required for {command}"))?;
    Ok(PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(&url)
        .await?)
}

/// Queue depth of every dstack signer actor.
const SIGNER_QUEUE: NonZeroUsize =
    NonZeroUsize::new(32).expect("constant signer queue is non-zero");
/// Maximum duration of one dstack signing request.
const SIGNER_TIMEOUT: Duration = Duration::from_secs(15);

/// Starts a dstack signer actor, deriving `operator/v{n}` when an operator key version is given.
fn spawn_signer(operator_key_version: Option<NonZeroU32>) -> std::io::Result<SignerHandle> {
    let signer = match operator_key_version {
        Some(version) => DstackSigner::new().with_operator_key_version(version),
        None => DstackSigner::new(),
    };
    SignerHandle::spawn(signer, SIGNER_QUEUE, SIGNER_TIMEOUT)
}

fn parse_event_id(value: &str) -> Result<Uuid, String> {
    topup::ids::parse_event(value).ok_or_else(|| "expected an evt_ id or a UUID".to_owned())
}

async fn replay_outbox(
    id: Option<Uuid>,
    since: Option<&str>,
    force: bool,
) -> anyhow::Result<ExitCode> {
    let selector = match (id, since) {
        (Some(id), None) => topup::outbox::ReplaySelector::Id(id),
        (None, Some(since)) => topup::outbox::ReplaySelector::Since(
            DateTime::parse_from_rfc3339(since)
                .context("--since must be an RFC 3339 timestamp")?
                .with_timezone(&Utc),
        ),
        _ => bail!("exactly one of --id or --since is required"),
    };
    let pool = connect("DATABASE_URL", "outbox replay", 1)
        .await
        .context("failed to connect to database")?;
    let reason = if force {
        "manual CLI replay with delivered state reset"
    } else {
        "manual CLI replay of pending events"
    };
    let count = topup::outbox::replay(&pool, selector, force, "cli", reason)
        .await
        .context("failed to schedule outbox replay")?;
    tracing::info!(count, force, "outbox replay scheduled");
    Ok(ExitCode::SUCCESS)
}

async fn migrate() -> anyhow::Result<ExitCode> {
    let pool = connect("MIGRATE_DATABASE_URL", "migrate", 1)
        .await
        .context("failed to connect to database")?;
    topup::db::migrate(&pool)
        .await
        .context("failed to apply database migrations")?;
    tracing::info!("database migrations applied");
    Ok(ExitCode::SUCCESS)
}

fn show_route(file: &Path, template: bool) -> ExitCode {
    let resolved = std::fs::read_to_string(file)
        .map_err(|error| format!("failed to read route file `{}`: {error}", file.display()))
        .and_then(|yaml| {
            route::parse_and_validate(&yaml, template)
                .map_err(|error| format!("route file `{}` is invalid: {error}", file.display()))
        })
        .and_then(|route| route::resolved_json(&route));
    match resolved {
        Ok(json) => {
            print!("{json}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn validate_route(file: &Path, template: bool) -> ExitCode {
    let yaml = match std::fs::read_to_string(file) {
        Ok(yaml) => yaml,
        Err(error) => {
            eprintln!("failed to read route file `{}`: {error}", file.display());
            return ExitCode::FAILURE;
        }
    };
    match route::parse_and_validate(&yaml, template) {
        Ok(_) => {
            let kind = if template {
                "route template"
            } else {
                "route file"
            };
            println!(
                "{kind} `{}` is valid at schema level; on-chain deployment and Safe control were not checked",
                file.display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("route file `{}` is invalid: {error}", file.display());
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::{ServiceTasks, parse_nonce};

    #[test]
    fn nonce_policy_accepts_one_through_thirty_two_bytes() {
        assert_eq!(parse_nonce("00"), Ok(vec![0]));
        assert_eq!(parse_nonce(&"ab".repeat(32)), Ok(vec![0xab; 32]));
    }

    #[test]
    fn nonce_policy_rejects_empty_oversized_and_invalid_values() {
        assert_eq!(parse_nonce(""), Err("nonce must be non-empty hexadecimal"));
        assert_eq!(
            parse_nonce(&"ab".repeat(33)),
            Err("nonce must be at most 32 bytes (64 hexadecimal characters)")
        );
        assert_eq!(parse_nonce("0"), Err("nonce must be valid hexadecimal"));
        assert_eq!(parse_nonce("zz"), Err("nonce must be valid hexadecimal"));
    }

    #[tokio::test]
    async fn an_early_task_exit_is_reported_and_shutdown_stops_every_task() {
        let mut tasks = ServiceTasks::new();
        let stopped = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&stopped);
        tasks.spawn("long-running", |cancellation| async move {
            cancellation.cancelled().await;
            observed.store(true, Ordering::SeqCst);
        });
        tasks.spawn("failing", |_| async { Err::<(), _>("boom") });

        tokio::time::timeout(std::time::Duration::from_secs(5), tasks.first_exit())
            .await
            .expect("the failing task exits first");
        assert!(!stopped.load(Ordering::SeqCst));
        assert!(tasks.shutdown().await, "the remaining task stops cleanly");
        assert!(stopped.load(Ordering::SeqCst));

        let mut tasks = ServiceTasks::new();
        tasks.spawn("failing on shutdown", |cancellation| async move {
            cancellation.cancelled().await;
            Err::<(), _>("shutdown failure")
        });
        assert!(
            !tasks.shutdown().await,
            "a failure during shutdown is unclean"
        );
    }
}

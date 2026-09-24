//! Command-line entry point for the crypto top-up service.

mod route;

use std::collections::{BTreeSet, HashMap};
use std::future::{Future, IntoFuture as _};
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use clap::{Args, Parser, Subcommand};
use serde_json::json;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use topup::pump::{AgeAlertConfig, AgeAlerter, Pump, PumpConfig, StepSet};
use topup::routes::RouteSet;
use topup::steps::confirm::{ConfirmStep, SettlementProductLookup};
use topup::steps::screen::ScreenStep;
use topup::steps::settle::SettleStep;
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
#[command(name = "topup", version, about = "Crypto top-up service")]
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
    /// Run one reconciliation pass and exit; with --post-restore, only while the service is stopped.
    Reconcile(ReconcileArgs),
    Attest(AttestArgs),
    /// Derive the backup key and database credentials from dstack into a shared tmpfs.
    Keys(KeysArgs),
    Heartbeat(HeartbeatArgs),
    /// Validate a restored database and run post-restore reconciliation.
    ///
    /// Run only while the service, heartbeat, and backup processes are stopped: the post-restore
    /// round claims every deposit at or beyond `cleared` and adopts the product's answers.
    RestoreCheck(RestoreCheckArgs),
}

#[derive(Args)]
struct RunArgs {
    /// API socket address; defaults to the deployment port on all interfaces.
    #[arg(long, default_value = "0.0.0.0:8080")]
    bind: std::net::SocketAddr,
    /// Monitoring socket address; keep this listener off the public gateway.
    #[arg(long, default_value = "127.0.0.1:9464")]
    metrics_bind: std::net::SocketAddr,
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
/// Minimum interval between repeated alerts for the same deposit state.
const AGE_ALERT_REMINDER: Duration = Duration::from_secs(60 * 60);
/// Interval between full reconciliation passes.
const RECONCILIATION_INTERVAL: Duration = Duration::from_secs(10 * 60);

#[derive(Args)]
struct ReconcileArgs {
    /// Run the restore gate and fail while any deposit is incomplete. Run it only while the
    /// service, heartbeat, and backup processes are stopped; it refuses to start while a service
    /// or reconcile process holds deposit leases.
    #[arg(long)]
    post_restore: bool,
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
    /// Directory (tmpfs) for backup.key and backup-vN.key, which WAL-G reads.
    #[arg(long, value_name = "DIR")]
    backup_dir: PathBuf,
    /// Directory (tmpfs) for the owner login's postgres.password and postgres.pgpass.
    #[arg(long, value_name = "DIR")]
    owner_dir: PathBuf,
    /// Directory (tmpfs) for the application login's topup_service.pgpass.
    #[arg(long, value_name = "DIR")]
    app_dir: PathBuf,
    /// dstack backup key version, producing the domain backup/vN.
    #[arg(long, default_value_t = 1)]
    version: u32,
    /// Comma-separated retained domains written as backup-vN.key for restore fallback.
    #[arg(long, value_delimiter = ',', default_value = "0")]
    fallback_versions: Vec<u32>,
    /// Keep the process alive so the shared tmpfs remains mounted.
    #[arg(long, conflicts_with = "check")]
    hold: bool,
    /// Check only that the files were atomically published with safe metadata.
    #[arg(long, conflicts_with = "hold")]
    check: bool,
}

#[derive(Args)]
struct RestoreCheckArgs {
    /// Last source heartbeat committed before the recorded failure point.
    #[arg(long, value_name = "RFC3339")]
    expected_heartbeat_at: DateTime<Utc>,
    /// Source WAL location from the same heartbeat log line. Omit only as a declared incident
    /// exception; RPO is then proven by the heartbeat timestamp alone and flagged in the report.
    #[arg(long, value_name = "PG_LSN")]
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

#[derive(Subcommand)]
enum OutboxCommand {
    Replay {
        /// Replay one event by its stable webhook identifier.
        #[arg(long, conflicts_with = "since", required_unless_present = "since")]
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
}

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(error) = topup::observability::log_subscriber(std::io::stdout).try_init() {
        eprintln!("failed to initialize tracing: {error}");
        return ExitCode::FAILURE;
    }
    if let Err(error) = topup::observability::init() {
        tracing::error!(%error, "failed to initialize observability");
        return ExitCode::FAILURE;
    }

    let cli = Cli::parse();

    let result = match cli.command {
        TopupCommand::Run(args) => return run(&args).await,
        TopupCommand::Migrate => return migrate().await,
        TopupCommand::Route {
            command: RouteCommand::Validate { template, file },
        } => return validate_route(&file, template),
        TopupCommand::Outbox {
            command: OutboxCommand::Replay { id, since, force },
        } => return replay_outbox(id, since.as_deref(), force).await,
        TopupCommand::Reconcile(args) => return reconcile(&args).await,
        TopupCommand::Attest(args) => attest(&args).await,
        TopupCommand::Keys(args) => return keys(&args).await,
        TopupCommand::Heartbeat(args) => return heartbeat(&args).await,
        TopupCommand::RestoreCheck(args) => return restore_check(&args).await,
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

async fn keys(args: &KeysArgs) -> ExitCode {
    if args.check {
        return if topup::keys::check(&args.backup_dir, &args.owner_dir, &args.app_dir).is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    let signer = DstackSigner::new();
    let versions = std::iter::once(args.version)
        .chain(args.fallback_versions.iter().copied())
        .collect::<BTreeSet<_>>();
    for version in versions {
        let Ok(key) = signer.derive_backup_key_version(version).await else {
            tracing::error!(version, "failed to derive backup key");
            return ExitCode::FAILURE;
        };
        let versioned = topup::keys::versioned_key_path(&args.backup_dir, version);
        if topup::keys::write_backup_key(&versioned, &key).is_err() {
            tracing::error!(path = %versioned.display(), version, "failed to write backup key file");
            return ExitCode::FAILURE;
        }
        let current = args.backup_dir.join(topup::keys::BACKUP_KEY_FILE);
        if version == args.version && topup::keys::write_backup_key(&current, &key).is_err() {
            tracing::error!(path = %current.display(), version, "failed to write current backup key file");
            return ExitCode::FAILURE;
        }
    }
    let (Ok(owner), Ok(app)) = tokio::join!(
        signer.derive_secret(DB_OWNER_KEY_DOMAIN),
        signer.derive_secret(DB_APP_KEY_DOMAIN),
    ) else {
        tracing::error!("failed to derive database credentials");
        return ExitCode::FAILURE;
    };
    if topup::keys::write_database_credentials(&args.owner_dir, &args.app_dir, &owner, &app)
        .is_err()
    {
        tracing::error!("failed to write database credential files");
        return ExitCode::FAILURE;
    }
    tracing::info!(version = args.version, "key files are ready");
    if args.hold {
        if wait_for_shutdown_signal().await.is_err() {
            tracing::error!("failed to listen for key holder shutdown signal");
            return ExitCode::FAILURE;
        }
        tracing::info!("key holder stopped");
    }
    ExitCode::SUCCESS
}

/// Refuses service commands on a replacement CVM that boots for a restore (`deploy/RESTORE.md`).
fn service_enabled(command: &str) -> Result<(), String> {
    match std::env::var("TOPUP_SERVICE_ENABLED").as_deref() {
        Err(_) | Ok("on") => Ok(()),
        Ok("off") => Err(format!(
            "{command} is disabled while TOPUP_SERVICE_ENABLED=off"
        )),
        Ok(_) => Err("TOPUP_SERVICE_ENABLED must be on or off".to_owned()),
    }
}

async fn heartbeat(args: &HeartbeatArgs) -> ExitCode {
    if let Err(error) = service_enabled("heartbeat") {
        tracing::error!(%error, "heartbeat refused to start");
        return ExitCode::FAILURE;
    }
    let pool = match connect("DATABASE_URL", "heartbeat", 1).await {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let mut interval = tokio::time::interval(Duration::from_secs(args.interval_s));
    loop {
        tokio::select! {
            _ = interval.tick() => {
                match topup::heartbeat::record(&pool).await {
                    Ok(record) => tracing::info!(
                        heartbeat_id = record.id,
                        // RFC 3339, so the value can be passed to --expected-heartbeat-at as is.
                        recorded_at = %record
                            .recorded_at
                            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
                        rpo_seconds = record.rpo_seconds,
                        wal_lsn = %record.wal_lsn,
                        "restore heartbeat recorded"
                    ),
                    Err(_) => {
                        tracing::error!("failed to record restore heartbeat");
                        return ExitCode::FAILURE;
                    }
                }
            }
            signal = tokio::signal::ctrl_c() => {
                if signal.is_err() {
                    tracing::error!("failed to listen for heartbeat shutdown signal");
                    return ExitCode::FAILURE;
                }
                tracing::info!("heartbeat stopped");
                return ExitCode::SUCCESS;
            }
        }
    }
}

async fn restore_check(args: &RestoreCheckArgs) -> ExitCode {
    let routes = match load_routes(&args.routes) {
        Ok(routes) => routes,
        Err(error) => {
            tracing::error!(%error, "failed to load route configuration");
            return ExitCode::FAILURE;
        }
    };
    // The post-restore gate reads and repairs with owner credentials, never the service login.
    let pool = match connect("MIGRATE_DATABASE_URL", "restore-check", 4).await {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let signer = match spawn_signer(None) {
        Ok(signer) => signer,
        Err(error) => {
            tracing::error!(%error, "failed to start restore-check signer actor");
            return ExitCode::FAILURE;
        }
    };
    let reconciler =
        match topup::reconciler::Reconciler::from_routes(pool.clone(), Arc::new(routes), signer) {
            Ok(reconciler) => reconciler,
            Err(error) => {
                tracing::error!(%error, "failed to configure post-restore reconciler");
                return ExitCode::FAILURE;
            }
        };
    let expectations = topup::restore::RestoreExpectations {
        expected_heartbeat_at: args.expected_heartbeat_at,
        expected_lsn: args.expected_lsn.clone(),
    };
    let report = match topup::restore::check(&pool, &expectations, &reconciler).await {
        Ok(report) => report,
        Err(message) => {
            tracing::error!(%message, "restore check failed");
            return ExitCode::FAILURE;
        }
    };
    match serde_json::to_string(&report) {
        Ok(encoded) => {
            println!("{encoded}");
            if report.status == "ok" {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(_) => {
            tracing::error!("failed to encode restore check report");
            ExitCode::FAILURE
        }
    }
}

async fn attest(args: &AttestArgs) -> Result<(), &'static str> {
    let nonce = parse_nonce(&args.nonce)?;
    let version = args.operator_key_version;
    let operator_keys = if args.routes.is_empty() {
        Vec::new()
    } else {
        load_routes(&args.routes)
            .and_then(|routes| routes.operator_keys())
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

async fn run(args: &RunArgs) -> ExitCode {
    let routes = match load_routes(&args.routes) {
        Ok(routes) => routes,
        Err(error) => {
            tracing::error!(%error, "failed to load route configuration");
            return ExitCode::FAILURE;
        }
    };
    let age_config = match AgeAlertConfig::from_routes(routes.routes()) {
        Ok(config) => config,
        Err(error) => {
            tracing::error!(%error, "invalid age alert configuration");
            return ExitCode::FAILURE;
        }
    };
    let rate_lock_quotes = match topup::locks::ConfiguredQuoteProvider::from_routes(routes.routes())
    {
        Ok(provider) => Arc::new(provider) as Arc<dyn topup::locks::QuoteProvider>,
        Err(error) => {
            tracing::error!(%error, "invalid rate-lock pricing configuration");
            return ExitCode::FAILURE;
        }
    };
    let pump_config = PumpConfig {
        step_timeout: STEP_TIMEOUT,
        wait_interval: Duration::from_secs(args.wait_interval_s),
        ..PumpConfig::default()
    };
    // Checked before the on-chain contract check; `connect` reads it again below.
    if let Err(error) = service_enabled("run").and_then(|()| required_env("DATABASE_URL")) {
        tracing::error!(%error, "missing runtime configuration");
        return ExitCode::FAILURE;
    }
    let admin_kid = match required_env("TOPUP_ADMIN_KID") {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "missing runtime configuration");
            return ExitCode::FAILURE;
        }
    };
    let admin_public_key = match required_env("TOPUP_ADMIN_PUBLIC_KEY") {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "missing runtime configuration");
            return ExitCode::FAILURE;
        }
    };
    let admin_key = match topup::api::VerificationKey::from_base64(admin_kid, &admin_public_key) {
        Ok(key) => key,
        Err(error) => {
            tracing::error!(%error, "invalid administrative verification key");
            return ExitCode::FAILURE;
        }
    };
    let public_origin = match required_env("TOPUP_PUBLIC_ORIGIN").and_then(|value| {
        topup::api::PublicOrigin::parse(&value).map_err(|error| error.to_string())
    }) {
        Ok(origin) => origin,
        Err(error) => {
            tracing::error!(%error, "invalid TOPUP_PUBLIC_ORIGIN");
            return ExitCode::FAILURE;
        }
    };
    // Architecture §4: before the database is touched, every provider must show the route's
    // factory, implementation, and treasury, so nothing issues addresses or moves funds otherwise.
    if let Err(error) = topup::contracts::verify_routes(&routes).await {
        tracing::error!(%error, "on-chain contract check failed");
        return ExitCode::FAILURE;
    }
    let routes = Arc::new(routes);
    let scanner_count = routes.chain_ids().count();
    let route_count = routes.routes().len();
    let connection_count = match u32::try_from(PUMPS)
        .ok()
        .zip(u32::try_from(scanner_count).ok())
        .zip(u32::try_from(route_count).ok())
        .and_then(|((pumps, scanners), routes)| pumps.checked_add(scanners)?.checked_add(routes))
        .and_then(|count| count.checked_add(3))
    {
        Some(count) => count,
        None => {
            tracing::error!("route count is too large");
            return ExitCode::FAILURE;
        }
    };
    let pool = match connect("DATABASE_URL", "run", connection_count).await {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let lease_owner = match wait_for_lease_owner_lock(&pool).await {
        Ok(lock) => lock,
        Err(code) => return code,
    };
    let signer = match spawn_signer(None) {
        Ok(signer) => signer,
        Err(error) => {
            tracing::error!(%error, "failed to start signer actor");
            return ExitCode::FAILURE;
        }
    };
    let delivery_worker = match topup::outbox::DeliveryWorker::new(
        pool.clone(),
        Arc::new(signer.clone()),
        topup::outbox::DeliveryConfig::default(),
    ) {
        Ok(worker) => worker,
        Err(error) => {
            tracing::error!(%error, "failed to configure webhook delivery");
            return ExitCode::FAILURE;
        }
    };
    match topup::reconciler::frozen_chains(&pool, &routes).await {
        Ok(frozen) => {
            for chain_id in frozen {
                tracing::error!(
                    chain_id,
                    "reconciliation froze configured chain; its scanner, pumps, flusher, and \
                     address issuance stay paused until the block is removed"
                );
            }
        }
        Err(error) => {
            tracing::error!(%error, "failed to load reconciliation blocks");
            return ExitCode::FAILURE;
        }
    }
    let reconciler = match topup::reconciler::Reconciler::from_routes(
        pool.clone(),
        Arc::clone(&routes),
        signer.clone(),
    ) {
        Ok(reconciler) => Arc::new(reconciler),
        Err(error) => {
            tracing::error!(%error, "failed to configure reconciler");
            return ExitCode::FAILURE;
        }
    };
    let flusher_tasks =
        match topup::flusher::runtime::configure_tasks(pool.clone(), &routes, |version| {
            spawn_signer(Some(version))
        }) {
            Ok(tasks) => tasks,
            Err(error) => {
                tracing::error!(%error, "invalid flusher runtime configuration");
                return ExitCode::FAILURE;
            }
        };
    let listener = match tokio::net::TcpListener::bind(args.bind).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, bind = %args.bind, "failed to bind API listener");
            return ExitCode::FAILURE;
        }
    };
    let metrics_listener = match tokio::net::TcpListener::bind(args.metrics_bind).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, bind = %args.metrics_bind, "failed to bind metrics listener");
            return ExitCode::FAILURE;
        }
    };
    let product_lookup = SettlementProductLookup::new(
        pool.clone(),
        Arc::clone(&routes),
        signer.clone(),
        Duration::from_secs(30),
    );
    let confirm_step =
        match ConfirmStep::from_routes(pool.clone(), &routes, Arc::new(product_lookup)) {
            Ok(step) => step,
            Err(error) => {
                tracing::error!(%error, "invalid confirm-step configuration");
                return ExitCode::FAILURE;
            }
        };
    let screen_step = match ScreenStep::from_routes(pool.clone(), &routes) {
        Ok(step) => step,
        Err(error) => {
            tracing::error!(%error, "failed to configure screening step");
            return ExitCode::FAILURE;
        }
    };
    let steps = Arc::new(StepSet::new(
        Box::new(confirm_step),
        Box::new(screen_step),
        Box::new(SettleStep::new(
            pool.clone(),
            Arc::clone(&routes),
            signer,
            Duration::from_secs(30),
        )),
        Box::new(topup::flusher::SweepStep),
    ));
    let pump = match Pump::new(pool.clone(), Arc::<StepSet>::clone(&steps), pump_config) {
        Ok(pump) => pump,
        Err(error) => {
            tracing::error!(%error, "invalid pump configuration");
            return ExitCode::FAILURE;
        }
    };
    let refund_config = topup::refunds::RefundConfirmationConfig::default();
    let refund_reader = match topup::refunds::EvmRefundChainReader::from_routes(&routes) {
        Ok(reader) => reader,
        Err(error) => {
            tracing::error!(%error, "failed to configure refund confirmation chain reader");
            return ExitCode::FAILURE;
        }
    };
    let refund_worker = match topup::refunds::RefundConfirmationWorker::new(
        pool.clone(),
        refund_reader,
        routes.routes(),
        refund_config,
    ) {
        Ok(worker) => worker,
        Err(error) => {
            tracing::error!(%error, "failed to configure refund confirmation worker");
            return ExitCode::FAILURE;
        }
    };
    let mut tasks = ServiceTasks::new();
    let state = topup::api::AppState {
        pool: pool.clone(),
        routes: Arc::clone(&routes),
        admin_key,
        public_origin,
        attestor: Arc::new(DstackAttestor::new()),
        rate_lock_quotes,
    };
    let (application, _) = topup::api::router(state);
    tasks.spawn("API server", |cancellation| {
        axum::serve(listener, application)
            .with_graceful_shutdown(cancellation.cancelled_owned())
            .into_future()
    });
    tracing::info!(bind = %args.bind, "API listening");
    tasks.spawn("metrics server", |cancellation| {
        axum::serve(metrics_listener, topup::observability::metrics_router())
            .with_graceful_shutdown(cancellation.cancelled_owned())
            .into_future()
    });
    tracing::info!(bind = %args.metrics_bind, "metrics listening");

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
    let age_alerter = AgeAlerter::with_reminder_interval(
        pool.clone(),
        age_config,
        AGE_ALERT_INTERVAL,
        AGE_ALERT_REMINDER,
    );
    tasks.spawn("age alerter", |cancellation| async move {
        age_alerter.run(cancellation).await;
    });
    let metrics_pool = pool.clone();
    let lock_exposure_caps = topup::observability::LockExposureCaps::from_routes(routes.routes());
    tasks.spawn("database metrics collector", |cancellation| {
        topup::observability::collect_database_metrics(
            metrics_pool,
            lock_exposure_caps,
            cancellation,
        )
    });
    tasks.spawn(
        "backup metrics collector",
        topup::observability::collect_backup_metrics,
    );
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

    if clean_shutdown {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

async fn reconcile(args: &ReconcileArgs) -> ExitCode {
    let routes = match load_routes(&args.routes) {
        Ok(routes) => routes,
        Err(error) => {
            tracing::error!(%error, "failed to load route configuration");
            return ExitCode::FAILURE;
        }
    };
    let pool = match connect("DATABASE_URL", "reconcile", 4).await {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let signer = match spawn_signer(None) {
        Ok(signer) => signer,
        Err(error) => {
            tracing::error!(%error, "failed to start signer actor");
            return ExitCode::FAILURE;
        }
    };
    let reconciler =
        match topup::reconciler::Reconciler::from_routes(pool.clone(), Arc::new(routes), signer) {
            Ok(reconciler) => reconciler,
            Err(error) => {
                tracing::error!(%error, "failed to configure reconciler");
                return ExitCode::FAILURE;
            }
        };
    let result = if args.post_restore {
        topup::reconciler::post_restore_once(&reconciler).await
    } else {
        match topup::reconciler::hold_lease_owner_lock(&pool).await {
            Ok(lease_owner) => {
                let result = reconciler.run_once().await;
                if let Err(error) = lease_owner.release().await {
                    tracing::warn!(%error, "failed to release the lease-owner lock");
                }
                result
            }
            Err(error) => Err(error),
        }
    };
    pool.close().await;
    match result {
        Ok(report) if args.post_restore && report.incomplete => {
            tracing::error!(
                findings = report.findings.len(),
                "post-restore reconciliation is incomplete"
            );
            ExitCode::FAILURE
        }
        Ok(report) if !args.post_restore && !report.succeeded() => {
            tracing::error!(
                findings = report.findings.len(),
                failed_checks = report.failed_checks.len(),
                "reconciliation completed with failed checks"
            );
            ExitCode::FAILURE
        }
        Ok(report) => {
            tracing::info!(findings = report.findings.len(), "reconciliation completed");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, "reconciliation failed");
            ExitCode::FAILURE
        }
    }
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

/// Takes the lease-owner lock, retrying with backoff while the post-restore gate holds it.
///
/// Waiting instead of exiting keeps a restart policy from crash-looping during a restore.
async fn wait_for_lease_owner_lock(
    pool: &sqlx::PgPool,
) -> Result<topup::reconciler::LeaseOwnerLock, ExitCode> {
    let mut delay = Duration::from_secs(1);
    let shutdown = wait_for_shutdown_signal();
    tokio::pin!(shutdown);
    loop {
        match topup::reconciler::hold_lease_owner_lock(pool).await {
            Ok(lock) => return Ok(lock),
            Err(topup::reconciler::ReconciliationError::LeaseOwnerLock(reason)) => {
                tracing::warn!(
                    reason,
                    retry_in_s = delay.as_secs(),
                    "waiting for the lease-owner lock before processing deposits"
                );
            }
            Err(error) => {
                tracing::error!(%error, "failed to take the lease-owner lock");
                return Err(ExitCode::FAILURE);
            }
        }
        tokio::select! {
            signal = &mut shutdown => {
                return Err(match signal {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(error) => {
                        tracing::error!(%error, "failed to listen for shutdown signal");
                        ExitCode::FAILURE
                    }
                });
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

fn load_routes(paths: &[PathBuf]) -> Result<RouteSet, String> {
    let routes = paths
        .iter()
        .map(|path| {
            let yaml = std::fs::read_to_string(path)
                .map_err(|error| format!("failed to read `{}`: {error}", path.display()))?;
            route::parse_and_validate(&yaml, false)
                .map_err(|error| format!("invalid route `{}`: {error}", path.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    RouteSet::new(routes)
}

fn required_env(name: &'static str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{name} is required for run"))
}

/// Connects a pool of at most `max_connections` to the database URL in `environment`.
async fn connect(
    environment: &'static str,
    command: &'static str,
    max_connections: u32,
) -> Result<PgPool, ExitCode> {
    let Some(url) = std::env::var(environment)
        .ok()
        .filter(|value| !value.is_empty())
    else {
        tracing::error!("{environment} is required for {command}");
        return Err(ExitCode::FAILURE);
    };
    PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(&url)
        .await
        .map_err(|error| {
            tracing::error!(%error, command, "failed to connect to database");
            ExitCode::FAILURE
        })
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

async fn replay_outbox(id: Option<Uuid>, since: Option<&str>, force: bool) -> ExitCode {
    let selector = match (id, since) {
        (Some(id), None) => topup::outbox::ReplaySelector::Id(id),
        (None, Some(since)) => match DateTime::parse_from_rfc3339(since) {
            Ok(value) => topup::outbox::ReplaySelector::Since(value.with_timezone(&Utc)),
            Err(_) => {
                tracing::error!("--since must be an RFC 3339 timestamp");
                return ExitCode::FAILURE;
            }
        },
        _ => {
            tracing::error!("exactly one of --id or --since is required");
            return ExitCode::FAILURE;
        }
    };
    let pool = match connect("DATABASE_URL", "outbox replay", 1).await {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    let reason = if force {
        "manual CLI replay with delivered state reset"
    } else {
        "manual CLI replay of pending events"
    };
    match topup::outbox::replay(&pool, selector, force, "cli", reason).await {
        Ok(count) => {
            tracing::info!(count, force, "outbox replay scheduled");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, "failed to schedule outbox replay");
            ExitCode::FAILURE
        }
    }
}

async fn migrate() -> ExitCode {
    let pool = match connect("MIGRATE_DATABASE_URL", "migrate", 1).await {
        Ok(pool) => pool,
        Err(code) => return code,
    };
    if let Err(error) = topup::db::migrate(&pool).await {
        tracing::error!(%error, "failed to apply database migrations");
        return ExitCode::FAILURE;
    }
    tracing::info!("database migrations applied");
    ExitCode::SUCCESS
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

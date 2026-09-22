//! Command-line entry point for the crypto top-up service.

mod route;

use std::collections::BTreeSet;
use std::num::NonZeroUsize;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use clap::{Args, Parser, Subcommand};
use serde_json::json;
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;
use topup::pump::{
    AgeAlertConfig, AgeAlerter, NoopStepSet, Pump, PumpConfig, PumpMetrics, StepSet,
};
use topup::steps::confirm::{ConfirmStep, SettlementProductLookup};
use topup::steps::screen::ScreenStep;
use topup::steps::settle::SettleStep;
use topup_adapters::attestation::DstackAttestor;
#[cfg(feature = "dev-signer")]
use topup_adapters::attestation::report_data;
use topup_adapters::risk::oracle::DEFAULT_REQUEST_TIMEOUT;
use topup_adapters::settlement::http::{SettlementApi, SettlementClient};
#[cfg(feature = "dev-signer")]
use topup_adapters::signer::DevSigner;
use topup_adapters::signer::actor::SignerHandle;
use topup_adapters::signer::dstack::DstackSigner;
use topup_core::SETTLEMENT_KEY_DOMAIN;
use topup_core::route::RouteFile;
#[cfg(feature = "dev-signer")]
use topup_core::{SecretKey32, Signer as _};
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
    Attest(AttestArgs),
    BackupKey(BackupKeyArgs),
    Heartbeat(HeartbeatArgs),
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
    /// Number of concurrent deposit pumps in this process.
    #[arg(long, default_value_t = NonZeroUsize::MIN)]
    pumps: NonZeroUsize,
    /// Maximum duration of one step; must be shorter than five minutes.
    #[arg(long, default_value_t = 240)]
    step_timeout_s: u64,
    /// Delay before retrying an expected wait outcome.
    #[arg(long, default_value_t = 60)]
    wait_interval_s: u64,
    /// Interval between deposit state-age scans.
    #[arg(long, default_value_t = 60)]
    age_alert_interval_s: u64,
    /// Minimum interval between repeated alerts for the same deposit state.
    #[arg(long, default_value_t = 60 * 60)]
    age_alert_reminder_s: u64,
}

#[derive(Args)]
struct AttestArgs {
    #[arg(long, value_name = "HEX")]
    nonce: String,
    #[cfg(feature = "dev-signer")]
    #[arg(long, help = "Use development keys without a hardware quote")]
    dev: bool,
}

#[derive(Args)]
struct BackupKeyArgs {
    /// File that WAL-G reads through WALG_LIBSODIUM_KEY_PATH.
    #[arg(long, value_name = "FILE")]
    output: PathBuf,
    /// dstack backup key version, producing the domain backup/vN.
    #[arg(long, default_value_t = 1)]
    version: u32,
    /// Comma-separated retained domains written as backup-vN.key for restore fallback.
    #[arg(long, value_delimiter = ',', default_value = "0")]
    fallback_versions: Vec<u32>,
    /// Keep the process alive so the shared tmpfs remains mounted.
    #[arg(long, conflicts_with = "check")]
    hold: bool,
    /// Check only that the key file was atomically published with safe metadata.
    #[arg(long, conflicts_with = "hold")]
    check: bool,
    #[cfg(feature = "dev-signer")]
    #[arg(long, help = "Use deterministic local-only backup key material")]
    dev: bool,
}

#[derive(Args)]
struct RestoreCheckArgs {
    /// Last source heartbeat committed before the recorded failure point.
    #[arg(long, value_name = "RFC3339")]
    expected_heartbeat_at: DateTime<Utc>,
    /// Source WAL insert location recorded at the same failure point.
    #[arg(long, value_name = "PG_LSN")]
    expected_lsn: String,
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
    if let Err(error) = tracing_subscriber::fmt()
        .json()
        .with_target(false)
        .try_init()
    {
        eprintln!("failed to initialize tracing: {error}");
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
        TopupCommand::Attest(args) => attest(&args).await,
        TopupCommand::BackupKey(args) => return backup_key(&args).await,
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

async fn backup_key(args: &BackupKeyArgs) -> ExitCode {
    if args.check {
        return if topup::backup::check_libsodium_key(&args.output).is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    let versions = std::iter::once(args.version)
        .chain(args.fallback_versions.iter().copied())
        .collect::<BTreeSet<_>>();
    for version in versions {
        #[cfg(feature = "dev-signer")]
        let key = if args.dev {
            let signer = DevSigner::new(SecretKey32::new([1; 32]), SecretKey32::new([2; 32]));
            signer.derive_backup_key_version(version)
        } else {
            match DstackSigner::new().derive_backup_key_version(version).await {
                Ok(key) => key,
                Err(_) => {
                    tracing::error!(version, "failed to derive backup key");
                    return ExitCode::FAILURE;
                }
            }
        };

        #[cfg(not(feature = "dev-signer"))]
        let key = match DstackSigner::new().derive_backup_key_version(version).await {
            Ok(key) => key,
            Err(_) => {
                tracing::error!(version, "failed to derive backup key");
                return ExitCode::FAILURE;
            }
        };

        let versioned = topup::backup::versioned_key_path(&args.output, version);
        if topup::backup::write_libsodium_key(&versioned, &key).is_err() {
            tracing::error!(path = %versioned.display(), version, "failed to write backup key file");
            return ExitCode::FAILURE;
        }
        if version == args.version
            && topup::backup::write_libsodium_key(&args.output, &key).is_err()
        {
            tracing::error!(path = %args.output.display(), version, "failed to write current backup key file");
            return ExitCode::FAILURE;
        }
    }
    tracing::info!(
        path = %args.output.display(),
        version = args.version,
        "backup key file is ready"
    );
    if args.hold {
        if tokio::signal::ctrl_c().await.is_err() {
            tracing::error!("failed to listen for backup key shutdown signal");
            return ExitCode::FAILURE;
        }
        tracing::info!("backup key holder stopped");
    }
    ExitCode::SUCCESS
}

async fn heartbeat(args: &HeartbeatArgs) -> ExitCode {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(value) if !value.is_empty() => value,
        Ok(_) | Err(_) => {
            tracing::error!("DATABASE_URL is required for heartbeat");
            return ExitCode::FAILURE;
        }
    };
    let pool = match PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
    {
        Ok(pool) => pool,
        Err(_) => {
            tracing::error!("failed to connect to database for heartbeat");
            return ExitCode::FAILURE;
        }
    };
    let mut interval = tokio::time::interval(Duration::from_secs(args.interval_s));
    loop {
        tokio::select! {
            _ = interval.tick() => {
                match topup::heartbeat::record(&pool).await {
                    Ok(record) => tracing::info!(
                        heartbeat_id = record.id,
                        recorded_at = %record.recorded_at,
                        rpo_seconds = record.rpo_seconds,
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
    let database_url = [
        "RESTORE_DATABASE_URL",
        "MIGRATE_DATABASE_URL",
        "DATABASE_URL",
    ]
    .into_iter()
    .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()));
    let Some(database_url) = database_url else {
        tracing::error!(
            "RESTORE_DATABASE_URL, MIGRATE_DATABASE_URL, or DATABASE_URL is required for restore-check"
        );
        return ExitCode::FAILURE;
    };
    let pool = match PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
    {
        Ok(pool) => pool,
        Err(_) => {
            tracing::error!("failed to connect to restored database");
            return ExitCode::FAILURE;
        }
    };
    let signer = match SignerHandle::spawn(
        DstackSigner::new(),
        NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN),
        Duration::from_secs(10),
    ) {
        Ok(signer) => signer,
        Err(error) => {
            tracing::error!(%error, "failed to start restore-check signer actor");
            return ExitCode::FAILURE;
        }
    };
    let client_factory = |endpoint: &str| -> Result<Arc<dyn SettlementApi>, String> {
        SettlementClient::new(endpoint, signer.clone(), Duration::from_secs(30))
            .map(|client| Arc::new(client) as Arc<dyn SettlementApi>)
            .map_err(|_| "invalid product settlement endpoint during restore-check".to_owned())
    };
    let expectations = topup::restore::RestoreExpectations {
        expected_heartbeat_at: args.expected_heartbeat_at,
        expected_lsn: args.expected_lsn.clone(),
    };
    let report = match topup::restore::check(&pool, &expectations, &client_factory).await {
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

    #[cfg(feature = "dev-signer")]
    if args.dev {
        let signer = DevSigner::new(SecretKey32::new([1; 32]), SecretKey32::new([2; 32]));
        let public_key = signer
            .settlement_public_key()
            .await
            .map_err(|_| "development settlement key is invalid")?;
        return print_attestation(
            &public_key.0,
            &report_data(&nonce, &public_key),
            &[],
            &[],
            &[],
        );
    }

    let evidence = DstackAttestor::new()
        .attest(&nonce)
        .await
        .map_err(|_| "failed to collect dstack attestation")?;
    print_attestation(
        &evidence.settlement_public_key.0,
        &evidence.report_data,
        &evidence.quote,
        &evidence.info.app_id,
        &evidence.info.compose_hash,
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

fn print_attestation(
    settlement_public_key: &[u8; 32],
    report_data: &[u8; 32],
    quote: &[u8],
    app_id: &[u8],
    compose_hash: &[u8],
) -> Result<(), &'static str> {
    let output = json!({
        "keyid": SETTLEMENT_KEY_DOMAIN,
        "settlement_pubkey": hex::encode(settlement_public_key),
        "report_data": hex::encode(report_data),
        "quote": hex::encode(quote),
        "app_id": if app_id.is_empty() { String::new() } else { format!("0x{}", hex::encode(app_id)) },
        "compose_hash": if compose_hash.is_empty() { String::new() } else { format!("0x{}", hex::encode(compose_hash)) },
    });
    let encoded = serde_json::to_string(&output).map_err(|_| "failed to encode attestation")?;
    println!("{encoded}");
    Ok(())
}

async fn run(args: &RunArgs) -> ExitCode {
    if args.age_alert_interval_s == 0 {
        tracing::error!("age alert interval must be positive");
        return ExitCode::FAILURE;
    }
    if args.age_alert_reminder_s == 0 {
        tracing::error!("age alert reminder interval must be positive");
        return ExitCode::FAILURE;
    }
    let routes = match load_routes(&args.routes) {
        Ok(routes) => routes,
        Err(error) => {
            tracing::error!(%error, "failed to load route configuration");
            return ExitCode::FAILURE;
        }
    };
    let age_config = match AgeAlertConfig::from_routes(&routes) {
        Ok(config) => config,
        Err(error) => {
            tracing::error!(%error, "invalid age alert configuration");
            return ExitCode::FAILURE;
        }
    };
    let scanner_routes = match topup::scanner::configure_routes(&routes) {
        Ok(routes) => routes,
        Err(error) => {
            tracing::error!(%error, "invalid scanner route configuration");
            return ExitCode::FAILURE;
        }
    };
    let pump_config = PumpConfig {
        step_timeout: Duration::from_secs(args.step_timeout_s),
        wait_interval: Duration::from_secs(args.wait_interval_s),
        ..PumpConfig::default()
    };
    let database_url = match required_env("DATABASE_URL") {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(%error, "missing runtime configuration");
            return ExitCode::FAILURE;
        }
    };
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
    let scanner_count = scanner_routes.len();
    let route_count = routes.len();
    let connection_count = match u32::try_from(args.pumps.get())
        .ok()
        .zip(u32::try_from(scanner_count).ok())
        .zip(u32::try_from(route_count).ok())
        .and_then(|((pumps, scanners), routes)| pumps.checked_add(scanners)?.checked_add(routes))
        .and_then(|count| count.checked_add(2))
    {
        Some(count) => count,
        None => {
            tracing::error!("pump count is too large");
            return ExitCode::FAILURE;
        }
    };
    let pool = match PgPoolOptions::new()
        .max_connections(connection_count)
        .connect(&database_url)
        .await
    {
        Ok(pool) => pool,
        Err(error) => {
            tracing::error!(%error, "failed to connect to database");
            return ExitCode::FAILURE;
        }
    };
    let signer = match SignerHandle::spawn(
        DstackSigner::new(),
        NonZeroUsize::new(32).expect("constant signer queue is non-zero"),
        Duration::from_secs(15),
    ) {
        Ok(signer) => signer,
        Err(error) => {
            tracing::error!(%error, "failed to start signer actor");
            return ExitCode::FAILURE;
        }
    };
    let flusher_tasks =
        match topup::flusher::runtime::configure_tasks(pool.clone(), &routes, signer.clone()) {
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
    let product_lookup =
        SettlementProductLookup::new(pool.clone(), signer.clone(), Duration::from_secs(30));
    let confirm_step =
        match ConfirmStep::from_routes(pool.clone(), &routes, Arc::new(product_lookup)) {
            Ok(step) => step,
            Err(error) => {
                tracing::error!(%error, "invalid confirm-step configuration");
                return ExitCode::FAILURE;
            }
        };
    let screen_step = match ScreenStep::from_routes(pool.clone(), &routes, DEFAULT_REQUEST_TIMEOUT)
    {
        Ok(step) => step,
        Err(error) => {
            tracing::error!(%error, "failed to configure screening step");
            return ExitCode::FAILURE;
        }
    };
    let steps = Arc::new(
        NoopStepSet::build()
            .with_detected(Box::new(confirm_step))
            .with_confirmed(Box::new(screen_step))
            .with_cleared(Box::new(SettleStep::new(
                pool.clone(),
                signer,
                Duration::from_secs(30),
            )))
            .with_credited(Box::new(topup::flusher::SweepStep)),
    );
    let pump = match Pump::new(pool.clone(), Arc::<StepSet>::clone(&steps), pump_config) {
        Ok(pump) => pump,
        Err(error) => {
            tracing::error!(%error, "invalid pump configuration");
            return ExitCode::FAILURE;
        }
    };
    let cancellation = CancellationToken::new();
    let state = topup::api::AppState {
        pool: pool.clone(),
        routes: Arc::new(routes),
        admin_key,
        attestor: Arc::new(DstackAttestor::new()),
    };
    let (application, _) = topup::api::router(state);
    let api_cancellation = cancellation.child_token();
    let mut api_task = tokio::spawn(async move {
        axum::serve(listener, application)
            .with_graceful_shutdown(api_cancellation.cancelled_owned())
            .await
    });
    tracing::info!(bind = %args.bind, "API listening");

    let scanner_pool = pool.clone();
    let scanner_cancellation = cancellation.child_token();
    let mut scanner_task = tokio::spawn(async move {
        topup::scanner::run(scanner_pool, scanner_routes, scanner_cancellation).await
    });
    let mut pump_tasks = Vec::with_capacity(args.pumps.get());
    for worker in 0..args.pumps.get() {
        let worker_pump = pump.clone();
        let worker_cancellation = cancellation.child_token();
        pump_tasks.push(tokio::spawn(async move {
            tracing::info!(worker, "deposit pump started");
            worker_pump.run(worker_cancellation).await;
        }));
    }
    let metrics = Arc::new(PumpMetrics::default());
    let age_alerter = AgeAlerter::with_reminder_interval(
        pool.clone(),
        age_config,
        Arc::clone(&metrics),
        Duration::from_secs(args.age_alert_interval_s),
        Duration::from_secs(args.age_alert_reminder_s),
    );
    let age_cancellation = cancellation.child_token();
    let age_task = tokio::spawn(async move {
        age_alerter.run(age_cancellation).await;
    });
    let mut flusher_handles = Vec::with_capacity(flusher_tasks.len());
    for task in flusher_tasks {
        let flusher_cancellation = cancellation.child_token();
        flusher_handles.push(tokio::spawn(async move {
            task.run(flusher_cancellation).await;
        }));
    }

    tracing::info!(
        pumps = args.pumps.get(),
        scanners = scanner_count,
        "topup service started"
    );
    let mut clean_shutdown = true;
    let mut api_finished = false;
    let mut scanner_finished = false;
    tokio::select! {
        signal = wait_for_shutdown_signal() => {
            if let Err(error) = signal {
                tracing::error!(%error, "failed to listen for shutdown signal");
                clean_shutdown = false;
            }
        }
        result = &mut api_task => {
            api_finished = true;
            match result {
                Ok(Ok(())) => tracing::error!("API server stopped before shutdown"),
                Ok(Err(error)) => tracing::error!(%error, "API server failed"),
                Err(error) => tracing::error!(%error, "API task failed"),
            }
            clean_shutdown = false;
        }
        result = &mut scanner_task => {
            scanner_finished = true;
            match result {
                Ok(Ok(())) => tracing::error!("scanner stopped before shutdown"),
                Ok(Err(error)) => tracing::error!(%error, "scanner task failed"),
                Err(error) => tracing::error!(%error, "scanner task failed to join"),
            }
            clean_shutdown = false;
        }
    }
    tracing::info!("shutdown requested; finishing in-flight service work");
    cancellation.cancel();

    if !api_finished {
        match api_task.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::error!(%error, "API server failed during shutdown");
                clean_shutdown = false;
            }
            Err(error) => {
                tracing::error!(%error, "API task failed during shutdown");
                clean_shutdown = false;
            }
        }
    }
    if !scanner_finished {
        match scanner_task.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::error!(%error, "scanner task failed during shutdown");
                clean_shutdown = false;
            }
            Err(error) => {
                tracing::error!(%error, "scanner task failed to join during shutdown");
                clean_shutdown = false;
            }
        }
    }
    for task in pump_tasks {
        if let Err(error) = task.await {
            tracing::error!(%error, "deposit pump task failed during shutdown");
            clean_shutdown = false;
        }
    }
    if let Err(error) = age_task.await {
        tracing::error!(%error, "age alert task failed during shutdown");
        clean_shutdown = false;
    }
    for task in flusher_handles {
        if let Err(error) = task.await {
            tracing::error!(%error, "flusher task failed during shutdown");
            clean_shutdown = false;
        }
    }
    pool.close().await;
    tracing::info!(
        stuck_deposit_alerts = metrics.stuck_deposit_alerts(),
        "topup service stopped"
    );

    if clean_shutdown {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
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

fn load_routes(paths: &[PathBuf]) -> Result<Vec<RouteFile>, String> {
    let routes = paths
        .iter()
        .map(|path| {
            let yaml = std::fs::read_to_string(path)
                .map_err(|error| format!("failed to read `{}`: {error}", path.display()))?;
            route::parse_and_validate(&yaml, false)
                .map_err(|error| format!("invalid route `{}`: {error}", path.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_route_versions(&routes)?;
    Ok(routes)
}

fn validate_route_versions(routes: &[RouteFile]) -> Result<(), String> {
    let mut versions = std::collections::BTreeSet::new();
    for route in routes {
        if !versions.insert((route.route.as_str(), route.version)) {
            return Err(format!(
                "duplicate route `{}` version {}",
                route.route, route.version
            ));
        }
    }
    Ok(())
}

fn required_env(name: &'static str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{name} is required for run"))
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
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(value) if !value.is_empty() => value,
        Ok(_) | Err(_) => {
            tracing::error!("DATABASE_URL is required for outbox replay");
            return ExitCode::FAILURE;
        }
    };
    let pool = match PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
    {
        Ok(pool) => pool,
        Err(error) => {
            tracing::error!(%error, "failed to connect to database");
            return ExitCode::FAILURE;
        }
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
    let database_url = match std::env::var("MIGRATE_DATABASE_URL") {
        Ok(value) if !value.is_empty() => value,
        Ok(_) | Err(_) => {
            tracing::error!("MIGRATE_DATABASE_URL is required for migrate");
            return ExitCode::FAILURE;
        }
    };
    let pool = match PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
    {
        Ok(pool) => pool,
        Err(error) => {
            tracing::error!(%error, "failed to connect to database");
            return ExitCode::FAILURE;
        }
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
    use super::{parse_nonce, validate_route_versions};
    use topup_core::route::RouteFile;

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

    #[test]
    fn route_loading_accepts_versions_and_rejects_exact_duplicates() {
        let route: RouteFile =
            serde_saphyr::from_str(include_str!("../tests/fixtures/phala-cloud-pha.yaml"))
                .expect("route fixture parses");
        let mut newer = route.clone();
        newer.version = route.version + 1;

        assert_eq!(validate_route_versions(&[route.clone(), newer]), Ok(()));
        assert_eq!(
            validate_route_versions(&[route.clone(), route]),
            Err("duplicate route `phala-cloud-ethereum-pha-usd` version 1".to_owned())
        );
    }
}

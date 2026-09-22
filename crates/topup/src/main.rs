//! Command-line entry point for the crypto top-up service.

mod route;

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
use topup::steps::settle::SettleStep;
use topup_adapters::attestation::DstackAttestor;
#[cfg(feature = "dev-signer")]
use topup_adapters::attestation::report_data;
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
    Run {
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
    },
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
    RestoreCheck,
}

#[derive(Args)]
struct AttestArgs {
    #[arg(long, value_name = "HEX")]
    nonce: String,
    #[cfg(feature = "dev-signer")]
    #[arg(long, help = "Use development keys without a hardware quote")]
    dev: bool,
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
        TopupCommand::Run {
            routes,
            pumps,
            step_timeout_s,
            wait_interval_s,
            age_alert_interval_s,
            age_alert_reminder_s,
        } => {
            return run(
                &routes,
                pumps,
                step_timeout_s,
                wait_interval_s,
                age_alert_interval_s,
                age_alert_reminder_s,
            )
            .await;
        }
        TopupCommand::Migrate => return migrate().await,
        TopupCommand::Route {
            command: RouteCommand::Validate { template, file },
        } => return validate_route(&file, template),
        TopupCommand::Outbox {
            command: OutboxCommand::Replay { id, since, force },
        } => return replay_outbox(id, since.as_deref(), force).await,
        TopupCommand::Attest(args) => attest(&args).await,
        TopupCommand::RestoreCheck => Err("restore-check is not implemented"),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
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
        return print_attestation(&public_key.0, &report_data(&nonce, &public_key), &[]);
    }

    let evidence = DstackAttestor::new()
        .attest(&nonce)
        .await
        .map_err(|_| "failed to collect dstack attestation")?;
    print_attestation(
        &evidence.settlement_public_key.0,
        &evidence.report_data,
        &evidence.quote,
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
) -> Result<(), &'static str> {
    let output = json!({
        "keyid": SETTLEMENT_KEY_DOMAIN,
        "settlement_pubkey": hex::encode(settlement_public_key),
        "report_data": hex::encode(report_data),
        "quote": hex::encode(quote),
    });
    let encoded = serde_json::to_string(&output).map_err(|_| "failed to encode attestation")?;
    println!("{encoded}");
    Ok(())
}

async fn run(
    route_paths: &[PathBuf],
    pump_count: NonZeroUsize,
    step_timeout_s: u64,
    wait_interval_s: u64,
    age_alert_interval_s: u64,
    age_alert_reminder_s: u64,
) -> ExitCode {
    if age_alert_interval_s == 0 {
        tracing::error!("age alert interval must be positive");
        return ExitCode::FAILURE;
    }
    if age_alert_reminder_s == 0 {
        tracing::error!("age alert reminder interval must be positive");
        return ExitCode::FAILURE;
    }
    let routes = match load_routes(route_paths) {
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
        step_timeout: Duration::from_secs(step_timeout_s),
        wait_interval: Duration::from_secs(wait_interval_s),
        ..PumpConfig::default()
    };
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(value) if !value.is_empty() => value,
        Ok(_) | Err(_) => {
            tracing::error!("DATABASE_URL is required for run");
            return ExitCode::FAILURE;
        }
    };
    let connection_count = match u32::try_from(pump_count.get())
        .ok()
        .and_then(|count| count.checked_add(3))
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
        NonZeroUsize::new(32).unwrap_or(NonZeroUsize::MIN),
        Duration::from_secs(10),
    ) {
        Ok(signer) => signer,
        Err(error) => {
            tracing::error!(%error, "failed to start signer actor");
            return ExitCode::FAILURE;
        }
    };
    let steps = Arc::new(NoopStepSet::build().with_cleared(Box::new(SettleStep::new(
        pool.clone(),
        signer,
        Duration::from_secs(30),
    ))));
    tracing::warn!("placeholder steps remain active outside the cleared state");
    let pump = match Pump::new(pool.clone(), Arc::<StepSet>::clone(&steps), pump_config) {
        Ok(pump) => pump,
        Err(error) => {
            tracing::error!(%error, "invalid pump configuration");
            return ExitCode::FAILURE;
        }
    };
    let cancellation = CancellationToken::new();
    let scanner_count = scanner_routes.len();
    let scanner_pool = pool.clone();
    let scanner_cancellation = cancellation.child_token();
    let scanner_task = tokio::spawn(async move {
        topup::scanner::run(scanner_pool, scanner_routes, scanner_cancellation).await
    });
    let mut pump_tasks = Vec::with_capacity(pump_count.get());
    for worker in 0..pump_count.get() {
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
        Duration::from_secs(age_alert_interval_s),
        Duration::from_secs(age_alert_reminder_s),
    );
    let age_cancellation = cancellation.child_token();
    let age_task = tokio::spawn(async move {
        age_alerter.run(age_cancellation).await;
    });

    tracing::info!(
        pumps = pump_count.get(),
        scanners = scanner_count,
        "topup service started"
    );
    let mut clean_shutdown = true;
    if let Err(error) = wait_for_shutdown_signal().await {
        tracing::error!(%error, "failed to listen for shutdown signal");
        clean_shutdown = false;
    }
    tracing::info!("shutdown requested; finishing in-flight deposit steps");
    cancellation.cancel();

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
    paths
        .iter()
        .map(|path| {
            let yaml = std::fs::read_to_string(path)
                .map_err(|error| format!("failed to read `{}`: {error}", path.display()))?;
            route::parse_and_validate(&yaml, false)
                .map_err(|error| format!("invalid route `{}`: {error}", path.display()))
        })
        .collect()
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
    use super::parse_nonce;

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
}

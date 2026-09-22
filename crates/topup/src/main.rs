//! Command-line entry point for the crypto top-up service.

mod route;

use std::num::NonZeroUsize;
use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use sqlx::postgres::PgPoolOptions;
use tokio_util::sync::CancellationToken;
use topup::pump::{
    AgeAlertConfig, AgeAlerter, NoopStepSet, Pump, PumpConfig, PumpMetrics, StepSet,
};
use topup_core::route::RouteFile;

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
    },
    Migrate,
    Route {
        #[command(subcommand)]
        command: RouteCommand,
    },
    Attest,
    RestoreCheck,
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

    match cli.command {
        TopupCommand::Run {
            routes,
            pumps,
            step_timeout_s,
            wait_interval_s,
            age_alert_interval_s,
        } => {
            return run(
                &routes,
                pumps,
                step_timeout_s,
                wait_interval_s,
                age_alert_interval_s,
            )
            .await;
        }
        TopupCommand::Migrate => return migrate().await,
        TopupCommand::Route {
            command: RouteCommand::Validate { template, file },
        } => return validate_route(&file, template),
        TopupCommand::Attest => not_implemented("attest"),
        TopupCommand::RestoreCheck => not_implemented("restore-check"),
    }

    ExitCode::FAILURE
}

async fn run(
    route_paths: &[PathBuf],
    pump_count: NonZeroUsize,
    step_timeout_s: u64,
    wait_interval_s: u64,
    age_alert_interval_s: u64,
) -> ExitCode {
    if age_alert_interval_s == 0 {
        tracing::error!("age alert interval must be positive");
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
    let steps = Arc::new(NoopStepSet::build());
    let pump = match Pump::new(pool.clone(), Arc::<StepSet>::clone(&steps), pump_config) {
        Ok(pump) => pump,
        Err(error) => {
            tracing::error!(%error, "invalid pump configuration");
            return ExitCode::FAILURE;
        }
    };
    let cancellation = CancellationToken::new();
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
    let age_alerter = AgeAlerter::new(
        pool.clone(),
        age_config,
        Arc::clone(&metrics),
        Duration::from_secs(age_alert_interval_s),
    );
    let age_cancellation = cancellation.child_token();
    let age_task = tokio::spawn(async move {
        age_alerter.run(age_cancellation).await;
    });

    tracing::info!(pumps = pump_count.get(), "topup service started");
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::error!(%error, "failed to listen for shutdown signal");
        cancellation.cancel();
        return ExitCode::FAILURE;
    }
    tracing::info!("shutdown requested; finishing in-flight deposit steps");
    cancellation.cancel();

    let mut clean_shutdown = true;
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

fn not_implemented(command: &'static str) {
    tracing::error!(command, "not implemented");
}

//! Command-line entry point for the crypto top-up service.

mod route;

use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;

use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

#[derive(Parser)]
#[command(name = "topup", version, about = "Crypto top-up service")]
struct Cli {
    #[command(subcommand)]
    command: TopupCommand,
}

#[derive(Subcommand)]
enum TopupCommand {
    Run,
    Migrate,
    Route {
        #[command(subcommand)]
        command: RouteCommand,
    },
    Outbox {
        #[command(subcommand)]
        command: OutboxCommand,
    },
    Attest,
    RestoreCheck,
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

    match cli.command {
        TopupCommand::Run => not_implemented("run"),
        TopupCommand::Migrate => return migrate().await,
        TopupCommand::Route {
            command: RouteCommand::Validate { template, file },
        } => return validate_route(&file, template),
        TopupCommand::Outbox {
            command: OutboxCommand::Replay { id, since, force },
        } => return replay_outbox(id, since.as_deref(), force).await,
        TopupCommand::Attest => not_implemented("attest"),
        TopupCommand::RestoreCheck => not_implemented("restore-check"),
    }

    ExitCode::FAILURE
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

fn not_implemented(command: &'static str) {
    tracing::error!(command, "not implemented");
}

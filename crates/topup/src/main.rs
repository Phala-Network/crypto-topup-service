//! Command-line entry point for the crypto top-up service.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use sqlx::postgres::PgPoolOptions;

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
    Attest,
    RestoreCheck,
}

#[derive(Subcommand)]
enum RouteCommand {
    Validate { file: PathBuf },
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
            command: RouteCommand::Validate { file },
        } => {
            tracing::error!(command = "route validate", file = %file.display(), "not implemented");
        }
        TopupCommand::Attest => not_implemented("attest"),
        TopupCommand::RestoreCheck => not_implemented("restore-check"),
    }

    ExitCode::FAILURE
}

async fn migrate() -> ExitCode {
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(value) if !value.is_empty() => value,
        Ok(_) | Err(_) => {
            tracing::error!("DATABASE_URL is required for migrate");
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

fn not_implemented(command: &'static str) {
    tracing::error!(command, "not implemented");
}

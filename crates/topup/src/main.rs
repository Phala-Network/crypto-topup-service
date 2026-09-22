//! Command-line entry point for the crypto top-up service.

mod route;

use std::path::Path;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use sqlx::postgres::PgPoolOptions;

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
    Attest,
    RestoreCheck,
}

#[derive(Args)]
struct RunArgs {
    /// Socket address on which the API listens.
    #[arg(long, default_value = "127.0.0.1:3000")]
    bind: std::net::SocketAddr,
    /// Attested route file. Repeat for each enabled route.
    #[arg(long = "route", required = true)]
    routes: Vec<PathBuf>,
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
        TopupCommand::Run(args) => return run(&args).await,
        TopupCommand::Migrate => return migrate().await,
        TopupCommand::Route {
            command: RouteCommand::Validate { template, file },
        } => return validate_route(&file, template),
        TopupCommand::Attest => not_implemented("attest"),
        TopupCommand::RestoreCheck => not_implemented("restore-check"),
    }

    ExitCode::FAILURE
}

async fn run(args: &RunArgs) -> ExitCode {
    let database_url = match required_env("DATABASE_URL") {
        Ok(value) => value,
        Err(message) => {
            tracing::error!(%message);
            return ExitCode::FAILURE;
        }
    };
    let admin_kid = match required_env("TOPUP_ADMIN_KID") {
        Ok(value) => value,
        Err(message) => {
            tracing::error!(%message);
            return ExitCode::FAILURE;
        }
    };
    let admin_public_key = match required_env("TOPUP_ADMIN_PUBLIC_KEY") {
        Ok(value) => value,
        Err(message) => {
            tracing::error!(%message);
            return ExitCode::FAILURE;
        }
    };
    let admin_key = match topup::api::VerificationKey::from_base64(admin_kid, &admin_public_key) {
        Ok(key) => key,
        Err(message) => {
            tracing::error!(%message, "invalid administrative verification key");
            return ExitCode::FAILURE;
        }
    };
    let routes = match load_routes(&args.routes) {
        Ok(routes) => routes,
        Err(message) => {
            tracing::error!(%message);
            return ExitCode::FAILURE;
        }
    };
    let pool = match PgPoolOptions::new()
        .max_connections(16)
        .connect(&database_url)
        .await
    {
        Ok(pool) => pool,
        Err(error) => {
            tracing::error!(%error, "failed to connect to database");
            return ExitCode::FAILURE;
        }
    };
    let state = topup::api::AppState {
        pool,
        routes: Arc::new(routes),
        admin_key,
        // C11 will replace this placeholder with the dstack attestation adapter.
        attestor: Arc::new(topup::api::UnavailableAttestor),
    };
    let (application, _) = topup::api::router(state);
    let listener = match tokio::net::TcpListener::bind(args.bind).await {
        Ok(listener) => listener,
        Err(error) => {
            tracing::error!(%error, bind = %args.bind, "failed to bind API listener");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(bind = %args.bind, "API listening");
    if let Err(error) = axum::serve(listener, application).await {
        tracing::error!(%error, "API server failed");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn load_routes(files: &[PathBuf]) -> Result<Vec<topup_core::route::RouteFile>, String> {
    let mut routes = Vec::with_capacity(files.len());
    for file in files {
        let yaml = std::fs::read_to_string(file)
            .map_err(|error| format!("failed to read route file `{}`: {error}", file.display()))?;
        let parsed = route::parse_and_validate(&yaml, false)
            .map_err(|error| format!("route file `{}` is invalid: {error}", file.display()))?;
        if routes
            .iter()
            .any(|existing: &topup_core::route::RouteFile| existing.route == parsed.route)
        {
            return Err(format!("duplicate route `{}`", parsed.route));
        }
        routes.push(parsed);
    }
    Ok(routes)
}

fn required_env(name: &'static str) -> Result<String, String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{name} is required"))
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

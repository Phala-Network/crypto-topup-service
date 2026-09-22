//! Command-line entry point for the crypto top-up service.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

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

fn main() -> ExitCode {
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
        TopupCommand::Migrate => not_implemented("migrate"),
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

fn not_implemented(command: &'static str) {
    tracing::error!(command, "not implemented");
}

//! Command-line entry point for the crypto top-up service.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use topup_core::route::RouteFile;

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
        } => return validate_route(&file),
        TopupCommand::Attest => not_implemented("attest"),
        TopupCommand::RestoreCheck => not_implemented("restore-check"),
    }

    ExitCode::FAILURE
}

fn validate_route(file: &PathBuf) -> ExitCode {
    let yaml = match std::fs::read_to_string(file) {
        Ok(yaml) => yaml,
        Err(error) => {
            eprintln!("failed to read route file `{}`: {error}", file.display());
            return ExitCode::FAILURE;
        }
    };
    match RouteFile::from_yaml(&yaml) {
        Ok(_) => {
            println!("route file `{}` is valid", file.display());
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

//! Command-line entry point for the crypto top-up service.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use serde_json::json;
use topup_adapters::attestation::DstackAttestor;
#[cfg(feature = "dev-signer")]
use topup_adapters::attestation::report_data;
#[cfg(feature = "dev-signer")]
use topup_adapters::signer::DevSigner;
use topup_core::SETTLEMENT_KEY_DOMAIN;
#[cfg(feature = "dev-signer")]
use topup_core::{SecretKey32, Signer as _};

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

    let result = match cli.command {
        TopupCommand::Run => Err("run is not implemented"),
        TopupCommand::Migrate => Err("migrate is not implemented"),
        TopupCommand::Route {
            command: RouteCommand::Validate { file: _ },
        } => Err("route validate is not implemented"),
        TopupCommand::Attest(args) => attest(&args),
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

fn attest(args: &AttestArgs) -> Result<(), &'static str> {
    let nonce = hex::decode(&args.nonce).map_err(|_| "nonce must be valid hexadecimal")?;

    #[cfg(feature = "dev-signer")]
    if args.dev {
        let signer = DevSigner::new(SecretKey32::new([1; 32]), SecretKey32::new([2; 32]));
        let public_key = signer
            .settlement_public_key()
            .map_err(|_| "development settlement key is invalid")?;
        return print_attestation(&public_key.0, &report_data(&nonce, &public_key), &[]);
    }

    let evidence = DstackAttestor::new()
        .attest(&nonce)
        .map_err(|_| "failed to collect dstack attestation")?;
    print_attestation(
        &evidence.settlement_public_key.0,
        &evidence.report_data,
        &evidence.quote,
    )
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

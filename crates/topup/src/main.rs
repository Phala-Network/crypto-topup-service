//! Command-line entry point for the crypto top-up service.

mod route;

use std::path::Path;
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
    Validate {
        /// Permit zero factory and treasury placeholders in deployment templates.
        #[arg(long)]
        template: bool,
        file: PathBuf,
    },
}

#[tokio::main(flavor = "current_thread")]
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
        TopupCommand::Run => Err("run is not implemented"),
        TopupCommand::Migrate => Err("migrate is not implemented"),
        TopupCommand::Route {
            command: RouteCommand::Validate { template, file },
        } => return validate_route(&file, template),
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

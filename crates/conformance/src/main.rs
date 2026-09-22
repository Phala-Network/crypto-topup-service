//! CLI entry point for the product settlement conformance suite.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::Address;
use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand};
use topup_conformance::chain::{ChainFixture, Manifest, PrepareOptions};
use topup_conformance::suite::{Accounts, Caps, CommandRestart, Restart, SuiteConfig};
use topup_conformance::{load_seed, signer_handle, validate_keyid};

#[derive(Debug, Parser)]
#[command(name = "topup-conformance")]
#[command(about = "Run the crypto top-up product settlement conformance suite")]
struct Cli {
    #[command(subcommand)]
    command: Phase,
}

#[derive(Debug, Subcommand)]
enum Phase {
    /// Deploy (or adopt) chain fixtures and write the manifest used to configure the product.
    Prepare(PrepareArgs),
    /// Run every case against a product configured from the manifest.
    Run(Box<RunArgs>),
}

#[derive(Debug, Args)]
struct PrepareArgs {
    /// JSON-RPC URL of a disposable development chain with an unlocked funded account.
    #[arg(long)]
    rpc_url: String,
    /// Expected chain id.
    #[arg(long)]
    chain_id: Option<u64>,
    /// Pre-deployed A1 forwarder factory.
    #[arg(long)]
    factory: Option<Address>,
    /// Pre-deployed approved token exposing `mint(address,uint256)`.
    #[arg(long)]
    asset_contract: Option<Address>,
    /// Pre-deployed second token exposing `mint(address,uint256)`; never approved.
    #[arg(long)]
    unapproved_asset_contract: Option<Address>,
    /// Foundry project used to deploy missing fixtures.
    #[arg(long, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/../../contracts"))]
    contracts_dir: PathBuf,
    #[arg(long, default_value = "conformance-manifest.json")]
    manifest: PathBuf,
}

#[derive(Debug, Args)]
struct RunArgs {
    #[arg(long, default_value = "conformance-manifest.json")]
    manifest: PathBuf,
    #[arg(long)]
    settlement_url: String,
    #[arg(long)]
    signing_key: String,
    #[arg(long)]
    keyid: String,
    #[arg(long)]
    per_deposit_cap: u64,
    #[arg(long)]
    per_period_cap: u64,
    #[arg(long)]
    period_seconds: u64,
    /// Shell command which restarts the product; required for a pass.
    #[arg(long)]
    restart_command: Option<String>,
    #[arg(long, default_value = "conformance-accepted")]
    accepted_account_id: String,
    #[arg(long, default_value = "conformance-refused")]
    refused_account_id: String,
    #[arg(long, default_value = "conformance-processing")]
    processing_account_id: String,
    #[arg(long, default_value = "conformance-period")]
    period_account_id: String,
    #[arg(long, default_value = "conformance-cap")]
    cap_account_id: String,
    /// How long to keep resending an unchanged request that is not yet settled.
    #[arg(long, default_value_t = 30)]
    resend_window_seconds: u64,
    #[arg(long, default_value = "conformance-report.json")]
    report: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Phase::Prepare(args) => prepare(args).await,
        Phase::Run(args) => run(*args).await,
    }
}

async fn prepare(args: PrepareArgs) -> Result<()> {
    let manifest = topup_conformance::chain::prepare(
        &args.rpc_url,
        PrepareOptions {
            chain_id: args.chain_id,
            factory: args.factory,
            asset_contract: args.asset_contract,
            unapproved_asset_contract: args.unapproved_asset_contract,
            contracts_dir: args.contracts_dir,
        },
    )
    .await?;
    manifest.write(&args.manifest)?;
    println!("{}", serde_json::to_string_pretty(&manifest)?);
    eprintln!(
        "manifest written to {}; configure the product from it, then run the suite",
        args.manifest.display()
    );
    Ok(())
}

async fn run(args: RunArgs) -> Result<()> {
    validate_keyid(&args.keyid)?;
    let caps = Caps {
        per_deposit: args.per_deposit_cap,
        per_period: args.per_period_cap,
        period: Duration::from_secs(args.period_seconds),
    };
    caps.validate()?;
    let manifest = Manifest::read(&args.manifest)?;
    let chain = ChainFixture::connect(manifest).await?;
    let signer = signer_handle(load_seed(&args.signing_key)?)?;
    let restart = args
        .restart_command
        .map(|command| Arc::new(CommandRestart::new(command)) as Arc<dyn Restart>);
    let report = topup_conformance::suite::run(SuiteConfig {
        settlement_url: args.settlement_url,
        signer,
        keyid: args.keyid,
        caps,
        accounts: Accounts {
            accepted: args.accepted_account_id,
            refused: args.refused_account_id,
            processing: args.processing_account_id,
            period: args.period_account_id,
            cap: args.cap_account_id,
        },
        chain,
        restart,
        resend_window: Duration::from_secs(args.resend_window_seconds),
    })
    .await?;
    let bytes = serde_json::to_vec_pretty(&report)?;
    std::fs::write(&args.report, &bytes)
        .with_context(|| format!("write report {}", args.report.display()))?;
    println!("{}", String::from_utf8(bytes)?);
    ensure!(
        report.passed,
        "conformance suite did not pass: {} failed, {} incomplete",
        report.summary.failed,
        report.summary.incomplete
    );
    Ok(())
}

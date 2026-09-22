//! CLI entry point for the product settlement conformance suite.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use clap::Parser;
use topup_conformance::suite::{AnvilEvidence, EvidenceProvider, SuiteConfig, SyntheticEvidence};
use topup_conformance::{load_seed, signer_handle, validate_keyid};

#[derive(Debug, Parser)]
#[command(name = "topup-conformance")]
#[command(about = "Run the crypto top-up product settlement conformance suite")]
struct Args {
    #[arg(long)]
    settlement_url: String,
    #[arg(long)]
    signing_key: String,
    #[arg(long)]
    keyid: String,
    #[arg(long)]
    per_deposit_cap: u64,
    #[arg(long)]
    anvil_rpc: Option<String>,
    #[arg(long, default_value_t = 31_337)]
    chain_id: u64,
    #[arg(long, default_value = "conformance-accepted")]
    accepted_account_id: String,
    #[arg(long, default_value = "conformance-refused")]
    refused_account_id: String,
    #[arg(long, default_value = "conformance-processing")]
    processing_account_id: String,
    #[arg(long, default_value = "conformance-report.json")]
    report: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    validate_keyid(&args.keyid)?;
    ensure!(
        args.per_deposit_cap > 200,
        "--per-deposit-cap must exceed the suite's baseline amounts"
    );
    ensure!(
        args.per_deposit_cap < u64::MAX,
        "--per-deposit-cap must be less than u64::MAX"
    );
    let seed = load_seed(&args.signing_key)?;
    let signer = signer_handle(seed)?;
    let check_chain_evidence = args.anvil_rpc.is_some();
    let evidence: Arc<dyn EvidenceProvider> = match &args.anvil_rpc {
        Some(rpc) => Arc::new(AnvilEvidence::prepare(rpc.clone(), args.chain_id)?),
        None => Arc::new(SyntheticEvidence::new(args.chain_id)),
    };
    let report = topup_conformance::suite::run(SuiteConfig {
        settlement_url: args.settlement_url,
        signer,
        keyid: args.keyid,
        per_deposit_cap: args.per_deposit_cap,
        accepted_account_id: args.accepted_account_id,
        refused_account_id: args.refused_account_id,
        processing_account_id: args.processing_account_id,
        evidence,
        check_chain_evidence,
    })
    .await?;
    let bytes = serde_json::to_vec_pretty(&report)?;
    std::fs::write(&args.report, &bytes)
        .with_context(|| format!("write report {}", args.report.display()))?;
    println!("{}", String::from_utf8(bytes)?);
    ensure!(report.passed, "conformance suite failed");
    Ok(())
}

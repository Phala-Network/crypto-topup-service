//! CLI entry point for the reference product settlement endpoint.

use std::net::SocketAddr;
use std::str::FromStr;

use alloy_primitives::Address;
use anyhow::{Context, Result};
use clap::Parser;
use ed25519_dalek::SigningKey;
use topup_conformance::load_seed;
use topup_conformance::reference::{
    BrokenVariant, EvidencePolicy, ReferenceConfig, ReferenceState, RpcEvidenceConfig, router,
};
use topup_conformance::validate_keyid;

#[derive(Debug, Parser)]
#[command(name = "topup-conformance-reference")]
#[command(about = "Run the E1 reference product settlement endpoint")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8089")]
    listen: SocketAddr,
    #[arg(long, default_value = "dev")]
    signing_key: String,
    #[arg(long, default_value = "settlement/v1")]
    keyid: String,
    #[arg(long, default_value_t = 10_000)]
    per_deposit_cap: u64,
    #[arg(long, default_value = "conformance-refused")]
    refused_account_id: String,
    #[arg(long, default_value = "conformance-processing")]
    processing_account_id: String,
    #[arg(long, default_value = "none")]
    broken: String,
    #[arg(long)]
    anvil_rpc: Option<String>,
    #[arg(long, default_value_t = 31_337)]
    chain_id: u64,
    #[arg(long, requires = "anvil_rpc")]
    asset_contract: Option<String>,
    #[arg(long, requires = "anvil_rpc")]
    factory: Option<String>,
    #[arg(long, requires = "anvil_rpc")]
    implementation: Option<String>,
    #[cfg(feature = "postgres")]
    #[arg(long)]
    database_url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    validate_keyid(&args.keyid)?;
    let seed = load_seed(&args.signing_key)?;
    let broken = BrokenVariant::from_str(&args.broken)?;
    let evidence_policy = match args.anvil_rpc {
        Some(rpc_url) => EvidencePolicy::Rpc(RpcEvidenceConfig {
            rpc_url,
            chain_id: args.chain_id,
            asset_contract: Address::from_str(
                args.asset_contract
                    .as_deref()
                    .context("--asset-contract is required")?,
            )?,
            factory: Address::from_str(args.factory.as_deref().context("--factory is required")?)?,
            implementation: Address::from_str(
                args.implementation
                    .as_deref()
                    .context("--implementation is required")?,
            )?,
        }),
        None => EvidencePolicy::Synthetic,
    };
    let config = ReferenceConfig {
        verifying_key: SigningKey::from_bytes(&seed).verifying_key(),
        keyid: args.keyid,
        per_deposit_cap: args.per_deposit_cap,
        refused_account_id: args.refused_account_id,
        processing_account_id: args.processing_account_id,
        broken,
        evidence_policy,
    };
    #[cfg(feature = "postgres")]
    let state = match args.database_url {
        Some(database_url) => {
            let pool = sqlx::PgPool::connect(&database_url).await?;
            ReferenceState::new_postgres(config, pool).await?
        }
        None => ReferenceState::new(config),
    };
    #[cfg(not(feature = "postgres"))]
    let state = ReferenceState::new(config);
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .with_context(|| format!("bind reference endpoint at {}", args.listen))?;
    eprintln!(
        "reference settlement endpoint: http://{}/settlements",
        listener.local_addr()?
    );
    axum::serve(listener, router(state)).await?;
    Ok(())
}

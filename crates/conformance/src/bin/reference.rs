//! CLI entry point for the reference product settlement endpoint.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use ed25519_dalek::SigningKey;
use topup_conformance::chain::Manifest;
use topup_conformance::reference::{BrokenVariant, ReferenceConfig, ReferenceState, router};
use topup_conformance::{load_seed, validate_keyid};

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
    #[arg(long, default_value_t = 50_000)]
    per_period_cap: u64,
    #[arg(long, default_value_t = 86_400)]
    period_seconds: u64,
    #[arg(long, default_value = "conformance-refused")]
    refused_account_id: String,
    #[arg(long, default_value = "conformance-processing")]
    processing_account_id: String,
    #[arg(long, default_value = "none")]
    broken: String,
    /// Manifest written by `topup-conformance prepare`.
    #[arg(long, default_value = "conformance-manifest.json")]
    manifest: PathBuf,
    /// The reference's own RPC URL for the manifest chain; defaults to the manifest's.
    #[arg(long)]
    rpc_url: Option<String>,
    #[cfg(feature = "postgres")]
    #[arg(long)]
    database_url: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    validate_keyid(&args.keyid)?;
    let seed = load_seed(&args.signing_key)?;
    let mut manifest = Manifest::read(&args.manifest)?;
    if let Some(rpc_url) = args.rpc_url {
        manifest.rpc_url = rpc_url;
    }
    let config = ReferenceConfig {
        verifying_key: SigningKey::from_bytes(&seed).verifying_key(),
        keyid: args.keyid,
        per_deposit_cap: args.per_deposit_cap,
        per_period_cap: args.per_period_cap,
        period: Duration::from_secs(args.period_seconds),
        refused_account_id: args.refused_account_id,
        processing_account_id: args.processing_account_id,
        broken: BrokenVariant::from_str(&args.broken)?,
        manifest,
    };
    #[cfg(feature = "postgres")]
    let state = match args.database_url {
        Some(database_url) => {
            let pool = sqlx::PgPool::connect(&database_url).await?;
            ReferenceState::new_postgres(config, pool).await?
        }
        None => ReferenceState::new(config)?,
    };
    #[cfg(not(feature = "postgres"))]
    let state = ReferenceState::new(config)?;
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

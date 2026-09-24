//! Key-domain selection by the dstack signer against a recording guest-agent stub.

use std::num::{NonZeroU32, NonZeroUsize};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alloy_consensus::TxEnvelope;
use alloy_consensus::transaction::SignerRecoverable as _;
use alloy_eips::eip2718::Decodable2718 as _;
use alloy_primitives::{Address, Bytes, U256};
use alloy_signer_local::PrivateKeySigner;
use anyhow::{Context as _, Result};
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use topup_adapters::signer::actor::SignerHandle;
use topup_adapters::signer::dstack::DstackSigner;
use topup_core::{Signer as _, TxRequest};

type Requests = Arc<Mutex<Vec<String>>>;

/// Serves the dstack 0.5 `/GetKey` with a key derived from the requested path and records each path.
struct GuestAgent {
    endpoint: String,
    requests: Requests,
    task: tokio::task::JoinHandle<()>,
}

impl GuestAgent {
    async fn start() -> Result<Self> {
        let requests = Requests::default();
        let app = Router::new()
            .route("/GetKey", post(get_key))
            .with_state(Arc::clone(&requests));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self {
            endpoint,
            requests,
            task,
        })
    }

    fn take_requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .map(|mut requests| std::mem::take(&mut *requests))
            .unwrap_or_default()
    }
}

impl Drop for GuestAgent {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn stub_key(domain: &str) -> [u8; 32] {
    Sha256::digest(domain.as_bytes()).into()
}

fn stub_operator(domain: &str) -> Result<Address> {
    Ok(PrivateKeySigner::from_slice(&stub_key(domain))?.address())
}

async fn get_key(State(requests): State<Requests>, Json(body): Json<Value>) -> Json<Value> {
    let path = body
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let key = stub_key(&path);
    if let Ok(mut requests) = requests.lock() {
        requests.push(path);
    }
    Json(json!({
        "key": hex::encode(key),
        "signature_chain": [],
    }))
}

fn request(chain_id: u64) -> TxRequest {
    TxRequest {
        chain_id,
        nonce: 0,
        to: Address::from([3; 20]),
        value: U256::ZERO,
        data: Bytes::new(),
        gas_limit: 21_000,
        max_fee_per_gas: 2,
        max_priority_fee_per_gas: 1,
    }
}

#[tokio::test]
async fn operator_domain_follows_the_configured_version_only() -> Result<()> {
    let agent = GuestAgent::start().await?;
    let version = NonZeroU32::new(2).context("two is non-zero")?;
    let signer =
        DstackSigner::with_endpoint(agent.endpoint.clone()).with_operator_key_version(version);
    let handle = SignerHandle::spawn(
        signer.clone(),
        NonZeroUsize::new(4).context("queue capacity is non-zero")?,
        Duration::from_secs(5),
    )?;

    let operator = handle.operator_address().await?;
    assert_eq!(operator, stub_operator("operator/v2")?);
    assert_ne!(operator, stub_operator("operator/v1")?);
    let signed = handle.sign_operator_tx(request(1)).await?;
    let envelope = TxEnvelope::decode_2718_exact(&signed.raw_signed_bytes)?;
    assert_eq!(envelope.recover_signer()?, operator);
    handle.settlement_public_key().await?;
    handle.sign_settlement(b"payload").await?;
    signer.derive_backup_key().await?;
    assert_eq!(
        agent.take_requests(),
        vec![
            "operator/v2",
            "operator/v2",
            "settlement/v1",
            "settlement/v1",
            "backup/v1",
        ]
    );

    let default = DstackSigner::with_endpoint(agent.endpoint.clone());
    assert_eq!(
        default.operator_address().await?,
        stub_operator("operator/v1")?
    );
    assert_eq!(agent.take_requests(), vec!["operator/v1"]);
    Ok(())
}

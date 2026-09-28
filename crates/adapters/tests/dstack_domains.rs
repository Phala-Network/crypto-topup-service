//! Key-domain selection by the dstack signer against a recording guest-agent stub.

use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result};
use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use topup_adapters::signer::actor::SignerHandle;
use topup_adapters::signer::dstack::DstackSigner;
use topup_core::Signer as _;

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

#[tokio::test]
async fn signer_requests_only_its_fixed_domains() -> Result<()> {
    let agent = GuestAgent::start().await?;
    let signer = DstackSigner::with_endpoint(agent.endpoint.clone());
    let handle = SignerHandle::spawn(
        signer.clone(),
        NonZeroUsize::new(4).context("queue capacity is non-zero")?,
        Duration::from_secs(5),
    )?;

    handle.settlement_public_key().await?;
    handle.sign_settlement(b"payload").await?;
    signer.derive_backup_key().await?;
    assert_eq!(
        agent.take_requests(),
        vec!["settlement/v1", "settlement/v1", "backup/v1"]
    );
    Ok(())
}

/// Staging's backup prefix was written by the versioned `backup/v{n}` derivation at version 1, so
/// the single backup key must request exactly that path: dstack derives a key from the app key and
/// the path alone, and the existing backups then stay restorable.
#[tokio::test]
async fn backup_key_is_the_version_one_derivation_of_existing_backups() -> Result<()> {
    let agent = GuestAgent::start().await?;
    let signer = DstackSigner::with_endpoint(agent.endpoint.clone());

    let key = signer.derive_backup_key().await?;
    // The removed `derive_backup_key_version(version)` requested `format!("backup/v{version}")`.
    let version = 1;
    let version_one = signer.derive_secret(&format!("backup/v{version}")).await?;
    assert_eq!(key.expose_secret(), version_one.expose_secret());
    assert_eq!(key.expose_secret(), &stub_key("backup/v1"));
    assert_eq!(agent.take_requests(), vec!["backup/v1", "backup/v1"]);
    Ok(())
}

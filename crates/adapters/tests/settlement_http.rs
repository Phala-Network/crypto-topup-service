//! Reference product tests for the RFC 9421 settlement client.

#![allow(clippy::indexing_slicing)]

use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use serde_json::json;
use tokio::sync::Mutex;
use topup_adapters::settlement::http::{
    SettlementAnswer, SettlementApi as _, SettlementClient, SettlementRequest,
};
use topup_adapters::signer::actor::SignerHandle;
use topup_core::{
    Ed25519PublicKey, Ed25519Signature, SignedTx, Signer as CoreSigner, SignerError, TxRequest,
};

#[derive(Clone)]
struct TestSigner(SigningKey);

impl CoreSigner for TestSigner {
    async fn sign_operator_tx(&self, _tx: TxRequest) -> Result<SignedTx, SignerError> {
        Err(SignerError::SigningFailed)
    }

    async fn sign_settlement(&self, payload: &[u8]) -> Result<Ed25519Signature, SignerError> {
        Ok(Ed25519Signature(self.0.sign(payload).to_bytes()))
    }

    async fn operator_address(&self) -> Result<alloy_primitives::Address, SignerError> {
        Err(SignerError::KeyUnavailable)
    }

    async fn settlement_public_key(&self) -> Result<Ed25519PublicKey, SignerError> {
        Ok(Ed25519PublicKey(self.0.verifying_key().to_bytes()))
    }
}

#[derive(Clone)]
struct Received {
    method: String,
    path: String,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Received {
    fn created(&self) -> Option<i64> {
        let value = self.headers.get("signature-input")?.to_str().ok()?;
        let value = value.split(";created=").nth(1)?.split(';').next()?;
        value.parse().ok()
    }
}

#[derive(Clone)]
struct Plan {
    status: StatusCode,
    body: &'static str,
}

#[derive(Clone)]
struct ProductState {
    verifying_key: VerifyingKey,
    plans: Arc<Mutex<VecDeque<Plan>>>,
    received: Arc<Mutex<Vec<Received>>>,
}

struct ProductServer {
    url: String,
    received: Arc<Mutex<Vec<Received>>>,
    task: tokio::task::JoinHandle<()>,
}

impl ProductServer {
    async fn start(plans: Vec<Plan>) -> anyhow::Result<(Self, SignerHandle)> {
        let signer = TestSigner(SigningKey::from_bytes(&[7; 32]));
        let verifying_key = signer.0.verifying_key();
        let signer = SignerHandle::spawn(
            signer,
            NonZeroUsize::new(4).unwrap_or(NonZeroUsize::MIN),
            Duration::from_secs(1),
        )?;
        let received = Arc::new(Mutex::new(Vec::new()));
        let state = ProductState {
            verifying_key,
            plans: Arc::new(Mutex::new(plans.into_iter().collect())),
            received: Arc::clone(&received),
        };
        let app = Router::new()
            .route("/settlements", post(handle_post))
            .route("/settlements/{key}", get(handle_get))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok((
            Self {
                url: format!("http://{address}/settlements"),
                received,
                task,
            },
            signer,
        ))
    }

    async fn received(&self) -> Vec<Received> {
        self.received.lock().await.clone()
    }

    async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
    }
}

async fn handle_post(
    State(state): State<ProductState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle(state, "POST", "/settlements".to_owned(), headers, body).await
}

async fn handle_get(
    State(state): State<ProductState>,
    Path(key): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    handle(state, "GET", format!("/settlements/{key}"), headers, body).await
}

async fn handle(
    state: ProductState,
    method: &str,
    path: String,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if verify(&state.verifying_key, method, &path, &headers, &body).is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    state.received.lock().await.push(Received {
        method: method.to_owned(),
        path,
        headers,
        body: body.to_vec(),
    });
    let plan = state.plans.lock().await.pop_front().unwrap_or(Plan {
        status: StatusCode::NOT_FOUND,
        body: "",
    });
    (plan.status, plan.body).into_response()
}

fn verify(
    key: &VerifyingKey,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), ()> {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use sha2::{Digest, Sha256};

    let content_digest = header(headers, "content-digest")?;
    let expected_digest = format!("sha-256=:{}:", STANDARD.encode(Sha256::digest(body)));
    if content_digest != expected_digest {
        return Err(());
    }
    let idempotency_key = header(headers, "idempotency-key")?;
    if idempotency_key != "\"deposit:test\"" {
        return Err(());
    }
    let signature_input = header(headers, "signature-input")?;
    let parameters = signature_input.strip_prefix("sig1=").ok_or(())?;
    if !parameters.starts_with(
        "(\"@method\" \"@target-uri\" \"content-digest\" \"idempotency-key\");created=",
    ) || !parameters.ends_with(";keyid=\"settlement/v1\"")
    {
        return Err(());
    }
    let host = header(headers, "host")?;
    let target_uri = format!("http://{host}{path}");
    let base = format!(
        "\"@method\": {method}\n\"@target-uri\": {target_uri}\n\"content-digest\": {content_digest}\n\"idempotency-key\": {idempotency_key}\n\"@signature-params\": {parameters}"
    );
    let encoded = header(headers, "signature")?
        .strip_prefix("sig1=:")
        .and_then(|value| value.strip_suffix(':'))
        .ok_or(())?;
    let signature = STANDARD.decode(encoded).map_err(|_| ())?;
    let signature = Signature::from_slice(&signature).map_err(|_| ())?;
    key.verify(base.as_bytes(), &signature).map_err(|_| ())
}

fn header<'a>(headers: &'a HeaderMap, name: &'static str) -> Result<&'a str, ()> {
    headers.get(name).ok_or(())?.to_str().map_err(|_| ())
}

fn request() -> SettlementRequest {
    SettlementRequest {
        idempotency_key: "deposit:test".to_owned(),
        payload: json!({"version": 1, "idempotency_key": "deposit:test"}),
    }
}

#[tokio::test]
async fn maps_every_contract_response_and_signs_post_and_get() -> anyhow::Result<()> {
    let cases = [
        (
            Plan {
                status: StatusCode::OK,
                body: r#"{"status":"accepted","destination_tx_id":"credit-1"}"#,
            },
            SettlementAnswer::Accepted {
                destination_tx_id: "credit-1".to_owned(),
                payload: request().payload,
            },
        ),
        (
            Plan {
                status: StatusCode::OK,
                body: r#"{"status":"processing"}"#,
            },
            SettlementAnswer::Processing {
                payload: request().payload,
            },
        ),
        (
            Plan {
                status: StatusCode::OK,
                body: r#"{"status":"rejected","reason":"cap"}"#,
            },
            SettlementAnswer::Rejected {
                reason: "cap".to_owned(),
                payload: request().payload,
            },
        ),
        (
            Plan {
                status: StatusCode::CONFLICT,
                body: "",
            },
            SettlementAnswer::Conflict409,
        ),
        (
            Plan {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                body: "",
            },
            SettlementAnswer::PayloadMismatch422,
        ),
        (
            Plan {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                body: "broken",
            },
            SettlementAnswer::Unknown {
                status: 500,
                body: "broken".to_owned(),
            },
        ),
    ];

    for (plan, expected) in cases {
        let (server, signer) = ProductServer::start(vec![plan]).await?;
        let client = SettlementClient::new(&server.url, signer, Duration::from_secs(1))?;
        assert_eq!(client.post(&request()).await?, expected);
        let received = server.received().await;
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].method, "POST");
        assert_eq!(received[0].path, "/settlements");
        assert!(!received[0].body.is_empty());
        server.stop().await;
    }

    let (server, signer) = ProductServer::start(vec![Plan {
        status: StatusCode::NOT_FOUND,
        body: "",
    }])
    .await?;
    let client = SettlementClient::new(&server.url, signer, Duration::from_secs(1))?;
    assert_eq!(client.get_by_key("deposit:test").await?, None);
    let received = server.received().await;
    assert_eq!(received[0].method, "GET");
    assert_eq!(received[0].path, "/settlements/deposit:test");
    assert!(received[0].body.is_empty());
    assert!(received[0].headers.get("content-type").is_none());
    server.stop().await;

    let (server, signer) = ProductServer::start(vec![Plan {
        status: StatusCode::OK,
        body: r#"{"status":"accepted","destination_tx_id":"credit-get","payload":{"version":1,"idempotency_key":"deposit:test","amount_minor":"999"}}"#,
    }])
    .await?;
    let client = SettlementClient::new(&server.url, signer, Duration::from_secs(1))?;
    assert_eq!(
        client.get_by_key("deposit:test").await?,
        Some(SettlementAnswer::Accepted {
            destination_tx_id: "credit-get".to_owned(),
            payload: json!({
                "version": 1,
                "idempotency_key": "deposit:test",
                "amount_minor": "999",
            }),
        })
    );
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn retries_keep_body_identical_and_refresh_created() -> anyhow::Result<()> {
    let (server, signer) = ProductServer::start(vec![
        Plan {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: "unknown",
        },
        Plan {
            status: StatusCode::OK,
            body: r#"{"status":"accepted","destination_tx_id":"credit-2"}"#,
        },
    ])
    .await?;
    let client = SettlementClient::new(&server.url, signer, Duration::from_secs(1))?;
    assert!(matches!(
        client.post(&request()).await?,
        SettlementAnswer::Unknown { status: 500, .. }
    ));
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(matches!(
        client.post(&request()).await?,
        SettlementAnswer::Accepted { .. }
    ));
    let received = server.received().await;
    assert_eq!(received.len(), 2);
    assert_eq!(received[0].body, received[1].body);
    assert_ne!(received[0].created(), received[1].created());
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn caps_unknown_response_bodies() -> anyhow::Result<()> {
    let oversized = Box::leak("x".repeat(70 * 1024).into_boxed_str());
    let (server, signer) = ProductServer::start(vec![Plan {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        body: oversized,
    }])
    .await?;
    let client = SettlementClient::new(&server.url, signer, Duration::from_secs(1))?;
    let SettlementAnswer::Unknown { status, body } = client.post(&request()).await? else {
        anyhow::bail!("oversized response must be unknown");
    };
    assert_eq!(status, 500);
    assert!(body.len() < 5 * 1024);
    assert!(body.ends_with("[response body exceeded 65536 bytes]"));
    server.stop().await;
    Ok(())
}

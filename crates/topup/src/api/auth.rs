//! RFC 9421 HTTP Message Signatures verification for inbound requests.

use super::AppState;
use super::error::ApiError;
use super::repository;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use ed25519_dalek::VerifyingKey;
use topup_adapters::http_signature::{self, PublicOrigin, SignedMessage};

const MAX_SIGNED_BODY_BYTES: usize = 1_048_576;
const IDEMPOTENCY_HEADER: &str = "idempotency-key";

/// A configured RFC 9421 ed25519 verification key.
#[derive(Clone, Debug)]
pub struct VerificationKey {
    /// Key identifier required in `Signature-Input`.
    pub kid: String,
    key: VerifyingKey,
}

impl VerificationKey {
    /// Parses a standard-base64 raw ed25519 public key.
    pub fn from_base64(kid: String, encoded: &str) -> Result<Self, &'static str> {
        let decoded = STANDARD
            .decode(encoded)
            .map_err(|_| "public key must be standard base64")?;
        let bytes: [u8; 32] = decoded
            .try_into()
            .map_err(|_| "public key must contain 32 bytes")?;
        let key = VerifyingKey::from_bytes(&bytes)
            .map_err(|_| "public key is not a valid ed25519 key")?;
        Ok(Self { kid, key })
    }
}

/// Authenticates a product request and attaches its product identifier.
pub async fn authenticate_product(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(product_slug) = product_slug_from_path(request.uri().path()) else {
        return ApiError::unauthorized().into_response();
    };
    let product = match repository::find_product_by_slug(&state.pool, product_slug).await {
        Ok(Some(product)) => product,
        Ok(None) => return ApiError::unauthorized().into_response(),
        Err(error) => return error.into_response(),
    };
    // The key id is attested in the route; a product no loaded route names cannot authenticate.
    let Some(kid) = state
        .routes
        .destination(&product.slug)
        .map(|destination| destination.product_kid.clone())
    else {
        return ApiError::unauthorized().into_response();
    };
    let key = match VerificationKey::from_base64(kid, &product.pubkey) {
        Ok(key) => key,
        Err(message) => {
            tracing::error!(product_id = %product.id, %message, "stored product key is invalid");
            return ApiError::unauthorized().into_response();
        }
    };
    let verified = match verify_request(&mut request, &state.public_origin, &key).await {
        Ok(verified) => verified,
        Err(()) => return ApiError::unauthorized().into_response(),
    };
    if let Err(error) = repository::record_signature(&state.pool, &verified).await {
        return error.into_response();
    }
    request.extensions_mut().insert(product);
    next.run(request).await
}

/// Authenticates an administrative request with the separately configured key.
pub async fn authenticate_admin(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let verified = match verify_request(&mut request, &state.public_origin, &state.admin_key).await
    {
        Ok(verified) => verified,
        Err(()) => return ApiError::unauthorized().into_response(),
    };
    if let Err(error) = repository::record_signature(&state.pool, &verified).await {
        return error.into_response();
    }
    next.run(request).await
}

/// A verified request signature ready for single-use persistence.
pub(crate) struct VerifiedSignature {
    pub(crate) kid: String,
    pub(crate) signature_hash: [u8; 32],
    pub(crate) created: DateTime<Utc>,
}

async fn verify_request(
    request: &mut Request,
    public_origin: &PublicOrigin,
    key: &VerificationKey,
) -> Result<VerifiedSignature, ()> {
    let body = std::mem::replace(request.body_mut(), Body::empty());
    let bytes = to_bytes(body, MAX_SIGNED_BODY_BYTES)
        .await
        .map_err(|_| ())?;
    *request.body_mut() = Body::from(bytes.clone());

    // `Host` and `X-Forwarded-*` describe the gateway hop, so the configured origin is used.
    let target_uri = public_origin.target_uri(
        request
            .uri()
            .path_and_query()
            .map_or("/", |value| value.as_str()),
    );
    let headers = request.headers();
    let idempotency_key = headers
        .get(IDEMPOTENCY_HEADER)
        .map(|value| value.to_str().map_err(|_| ()))
        .transpose()?;
    let verified = http_signature::verify(
        &SignedMessage {
            method: request.method().as_str(),
            target_uri: &target_uri,
            content_digest: header_value(headers, "content-digest")?,
            idempotency_key,
            signature_input: header_value(headers, "signature-input")?,
            signature: header_value(headers, "signature")?,
            body: &bytes,
        },
        &key.kid,
        &key.key,
        Utc::now().timestamp(),
    )
    .map_err(|_| ())?;
    Ok(VerifiedSignature {
        kid: verified.keyid,
        signature_hash: verified.signature_hash,
        created: DateTime::from_timestamp(verified.created, 0).ok_or(())?,
    })
}

fn header_value<'a>(headers: &'a HeaderMap, name: &'static str) -> Result<&'a str, ()> {
    headers
        .get(name)
        .ok_or(())?
        .to_str()
        .map(str::trim)
        .map_err(|_| ())
}

fn product_slug_from_path(path: &str) -> Option<&str> {
    let mut segments = path.trim_start_matches('/').split('/');
    match (segments.next(), segments.next(), segments.next()) {
        (Some("v1"), Some("products"), Some(product)) if !product.is_empty() => Some(product),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::num::NonZeroUsize;
    use std::time::Duration;

    use alloy_primitives::Address;
    use ed25519_dalek::{Signer as _, SigningKey};
    use serde_json::json;
    use topup_adapters::settlement::http::{SettlementClient, SettlementRequest};
    use topup_adapters::signer::actor::SignerHandle;
    use topup_core::{
        Ed25519PublicKey, Ed25519Signature, SETTLEMENT_KEY_DOMAIN, SignedTx, Signer, SignerError,
        TxRequest,
    };

    use super::*;

    struct TestSigner(SigningKey);

    impl Signer for TestSigner {
        async fn sign_operator_tx(&self, _tx: TxRequest) -> Result<SignedTx, SignerError> {
            Err(SignerError::SigningFailed)
        }

        async fn sign_settlement(&self, payload: &[u8]) -> Result<Ed25519Signature, SignerError> {
            Ok(Ed25519Signature(self.0.sign(payload).to_bytes()))
        }

        async fn operator_address(&self) -> Result<Address, SignerError> {
            Err(SignerError::KeyUnavailable)
        }

        async fn settlement_public_key(&self) -> Result<Ed25519PublicKey, SignerError> {
            Ok(Ed25519PublicKey(self.0.verifying_key().to_bytes()))
        }
    }

    #[tokio::test]
    async fn settlement_client_signature_verifies_without_network() -> Result<(), Box<dyn Error>> {
        let signing_key = SigningKey::from_bytes(&[11; 32]);
        let encoded_key = STANDARD.encode(signing_key.verifying_key().as_bytes());
        let key = VerificationKey::from_base64(SETTLEMENT_KEY_DOMAIN.to_owned(), &encoded_key)?;
        let signer = SignerHandle::spawn(
            TestSigner(signing_key),
            NonZeroUsize::new(2).ok_or("queue capacity")?,
            Duration::from_secs(1),
        )?;
        let client = SettlementClient::new(
            "http://api.test/settlements",
            signer,
            Duration::from_secs(1),
        )?;
        let signed = client
            .signed_post_request(&SettlementRequest {
                idempotency_key: "deposit:test".to_owned(),
                payload: json!({"version": 1, "idempotency_key": "deposit:test"}),
            })
            .await?;
        let body = signed
            .body()
            .and_then(reqwest::Body::as_bytes)
            .ok_or("signed request body must be buffered")?
            .to_vec();
        let mut request = Request::builder()
            .method(signed.method().clone())
            .uri(signed.url().as_str())
            .body(Body::from(body))?;
        *request.headers_mut() = signed.headers().clone();

        verify_request(&mut request, &PublicOrigin::parse("http://api.test")?, &key)
            .await
            .map_err(|()| "settlement signature must verify")?;
        Ok(())
    }

    fn signed_message<'a>(
        vector: &'a serde_json::Value,
        target_uri: &'a str,
        body: &'a [u8],
    ) -> SignedMessage<'a> {
        let header = |name: &str| vector["headers"][name].as_str();
        SignedMessage {
            method: vector["method"].as_str().unwrap_or_default(),
            target_uri,
            content_digest: header("content-digest").unwrap_or_default(),
            idempotency_key: header("idempotency-key"),
            signature_input: header("signature-input").unwrap_or_default(),
            signature: header("signature").unwrap_or_default(),
            body,
        }
    }

    /// Requests signed by the Python SDK (`sdk/python/tests/vectors.py`) verify with the shared
    /// verifier, and this module rebuilds the same `@target-uri` from the configured origin and
    /// the request target.
    #[test]
    fn python_sdk_signatures_verify() -> Result<(), Box<dyn Error>> {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/rfc9421-python-signer.json"
        ))?;
        let key = VerificationKey::from_base64(
            fixture["keyid"].as_str().ok_or("keyid")?.to_owned(),
            fixture["public_key"].as_str().ok_or("public_key")?,
        )?;
        let other_key = SigningKey::from_bytes(&[9; 32]).verifying_key();
        let created = fixture["created"].as_i64().ok_or("created")?;
        let vectors = fixture["vectors"].as_array().ok_or("vectors")?;
        let origin = PublicOrigin::parse("http://127.0.0.1:18080")?;
        assert_eq!(vectors.len(), 5);
        assert!(
            vectors
                .iter()
                .any(|vector| vector["headers"]["signature-input"]
                    .as_str()
                    .is_some_and(|input| input.contains(";nonce=\""))),
            "the fixture must cover the nonce parameter"
        );

        for vector in vectors {
            let name = vector["name"].as_str().ok_or("name")?;
            let target_uri = origin.target_uri(vector["target"].as_str().ok_or("target")?);
            assert_eq!(
                Some(target_uri.as_str()),
                vector["target_uri"].as_str(),
                "{name}"
            );
            let body = vector["body"].as_str().ok_or("body")?.as_bytes();
            let message = |body| signed_message(vector, &target_uri, body);

            let verified =
                http_signature::verify(&message(body), &key.kid, &key.key, created + 300)
                    .map_err(|_| format!("{name}: Python signature must verify"))?;
            assert_eq!(verified.keyid, "sdk-vector/v1", "{name}");
            assert_eq!(verified.created, created, "{name}");

            assert!(
                http_signature::verify(&message(body), &key.kid, &key.key, created + 301).is_err(),
                "{name}: stale signature must fail"
            );
            assert!(
                http_signature::verify(&message(b"altered"), &key.kid, &key.key, created).is_err(),
                "{name}: altered body must fail"
            );
            assert!(
                http_signature::verify(&message(body), &key.kid, &other_key, created).is_err(),
                "{name}: wrong key must fail"
            );
        }
        Ok(())
    }
}

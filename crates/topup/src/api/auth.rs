//! RFC 9421 HTTP Message Signatures verification for inbound requests.

use super::AppState;
use super::error::ApiError;
use super::repository;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::header::HOST;
use axum::http::{HeaderMap, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use ed25519_dalek::VerifyingKey;
use topup_adapters::http_signature::{self, SignedMessage};

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
    let key = match VerificationKey::from_base64(product.kid.clone(), &product.pubkey) {
        Ok(key) => key,
        Err(message) => {
            tracing::error!(product_id = %product.id, %message, "stored product key is invalid");
            return ApiError::unauthorized().into_response();
        }
    };
    let verified = match verify_request(&mut request, &key).await {
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
    let verified = match verify_request(&mut request, &state.admin_key).await {
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
    key: &VerificationKey,
) -> Result<VerifiedSignature, ()> {
    let body = std::mem::replace(request.body_mut(), Body::empty());
    let bytes = to_bytes(body, MAX_SIGNED_BODY_BYTES)
        .await
        .map_err(|_| ())?;
    *request.body_mut() = Body::from(bytes.clone());

    let target_uri = target_uri(request.uri(), request.headers())?;
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

fn target_uri(uri: &Uri, headers: &HeaderMap) -> Result<String, ()> {
    if uri.scheme().is_some() && uri.authority().is_some() {
        return Ok(uri.to_string());
    }
    let scheme = headers
        .get("x-forwarded-proto")
        .map(|value| value.to_str().map_err(|_| ()))
        .transpose()?
        .unwrap_or("http");
    if !matches!(scheme, "http" | "https") {
        return Err(());
    }
    let authority = headers.get(HOST).ok_or(())?.to_str().map_err(|_| ())?;
    let path = uri.path_and_query().map_or("/", |value| value.as_str());
    Ok(format!("{scheme}://{authority}{path}"))
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

        verify_request(&mut request, &key)
            .await
            .map_err(|()| "settlement signature must verify")?;
        Ok(())
    }
}

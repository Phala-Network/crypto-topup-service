//! RFC 9421 HTTP Message Signatures verification for inbound requests.

use super::AppState;
use super::error::ApiError;
use super::repository;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::header::HOST;
use axum::http::{HeaderMap, Method, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use sfv::{BareItem, Dictionary, FieldType as _, List, ListEntry, Parser, Version};
use sha2::{Digest, Sha256};

const MAX_SIGNED_BODY_BYTES: usize = 1_048_576;
const REQUIRED_SIGNATURE_COMPONENTS: [&str; 3] = ["@method", "@target-uri", "content-digest"];
const IDEMPOTENCY_COMPONENT: &str = "idempotency-key";

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
    verify_request_at(request, key, Utc::now().timestamp()).await
}

async fn verify_request_at(
    request: &mut Request,
    key: &VerificationKey,
    now: i64,
) -> Result<VerifiedSignature, ()> {
    let body = std::mem::replace(request.body_mut(), Body::empty());
    let bytes = to_bytes(body, MAX_SIGNED_BODY_BYTES)
        .await
        .map_err(|_| ())?;
    *request.body_mut() = Body::from(bytes.clone());

    let digest_value = header_value(request.headers(), "content-digest")?;
    verify_content_digest(digest_value, &bytes)?;

    let signature_inputs = parse_dictionary(header_value(request.headers(), "signature-input")?)?;
    let signatures = parse_dictionary(header_value(request.headers(), "signature")?)?;
    let target_uri = target_uri(request.uri(), request.headers())?;
    let idempotency_key = request
        .headers()
        .get(IDEMPOTENCY_COMPONENT)
        .map(|value| value.to_str().map(str::trim).map_err(|_| ()))
        .transpose()?;

    for (label, entry) in &signature_inputs {
        let Ok(parsed) = parse_signature_input_entry(entry) else {
            continue;
        };
        if parsed.covers_idempotency_key != idempotency_key.is_some() {
            continue;
        }
        if parsed.keyid != key.kid || now.abs_diff(parsed.created) > 300 {
            continue;
        }
        let Some(signature_bytes) = signature_bytes(signatures.get(label.as_str())) else {
            continue;
        };
        let Ok(signature) = Signature::from_slice(signature_bytes) else {
            continue;
        };
        let base = signature_base(
            request.method(),
            &target_uri,
            digest_value,
            idempotency_key,
            &parsed.parameters,
        );
        if key.key.verify_strict(base.as_bytes(), &signature).is_ok() {
            return Ok(VerifiedSignature {
                kid: parsed.keyid,
                signature_hash: Sha256::digest(signature_bytes).into(),
                created: DateTime::from_timestamp(parsed.created, 0).ok_or(())?,
            });
        }
    }
    Err(())
}

fn verify_content_digest(value: &str, body: &[u8]) -> Result<(), ()> {
    let encoded = value
        .strip_prefix("sha-256=:")
        .and_then(|value| value.strip_suffix(':'))
        .ok_or(())?;
    let supplied = STANDARD.decode(encoded).map_err(|_| ())?;
    let expected: [u8; 32] = Sha256::digest(body).into();
    if supplied.as_slice() == expected {
        Ok(())
    } else {
        Err(())
    }
}

struct ParsedSignatureInput {
    created: i64,
    keyid: String,
    parameters: String,
    covers_idempotency_key: bool,
}

fn parse_dictionary(value: &str) -> Result<Dictionary, ()> {
    Parser::new(value)
        .with_version(Version::Rfc8941)
        .parse()
        .map_err(|_| ())
}

fn parse_signature_input_entry(entry: &ListEntry) -> Result<ParsedSignatureInput, ()> {
    let ListEntry::InnerList(inner_list) = entry else {
        return Err(());
    };
    if !matches!(inner_list.items.len(), 3 | 4) {
        return Err(());
    }
    for (item, expected) in inner_list
        .items
        .iter()
        .take(REQUIRED_SIGNATURE_COMPONENTS.len())
        .zip(REQUIRED_SIGNATURE_COMPONENTS)
    {
        if !item.params.is_empty() {
            return Err(());
        }
        let BareItem::String(component) = &item.bare_item else {
            return Err(());
        };
        if component.as_str() != expected {
            return Err(());
        }
    }
    let covers_idempotency_key = if inner_list.items.len() == 4 {
        let item = inner_list.items.get(3).ok_or(())?;
        if !item.params.is_empty() {
            return Err(());
        }
        matches!(&item.bare_item, BareItem::String(component) if component.as_str() == IDEMPOTENCY_COMPONENT)
    } else {
        false
    };
    if inner_list.items.len() == 4 && !covers_idempotency_key {
        return Err(());
    }

    let created = match inner_list.params.get("created") {
        Some(BareItem::Integer(created)) => (*created).into(),
        _ => return Err(()),
    };
    let keyid = match inner_list.params.get("keyid") {
        Some(BareItem::String(keyid)) if !keyid.as_str().is_empty() => keyid.as_str().to_owned(),
        _ => return Err(()),
    };
    match inner_list.params.get("alg") {
        None => {}
        Some(BareItem::String(algorithm)) if algorithm.as_str() == "ed25519" => {}
        Some(_) => return Err(()),
    }

    let parameters = List::from([entry.clone()]).serialize().ok_or(())?;
    Ok(ParsedSignatureInput {
        created,
        keyid,
        parameters,
        covers_idempotency_key,
    })
}

fn signature_bytes(entry: Option<&ListEntry>) -> Option<&[u8]> {
    let ListEntry::Item(item) = entry? else {
        return None;
    };
    if !item.params.is_empty() {
        return None;
    }
    let BareItem::ByteSequence(bytes) = &item.bare_item else {
        return None;
    };
    Some(bytes)
}

fn signature_base(
    method: &Method,
    target_uri: &str,
    content_digest: &str,
    idempotency_key: Option<&str>,
    parameters: &str,
) -> String {
    let mut base = format!(
        "\"@method\": {}\n\"@target-uri\": {target_uri}\n\"content-digest\": {content_digest}",
        method.as_str()
    );
    if let Some(idempotency_key) = idempotency_key {
        base.push_str("\n\"idempotency-key\": ");
        base.push_str(idempotency_key);
    }
    base.push_str("\n\"@signature-params\": ");
    base.push_str(parameters);
    base
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

    #[test]
    fn signature_input_requires_the_exact_covered_components() -> Result<(), ()> {
        let valid = parse_dictionary(
            "sig1=(\"@method\" \"@target-uri\" \"content-digest\");created=1;keyid=\"product/v1\"",
        )?;
        assert!(parse_signature_input_entry(valid.get("sig1").ok_or(())?).is_ok());

        let with_idempotency = parse_dictionary(
            "sig1=(\"@method\" \"@target-uri\" \"content-digest\" \"idempotency-key\");created=1;keyid=\"settlement/v1\"",
        )?;
        let parsed = parse_signature_input_entry(with_idempotency.get("sig1").ok_or(())?)?;
        assert!(parsed.covers_idempotency_key);

        let missing =
            parse_dictionary("sig1=(\"@method\" \"@target-uri\");created=1;keyid=\"product/v1\"")?;
        assert!(parse_signature_input_entry(missing.get("sig1").ok_or(())?).is_err());

        let unexpected = parse_dictionary(
            "sig1=(\"@method\" \"@target-uri\" \"content-digest\" \"x-extra\");created=1;keyid=\"product/v1\"",
        )?;
        assert!(parse_signature_input_entry(unexpected.get("sig1").ok_or(())?).is_err());
        Ok(())
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

    /// Requests signed by the Python SDK (`sdk/python/tests/vectors.py`) verify here unchanged.
    #[tokio::test]
    async fn python_sdk_signatures_verify() -> Result<(), Box<dyn Error>> {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/rfc9421-python-signer.json"
        ))?;
        let key = VerificationKey::from_base64(
            fixture["keyid"].as_str().ok_or("keyid")?.to_owned(),
            fixture["public_key"].as_str().ok_or("public_key")?,
        )?;
        let created = fixture["created"].as_i64().ok_or("created")?;
        let vectors = fixture["vectors"].as_array().ok_or("vectors")?;
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
            let body = vector["body"].as_str().ok_or("body")?;
            let request = |body: &str| -> Result<Request, Box<dyn Error>> {
                let mut builder = Request::builder()
                    .method(vector["method"].as_str().ok_or("method")?)
                    .uri(vector["target"].as_str().ok_or("target")?);
                for (header, value) in vector["headers"].as_object().ok_or("headers")? {
                    builder = builder.header(header, value.as_str().ok_or("header value")?);
                }
                Ok(builder.body(Body::from(body.to_owned()))?)
            };

            let verified = verify_request_at(&mut request(body)?, &key, created + 300)
                .await
                .map_err(|()| format!("{name}: Python signature must verify"))?;
            assert_eq!(verified.kid, "sdk-vector/v1", "{name}");
            assert_eq!(verified.created.timestamp(), created, "{name}");

            assert!(
                verify_request_at(&mut request(body)?, &key, created + 301)
                    .await
                    .is_err(),
                "{name}: stale signature must fail"
            );
            assert!(
                verify_request_at(&mut request(&format!("{body} "))?, &key, created)
                    .await
                    .is_err(),
                "{name}: altered body must fail"
            );
            let other_key = VerificationKey::from_base64(
                key.kid.clone(),
                &STANDARD.encode(SigningKey::from_bytes(&[9; 32]).verifying_key().as_bytes()),
            )?;
            assert!(
                verify_request_at(&mut request(body)?, &other_key, created)
                    .await
                    .is_err(),
                "{name}: wrong key must fail"
            );
        }
        Ok(())
    }
}

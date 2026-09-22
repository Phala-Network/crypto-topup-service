//! RFC 9421 HTTP Message Signatures verification for inbound requests.

use std::str::FromStr;

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
use chrono::Utc;
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

const MAX_SIGNED_BODY_BYTES: usize = 1_048_576;
const SIGNATURE_COMPONENTS: &str = "(\"@method\" \"@target-uri\" \"content-digest\")";

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
    if verify_request(&mut request, &key).await.is_err() {
        return ApiError::unauthorized().into_response();
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
    if verify_request(&mut request, &state.admin_key)
        .await
        .is_err()
    {
        return ApiError::unauthorized().into_response();
    }
    next.run(request).await
}

async fn verify_request(request: &mut Request, key: &VerificationKey) -> Result<(), ()> {
    let body = std::mem::replace(request.body_mut(), Body::empty());
    let bytes = to_bytes(body, MAX_SIGNED_BODY_BYTES)
        .await
        .map_err(|_| ())?;
    *request.body_mut() = Body::from(bytes.clone());

    let digest_value = header_value(request.headers(), "content-digest")?;
    verify_content_digest(digest_value, &bytes)?;

    let signature_input = header_value(request.headers(), "signature-input")?;
    let parsed = parse_signature_input(signature_input)?;
    if parsed.keyid != key.kid {
        return Err(());
    }
    let now = Utc::now().timestamp();
    if now.abs_diff(parsed.created) > 300 {
        return Err(());
    }

    let target_uri = target_uri(request.uri(), request.headers())?;
    let base = signature_base(
        request.method(),
        &target_uri,
        digest_value,
        parsed.parameters,
    );
    let signature = parse_signature(header_value(request.headers(), "signature")?)?;
    key.key
        .verify_strict(base.as_bytes(), &signature)
        .map_err(|_| ())
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

struct ParsedSignatureInput<'a> {
    created: i64,
    keyid: String,
    parameters: &'a str,
}

fn parse_signature_input(value: &str) -> Result<ParsedSignatureInput<'_>, ()> {
    let parameters = value.strip_prefix("sig1=").ok_or(())?;
    let remainder = parameters.strip_prefix(SIGNATURE_COMPONENTS).ok_or(())?;
    let remainder = remainder.strip_prefix(";created=").ok_or(())?;
    let (created, remainder) = remainder.split_once(';').ok_or(())?;
    let created = i64::from_str(created).map_err(|_| ())?;
    let keyid = remainder
        .strip_prefix("keyid=\"")
        .and_then(|value| value.strip_suffix('"'))
        .ok_or(())?;
    if keyid.is_empty() || keyid.contains(['"', '\\', '\r', '\n']) {
        return Err(());
    }
    Ok(ParsedSignatureInput {
        created,
        keyid: keyid.to_owned(),
        parameters,
    })
}

fn parse_signature(value: &str) -> Result<Signature, ()> {
    let encoded = value
        .strip_prefix("sig1=:")
        .and_then(|value| value.strip_suffix(':'))
        .ok_or(())?;
    let bytes = STANDARD.decode(encoded).map_err(|_| ())?;
    Signature::from_slice(&bytes).map_err(|_| ())
}

fn signature_base(
    method: &Method,
    target_uri: &str,
    content_digest: &str,
    parameters: &str,
) -> String {
    format!(
        "\"@method\": {}\n\"@target-uri\": {target_uri}\n\"content-digest\": {content_digest}\n\"@signature-params\": {parameters}",
        method.as_str()
    )
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
    use super::*;

    #[test]
    fn signature_input_requires_the_exact_covered_components() {
        let valid =
            "sig1=(\"@method\" \"@target-uri\" \"content-digest\");created=1;keyid=\"product/v1\"";
        assert!(parse_signature_input(valid).is_ok());
        assert!(
            parse_signature_input(
                "sig1=(\"@method\" \"@target-uri\");created=1;keyid=\"product/v1\""
            )
            .is_err()
        );
    }
}

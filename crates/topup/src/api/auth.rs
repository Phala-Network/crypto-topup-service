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
use sqlx::PgPool;
use topup_adapters::http_signature::{self, PublicOrigin, SignedMessage};
use uuid::Uuid;

use crate::audit::Actor;
use crate::db::Account;
use crate::tenancy::{self, Permission, Principal, Scope};

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

/// The authenticated merchant of a request: its account, the [`Scope`] every query it makes
/// takes, and the credential it signed with.
#[derive(Clone, Debug)]
pub(crate) struct Merchant {
    /// The account the credential belongs to.
    pub(crate) account: Account,
    /// Built from the credential alone: its account and mode.
    pub(crate) scope: Scope,
    /// The credential's key id, `{acct_…}/v1`.
    pub(crate) key_id: String,
}

impl Merchant {
    /// The audit actor of the merchant's requests.
    pub(crate) fn actor(&self) -> Actor {
        Actor::api_key(&self.key_id)
    }

    /// Fails with `403 permission_denied` unless the authorization table grants `permission` to
    /// the credential. The request signing key is a secret key until API keys replace it.
    pub(crate) async fn require(
        &self,
        pool: &PgPool,
        permission: Permission,
    ) -> Result<(), ApiError> {
        if tenancy::holds(pool, Principal::SecretKey, permission).await? {
            Ok(())
        } else {
            Err(ApiError::permission_denied())
        }
    }
}

/// Authenticates a merchant request and attaches its [`Merchant`], whose scope comes from the
/// signing key: key id `{acct_…}/v1` names the account, and the stored key fixes the mode.
pub async fn authenticate_merchant(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some((account_id, kid)) = request
        .headers()
        .get("signature-input")
        .and_then(|value| value.to_str().ok())
        .map(http_signature::signature_keyids)
        .and_then(|keyids| {
            keyids
                .into_iter()
                .find_map(|keyid| account_of_key_id(&keyid).map(|account| (account, keyid)))
        })
    else {
        return ApiError::unauthorized().into_response();
    };
    let signing_key = match repository::find_signing_key(&state.pool, account_id).await {
        Ok(Some(signing_key)) => signing_key,
        Ok(None) => return ApiError::unauthorized().into_response(),
        Err(error) => return error.into_response(),
    };
    let key = match VerificationKey::from_base64(kid.clone(), &signing_key.public_key) {
        Ok(key) => key,
        Err(message) => {
            tracing::error!(account_id = %account_id, %message, "stored signing key is invalid");
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
    let scope = Scope::new(signing_key.account.id, signing_key.livemode);
    request.extensions_mut().insert(Merchant {
        account: signing_key.account,
        scope,
        key_id: kid,
    });
    next.run(request).await
}

/// Passes an unsigned request that carries a `client_secret` query parameter to the handler
/// without a merchant, which then serves the quote's public view; any other request must be a
/// signed merchant request.
pub async fn authenticate_merchant_or_client_secret(
    state: State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let unsigned = !request.headers().contains_key("signature-input")
        && !request.headers().contains_key("signature");
    let has_client_secret = request.uri().query().is_some_and(|query| {
        url::form_urlencoded::parse(query.as_bytes()).any(|(name, _)| name == "client_secret")
    });
    if unsigned && has_client_secret {
        next.run(request).await
    } else {
        authenticate_merchant(state, request, next).await
    }
}

/// The account a merchant key id names: `{acct_…}/v1`.
fn account_of_key_id(keyid: &str) -> Option<Uuid> {
    crate::ids::parse(crate::ids::ACCOUNT, keyid.strip_suffix("/v1")?)
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

#[cfg(test)]
mod tests {
    use std::error::Error;

    use ed25519_dalek::SigningKey;

    use super::*;

    /// The scope's account comes only from the key id the request was verified with; anything
    /// but `{acct_…}/v1` names no account.
    #[test]
    fn only_an_account_key_id_names_an_account() {
        let account = Uuid::from_u128(0x0c6e_1d0a_9b3f_4c2e_8d7a_6b5c_4d3e_2f10);
        assert_eq!(
            account_of_key_id("acct_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10/v1"),
            Some(account)
        );
        for keyid in [
            "acct_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10",
            "acct_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10/v2",
            "phala-cloud/v1",
            "admin/v1",
            "0c6e1d0a-9b3f-4c2e-8d7a-6b5c4d3e2f10/v1",
            "acct_0C6E1D0A9B3F4C2E8D7A6B5C4D3E2F10/v1",
        ] {
            assert_eq!(account_of_key_id(keyid), None, "{keyid}");
        }
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

//! RFC 9421 signed HTTP client for product settlements.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use reqwest::{Method, Request, StatusCode, Url};
use serde::Deserialize;
use serde_json::Value;
use topup_core::{SETTLEMENT_KEY_DOMAIN, SignerError};

use crate::http_signature::{self, SignError};
use crate::redaction::{Redacted, RedactedTransportError};
use crate::signer::actor::SignerHandle;

const RESPONSE_BODY_LIMIT: usize = 64 * 1024;
const EVIDENCE_BODY_LIMIT: usize = 4 * 1024;

#[derive(Clone, Copy)]
struct SigningOptions {
    created: i64,
    cover_idempotency_key: bool,
}

/// A settlement payload and its deterministic product idempotency key.
#[derive(Clone, Debug, PartialEq)]
pub struct SettlementRequest {
    /// Unquoted key also carried inside the JSON payload.
    pub idempotency_key: String,
    /// Immutable JSON body retained by the service across retries.
    pub payload: Value,
}

/// A typed business or protocol answer from the product settlement endpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettlementAnswer {
    /// The product durably credited the account.
    Accepted {
        /// Product-owned transaction identifier.
        destination_tx_id: String,
        /// Product's immutable original settlement payload.
        payload: Value,
    },
    /// The product has retained the request and is still processing it.
    Processing {
        /// Product's immutable original settlement payload.
        payload: Value,
    },
    /// The product durably refused the credit.
    Rejected {
        /// Product-provided refusal reason.
        reason: String,
        /// Product's immutable original settlement payload.
        payload: Value,
    },
    /// Another request with this key is currently processing.
    Conflict409,
    /// The key already belongs to a different payload.
    PayloadMismatch422,
    /// A response outside the settlement contract.
    Unknown {
        /// HTTP status code.
        status: u16,
        /// Bounded-by-request-time response body retained for evidence.
        body: String,
    },
}

/// Failure before a typed HTTP response was available.
#[derive(Debug)]
pub enum SettlementClientError {
    /// The configured settlement endpoint is not a usable HTTP URL.
    InvalidEndpoint,
    /// The idempotency key cannot be represented as an RFC 8941 string.
    InvalidIdempotencyKey,
    /// JSON serialization failed.
    Encode,
    /// The system clock cannot produce a valid Unix timestamp.
    InvalidClock,
    /// The settlement signer failed.
    Signer(SignerError),
    /// The HTTP request failed or timed out.
    Transport(RedactedTransportError),
}

impl Display for SettlementClientError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint => formatter.write_str("invalid settlement endpoint"),
            Self::InvalidIdempotencyKey => {
                formatter.write_str("invalid settlement idempotency key")
            }
            Self::Encode => formatter.write_str("failed to encode settlement payload"),
            Self::InvalidClock => formatter.write_str("system clock is before the Unix epoch"),
            Self::Signer(error) => Display::fmt(error, formatter),
            Self::Transport(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for SettlementClientError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Signer(error) => Some(error),
            Self::Transport(error) => Some(error),
            Self::InvalidEndpoint
            | Self::InvalidIdempotencyKey
            | Self::Encode
            | Self::InvalidClock => None,
        }
    }
}

/// Mockable product settlement boundary.
#[async_trait]
pub trait SettlementApi: Send + Sync {
    /// Sends or idempotently re-sends one immutable settlement request.
    async fn post(
        &self,
        request: &SettlementRequest,
    ) -> Result<SettlementAnswer, SettlementClientError>;

    /// Looks up the product's authoritative answer by deterministic key.
    async fn get_by_key(
        &self,
        key: &str,
    ) -> Result<Option<SettlementAnswer>, SettlementClientError>;
}

/// Rustls-backed RFC 9421 settlement client for one product endpoint.
#[derive(Clone)]
pub struct SettlementClient {
    endpoint: Redacted,
    client: reqwest::Client,
    signer: SignerHandle,
    keyid: String,
}

impl SettlementClient {
    /// Creates a no-redirect client with a total request timeout.
    pub fn new(
        endpoint: &str,
        signer: SignerHandle,
        request_timeout: Duration,
    ) -> Result<Self, SettlementClientError> {
        Self::build(
            endpoint,
            signer,
            request_timeout,
            SETTLEMENT_KEY_DOMAIN.to_owned(),
        )
    }

    /// Creates a client with an explicitly pinned settlement key identifier.
    ///
    /// Only the conformance suite needs a non-default `keyid`, to prove that products reject
    /// a valid signature under the wrong key identifier.
    #[cfg(any(test, feature = "conformance"))]
    #[doc(hidden)]
    pub fn new_with_keyid(
        endpoint: &str,
        signer: SignerHandle,
        request_timeout: Duration,
        keyid: String,
    ) -> Result<Self, SettlementClientError> {
        Self::build(endpoint, signer, request_timeout, keyid)
    }

    fn build(
        endpoint: &str,
        signer: SignerHandle,
        request_timeout: Duration,
        keyid: String,
    ) -> Result<Self, SettlementClientError> {
        if request_timeout.is_zero() {
            return Err(SettlementClientError::InvalidEndpoint);
        }
        if keyid.is_empty() || http_signature::structured_string(&keyid).is_err() {
            return Err(SettlementClientError::InvalidEndpoint);
        }
        let endpoint =
            Redacted::parse(endpoint).map_err(|_| SettlementClientError::InvalidEndpoint)?;
        if !matches!(endpoint.expose().scheme(), "http" | "https")
            || endpoint.expose().cannot_be_a_base()
            || endpoint.expose().fragment().is_some()
        {
            return Err(SettlementClientError::InvalidEndpoint);
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(request_timeout)
            .build()
            .map_err(|error| {
                SettlementClientError::Transport(
                    endpoint.request_error("settlement client construction", &error),
                )
            })?;
        Ok(Self {
            endpoint,
            client,
            signer,
            keyid,
        })
    }

    async fn signed_request(
        &self,
        method: Method,
        url: Url,
        key: &str,
        body: Vec<u8>,
        include_content_type: bool,
        options: SigningOptions,
    ) -> Result<Request, SettlementClientError> {
        let idempotency_key = http_signature::structured_string(key)
            .map_err(|_| SettlementClientError::InvalidIdempotencyKey)?;
        let content_digest = http_signature::content_digest(&body);
        let components = http_signature::Components {
            method: method.as_str(),
            target_uri: url.as_str(),
            content_digest: &content_digest,
            idempotency_key: options
                .cover_idempotency_key
                .then_some(idempotency_key.as_str()),
        };
        let headers = http_signature::sign(&self.signer, &components, options.created, &self.keyid)
            .await
            .map_err(|error| match error {
                SignError::Signer(error) => SettlementClientError::Signer(error),
                SignError::InvalidInput => SettlementClientError::InvalidIdempotencyKey,
            })?;

        let mut request = self
            .client
            .request(method, url)
            .header("content-digest", content_digest)
            .header("idempotency-key", idempotency_key)
            .header("signature-input", headers.signature_input)
            .header("signature", headers.signature)
            .body(body);
        if include_content_type {
            request = request.header("content-type", "application/json");
        }
        request.build().map_err(|error| {
            SettlementClientError::Transport(
                self.endpoint
                    .request_error("settlement request construction", &error),
            )
        })
    }

    /// Builds a fully signed POST request without sending it.
    ///
    /// This keeps RFC 9421 interoperability tests independent of a network server.
    pub async fn signed_post_request(
        &self,
        request: &SettlementRequest,
    ) -> Result<Request, SettlementClientError> {
        self.signed_post(
            request,
            SigningOptions {
                created: unix_timestamp()?,
                cover_idempotency_key: true,
            },
        )
        .await
    }

    /// Builds a signed POST at a caller-supplied time and coverage profile.
    ///
    /// Normal callers should use [`Self::signed_post_request`]. This entry point exists so the
    /// conformance suite can produce expired signatures and signatures which deliberately omit
    /// `idempotency-key` without maintaining a second signing implementation.
    #[cfg(any(test, feature = "conformance"))]
    #[doc(hidden)]
    pub async fn signed_post_request_at(
        &self,
        request: &SettlementRequest,
        created: i64,
        cover_idempotency_key: bool,
    ) -> Result<Request, SettlementClientError> {
        self.signed_post(
            request,
            SigningOptions {
                created,
                cover_idempotency_key,
            },
        )
        .await
    }

    async fn signed_post(
        &self,
        request: &SettlementRequest,
        options: SigningOptions,
    ) -> Result<Request, SettlementClientError> {
        let body =
            serde_json::to_vec(&request.payload).map_err(|_| SettlementClientError::Encode)?;
        self.signed_request(
            Method::POST,
            self.endpoint.expose().clone(),
            &request.idempotency_key,
            body,
            true,
            options,
        )
        .await
    }

    /// Builds the signed `GET {endpoint}/{key}` lookup without sending it.
    pub async fn signed_get_request(&self, key: &str) -> Result<Request, SettlementClientError> {
        self.signed_get(
            key,
            SigningOptions {
                created: unix_timestamp()?,
                cover_idempotency_key: true,
            },
        )
        .await
    }

    async fn signed_get(
        &self,
        key: &str,
        options: SigningOptions,
    ) -> Result<Request, SettlementClientError> {
        let mut url = self.endpoint.expose().clone();
        url.path_segments_mut()
            .map_err(|()| SettlementClientError::InvalidEndpoint)?
            .pop_if_empty()
            .push(key);
        self.signed_request(Method::GET, url, key, Vec::new(), false, options)
            .await
    }
}

#[async_trait]
impl SettlementApi for SettlementClient {
    async fn post(
        &self,
        request: &SettlementRequest,
    ) -> Result<SettlementAnswer, SettlementClientError> {
        let signed = self.signed_post_request(request).await?;
        let response = self.client.execute(signed).await.map_err(|error| {
            SettlementClientError::Transport(self.endpoint.request_error("settlement POST", &error))
        })?;
        parse_response(&self.endpoint, response, Some(&request.payload), false).await
    }

    async fn get_by_key(
        &self,
        key: &str,
    ) -> Result<Option<SettlementAnswer>, SettlementClientError> {
        let signed = self.signed_get_request(key).await?;
        let response = self.client.execute(signed).await.map_err(|error| {
            SettlementClientError::Transport(self.endpoint.request_error("settlement GET", &error))
        })?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        parse_response(&self.endpoint, response, None, true)
            .await
            .map(Some)
    }
}

#[derive(Deserialize)]
struct ProductAnswer {
    status: String,
    destination_tx_id: Option<String>,
    reason: Option<String>,
    payload: Option<Value>,
}

async fn parse_response(
    endpoint: &Redacted,
    mut response: reqwest::Response,
    fallback_payload: Option<&Value>,
    payload_required: bool,
) -> Result<SettlementAnswer, SettlementClientError> {
    let status = response.status();
    let (body, over_limit) = read_response_body(endpoint, &mut response).await?;
    if over_limit {
        return Ok(unknown(status, &body, true));
    }
    if status == StatusCode::CONFLICT {
        return Ok(SettlementAnswer::Conflict409);
    }
    if status == StatusCode::UNPROCESSABLE_ENTITY {
        return Ok(SettlementAnswer::PayloadMismatch422);
    }
    if status == StatusCode::OK
        && let Ok(answer) = serde_json::from_slice::<ProductAnswer>(&body)
    {
        let payload = match answer.payload {
            Some(payload) => Some(payload),
            None if !payload_required => fallback_payload.cloned(),
            None => None,
        };
        return match (answer.status.as_str(), payload) {
            ("accepted", Some(payload)) => Ok(answer
                .destination_tx_id
                .filter(|value| !value.is_empty())
                .map_or_else(
                    || unknown(status, &body, false),
                    |destination_tx_id| SettlementAnswer::Accepted {
                        destination_tx_id,
                        payload,
                    },
                )),
            ("processing", Some(payload)) => Ok(SettlementAnswer::Processing { payload }),
            ("rejected", Some(payload)) => {
                Ok(answer.reason.filter(|value| !value.is_empty()).map_or_else(
                    || unknown(status, &body, false),
                    |reason| SettlementAnswer::Rejected { reason, payload },
                ))
            }
            _ => Ok(unknown(status, &body, false)),
        };
    }
    Ok(unknown(status, &body, false))
}

async fn read_response_body(
    endpoint: &Redacted,
    response: &mut reqwest::Response,
) -> Result<(Vec<u8>, bool), SettlementClientError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        SettlementClientError::Transport(endpoint.request_error("settlement response body", &error))
    })? {
        let remaining = RESPONSE_BODY_LIMIT.saturating_sub(body.len());
        if chunk.len() > remaining {
            let retained = chunk.get(..remaining).unwrap_or_default();
            body.extend_from_slice(retained);
            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, false))
}

fn unknown(status: StatusCode, body: &[u8], over_limit: bool) -> SettlementAnswer {
    let original_len = body.len();
    let retained = body
        .get(..original_len.min(EVIDENCE_BODY_LIMIT))
        .unwrap_or(body);
    let mut body = String::from_utf8_lossy(retained).into_owned();
    if over_limit {
        body.push_str(" [response body exceeded 65536 bytes]");
    } else if retained.len() < original_len {
        body.push_str(" [response body truncated]");
    }
    SettlementAnswer::Unknown {
        status: status.as_u16(),
        body,
    }
}

fn unix_timestamp() -> Result<i64, SettlementClientError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| SettlementClientError::InvalidClock)?
        .as_secs();
    i64::try_from(seconds).map_err(|_| SettlementClientError::InvalidClock)
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use ed25519_dalek::{Signer as _, SigningKey};

    use super::*;

    struct FixedSigner(SigningKey);

    impl topup_core::Signer for FixedSigner {
        async fn sign_operator_tx(
            &self,
            _tx: topup_core::TxRequest,
        ) -> Result<topup_core::SignedTx, SignerError> {
            Err(SignerError::SigningFailed)
        }

        async fn sign_settlement(
            &self,
            payload: &[u8],
        ) -> Result<topup_core::Ed25519Signature, SignerError> {
            Ok(topup_core::Ed25519Signature(
                self.0.sign(payload).to_bytes(),
            ))
        }

        async fn operator_address(&self) -> Result<alloy_primitives::Address, SignerError> {
            Err(SignerError::KeyUnavailable)
        }

        async fn settlement_public_key(&self) -> Result<topup_core::Ed25519PublicKey, SignerError> {
            Ok(topup_core::Ed25519PublicKey(
                self.0.verifying_key().to_bytes(),
            ))
        }
    }

    fn fixture_vector(name: &str, request: &Request) -> serde_json::Value {
        let headers = [
            "content-type",
            "content-digest",
            "idempotency-key",
            "signature-input",
            "signature",
        ]
        .into_iter()
        .filter_map(|name| {
            let value = request.headers().get(name)?.to_str().ok()?;
            Some((name.to_owned(), serde_json::Value::from(value)))
        })
        .collect::<serde_json::Map<_, _>>();
        let body = request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .map(|body| String::from_utf8_lossy(body).into_owned())
            .unwrap_or_default();
        serde_json::json!({
            "name": name,
            "method": request.method().as_str(),
            "target_uri": request.url().as_str(),
            "headers": headers,
            "body": body,
        })
    }

    /// Settlement requests signed here are verified by the Python SDK
    /// (`sdk/python/tests/test_rust_settlement_vectors.py`).
    #[tokio::test]
    async fn settlement_signatures_match_the_cross_language_fixture() {
        const CREATED: i64 = 1_790_000_000;
        let key = SigningKey::from_bytes(&[11; 32]);
        let public_key = STANDARD.encode(key.verifying_key().as_bytes());
        let signer = SignerHandle::spawn(
            FixedSigner(key),
            std::num::NonZeroUsize::new(2).expect("non-zero queue"),
            Duration::from_secs(5),
        )
        .expect("signer starts");
        let client = SettlementClient::new(
            "https://product.example/topup/settlements",
            signer,
            Duration::from_secs(5),
        )
        .expect("client builds");
        let idempotency_key = "deposit:20513a59-9b80-53df-9832-08749de3dcc5";
        let payload = serde_json::json!({
            "version": 1,
            "idempotency_key": idempotency_key,
            "account_id": "workspace-42",
            "unit": "USD",
            "amount_minor": "1234",
            "source": "crypto_deposit",
        });
        let post = client
            .signed_post_request_at(
                &SettlementRequest {
                    idempotency_key: idempotency_key.to_owned(),
                    payload,
                },
                CREATED,
                true,
            )
            .await
            .expect("POST signs");
        let get = client
            .signed_get(
                idempotency_key,
                SigningOptions {
                    created: CREATED,
                    cover_idempotency_key: true,
                },
            )
            .await
            .expect("GET signs");
        let actual = serde_json::json!({
            "description": concat!(
                "RFC 9421 settlement requests signed by crates/adapters SettlementClient. ",
                "Verified by sdk/python/tests/test_rust_settlement_vectors.py."
            ),
            "keyid": SETTLEMENT_KEY_DOMAIN,
            "public_key": public_key,
            "created": CREATED,
            "vectors": [fixture_vector("post", &post), fixture_vector("get_by_key", &get)],
        });
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/rfc9421-rust-settlement.json"
        ))
        .expect("fixture parses");
        assert_eq!(
            actual,
            expected,
            "update crates/adapters/tests/fixtures/rfc9421-rust-settlement.json to:\n{}",
            serde_json::to_string_pretty(&actual).expect("fixture encodes")
        );
    }
}

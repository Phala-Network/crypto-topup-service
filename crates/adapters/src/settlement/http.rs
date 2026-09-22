//! RFC 9421 signed HTTP client for product settlements.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::{Method, Request, StatusCode, Url};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use topup_core::{SETTLEMENT_KEY_DOMAIN, Signer as _, SignerError};

use crate::redaction::{Redacted, RedactedTransportError};
use crate::signer::actor::SignerHandle;

const SIGNATURE_LABEL: &str = "sig1";
const RESPONSE_BODY_LIMIT: usize = 64 * 1024;
const EVIDENCE_BODY_LIMIT: usize = 4 * 1024;
const SIGNATURE_COMPONENTS: [&str; 4] = [
    "@method",
    "@target-uri",
    "content-digest",
    "idempotency-key",
];

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
}

impl SettlementClient {
    /// Creates a no-redirect client with a total request timeout.
    pub fn new(
        endpoint: &str,
        signer: SignerHandle,
        request_timeout: Duration,
    ) -> Result<Self, SettlementClientError> {
        if request_timeout.is_zero() {
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
        })
    }

    async fn signed_request(
        &self,
        method: Method,
        url: Url,
        key: &str,
        body: Vec<u8>,
        include_content_type: bool,
    ) -> Result<Request, SettlementClientError> {
        let created = unix_timestamp()?;
        let idempotency_key = structured_field_string(key)?;
        let content_digest = content_digest(&body);
        let signature_parameters = signature_parameters(created);
        let components = [
            ("@method", method.as_str()),
            ("@target-uri", url.as_str()),
            ("content-digest", content_digest.as_str()),
            ("idempotency-key", idempotency_key.as_str()),
        ];
        let signature_base = signature_base(&components, &signature_parameters);
        let signature = self
            .signer
            .sign_settlement(signature_base.as_bytes())
            .await
            .map_err(SettlementClientError::Signer)?;
        let signature_input = format!("{SIGNATURE_LABEL}={signature_parameters}");
        let signature = format!("{SIGNATURE_LABEL}=:{}:", STANDARD.encode(signature.0));

        let mut request = self
            .client
            .request(method, url)
            .header("content-digest", content_digest)
            .header("idempotency-key", idempotency_key)
            .header("signature-input", signature_input)
            .header("signature", signature)
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
        let body =
            serde_json::to_vec(&request.payload).map_err(|_| SettlementClientError::Encode)?;
        self.signed_request(
            Method::POST,
            self.endpoint.expose().clone(),
            &request.idempotency_key,
            body,
            true,
        )
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
        let mut url = self.endpoint.expose().clone();
        url.path_segments_mut()
            .map_err(|()| SettlementClientError::InvalidEndpoint)?
            .pop_if_empty()
            .push(key);
        let signed = self
            .signed_request(Method::GET, url, key, Vec::new(), false)
            .await?;
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

fn content_digest(body: &[u8]) -> String {
    format!("sha-256=:{}:", STANDARD.encode(Sha256::digest(body)))
}

fn structured_field_string(value: &str) -> Result<String, SettlementClientError> {
    let mut encoded = String::with_capacity(value.len().saturating_add(2));
    encoded.push('"');
    for character in value.chars() {
        if !character.is_ascii() || !(' '..='~').contains(&character) {
            return Err(SettlementClientError::InvalidIdempotencyKey);
        }
        if matches!(character, '"' | '\\') {
            encoded.push('\\');
        }
        encoded.push(character);
    }
    encoded.push('"');
    Ok(encoded)
}

fn signature_parameters(created: i64) -> String {
    let components = SIGNATURE_COMPONENTS
        .iter()
        .map(|component| format!("\"{component}\""))
        .collect::<Vec<_>>()
        .join(" ");
    format!("({components});created={created};keyid=\"{SETTLEMENT_KEY_DOMAIN}\"")
}

fn signature_base(components: &[(&str, &str)], parameters: &str) -> String {
    let mut lines = components
        .iter()
        .map(|(identifier, value)| format!("\"{identifier}\": {value}"))
        .collect::<Vec<_>>();
    lines.push(format!("\"@signature-params\": {parameters}"));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _};

    use super::*;

    #[test]
    fn settlement_profile_signature_base_has_exact_components() {
        let parameters = signature_parameters(1_618_884_473);
        let components = [
            ("@method", "POST"),
            ("@target-uri", "https://product.example/settlements"),
            ("content-digest", "sha-256=:YWJj:"),
            ("idempotency-key", "\"deposit:123\""),
        ];
        assert_eq!(
            signature_base(&components, &parameters),
            concat!(
                "\"@method\": POST\n",
                "\"@target-uri\": https://product.example/settlements\n",
                "\"content-digest\": sha-256=:YWJj:\n",
                "\"idempotency-key\": \"deposit:123\"\n",
                "\"@signature-params\": (\"@method\" \"@target-uri\" ",
                "\"content-digest\" \"idempotency-key\");created=1618884473;",
                "keyid=\"settlement/v1\""
            )
        );
    }

    #[test]
    fn rfc_9421_ed25519_example_matches_base_and_signature() {
        let components = [
            ("date", "Tue, 20 Apr 2021 02:07:55 GMT"),
            ("@method", "POST"),
            ("@path", "/foo"),
            ("@authority", "example.com"),
            ("content-type", "application/json"),
            ("content-length", "18"),
        ];
        let parameters = concat!(
            "(\"date\" \"@method\" \"@path\" \"@authority\" ",
            "\"content-type\" \"content-length\");created=1618884473;",
            "keyid=\"test-key-ed25519\""
        );
        let base = signature_base(&components, parameters);
        let private = URL_SAFE_NO_PAD
            .decode("n4Ni-HpISpVObnQMW0wOhCKROaIKqKtW_2ZYb2p9KcU")
            .expect("RFC key is valid base64url");
        let private: [u8; 32] = private.try_into().expect("RFC key is 32 bytes");
        let key = SigningKey::from_bytes(&private);
        let signature = key.sign(base.as_bytes());
        assert_eq!(
            STANDARD.encode(signature.to_bytes()),
            concat!(
                "wqcAqbmYJ2ji2glfAMaRy4gruYYnx2nEFN2HN6jrnDnQCK1",
                "u02Gb04v9EDgwUPiu4A0w6vuQv5lIp5WPpBKRCw=="
            )
        );
        assert!(
            key.verifying_key()
                .verify(base.as_bytes(), &signature)
                .is_ok()
        );
        assert_eq!(Signature::from_bytes(&signature.to_bytes()), signature);
    }

    #[test]
    fn idempotency_key_is_a_quoted_structured_field_string() {
        assert_eq!(
            structured_field_string("deposit:123").expect("key is valid"),
            "\"deposit:123\""
        );
        assert_eq!(
            structured_field_string("quoted\"slash\\").expect("key is escapable"),
            "\"quoted\\\"slash\\\\\""
        );
        assert!(structured_field_string("line\nbreak").is_err());
    }
}

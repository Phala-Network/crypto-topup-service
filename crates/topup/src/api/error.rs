//! Stable API error responses.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use utoipa::ToSchema;

/// Machine-readable error envelope returned by every API failure, Stripe's error object
/// (<https://docs.stripe.com/api/errors>).
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ErrorResponse {
    /// Error details.
    pub error: ErrorDetail,
}

/// Stable error fields safe to expose to callers.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ErrorDetail {
    /// Error category: `invalid_request_error` for any 4xx except an idempotency conflict,
    /// `idempotency_error` for an `Idempotency-Key` reused with another request or still in use,
    /// `api_error` for 5xx.
    #[serde(rename = "type")]
    pub error_type: ErrorType,
    /// Stable machine-readable code.
    pub code: &'static str,
    /// Human-readable summary without internal details; it may change.
    pub message: String,
    /// The request parameter the error is about, when there is one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
}

/// Error category of [`ErrorDetail`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub enum ErrorType {
    /// The request cannot succeed as sent.
    #[serde(rename = "invalid_request_error")]
    InvalidRequest,
    /// An `Idempotency-Key` was reused with different parameters.
    #[serde(rename = "idempotency_error")]
    Idempotency,
    /// The service failed; retry with backoff.
    #[serde(rename = "api_error")]
    Api,
}

/// API failure with an HTTP status and stable public body.
#[derive(Clone, Debug)]
pub struct ApiError {
    status: StatusCode,
    detail: ErrorDetail,
}

impl ApiError {
    /// Returns a request-validation failure not tied to one parameter.
    #[must_use]
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "parameter_invalid", message)
    }

    /// Returns a validation failure of the request parameter `param`.
    #[must_use]
    pub fn invalid_param(param: impl Into<String>, message: impl Into<String>) -> Self {
        Self::bad_request(message).with_param(param)
    }

    /// Returns a request missing a required parameter.
    #[must_use]
    pub fn missing_param(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "parameter_missing", message)
    }

    /// Returns a request naming a parameter the operation does not take.
    #[must_use]
    pub fn unknown_param(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "parameter_unknown", message)
    }

    /// Returns a failure for a requested amount below the minimum.
    #[must_use]
    pub fn amount_too_small(param: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "amount_too_small", message).with_param(param)
    }

    /// Returns a failure for a requested amount above the maximum.
    #[must_use]
    pub fn amount_too_large(param: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "amount_too_large", message).with_param(param)
    }

    /// Returns an admin request whose RFC 9421 signature did not verify.
    #[must_use]
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "signature_invalid",
            "request signature verification failed",
        )
    }

    /// Returns a merchant request without `Authorization: Bearer`.
    #[must_use]
    pub fn api_key_missing() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "api_key_missing",
            "send your secret key as `Authorization: Bearer ppay_sk_…`",
        )
    }

    /// Returns a merchant request with a malformed, unknown, or revoked key.
    #[must_use]
    pub fn api_key_invalid() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "api_key_invalid",
            "the API key is invalid or revoked",
        )
    }

    /// Returns a merchant request with a rolled key past its expiry (Stripe's code).
    #[must_use]
    pub fn api_key_expired() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "api_key_expired",
            "the API key has expired; use the key it was rolled to",
        )
    }

    /// Returns a request the credential is not permitted to make (design D13).
    #[must_use]
    pub fn permission_denied() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "permission_denied",
            "the credential does not have the required permission",
        )
    }

    /// Returns a missing or foreign resource.
    #[must_use]
    pub fn not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "resource_missing",
            "resource not found",
        )
    }

    /// Returns a state conflict.
    #[must_use]
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", message)
    }

    /// Returns an open quote exposure cap failure.
    #[must_use]
    pub fn exposure_cap(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "exposure_cap_exceeded", message).with_param("amount")
    }

    /// Returns a cancellation refused because the quote's address already received a payment.
    #[must_use]
    pub fn quote_payment_received() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "quote_payment_received",
            "the quote's address already received a payment",
        )
    }

    /// Returns a cancellation refused because the quote's payment window has closed.
    #[must_use]
    pub fn quote_window_closed() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "quote_window_closed",
            "the quote's payment window has closed",
        )
    }

    /// Returns a cancellation refused because the quote is complete or expired.
    #[must_use]
    pub fn quote_unexpected_state(status: &str) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "quote_unexpected_state",
            format!("the quote is {status}"),
        )
    }

    /// Returns a refund refused because the deposit is not refundable (architecture §15).
    #[must_use]
    pub fn deposit_not_refundable() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "deposit_not_refundable",
            "the deposit is not eligible for a refund",
        )
    }

    /// Returns a refund refused because the deposit is not final yet and could still be reversed
    /// (design D1, D5).
    #[must_use]
    pub fn deposit_not_final() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "deposit_not_final",
            "the deposit is not final yet; request the refund once it is (about 15 minutes after its block on Ethereum)",
        )
    }

    /// Returns a refund refused because a sanctions list names its destination (design §8).
    #[must_use]
    pub fn destination_sanctioned() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "destination_sanctioned",
            "the destination address is on a sanctions list",
        )
        .with_param("destination_address")
    }

    /// Returns a refund transaction whose transfer log already pays another refund.
    #[must_use]
    pub fn transfer_already_used() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "transfer_already_used",
            "the transfer log already pays another refund",
        )
        .with_param("log_index")
    }

    /// Returns a refund action its status does not allow.
    #[must_use]
    pub fn refund_unexpected_state(detail: impl std::fmt::Display) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "refund_unexpected_state",
            format!("the refund is {detail}"),
        )
    }

    /// Returns an `Idempotency-Key` reused with a different request (design §13).
    #[must_use]
    pub fn idempotency_key_reused() -> Self {
        let mut error = Self::new(
            StatusCode::BAD_REQUEST,
            "idempotency_key_reused",
            "the Idempotency-Key was used with a different request",
        );
        error.detail.error_type = ErrorType::Idempotency;
        error
    }

    /// Returns a request whose `Idempotency-Key` is held by a request still running; retry.
    #[must_use]
    pub fn idempotency_key_in_use() -> Self {
        let mut error = Self::new(
            StatusCode::CONFLICT,
            "idempotency_key_in_use",
            "a request with this Idempotency-Key is still running; retry",
        );
        error.detail.error_type = ErrorType::Idempotency;
        error
    }

    /// Returns a request over the account's or the platform's rate limit (design §12).
    #[must_use]
    pub fn too_many_requests() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit",
            "too many requests; retry with exponential backoff",
        )
    }

    /// Returns a roll of a revoked or already rolled key.
    #[must_use]
    pub fn api_key_inactive() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "api_key_inactive",
            "the API key is revoked or already rolled",
        )
    }

    /// Returns a revoke that would leave the account's mode without a non-expiring key.
    #[must_use]
    pub fn last_api_key() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "last_api_key",
            "the account's last active key cannot be revoked; create or roll a key first",
        )
    }

    /// Returns a live-mode request of an account the operator has not enabled for live mode
    /// (design D12; Stripe's code).
    #[must_use]
    pub fn testmode_charges_only() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "testmode_charges_only",
            "the account is not enabled for live mode; use a test key",
        )
    }

    /// Returns a quote creation rate-limit failure.
    #[must_use]
    pub fn rate_limited() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit",
            "quote creation limit exceeded",
        )
    }

    /// Returns a deposit address rotation over the customer's hourly limit.
    #[must_use]
    pub fn rotation_rate_limited() -> Self {
        Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit",
            "deposit address rotation limit exceeded; retry later",
        )
    }

    /// Returns a new deposit address over the account's cap of active addresses in the mode.
    #[must_use]
    pub fn deposit_address_cap(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "deposit_address_cap_exceeded",
            message,
        )
    }

    /// Returns a rotation of an already retired deposit address.
    #[must_use]
    pub fn deposit_address_retired() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "deposit_address_retired",
            "the deposit address is retired; rotate the customer's active address",
        )
    }

    /// Returns a quote or deposit address requested where the account has no treasury (design
    /// D10).
    #[must_use]
    pub fn treasury_not_set() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "treasury_not_set",
            "set a treasury on the chain first: POST /v1/treasuries/challenge, then POST \
             /v1/treasuries",
        )
    }

    /// Returns a treasury proof that does not prove the address (design D10).
    #[must_use]
    pub fn treasury_proof_invalid(param: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "treasury_proof_invalid", message).with_param(param)
    }

    /// Returns a treasury proof whose challenge expired.
    #[must_use]
    pub fn treasury_challenge_expired() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "treasury_challenge_expired",
            "the challenge has expired; request a new one",
        )
        .with_param("message")
    }

    /// Returns a treasury proof whose challenge was already used.
    #[must_use]
    pub fn treasury_challenge_used() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "treasury_challenge_used",
            "the challenge was already used; request a new one",
        )
        .with_param("message")
    }

    /// Returns a treasury proof by a contract that is not deployed on the chain.
    #[must_use]
    pub fn treasury_not_deployed() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "treasury_not_deployed",
            "the signature does not recover to the address, and no contract is deployed at it at \
             the chain's finalized block; deploy the Safe on this chain first (ERC-6492 \
             signatures are not accepted)",
        )
        .with_param("signature")
    }

    /// Returns a treasury a sanctions list names (design §8).
    #[must_use]
    pub fn treasury_sanctioned() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "treasury_sanctioned",
            "the treasury address is on a sanctions list",
        )
        .with_param("address")
    }

    /// Returns a treasury change while another one of the chain is pending.
    #[must_use]
    pub fn treasury_change_pending() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "treasury_change_pending",
            "a treasury change is already pending on this chain; cancel it first",
        )
    }

    /// Returns a treasury change to the chain's current treasury.
    #[must_use]
    pub fn treasury_unchanged() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "treasury_unchanged",
            "the address is already the chain's treasury",
        )
    }

    /// Returns a cancellation of a treasury that is not pending.
    #[must_use]
    pub fn treasury_unexpected_state(status: &str) -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "treasury_unexpected_state",
            format!("the treasury is {status}"),
        )
    }

    /// Returns a replayed request signature.
    #[must_use]
    pub fn signature_replayed() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "signature_replayed",
            "request signature has already been used",
        )
    }

    /// Returns an operation blocked by a pause scope; not retried automatically.
    #[must_use]
    pub fn paused(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "paused", message)
    }

    /// Returns an operation blocked because reconciliation froze the chain.
    #[must_use]
    pub fn chain_frozen() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "chain_frozen",
            "the route chain is frozen pending reconciliation review",
        )
    }

    /// Returns a temporary dependency failure; retry.
    #[must_use]
    pub fn service_unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "unavailable", message)
    }

    /// Returns an internal failure without exposing its cause.
    #[must_use]
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "request could not be completed",
        )
    }

    /// Names the request parameter the error is about.
    #[must_use]
    pub fn with_param(mut self, param: impl Into<String>) -> Self {
        self.detail.param = Some(param.into());
        self
    }

    /// The request parameter the error names.
    #[cfg(test)]
    pub(crate) fn param(&self) -> Option<&str> {
        self.detail.param.as_deref()
    }

    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        let error_type = if status.is_server_error() {
            ErrorType::Api
        } else {
            ErrorType::InvalidRequest
        };
        Self {
            status,
            detail: ErrorDetail {
                error_type,
                code,
                message: message.into(),
                param: None,
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(ErrorResponse { error: self.detail })).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(%error, "database operation failed");
        Self::internal()
    }
}

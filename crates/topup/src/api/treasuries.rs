//! Treasuries (`/v1/treasuries`, design D10): prove a treasury per chain with an EIP-4361 message,
//! list the account's treasuries, and cancel a pending live change.

use std::str::FromStr;

use alloy_primitives::Address;
use axum::Json;
use axum::extract::{Extension, RawQuery, State};
use chrono::Utc;
use topup_core::route::RouteFile;

use crate::ids;
use crate::refunds::DestinationScreening;
use crate::tenancy::Permission;
use crate::treasuries::{self, ListFilter, MessageOrigin, Proof, Status, TreasuryError};

use super::AppState;
use super::auth::Merchant;
use super::error::{ApiError, ErrorResponse};
use super::extract::{ApiJson, ApiPath, query_pairs};
use super::models::{
    CreateTreasuryChallengeRequest, CreateTreasuryRequest, Treasury, TreasuryChallenge,
    TreasuryList,
};

type ApiResult<T> = Result<T, ApiError>;

const DEFAULT_LIMIT: i64 = 10;
const MAX_LIMIT: i64 = 100;
/// Longest accepted message; a challenge is about 400 characters.
const MAX_MESSAGE_CHARS: usize = 2_048;
/// Longest accepted signature in bytes; a Safe's is 65 per owner.
const MAX_SIGNATURE_BYTES: usize = 8_192;

#[utoipa::path(
    post,
    path = "/v1/treasuries/challenge",
    params(
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    request_body = CreateTreasuryChallengeRequest,
    responses(
        (status = 200, description = "OK", body = TreasuryChallenge),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "treasuries"
)]
/// Issues the EIP-4361 message that proves `address` as your treasury on `chain_id` in the key's
/// mode. Sign it and send it to `POST /v1/treasuries` before `expires_at`: 10 minutes for an EOA,
/// 24 hours for an address that holds code (a Safe, whose owners sign it as a Safe message); it
/// can be used once.
pub(crate) async fn create_treasury_challenge(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiJson(request): ApiJson<CreateTreasuryChallengeRequest>,
) -> ApiResult<Json<TreasuryChallenge>> {
    merchant
        .require(&state.pool, Permission::TreasuryWrite)
        .await?;
    chain_route(&state, merchant.scope.livemode(), request.chain_id)?;
    let address = parse_address(&request.address)?;
    // A contract wallet's owners need longer than an EOA to sign (design D10).
    let ttl = if state
        .contract_signatures
        .has_code(request.chain_id, address)
        .await
    {
        treasuries::CONTRACT_CHALLENGE_TTL
    } else {
        treasuries::CHALLENGE_TTL
    };
    let challenge = treasuries::create_challenge(
        &state.pool,
        merchant.scope,
        &merchant.account.public_id,
        &MessageOrigin::new(&state.public_origin),
        request.chain_id,
        address,
        ttl,
    )
    .await
    .map_err(map_error)?;
    Ok(Json(TreasuryChallenge {
        object: "treasury_challenge".to_owned(),
        livemode: merchant.scope.livemode(),
        chain_id: challenge.chain_id,
        address: request.address,
        nonce: challenge.nonce,
        message: challenge.message,
        expires_at: challenge.expires_at.timestamp(),
    }))
}

#[utoipa::path(
    post,
    path = "/v1/treasuries",
    params(
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    request_body = CreateTreasuryRequest,
    responses(
        (status = 200, description = "OK: `active`, or `pending` for a later live change", body = Treasury),
        (
            status = 400,
            description = "`treasury_proof_invalid`, `treasury_challenge_expired`, \
                           `treasury_challenge_used`, `treasury_not_deployed` (no contract at \
                           the address at the chain's finalized block; ERC-6492 signatures are \
                           refused), or `treasury_sanctioned`",
            body = ErrorResponse
        ),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse),
        (
            status = 409,
            description = "`treasury_change_pending`, `treasury_unchanged`, or \
                           `idempotency_key_in_use`",
            body = ErrorResponse
        ),
        (status = 503, description = "The chain or sanctions screening could not be read; retry", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "treasuries"
)]
/// Sets the chain's treasury with a signed challenge. An EOA's `personal_sign` signature must
/// recover to the address; otherwise a contract deployed at the address must return `0x1626ba7e`
/// from EIP-1271 `isValidSignature` for the message's EIP-191 hash at the chain's `finalized`
/// block on both of the service's RPC providers. The address is screened against sanctions lists.
///
/// The chain's first treasury, and any test-mode change, applies at once. A later live change is
/// `pending` for 48 hours (`account.treasury.pending`), then applies
/// (`account.treasury.updated`) unless canceled first: new quotes and deposit address networks
/// then pay it, while addresses issued before keep paying the former treasury and are still
/// credited. Treasury events go to every enabled webhook endpoint of the mode.
pub(crate) async fn create_treasury(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiJson(request): ApiJson<CreateTreasuryRequest>,
) -> ApiResult<Json<Treasury>> {
    merchant
        .require(&state.pool, Permission::TreasuryWrite)
        .await?;
    let route = chain_route(&state, merchant.scope.livemode(), request.chain_id)?;
    if request.message.chars().count() > MAX_MESSAGE_CHARS {
        return Err(ApiError::invalid_param("message", "message is too long"));
    }
    let signature = parse_signature(&request.signature)?;
    let now = Utc::now();
    let challenge = treasuries::find_challenge(
        &state.pool,
        merchant.scope,
        request.chain_id,
        &request.message,
        now,
    )
    .await
    .map_err(map_error)?;
    let kind = treasuries::verify_signature(&*state.contract_signatures, &challenge, &signature)
        .await
        .map_err(map_error)?;
    match state.screening.screen(route, challenge.address).await {
        DestinationScreening::Clear => {}
        DestinationScreening::Sanctioned => return Err(ApiError::treasury_sanctioned()),
        DestinationScreening::Unavailable => {
            return Err(ApiError::service_unavailable(
                "sanctions screening of the treasury is unavailable; retry",
            ));
        }
    }
    let treasury = treasuries::submit(
        &state.pool,
        &state.routes,
        merchant.scope,
        &merchant.actor(),
        &Proof {
            challenge,
            kind,
            signature,
        },
        now,
    )
    .await
    .map_err(map_error)?;
    Ok(Json(treasury_object(&treasury)))
}

#[utoipa::path(
    get,
    path = "/v1/treasuries",
    params(
        ("chain_id" = Option<u64>, Query, description = "Only this chain's treasuries"),
        ("status" = Option<String>, Query, description = "`pending`, `active`, `replaced`, or `canceled`"),
        ("limit" = Option<i64>, Query, description = "1 to 100, default 10")
    ),
    responses(
        (status = 200, description = "OK", body = TreasuryList),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "treasuries"
)]
/// The account's treasuries in the key's mode, newest first: each chain's `active` one, and any
/// `pending` change.
pub(crate) async fn list_treasuries(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<TreasuryList>> {
    merchant
        .require(&state.pool, Permission::TreasuryRead)
        .await?;
    let mut filter = ListFilter::default();
    let mut limit = DEFAULT_LIMIT;
    for (name, value) in query_pairs(query.as_deref()) {
        match name.as_str() {
            "chain_id" => {
                filter.chain_id = Some(
                    value
                        .parse()
                        .map_err(|_| ApiError::invalid_param("chain_id", "not a chain id"))?,
                );
            }
            "status" => {
                filter.status = Some(
                    Status::parse(&value)
                        .ok_or_else(|| ApiError::invalid_param("status", "unknown status"))?,
                );
            }
            "limit" => {
                limit = value
                    .parse::<i64>()
                    .ok()
                    .filter(|limit| (1..=MAX_LIMIT).contains(limit))
                    .ok_or_else(|| ApiError::invalid_param("limit", "limit must be 1 to 100"))?;
            }
            other => {
                return Err(
                    ApiError::unknown_param(format!("unknown parameter {other}")).with_param(other),
                );
            }
        }
    }
    let (data, has_more) = treasuries::list(&state.pool, merchant.scope, filter, limit)
        .await
        .map_err(map_error)?;
    Ok(Json(TreasuryList {
        object: "list".to_owned(),
        url: "/v1/treasuries".to_owned(),
        has_more,
        data: data.iter().map(treasury_object).collect(),
    }))
}

#[utoipa::path(
    get,
    path = "/v1/treasuries/{id}",
    params(("id" = String, Path, description = "Treasury id, `trs_…`")),
    responses(
        (status = 200, description = "OK", body = Treasury),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "treasuries"
)]
/// One treasury.
pub(crate) async fn get_treasury(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<Json<Treasury>> {
    merchant
        .require(&state.pool, Permission::TreasuryRead)
        .await?;
    let id = ids::parse(ids::TREASURY, &id).ok_or_else(ApiError::not_found)?;
    treasuries::get(&state.pool, merchant.scope, id)
        .await
        .map_err(map_error)?
        .map(|treasury| Json(treasury_object(&treasury)))
        .ok_or_else(ApiError::not_found)
}

#[utoipa::path(
    post,
    path = "/v1/treasuries/{id}/cancel",
    params(
        ("id" = String, Path, description = "Treasury id, `trs_…`"),
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    responses(
        (status = 200, description = "OK: `canceled`", body = Treasury),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse),
        (status = 409, description = "`treasury_unexpected_state`: not pending", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "treasuries"
)]
/// Cancels a pending treasury change before it applies (`account.treasury.canceled`); the
/// chain's current treasury stays. If you did not request the change, also roll your keys.
pub(crate) async fn cancel_treasury(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<Json<Treasury>> {
    merchant
        .require(&state.pool, Permission::TreasuryWrite)
        .await?;
    let id = ids::parse(ids::TREASURY, &id).ok_or_else(ApiError::not_found)?;
    let treasury = treasuries::cancel(&state.pool, merchant.scope, &merchant.actor(), id)
        .await
        .map_err(map_error)?;
    Ok(Json(treasury_object(&treasury)))
}

/// The API representation of a treasury.
pub(crate) fn treasury_object(treasury: &treasuries::Treasury) -> Treasury {
    Treasury {
        id: treasuries::public_id(treasury.id),
        object: "treasury".to_owned(),
        livemode: treasury.livemode,
        chain_id: treasury.chain_id,
        address: format!("{:#x}", treasury.address),
        kind: treasury.kind.code().to_owned(),
        status: treasury.status.code().to_owned(),
        effective_at: treasury.effective_at.timestamp(),
        created: treasury.created_at.timestamp(),
        replaced_at: treasury.replaced_at.map(|at| at.timestamp()),
        canceled_at: treasury.canceled_at.map(|at| at.timestamp()),
        cancellation_reason: treasury
            .cancellation_reason
            .map(|reason| reason.code().to_owned()),
    }
}

/// A current route of the mode on `chain_id`, whose sanctions oracle screens the treasury.
fn chain_route(state: &AppState, livemode: bool, chain_id: u64) -> ApiResult<&RouteFile> {
    state
        .routes
        .current_in(livemode)
        .find(|route| route.chain.chain_id == chain_id)
        .ok_or_else(|| {
            ApiError::invalid_param("chain_id", "no route of the key's mode is on chain_id")
        })
}

fn parse_address(value: &str) -> ApiResult<Address> {
    let invalid = || ApiError::invalid_param("address", "address must be 0x and 40 hex digits");
    if value.len() != 42 || !value.starts_with("0x") {
        return Err(invalid());
    }
    let address = Address::from_str(value).map_err(|_| invalid())?;
    if address.is_zero() {
        return Err(invalid());
    }
    Ok(address)
}

fn parse_signature(value: &str) -> ApiResult<Vec<u8>> {
    let hex_digits = value.strip_prefix("0x").ok_or_else(|| {
        ApiError::invalid_param("signature", "signature must be 0x and hex digits")
    })?;
    let bytes = hex::decode(hex_digits)
        .map_err(|_| ApiError::invalid_param("signature", "signature must be 0x and hex digits"))?;
    if bytes.len() > MAX_SIGNATURE_BYTES {
        return Err(ApiError::invalid_param(
            "signature",
            "signature is too long",
        ));
    }
    Ok(bytes)
}

fn map_error(error: TreasuryError) -> ApiError {
    match error {
        TreasuryError::NotFound => ApiError::not_found(),
        TreasuryError::MessageInvalid(message) => {
            ApiError::treasury_proof_invalid("message", message)
        }
        TreasuryError::ChallengeUnknown => ApiError::treasury_proof_invalid(
            "message",
            "the message's nonce is not a challenge of this account and mode",
        ),
        TreasuryError::ChallengeExpired => ApiError::treasury_challenge_expired(),
        TreasuryError::ChallengeUsed => ApiError::treasury_challenge_used(),
        TreasuryError::ChainMismatch => ApiError::treasury_proof_invalid(
            "chain_id",
            "the message's chain differs from chain_id",
        ),
        TreasuryError::Erc6492 => ApiError::treasury_proof_invalid(
            "signature",
            "ERC-6492 signatures are not accepted: the treasury must be deployed on the chain",
        ),
        TreasuryError::SignatureInvalid => ApiError::treasury_proof_invalid(
            "signature",
            "the signature is neither the address's EIP-191 signature of the message nor \
             accepted by its contract's EIP-1271 isValidSignature at the finalized block",
        ),
        TreasuryError::NotDeployed => ApiError::treasury_not_deployed(),
        TreasuryError::Unavailable => {
            ApiError::service_unavailable("the chain could not be read; retry")
        }
        TreasuryError::ChangePending => ApiError::treasury_change_pending(),
        TreasuryError::Unchanged => ApiError::treasury_unchanged(),
        TreasuryError::NotPending(status) => ApiError::treasury_unexpected_state(status.code()),
        TreasuryError::EntropyUnavailable
        | TreasuryError::DatabaseInvariant
        | TreasuryError::DepositAddresses(_) => {
            tracing::error!(%error, "treasury operation failed");
            ApiError::internal()
        }
        TreasuryError::Database(error) => ApiError::from(error),
    }
}

//! Treasuries (docs/design/multi-tenant.md D10): the address each account's forwarders pay on a
//! chain, set through the API with a proof, time-locked when a live one changes, and announced as
//! account events.
//!
//! **Proof.** `POST /v1/treasuries/challenge` issues an EIP-4361 (Sign-In with Ethereum) message
//! for `(account, mode, chain, address)`: `domain` is the authority of the service's public origin
//! and `uri` the origin, the statement names the account and mode, the nonce is single-use, and the
//! message expires after [`CHALLENGE_TTL`]. The merchant signs it and submits it unchanged. The
//! signature proves the treasury when either
//!
//! - it is a 65-byte ECDSA signature whose EIP-191 `personal_sign` recovery is the address (an
//!   EOA), or
//! - a contract is deployed at the address at the chain's `finalized` block and its EIP-1271
//!   `isValidSignature(hash, signature)` returns the magic value `0x1626ba7e` there, where `hash` is
//!   the message's EIP-191 hash, on both RPC providers (a Safe, with the owners' signatures of the
//!   Safe message or a `SignMessageLib` approval).
//!
//! An ERC-6492 signature (the magic suffix `0x6492…6492`, for a contract not deployed yet) is
//! refused, as is a contract that is not deployed on the chain: the treasury must exist there.
//! The message is rendered and parsed by the `siwe` crate and the signature recovered by Alloy.
//!
//! **Changes.** The first treasury of a chain, and every test-mode change, applies at once
//! (`treasury.created`, `current`). A later live change is created `pending`
//! (`treasury.created`) and applies [`TIME_LOCK`] after it is proven (`treasury.updated`), unless
//! the merchant cancels it first (`treasury.canceled`); a leaked key therefore cannot redirect
//! new payments unseen. The treasury it replaces becomes `replaced` (`treasury.updated`). When a
//! treasury applies the chain's network of
//! every deposit address of the account and mode is replaced by a forwarder over it, in the same
//! transaction; the replaced forwarders stay watched and credited and keep paying the old treasury,
//! which the forwarder's clone argument fixes for good. Quotes and new networks take the chain's
//! current treasury; quotes issued before keep their address.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use alloy_primitives::{Address, B256, Bytes, Signature, eip191_hash_message};
use async_trait::async_trait;
use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rand::TryRng as _;
use rand::rngs::SysRng;
use sqlx::{FromRow, PgConnection, PgPool, Postgres, Transaction};
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use topup_adapters::chain::evm::EvmClient;
use topup_core::route::RouteFile;
use uuid::Uuid;

use crate::api_keys::event_actor;
use crate::audit::{self, Actor};
use crate::deposit_addresses::{self, ChainContracts};
use crate::refunds::{DestinationScreener, DestinationScreening};
use crate::routes::RouteSet;
use crate::tenancy::Scope;

/// How long a later live treasury change waits before it applies.
pub const TIME_LOCK: Duration = Duration::hours(48);
/// How long a challenge for an EOA can be submitted.
pub const CHALLENGE_TTL: Duration = Duration::minutes(10);
/// How long a challenge for an address that holds code (a Safe) can be submitted: its owners
/// collect signatures, or approve the message on chain and wait for `finalized`, which takes
/// longer than an EOA's signature.
pub const CONTRACT_CHALLENGE_TTL: Duration = Duration::hours(24);
/// How often every current treasury is screened again.
pub const RESCREEN_INTERVAL: Duration = Duration::days(1);
/// Treasuries screened per pass of the worker.
const SCREEN_BATCH: i64 = 100;
/// How long spent and expired challenges are kept before they are pruned.
const CHALLENGE_RETENTION: Duration = Duration::days(1);

/// The ERC-6492 wrapper's magic suffix (<https://eips.ethereum.org/EIPS/eip-6492>).
const ERC6492_MAGIC_SUFFIX: [u8; 32] = [
    0x64, 0x92, 0x64, 0x92, 0x64, 0x92, 0x64, 0x92, 0x64, 0x92, 0x64, 0x92, 0x64, 0x92, 0x64, 0x92,
    0x64, 0x92, 0x64, 0x92, 0x64, 0x92, 0x64, 0x92, 0x64, 0x92, 0x64, 0x92, 0x64, 0x92, 0x64, 0x92,
];

/// The treasury event types. They are account security events: delivered to every enabled
/// endpoint of the mode whatever its `enabled_events` (design §11).
pub const EVENT_TYPES: [&str; 3] = ["treasury.created", "treasury.updated", "treasury.canceled"];

/// The public id of a treasury, `trs_` and the hex of its id.
#[must_use]
pub fn public_id(id: Uuid) -> String {
    crate::ids::format(crate::ids::TREASURY, id)
}

/// How the treasury proved control of its address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    /// An EIP-191 signature recovering to the address.
    Eoa,
    /// A deployed contract's EIP-1271 answer.
    Contract,
}

impl Kind {
    /// The stable API and database code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Eoa => "eoa",
            Self::Contract => "contract",
        }
    }

    fn parse(value: &str) -> Result<Self, TreasuryError> {
        match value {
            "eoa" => Ok(Self::Eoa),
            "contract" => Ok(Self::Contract),
            _ => Err(TreasuryError::DatabaseInvariant),
        }
    }
}

/// Lifecycle of a treasury.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    /// A live change waiting for its time-lock.
    Pending,
    /// The chain's current treasury.
    Active,
    /// A former treasury, replaced by a later one; forwarders issued over it still pay it.
    Replaced,
    /// A pending change the merchant canceled.
    Canceled,
}

impl Status {
    /// The stable API code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Replaced => "replaced",
            Self::Canceled => "canceled",
        }
    }

    /// Parses an API code.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "active" => Some(Self::Active),
            "replaced" => Some(Self::Replaced),
            "canceled" => Some(Self::Canceled),
            _ => None,
        }
    }

    /// The SQL condition selecting treasuries in this status.
    const fn condition(self) -> &'static str {
        match self {
            Self::Pending => "applied_at IS NULL AND canceled_at IS NULL",
            Self::Active => "applied_at IS NOT NULL AND replaced_at IS NULL",
            Self::Replaced => "replaced_at IS NOT NULL",
            Self::Canceled => "canceled_at IS NOT NULL",
        }
    }
}

/// Why a pending treasury change was canceled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancellationReason {
    /// The merchant canceled it (`POST /v1/treasuries/{id}/cancel`).
    Requested,
    /// A sanctions list named the treasury when it was due to apply.
    Sanctioned,
}

impl CancellationReason {
    /// The stable API and database code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Sanctioned => "sanctioned",
        }
    }

    fn parse(value: &str) -> Result<Self, TreasuryError> {
        match value {
            "requested" => Ok(Self::Requested),
            "sanctioned" => Ok(Self::Sanctioned),
            _ => Err(TreasuryError::DatabaseInvariant),
        }
    }
}

/// An account's treasury of one chain and mode.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Treasury {
    /// Treasury identifier.
    pub id: Uuid,
    /// Mode.
    pub livemode: bool,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// The treasury address.
    pub address: Address,
    /// How it was proven.
    pub kind: Kind,
    /// Lifecycle status.
    pub status: Status,
    /// When it applies, or applied.
    pub effective_at: DateTime<Utc>,
    /// When it was proven.
    pub created_at: DateTime<Utc>,
    /// When a later treasury replaced it.
    pub replaced_at: Option<DateTime<Utc>>,
    /// When it was canceled.
    pub canceled_at: Option<DateTime<Utc>>,
    /// Why it was canceled.
    pub cancellation_reason: Option<CancellationReason>,
}

/// An issued EIP-4361 challenge.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Challenge {
    /// Single-use nonce.
    pub nonce: String,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// The address to prove.
    pub address: Address,
    /// The message to sign, exactly.
    pub message: String,
    /// When the message stops being accepted.
    pub expires_at: DateTime<Utc>,
}

/// The service's identity in challenge messages: EIP-4361's `domain` (the authority of the public
/// origin) and `uri` (the origin).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageOrigin {
    domain: String,
    uri: String,
}

impl MessageOrigin {
    /// The identity of the service at `origin`, such as `https://api.example`.
    #[must_use]
    pub fn new(origin: &crate::api::PublicOrigin) -> Self {
        let uri = origin.to_string();
        let domain = uri
            .split_once("://")
            .map_or(uri.as_str(), |(_, authority)| authority)
            .to_owned();
        Self { domain, uri }
    }
}

/// Treasury failure mapped by the API boundary.
#[derive(Debug, thiserror::Error)]
pub enum TreasuryError {
    /// No such treasury in the scope.
    #[error("treasury not found")]
    NotFound,
    /// The message is not an EIP-4361 message, or not the challenge's message.
    #[error("{0}")]
    MessageInvalid(&'static str),
    /// The message's nonce is not a challenge of this account and mode.
    #[error("the message's nonce is not a challenge of this account and mode")]
    ChallengeUnknown,
    /// The challenge expired.
    #[error("the challenge has expired; request a new one")]
    ChallengeExpired,
    /// The challenge was already used.
    #[error("the challenge was already used; request a new one")]
    ChallengeUsed,
    /// The message is for another chain than the request's.
    #[error("the message's chain differs from chain_id")]
    ChainMismatch,
    /// An ERC-6492 signature of a contract that is not deployed.
    #[error("ERC-6492 signatures are not accepted; deploy the treasury on the chain first")]
    Erc6492,
    /// Neither an EOA's nor the deployed contract's signature of the message.
    #[error("the signature does not prove the address")]
    SignatureInvalid,
    /// Not an EOA's signature, and no contract is deployed at the address.
    #[error("no contract is deployed at the address at the chain's finalized block")]
    NotDeployed,
    /// A chain read failed, or the providers disagree; retry.
    #[error("the chain could not be read")]
    Unavailable,
    /// A change of the chain's treasury is already pending.
    #[error("a treasury change is already pending on this chain")]
    ChangePending,
    /// The address is already the chain's treasury.
    #[error("the address is already the chain's treasury")]
    Unchanged,
    /// The treasury is not pending, so it cannot be canceled.
    #[error("the treasury is {}", .0.code())]
    NotPending(Status),
    /// The OS RNG failed.
    #[error("entropy unavailable")]
    EntropyUnavailable,
    /// Persisted data violated an internal invariant.
    #[error("treasury database invariant failed")]
    DatabaseInvariant,
    /// Deposit address networks could not be replaced.
    #[error("{0}")]
    DepositAddresses(#[from] deposit_addresses::DepositAddressError),
    /// PostgreSQL failed.
    #[error("{0}")]
    Database(#[from] sqlx::Error),
}

/// The EIP-4361 statement of a challenge: what the signer agrees to.
fn statement(account_public_id: &str, livemode: bool) -> String {
    let mode = if livemode { "live" } else { "test" };
    format!("Set this address as the {mode} mode treasury of {account_public_id} on Phala Pay.")
}

fn timestamp(time: DateTime<Utc>) -> Result<siwe::TimeStamp, TreasuryError> {
    time.to_rfc3339_opts(SecondsFormat::Millis, true)
        .parse()
        .map_err(|_| TreasuryError::DatabaseInvariant)
}

/// Renders the EIP-4361 message of a challenge with `statement`, issued at `issued_at` and
/// expiring at `expires_at`.
fn render_message(
    origin: &MessageOrigin,
    statement: String,
    chain_id: u64,
    address: Address,
    nonce: &str,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Result<String, TreasuryError> {
    let message = siwe::Message {
        domain: origin
            .domain
            .parse()
            .map_err(|_| TreasuryError::DatabaseInvariant)?,
        address: address.into_array(),
        statement: Some(statement),
        uri: origin
            .uri
            .parse()
            .map_err(|_| TreasuryError::DatabaseInvariant)?,
        version: siwe::Version::V1,
        chain_id,
        nonce: nonce.to_owned(),
        issued_at: timestamp(issued_at)?,
        expiration_time: Some(timestamp(expires_at)?),
        not_before: None,
        request_id: None,
        resources: Vec::new(),
    };
    Ok(message.to_string())
}

/// Issues a challenge to prove `address` as the scope's treasury on `chain_id`, valid for `ttl`:
/// [`CHALLENGE_TTL`] for an EOA, [`CONTRACT_CHALLENGE_TTL`] for an address that holds code.
pub async fn create_challenge(
    pool: &PgPool,
    scope: Scope,
    account_public_id: &str,
    origin: &MessageOrigin,
    chain_id: u64,
    address: Address,
    ttl: Duration,
) -> Result<Challenge, TreasuryError> {
    let now = Utc::now();
    let mut random = [0_u8; 16];
    SysRng.try_fill_bytes(&mut random).map_err(|error| {
        tracing::error!(%error, "OS RNG failed; no treasury challenge issued");
        TreasuryError::EntropyUnavailable
    })?;
    // EIP-4361 nonces are at least 8 alphanumeric characters; 128 random bits as hex.
    let nonce = hex::encode(random);
    let expires_at = now
        .checked_add_signed(ttl)
        .ok_or(TreasuryError::DatabaseInvariant)?;
    let message = render_message(
        origin,
        statement(account_public_id, scope.livemode()),
        chain_id,
        address,
        &nonce,
        now,
        expires_at,
    )?;
    sqlx::query(
        r#"
        INSERT INTO treasury_challenges
            (nonce, account_id, livemode, chain_id, address, message, expires_at, created_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        "#,
    )
    .bind(&nonce)
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(i64::try_from(chain_id).map_err(|_| TreasuryError::DatabaseInvariant)?)
    .bind(format!("{address:#x}"))
    .bind(&message)
    .bind(expires_at)
    .bind(now)
    .execute(pool)
    .await?;
    Ok(Challenge {
        nonce,
        chain_id,
        address,
        message,
        expires_at,
    })
}

/// Finds the scope's challenge `message` answers, for a submission to `chain_id`, without using
/// it: the message must be the challenge's exactly, unexpired, and unused.
pub async fn find_challenge(
    pool: &PgPool,
    scope: Scope,
    chain_id: u64,
    message: &str,
    now: DateTime<Utc>,
) -> Result<Challenge, TreasuryError> {
    let parsed = siwe::Message::from_str(message)
        .map_err(|_| TreasuryError::MessageInvalid("message is not an EIP-4361 message"))?;
    if parsed.chain_id != chain_id {
        return Err(TreasuryError::ChainMismatch);
    }
    let row = sqlx::query_as::<_, (i64, String, String, DateTime<Utc>, Option<DateTime<Utc>>)>(
        r#"
        SELECT chain_id, address, message, expires_at, used_at
        FROM treasury_challenges
        WHERE nonce = $1 AND account_id = $2 AND livemode = $3
        "#,
    )
    .bind(&parsed.nonce)
    .bind(scope.account_id())
    .bind(scope.livemode())
    .fetch_optional(pool)
    .await?;
    let Some((stored_chain, address, stored_message, expires_at, used_at)) = row else {
        return Err(TreasuryError::ChallengeUnknown);
    };
    if stored_message != message {
        return Err(TreasuryError::MessageInvalid(
            "message differs from the challenge's; sign it exactly as issued",
        ));
    }
    if used_at.is_some() {
        return Err(TreasuryError::ChallengeUsed);
    }
    if expires_at <= now {
        return Err(TreasuryError::ChallengeExpired);
    }
    let stored_chain = u64::try_from(stored_chain).map_err(|_| TreasuryError::DatabaseInvariant)?;
    let address = Address::from_str(&address).map_err(|_| TreasuryError::DatabaseInvariant)?;
    // The message is the stored one, so its fields are the challenge's; checked all the same.
    if stored_chain != chain_id || Address::from(parsed.address) != address {
        return Err(TreasuryError::DatabaseInvariant);
    }
    Ok(Challenge {
        nonce: parsed.nonce,
        chain_id,
        address,
        message: stored_message,
        expires_at,
    })
}

/// A contract's answer to an EIP-1271 check, as both providers agree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContractAnswer {
    /// A contract is deployed and returned the magic value.
    Valid,
    /// A contract is deployed and did not return the magic value.
    Invalid,
    /// No contract is deployed at the address.
    NotDeployed,
    /// A read failed or the providers disagree.
    Unavailable,
}

/// EIP-1271 checks at the chain's `finalized` block.
#[async_trait]
pub trait ContractSignatures: Send + Sync {
    /// Whether `account` on `chain_id` holds code at provider A's latest block, which gives its
    /// challenge the longer [`CONTRACT_CHALLENGE_TTL`]; `false` when the read fails.
    async fn has_code(&self, chain_id: u64, account: Address) -> bool;

    /// Whether the contract at `account` on `chain_id` accepts `signature` of `hash`.
    async fn verify(
        &self,
        chain_id: u64,
        account: Address,
        hash: B256,
        signature: Bytes,
    ) -> ContractAnswer;
}

/// EIP-1271 checks on both RPC providers of each chain, each at its own `finalized` block.
pub struct EvmContractSignatures {
    clients: BTreeMap<u64, [Arc<EvmClient>; 2]>,
}

impl EvmContractSignatures {
    /// Checks through providers A and B of every chain of `routes`.
    pub fn from_routes(routes: &RouteSet) -> Result<Self, String> {
        let mut clients = BTreeMap::new();
        for chain_id in routes.chain_ids() {
            let provider = |index| {
                routes
                    .provider(chain_id, index)
                    .map(Arc::clone)
                    .map_err(|error| error.to_string())
            };
            clients.insert(chain_id, [provider(0)?, provider(1)?]);
        }
        Ok(Self::new(clients))
    }

    /// Checks through explicit per-chain providers A and B.
    #[must_use]
    pub const fn new(clients: BTreeMap<u64, [Arc<EvmClient>; 2]>) -> Self {
        Self { clients }
    }
}

async fn provider_answer(
    client: &EvmClient,
    account: Address,
    hash: B256,
    signature: Bytes,
) -> ContractAnswer {
    let Ok(Some(block)) = client.finalized_block().await else {
        return ContractAnswer::Unavailable;
    };
    match client.code_at_block(account, block).await {
        Ok(code) if code.is_empty() => return ContractAnswer::NotDeployed,
        Ok(_) => {}
        Err(_) => return ContractAnswer::Unavailable,
    }
    match client
        .is_valid_signature(account, hash, signature, block)
        .await
    {
        Ok(true) => ContractAnswer::Valid,
        Ok(false) => ContractAnswer::Invalid,
        Err(_) => ContractAnswer::Unavailable,
    }
}

#[async_trait]
impl ContractSignatures for EvmContractSignatures {
    async fn has_code(&self, chain_id: u64, account: Address) -> bool {
        let Some([primary, _]) = self.clients.get(&chain_id) else {
            return false;
        };
        match primary.code_at(account).await {
            Ok(code) => !code.is_empty(),
            Err(error) => {
                tracing::warn!(chain_id, %error, "treasury challenge code read failed");
                false
            }
        }
    }

    async fn verify(
        &self,
        chain_id: u64,
        account: Address,
        hash: B256,
        signature: Bytes,
    ) -> ContractAnswer {
        let Some([primary, secondary]) = self.clients.get(&chain_id) else {
            return ContractAnswer::Unavailable;
        };
        let (a, b) = tokio::join!(
            provider_answer(primary, account, hash, signature.clone()),
            provider_answer(secondary, account, hash, signature)
        );
        match (a, b) {
            (a, b) if a == b => a,
            (ContractAnswer::Unavailable, _) | (_, ContractAnswer::Unavailable) => {
                ContractAnswer::Unavailable
            }
            // A provider whose contract refuses the signature: not proven.
            (ContractAnswer::Invalid, _) | (_, ContractAnswer::Invalid) => ContractAnswer::Invalid,
            _ => ContractAnswer::Unavailable,
        }
    }
}

/// EIP-1271 checks that are never available, for an instance that sets no treasuries.
pub struct UnavailableContractSignatures;

#[async_trait]
impl ContractSignatures for UnavailableContractSignatures {
    async fn has_code(&self, _: u64, _: Address) -> bool {
        false
    }

    async fn verify(&self, _: u64, _: Address, _: B256, _: Bytes) -> ContractAnswer {
        ContractAnswer::Unavailable
    }
}

/// Checks that `signature` of the challenge's message proves its address (module docs): an EOA's
/// EIP-191 signature, or a deployed contract's EIP-1271 approval on both providers at
/// `finalized`.
pub async fn verify_signature(
    contracts: &dyn ContractSignatures,
    challenge: &Challenge,
    signature: &[u8],
) -> Result<Kind, TreasuryError> {
    if signature.ends_with(&ERC6492_MAGIC_SUFFIX) {
        return Err(TreasuryError::Erc6492);
    }
    let message = challenge.message.as_bytes();
    if let Ok(parsed) = Signature::try_from(signature)
        && parsed.recover_address_from_msg(message).ok() == Some(challenge.address)
    {
        return Ok(Kind::Eoa);
    }
    match contracts
        .verify(
            challenge.chain_id,
            challenge.address,
            eip191_hash_message(message),
            Bytes::copy_from_slice(signature),
        )
        .await
    {
        ContractAnswer::Valid => Ok(Kind::Contract),
        ContractAnswer::Invalid => Err(TreasuryError::SignatureInvalid),
        ContractAnswer::NotDeployed => Err(TreasuryError::NotDeployed),
        ContractAnswer::Unavailable => Err(TreasuryError::Unavailable),
    }
}

/// A verified, screened proof, ready to be recorded.
#[derive(Clone, Debug)]
pub struct Proof {
    /// The challenge answered.
    pub challenge: Challenge,
    /// How the address was proven.
    pub kind: Kind,
    /// The signature, as submitted.
    pub signature: Vec<u8>,
}

/// Records `proof` as the scope's treasury of its chain, using its challenge. The first treasury
/// of a chain and any test-mode change apply at once; a later live change waits [`TIME_LOCK`].
/// `routes` supply the chain's forwarder contracts for the deposit address networks.
pub async fn submit(
    pool: &PgPool,
    routes: &RouteSet,
    scope: Scope,
    actor: &Actor,
    proof: &Proof,
    now: DateTime<Utc>,
) -> Result<Treasury, TreasuryError> {
    let challenge = &proof.challenge;
    let chain_id =
        i64::try_from(challenge.chain_id).map_err(|_| TreasuryError::DatabaseInvariant)?;
    let mut transaction = pool.begin().await?;
    lock(&mut transaction, scope, true).await?;
    let used = sqlx::query(
        "UPDATE treasury_challenges SET used_at = $2 \
         WHERE nonce = $1 AND used_at IS NULL AND expires_at > $2",
    )
    .bind(&challenge.nonce)
    .bind(now)
    .execute(&mut *transaction)
    .await?
    .rows_affected();
    if used != 1 {
        return Err(TreasuryError::ChallengeUsed);
    }
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM treasuries \
         WHERE account_id = $1 AND livemode = $2 AND chain_id = $3 \
           AND applied_at IS NULL AND canceled_at IS NULL)",
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(chain_id)
    .fetch_one(&mut *transaction)
    .await?;
    if pending {
        return Err(TreasuryError::ChangePending);
    }
    let current = current_on(&mut transaction, scope, challenge.chain_id).await?;
    if current == Some(challenge.address) {
        return Err(TreasuryError::Unchanged);
    }
    let immediate = !scope.livemode() || current.is_none();
    let effective_at = if immediate {
        now
    } else {
        now.checked_add_signed(TIME_LOCK)
            .ok_or(TreasuryError::DatabaseInvariant)?
    };
    let id = Uuid::new_v4();
    sqlx::query(
        r#"
        INSERT INTO treasuries (
            id, account_id, livemode, chain_id, address, kind, proof_message, proof_signature,
            verified_at, effective_at, screened_at, created_by, created_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $9, $11, $9)
        "#,
    )
    .bind(id)
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(chain_id)
    .bind(format!("{:#x}", challenge.address))
    .bind(proof.kind.code())
    .bind(&challenge.message)
    .bind(format!("0x{}", hex::encode(&proof.signature)))
    .bind(now)
    .bind(effective_at)
    .bind(event_actor(actor))
    .execute(&mut *transaction)
    .await?;
    if immediate {
        apply(&mut transaction, routes, scope, id, now, actor, None).await?;
    } else {
        record(
            &mut transaction,
            scope,
            id,
            None,
            actor,
            "treasury.created",
            &format!("applies at {}", effective_at.to_rfc3339()),
        )
        .await?;
    }
    let treasury = get_in(&mut transaction, scope, id)
        .await?
        .ok_or(TreasuryError::DatabaseInvariant)?;
    transaction.commit().await?;
    Ok(treasury)
}

/// Cancels the scope's pending treasury `id`.
pub async fn cancel(
    pool: &PgPool,
    scope: Scope,
    actor: &Actor,
    id: Uuid,
) -> Result<Treasury, TreasuryError> {
    let mut transaction = pool.begin().await?;
    lock(&mut transaction, scope, true).await?;
    let treasury = get_in(&mut transaction, scope, id)
        .await?
        .ok_or(TreasuryError::NotFound)?;
    if treasury.status != Status::Pending {
        return Err(TreasuryError::NotPending(treasury.status));
    }
    mark_canceled(
        &mut transaction,
        scope,
        id,
        CancellationReason::Requested,
        actor,
        "",
    )
    .await?;
    let treasury = get_in(&mut transaction, scope, id)
        .await?
        .ok_or(TreasuryError::DatabaseInvariant)?;
    transaction.commit().await?;
    Ok(treasury)
}

/// Applies every pending treasury whose time-lock ended by `now`, one transaction each, and
/// returns how many applied. Each is screened again first: a treasury a sanctions list now names
/// is canceled instead (`cancellation_reason: sanctioned`, `treasury.canceled`), and one
/// that cannot be screened now, or whose chain has no current route to screen it with, stays
/// pending until a later pass.
pub async fn apply_due(
    pool: &PgPool,
    routes: &RouteSet,
    screening: &dyn DestinationScreener,
    now: DateTime<Utc>,
) -> Result<usize, TreasuryError> {
    let actor = Actor::system("treasury_time_lock");
    let due: Vec<(Uuid, Uuid, bool, i64, String)> = sqlx::query_as(
        r#"
        SELECT id, account_id, livemode, chain_id, address FROM treasuries
        WHERE applied_at IS NULL AND canceled_at IS NULL AND effective_at <= $1
        ORDER BY effective_at, id
        LIMIT $2
        "#,
    )
    .bind(now)
    .bind(SCREEN_BATCH)
    .fetch_all(pool)
    .await?;
    let mut applied = 0_usize;
    for (id, account_id, livemode, chain_id, address) in due {
        let scope = Scope::new(account_id, livemode);
        let (chain_id, address) = parse_chain_address(chain_id, &address)?;
        let Some(route) = chain_route(routes, livemode, chain_id) else {
            tracing::warn!(
                chain_id,
                "a due treasury's chain has no current route to screen it"
            );
            continue;
        };
        let sanctioned = match screening.screen(route, address).await {
            DestinationScreening::Clear => false,
            DestinationScreening::Sanctioned => true,
            DestinationScreening::Unavailable => {
                tracing::warn!(
                    chain_id,
                    "screening a due treasury is unavailable; retrying"
                );
                continue;
            }
        };
        let mut transaction = pool.begin().await?;
        // The scope lock first, as submissions and cancellations take it, then the row.
        lock(&mut transaction, scope, true).await?;
        let still_due: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM treasuries WHERE id = $1 \
             AND applied_at IS NULL AND canceled_at IS NULL AND effective_at <= $2)",
        )
        .bind(id)
        .bind(now)
        .fetch_one(&mut *transaction)
        .await?;
        if still_due && sanctioned {
            tracing::error!(
                tags.alert = "TopupTreasurySanctioned",
                treasury = %public_id(id),
                chain_id,
                "a pending treasury is on a sanctions list; its change was canceled"
            );
            mark_canceled(
                &mut transaction,
                scope,
                id,
                CancellationReason::Sanctioned,
                &actor,
                "the treasury is on a sanctions list at its effective time",
            )
            .await?;
        } else if still_due {
            sqlx::query("UPDATE treasuries SET screened_at = $2 WHERE id = $1")
                .bind(id)
                .bind(now)
                .execute(&mut *transaction)
                .await?;
            let before = snapshot(&mut transaction, scope, id).await?;
            apply(
                &mut transaction,
                routes,
                scope,
                id,
                now,
                &actor,
                Some(&before),
            )
            .await?;
            applied = applied.saturating_add(1);
        }
        transaction.commit().await?;
    }
    Ok(applied)
}

/// What one pass of [`rescreen_due`] did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Rescreen {
    /// Current treasuries screened clear.
    pub clear: usize,
    /// Current treasuries a sanctions list names; their accounts' `quotes` and `settlement` are
    /// paused.
    pub sanctioned: usize,
}

/// Screens again every current treasury last screened [`RESCREEN_INTERVAL`] or more before `now`
/// (design §8: "screened when set and daily"), up to a batch per pass. A treasury a sanctions list
/// now names pauses its account's `quotes` and `settlement` (audited, `account.updated`, and the
/// `TopupTreasurySanctioned` alert); the operator lifts the pause after review. A treasury that
/// cannot be screened now is tried again on a later pass.
pub async fn rescreen_due(
    pool: &PgPool,
    routes: &RouteSet,
    screening: &dyn DestinationScreener,
    now: DateTime<Utc>,
) -> Result<Rescreen, TreasuryError> {
    let before = now
        .checked_sub_signed(RESCREEN_INTERVAL)
        .ok_or(TreasuryError::DatabaseInvariant)?;
    let current: Vec<(Uuid, Uuid, bool, i64, String)> = sqlx::query_as(
        r#"
        SELECT id, account_id, livemode, chain_id, address FROM treasuries
        WHERE applied_at IS NOT NULL AND replaced_at IS NULL AND screened_at <= $1
        ORDER BY screened_at, id
        LIMIT $2
        "#,
    )
    .bind(before)
    .bind(SCREEN_BATCH)
    .fetch_all(pool)
    .await?;
    let actor = Actor::system("treasury_screening");
    let mut outcome = Rescreen::default();
    for (id, account_id, livemode, chain_id, address) in current {
        let (chain_id, address) = parse_chain_address(chain_id, &address)?;
        let Some(route) = chain_route(routes, livemode, chain_id) else {
            continue;
        };
        let answer = screening.screen(route, address).await;
        if answer == DestinationScreening::Unavailable {
            tracing::warn!(chain_id, "re-screening a treasury is unavailable; retrying");
            continue;
        }
        let mut transaction = pool.begin().await?;
        match answer {
            DestinationScreening::Unavailable => {}
            DestinationScreening::Clear => outcome.clear = outcome.clear.saturating_add(1),
            DestinationScreening::Sanctioned => {
                outcome.sanctioned = outcome.sanctioned.saturating_add(1);
                tracing::error!(
                    tags.alert = "TopupTreasurySanctioned",
                    treasury = %public_id(id),
                    chain_id,
                    "a current treasury is on a sanctions list; the account's quotes and \
                     settlement are paused"
                );
                crate::pause::mutate_account_scopes_in(
                    &mut transaction,
                    routes,
                    account_id,
                    crate::pause::PauseOwner::Operator,
                    &["quotes", "settlement"],
                    true,
                    &actor,
                    &format!(
                        "treasury {} on chain {chain_id} is on a sanctions list",
                        public_id(id)
                    ),
                )
                .await?;
            }
        }
        sqlx::query("UPDATE treasuries SET screened_at = $2 WHERE id = $1")
            .bind(id)
            .bind(now)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
    }
    Ok(outcome)
}

/// A current route of the mode on `chain_id`, whose sanctions oracle screens its treasuries.
fn chain_route(routes: &RouteSet, livemode: bool, chain_id: u64) -> Option<&RouteFile> {
    routes
        .current_in(livemode)
        .find(|route| route.chain.chain_id == chain_id)
}

fn parse_chain_address(chain_id: i64, address: &str) -> Result<(u64, Address), TreasuryError> {
    Ok((
        u64::try_from(chain_id).map_err(|_| TreasuryError::DatabaseInvariant)?,
        Address::from_str(address).map_err(|_| TreasuryError::DatabaseInvariant)?,
    ))
}

/// Makes treasury `id` its chain's current one: the former one is replaced, the chain's network of
/// every deposit address of the scope is replaced by a forwarder over it, and events are sent:
/// `treasury.created` for a treasury that applies as it is submitted, `treasury.updated` with
/// `pending` (`before`, its representation until now) for one whose time-lock ended, and
/// `treasury.updated` for the replaced one. The caller holds the scope's exclusive lock.
async fn apply(
    transaction: &mut Transaction<'_, Postgres>,
    routes: &RouteSet,
    scope: Scope,
    id: Uuid,
    now: DateTime<Utc>,
    actor: &Actor,
    before: Option<&serde_json::Value>,
) -> Result<(), TreasuryError> {
    let (chain_id, address): (i64, String) =
        sqlx::query_as("SELECT chain_id, address FROM treasuries WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut **transaction)
            .await?;
    let former: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM treasuries \
         WHERE account_id = $1 AND livemode = $2 AND chain_id = $3 \
           AND applied_at IS NOT NULL AND replaced_at IS NULL \
         FOR UPDATE",
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(chain_id)
    .fetch_optional(&mut **transaction)
    .await?;
    let former = match former {
        Some(former) => Some((former, snapshot(transaction, scope, former).await?)),
        None => None,
    };
    if let Some((former, _)) = &former {
        sqlx::query("UPDATE treasuries SET replaced_at = $2 WHERE id = $1")
            .bind(former)
            .bind(now)
            .execute(&mut **transaction)
            .await?;
    }
    sqlx::query("UPDATE treasuries SET applied_at = $2 WHERE id = $1")
        .bind(id)
        .bind(now)
        .execute(&mut **transaction)
        .await?;
    let chain_id = u64::try_from(chain_id).map_err(|_| TreasuryError::DatabaseInvariant)?;
    let treasury = Address::from_str(&address).map_err(|_| TreasuryError::DatabaseInvariant)?;
    let reason = match routes
        .current_in(scope.livemode())
        .find(|route| route.chain.chain_id == chain_id)
    {
        Some(route) => {
            let chain = ChainContracts::of(route).with_treasury(treasury);
            let moved = deposit_addresses::replace_networks(transaction, scope, chain).await?;
            if former.is_none() {
                String::new()
            } else {
                format!(
                    "{moved} deposit address networks on chain {chain_id} now pay it; the \
                     replaced forwarders keep paying the former treasury and are still credited"
                )
            }
        }
        // No route of the mode serves the chain now, so no network is issued there; creation
        // adds them over this treasury when a route returns.
        None => {
            tracing::warn!(
                chain_id,
                "treasury applied on a chain without a current route"
            );
            String::new()
        }
    };
    let event_type = if before.is_some() {
        "treasury.updated"
    } else {
        "treasury.created"
    };
    record(transaction, scope, id, before, actor, event_type, &reason).await?;
    if let Some((former, before)) = former {
        record(
            transaction,
            scope,
            former,
            Some(&before),
            actor,
            "treasury.updated",
            &format!("replaced by {}", public_id(id)),
        )
        .await?;
    }
    Ok(())
}

/// The API representation of the scope's treasury `id`, as an event's `data.object`.
async fn snapshot(
    connection: &mut PgConnection,
    scope: Scope,
    id: Uuid,
) -> Result<serde_json::Value, TreasuryError> {
    let treasury = get_in(connection, scope, id)
        .await?
        .ok_or(TreasuryError::DatabaseInvariant)?;
    Ok(crate::db::to_object(&crate::api::treasury_object(
        &treasury,
    ))?)
}

/// Cancels pending treasury `id` for `reason`, with its audit row and `treasury.canceled`.
async fn mark_canceled(
    transaction: &mut Transaction<'_, Postgres>,
    scope: Scope,
    id: Uuid,
    reason: CancellationReason,
    actor: &Actor,
    note: &str,
) -> Result<(), TreasuryError> {
    sqlx::query(
        "UPDATE treasuries SET canceled_at = now(), cancellation_reason = $2 WHERE id = $1",
    )
    .bind(id)
    .bind(reason.code())
    .execute(&mut **transaction)
    .await?;
    record(
        transaction,
        scope,
        id,
        None,
        actor,
        "treasury.canceled",
        note,
    )
    .await
}

/// The audit row and event of a change to treasury `id`; `before` is its representation before
/// an update, for `previous_attributes`.
async fn record(
    transaction: &mut Transaction<'_, Postgres>,
    scope: Scope,
    id: Uuid,
    before: Option<&serde_json::Value>,
    actor: &Actor,
    event_type: &str,
    reason: &str,
) -> Result<(), TreasuryError> {
    audit::insert(
        &mut **transaction,
        &audit::Entry {
            account_id: Some(scope.account_id()),
            actor,
            action: event_type,
            subject: &format!("treasury:{}", public_id(id)),
            reason,
        },
    )
    .await?;
    let object = snapshot(transaction, scope, id).await?;
    let event = crate::db::NewOutboxEvent::new(
        event_type,
        scope,
        crate::db::EventObject::Treasury(id),
        actor,
    );
    crate::db::enqueue_rendered_in(
        transaction,
        &event,
        &crate::db::event_data(object, before),
        None,
        true,
    )
    .await?;
    Ok(())
}

/// Serializes treasury changes of the scope (exclusive) against issuers of forwarders, which read
/// the current treasuries (shared), so no forwarder is issued over a treasury being replaced.
pub(crate) async fn lock(
    transaction: &mut Transaction<'_, Postgres>,
    scope: Scope,
    exclusive: bool,
) -> Result<(), sqlx::Error> {
    let query = if exclusive {
        "SELECT pg_advisory_xact_lock(hashtextextended('treasury:' || $1, 0))"
    } else {
        "SELECT pg_advisory_xact_lock_shared(hashtextextended('treasury:' || $1, 0))"
    };
    sqlx::query(query)
        .bind(format!("{}:{}", scope.account_id(), scope.livemode()))
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

/// The scope's current treasury of every chain that has one.
pub async fn current(
    connection: &mut PgConnection,
    scope: Scope,
) -> Result<BTreeMap<u64, Address>, TreasuryError> {
    let rows = sqlx::query_as::<_, (i64, String)>(
        "SELECT chain_id, address FROM treasuries \
         WHERE account_id = $1 AND livemode = $2 AND applied_at IS NOT NULL AND replaced_at IS NULL",
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .fetch_all(connection)
    .await?;
    rows.into_iter()
        .map(|(chain_id, address)| {
            Ok((
                u64::try_from(chain_id).map_err(|_| TreasuryError::DatabaseInvariant)?,
                Address::from_str(&address).map_err(|_| TreasuryError::DatabaseInvariant)?,
            ))
        })
        .collect()
}

/// The scope's current treasury of `chain_id`, if it has one.
pub async fn current_on(
    connection: &mut PgConnection,
    scope: Scope,
    chain_id: u64,
) -> Result<Option<Address>, TreasuryError> {
    Ok(current(connection, scope).await?.remove(&chain_id))
}

/// The columns of [`TreasuryRow`]; callers append the `WHERE` clause.
const SELECT: &str = r#"
    SELECT id, livemode, chain_id, address, kind, effective_at, created_at, applied_at,
           replaced_at, canceled_at, cancellation_reason
    FROM treasuries
    WHERE account_id = $1 AND livemode = $2
"#;

/// Loads the scope's treasury `id`.
pub async fn get(pool: &PgPool, scope: Scope, id: Uuid) -> Result<Option<Treasury>, TreasuryError> {
    let mut connection = pool.acquire().await?;
    get_in(&mut connection, scope, id).await
}

/// The scope's treasury `id`, read on `connection`.
pub(crate) async fn get_in(
    connection: &mut PgConnection,
    scope: Scope,
    id: Uuid,
) -> Result<Option<Treasury>, TreasuryError> {
    sqlx::query_as::<_, TreasuryRow>(sqlx::AssertSqlSafe(format!("{SELECT} AND id = $3")))
        .bind(scope.account_id())
        .bind(scope.livemode())
        .bind(id)
        .fetch_optional(connection)
        .await?
        .map(TreasuryRow::into_treasury)
        .transpose()
}

/// Filters of [`list`].
#[derive(Clone, Copy, Debug, Default)]
pub struct ListFilter {
    /// Only this chain's treasuries.
    pub chain_id: Option<u64>,
    /// Only treasuries in this status.
    pub status: Option<Status>,
}

/// A page of the scope's treasuries, newest first, at most `limit`, and whether more follow in its
/// direction (Stripe's cursor pagination): `cursor` is the treasury the page starts after (or, with
/// `before`, ends before); `NotFound` for a cursor outside the scope.
pub async fn list(
    pool: &PgPool,
    scope: Scope,
    filter: ListFilter,
    limit: i64,
    cursor: Option<(Uuid, bool)>,
) -> Result<(Vec<Treasury>, bool), TreasuryError> {
    let before = cursor.is_some_and(|(_, before)| before);
    let cursor = match cursor {
        Some((id, _)) => Some(
            sqlx::query_as::<_, (DateTime<Utc>, Uuid)>(
                "SELECT created_at, id FROM treasuries \
                 WHERE id = $1 AND account_id = $2 AND livemode = $3",
            )
            .bind(id)
            .bind(scope.account_id())
            .bind(scope.livemode())
            .fetch_optional(pool)
            .await?
            .ok_or(TreasuryError::NotFound)?,
        ),
        None => None,
    };
    let mut query = SELECT.to_owned();
    if let Some(status) = filter.status {
        query.push_str(" AND ");
        query.push_str(status.condition());
    }
    query.push_str(" AND ($3::bigint IS NULL OR chain_id = $3)");
    query.push_str(if before {
        " AND ($5::timestamptz IS NULL OR (created_at, id) > ($5, $6)) \
         ORDER BY created_at ASC, id ASC LIMIT $4"
    } else {
        " AND ($5::timestamptz IS NULL OR (created_at, id) < ($5, $6)) \
         ORDER BY created_at DESC, id DESC LIMIT $4"
    });
    let chain_id = filter
        .chain_id
        .map(i64::try_from)
        .transpose()
        .map_err(|_| TreasuryError::DatabaseInvariant)?;
    let mut rows = sqlx::query_as::<_, TreasuryRow>(sqlx::AssertSqlSafe(query))
        .bind(scope.account_id())
        .bind(scope.livemode())
        .bind(chain_id)
        .bind(limit.saturating_add(1))
        .bind(cursor.map(|(created_at, _)| created_at))
        .bind(cursor.map(|(_, id)| id))
        .fetch_all(pool)
        .await?;
    let limit = usize::try_from(limit).map_err(|_| TreasuryError::DatabaseInvariant)?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    if before {
        rows.reverse();
    }
    let treasuries = rows
        .into_iter()
        .map(TreasuryRow::into_treasury)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((treasuries, has_more))
}

#[derive(FromRow)]
struct TreasuryRow {
    id: Uuid,
    livemode: bool,
    chain_id: i64,
    address: String,
    kind: String,
    effective_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    applied_at: Option<DateTime<Utc>>,
    replaced_at: Option<DateTime<Utc>>,
    canceled_at: Option<DateTime<Utc>>,
    cancellation_reason: Option<String>,
}

impl TreasuryRow {
    fn into_treasury(self) -> Result<Treasury, TreasuryError> {
        let status = match (self.canceled_at, self.replaced_at, self.applied_at) {
            (Some(_), _, _) => Status::Canceled,
            (None, Some(_), _) => Status::Replaced,
            (None, None, Some(_)) => Status::Active,
            (None, None, None) => Status::Pending,
        };
        Ok(Treasury {
            id: self.id,
            livemode: self.livemode,
            chain_id: u64::try_from(self.chain_id).map_err(|_| TreasuryError::DatabaseInvariant)?,
            address: Address::from_str(&self.address)
                .map_err(|_| TreasuryError::DatabaseInvariant)?,
            kind: Kind::parse(&self.kind)?,
            status,
            effective_at: self.effective_at,
            created_at: self.created_at,
            replaced_at: self.replaced_at,
            canceled_at: self.canceled_at,
            cancellation_reason: self
                .cancellation_reason
                .as_deref()
                .map(CancellationReason::parse)
                .transpose()?,
        })
    }
}

/// Applies treasury changes whose time-lock ended, screens current treasuries again daily, and
/// prunes old challenges.
pub struct TreasuryWorker {
    pool: PgPool,
    routes: Arc<RouteSet>,
    screening: Arc<dyn DestinationScreener>,
    interval: StdDuration,
}

impl TreasuryWorker {
    /// Checks every `interval`, screening with `screening`.
    #[must_use]
    pub fn new(
        pool: PgPool,
        routes: Arc<RouteSet>,
        screening: Arc<dyn DestinationScreener>,
        interval: StdDuration,
    ) -> Self {
        Self {
            pool,
            routes,
            screening,
            interval,
        }
    }

    /// Runs until cancelled.
    pub async fn run(&self, cancellation: CancellationToken) {
        let mut ticker = interval(self.interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return,
                _ = ticker.tick() => {
                    let now = Utc::now();
                    let screening = &*self.screening;
                    if let Err(error) = apply_due(&self.pool, &self.routes, screening, now).await {
                        tracing::error!(%error, "applying due treasury changes failed");
                    }
                    if let Err(error) = rescreen_due(&self.pool, &self.routes, screening, now).await {
                        tracing::error!(%error, "re-screening treasuries failed");
                    }
                    if let Err(error) = prune_challenges(&self.pool, now).await {
                        tracing::warn!(%error, "pruning treasury challenges failed");
                    }
                }
            }
        }
    }
}

async fn prune_challenges(pool: &PgPool, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    let before = now.checked_sub_signed(CHALLENGE_RETENTION).unwrap_or(now);
    sqlx::query("DELETE FROM treasury_challenges WHERE expires_at < $1")
        .bind(before)
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    fn origin() -> MessageOrigin {
        MessageOrigin::new(
            &crate::api::PublicOrigin::parse("https://api.example:8443").expect("origin"),
        )
    }

    #[test]
    fn the_challenge_is_an_eip4361_message_naming_the_account_mode_and_chain() {
        let issued = DateTime::parse_from_rfc3339("2026-09-28T12:00:00Z")
            .expect("time")
            .with_timezone(&Utc);
        let message = render_message(
            &origin(),
            statement("acct_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10", true),
            1,
            address!("0x6Da01670d8fc844e736095918bbE11fE8D564163"),
            "0123456789abcdef0123456789abcdef",
            issued,
            issued + CHALLENGE_TTL,
        )
        .expect("message");
        assert_eq!(
            message,
            "api.example:8443 wants you to sign in with your Ethereum account:\n\
             0x6Da01670d8fc844e736095918bbE11fE8D564163\n\
             \n\
             Set this address as the live mode treasury of acct_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10 \
             on Phala Pay.\n\
             \n\
             URI: https://api.example:8443\n\
             Version: 1\n\
             Chain ID: 1\n\
             Nonce: 0123456789abcdef0123456789abcdef\n\
             Issued At: 2026-09-28T12:00:00.000Z\n\
             Expiration Time: 2026-09-28T12:10:00.000Z"
        );
        let parsed = siwe::Message::from_str(&message).expect("parses");
        assert_eq!(parsed.to_string(), message);
        // The personal_sign hash Alloy computes over the message is the one EIP-4361 defines.
        assert_eq!(
            eip191_hash_message(message.as_bytes()).0,
            parsed.eip191_hash().expect("hash")
        );
    }

    #[tokio::test]
    async fn an_erc6492_wrapped_signature_is_refused_before_any_chain_read() {
        let challenge = Challenge {
            nonce: "0123456789abcdef0123456789abcdef".to_owned(),
            chain_id: 1,
            address: Address::repeat_byte(0x11),
            message: "message".to_owned(),
            expires_at: Utc::now(),
        };
        let mut wrapped = vec![0xab; 96];
        wrapped.extend_from_slice(&ERC6492_MAGIC_SUFFIX);
        assert!(matches!(
            verify_signature(&UnavailableContractSignatures, &challenge, &wrapped).await,
            Err(TreasuryError::Erc6492)
        ));
    }
}

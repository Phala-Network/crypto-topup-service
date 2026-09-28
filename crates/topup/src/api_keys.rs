//! API keys (docs/design/multi-tenant.md D7): the key format, its hash, and the `api_keys`
//! repository.
//!
//! A key is `ppay_sk_{live|test}_` (secret) or `ppay_rk_{live|test}_` (restricted), 43 base62
//! characters drawn from the OS RNG (256 bits, as 32 random bytes), and a 6-character base62 CRC32
//! of everything before it, the shape of GitHub's token format. The checksum lets the service refuse a mistyped or made-up key without a
//! database read and lets secret scanners recognise a real one. Only the SHA-256 of the whole key
//! is stored: a 256-bit random key needs no slow hash, and the lookup is by that hash, so there is
//! no comparison to time.
//!
//! The operator issues an account's first secret key of each enabled mode when it creates the
//! account (design D8); the merchant then creates, rolls, and revokes its keys with a secret key
//! (`/v1/api_keys`). A secret key holds every permission; a restricted key (design PR 12, Stripe's
//! restricted keys) holds only the permissions it was created with, which never include managing
//! keys, treasuries, webhook endpoints, webhook keys, or account settings, so a server that runs
//! with a restricted key cannot redirect funds or silence notices if it leaks. A roll keeps the old
//! key working until an expiry of at most 7 days, Stripe's grace period. A revoke that would leave the account's mode without a non-expiring key is
//! refused, so a merchant cannot lock itself out; a merchant that loses every key asks the
//! operator, who may revoke the mode's keys and issues a recovery key. Every change is audited and
//! is an `api_key.*` event carrying its actor.

use chrono::{DateTime, Duration, Utc};
use rand::TryRng as _;
use rand::rngs::SysRng;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::audit::{self, Actor, ActorType};
use crate::db::Account;
use crate::ids;
use crate::tenancy::{self, Permission, Principal, Scope};

/// Base62 characters of the random part.
const RANDOM_CHARS: usize = 43;
/// Base62 characters of the CRC32 checksum; `62^6 > 2^32`.
const CHECKSUM_CHARS: usize = 6;
const ALPHABET: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const CRC32: crc::Crc<u32> = crc::Crc::<u32>::new(&crc::CRC_32_ISO_HDLC);

/// The longest a rolled key keeps working, Stripe's grace period.
pub const MAX_ROLL_EXPIRY: Duration = Duration::days(7);

/// The kind of an API key and the permissions principal it holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyKind {
    /// Holds every API permission.
    Secret,
    /// Holds the permissions it was granted (design PR 12).
    Restricted,
}

impl KeyKind {
    const fn code(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::Restricted => "restricted",
        }
    }

    const fn tag(self) -> &'static str {
        match self {
            Self::Secret => "sk",
            Self::Restricted => "rk",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        match code {
            "secret" => Some(Self::Secret),
            "restricted" => Some(Self::Restricted),
            _ => None,
        }
    }
}

/// The prefix of a key of `kind` in the given mode, `ppay_sk_test_` and so on.
#[must_use]
pub fn prefix(kind: KeyKind, livemode: bool) -> String {
    let mode = if livemode { "live" } else { "test" };
    format!("ppay_{}_{mode}_", kind.tag())
}

/// What a key's text says about it before any database read: its kind and mode. `None` for
/// anything that is not a well-formed key with a valid checksum.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyFormat {
    /// The key's kind.
    pub kind: KeyKind,
    /// The key's mode.
    pub livemode: bool,
}

/// Checks a presented key's form and checksum.
#[must_use]
pub fn check_format(key: &str) -> Option<KeyFormat> {
    let (kind, livemode, rest) = [
        (KeyKind::Secret, false),
        (KeyKind::Secret, true),
        (KeyKind::Restricted, false),
        (KeyKind::Restricted, true),
    ]
    .into_iter()
    .find_map(|(kind, livemode)| {
        key.strip_prefix(prefix(kind, livemode).as_str())
            .map(|rest| (kind, livemode, rest))
    })?;
    if rest.len() != RANDOM_CHARS.checked_add(CHECKSUM_CHARS)?
        || !rest.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return None;
    }
    let (body, checksum) = key.split_at(key.len().checked_sub(CHECKSUM_CHARS)?);
    (checksum == encode_checksum(body)).then_some(KeyFormat { kind, livemode })
}

/// The stored hash of a key.
#[must_use]
pub fn hash(key: &str) -> [u8; 32] {
    Sha256::digest(key.as_bytes()).into()
}

fn encode_checksum(body: &str) -> String {
    let mut value = CRC32.checksum(body.as_bytes());
    let mut digits = [b'0'; CHECKSUM_CHARS];
    for digit in digits.iter_mut().rev() {
        *digit = alphabet_char(value % 62);
        value /= 62;
    }
    digits.iter().copied().map(char::from).collect()
}

fn alphabet_char(index: u32) -> u8 {
    usize::try_from(index)
        .ok()
        .and_then(|index| ALPHABET.get(index))
        .copied()
        .unwrap_or(b'0')
}

/// Generates a new key of `kind` in the given mode.
pub fn generate(kind: KeyKind, livemode: bool) -> Result<Zeroizing<String>, ApiKeyError> {
    let mut key = Zeroizing::new(prefix(kind, livemode));
    let mut random = Zeroizing::new([0_u8; 64]);
    let mut drawn = 0;
    while drawn < RANDOM_CHARS {
        SysRng
            .try_fill_bytes(random.as_mut_slice())
            .map_err(|error| {
                tracing::error!(%error, "OS RNG failed; no API key issued");
                ApiKeyError::EntropyUnavailable
            })?;
        // Rejection sampling keeps every character uniform: 248 = 4 × 62.
        for byte in random.iter().copied().filter(|byte| *byte < 248) {
            if drawn == RANDOM_CHARS {
                break;
            }
            key.push(char::from(alphabet_char(u32::from(byte % 62))));
            drawn = drawn.saturating_add(1);
        }
    }
    let checksum = encode_checksum(&key);
    key.push_str(&checksum);
    Ok(key)
}

/// A stored API key, without its secret.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApiKey {
    /// Key id.
    pub id: Uuid,
    /// The account the key acts for.
    pub account_id: Uuid,
    /// The mode the key selects.
    pub livemode: bool,
    /// The key's kind.
    pub kind: KeyKind,
    /// The merchant's label.
    pub name: String,
    /// `ppay_sk_test_` and so on.
    pub prefix: String,
    /// The key's last four characters.
    pub last4: String,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// When a rolled key stops working.
    pub expires_at: Option<DateTime<Utc>>,
    /// Last authenticated use, to the minute.
    pub last_used_at: Option<DateTime<Utc>>,
    /// When the key was revoked.
    pub revoked_at: Option<DateTime<Utc>>,
    /// A restricted key's granted permissions; `None` for a secret key.
    pub permissions: Option<Vec<Permission>>,
}

impl ApiKey {
    /// The key's API id, `key_…`.
    #[must_use]
    pub fn public_id(&self) -> String {
        ids::format(ids::API_KEY, self.id)
    }

    /// The audit actor of requests made with the key.
    #[must_use]
    pub fn actor(&self) -> Actor {
        Actor::api_key(self.public_id())
    }

    /// The key's scope.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        Scope::new(self.account_id, self.livemode)
    }

    /// Whether the key's own grants include `permission`: always for a secret key. The
    /// authorization table must grant it to the key's kind as well.
    #[must_use]
    pub fn granted(&self, permission: Permission) -> bool {
        self.permissions
            .as_ref()
            .is_none_or(|granted| granted.contains(&permission))
    }
}

/// A newly issued key with its secret, which is shown once and never stored.
pub struct IssuedKey {
    /// The stored key.
    pub key: ApiKey,
    /// The whole key.
    pub secret: Zeroizing<String>,
}

/// Why a presented key did not authenticate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Rejection {
    /// Not a well-formed key, a checksum mismatch, or no such key.
    Invalid,
    /// A revoked key.
    Revoked,
    /// A rolled key past its expiry.
    Expired,
}

/// Failures of the key repository.
#[derive(Debug, thiserror::Error)]
pub enum ApiKeyError {
    /// No such key in the scope, or no such account.
    #[error("API key not found")]
    NotFound,
    /// The key is revoked or already rolled.
    #[error("the API key is revoked or already rolled")]
    Inactive,
    /// Revoking the key would leave the account's mode without a non-expiring key.
    #[error("the account's last active API key cannot be revoked")]
    LastActiveKey,
    /// A live key for an account the operator has not enabled for live mode.
    #[error("the account is not enabled for live mode")]
    ChargesNotEnabled,
    /// A roll expiry outside `0..=7 days`.
    #[error("expiry must be between now and 7 days")]
    InvalidExpiry,
    /// A restricted key's grant is empty, unknown, or not grantable to a restricted key.
    #[error("permission {0} cannot be granted to a restricted key")]
    PermissionNotGrantable(String),
    /// The OS RNG failed.
    #[error("entropy unavailable")]
    EntropyUnavailable,
    /// PostgreSQL failed.
    #[error("{0}")]
    Database(#[from] sqlx::Error),
}

/// The columns of [`KeyRow`].
macro_rules! key_columns {
    () => {
        "id, account_id, livemode, kind, name, prefix, last4, created_at, expires_at, \
         last_used_at, revoked_at, permissions"
    };
}

#[derive(sqlx::FromRow)]
struct KeyRow {
    id: Uuid,
    account_id: Uuid,
    livemode: bool,
    kind: String,
    name: String,
    prefix: String,
    last4: String,
    created_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    last_used_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    permissions: Option<sqlx::types::Json<Vec<String>>>,
}

impl TryFrom<KeyRow> for ApiKey {
    type Error = sqlx::Error;

    fn try_from(row: KeyRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            account_id: row.account_id,
            livemode: row.livemode,
            kind: KeyKind::from_code(&row.kind).ok_or_else(|| {
                sqlx::Error::Decode(format!("unknown API key kind {}", row.kind).into())
            })?,
            name: row.name,
            prefix: row.prefix,
            last4: row.last4,
            created_at: row.created_at,
            expires_at: row.expires_at,
            last_used_at: row.last_used_at,
            revoked_at: row.revoked_at,
            permissions: row
                .permissions
                .map(|codes| {
                    codes
                        .iter()
                        .map(|code| {
                            Permission::parse(code).ok_or_else(|| {
                                sqlx::Error::Decode(format!("unknown permission {code}").into())
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()
                })
                .transpose()?,
        })
    }
}

/// An authenticated key with its account.
pub struct Authenticated {
    /// The key's account.
    pub account: Account,
    /// Whether the operator enabled the account for live mode.
    pub charges_enabled: bool,
    /// The key.
    pub key: ApiKey,
}

/// Authenticates a presented key: its account and the key, or why it is refused. A usable key's
/// `last_used_at` is refreshed at most once a minute.
pub async fn authenticate(
    pool: &PgPool,
    presented: &str,
) -> Result<Result<Authenticated, Rejection>, sqlx::Error> {
    if check_format(presented).is_none() {
        return Ok(Err(Rejection::Invalid));
    }
    let row = sqlx::query_as::<_, KeyRow>(concat!(
        "SELECT ",
        key_columns!(),
        " FROM api_keys WHERE key_hash = $1"
    ))
    .bind(hash(presented).as_slice())
    .fetch_optional(pool)
    .await?;
    let Some(key) = row.map(ApiKey::try_from).transpose()? else {
        return Ok(Err(Rejection::Invalid));
    };
    if key.revoked_at.is_some() {
        return Ok(Err(Rejection::Revoked));
    }
    if key
        .expires_at
        .is_some_and(|expires_at| expires_at <= Utc::now())
    {
        return Ok(Err(Rejection::Expired));
    }
    let account = crate::db::get_account(pool, key.account_id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)?;
    let charges_enabled: bool =
        sqlx::query_scalar("SELECT charges_enabled FROM accounts WHERE id = $1")
            .bind(key.account_id)
            .fetch_one(pool)
            .await?;
    sqlx::query(
        "UPDATE api_keys SET last_used_at = date_trunc('minute', now()) \
         WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < date_trunc('minute', now()))",
    )
    .bind(key.id)
    .execute(pool)
    .await?;
    Ok(Ok(Authenticated {
        account,
        charges_enabled,
        key,
    }))
}

/// A page of the scope's keys, newest first, and whether more follow in its direction (Stripe's
/// cursor pagination): `cursor` is the key the page starts after (or, with `before`, ends before);
/// `None` for a cursor outside the scope.
pub async fn list(
    pool: &PgPool,
    scope: Scope,
    limit: i64,
    cursor: Option<(Uuid, bool)>,
) -> Result<Option<(Vec<ApiKey>, bool)>, ApiKeyError> {
    let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new(concat!(
        "SELECT ",
        key_columns!(),
        " FROM api_keys WHERE account_id = "
    ));
    builder
        .push_bind(scope.account_id())
        .push(" AND livemode = ")
        .push_bind(scope.livemode());
    let before = cursor.is_some_and(|(_, before)| before);
    if let Some((id, _)) = cursor {
        let found: Option<(DateTime<Utc>, Uuid)> = sqlx::query_as(
            "SELECT created_at, id FROM api_keys WHERE id = $1 AND account_id = $2 AND livemode = $3",
        )
        .bind(id)
        .bind(scope.account_id())
        .bind(scope.livemode())
        .fetch_optional(pool)
        .await?;
        let Some((created_at, id)) = found else {
            return Ok(None);
        };
        builder
            .push(if before {
                " AND (created_at, id) > ("
            } else {
                " AND (created_at, id) < ("
            })
            .push_bind(created_at)
            .push(", ")
            .push_bind(id)
            .push(")");
    }
    builder
        .push(if before {
            " ORDER BY created_at ASC, id ASC LIMIT "
        } else {
            " ORDER BY created_at DESC, id DESC LIMIT "
        })
        .push_bind(limit.saturating_add(1));
    let mut keys = builder
        .build_query_as::<KeyRow>()
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(ApiKey::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let has_more = keys.len() > limit;
    keys.truncate(limit);
    if before {
        keys.reverse();
    }
    Ok(Some((keys, has_more)))
}

/// One key of the scope.
pub async fn get<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    scope: Scope,
    id: Uuid,
) -> Result<Option<ApiKey>, sqlx::Error> {
    sqlx::query_as::<_, KeyRow>(concat!(
        "SELECT ",
        key_columns!(),
        " FROM api_keys WHERE id = $1 AND account_id = $2 AND livemode = $3"
    ))
    .bind(id)
    .bind(scope.account_id())
    .bind(scope.livemode())
    .fetch_optional(executor)
    .await?
    .map(ApiKey::try_from)
    .transpose()
}

/// Issues a secret key in `scope` named `name`, with its audit row and `api_key.created` event,
/// in one transaction. A live key needs an account enabled for live mode.
pub async fn create(
    pool: &PgPool,
    scope: Scope,
    name: &str,
    actor: &Actor,
    reason: &str,
) -> Result<IssuedKey, ApiKeyError> {
    let mut transaction = pool.begin().await?;
    let issued = create_in(&mut transaction, scope, name, actor, reason).await?;
    transaction.commit().await?;
    Ok(issued)
}

/// The operator's recovery key (design D7): optionally revokes every key of `scope`, then issues
/// a new secret key, audited with `reason`, in one transaction.
pub async fn recover(
    pool: &PgPool,
    scope: Scope,
    name: &str,
    revoke_existing: bool,
    actor: &Actor,
    reason: &str,
) -> Result<IssuedKey, ApiKeyError> {
    let mut transaction = pool.begin().await?;
    if revoke_existing {
        let revoked = sqlx::query_as::<_, KeyRow>(concat!(
            "UPDATE api_keys SET revoked_at = now() \
             WHERE account_id = $1 AND livemode = $2 AND revoked_at IS NULL \
             RETURNING ",
            key_columns!()
        ))
        .bind(scope.account_id())
        .bind(scope.livemode())
        .fetch_all(&mut *transaction)
        .await?;
        for row in revoked {
            let key = ApiKey::try_from(row)?;
            record(
                &mut transaction,
                &key,
                None,
                actor,
                "api_key.revoked",
                reason,
            )
            .await?;
        }
    }
    let issued = create_in(&mut transaction, scope, name, actor, reason).await?;
    transaction.commit().await?;
    Ok(issued)
}

/// Issues a secret key inside the caller's transaction; see [`create`].
pub(crate) async fn create_in(
    transaction: &mut Transaction<'_, Postgres>,
    scope: Scope,
    name: &str,
    actor: &Actor,
    reason: &str,
) -> Result<IssuedKey, ApiKeyError> {
    let charges_enabled: bool =
        sqlx::query_scalar("SELECT charges_enabled FROM accounts WHERE id = $1 FOR SHARE")
            .bind(scope.account_id())
            .fetch_optional(&mut **transaction)
            .await?
            .ok_or(ApiKeyError::NotFound)?;
    if scope.livemode() && !charges_enabled {
        return Err(ApiKeyError::ChargesNotEnabled);
    }
    let issued = insert(transaction, scope, KeyKind::Secret, name, None, actor).await?;
    record(
        transaction,
        &issued.key,
        None,
        actor,
        "api_key.created",
        reason,
    )
    .await?;
    Ok(issued)
}

/// Issues a restricted key in `scope` named `name` holding `permissions`, each a permission the
/// authorization table grants to restricted keys; a `write` grant includes its resource's `read`.
/// Audited and announced as `api_key.created`.
pub async fn create_restricted(
    pool: &PgPool,
    scope: Scope,
    name: &str,
    permissions: &[Permission],
    actor: &Actor,
) -> Result<IssuedKey, ApiKeyError> {
    let grantable = tenancy::grants(pool, Principal::RestrictedKey).await?;
    let mut granted = Vec::with_capacity(permissions.len().saturating_mul(2));
    for permission in permissions {
        if !grantable.contains(permission) {
            return Err(ApiKeyError::PermissionNotGrantable(
                permission.code().to_owned(),
            ));
        }
        granted.push(*permission);
        granted.extend(permission.read_of_write());
    }
    if granted.is_empty() {
        return Err(ApiKeyError::PermissionNotGrantable(String::new()));
    }
    granted.sort_by_key(|permission| permission.code());
    granted.dedup();
    let mut transaction = pool.begin().await?;
    let charges_enabled: bool =
        sqlx::query_scalar("SELECT charges_enabled FROM accounts WHERE id = $1 FOR SHARE")
            .bind(scope.account_id())
            .fetch_optional(&mut *transaction)
            .await?
            .ok_or(ApiKeyError::NotFound)?;
    if scope.livemode() && !charges_enabled {
        return Err(ApiKeyError::ChargesNotEnabled);
    }
    let issued = insert(
        &mut transaction,
        scope,
        KeyKind::Restricted,
        name,
        Some(&granted),
        actor,
    )
    .await?;
    record(
        &mut transaction,
        &issued.key,
        None,
        actor,
        "api_key.created",
        "",
    )
    .await?;
    transaction.commit().await?;
    Ok(issued)
}

/// Rolls the scope's key `id`: a new key of the same kind, name, and permissions, and the old key
/// expiring after `expires_in` (at most 7 days), or revoked at once for zero.
pub async fn roll(
    pool: &PgPool,
    scope: Scope,
    id: Uuid,
    expires_in: Duration,
    actor: &Actor,
) -> Result<IssuedKey, ApiKeyError> {
    if expires_in < Duration::zero() || expires_in > MAX_ROLL_EXPIRY {
        return Err(ApiKeyError::InvalidExpiry);
    }
    let mut transaction = pool.begin().await?;
    let old = locked_key(&mut transaction, scope, id).await?;
    if old.revoked_at.is_some() || old.expires_at.is_some() {
        return Err(ApiKeyError::Inactive);
    }
    let issued = insert(
        &mut transaction,
        scope,
        old.kind,
        &old.name,
        old.permissions.as_deref(),
        actor,
    )
    .await?;
    let rolled = format!("rolled to {}", issued.key.public_id());
    record(
        &mut transaction,
        &issued.key,
        None,
        actor,
        "api_key.created",
        &rolled,
    )
    .await?;
    if expires_in.is_zero() {
        let revoked = mark_revoked(&mut transaction, id).await?;
        record(
            &mut transaction,
            &revoked,
            None,
            actor,
            "api_key.revoked",
            &rolled,
        )
        .await?;
    } else {
        let expiring = sqlx::query_as::<_, KeyRow>(concat!(
            "UPDATE api_keys SET expires_at = now() + $2 WHERE id = $1 RETURNING ",
            key_columns!()
        ))
        .bind(id)
        .bind(expires_in)
        .fetch_one(&mut *transaction)
        .await?
        .try_into()?;
        record(
            &mut transaction,
            &expiring,
            Some(&old),
            actor,
            "api_key.updated",
            &rolled,
        )
        .await?;
    }
    transaction.commit().await?;
    Ok(issued)
}

/// Revokes the scope's key `id`; revoking a revoked key returns it unchanged. Refused when it
/// would leave the scope without a key that is neither revoked nor expiring, so the account
/// always keeps a working key.
pub async fn revoke(
    pool: &PgPool,
    scope: Scope,
    id: Uuid,
    actor: &Actor,
) -> Result<ApiKey, ApiKeyError> {
    let mut transaction = pool.begin().await?;
    // Every key of the scope is locked, so two revokes cannot each leave the other key.
    let keys = sqlx::query_as::<_, KeyRow>(concat!(
        "SELECT ",
        key_columns!(),
        " FROM api_keys WHERE account_id = $1 AND livemode = $2 ORDER BY id FOR UPDATE"
    ))
    .bind(scope.account_id())
    .bind(scope.livemode())
    .fetch_all(&mut *transaction)
    .await?
    .into_iter()
    .map(ApiKey::try_from)
    .collect::<Result<Vec<_>, _>>()?;
    let key = keys
        .iter()
        .find(|key| key.id == id)
        .cloned()
        .ok_or(ApiKeyError::NotFound)?;
    if key.revoked_at.is_some() {
        transaction.commit().await?;
        return Ok(key);
    }
    let lasting = |candidate: &ApiKey| {
        candidate.kind == KeyKind::Secret
            && candidate.revoked_at.is_none()
            && candidate.expires_at.is_none()
    };
    if lasting(&key) && !keys.iter().any(|other| other.id != id && lasting(other)) {
        return Err(ApiKeyError::LastActiveKey);
    }
    let revoked = mark_revoked(&mut transaction, id).await?;
    record(
        &mut transaction,
        &revoked,
        None,
        actor,
        "api_key.revoked",
        "",
    )
    .await?;
    transaction.commit().await?;
    Ok(revoked)
}

/// Inserts a new key of `kind` in `scope`, inside the caller's transaction; `permissions` are a
/// restricted key's grants and `None` for a secret key.
async fn insert(
    transaction: &mut Transaction<'_, Postgres>,
    scope: Scope,
    kind: KeyKind,
    name: &str,
    permissions: Option<&[Permission]>,
    actor: &Actor,
) -> Result<IssuedKey, ApiKeyError> {
    let secret = generate(kind, scope.livemode())?;
    let last4 = secret
        .get(secret.len().saturating_sub(4)..)
        .ok_or(ApiKeyError::EntropyUnavailable)?;
    let row = sqlx::query_as::<_, KeyRow>(concat!(
        "INSERT INTO api_keys \
         (id, account_id, livemode, kind, name, prefix, last4, key_hash, created_by, permissions) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING ",
        key_columns!()
    ))
    .bind(Uuid::new_v4())
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(kind.code())
    .bind(name)
    .bind(prefix(kind, scope.livemode()))
    .bind(last4)
    .bind(hash(&secret).as_slice())
    .bind(event_actor(actor))
    .bind(permissions.map(|permissions| {
        sqlx::types::Json(
            permissions
                .iter()
                .map(|permission| permission.code())
                .collect::<Vec<_>>(),
        )
    }))
    .fetch_one(&mut **transaction)
    .await?;
    Ok(IssuedKey {
        key: row.try_into()?,
        secret,
    })
}

/// The `actor` an event and `api_keys.created_by` record: the key id, `admin`, or `system`.
#[must_use]
pub fn event_actor(actor: &Actor) -> String {
    match actor.actor_type {
        ActorType::ApiKey => actor.id.clone(),
        ActorType::Admin => "admin".to_owned(),
        ActorType::System => crate::db::SYSTEM_ACTOR.to_owned(),
    }
}

/// Appends the audit row of a change to `key` and its `event_type` event, delivered to the
/// key's account and mode; `before` is the key before an update, for `previous_attributes`.
async fn record(
    transaction: &mut Transaction<'_, Postgres>,
    key: &ApiKey,
    before: Option<&ApiKey>,
    actor: &Actor,
    event_type: &str,
    reason: &str,
) -> Result<(), sqlx::Error> {
    audit::insert(
        &mut **transaction,
        &audit::Entry {
            account_id: Some(key.account_id),
            actor,
            action: event_type,
            subject: &format!("api_key:{}", key.public_id()),
            reason,
        },
    )
    .await?;
    let render = |key: &ApiKey| crate::db::to_object(&crate::api::api_key_object(key, None));
    let before = before.map(render).transpose()?;
    let event = crate::db::NewOutboxEvent::new(
        event_type,
        key.scope(),
        crate::db::EventObject::ApiKey(key.id),
        actor,
    );
    crate::db::enqueue_rendered_in(
        transaction,
        &event,
        &crate::db::event_data(render(key)?, before.as_ref()),
        None,
        true,
    )
    .await
}

async fn locked_key(
    transaction: &mut Transaction<'_, Postgres>,
    scope: Scope,
    id: Uuid,
) -> Result<ApiKey, ApiKeyError> {
    sqlx::query_as::<_, KeyRow>(concat!(
        "SELECT ",
        key_columns!(),
        " FROM api_keys WHERE id = $1 AND account_id = $2 AND livemode = $3 FOR UPDATE"
    ))
    .bind(id)
    .bind(scope.account_id())
    .bind(scope.livemode())
    .fetch_optional(&mut **transaction)
    .await?
    .map(ApiKey::try_from)
    .transpose()?
    .ok_or(ApiKeyError::NotFound)
}

async fn mark_revoked(
    transaction: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<ApiKey, ApiKeyError> {
    Ok(sqlx::query_as::<_, KeyRow>(concat!(
        "UPDATE api_keys SET revoked_at = now() WHERE id = $1 RETURNING ",
        key_columns!()
    ))
    .bind(id)
    .fetch_one(&mut **transaction)
    .await?
    .try_into()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_have_the_documented_form_and_a_valid_checksum() {
        for (kind, livemode, expected) in [
            (KeyKind::Secret, false, "ppay_sk_test_"),
            (KeyKind::Secret, true, "ppay_sk_live_"),
            (KeyKind::Restricted, true, "ppay_rk_live_"),
        ] {
            let key = generate(kind, livemode).unwrap();
            assert!(key.starts_with(expected), "{}", key.as_str());
            assert_eq!(key.len(), expected.len() + RANDOM_CHARS + CHECKSUM_CHARS);
            assert_eq!(check_format(&key), Some(KeyFormat { kind, livemode }));
        }
        assert_ne!(
            generate(KeyKind::Secret, false).unwrap().as_str(),
            generate(KeyKind::Secret, false).unwrap().as_str()
        );
    }

    #[test]
    fn malformed_keys_and_bad_checksums_are_refused_without_a_lookup() {
        let key = generate(KeyKind::Secret, false).unwrap();
        let body = &key[..key.len() - CHECKSUM_CHARS];
        let checksum = &key[key.len() - CHECKSUM_CHARS..];
        let flipped = if checksum.starts_with('A') { "B" } else { "A" };
        let bad_checksum = format!("{body}{flipped}{}", &checksum[1..]);
        let other_mode = key.replacen("_test_", "_live_", 1);
        for candidate in [
            "",
            "ppay_sk_test_",
            "sk_test_abc",
            bad_checksum.as_str(),
            other_mode.as_str(),
            &key[..key.len() - 1],
            &format!("{}!", &key[..key.len() - 1]),
            &format!("{}x", key.as_str()),
            &format!("Bearer {}", key.as_str()),
        ] {
            assert_eq!(check_format(candidate), None, "{candidate}");
        }
    }

    #[test]
    fn the_checksum_is_the_base62_crc32_of_the_body() {
        // CRC-32/ISO-HDLC("123456789") = 0xCBF43926, "3jZRME" in base62.
        assert_eq!(encode_checksum("123456789"), "3jZRME");
        assert_eq!(encode_checksum(""), "000000");
    }
}

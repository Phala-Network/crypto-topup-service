//! Operator onboarding, API keys, idempotent POSTs, and rate limits (design D7, D8, D12, §12,
//! §13, §16 PR 5), in process against PostgreSQL.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{Context, Result, ensure};
use axum::body::to_bytes;
use axum::http::{HeaderMap, Method, StatusCode};
use chrono::Utc;
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use sqlx::PgPool;
use topup::api::{AppState, PublicOrigin, RateLimits, VerificationKey};
use topup::api_keys::{self, KeyKind};
use topup_adapters::attestation::DstackAttestor;
use topup_core::route::RouteFile;
use tower::ServiceExt;
use uuid::Uuid;

use support::seed::{self, NewAccount};
use support::{TEST_ORIGIN, TestDatabase, merchant_request, public_key_base64, signed_request};

const ADMIN_KID: &str = "admin/v1";

struct Harness {
    app: axum::Router,
    admin_key: SigningKey,
    /// Admin signatures are single-use, so each admin request is signed at a distinct second.
    created: AtomicI64,
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Harness {
    fn new(pool: &PgPool, limits: RateLimits) -> Result<Self> {
        let admin_key = SigningKey::from_bytes(&[77; 32]);
        let route: RouteFile =
            serde_saphyr::from_str(include_str!("fixtures/phala-cloud-pha.yaml"))?;
        let state = AppState {
            pool: pool.clone(),
            routes: Arc::new(
                topup::routes::RouteSet::new(vec![route]).map_err(anyhow::Error::msg)?,
            ),
            admin_key: VerificationKey::from_base64(
                ADMIN_KID.to_owned(),
                &public_key_base64(&admin_key),
            )
            .map_err(anyhow::Error::msg)?,
            public_origin: PublicOrigin::parse(TEST_ORIGIN)?,
            attestor: Arc::new(DstackAttestor::new()),
            rate_lock_quotes: Arc::new(topup::locks::UnavailableQuoteProvider),
            client_reads: Arc::default(),
            rate_limits: Arc::new(topup::api::ApiRateLimiter::new(limits)),
        };
        Ok(Self {
            app: topup::api::router(state).0,
            admin_key,
            created: AtomicI64::new(Utc::now().timestamp()),
        })
    }

    async fn admin(&self, method: Method, path: &str, body: &Value) -> Result<Answer> {
        let created = self.created.fetch_sub(1, Ordering::Relaxed);
        let request = signed_request(
            method,
            path,
            serde_json::to_vec(body)?,
            ADMIN_KID,
            &self.admin_key,
            created,
        );
        answer(self.app.clone().oneshot(request).await?).await
    }

    async fn merchant(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        key: &str,
        idempotency_key: Option<&str>,
    ) -> Result<Answer> {
        let body = body
            .map(serde_json::to_vec)
            .transpose()?
            .unwrap_or_default();
        let mut request = merchant_request(method, path, body, key);
        if let Some(idempotency_key) = idempotency_key {
            request
                .headers_mut()
                .insert("idempotency-key", idempotency_key.parse()?);
        }
        answer(self.app.clone().oneshot(request).await?).await
    }

    async fn get(&self, path: &str, key: &str) -> Result<Answer> {
        self.merchant(Method::GET, path, None, key, None).await
    }

    /// A request with a raw `Authorization` header, or none.
    async fn with_authorization(&self, authorization: Option<&str>) -> Result<Answer> {
        let mut request = axum::http::Request::get(format!("{TEST_ORIGIN}/v1/account"));
        if let Some(authorization) = authorization {
            request = request.header("authorization", authorization);
        }
        answer(
            self.app
                .clone()
                .oneshot(request.body(axum::body::Body::empty())?)
                .await?,
        )
        .await
    }
}

async fn answer(response: axum::response::Response) -> Result<Answer> {
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1_048_576).await?;
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    Ok(Answer {
        status,
        headers,
        body,
    })
}

fn account_body(name: &str, charges_enabled: bool) -> Value {
    json!({
        "name": name,
        "contact": {"name": "Ada Ops", "email": "security@merchant.test"},
        "due_diligence": {
            "reference": "DD-2026-042",
            "reviewed_at": "2026-09-28",
            "reviewed_by": "operator@phala.network",
        },
        "charges_enabled": charges_enabled,
        "reason": "merchant agreement signed",
    })
}

fn secret(key: &Value) -> Result<String> {
    Ok(key["secret"].as_str().context("secret")?.to_owned())
}

/// A test account and its test key.
async fn seed_test_account(pool: &PgPool, name: &str) -> Result<(Uuid, String)> {
    let account = seed::create_account(
        pool,
        &NewAccount {
            livemode: false,
            ..NewAccount::named(name)
        },
    )
    .await?;
    Ok((
        account.id,
        seed::create_api_key(pool, account.id, false).await?,
    ))
}

async fn events(pool: &PgPool, account: Uuid) -> Result<Vec<(String, bool, String)>> {
    Ok(sqlx::query_as(
        "SELECT type, livemode, actor FROM events WHERE account_id = $1 ORDER BY created, type",
    )
    .bind(account)
    .fetch_all(pool)
    .await?)
}

async fn audit_actions(pool: &PgPool, account: Uuid) -> Result<Vec<(String, String)>> {
    Ok(sqlx::query_as(
        "SELECT action, actor_type FROM audit WHERE account_id = $1 ORDER BY created_at, action",
    )
    .bind(account)
    .fetch_all(pool)
    .await?)
}

/// The operator creates an account with its contact and due diligence and hands over the first
/// test key; live mode (and the first live key) comes only from the operator, and a recovery key
/// can revoke the mode's keys. Every step is audited and is an event naming its actor.
#[tokio::test]
async fn operator_onboards_accounts_enables_live_mode_and_recovers_keys() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let pool = &database.app_pool;
        let harness = Harness::new(pool, RateLimits::default())?;

        for (field, value) in [
            ("name", json!(" ")),
            ("contact", json!({"name": "Ada", "email": "not-an-email"})),
            ("contact", json!({"name": "", "email": "a@b.test"})),
            (
                "due_diligence",
                json!({"reference": "", "reviewed_at": "2026-09-28", "reviewed_by": "op"}),
            ),
            ("reason", json!("")),
            ("webhook_url", json!("ftp://merchant.test/hooks")),
            ("live_access", json!(true)),
        ] {
            let mut body = account_body("Merchant", false);
            body[field] = value;
            let refused = harness
                .admin(Method::POST, "/v1/admin/accounts", &body)
                .await?;
            ensure!(
                refused.status == StatusCode::BAD_REQUEST,
                "{field}: {}",
                refused.body
            );
        }

        let created = harness
            .admin(
                Method::POST,
                "/v1/admin/accounts",
                &account_body("Merchant", false),
            )
            .await?;
        ensure!(created.status == StatusCode::OK, "{}", created.body);
        let account = created.body;
        let account_id =
            topup::ids::parse(topup::ids::ACCOUNT, account["id"].as_str().context("id")?)
                .context("acct_ id")?;
        ensure!(account["object"] == "account" && account["charges_enabled"] == false);
        ensure!(account["contact"]["email"] == "security@merchant.test");
        ensure!(account["due_diligence"]["reference"] == "DD-2026-042");
        let keys = account["api_keys"].as_array().context("api_keys")?;
        ensure!(keys.len() == 1, "only a test key without live mode");
        let test_key = secret(&keys[0])?;
        ensure!(test_key.starts_with("ppay_sk_test_") && keys[0]["livemode"] == false);
        ensure!(api_keys::check_format(&test_key).is_some());

        // The first key works, and nothing stored reveals it again.
        let me = harness.get("/v1/account", &test_key).await?;
        ensure!(me.status == StatusCode::OK, "{}", me.body);
        ensure!(me.body["id"] == account["id"] && me.body["livemode"] == false);
        let listed = harness.get("/v1/api_keys", &test_key).await?;
        ensure!(listed.body["data"].as_array().map(Vec::len) == Some(1));
        ensure!(listed.body["data"][0].get("secret").is_none());
        ensure!(
            listed.body["data"][0]["redacted"]
                == format!("ppay_sk_test_…{}", &test_key[test_key.len() - 4..])
        );
        let created_by: String =
            sqlx::query_scalar("SELECT created_by FROM api_keys WHERE account_id = $1")
                .bind(account_id)
                .fetch_one(pool)
                .await?;
        ensure!(created_by == "admin");

        // A merchant key cannot reach the admin API.
        let merchant_admin = harness
            .merchant(
                Method::POST,
                "/v1/admin/accounts",
                Some(&account_body("Other", true)),
                &test_key,
                None,
            )
            .await?;
        ensure!(merchant_admin.status == StatusCode::UNAUTHORIZED);

        // No live key until the operator enables live mode.
        let early_live = harness
            .admin(
                Method::POST,
                &format!(
                    "/v1/admin/accounts/{}/api_keys",
                    account["id"].as_str().context("id")?
                ),
                &json!({"livemode": true, "reason": "asked by the contact"}),
            )
            .await?;
        ensure!(early_live.status == StatusCode::FORBIDDEN);
        ensure!(early_live.body["error"]["code"] == "testmode_charges_only");

        let account_path = format!(
            "/v1/admin/accounts/{}",
            account["id"].as_str().context("id")?
        );
        let enable = json!({"charges_enabled": true, "reason": "due diligence DD-2026-042 passed"});
        let enabled = harness.admin(Method::POST, &account_path, &enable).await?;
        ensure!(enabled.status == StatusCode::OK, "{}", enabled.body);
        ensure!(enabled.body["charges_enabled"] == true);
        let live_keys = enabled.body["api_keys"].as_array().context("api_keys")?;
        ensure!(live_keys.len() == 1 && live_keys[0]["livemode"] == true);
        let live_key = secret(&live_keys[0])?;
        ensure!(live_key.starts_with("ppay_sk_live_"));
        let live_me = harness.get("/v1/account", &live_key).await?;
        ensure!(live_me.status == StatusCode::OK && live_me.body["livemode"] == true);
        // A test key never reaches live keys.
        let test_view = harness.get("/v1/api_keys", &test_key).await?;
        ensure!(
            test_view.body["data"]
                .as_array()
                .is_some_and(|keys| keys.iter().all(|key| key["livemode"] == false))
        );
        let live_key_id = live_keys[0]["id"].as_str().context("key id")?;
        let hidden = harness
            .get(&format!("/v1/api_keys/{live_key_id}"), &test_key)
            .await?;
        ensure!(hidden.status == StatusCode::NOT_FOUND);

        // Repeating the update changes nothing and issues no second live key.
        let repeated = harness.admin(Method::POST, &account_path, &enable).await?;
        ensure!(repeated.status == StatusCode::OK);
        ensure!(repeated.body["api_keys"] == json!([]));

        // Turning live mode off stops live keys at once.
        let disabled = harness
            .admin(
                Method::POST,
                &account_path,
                &json!({"charges_enabled": false, "reason": "incident review"}),
            )
            .await?;
        ensure!(disabled.status == StatusCode::OK);
        let refused = harness.get("/v1/account", &live_key).await?;
        ensure!(refused.status == StatusCode::FORBIDDEN);
        ensure!(refused.body["error"]["code"] == "testmode_charges_only");

        // Recovery: the operator revokes the mode's keys and issues a new one.
        let recovered = harness
            .admin(
                Method::POST,
                &format!("{account_path}/api_keys"),
                &json!({
                    "livemode": false,
                    "revoke_existing": true,
                    "reason": "contact reported a leak; verified by phone",
                }),
            )
            .await?;
        ensure!(recovered.status == StatusCode::OK, "{}", recovered.body);
        let recovery_key = secret(&recovered.body)?;
        let old = harness.get("/v1/account", &test_key).await?;
        ensure!(old.status == StatusCode::UNAUTHORIZED);
        ensure!(old.body["error"]["code"] == "api_key_invalid");
        ensure!(harness.get("/v1/account", &recovery_key).await?.status == StatusCode::OK);

        let audit = audit_actions(pool, account_id).await?;
        for action in [
            "account.create",
            "account.update",
            "api_key.created",
            "api_key.revoked",
        ] {
            ensure!(
                audit
                    .iter()
                    .any(|(stored, actor)| stored == action && actor == "admin"),
                "{action}: {audit:?}"
            );
        }
        let updates = audit
            .iter()
            .filter(|(action, _)| action == "account.update")
            .count();
        ensure!(updates == 2, "the repeat is not audited: {audit:?}");

        let events = events(pool, account_id).await?;
        ensure!(
            events.iter().all(|(_, _, actor)| actor == "admin"),
            "{events:?}"
        );
        for expected in [
            ("api_key.created", false),
            ("api_key.created", true),
            ("account.updated", false),
            ("account.updated", true),
            ("api_key.revoked", false),
        ] {
            ensure!(
                events
                    .iter()
                    .any(|(kind, livemode, _)| (kind.as_str(), *livemode) == expected),
                "{expected:?}: {events:?}"
            );
        }

        // Creating with live mode hands over both keys at once.
        let both = harness
            .admin(
                Method::POST,
                "/v1/admin/accounts",
                &account_body("Phala Cloud", true),
            )
            .await?;
        ensure!(both.status == StatusCode::OK);
        let modes: Vec<Value> = both.body["api_keys"]
            .as_array()
            .context("api_keys")?
            .iter()
            .map(|key| key["livemode"].clone())
            .collect();
        ensure!(modes == [json!(false), json!(true)]);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

/// Bearer keys only: valid, missing, other schemes, malformed, bad checksum, unknown, revoked,
/// expired, and rolled keys within and after their overlap.
#[tokio::test]
async fn keys_authenticate_by_bearer_and_expire_or_revoke() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let pool = &database.app_pool;
        let harness = Harness::new(pool, RateLimits::default())?;
        let (account, key) = seed_test_account(pool, "auth").await?;

        let valid = harness
            .with_authorization(Some(&format!("Bearer {key}")))
            .await?;
        ensure!(valid.status == StatusCode::OK, "{}", valid.body);
        let lowercase = harness
            .with_authorization(Some(&format!("bearer {key}")))
            .await?;
        ensure!(lowercase.status == StatusCode::OK);
        let last_used: Option<chrono::DateTime<Utc>> =
            sqlx::query_scalar("SELECT last_used_at FROM api_keys WHERE account_id = $1")
                .bind(account)
                .fetch_one(pool)
                .await?;
        ensure!(last_used.is_some());

        let missing = harness.with_authorization(None).await?;
        ensure!(missing.status == StatusCode::UNAUTHORIZED);
        ensure!(missing.body["error"]["code"] == "api_key_missing");
        ensure!(missing.headers["www-authenticate"] == "Bearer realm=\"Phala Pay\"");

        let basic = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            format!("{key}:"),
        );
        let mut checksum_flipped = key.clone();
        let last = checksum_flipped.pop().context("key")?;
        checksum_flipped.push(if last == 'A' { 'B' } else { 'A' });
        let unknown = api_keys::generate(KeyKind::Secret, false)?;
        for authorization in [
            format!("Basic {basic}"),
            format!("Token {key}"),
            "Bearer".to_owned(),
            // Another vendor's key shape, built so no scanner mistakes it for a real key.
            format!("Bearer sk_{}_{}", "test", "x".repeat(24)),
            format!("Bearer {}", &key[..key.len() - 1]),
            format!("Bearer {checksum_flipped}"),
            format!("Bearer {}", unknown.as_str()),
        ] {
            let refused = harness.with_authorization(Some(&authorization)).await?;
            ensure!(
                refused.status == StatusCode::UNAUTHORIZED
                    && refused.body["error"]["code"] == "api_key_invalid",
                "{authorization}: {} {}",
                refused.status,
                refused.body
            );
        }

        // A rolled key keeps working until its expiry, then answers `api_key_expired`.
        let key_id = harness.get("/v1/api_keys", &key).await?.body["data"][0]["id"]
            .as_str()
            .context("key id")?
            .to_owned();
        let rolled = harness
            .merchant(
                Method::POST,
                &format!("/v1/api_keys/{key_id}/roll"),
                Some(&json!({"expires_in": 3600})),
                &key,
                None,
            )
            .await?;
        ensure!(rolled.status == StatusCode::OK, "{}", rolled.body);
        let new_key = secret(&rolled.body)?;
        ensure!(harness.get("/v1/account", &key).await?.status == StatusCode::OK);
        ensure!(harness.get("/v1/account", &new_key).await?.status == StatusCode::OK);
        let old = harness
            .get(&format!("/v1/api_keys/{key_id}"), &new_key)
            .await?;
        ensure!(old.body["status"] == "expiring", "{}", old.body);
        ensure!(old.body["expires_at"].as_i64() > Some(Utc::now().timestamp() + 3500));
        sqlx::query("UPDATE api_keys SET expires_at = now() - interval '1 second' WHERE id = $1")
            .bind(topup::ids::parse(topup::ids::API_KEY, &key_id).context("key id")?)
            .execute(&database.owner_pool)
            .await?;
        let expired = harness.get("/v1/account", &key).await?;
        ensure!(expired.status == StatusCode::UNAUTHORIZED);
        ensure!(expired.body["error"]["code"] == "api_key_expired");

        // Rolling with no overlap revokes the old key at once.
        let new_id = rolled.body["id"].as_str().context("id")?.to_owned();
        let rolled_again = harness
            .merchant(
                Method::POST,
                &format!("/v1/api_keys/{new_id}/roll"),
                Some(&json!({})),
                &new_key,
                None,
            )
            .await?;
        ensure!(rolled_again.status == StatusCode::OK);
        let revoked = harness.get("/v1/account", &new_key).await?;
        ensure!(revoked.body["error"]["code"] == "api_key_invalid");
        ensure!(
            harness
                .get("/v1/account", &secret(&rolled_again.body)?)
                .await?
                .status
                == StatusCode::OK
        );
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

/// A secret key creates, lists, rolls, and revokes its mode's keys; the last active key cannot
/// be revoked, and every change is an event naming the acting key.
#[tokio::test]
async fn merchants_manage_their_keys_and_cannot_lock_themselves_out() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let pool = &database.app_pool;
        let harness = Harness::new(pool, RateLimits::default())?;
        let (account, key) = seed_test_account(pool, "manage").await?;
        let first_id = harness.get("/v1/api_keys", &key).await?.body["data"][0]["id"]
            .as_str()
            .context("id")?
            .to_owned();

        let last = harness
            .merchant(
                Method::DELETE,
                &format!("/v1/api_keys/{first_id}"),
                None,
                &key,
                None,
            )
            .await?;
        ensure!(last.status == StatusCode::CONFLICT);
        ensure!(last.body["error"]["code"] == "last_api_key");

        let long_name = "n".repeat(201);
        let refused = harness
            .merchant(
                Method::POST,
                "/v1/api_keys",
                Some(&json!({"name": long_name})),
                &key,
                None,
            )
            .await?;
        ensure!(refused.status == StatusCode::BAD_REQUEST);

        let created = harness
            .merchant(
                Method::POST,
                "/v1/api_keys",
                Some(&json!({"name": "ci deploys"})),
                &key,
                None,
            )
            .await?;
        ensure!(created.status == StatusCode::OK, "{}", created.body);
        ensure!(created.body["name"] == "ci deploys" && created.body["status"] == "active");
        ensure!(created.body["type"] == "secret" && created.body["livemode"] == false);
        let second = secret(&created.body)?;
        let second_id = created.body["id"].as_str().context("id")?.to_owned();
        let created_by: String =
            sqlx::query_scalar("SELECT created_by FROM api_keys WHERE id = $1")
                .bind(topup::ids::parse(topup::ids::API_KEY, &second_id).context("id")?)
                .fetch_one(pool)
                .await?;
        ensure!(created_by == first_id);

        for body in [json!({"expires_in": 604_801}), json!({"expires_in": -1})] {
            let refused = harness
                .merchant(
                    Method::POST,
                    &format!("/v1/api_keys/{second_id}/roll"),
                    Some(&body),
                    &key,
                    None,
                )
                .await?;
            ensure!(refused.status == StatusCode::BAD_REQUEST, "{body}");
        }
        let rolled = harness
            .merchant(
                Method::POST,
                &format!("/v1/api_keys/{second_id}/roll"),
                Some(&json!({"expires_in": 604_800})),
                &key,
                None,
            )
            .await?;
        ensure!(rolled.status == StatusCode::OK);
        ensure!(rolled.body["name"] == "ci deploys");
        let again = harness
            .merchant(
                Method::POST,
                &format!("/v1/api_keys/{second_id}/roll"),
                Some(&json!({})),
                &key,
                None,
            )
            .await?;
        ensure!(again.status == StatusCode::CONFLICT);
        ensure!(again.body["error"]["code"] == "api_key_inactive");

        // With other active keys, a key may revoke itself; revoking twice returns it unchanged.
        let revoked = harness
            .merchant(
                Method::DELETE,
                &format!("/v1/api_keys/{first_id}"),
                None,
                &key,
                None,
            )
            .await?;
        ensure!(revoked.status == StatusCode::OK && revoked.body["status"] == "revoked");
        ensure!(harness.get("/v1/account", &key).await?.status == StatusCode::UNAUTHORIZED);
        let twice = harness
            .merchant(
                Method::DELETE,
                &format!("/v1/api_keys/{first_id}"),
                None,
                &second,
                None,
            )
            .await?;
        ensure!(twice.status == StatusCode::OK && twice.body["status"] == "revoked");

        let listed = harness.get("/v1/api_keys", &second).await?;
        let statuses: Vec<Value> = listed.body["data"]
            .as_array()
            .context("data")?
            .iter()
            .map(|key| key["status"].clone())
            .collect();
        ensure!(
            statuses == [json!("active"), json!("expiring"), json!("revoked")],
            "{statuses:?}"
        );

        let events = events(pool, account).await?;
        let kinds: Vec<&str> = events.iter().map(|(kind, _, _)| kind.as_str()).collect();
        ensure!(
            kinds
                .iter()
                .filter(|kind| **kind == "api_key.created")
                .count()
                == 2
        );
        ensure!(kinds.contains(&"api_key.updated") && kinds.contains(&"api_key.revoked"));
        ensure!(
            events
                .iter()
                .all(|(_, livemode, actor)| !livemode && actor.starts_with("key_")),
            "{events:?}"
        );
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

/// Every merchant POST is idempotent per account and mode: a repeat of the same request replays
/// the first response, a different request with the same key is refused, and a key's secret is
/// never stored for replay.
#[tokio::test]
async fn posts_are_idempotent_per_account_and_mode() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let pool = &database.app_pool;
        let harness = Harness::new(pool, RateLimits::default())?;
        let (account, key) = seed_test_account(pool, "idempotent").await?;
        let (_, other_key) = seed_test_account(pool, "other").await?;
        let create = |body: Value, key: String, idempotency_key: &'static str| {
            let harness = &harness;
            async move {
                harness
                    .merchant(
                        Method::POST,
                        "/v1/api_keys",
                        Some(&body),
                        &key,
                        Some(idempotency_key),
                    )
                    .await
            }
        };
        let count = || async {
            anyhow::Ok(
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM api_keys WHERE account_id = $1")
                    .bind(account)
                    .fetch_one(pool)
                    .await?,
            )
        };

        let first = create(json!({"name": "a"}), key.clone(), "retry-1").await?;
        ensure!(first.status == StatusCode::OK && first.body.get("secret").is_some());
        ensure!(!first.headers.contains_key("idempotent-replayed"));
        let replay = create(json!({"name": "a"}), key.clone(), "\"retry-1\"").await?;
        ensure!(replay.status == StatusCode::OK);
        ensure!(replay.headers["idempotent-replayed"] == "true");
        ensure!(replay.body["id"] == first.body["id"]);
        ensure!(replay.body.get("secret").is_none(), "secrets are never stored");
        ensure!(count().await? == 2);
        let stored: String = sqlx::query_scalar(
            "SELECT response::text FROM idempotency_keys WHERE account_id = $1 AND key = 'retry-1'",
        )
        .bind(account)
        .fetch_one(pool)
        .await?;
        ensure!(!stored.contains(&secret(&first.body)?), "{stored}");

        let other_body = create(json!({"name": "b"}), key.clone(), "retry-1").await?;
        ensure!(other_body.status == StatusCode::BAD_REQUEST);
        ensure!(other_body.body["error"]["type"] == "idempotency_error");
        ensure!(other_body.body["error"]["code"] == "idempotency_key_reused");
        let other_path = harness
            .merchant(
                Method::POST,
                "/v1/quotes",
                Some(&json!({"name": "a"})),
                &key,
                Some("retry-1"),
            )
            .await?;
        ensure!(other_path.status == StatusCode::BAD_REQUEST);
        ensure!(other_path.body["error"]["type"] == "idempotency_error");

        // Keys are scoped per account and mode.
        let other_account = create(json!({"name": "a"}), other_key.clone(), "retry-1").await?;
        ensure!(other_account.status == StatusCode::OK);
        ensure!(!other_account.headers.contains_key("idempotent-replayed"));
        let live_key = seed::create_api_key(pool, account, true).await?;
        sqlx::query("UPDATE accounts SET charges_enabled = true WHERE id = $1")
            .bind(account)
            .execute(pool)
            .await?;
        let other_mode = create(json!({"name": "a"}), live_key, "retry-1").await?;
        ensure!(other_mode.status == StatusCode::OK && other_mode.body["livemode"] == true);
        ensure!(!other_mode.headers.contains_key("idempotent-replayed"));

        // A refused request is replayed as refused.
        let long = json!({"name": "n".repeat(201)});
        let refused = create(long.clone(), key.clone(), "retry-2").await?;
        ensure!(refused.status == StatusCode::BAD_REQUEST);
        let refused_again = create(long, key.clone(), "retry-2").await?;
        ensure!(refused_again.status == StatusCode::BAD_REQUEST);
        ensure!(refused_again.headers["idempotent-replayed"] == "true");

        // A request still running holds its key; one that never finished frees it after a
        // minute; after 24 hours a key may be used for anything.
        let before = count().await?;
        sqlx::query(
            "INSERT INTO idempotency_keys (account_id, livemode, key, fingerprint) \
             VALUES ($1, false, 'running', $2)",
        )
        .bind(account)
        .bind(vec![0_u8; 32])
        .execute(pool)
        .await?;
        let running = create(json!({"name": "a"}), key.clone(), "running").await?;
        ensure!(running.status == StatusCode::BAD_REQUEST, "another fingerprint");
        let fingerprint: Vec<u8> = {
            let mut digest = <sha2::Sha256 as sha2::Digest>::new();
            sha2::Digest::update(&mut digest, b"POST\0/v1/api_keys\0{\"name\":\"a\"}");
            sha2::Digest::finalize(digest).to_vec()
        };
        sqlx::query(
            "UPDATE idempotency_keys SET fingerprint = $2 WHERE account_id = $1 AND key = 'running'",
        )
        .bind(account)
        .bind(&fingerprint)
        .execute(pool)
        .await?;
        let in_use = create(json!({"name": "a"}), key.clone(), "running").await?;
        ensure!(in_use.status == StatusCode::CONFLICT, "{}", in_use.body);
        ensure!(in_use.body["error"]["code"] == "idempotency_key_in_use");
        sqlx::query(
            "UPDATE idempotency_keys SET created_at = now() - interval '2 minutes' \
             WHERE account_id = $1 AND key = 'running'",
        )
        .bind(account)
        .execute(pool)
        .await?;
        let taken_over = create(json!({"name": "a"}), key.clone(), "running").await?;
        ensure!(taken_over.status == StatusCode::OK, "{}", taken_over.body);
        ensure!(count().await? == before + 1);
        sqlx::query(
            "UPDATE idempotency_keys SET created_at = now() - interval '25 hours' \
             WHERE account_id = $1 AND key = 'running'",
        )
        .bind(account)
        .execute(pool)
        .await?;
        let reused = create(json!({"name": "c"}), key.clone(), "running").await?;
        ensure!(reused.status == StatusCode::OK && reused.body["name"] == "c");
        let pruned: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM idempotency_keys WHERE created_at < now() - interval '24 hours'",
        )
        .fetch_one(pool)
        .await?;
        ensure!(pruned == 0);
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

/// Each account and mode has its own rate limit, and test mode shares a platform ceiling.
#[tokio::test]
async fn requests_are_rate_limited_per_account_and_mode() -> Result<()> {
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = async {
        let pool = &database.app_pool;
        let harness = Harness::new(
            pool,
            RateLimits {
                live: 3,
                test: 2,
                test_platform: 3,
            },
        )?;
        let (account, test_key) = seed_test_account(pool, "limited").await?;
        sqlx::query("UPDATE accounts SET charges_enabled = true WHERE id = $1")
            .bind(account)
            .execute(pool)
            .await?;
        let live_key = seed::create_api_key(pool, account, true).await?;
        let (_, other_key) = seed_test_account(pool, "neighbour").await?;

        let mut statuses = Vec::new();
        for _ in 0..3 {
            statuses.push(harness.get("/v1/account", &test_key).await?.status);
        }
        ensure!(
            statuses
                == [
                    StatusCode::OK,
                    StatusCode::OK,
                    StatusCode::TOO_MANY_REQUESTS
                ],
            "{statuses:?}"
        );
        let limited = harness.get("/v1/account", &test_key).await?;
        ensure!(limited.body["error"]["code"] == "rate_limit");
        // The live mode of the same account has its own limit.
        for _ in 0..3 {
            ensure!(harness.get("/v1/account", &live_key).await?.status == StatusCode::OK);
        }
        ensure!(
            harness.get("/v1/account", &live_key).await?.status == StatusCode::TOO_MANY_REQUESTS
        );
        // Another test account has its own limit but shares the test-mode ceiling of 3, of
        // which the first account used 2.
        ensure!(harness.get("/v1/account", &other_key).await?.status == StatusCode::OK);
        ensure!(
            harness.get("/v1/account", &other_key).await?.status == StatusCode::TOO_MANY_REQUESTS
        );
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

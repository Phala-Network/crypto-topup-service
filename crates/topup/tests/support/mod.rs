#![allow(dead_code)]

pub mod chain;

use std::env;
use std::future::Future;
use std::pin::Pin;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::http::{Method, Request};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer as _, SigningKey};
use sfv::{DictSerializer, Integer, KeyRef, ListSerializer, StringRef};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use url::Url;
use uuid::Uuid;

/// Waits out transient `max_connections` exhaustion when many test databases share one
/// server under load; sqlx's 30 s default turns that into spurious `PoolTimedOut` failures.
pub const DB_ACQUIRE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

pub type TestFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + 'a>>;

/// An isolated, migrated database with an owner connection and a `topup_app` login.
pub struct TestDatabase {
    admin_pool: PgPool,
    /// Connects as the migration owner; only for migrations and fault injection.
    pub owner_pool: PgPool,
    pub app_pool: PgPool,
    pub owner_url: String,
    pub app_url: String,
    database_name: String,
    app_role: String,
}

/// Creates the cluster-wide `topup_app` role before any per-test database is migrated.
///
/// Roles are shared by every database in the cluster, so parallel per-test migrations of
/// `20260922000001` would otherwise race on `CREATE ROLE` on a fresh cluster. The advisory lock
/// key is shared by every test binary; the migration then finds the role and skips creating it.
pub async fn ensure_app_role(admin_pool: &PgPool) -> Result<()> {
    let mut transaction = admin_pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(704_200_000)")
        .execute(&mut *transaction)
        .await?;
    transaction
        .execute(
            "DO $$ BEGIN \
             IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'topup_app') THEN \
             CREATE ROLE topup_app NOLOGIN; \
             END IF; \
             END $$",
        )
        .await?;
    transaction
        .commit()
        .await
        .context("create topup_app role")?;
    Ok(())
}

impl TestDatabase {
    pub async fn create() -> Result<Option<Self>> {
        let Some(owner_template) = required_url("MIGRATE_DATABASE_URL") else {
            return Ok(None);
        };
        let Some(app_template) = required_url("DATABASE_URL") else {
            return Ok(None);
        };

        let mut admin_url = Url::parse(&owner_template)?;
        admin_url.set_path("/postgres");
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(DB_ACQUIRE_TIMEOUT)
            .connect(admin_url.as_str())
            .await?;
        ensure_app_role(&admin_pool).await?;
        sqlx::query("SELECT pg_advisory_lock(704_209_001)")
            .execute(&admin_pool)
            .await?;

        let suffix = Uuid::new_v4().simple().to_string();
        let database_name = format!("topup_test_{suffix}");
        let app_role = format!("topup_test_app_{suffix}");
        let password = format!("test_{suffix}");
        admin_pool
            .execute(format!("CREATE DATABASE \"{database_name}\"").as_str())
            .await?;

        let mut owner_url = Url::parse(&owner_template)?;
        owner_url.set_path(&format!("/{database_name}"));
        let owner_pool = PgPoolOptions::new()
            .max_connections(4)
            .acquire_timeout(DB_ACQUIRE_TIMEOUT)
            .connect(owner_url.as_str())
            .await?;
        topup::db::migrate(&owner_pool).await?;

        admin_pool
            .execute(
                format!("CREATE ROLE \"{app_role}\" LOGIN PASSWORD '{password}' IN ROLE topup_app")
                    .as_str(),
            )
            .await?;
        let mut app_url = Url::parse(&app_template)?;
        app_url
            .set_username(&app_role)
            .map_err(|()| anyhow::anyhow!("DATABASE_URL cannot accept a username"))?;
        app_url
            .set_password(Some(&password))
            .map_err(|()| anyhow::anyhow!("DATABASE_URL cannot accept a password"))?;
        app_url.set_path(&format!("/{database_name}"));
        let app_pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(DB_ACQUIRE_TIMEOUT)
            .connect(app_url.as_str())
            .await?;

        sqlx::query("SELECT pg_advisory_unlock(704_209_001)")
            .execute(&admin_pool)
            .await?;
        Ok(Some(Self {
            admin_pool,
            owner_pool,
            app_pool,
            owner_url: owner_url.into(),
            app_url: app_url.into(),
            database_name,
            app_role,
        }))
    }

    pub async fn cleanup(self) -> Result<()> {
        self.app_pool.close().await;
        self.owner_pool.close().await;
        self.admin_pool
            .execute(format!("DROP DATABASE \"{}\" WITH (FORCE)", self.database_name).as_str())
            .await
            .context("drop test database")?;
        self.admin_pool
            .execute(format!("DROP ROLE \"{}\"", self.app_role).as_str())
            .await
            .context("drop test role")?;
        self.admin_pool.close().await;
        Ok(())
    }
}

/// Runs `test` against a fresh [`TestDatabase`] and drops it afterwards, or skips when the
/// database URLs are not set.
pub async fn with_database<F>(test: F) -> Result<()>
where
    F: for<'a> FnOnce(&'a TestDatabase) -> TestFuture<'a>,
{
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let result = test(&database).await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

/// Public origin the test routers are configured with and requests are signed for by default.
pub const TEST_ORIGIN: &str = "http://api.test";

pub fn public_key_base64(key: &SigningKey) -> String {
    STANDARD.encode(key.verifying_key().as_bytes())
}

pub fn signed_request(
    method: Method,
    path: &str,
    body: Vec<u8>,
    kid: &str,
    key: &SigningKey,
    created: i64,
) -> Request<Body> {
    signed_request_with_options(
        method,
        path,
        body,
        kid,
        key,
        created,
        &SignatureOptions::default(),
    )
}

#[derive(Clone, Debug)]
pub enum SignatureParameter {
    Created,
    KeyId,
    Algorithm(String),
}

#[derive(Clone, Debug)]
pub struct SignatureOptions {
    pub label: String,
    pub parameters: Vec<SignatureParameter>,
    pub origin_form: bool,
    pub idempotency_key: Option<String>,
    /// Origin the signer addressed; `@target-uri` is this origin plus `path`.
    pub origin: String,
}

impl Default for SignatureOptions {
    fn default() -> Self {
        Self {
            label: "sig1".to_owned(),
            parameters: vec![
                SignatureParameter::Created,
                SignatureParameter::KeyId,
                SignatureParameter::Algorithm("ed25519".to_owned()),
            ],
            origin_form: false,
            idempotency_key: None,
            origin: TEST_ORIGIN.to_owned(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn signed_request_with_options(
    method: Method,
    path: &str,
    body: Vec<u8>,
    kid: &str,
    key: &SigningKey,
    created: i64,
    options: &SignatureOptions,
) -> Request<Body> {
    let target_uri = format!("{}{path}", options.origin);
    let digest = STANDARD.encode(Sha256::digest(&body));
    let content_digest = format!("sha-256=:{digest}:");
    let signature_parameters = signature_parameters(
        kid,
        created,
        &options.parameters,
        options.idempotency_key.is_some(),
    );
    let mut base = format!(
        "\"@method\": {}\n\"@target-uri\": {target_uri}\n\"content-digest\": {content_digest}",
        method.as_str()
    );
    if let Some(idempotency_key) = &options.idempotency_key {
        base.push_str("\n\"idempotency-key\": ");
        base.push_str(idempotency_key);
    }
    base.push_str("\n\"@signature-params\": ");
    base.push_str(&signature_parameters);
    let signature = key.sign(base.as_bytes()).to_bytes();
    let label = KeyRef::from_str(&options.label).expect("signature label must be an SFV key");
    let signature_input = format!("{label}={signature_parameters}");
    let mut signature_serializer = DictSerializer::new();
    let _ = signature_serializer
        .bare_item(label, signature.as_slice())
        .finish();
    let signature_header = signature_serializer
        .finish()
        .expect("signature dictionary must not be empty");
    let request_target = if options.origin_form {
        path
    } else {
        &target_uri
    };
    let mut request = Request::builder()
        .method(method)
        .uri(request_target)
        .header("host", "api.test")
        .header("content-type", "application/json")
        .header("content-digest", content_digest)
        .header("signature-input", signature_input)
        .header("signature", signature_header);
    if let Some(idempotency_key) = &options.idempotency_key {
        request = request.header("idempotency-key", idempotency_key);
    }
    request
        .body(Body::from(body))
        .expect("test request must be valid")
}

fn signature_parameters(
    kid: &str,
    created: i64,
    order: &[SignatureParameter],
    include_idempotency_key: bool,
) -> String {
    let mut serializer = ListSerializer::new();
    {
        let mut inner = serializer.inner_list();
        for component in ["@method", "@target-uri", "content-digest"] {
            let _ = inner
                .bare_item(
                    StringRef::from_str(component)
                        .expect("signature component must be an SFV string"),
                )
                .finish();
        }
        if include_idempotency_key {
            let _ = inner
                .bare_item(
                    StringRef::from_str("idempotency-key")
                        .expect("signature component must be an SFV string"),
                )
                .finish();
        }
        let mut parameters = inner.finish();
        for parameter in order {
            parameters = match parameter {
                SignatureParameter::Created => parameters.parameter(
                    KeyRef::from_str("created").expect("created must be an SFV key"),
                    Integer::try_from(created).expect("created must fit an SFV integer"),
                ),
                SignatureParameter::KeyId => parameters.parameter(
                    KeyRef::from_str("keyid").expect("keyid must be an SFV key"),
                    StringRef::from_str(kid).expect("kid must be an SFV string"),
                ),
                SignatureParameter::Algorithm(algorithm) => parameters.parameter(
                    KeyRef::from_str("alg").expect("alg must be an SFV key"),
                    StringRef::from_str(algorithm).expect("algorithm must be an SFV string"),
                ),
            };
        }
        let _ = parameters.finish();
    }
    serializer
        .finish()
        .expect("signature parameter inner list must not be empty")
}

fn required_url(name: &str) -> Option<String> {
    match env::var(name).ok().filter(|value| !value.is_empty()) {
        Some(value) => Some(value),
        None => {
            eprintln!("skipping integration test: {name} is not set");
            None
        }
    }
}

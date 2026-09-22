#![allow(dead_code)]

use std::env;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::http::{Method, Request};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer as _, SigningKey};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::{Executor, PgPool};
use url::Url;
use uuid::Uuid;

pub struct TestDatabase {
    admin_pool: PgPool,
    owner_pool: PgPool,
    pub app_pool: PgPool,
    database_name: String,
    app_role: String,
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
            .connect(admin_url.as_str())
            .await?;
        sqlx::query("SELECT pg_advisory_lock(704_209_001)")
            .execute(&admin_pool)
            .await?;

        let suffix = Uuid::new_v4().simple().to_string();
        let database_name = format!("topup_c9_{suffix}");
        let app_role = format!("topup_c9_app_{suffix}");
        let password = format!("c9_{suffix}");
        admin_pool
            .execute(format!("CREATE DATABASE \"{database_name}\"").as_str())
            .await?;

        let mut owner_url = Url::parse(&owner_template)?;
        owner_url.set_path(&format!("/{database_name}"));
        let owner_pool = PgPoolOptions::new()
            .max_connections(4)
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
            .connect(app_url.as_str())
            .await?;

        sqlx::query("SELECT pg_advisory_unlock(704_209_001)")
            .execute(&admin_pool)
            .await?;
        Ok(Some(Self {
            admin_pool,
            owner_pool,
            app_pool,
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
            .context("drop API test database")?;
        self.admin_pool
            .execute(format!("DROP ROLE \"{}\"", self.app_role).as_str())
            .await
            .context("drop API test role")?;
        self.admin_pool.close().await;
        Ok(())
    }
}

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
    let target_uri = format!("http://api.test{path}");
    let digest = STANDARD.encode(Sha256::digest(&body));
    let content_digest = format!("sha-256=:{digest}:");
    let signature_parameters = format!(
        "(\"@method\" \"@target-uri\" \"content-digest\");created={created};keyid=\"{kid}\""
    );
    let base = format!(
        "\"@method\": {}\n\"@target-uri\": {target_uri}\n\"content-digest\": {content_digest}\n\"@signature-params\": {signature_parameters}",
        method.as_str()
    );
    let signature = STANDARD.encode(key.sign(base.as_bytes()).to_bytes());
    Request::builder()
        .method(method)
        .uri(target_uri)
        .header("host", "api.test")
        .header("content-type", "application/json")
        .header("content-digest", content_digest)
        .header("signature-input", format!("sig1={signature_parameters}"))
        .header("signature", format!("sig1=:{signature}:"))
        .body(Body::from(body))
        .expect("test request must be valid")
}

fn required_url(name: &str) -> Option<String> {
    match env::var(name).ok().filter(|value| !value.is_empty()) {
        Some(value) => Some(value),
        None => {
            eprintln!("skipping API integration test: {name} is not set");
            None
        }
    }
}

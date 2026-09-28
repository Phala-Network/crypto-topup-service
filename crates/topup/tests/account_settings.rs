//! The account's own settings through the API (docs/design/multi-tenant.md D1, §12): a
//! confirmation policy stricter than a route's floor, reported by `GET /v1/config`, and the
//! merchant's own `quotes` pause, which never lifts the operator's.

mod support;

use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use axum::Router;
use axum::body::to_bytes;
use axum::http::{Method, StatusCode};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use topup::api::{AppState, PublicOrigin, VerificationKey};
use topup_adapters::attestation::DstackAttestor;
use topup_core::route::RouteFile;
use tower::ServiceExt;

use support::seed::{self, NewAccount};
use support::{TEST_ORIGIN, merchant_request, public_key_base64, with_database};

const TEST_CHAIN: u64 = 11_155_111;

#[tokio::test]
async fn a_confirmation_policy_is_only_ever_stricter_and_config_reports_it() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let pool = &database.app_pool;
            let fixture = Fixture::new(pool).await?;
            let asset = fixture.config_asset().await?;
            ensure!(asset["confirmations"] == "2" && asset["typical_credit_seconds"] == 30);

            let (status, account) = fixture
                .post(
                    "/v1/account",
                    json!({"confirmation_policies": [{"chain_id": 1, "confirmations": "12"}]}),
                )
                .await?;
            ensure!(status == StatusCode::OK, "{account}");
            ensure!(
                account["confirmation_policies"] == json!([{"chain_id": 1, "confirmations": "12"}]),
                "{account}"
            );
            let asset = fixture.config_asset().await?;
            ensure!(asset["confirmations"] == "12" && asset["typical_credit_seconds"] == 150);
            let (status, account) = fixture
                .post(
                    "/v1/account",
                    json!({"confirmation_policies": [{"chain_id": 1, "confirmations": "finalized"}]}),
                )
                .await?;
            ensure!(status == StatusCode::OK, "{account}");
            ensure!(fixture.config_asset().await?["confirmations"] == "finalized");
            let stored: String = sqlx::query_scalar(
                "SELECT required FROM confirmation_policies WHERE account_id = $1 AND chain_id = 1",
            )
            .bind(fixture.account.id)
            .fetch_one(pool)
            .await?;
            ensure!(stored == "finalized");

            // Weaker than the route, of another chain family, malformed, another mode's chain, or
            // listed twice: refused, naming the entry.
            for (policies, param) in [
                (json!([{"chain_id": 1, "confirmations": "1"}]), "confirmation_policies[0][confirmations]"),
                (json!([{"chain_id": 1, "confirmations": "safe"}]), "confirmation_policies[0][confirmations]"),
                (json!([{"chain_id": 1, "confirmations": "02"}]), "confirmation_policies[0][confirmations]"),
                (json!([{"chain_id": TEST_CHAIN, "confirmations": "finalized"}]), "confirmation_policies[0][chain_id]"),
                (
                    json!([{"chain_id": 1, "confirmations": "3"}, {"chain_id": 1, "confirmations": null}]),
                    "confirmation_policies[1][chain_id]",
                ),
            ] {
                let (status, body) = fixture
                    .post("/v1/account", json!({"confirmation_policies": policies}))
                    .await?;
                ensure!(status == StatusCode::BAD_REQUEST, "{policies}: {body}");
                ensure!(body["error"]["param"] == param, "{body}");
            }
            ensure!(fixture.config_asset().await?["confirmations"] == "finalized");

            // `null` removes it: the route's floor applies again.
            let (status, account) = fixture
                .post(
                    "/v1/account",
                    json!({"confirmation_policies": [{"chain_id": 1, "confirmations": null}]}),
                )
                .await?;
            ensure!(status == StatusCode::OK && account["confirmation_policies"] == json!([]));
            ensure!(fixture.config_asset().await?["confirmations"] == "2");
            // Each effective change is announced once in the key's mode; repeats write nothing.
            let (status, _) = fixture
                .post(
                    "/v1/account",
                    json!({"confirmation_policies": [{"chain_id": 1, "confirmations": null}]}),
                )
                .await?;
            ensure!(status == StatusCode::OK);
            let events: Vec<bool> = sqlx::query_scalar(
                "SELECT livemode FROM events WHERE account_id = $1 AND type = 'account.updated'",
            )
            .bind(fixture.account.id)
            .fetch_all(pool)
            .await?;
            ensure!(events == vec![true; 3], "{events:?}");
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_merchant_pauses_its_own_quotes_and_never_lifts_the_operators_pause() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let pool = &database.app_pool;
            let fixture = Fixture::new(pool).await?;
            let (status, account) = fixture
                .post("/v1/account/pause", json!({"scopes": ["quotes"]}))
                .await?;
            ensure!(status == StatusCode::OK, "{account}");
            ensure!(account["paused_scopes"] == json!(["quotes"]), "{account}");
            ensure!(fixture.quote_error().await? == "paused");
            let (status, body) = fixture
                .post(
                    "/v1/deposit_addresses",
                    json!({"client_reference_id": "team-42"}),
                )
                .await?;
            ensure!(status == StatusCode::CONFLICT && body["error"]["code"] == "paused");

            // The operator pauses too; the merchant's resume lifts only its own pause.
            seed::set_account_paused_scopes(pool, fixture.account.id, &["quotes".to_owned()])
                .await?;
            let (status, account) = fixture
                .post("/v1/account/resume", json!({"scopes": ["quotes"]}))
                .await?;
            ensure!(status == StatusCode::OK, "{account}");
            ensure!(account["paused_scopes"] == json!(["quotes"]), "{account}");
            ensure!(fixture.quote_error().await? == "paused");
            let own: Vec<String> =
                sqlx::query_scalar("SELECT self_paused_scopes FROM accounts WHERE id = $1")
                    .bind(fixture.account.id)
                    .fetch_one(pool)
                    .await
                    .map(|scopes: Vec<String>| scopes)?;
            ensure!(own.is_empty());
            seed::set_account_paused_scopes(pool, fixture.account.id, &[]).await?;
            ensure!(fixture.quote_error().await? != "paused");

            // Only `quotes` is the merchant's to pause.
            for scopes in [
                json!(["settlement"]),
                json!([]),
                json!(["quotes", "refunds"]),
            ] {
                let (status, body) = fixture
                    .post("/v1/account/pause", json!({"scopes": scopes}))
                    .await?;
                ensure!(status == StatusCode::BAD_REQUEST, "{body}");
                ensure!(body["error"]["param"] == "scopes");
            }
            Ok(())
        })
    })
    .await
}

struct Fixture {
    app: Router,
    account: topup::db::Account,
    live_key: String,
}

impl Fixture {
    async fn new(pool: &sqlx::PgPool) -> Result<Self> {
        let account = seed::create_account(pool, &NewAccount::named("merchant")).await?;
        let live_key = seed::create_api_key(pool, account.id, true).await?;
        seed::set_treasury(pool, account.id, true, 1, seed::FIXTURE_TREASURY).await?;
        let live_route: RouteFile = serde_saphyr::from_str(
            &include_str!("fixtures/phala-cloud-pha.yaml")
                .replace("confirmations: finalized", "confirmations: 2"),
        )?;
        let test_route: RouteFile = serde_saphyr::from_str(
            &include_str!("fixtures/phala-cloud-pha.yaml")
                .replace(
                    "route: phala-cloud-ethereum-pha-usd",
                    "route: phala-cloud-sepolia-pha",
                )
                .replace("chain_id: 1", &format!("chain_id: {TEST_CHAIN}"))
                .replace("livemode: true", "livemode: false"),
        )?;
        let admin_key = SigningKey::from_bytes(&[48; 32]);
        let app = topup::api::router(AppState {
            pool: pool.clone(),
            routes: Arc::new(
                topup::routes::RouteSet::new(vec![live_route, test_route])
                    .map_err(anyhow::Error::msg)?,
            ),
            admin_key: VerificationKey::from_base64(
                "admin/v1".to_owned(),
                &public_key_base64(&admin_key),
            )
            .map_err(anyhow::Error::msg)?,
            public_origin: PublicOrigin::parse(TEST_ORIGIN)?,
            attestor: Arc::new(DstackAttestor::new()),
            rate_lock_quotes: Arc::new(topup::locks::UnavailableQuoteProvider),
            client_reads: Arc::default(),
            rate_limits: Arc::default(),
            screening: Arc::new(topup::refunds::UnavailableDestinationScreener),
            contract_signatures: Arc::new(topup::treasuries::UnavailableContractSignatures),
        })
        .0;
        Ok(Self {
            app,
            account,
            live_key,
        })
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        body: Value,
    ) -> Result<(StatusCode, Value)> {
        let body = if body.is_null() {
            Vec::new()
        } else {
            serde_json::to_vec(&body)?
        };
        let response = self
            .app
            .clone()
            .oneshot(merchant_request(method, path, body, &self.live_key))
            .await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1_048_576).await?;
        Ok((status, serde_json::from_slice(&bytes)?))
    }

    async fn post(&self, path: &str, body: Value) -> Result<(StatusCode, Value)> {
        self.request(Method::POST, path, body).await
    }

    async fn config_asset(&self) -> Result<Value> {
        let (status, config) = self.request(Method::GET, "/v1/config", Value::Null).await?;
        ensure!(status == StatusCode::OK, "{config}");
        config["assets"]
            .as_array()
            .and_then(|assets| assets.first())
            .cloned()
            .context("one asset")
    }

    /// The error code of a quote request; pricing is unavailable in these tests, so an unpaused
    /// request fails later with another code.
    async fn quote_error(&self) -> Result<String> {
        let (status, body) = self
            .post(
                "/v1/quotes",
                json!({"client_reference_id": "team-42", "amount": 1000, "currency": "usd",
                       "chain_id": 1, "asset": "pha"}),
            )
            .await?;
        ensure!(!status.is_success(), "{body}");
        body["error"]["code"]
            .as_str()
            .map(str::to_owned)
            .context("error code")
    }
}

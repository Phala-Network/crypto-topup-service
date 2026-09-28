//! The sweep builder (`GET /v1/sweeps`, docs/design/multi-tenant.md D4) and the address export
//! (`GET /v1/addresses`, §13): sweepable balances come from final deposits and finalized
//! `Flushed` events only, a forwarder holding a sanctioned deposit is never swept, no call is
//! built to a sanctioned treasury, and the export names every address's derivation inputs.

mod support;

use std::sync::Arc;

use alloy_primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use axum::Router;
use axum::body::to_bytes;
use axum::http::{Method, StatusCode};
use chrono::Utc;
use ed25519_dalek::SigningKey;
use serde_json::Value;
use topup::api::{AppState, PublicOrigin, VerificationKey};
use topup::db::{self, NewDeposit};
use topup::refunds::{DestinationScreener, DestinationScreening};
use topup_adapters::attestation::DstackAttestor;
use topup_adapters::chain::flush::encode_flush;
use topup_core::deposit::{DepositState, RejectReason};
use topup_core::money::AtomicAmount;
use topup_core::route::RouteFile;
use tower::ServiceExt;
use uuid::Uuid;

use support::seed::{self, NewAccount, NewAddress};
use support::{TEST_ORIGIN, merchant_request, public_key_base64, with_database};

#[tokio::test]
async fn sweeps_list_final_unswept_balances_with_the_flush_call() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let pool = &database.app_pool;
            let fixture = Fixture::new(pool).await?;
            let token = fixture.route.asset.contract;
            // a: 100 final, 30 flushed -> 70. b: 50 final and 20 not final yet -> 50.
            // c: 10 final beside a sanctioned deposit -> never swept. d: reversed -> nothing.
            let a = fixture.address(1).await?;
            let b = fixture.address(2).await?;
            let c = fixture.address(3).await?;
            let d = fixture.address(4).await?;
            fixture
                .deposit(&a, 1, 100, DepositState::Credited, None, true)
                .await?;
            fixture.flushed(&a, 30).await?;
            fixture
                .deposit(&b, 2, 50, DepositState::Credited, None, true)
                .await?;
            fixture
                .deposit(&b, 3, 20, DepositState::Credited, None, false)
                .await?;
            fixture
                .deposit(&c, 4, 10, DepositState::Credited, None, true)
                .await?;
            fixture
                .deposit(
                    &c,
                    5,
                    5,
                    DepositState::Rejected,
                    Some(RejectReason::Sanctioned),
                    true,
                )
                .await?;
            fixture
                .deposit(&d, 6, 40, DepositState::Reversed, None, true)
                .await?;

            let (status, list) = fixture.get("/v1/sweeps", Clear).await?;
            ensure!(status == StatusCode::OK, "{list}");
            let sweeps = list["data"].as_array().context("data")?;
            ensure!(sweeps.len() == 1, "{list}");
            let sweep = &sweeps[0];
            ensure!(sweep["object"] == "sweep" && sweep["livemode"] == true);
            ensure!(sweep["chain_id"] == 1 && sweep["asset"] == "pha");
            ensure!(sweep["token"] == format!("{token:#x}"));
            ensure!(sweep["treasury"] == format!("{:#x}", seed::FIXTURE_TREASURY));
            ensure!(sweep["amount_atomic"] == "120", "{sweep}");
            let mut expected = [(a.address, a.salt, "70"), (b.address, b.salt, "50")];
            expected.sort_by_key(|(address, _, _)| format!("{address:#x}"));
            let listed = sweep["addresses"].as_array().context("addresses")?;
            ensure!(listed.len() == 2, "{sweep}");
            for ((address, salt, amount), listed) in expected.iter().zip(listed) {
                ensure!(listed["address"] == format!("{address:#x}"));
                ensure!(listed["salt"] == format!("{salt:#x}"));
                ensure!(listed["amount_atomic"] == *amount);
            }
            let factory = fixture.route.chain.contracts.forwarder_factory;
            ensure!(sweep["factory"] == format!("{factory:#x}"));
            ensure!(sweep["transaction"]["to"] == format!("{factory:#x}"));
            ensure!(sweep["transaction"]["value"] == "0");
            let salts = expected.iter().map(|(_, salt, _)| *salt).collect();
            ensure!(
                sweep["transaction"]["data"]
                    == format!("{:#x}", encode_flush(seed::FIXTURE_TREASURY, salts, token))
            );

            // Filters, another mode, a sanctioned treasury, and unavailable screening.
            let (_, other_chain) = fixture.get("/v1/sweeps?chain_id=10", Clear).await?;
            ensure!(other_chain["data"] == Value::Array(Vec::new()));
            let (_, other_token) = fixture
                .get(
                    &format!("/v1/sweeps?token={:#x}", Address::repeat_byte(9)),
                    Clear,
                )
                .await?;
            ensure!(other_token["data"] == Value::Array(Vec::new()));
            let (status, _) = fixture.get("/v1/sweeps?token=nope", Clear).await?;
            ensure!(status == StatusCode::BAD_REQUEST);
            let (_, sanctioned) = fixture.get("/v1/sweeps", Sanctioned).await?;
            ensure!(sanctioned["data"] == Value::Array(Vec::new()));
            let (status, _) = fixture.get("/v1/sweeps", Unavailable).await?;
            ensure!(status == StatusCode::SERVICE_UNAVAILABLE);
            let (_, test_mode) = fixture
                .get_with("/v1/sweeps", Clear, &fixture.test_key)
                .await?;
            ensure!(test_mode["data"] == Value::Array(Vec::new()));
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn the_address_export_names_every_address_derivation() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let pool = &database.app_pool;
            let fixture = Fixture::new(pool).await?;
            let mut issued = Vec::new();
            for number in 1..=3 {
                issued.push(fixture.address(number).await?);
            }
            let mut seen = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let path = match &cursor {
                    Some(id) => format!("/v1/addresses?limit=2&starting_after={id}"),
                    None => "/v1/addresses?limit=2".to_owned(),
                };
                let (status, page) = fixture.get(&path, Clear).await?;
                ensure!(status == StatusCode::OK, "{page}");
                let data = page["data"].as_array().context("data")?;
                seen.extend(data.iter().cloned());
                cursor = data
                    .last()
                    .and_then(|last| last["id"].as_str())
                    .map(str::to_owned);
                if page["has_more"] != true {
                    break;
                }
            }
            ensure!(seen.len() == 3, "{seen:?}");
            let route = &fixture.route;
            for address in &seen {
                ensure!(address["object"] == "address" && address["livemode"] == true);
                ensure!(
                    address["id"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("addr_"))
                );
                ensure!(
                    address["factory"] == format!("{:#x}", route.chain.contracts.forwarder_factory)
                );
                ensure!(
                    address["implementation"]
                        == format!("{:#x}", route.chain.contracts.implementation)
                );
                ensure!(address["treasury"] == format!("{:#x}", seed::FIXTURE_TREASURY));
                ensure!(address["client_reference_id"] == "team-42");
                ensure!(
                    address["quote"]
                        .as_str()
                        .is_some_and(|id| id.starts_with("qt_"))
                );
                ensure!(address["deposit_address"].is_null());
                let matching = issued
                    .iter()
                    .find(|issued| address["address"] == format!("{:#x}", issued.address))
                    .context("an issued address")?;
                ensure!(address["salt"] == format!("{:#x}", matching.salt));
            }
            // Ids are unique and the pages do not overlap.
            let mut ids: Vec<&str> = seen.iter().filter_map(|a| a["id"].as_str()).collect();
            ids.sort_unstable();
            ids.dedup();
            ensure!(ids.len() == 3);
            let (_, test_mode) = fixture
                .get_with("/v1/addresses", Clear, &fixture.test_key)
                .await?;
            ensure!(test_mode["data"] == Value::Array(Vec::new()));
            let (status, _) = fixture.get("/v1/addresses?limit=0", Clear).await?;
            ensure!(status == StatusCode::BAD_REQUEST);
            Ok(())
        })
    })
    .await
}

struct Fixture {
    pool: sqlx::PgPool,
    route: RouteFile,
    customer: Uuid,
    live_key: String,
    test_key: String,
}

struct Issued {
    id: Uuid,
    address: Address,
    salt: B256,
}

impl Fixture {
    async fn new(pool: &sqlx::PgPool) -> Result<Self> {
        let (account, customer) =
            seed::create_account_and_customer(pool, &NewAccount::named("merchant"), "team-42")
                .await?;
        seed::set_treasury(pool, account.id, true, 1, seed::FIXTURE_TREASURY).await?;
        Ok(Self {
            pool: pool.clone(),
            route: serde_saphyr::from_str(include_str!("fixtures/phala-cloud-pha.yaml"))?,
            customer: customer.id,
            live_key: seed::create_api_key(pool, account.id, true).await?,
            test_key: seed::create_api_key(pool, account.id, false).await?,
        })
    }

    async fn address(&self, number: u8) -> Result<Issued> {
        let issued = Issued {
            id: Uuid::new_v4(),
            address: Address::repeat_byte(0x40 + number),
            salt: B256::repeat_byte(number),
        };
        seed::insert_address(
            &self.pool,
            &NewAddress {
                id: issued.id,
                customer_id: self.customer,
                chain_id: 1,
                route: self.route.route.clone(),
                salt: issued.salt,
                address: issued.address,
            },
        )
        .await?;
        Ok(issued)
    }

    async fn deposit(
        &self,
        address: &Issued,
        number: u8,
        amount: u64,
        state: DepositState,
        reason: Option<RejectReason>,
        is_final: bool,
    ) -> Result<()> {
        ensure!(
            db::insert_deposit(
                &self.pool,
                &NewDeposit {
                    chain_id: 1,
                    tx_hash: B256::repeat_byte(0xd0 + number),
                    receipt_log_index: 0,
                    log_index: 0,
                    block_number: 10,
                    block_hash: B256::repeat_byte(0xbb),
                    block_time: Utc::now(),
                    address_id: address.id,
                    route: Some(self.route.route.clone()),
                    route_version: Some(self.route.version),
                    asset_contract: self.route.asset.contract,
                    from_address: Address::repeat_byte(0x74),
                    amount_atomic: AtomicAmount::new(U256::from(amount)),
                    state,
                    reason,
                    next_attempt_at: Utc::now(),
                    tx_from: Address::repeat_byte(0x74),
                    tx_nonce: u64::from(number),
                    is_final,
                },
            )
            .await?
        );
        Ok(())
    }

    async fn flushed(&self, address: &Issued, amount: u64) -> Result<()> {
        sqlx::query(
            "INSERT INTO flushed (chain_id, tx_hash, log_index, address_id, token, treasury, \
             amount_atomic, block_number, block_hash) \
             VALUES (1, $1, 0, $2, $3, $4, $5::numeric, 11, $6)",
        )
        .bind(format!("{:#x}", B256::repeat_byte(0xf1)))
        .bind(address.id)
        .bind(format!("{:#x}", self.route.asset.contract))
        .bind(format!("{:#x}", seed::FIXTURE_TREASURY))
        .bind(amount.to_string())
        .bind(format!("{:#x}", B256::repeat_byte(0xbc)))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn get(
        &self,
        path: &str,
        screening: impl DestinationScreener + 'static,
    ) -> Result<(StatusCode, Value)> {
        self.get_with(path, screening, &self.live_key).await
    }

    async fn get_with(
        &self,
        path: &str,
        screening: impl DestinationScreener + 'static,
        key: &str,
    ) -> Result<(StatusCode, Value)> {
        let response = self
            .app(Arc::new(screening))?
            .oneshot(merchant_request(Method::GET, path, Vec::new(), key))
            .await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1_048_576).await?;
        Ok((status, serde_json::from_slice(&bytes)?))
    }

    fn app(&self, screening: Arc<dyn DestinationScreener>) -> Result<Router> {
        let admin_key = SigningKey::from_bytes(&[49; 32]);
        Ok(topup::api::router(AppState {
            pool: self.pool.clone(),
            routes: Arc::new(
                topup::routes::RouteSet::new(vec![self.route.clone()])
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
            screening,
            contract_signatures: Arc::new(topup::treasuries::UnavailableContractSignatures),
        })
        .0)
    }
}

struct Clear;
struct Sanctioned;
struct Unavailable;

#[async_trait::async_trait]
impl DestinationScreener for Clear {
    async fn screen(&self, _: &RouteFile, _: Address) -> DestinationScreening {
        DestinationScreening::Clear
    }
}

#[async_trait::async_trait]
impl DestinationScreener for Sanctioned {
    async fn screen(&self, _: &RouteFile, _: Address) -> DestinationScreening {
        DestinationScreening::Sanctioned
    }
}

#[async_trait::async_trait]
impl DestinationScreener for Unavailable {
    async fn screen(&self, _: &RouteFile, _: Address) -> DestinationScreening {
        DestinationScreening::Unavailable
    }
}

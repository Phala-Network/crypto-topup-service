//! Treasuries through the API (docs/design/multi-tenant.md D10): EIP-4361 proofs by an EOA or,
//! on Anvil, by a deployed EIP-1271 contract; refused proofs; the 48-hour time-lock of a live
//! change, its cancellation, and the deposit address networks it moves; tenancy.

mod support;

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

use alloy_primitives::{Address, B256, Bytes, U256, eip191_hash_message};
use alloy_signer::Signer as _;
use alloy_signer_local::PrivateKeySigner;
use anyhow::{Context, Result, ensure};
use async_trait::async_trait;
use axum::Router;
use axum::body::to_bytes;
use axum::http::{Method, StatusCode};
use chrono::{Duration, Utc};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use topup::api::{AppState, PublicOrigin, VerificationKey};
use topup::db;
use topup::locks::QuoteProvider;
use topup::locks::pricing::ValidatedQuote;
use topup::refunds::{DestinationScreener, DestinationScreening};
use topup::routes::RouteSet;
use topup::treasuries::{
    ContractAnswer, ContractSignatures, EvmContractSignatures, TIME_LOCK, apply_due,
};
use topup_adapters::attestation::DstackAttestor;
use topup_adapters::chain::evm::EvmClient;
use topup_core::money::{AtomicAmount, PRICE_SCALE, ScaledPrice};
use topup_core::route::RouteFile;
use tower::ServiceExt;

use support::chain::{ANVIL_PRIVATE_KEY, Anvil, CHAIN_ID, forge_create, run_checked};
use support::seed::{self, NewAccount};
use support::{TEST_ORIGIN, TestDatabase, merchant_request, public_key_base64, with_database};

/// A second live chain (OP Mainnet) beside Ethereum's chain 1.
const OTHER_CHAIN: u64 = 10;
const SEPOLIA: u64 = 11_155_111;

#[tokio::test]
async fn an_eoa_proves_its_first_treasury_with_siwe_and_it_applies_at_once() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let fixture = Fixture::new(&database.app_pool, Contracts::None).await?;
            let signer = PrivateKeySigner::random();
            // No treasury yet: nothing can be issued.
            let (status, body) = fixture.quote(&fixture.live_key, 1).await?;
            ensure!(status == StatusCode::CONFLICT, "{body}");
            ensure!(body["error"]["code"] == "treasury_not_set");

            let challenge = fixture
                .challenge(&fixture.live_key, 1, signer.address())
                .await?;
            let message = challenge["message"].as_str().context("message")?;
            let account = &fixture.account.public_id;
            let lines: Vec<&str> = message.lines().collect();
            ensure!(lines[0] == "api.test wants you to sign in with your Ethereum account:");
            ensure!(lines[1] == signer.address().to_checksum(None));
            ensure!(
                lines[3]
                    == format!(
                        "Set this address as the live mode treasury of {account} on Phala Pay."
                    )
            );
            ensure!(lines[5] == "URI: http://api.test" && lines[6] == "Version: 1");
            ensure!(lines[7] == "Chain ID: 1");
            ensure!(
                lines[8] == format!("Nonce: {}", challenge["nonce"].as_str().context("nonce")?)
            );
            ensure!(lines[10].starts_with("Expiration Time: "));
            let expires_in =
                challenge["expires_at"].as_i64().context("expires_at")? - Utc::now().timestamp();
            ensure!((590..=600).contains(&expires_in), "{expires_in}");

            let signature = sign(&signer, message).await?;
            let (status, treasury) = fixture
                .submit(&fixture.live_key, 1, message, &signature)
                .await?;
            ensure!(status == StatusCode::OK, "{treasury}");
            ensure!(treasury["object"] == "treasury" && treasury["livemode"] == true);
            ensure!(treasury["status"] == "active" && treasury["kind"] == "eoa");
            ensure!(treasury["address"] == format!("{:#x}", signer.address()));
            ensure!(treasury["effective_at"] == treasury["created"]);
            let id = treasury["id"].as_str().context("id")?;
            ensure!(id.starts_with("trs_"));

            // Quotes and deposit addresses pay it on its chain, and only there.
            let (status, quote) = fixture.quote(&fixture.live_key, 1).await?;
            ensure!(status == StatusCode::OK, "{quote}");
            ensure!(quote["treasury"] == format!("{:#x}", signer.address()));
            let (status, _) = fixture.quote(&fixture.live_key, OTHER_CHAIN).await?;
            ensure!(status == StatusCode::CONFLICT);
            let address = fixture.deposit_address(&fixture.live_key, "team-1").await?;
            ensure!(chain_ids(&address) == vec![1], "{address}");

            // The account security event carries the account and mode and reaches an endpoint
            // that subscribes to other events only.
            let events = fixture.events("account.treasury.updated").await?;
            ensure!(events == vec![(fixture.account.id, true, "treasury".to_owned())]);
            ensure!(fixture.deliveries("account.treasury.updated").await? == 1);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn proofs_are_refused_unless_they_answer_an_unused_live_challenge_of_the_scope() -> Result<()>
{
    with_database(|database| {
        Box::pin(async move {
            let fixture = Fixture::new(&database.app_pool, Contracts::None).await?;
            let signer = PrivateKeySigner::random();
            let refused = |status: StatusCode, body: &Value, code: &str, param: &str| {
                ensure!(status == StatusCode::BAD_REQUEST, "{body}");
                ensure!(body["error"]["code"] == code, "{body}");
                ensure!(body["error"]["param"] == param, "{body}");
                Ok(())
            };

            // Expired.
            let challenge = fixture
                .challenge(&fixture.live_key, 1, signer.address())
                .await?;
            let message = challenge["message"].as_str().context("message")?;
            sqlx::query(
                "UPDATE treasury_challenges SET expires_at = now() - interval '1 second' \
                 WHERE nonce = $1",
            )
            .bind(challenge["nonce"].as_str())
            .execute(&fixture.pool)
            .await?;
            let signature = sign(&signer, message).await?;
            let (status, body) = fixture
                .submit(&fixture.live_key, 1, message, &signature)
                .await?;
            refused(status, &body, "treasury_challenge_expired", "message")?;

            // The message's chain is not the request's.
            let challenge = fixture
                .challenge(&fixture.live_key, 1, signer.address())
                .await?;
            let message = challenge["message"].as_str().context("message")?;
            let signature = sign(&signer, message).await?;
            let (status, body) = fixture
                .submit(&fixture.live_key, OTHER_CHAIN, message, &signature)
                .await?;
            refused(status, &body, "treasury_proof_invalid", "chain_id")?;
            // Editing the chain (or anything else) in the message breaks it.
            let edited = message.replace("Chain ID: 1", &format!("Chain ID: {OTHER_CHAIN}"));
            let signature_edited = sign(&signer, &edited).await?;
            let (status, body) = fixture
                .submit(&fixture.live_key, OTHER_CHAIN, &edited, &signature_edited)
                .await?;
            refused(status, &body, "treasury_proof_invalid", "message")?;
            // Another key's signature: not an EOA's, and no contract is deployed there.
            let stranger = sign(&PrivateKeySigner::random(), message).await?;
            let (status, body) = fixture
                .submit(&fixture.live_key, 1, message, &stranger)
                .await?;
            refused(status, &body, "treasury_not_deployed", "signature")?;
            // An ERC-6492 wrapper (a counterfactual contract's signature) is refused outright.
            let wrapped = format!(
                "{}{}",
                signature, "6492649264926492649264926492649264926492649264926492649264926492"
            );
            let (status, body) = fixture
                .submit(&fixture.live_key, 1, message, &wrapped)
                .await?;
            refused(status, &body, "treasury_proof_invalid", "signature")?;
            ensure!(
                body["error"]["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("ERC-6492")),
                "{body}"
            );
            // A challenge of the other mode is not this mode's.
            let (status, body) = fixture
                .submit(&fixture.test_key, 1, message, &signature)
                .await?;
            ensure!(status == StatusCode::BAD_REQUEST, "{body}");

            // The right proof succeeds once; the same nonce is refused afterwards.
            let (status, body) = fixture
                .submit(&fixture.live_key, 1, message, &signature)
                .await?;
            ensure!(status == StatusCode::OK, "{body}");
            let (status, body) = fixture
                .submit(&fixture.live_key, 1, message, &signature)
                .await?;
            refused(status, &body, "treasury_challenge_used", "message")?;

            // Another account cannot use this account's challenge.
            let other = fixture.other_account().await?;
            let challenge = fixture
                .challenge(&fixture.live_key, 1, signer.address())
                .await?;
            let message = challenge["message"].as_str().context("message")?;
            let signature = sign(&signer, message).await?;
            let (status, body) = fixture.submit(&other, 1, message, &signature).await?;
            refused(status, &body, "treasury_proof_invalid", "message")?;
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_sanctioned_treasury_is_refused() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let mut fixture = Fixture::new(&database.app_pool, Contracts::None).await?;
            fixture.app = app(
                &fixture.pool,
                fixture.routes.clone(),
                Arc::new(Sanctioned),
                Arc::new(NoContracts),
            )?;
            let signer = PrivateKeySigner::random();
            let (status, body) = fixture.prove(&fixture.live_key, 1, &signer).await?;
            ensure!(status == StatusCode::BAD_REQUEST, "{body}");
            ensure!(body["error"]["code"] == "treasury_sanctioned");
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM treasuries")
                .fetch_one(&fixture.pool)
                .await?;
            ensure!(count == 0);
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_live_change_waits_48_hours_then_moves_that_chains_deposit_addresses() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let fixture = Fixture::new(&database.app_pool, Contracts::None).await?;
            let first = PrivateKeySigner::random();
            let next = PrivateKeySigner::random();
            for chain_id in [1, OTHER_CHAIN] {
                let (status, body) = fixture.prove(&fixture.live_key, chain_id, &first).await?;
                ensure!(
                    status == StatusCode::OK && body["status"] == "active",
                    "{body}"
                );
            }
            let before = fixture.deposit_address(&fixture.live_key, "team-1").await?;
            let shared = before["address"]
                .as_str()
                .context("shared address")?
                .to_owned();
            let (_, old_quote) = fixture.quote(&fixture.live_key, 1).await?;

            // A later live change is pending for 48 hours.
            let (status, pending) = fixture.prove(&fixture.live_key, 1, &next).await?;
            ensure!(status == StatusCode::OK, "{pending}");
            ensure!(pending["status"] == "pending", "{pending}");
            let lock = pending["effective_at"].as_i64().context("effective_at")?
                - pending["created"].as_i64().context("created")?;
            ensure!(lock == TIME_LOCK.num_seconds(), "{lock}");
            ensure!(fixture.events("account.treasury.pending").await?.len() == 1);
            ensure!(fixture.deliveries("account.treasury.pending").await? == 1);
            // Only one change waits per chain, and the current treasury is not a change.
            let (status, body) = fixture.prove(&fixture.live_key, 1, &next).await?;
            ensure!(status == StatusCode::CONFLICT, "{body}");
            ensure!(body["error"]["code"] == "treasury_change_pending");
            let (status, body) = fixture
                .prove(&fixture.live_key, OTHER_CHAIN, &first)
                .await?;
            ensure!(body["error"]["code"] == "treasury_unchanged" && status.as_u16() == 409);

            // Until it applies, quotes and addresses keep paying the current treasury.
            let (_, quote) = fixture.quote(&fixture.live_key, 1).await?;
            ensure!(quote["treasury"] == format!("{:#x}", first.address()));
            let now = Utc::now();
            let routes = fixture.route_set()?;
            ensure!(apply_due(&fixture.pool, &routes, now + Duration::hours(47)).await? == 0);
            ensure!(fixture.deposit_address(&fixture.live_key, "team-1").await? == before);

            // After 48 hours it applies, and the chain's network of every deposit address moves.
            ensure!(
                apply_due(
                    &fixture.pool,
                    &routes,
                    now + TIME_LOCK + Duration::minutes(1)
                )
                .await?
                    == 1
            );
            let id = pending["id"].as_str().context("id")?;
            let (_, applied) = fixture.get(&format!("/v1/treasuries/{id}")).await?;
            ensure!(applied["status"] == "active", "{applied}");
            let (_, list) = fixture.get("/v1/treasuries?chain_id=1").await?;
            let statuses: Vec<&str> = list["data"]
                .as_array()
                .context("data")?
                .iter()
                .filter_map(|treasury| treasury["status"].as_str())
                .collect();
            ensure!(statuses == vec!["active", "replaced"], "{list}");
            ensure!(fixture.events("account.treasury.updated").await?.len() == 3);

            let after = fixture.deposit_address(&fixture.live_key, "team-1").await?;
            ensure!(
                after["id"] == before["id"] && after["address"].is_null(),
                "{after}"
            );
            let [ethereum, optimism] = networks(&after)? else {
                anyhow::bail!("two networks: {after}");
            };
            ensure!(ethereum["treasury"] == format!("{:#x}", next.address()));
            ensure!(ethereum["address"] != shared.as_str());
            ensure!(optimism["address"] == shared.as_str());
            // The old chain-1 forwarder is kept, watched, and pays the old treasury.
            let superseded: (String, bool) = sqlx::query_as(
                "SELECT treasury, superseded_at IS NOT NULL FROM addresses \
                 WHERE chain_id = 1 AND address = $1",
            )
            .bind(&shared)
            .fetch_one(&fixture.pool)
            .await?;
            ensure!(superseded == (format!("{:#x}", first.address()), true));
            let watched = db::list_scan_addresses(&fixture.pool, 1).await?;
            ensure!(
                watched
                    .iter()
                    .any(|watched| format!("{:#x}", watched.address) == shared)
            );

            // A quote created before keeps its address; new ones pay the new treasury.
            let (_, kept) = fixture
                .get(&format!(
                    "/v1/quotes/{}",
                    old_quote["id"].as_str().context("id")?
                ))
                .await?;
            ensure!(kept["address"] == old_quote["address"]);
            ensure!(kept["treasury"] == format!("{:#x}", first.address()));
            let (_, quote) = fixture.quote(&fixture.live_key, 1).await?;
            ensure!(quote["treasury"] == format!("{:#x}", next.address()));
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_pending_change_can_be_canceled_during_the_lock() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let fixture = Fixture::new(&database.app_pool, Contracts::None).await?;
            let first = PrivateKeySigner::random();
            let next = PrivateKeySigner::random();
            fixture.prove(&fixture.live_key, 1, &first).await?;
            let (_, pending) = fixture.prove(&fixture.live_key, 1, &next).await?;
            let id = pending["id"].as_str().context("id")?;
            let cancel = format!("/v1/treasuries/{id}/cancel");
            let (status, canceled) = fixture
                .post(&fixture.live_key, &cancel, Value::Null)
                .await?;
            ensure!(status == StatusCode::OK, "{canceled}");
            ensure!(canceled["status"] == "canceled" && canceled["canceled_at"].is_i64());
            ensure!(fixture.events("account.treasury.canceled").await?.len() == 1);
            ensure!(fixture.deliveries("account.treasury.canceled").await? == 1);
            let (status, body) = fixture
                .post(&fixture.live_key, &cancel, Value::Null)
                .await?;
            ensure!(status == StatusCode::CONFLICT, "{body}");
            ensure!(body["error"]["code"] == "treasury_unexpected_state");
            // It never applies; the current treasury stays.
            let later = Utc::now() + TIME_LOCK + Duration::hours(1);
            ensure!(apply_due(&fixture.pool, &fixture.route_set()?, later).await? == 0);
            let (_, quote) = fixture.quote(&fixture.live_key, 1).await?;
            ensure!(quote["treasury"] == format!("{:#x}", first.address()));
            // A new change can be requested after the cancellation.
            let (status, again) = fixture.prove(&fixture.live_key, 1, &next).await?;
            ensure!(
                status == StatusCode::OK && again["status"] == "pending",
                "{again}"
            );
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn test_mode_changes_apply_at_once() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let fixture = Fixture::new(&database.app_pool, Contracts::None).await?;
            let first = PrivateKeySigner::random();
            let next = PrivateKeySigner::random();
            let (_, body) = fixture.prove(&fixture.test_key, SEPOLIA, &first).await?;
            ensure!(
                body["status"] == "active" && body["livemode"] == false,
                "{body}"
            );
            let before = fixture.deposit_address(&fixture.test_key, "team-1").await?;
            let (status, body) = fixture.prove(&fixture.test_key, SEPOLIA, &next).await?;
            ensure!(
                status == StatusCode::OK && body["status"] == "active",
                "{body}"
            );
            let after = fixture.deposit_address(&fixture.test_key, "team-1").await?;
            ensure!(after["id"] == before["id"] && after["address"] != before["address"]);
            ensure!(networks(&after)?[0]["treasury"] == format!("{:#x}", next.address()));
            let events = fixture.events("account.treasury.updated").await?;
            ensure!(events.len() == 2 && events.iter().all(|(_, livemode, _)| !livemode));
            // A test key does not reach live chains.
            let (status, body) = fixture
                .post(
                    &fixture.test_key,
                    "/v1/treasuries/challenge",
                    json!({"chain_id": 1, "address": format!("{:#x}", first.address())}),
                )
                .await?;
            ensure!(status == StatusCode::BAD_REQUEST && body["error"]["param"] == "chain_id");
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn treasuries_of_another_account_or_mode_are_not_found() -> Result<()> {
    with_database(|database| {
        Box::pin(async move {
            let fixture = Fixture::new(&database.app_pool, Contracts::None).await?;
            fixture
                .prove(&fixture.live_key, 1, &PrivateKeySigner::random())
                .await?;
            let (_, pending) = fixture
                .prove(&fixture.live_key, 1, &PrivateKeySigner::random())
                .await?;
            let id = pending["id"].as_str().context("id")?;
            let other = fixture.other_account().await?;
            for key in [&other, &fixture.test_key] {
                let (status, _) = fixture
                    .request(
                        Method::GET,
                        &format!("/v1/treasuries/{id}"),
                        key,
                        Value::Null,
                    )
                    .await?;
                ensure!(status == StatusCode::NOT_FOUND);
                let (status, _) = fixture
                    .post(key, &format!("/v1/treasuries/{id}/cancel"), Value::Null)
                    .await?;
                ensure!(status == StatusCode::NOT_FOUND);
                let (_, list) = fixture
                    .request(Method::GET, "/v1/treasuries", key, Value::Null)
                    .await?;
                ensure!(list["data"] == json!([]), "{list}");
            }
            let (_, list) = fixture.get("/v1/treasuries?status=pending").await?;
            ensure!(list["data"].as_array().map(Vec::len) == Some(1), "{list}");
            Ok(())
        })
    })
    .await
}

#[tokio::test]
async fn a_deployed_contract_proves_with_eip1271_at_finalized_and_an_undeployed_one_is_refused()
-> Result<()> {
    let Some(anvil) = Anvil::start_if_available(&[]).await? else {
        return Ok(());
    };
    let Some(database) = TestDatabase::create().await? else {
        return Ok(());
    };
    let anvil = &anvil;
    let result = async {
        let client = Arc::new(EvmClient::new(&anvil.rpc_url)?);
        let contracts =
            EvmContractSignatures::new(BTreeMap::from([(CHAIN_ID, [Arc::clone(&client), client])]));
        let fixture =
            Fixture::new(&database.app_pool, Contracts::Anvil(Arc::new(contracts))).await?;
        let owner = PrivateKeySigner::from_str(ANVIL_PRIVATE_KEY)?;
        let deploy = || {
            let owner = format!("{:#x}", owner.address());
            forge_create(
                &anvil.rpc_url,
                "test/mocks/MockEip1271Wallet.sol:MockEip1271Wallet",
                &[&owner],
            )
        };
        let signed = deploy()?;
        let approved = deploy()?;
        let refusing = deploy()?;
        // 64 blocks bring them to Anvil's `finalized`.
        anvil.mine(70)?;

        // An owner's signature of the message's EIP-191 hash (a Safe message).
        let challenge = fixture
            .challenge(&fixture.test_key, CHAIN_ID, signed)
            .await?;
        let message = challenge["message"].as_str().context("message")?;
        let hash = eip191_hash_message(message.as_bytes());
        let signature = format!(
            "0x{}",
            hex::encode(owner.sign_hash(&hash).await?.as_bytes())
        );
        let (status, body) = fixture
            .submit(&fixture.test_key, CHAIN_ID, message, &signature)
            .await?;
        ensure!(status == StatusCode::OK, "{body}");
        ensure!(body["kind"] == "contract" && body["address"] == format!("{signed:#x}"));

        // An on-chain approval with an empty signature (Safe's SignMessageLib).
        let challenge = fixture
            .challenge(&fixture.test_key, CHAIN_ID, approved)
            .await?;
        let message = challenge["message"].as_str().context("message")?;
        let hash = eip191_hash_message(message.as_bytes());
        approve(anvil, approved, hash)?;
        anvil.mine(70)?;
        // Deployed above the finalized block: not yet a treasury.
        let unfinalized = deploy()?;
        let (status, body) = fixture
            .submit(&fixture.test_key, CHAIN_ID, message, "0x")
            .await?;
        ensure!(
            status == StatusCode::OK && body["kind"] == "contract",
            "{body}"
        );

        // A contract that does not accept the signature.
        let challenge = fixture
            .challenge(&fixture.test_key, CHAIN_ID, refusing)
            .await?;
        let message = challenge["message"].as_str().context("message")?;
        let stranger = PrivateKeySigner::random();
        let hash = eip191_hash_message(message.as_bytes());
        let signature = format!(
            "0x{}",
            hex::encode(stranger.sign_hash(&hash).await?.as_bytes())
        );
        let (status, body) = fixture
            .submit(&fixture.test_key, CHAIN_ID, message, &signature)
            .await?;
        ensure!(status == StatusCode::BAD_REQUEST, "{body}");
        ensure!(body["error"]["code"] == "treasury_proof_invalid");

        // A Safe not deployed yet (its owner's signature), or deployed above `finalized`.
        for undeployed in [Address::repeat_byte(0x5a), unfinalized] {
            let challenge = fixture
                .challenge(&fixture.test_key, CHAIN_ID, undeployed)
                .await?;
            let message = challenge["message"].as_str().context("message")?;
            let hash = eip191_hash_message(message.as_bytes());
            let signature = format!(
                "0x{}",
                hex::encode(owner.sign_hash(&hash).await?.as_bytes())
            );
            let (status, body) = fixture
                .submit(&fixture.test_key, CHAIN_ID, message, &signature)
                .await?;
            ensure!(status == StatusCode::BAD_REQUEST, "{body}");
            ensure!(body["error"]["code"] == "treasury_not_deployed", "{body}");
        }
        Ok(())
    }
    .await;
    let cleanup = database.cleanup().await;
    result.and(cleanup)
}

fn approve(anvil: &Anvil, wallet: Address, hash: B256) -> Result<()> {
    run_checked(
        "cast",
        &[
            "send",
            "--rpc-url",
            &anvil.rpc_url,
            "--private-key",
            ANVIL_PRIVATE_KEY,
            &format!("{wallet:#x}"),
            "signMessage(bytes32)",
            &format!("{hash:#x}"),
        ],
        None,
    )?;
    Ok(())
}

async fn sign(signer: &PrivateKeySigner, message: &str) -> Result<String> {
    let signature = signer.sign_message(message.as_bytes()).await?;
    Ok(format!("0x{}", hex::encode(signature.as_bytes())))
}

enum Contracts {
    /// No contract is deployed anywhere.
    None,
    /// Anvil's chain, with its test route.
    Anvil(Arc<dyn ContractSignatures>),
}

struct Fixture {
    app: Router,
    pool: sqlx::PgPool,
    routes: Vec<RouteFile>,
    account: db::Account,
    live_key: String,
    test_key: String,
}

impl Fixture {
    async fn new(pool: &sqlx::PgPool, contracts: Contracts) -> Result<Self> {
        let account = seed::create_account(
            pool,
            &NewAccount {
                webhook_url: "https://merchant.test/webhooks".to_owned(),
                ..NewAccount::named("merchant")
            },
        )
        .await?;
        // Two endpoints, live and test, that subscribe to deposits only: account security events
        // reach them anyway.
        sqlx::query(
            "INSERT INTO webhook_endpoints (id, account_id, livemode, url) \
             VALUES (gen_random_uuid(), $1, false, 'https://merchant.test/test-webhooks')",
        )
        .bind(account.id)
        .execute(pool)
        .await?;
        sqlx::query(
            "UPDATE webhook_endpoints SET enabled_events = ARRAY['deposit.credited'] \
             WHERE account_id = $1",
        )
        .bind(account.id)
        .execute(pool)
        .await?;
        let live_key = seed::create_api_key(pool, account.id, true).await?;
        let test_key = seed::create_api_key(pool, account.id, false).await?;
        let fixture_route = include_str!("fixtures/phala-cloud-pha.yaml");
        let route = |name: &str, chain_id: u64, livemode: bool| -> Result<RouteFile> {
            Ok(serde_saphyr::from_str(
                &fixture_route
                    .replace(
                        "route: phala-cloud-ethereum-pha-usd",
                        &format!("route: {name}"),
                    )
                    .replace("chain_id: 1", &format!("chain_id: {chain_id}"))
                    .replace("livemode: true", &format!("livemode: {livemode}")),
            )?)
        };
        let mut routes = vec![
            serde_saphyr::from_str(fixture_route)?,
            route("phala-cloud-optimism-pha-usd", OTHER_CHAIN, true)?,
            route("phala-cloud-sepolia-pha", SEPOLIA, false)?,
        ];
        let contracts: Arc<dyn ContractSignatures> = match contracts {
            Contracts::None => Arc::new(NoContracts),
            Contracts::Anvil(contracts) => {
                routes.push(route("anvil-pha", CHAIN_ID, false)?);
                contracts
            }
        };
        // Whole tokens at 1 USD, so a quote of 100 cents is 1 token.
        for route in &mut routes {
            route.asset.decimals = 0;
            route.rate_lock.amount_decimals = 0;
            route.destination.unit_decimals = 0;
            route.screening.min_deposit_atomic = AtomicAmount::new(U256::from(1_u64));
            route.screening.max_deposit_atomic = AtomicAmount::new(U256::from(1_000_000_u64));
            route.screening.min_credit_minor = 1;
        }
        Ok(Self {
            app: app(pool, routes.clone(), Arc::new(Clear), contracts)?,
            pool: pool.clone(),
            routes,
            account,
            live_key,
            test_key,
        })
    }

    fn route_set(&self) -> Result<RouteSet> {
        RouteSet::new(self.routes.clone()).map_err(anyhow::Error::msg)
    }

    /// A key of another live account.
    async fn other_account(&self) -> Result<String> {
        let other = seed::create_account(&self.pool, &NewAccount::named("other")).await?;
        Ok(seed::create_api_key(&self.pool, other.id, true).await?)
    }

    async fn request(
        &self,
        method: Method,
        path: &str,
        key: &str,
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
            .oneshot(merchant_request(method, path, body, key))
            .await?;
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1_048_576).await?;
        Ok((status, serde_json::from_slice(&bytes)?))
    }

    async fn post(&self, key: &str, path: &str, body: Value) -> Result<(StatusCode, Value)> {
        self.request(Method::POST, path, key, body).await
    }

    async fn get(&self, path: &str) -> Result<(StatusCode, Value)> {
        self.request(Method::GET, path, &self.live_key, Value::Null)
            .await
    }

    async fn challenge(&self, key: &str, chain_id: u64, address: Address) -> Result<Value> {
        let (status, body) = self
            .post(
                key,
                "/v1/treasuries/challenge",
                json!({"chain_id": chain_id, "address": format!("{address:#x}")}),
            )
            .await?;
        ensure!(status == StatusCode::OK, "{status}: {body}");
        ensure!(body["object"] == "treasury_challenge");
        Ok(body)
    }

    async fn submit(
        &self,
        key: &str,
        chain_id: u64,
        message: &str,
        signature: &str,
    ) -> Result<(StatusCode, Value)> {
        self.post(
            key,
            "/v1/treasuries",
            json!({"chain_id": chain_id, "message": message, "signature": signature}),
        )
        .await
    }

    /// Proves `signer` as the treasury of `chain_id` in `key`'s mode.
    async fn prove(
        &self,
        key: &str,
        chain_id: u64,
        signer: &PrivateKeySigner,
    ) -> Result<(StatusCode, Value)> {
        let challenge = self.challenge(key, chain_id, signer.address()).await?;
        let message = challenge["message"].as_str().context("message")?;
        let signature = sign(signer, message).await?;
        self.submit(key, chain_id, message, &signature).await
    }

    async fn quote(&self, key: &str, chain_id: u64) -> Result<(StatusCode, Value)> {
        self.post(
            key,
            "/v1/quotes",
            json!({"account_id": "team-1", "amount": 100, "currency": "usd",
                   "chain_id": chain_id, "asset": "pha"}),
        )
        .await
    }

    async fn deposit_address(&self, key: &str, customer: &str) -> Result<Value> {
        let (status, body) = self
            .post(
                key,
                "/v1/deposit_addresses",
                json!({"client_reference_id": customer}),
            )
            .await?;
        ensure!(status == StatusCode::OK, "{status}: {body}");
        Ok(body)
    }

    /// The account, mode, and object type of each recorded event of `event_type`.
    async fn events(&self, event_type: &str) -> Result<Vec<(uuid::Uuid, bool, String)>> {
        Ok(sqlx::query_as(
            "SELECT account_id, livemode, object_type FROM events WHERE type = $1 ORDER BY created",
        )
        .bind(event_type)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Deliveries of `event_type` events, each to the endpoint of its own mode.
    async fn deliveries(&self, event_type: &str) -> Result<i64> {
        Ok(sqlx::query_scalar(
            "SELECT count(*) FROM webhook_deliveries AS delivery \
             JOIN events AS event ON event.id = delivery.event_id \
             JOIN webhook_endpoints AS endpoint ON endpoint.id = delivery.endpoint_id \
             WHERE event.type = $1 AND endpoint.livemode = event.livemode",
        )
        .bind(event_type)
        .fetch_one(&self.pool)
        .await?)
    }
}

fn app(
    pool: &sqlx::PgPool,
    routes: Vec<RouteFile>,
    screening: Arc<dyn DestinationScreener>,
    contract_signatures: Arc<dyn ContractSignatures>,
) -> Result<Router> {
    let admin_key = SigningKey::from_bytes(&[53; 32]);
    Ok(topup::api::router(AppState {
        pool: pool.clone(),
        routes: Arc::new(RouteSet::new(routes).map_err(anyhow::Error::msg)?),
        admin_key: VerificationKey::from_base64(
            "admin/v1".to_owned(),
            &public_key_base64(&admin_key),
        )
        .map_err(anyhow::Error::msg)?,
        public_origin: PublicOrigin::parse(TEST_ORIGIN)?,
        attestor: Arc::new(DstackAttestor::new()),
        rate_lock_quotes: Arc::new(FixedQuote),
        client_reads: Arc::default(),
        rate_limits: Arc::default(),
        screening,
        contract_signatures,
    })
    .0)
}

fn networks(object: &Value) -> Result<&[Value]> {
    Ok(object["networks"]
        .as_array()
        .context("networks")?
        .as_slice())
}

fn chain_ids(object: &Value) -> Vec<u64> {
    object["networks"]
        .as_array()
        .map(|networks| {
            networks
                .iter()
                .filter_map(|network| network["chain_id"].as_u64())
                .collect()
        })
        .unwrap_or_default()
}

/// A fixed price of 1 USD per token.
struct FixedQuote;

#[async_trait]
impl QuoteProvider for FixedQuote {
    async fn quote(&self, _route: &RouteFile) -> Result<ValidatedQuote, Value> {
        Ok(ValidatedQuote {
            price: ScaledPrice::new(100_000_000, PRICE_SCALE).map_err(|_| json!("price"))?,
            evidence: json!({"mode": "spot"}),
        })
    }
}

/// Screening that clears every address.
struct Clear;

#[async_trait]
impl DestinationScreener for Clear {
    async fn screen(&self, _route: &RouteFile, _address: Address) -> DestinationScreening {
        DestinationScreening::Clear
    }
}

/// Screening that lists every address.
struct Sanctioned;

#[async_trait]
impl DestinationScreener for Sanctioned {
    async fn screen(&self, _route: &RouteFile, _address: Address) -> DestinationScreening {
        DestinationScreening::Sanctioned
    }
}

/// A chain where no contract is deployed.
struct NoContracts;

#[async_trait]
impl ContractSignatures for NoContracts {
    async fn verify(&self, _: u64, _: Address, _: B256, _: Bytes) -> ContractAnswer {
        ContractAnswer::NotDeployed
    }
}

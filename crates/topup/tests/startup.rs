//! Architecture §4 startup contract checks against a real factory deployment on Anvil.

mod support;

use std::process::Command;
use std::str::FromStr;

use alloy_primitives::Address;
use anyhow::{Context, Result, ensure};
use support::chain::{Anvil, forge_create, run_checked};
use topup_core::route::RouteFile;

const ADMIN_ADDRESS: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";
const TREASURY: &str = "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc";
const OTHER_TREASURY: &str = "0x90f79bf6eb2c4f870365e785982e1f101e93b906";
const FIXTURE: &str = include_str!("fixtures/phala-cloud-pha.yaml");

#[tokio::test]
async fn run_refuses_to_start_when_the_route_treasury_differs_from_the_chain() -> Result<()> {
    let Some(anvil) = Anvil::start_if_available(&[]).await? else {
        return Ok(());
    };
    let rpc_url = anvil.rpc_url.clone();
    let factory = forge_create(
        &rpc_url,
        "src/ForwarderFactory.sol:ForwarderFactory",
        &[ADMIN_ADDRESS, TREASURY],
    )?;
    let implementation = implementation_of(&rpc_url, factory)?;

    let yaml = route_yaml(&anvil, factory, implementation, TREASURY);
    let route: RouteFile = serde_saphyr::from_str(&yaml)?;
    route.validate().map_err(anyhow::Error::msg)?;
    topup::contracts::verify_routes(&route_set(&route)?)
        .await
        .map_err(anyhow::Error::msg)
        .context("the deployed contracts must match their own route")?;

    let mut wrong_implementation = route.clone();
    wrong_implementation.chain.contracts.implementation = Address::from_str(TREASURY)?;
    let error = topup::contracts::verify_routes(&route_set(&wrong_implementation)?)
        .await
        .expect_err("a wrong implementation must fail");
    ensure!(error.contains("implementation()"), "{error}");

    // Correct getters but different runtime code: one unreachable byte appended to each contract.
    for (contract, name) in [(factory, "factory"), (implementation, "implementation")] {
        let original = cast(&["code", &format!("{contract:#x}"), "--rpc-url", &rpc_url])?;
        set_code(&rpc_url, contract, &format!("{original}00"))?;
        ensure!(
            implementation_of(&rpc_url, factory)? == implementation,
            "the getters must still answer"
        );
        let error = topup::contracts::verify_routes(&route_set(&route)?)
            .await
            .expect_err("modified code must fail");
        ensure!(
            error.contains(name) && error.contains("differs from the recorded code hash"),
            "{error}"
        );
        set_code(&rpc_url, contract, &original)?;
    }
    topup::contracts::verify_routes(&route_set(&route)?)
        .await
        .map_err(anyhow::Error::msg)
        .context("restored code must pass again")?;

    // Balance and addressOf reads are aggregated through Multicall3: without the canonical
    // deployment the service must refuse to start rather than fail every read later.
    let multicall = Address::from_str("0xcA11bde05977b3631167028862bE2a173976CA11")?;
    let canonical = cast(&["code", &format!("{multicall:#x}"), "--rpc-url", &rpc_url])?;
    for (code, expected) in [
        ("0x", "has no code on chain"),
        ("0x00", "is not the canonical deployment"),
    ] {
        set_code(&rpc_url, multicall, code)?;
        let error = topup::contracts::verify_routes(&route_set(&route)?)
            .await
            .expect_err("a missing or different Multicall3 must fail");
        ensure!(
            error.contains("Multicall3") && error.contains(expected),
            "{error}"
        );
    }
    set_code(&rpc_url, multicall, &canonical)?;
    topup::contracts::verify_routes(&route_set(&route)?)
        .await
        .map_err(anyhow::Error::msg)
        .context("the canonical Multicall3 must pass again")?;

    let path = std::env::temp_dir().join(format!("topup-startup-{}.yaml", uuid::Uuid::new_v4()));
    std::fs::write(
        &path,
        route_yaml(&anvil, factory, implementation, OTHER_TREASURY),
    )?;
    let output = Command::new(env!("CARGO_BIN_EXE_topup"))
        .args(["run", "--route"])
        .arg(&path)
        .env_clear()
        // Complete runtime configuration, with a database nobody listens on: the contract check
        // must refuse before the service connects to it.
        .env("DATABASE_URL", "postgres://topup_service@127.0.0.1:1/topup")
        .env("TOPUP_ADMIN_KID", "admin/v1")
        .env(
            "TOPUP_ADMIN_PUBLIC_KEY",
            "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=",
        )
        .env("TOPUP_PUBLIC_ORIGIN", "http://127.0.0.1:8080")
        .output();
    std::fs::remove_file(&path)?;
    let output = output.context("start topup run")?;
    let logs = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(!output.status.success(), "run must refuse to start: {logs}");
    ensure!(
        logs.contains("on-chain contract check failed") && logs.contains("treasury()"),
        "run must name the treasury mismatch: {logs}"
    );
    ensure!(
        !logs.contains("failed to connect to database"),
        "the contract check must run before the database is used: {logs}"
    );
    Ok(())
}

fn route_set(route: &RouteFile) -> Result<topup::routes::RouteSet> {
    topup::routes::RouteSet::new(vec![route.clone()]).map_err(anyhow::Error::msg)
}

fn route_yaml(anvil: &Anvil, factory: Address, implementation: Address, treasury: &str) -> String {
    // Two distinct provider entries for the same node, as route validation requires.
    let primary = anvil.rpc_url.clone();
    let secondary = primary.replace("127.0.0.1", "localhost");
    FIXTURE
        .replace(
            "rpc_providers: [alchemy, quicknode]",
            &format!("rpc_providers: [\"{primary}\", \"{secondary}\"]"),
        )
        .replace(
            "0xe8A9Ab1AbC7651A5b7C2ED5B662F2f80BF5C446d",
            &format!("{factory:#x}"),
        )
        .replace(
            "0xfeb1871c9897251C74b39DFC74e577888290faE6",
            &format!("{implementation:#x}"),
        )
        .replace("0x0000000000000000000000000000000000007EA5", treasury)
}

fn implementation_of(rpc_url: &str, factory: Address) -> Result<Address> {
    let output = cast(&[
        "call",
        &format!("{factory:#x}"),
        "implementation()(address)",
        "--rpc-url",
        rpc_url,
    ])?;
    Ok(Address::from_str(&output)?)
}

fn set_code(rpc_url: &str, contract: Address, code: &str) -> Result<()> {
    cast(&[
        "rpc",
        "--rpc-url",
        rpc_url,
        "anvil_setCode",
        &format!("{contract:#x}"),
        code,
    ])
    .map(drop)
}

fn cast(arguments: &[&str]) -> Result<String> {
    let output = run_checked("cast", arguments, None)?;
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

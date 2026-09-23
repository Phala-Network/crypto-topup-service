//! Architecture §4 startup contract checks against a real factory deployment on Anvil.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::time::Duration;

use alloy_primitives::Address;
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use topup_core::route::RouteFile;

const ADMIN_ADDRESS: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";
const ADMIN_KEY: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const TREASURY: &str = "0x3c44cdddb6a900fa2b585dd299e03d12fa4293bc";
const OTHER_TREASURY: &str = "0x90f79bf6eb2c4f870365e785982e1f101e93b906";
const FIXTURE: &str = include_str!("fixtures/phala-cloud-pha.yaml");

struct Anvil {
    child: Child,
    port: u16,
}

impl Anvil {
    fn start() -> Result<Option<Self>> {
        if !command_available("anvil") {
            eprintln!("skipping startup contract test: anvil is not on PATH");
            return Ok(None);
        }
        ensure!(
            command_available("forge"),
            "forge is required when anvil is available"
        );
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let child = Command::new("anvil")
            .args(["--silent", "--port", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start anvil")?;
        let anvil = Self { child, port };
        for _ in 0..100 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return Ok(Some(anvil));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        bail!("anvil did not start")
    }

    /// Two distinct provider entries for the same node, as route validation requires.
    fn providers(&self) -> [String; 2] {
        [
            format!("http://127.0.0.1:{}", self.port),
            format!("http://localhost:{}", self.port),
        ]
    }
}

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
async fn run_refuses_to_start_when_the_route_treasury_differs_from_the_chain() -> Result<()> {
    let Some(anvil) = Anvil::start()? else {
        return Ok(());
    };
    let rpc_url = anvil.providers()[0].clone();
    let factory = deploy_factory(&rpc_url)?;
    let implementation = implementation_of(&rpc_url, factory)?;

    let yaml = route_yaml(&anvil, factory, implementation, TREASURY);
    let route: RouteFile = serde_saphyr::from_str(&yaml)?;
    route.validate().map_err(anyhow::Error::msg)?;
    topup::contracts::verify_routes(std::slice::from_ref(&route))
        .await
        .map_err(anyhow::Error::msg)
        .context("the deployed contracts must match their own route")?;

    let mut wrong_implementation = route.clone();
    wrong_implementation.chain.contracts.implementation = Address::from_str(TREASURY)?;
    let error = topup::contracts::verify_routes(&[wrong_implementation])
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
        let error = topup::contracts::verify_routes(std::slice::from_ref(&route))
            .await
            .expect_err("modified code must fail");
        ensure!(
            error.contains(name) && error.contains("differs from the recorded code hash"),
            "{error}"
        );
        set_code(&rpc_url, contract, &original)?;
    }
    topup::contracts::verify_routes(std::slice::from_ref(&route))
        .await
        .map_err(anyhow::Error::msg)
        .context("restored code must pass again")?;

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

fn route_yaml(anvil: &Anvil, factory: Address, implementation: Address, treasury: &str) -> String {
    let [primary, secondary] = anvil.providers();
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

fn deploy_factory(rpc_url: &str) -> Result<Address> {
    let output = Command::new("forge")
        .current_dir(repository_root())
        .args([
            "create",
            "--root",
            "contracts",
            "--rpc-url",
            rpc_url,
            "--private-key",
            ADMIN_KEY,
            "--broadcast",
            "--json",
            "src/ForwarderFactory.sol:ForwarderFactory",
            "--constructor-args",
            ADMIN_ADDRESS,
            TREASURY,
        ])
        .output()?;
    ensure!(
        output.status.success(),
        "forge create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&output.stdout)?;
    Ok(Address::from_str(
        value["deployedTo"]
            .as_str()
            .context("forge output omitted deployedTo")?,
    )?)
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
    let output = Command::new("cast").args(arguments).output()?;
    ensure!(
        output.status.success(),
        "cast {} failed: {}",
        arguments.first().copied().unwrap_or_default(),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn command_available(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

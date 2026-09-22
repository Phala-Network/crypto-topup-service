//! Disposable-chain fixtures shared by `prepare`, `run`, and the reference endpoint.
//!
//! `prepare` deploys (or adopts pre-deployed) contracts on a development chain such as Anvil and
//! writes a [`Manifest`]. The product is configured from that manifest, then `run` reads it and
//! emits real Transfer logs for every settlement request, including on-chain counter-examples.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::time::Duration;

use alloy_primitives::{Address, B256, Bytes, U256, keccak256};
use alloy_sol_types::{SolCall, sol};
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use topup_core::address::{forwarder_address, persistent_salt};

/// Current manifest schema version.
pub const MANIFEST_VERSION: u32 = 1;
/// Product slug used for conformance forwarder salts.
pub const PRODUCT_SLUG: &str = "conformance";
/// Route name carried in settlement evidence.
pub const ROUTE: &str = "conformance";
/// Route version carried in settlement evidence.
pub const ROUTE_VERSION: u64 = 1;
/// Persistent address version used for every conformance account.
pub const ADDRESS_VERSION: u64 = 1;
/// Blocks mined after a finalized fixture transfer; covers Anvil's default 32-slot epochs.
const FINALITY_BLOCKS: u64 = 65;
const RPC_TIMEOUT: Duration = Duration::from_secs(10);

sol! {
    function mint(address account, uint256 amount);
    function implementation() external view returns (address);
}

/// Chain fixture shared by the suite and the product configuration.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Manifest {
    /// Manifest schema version.
    pub version: u32,
    /// EVM chain id of the development chain.
    pub chain_id: u64,
    /// JSON-RPC URL the suite uses; the product should use its own connection to the same chain.
    pub rpc_url: String,
    /// Product slug used in forwarder salts.
    pub product_slug: String,
    /// Route name expected in evidence.
    pub route: String,
    /// Route version expected in evidence.
    pub route_version: u64,
    /// Persistent address version used in forwarder salts.
    pub address_version: u64,
    /// A1 forwarder factory.
    pub factory: Address,
    /// Forwarder implementation used by the factory.
    pub implementation: Address,
    /// The only token the product may approve for the route.
    pub asset_contract: Address,
    /// A second token used for wrong-emitter counter-examples; never approved.
    pub unapproved_asset_contract: Address,
    /// Unlocked development account which mints fixture transfers.
    pub funder: Address,
}

impl Manifest {
    /// Reads and validates a manifest written by `prepare`.
    pub fn read(path: &Path) -> Result<Self> {
        let bytes =
            std::fs::read(path).with_context(|| format!("read manifest {}", path.display()))?;
        let manifest: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse manifest {}", path.display()))?;
        ensure!(
            manifest.version == MANIFEST_VERSION,
            "manifest version {} is not supported",
            manifest.version
        );
        ensure!(
            manifest.asset_contract != manifest.unapproved_asset_contract,
            "approved and unapproved assets must differ"
        );
        Ok(manifest)
    }

    /// Writes the manifest as pretty JSON.
    pub fn write(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("write manifest {}", path.display()))
    }

    /// Forwarder address a conforming product computes for an account.
    #[must_use]
    pub fn forwarder(&self, account_id: &str) -> Address {
        forwarder_address(
            self.factory,
            self.implementation,
            persistent_salt(&self.product_slug, account_id, self.address_version),
        )
    }
}

/// Minimal JSON-RPC client.
#[derive(Clone)]
pub struct Rpc {
    url: String,
    client: reqwest::Client,
}

impl Rpc {
    /// Creates a client for one endpoint.
    pub fn new(url: &str) -> Result<Self> {
        Ok(Self {
            url: url.to_owned(),
            client: reqwest::Client::builder().timeout(RPC_TIMEOUT).build()?,
        })
    }

    /// Calls one method and returns its `result`, which may be JSON `null`.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value> {
        let response: Value = self
            .client
            .post(&self.url)
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .send()
            .await
            .with_context(|| format!("{method} request failed"))?
            .error_for_status()?
            .json()
            .await
            .with_context(|| format!("{method} returned invalid JSON"))?;
        if let Some(error) = response.get("error") {
            bail!("{method} returned an error: {error}");
        }
        Ok(response.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Returns `eth_chainId`.
    pub async fn chain_id(&self) -> Result<u64> {
        parse_quantity(&self.call("eth_chainId", json!([])).await?)
    }

    /// Returns the receipt of a transaction, if it has been mined.
    pub async fn receipt(&self, tx_hash: B256) -> Result<Option<Value>> {
        let receipt = self
            .call("eth_getTransactionReceipt", json!([tx_hash]))
            .await?;
        Ok((!receipt.is_null()).then_some(receipt))
    }

    /// Returns the number of the chain's `finalized` block.
    pub async fn finalized_block(&self) -> Result<u64> {
        let block = self
            .call("eth_getBlockByNumber", json!(["finalized", false]))
            .await?;
        parse_quantity(
            block
                .get("number")
                .context("finalized block is unavailable")?,
        )
    }

    async fn has_code(&self, address: Address) -> Result<bool> {
        let code = self.call("eth_getCode", json!([address, "latest"])).await?;
        Ok(code.as_str().is_some_and(|code| code.len() > 2))
    }
}

/// Parses a `0x`-prefixed JSON-RPC quantity.
pub fn parse_quantity(value: &Value) -> Result<u64> {
    let text = value.as_str().context("quantity must be a string")?;
    u64::from_str_radix(text.strip_prefix("0x").context("quantity needs 0x")?, 16)
        .context("quantity is not a u64")
}

/// Pre-deployed contracts supplied to `prepare`; missing ones are deployed.
#[derive(Clone, Debug, Default)]
pub struct PrepareOptions {
    /// Expected chain id; checked against `eth_chainId` when present.
    pub chain_id: Option<u64>,
    /// Existing A1 forwarder factory.
    pub factory: Option<Address>,
    /// Existing approved token exposing `mint(address,uint256)`.
    pub asset_contract: Option<Address>,
    /// Existing second token exposing `mint(address,uint256)`.
    pub unapproved_asset_contract: Option<Address>,
    /// Foundry project containing the A1 contracts and mocks.
    pub contracts_dir: PathBuf,
}

/// Deploys missing fixtures on a development chain and returns the manifest.
pub async fn prepare(rpc_url: &str, options: PrepareOptions) -> Result<Manifest> {
    let rpc = Rpc::new(rpc_url)?;
    let chain_id = rpc.chain_id().await?;
    if let Some(expected) = options.chain_id {
        ensure!(
            chain_id == expected,
            "chain id is {chain_id}, expected {expected}"
        );
    }
    let accounts = rpc.call("eth_accounts", json!([])).await?;
    let funder = accounts
        .as_array()
        .and_then(|accounts| accounts.first())
        .and_then(Value::as_str)
        .context("the development chain exposes no unlocked account")?;
    let funder = Address::from_str(funder)?;

    let factory = match options.factory {
        Some(factory) => factory,
        None => {
            deploy(
                &options.contracts_dir,
                rpc_url,
                funder,
                "src/ForwarderFactory.sol:ForwarderFactory",
                vec![format!("{funder:#x}"), format!("{funder:#x}")],
            )
            .await?
        }
    };
    let mut tokens = Vec::with_capacity(2);
    for supplied in [options.asset_contract, options.unapproved_asset_contract] {
        tokens.push(match supplied {
            Some(token) => token,
            None => {
                deploy(
                    &options.contracts_dir,
                    rpc_url,
                    funder,
                    "test/mocks/MockTokens.sol:MockERC20",
                    Vec::new(),
                )
                .await?
            }
        });
    }
    let (asset_contract, unapproved_asset_contract) = match tokens.as_slice() {
        [approved, unapproved] => (*approved, *unapproved),
        _ => bail!("expected two fixture tokens"),
    };
    ensure!(
        asset_contract != unapproved_asset_contract,
        "approved and unapproved assets must differ"
    );
    for (name, address) in [
        ("factory", factory),
        ("asset contract", asset_contract),
        ("unapproved asset contract", unapproved_asset_contract),
    ] {
        ensure!(
            rpc.has_code(address).await?,
            "{name} {address:#x} has no code"
        );
    }
    let data: Bytes = implementationCall {}.abi_encode().into();
    let output = rpc
        .call("eth_call", json!([{"to": factory, "data": data}, "latest"]))
        .await?;
    let output = Bytes::from_str(output.as_str().context("eth_call returned no data")?)?;
    let implementation = implementationCall::abi_decode_returns(&output)
        .context("factory does not expose implementation()")?;

    Ok(Manifest {
        version: MANIFEST_VERSION,
        chain_id,
        rpc_url: rpc_url.to_owned(),
        product_slug: PRODUCT_SLUG.to_owned(),
        route: ROUTE.to_owned(),
        route_version: ROUTE_VERSION,
        address_version: ADDRESS_VERSION,
        factory,
        implementation,
        asset_contract,
        unapproved_asset_contract,
        funder,
    })
}

async fn deploy(
    contracts_dir: &Path,
    rpc_url: &str,
    funder: Address,
    contract: &'static str,
    constructor_arguments: Vec<String>,
) -> Result<Address> {
    let contracts_dir = contracts_dir.to_owned();
    let rpc_url = rpc_url.to_owned();
    tokio::task::spawn_blocking(move || {
        let funder = format!("{funder:#x}");
        let mut command = Command::new("forge");
        command.current_dir(&contracts_dir).args([
            "create",
            "--rpc-url",
            &rpc_url,
            "--unlocked",
            "--from",
            &funder,
            "--broadcast",
            "--json",
            contract,
        ]);
        if !constructor_arguments.is_empty() {
            command
                .arg("--constructor-args")
                .args(&constructor_arguments);
        }
        let output = command
            .output()
            .context("run forge create; Foundry must be on PATH")?;
        ensure!(
            output.status.success(),
            "forge create {contract} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: Value =
            serde_json::from_slice(&output.stdout).context("parse forge create output")?;
        Address::from_str(
            result
                .get("deployedTo")
                .and_then(Value::as_str)
                .context("forge create omitted deployedTo")?,
        )
        .context("parse deployed address")
    })
    .await?
}

/// A Transfer log emitted by a fixture mint.
#[derive(Clone, Copy, Debug)]
pub struct MintedLog {
    /// Transaction containing the log.
    pub tx_hash: B256,
    /// Receipt-global index of the Transfer log.
    pub log_index: u64,
}

/// Emits real fixture transfers described by a manifest.
#[derive(Clone)]
pub struct ChainFixture {
    rpc: Rpc,
    manifest: Manifest,
}

impl ChainFixture {
    /// Connects to the manifest chain and checks its chain id.
    pub async fn connect(manifest: Manifest) -> Result<Self> {
        let rpc = Rpc::new(&manifest.rpc_url)?;
        let chain_id = rpc.chain_id().await?;
        ensure!(
            chain_id == manifest.chain_id,
            "chain id is {chain_id}, manifest says {}",
            manifest.chain_id
        );
        Ok(Self { rpc, manifest })
    }

    /// Manifest this fixture was created from.
    #[must_use]
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Mints `amount` of `token` to `to`; when `finalize`, mines enough blocks for finality.
    pub async fn mint(
        &self,
        token: Address,
        to: Address,
        amount: u64,
        finalize: bool,
    ) -> Result<MintedLog> {
        let data: Bytes = mintCall {
            account: to,
            amount: U256::from(amount),
        }
        .abi_encode()
        .into();
        let tx_hash = self
            .rpc
            .call(
                "eth_sendTransaction",
                json!([{"from": self.manifest.funder, "to": token, "data": data}]),
            )
            .await?;
        let tx_hash = B256::from_str(tx_hash.as_str().context("no transaction hash")?)?;
        let mut receipt = None;
        for _ in 0..50 {
            receipt = self.rpc.receipt(tx_hash).await?;
            if receipt.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let receipt = receipt.context("fixture transfer was not mined")?;
        ensure!(
            receipt.get("status").and_then(Value::as_str) == Some("0x1"),
            "fixture transfer reverted"
        );
        let transfer_topic = format!("{:#x}", keccak256("Transfer(address,address,uint256)"));
        let log_index = receipt
            .get("logs")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|log| {
                log.pointer("/topics/0").and_then(Value::as_str) == Some(transfer_topic.as_str())
            })
            .and_then(|log| log.get("logIndex"))
            .context("fixture transfer emitted no Transfer log")?;
        let log_index = parse_quantity(log_index)?;
        if finalize {
            self.finalize().await?;
        }
        Ok(MintedLog { tx_hash, log_index })
    }

    /// Mines enough blocks for every earlier block to be finalized.
    pub async fn finalize(&self) -> Result<()> {
        self.rpc
            .call("anvil_mine", json!([format!("{FINALITY_BLOCKS:#x}")]))
            .await
            .context("mine finality blocks; the chain must support anvil_mine")?;
        Ok(())
    }
}

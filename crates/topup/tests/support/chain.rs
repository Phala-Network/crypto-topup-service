//! Disposable Anvil chains and Foundry deployments for integration tests.

use std::collections::VecDeque;
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::str::FromStr;
use std::sync::Mutex;
use std::time::Duration;

use alloy_primitives::Address;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

/// Anvil's first default account, which deploys every test contract.
pub const ANVIL_PRIVATE_KEY: &str =
    "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
pub const CHAIN_ID: u64 = 31_337;

/// Generous bound on anvil start-up under CPU contention; a healthy start takes well under 1 s.
const ANVIL_START: Duration = Duration::from_secs(60);

/// Serializes `forge create` so parallel tests do not race on the Foundry build cache.
static FORGE: Mutex<()> = Mutex::new(());

pub struct Anvil {
    child: Child,
    pub rpc_url: String,
}

impl Anvil {
    /// Starts anvil, or returns `None` after [`super::skip`] when Foundry is not on `PATH`.
    pub async fn start_if_available(extra_args: &[&str]) -> Result<Option<Self>> {
        if !command_available("anvil") {
            super::skip("anvil is not on PATH")?;
            return Ok(None);
        }
        ensure!(
            command_available("forge"),
            "forge is required when anvil is available"
        );
        ensure!(
            command_available("cast"),
            "cast is required when anvil is available"
        );
        Self::start(extra_args).await.map(Some)
    }

    /// Starts anvil on a port it picks itself (no free-port race with parallel tests) and
    /// waits until it answers JSON-RPC, so a loaded host only slows the start down.
    pub async fn start(extra_args: &[&str]) -> Result<Self> {
        let mut child = Command::new("anvil")
            .args(["--port", "0", "--chain-id", &CHAIN_ID.to_string()])
            .args(extra_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("start anvil")?;
        let stdout = child.stdout.take().context("anvil stdout")?;
        let stderr = child.stderr.take().context("anvil stderr")?;
        let mut anvil = Self {
            child,
            rpc_url: String::new(),
        };
        // Both readers keep draining after start-up so anvil never blocks on a full pipe; the
        // stderr reader returns its last lines at EOF for the start-up failure message.
        let stderr_tail = std::thread::spawn(move || {
            let mut tail = VecDeque::new();
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if tail.len() == 20 {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
            Vec::from(tail).join("\n")
        });
        let (address_tx, address_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(address) = line.strip_prefix("Listening on ") {
                    let _ = address_tx.send(address.trim().to_owned());
                }
            }
        });
        let address = tokio::task::spawn_blocking(move || address_rx.recv_timeout(ANVIL_START))
            .await?
            .context("anvil did not report its listening address");
        let error = match address {
            Ok(address) => {
                anvil.rpc_url = format!("http://{address}");
                match anvil
                    .wait_for_rpc()
                    .await
                    .and_then(|()| anvil.install_multicall3())
                {
                    Ok(()) => return Ok(anvil),
                    Err(error) => error,
                }
            }
            Err(error) => error,
        };
        // Stopping anvil closes stderr, so the reader finishes with the complete tail.
        drop(anvil);
        let tail = stderr_tail.join().unwrap_or_default();
        Err(error.context(format!("anvil stderr tail:\n{tail}")))
    }

    async fn wait_for_rpc(&self) -> Result<()> {
        let client = reqwest::Client::new();
        let deadline = tokio::time::Instant::now() + ANVIL_START;
        loop {
            match chain_id(&client, &self.rpc_url).await {
                Ok(()) => return Ok(()),
                Err(error) if tokio::time::Instant::now() >= deadline => {
                    return Err(error.context("anvil did not answer eth_chainId"));
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    }

    pub fn mine(&self, count: u64) -> Result<()> {
        run_checked(
            "cast",
            &[
                "rpc",
                "--rpc-url",
                &self.rpc_url,
                "anvil_mine",
                &format!("0x{count:x}"),
            ],
            None,
        )?;
        Ok(())
    }

    pub fn block_number(&self) -> Result<u64> {
        let output = run_checked("cast", &["block-number", "--rpc-url", &self.rpc_url], None)?;
        Ok(String::from_utf8(output.stdout)?.trim().parse()?)
    }

    pub fn reset(&self) -> Result<()> {
        run_checked(
            "cast",
            &["rpc", "--rpc-url", &self.rpc_url, "anvil_reset"],
            None,
        )?;
        self.install_multicall3()
    }

    /// Installs the canonical Multicall3, which every real chain carries and Anvil lacks; the
    /// service aggregates its balance and `addressOf` reads through it.
    pub fn install_multicall3(&self) -> Result<()> {
        let recorded: Value = serde_json::from_str(&std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../deploy/contracts/multicall3.json"),
        )?)?;
        let field = |name: &str| {
            recorded[name]
                .as_str()
                .with_context(|| format!("multicall3.json lacks {name}"))
        };
        run_checked(
            "cast",
            &[
                "rpc",
                "--rpc-url",
                &self.rpc_url,
                "anvil_setCode",
                field("address")?,
                field("runtime_code")?,
            ],
            None,
        )?;
        Ok(())
    }
}

impl Drop for Anvil {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn chain_id(client: &reqwest::Client, rpc_url: &str) -> Result<()> {
    let response: Value = client
        .post(rpc_url)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "eth_chainId", "params": []}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    ensure!(
        response.get("result").is_some_and(Value::is_string),
        "unexpected eth_chainId response: {response}"
    );
    Ok(())
}

pub fn contracts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../contracts")
}

/// Deploys `contract` (a `path:Name` under `contracts/`) with the Anvil deployer key.
pub fn forge_create(rpc_url: &str, contract: &str, constructor_args: &[&str]) -> Result<Address> {
    let mut arguments = vec![
        "create",
        "--rpc-url",
        rpc_url,
        "--private-key",
        ANVIL_PRIVATE_KEY,
        "--broadcast",
        "--json",
        contract,
    ];
    if !constructor_args.is_empty() {
        arguments.push("--constructor-args");
        arguments.extend_from_slice(constructor_args);
    }
    let output = {
        let _guard = FORGE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        run_checked("forge", &arguments, Some(&contracts_dir()))?
    };
    let result: Value = serde_json::from_slice(&output.stdout)?;
    let address = result
        .get("deployedTo")
        .and_then(Value::as_str)
        .context("forge create omitted deployedTo")?;
    Address::from_str(address).context("forge returned an invalid deployment address")
}

pub fn command_available(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

pub fn run_checked(command: &str, arguments: &[&str], directory: Option<&Path>) -> Result<Output> {
    let mut invocation = Command::new(command);
    invocation.args(arguments);
    if let Some(directory) = directory {
        invocation.current_dir(directory);
    }
    let output = invocation
        .output()
        .with_context(|| format!("run {command}"))?;
    ensure!(
        output.status.success(),
        "{command} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

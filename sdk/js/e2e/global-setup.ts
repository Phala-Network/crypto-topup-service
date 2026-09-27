import { execFileSync, spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createTestClient, http, parseEther, publicActions, walletActions, type Hex } from "viem";
import { sepolia } from "viem/chains";

/**
 * Starts Anvil as Sepolia (so the wallet is asked to add a chain viem knows) and deploys the test
 * token from Anvil's first unlocked dev account, the payer, which holds its supply. Tests read
 * `ANVIL_URL`, `PAYER_ADDRESS`, and `TOKEN_ADDRESS`.
 */
export default async function globalSetup(): Promise<() => Promise<void>> {
  const work = mkdtempSync(join(tmpdir(), "phala-pay-e2e-"));
  execFileSync(
    process.env["FORGE"] ?? "forge",
    [
      ...["build", "e2e/TestToken.sol", "--root", ".", "--contracts", "e2e", "--no-lint"],
      ...["--out", join(work, "out"), "--cache-path", join(work, "cache")],
    ],
    { stdio: ["ignore", "ignore", "inherit"] },
  );
  const artifact = JSON.parse(
    readFileSync(join(work, "out", "TestToken.sol", "TestToken.json"), "utf8"),
  ) as { bytecode: { object: Hex } };

  const port = await freePort();
  const anvil = spawn(
    process.env["ANVIL"] ?? "anvil",
    ["--port", String(port), "--chain-id", String(sepolia.id), "--silent"],
    // Not inherited: an orphaned node would otherwise hold the runner's output pipe open.
    { stdio: "ignore" },
  );
  const teardown = async () => {
    if (anvil.exitCode === null) {
      const exited = new Promise((resolve) => anvil.once("exit", resolve));
      anvil.kill();
      await exited;
    }
    rmSync(work, { recursive: true, force: true });
  };
  try {
    const url = `http://127.0.0.1:${port}`;
    const client = createTestClient({ mode: "anvil", chain: sepolia, transport: http(url) })
      .extend(publicActions)
      .extend(walletActions);
    await waitFor(() => client.getChainId());
    const [payer] = await client.getAddresses();
    if (payer === undefined) {
      throw new Error("anvil has no dev account");
    }
    const hash = await client.deployContract({
      account: payer,
      abi: [
        {
          type: "constructor",
          inputs: [{ name: "supply", type: "uint256" }],
          stateMutability: "nonpayable",
        },
      ],
      bytecode: artifact.bytecode.object,
      args: [parseEther("1000000")],
    });
    const { contractAddress } = await client.waitForTransactionReceipt({ hash });
    if (contractAddress == null) {
      throw new Error("token deployment failed");
    }
    process.env["ANVIL_URL"] = url;
    process.env["PAYER_ADDRESS"] = payer;
    process.env["TOKEN_ADDRESS"] = contractAddress;
  } catch (error) {
    await teardown();
    throw error;
  }
  return teardown;
}

function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() => {
        if (typeof address === "object" && address !== null) {
          resolve(address.port);
        } else {
          reject(new Error("no port"));
        }
      });
    });
  });
}

async function waitFor(probe: () => Promise<unknown>): Promise<void> {
  for (let attempt = 0; ; attempt += 1) {
    try {
      await probe();
      return;
    } catch (error) {
      if (attempt >= 50) {
        throw error;
      }
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }
}

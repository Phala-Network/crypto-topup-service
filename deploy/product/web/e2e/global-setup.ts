import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { createPrivateKey, createPublicKey, generateKeyPairSync, randomBytes } from "node:crypto";
import { mkdirSync, mkdtempSync, openSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createTestClient, http, publicActions, walletActions, type Hex } from "viem";
import { sepolia } from "viem/chains";

const web = resolve(import.meta.dirname, "..");
const root = resolve(web, "../../..");
const FACTORY = "0x2407bE5Be2b632F5b166872A49E4946a70CCa531";
const IMPLEMENTATION = "0x70B714508BFa441449DC09f790Ca03Baa5170360";
const TREASURY = "0x936c1991f8dA9a919fa11b557a3514719f5A4504";

/**
 * Runs the whole demo locally: Anvil as Sepolia with the test token, the fake top-up service
 * (e2e/fake_service.py), and the reference product serving the built page at `/demo/`, pinned to
 * the fake service's webhook key. Tests read DEMO_URL, ANVIL_URL, PAYER_ADDRESS,
 * TOKEN_ADDRESS, and TREASURY. Service logs go to test-results/services.
 */
export default async function globalSetup(): Promise<() => Promise<void>> {
  const work = mkdtempSync(join(tmpdir(), "demo-e2e-"));
  const logs = join(web, "test-results", "services");
  mkdirSync(logs, { recursive: true });
  const children: ChildProcess[] = [];
  const teardown = async () => {
    await Promise.all(children.map(stop));
    rmSync(work, { recursive: true, force: true });
  };
  try {
    execFileSync(
      process.env["FORGE"] ?? "forge",
      [
        ...["build", "e2e/TestPha.sol", "--root", ".", "--contracts", "e2e", "--no-lint"],
        ...["--out", join(work, "out"), "--cache-path", join(work, "cache")],
      ],
      { cwd: web, stdio: ["ignore", "ignore", "inherit"] },
    );
    const artifact = JSON.parse(
      readFileSync(join(work, "out", "TestPha.sol", "TestPha.json"), "utf8"),
    ) as { bytecode: { object: Hex } };

    const [anvilPort, servicePort, productPort] = [await freePort(), await freePort(), await freePort()];
    children.push(
      spawn(
        process.env["ANVIL"] ?? "anvil",
        ["--port", String(anvilPort), "--chain-id", String(sepolia.id), "--silent"],
        { stdio: "ignore" },
      ),
    );
    const anvil = `http://127.0.0.1:${anvilPort}`;
    const chain = createTestClient({ mode: "anvil", chain: sepolia, transport: http(anvil) })
      .extend(publicActions)
      .extend(walletActions);
    await waitFor(() => chain.getChainId());
    const [payer] = await chain.getAddresses();
    if (payer === undefined) {
      throw new Error("anvil has no dev account");
    }
    const hash = await chain.deployContract({
      account: payer,
      abi: [],
      bytecode: artifact.bytecode.object,
    });
    const { contractAddress: token } = await chain.waitForTransactionReceipt({ hash });
    if (token == null) {
      throw new Error("token deployment failed");
    }

    const webhookSeed = randomBytes(32);
    // The stand-in service does not check the key; the SDK needs a secret key's form.
    writeFileSync(join(work, "product.key"), `ppay_sk_test_${"A".repeat(43)}000000`, { mode: 0o600 });
    const product = `http://127.0.0.1:${productPort}`;
    const service = `http://127.0.0.1:${servicePort}`;
    const uv = ["run", "--locked", "--project", join(root, "sdk/python"), "python"];

    children.push(
      spawn(
        "uv",
        [
          ...uv,
          join(web, "e2e/fake_service.py"),
          ...["--port", String(servicePort), "--rpc", anvil, "--token", token],
          ...["--product-webhook", `${product}/webhooks`],
          ...["--webhook-seed", webhookSeed.toString("hex")],
          ...["--factory", FACTORY, "--implementation", IMPLEMENTATION],
          ...["--product", "acme", "--treasury", TREASURY],
        ],
        { stdio: ["ignore", openSync(join(logs, "fake_service.log"), "w"), "inherit"] },
      ),
    );
    const config = {
      service_url: service,
      product_slug: "acme",
      api_key_file: join(work, "product.key"),
      route: "sandbox-acme-tpha-usd",
      chain_id: sepolia.id,
      rpc_url: anvil,
      factory: FACTORY,
      implementation: IMPLEMENTATION,
      treasury: TREASURY,
      token,
      token_symbol: "PHA",
      public_url: product,
      listen_host: "127.0.0.1",
      listen_port: productPort,
      ledger_path: join(work, "ledger.sqlite3"),
      driver_public_key: rawPublicKey(generateKeyPairSync("ed25519").publicKey.export({ format: "der", type: "spki" })),
      webhook_public_keys: [rawPublicKey(publicKeyOf(webhookSeed))],
      demo_dir: join(web, "dist"),
    };
    writeFileSync(join(work, "product.json"), JSON.stringify(config));
    children.push(
      spawn("uv", [...uv, "-m", "reference_product", "serve", "--config", join(work, "product.json")], {
        env: { ...process.env, PYTHONPATH: join(root, "deploy/product") },
        stdio: ["ignore", openSync(join(logs, "product.log"), "w"), openSync(join(logs, "product.err"), "w")],
      }),
    );
    await waitFor(() => fetchOk(`${service}/evidences/quote.json`), 600);
    await waitFor(() => fetchOk(`${product}/healthz`), 600);

    Object.assign(process.env, {
      DEMO_URL: `${product}/demo/`,
      ANVIL_URL: anvil,
      PAYER_ADDRESS: payer,
      TOKEN_ADDRESS: token,
      TREASURY,
    });
  } catch (error) {
    await teardown();
    throw error;
  }
  return teardown;
}

function publicKeyOf(seed: Buffer): Buffer {
  // PKCS#8 wrapping of a raw ed25519 seed (RFC 8410).
  const der = Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), seed]);
  const key = createPrivateKey({ key: der, format: "der", type: "pkcs8" });
  return createPublicKey(key).export({ format: "der", type: "spki" });
}

function rawPublicKey(spki: Buffer): string {
  return spki.subarray(-32).toString("hex");
}

async function fetchOk(url: string): Promise<void> {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(`${url} answered ${response.status}`);
  }
}

function stop(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) {
    return Promise.resolve();
  }
  const exited = new Promise<void>((done) => child.once("exit", () => done()));
  child.kill();
  return exited;
}

function freePort(): Promise<number> {
  return new Promise((done, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      server.close(() => {
        if (typeof address === "object" && address !== null) {
          done(address.port);
        } else {
          reject(new Error("no port"));
        }
      });
    });
  });
}

async function waitFor(probe: () => Promise<unknown>, attempts = 100): Promise<void> {
  for (let attempt = 0; ; attempt += 1) {
    try {
      await probe();
      return;
    } catch (error) {
      if (attempt >= attempts) {
        throw error;
      }
      await new Promise((done) => setTimeout(done, 100));
    }
  }
}

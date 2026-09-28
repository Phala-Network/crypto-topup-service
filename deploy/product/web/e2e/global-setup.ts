import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { createPrivateKey, createPublicKey, generateKeyPairSync, randomBytes } from "node:crypto";
import { mkdirSync, mkdtempSync, openSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { createTestClient, http, publicActions, walletActions, type Address, type Hash, type Hex } from "viem";
import { sepolia } from "viem/chains";
import { build, preview, type PreviewServer } from "vite";

const web = resolve(import.meta.dirname, "..");
const root = resolve(web, "../../..");
// The deterministic deployment (deploy/CONTRACTS.md) and staging's treasury, as the product pins them.
const FACTORY = "0x45466D37587E6E46DC35eB96b74ba3D3b1E5b747";
const IMPLEMENTATION = "0x49F2F1F1a25269Ea0C6FF2AB1C7B09dCBE9c5bA9";
const TREASURY = "0x936c1991f8dA9a919fa11b557a3514719f5A4504";
const ACCOUNT = `acct_${"e2e0".repeat(8)}`;
const DETERMINISTIC_PROXY = "0x4e59b44847b379578588920cA78FbF26c0B4956C";

/**
 * Runs the whole demo locally: Anvil as Sepolia with the test token and the real forwarder
 * factory, deployed as deploy/CONTRACTS.md deploys it (through the deterministic deployment proxy
 * with the committed salt), so it lands at its pinned address; the fake top-up service
 * (e2e/fake_service.py), the reference product serving the demo's API, pinned to the fake
 * service's webhook key, and the page, built against that API and served from its own origin (as
 * Cloudflare serves pay.phala.com), under the CSP of public/_headers with the local origins: the
 * page calls the API cross-origin, with CORS and the demo account cookie. Tests read SITE_URL,
 * API_URL, SERVICE_URL, ANVIL_URL, PAYER_ADDRESS, TOKEN_ADDRESS, and TREASURY (which the tests
 * control on Anvil, as the merchant's finance team controls its treasury). Service logs go to
 * test-results/services.
 */
export default async function globalSetup(): Promise<() => Promise<void>> {
  const work = mkdtempSync(join(tmpdir(), "demo-e2e-"));
  const logs = join(web, "test-results", "services");
  mkdirSync(logs, { recursive: true });
  const children: ChildProcess[] = [];
  let site: PreviewServer | undefined;
  const teardown = async () => {
    await site?.close();
    await Promise.all(children.map(stop));
    rmSync(work, { recursive: true, force: true });
  };
  try {
    execFileSync(
      process.env["FORGE"] ?? "forge",
      [
        ...["build", "e2e/TestPha.sol"],
        ...["--root", ".", "--contracts", "e2e", "--no-lint"],
        ...["--out", join(work, "out"), "--cache-path", join(work, "cache")],
      ],
      { cwd: web, stdio: ["ignore", "ignore", "inherit"] },
    );
    const bytecode = (file: string, name: string) =>
      (JSON.parse(readFileSync(join(work, "out", file, `${name}.json`), "utf8")) as { bytecode: { object: Hex } })
        .bytecode.object;
    // The factory's creation code, built with the contracts' own pinned compiler profile.
    const factoryBuild = execFileSync(
      process.env["FORGE"] ?? "forge",
      [
        ...["inspect", "ForwarderFactory", "bytecode", "--root", join(root, "contracts")],
        ...["--out", join(work, "contracts-out"), "--cache-path", join(work, "contracts-cache")],
      ],
      { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] },
    )
      .trim()
      .split("\n")
      .at(-1);
    if (factoryBuild === undefined || !/^0x[0-9a-f]+$/.test(factoryBuild)) {
      throw new Error("forge inspect printed no ForwarderFactory bytecode");
    }
    const factoryInitCode: Hex = `0x${factoryBuild.slice(2)}`;
    const { factory_salt: factorySalt } = JSON.parse(
      readFileSync(join(root, "deploy/contracts/expected-codehashes.json"), "utf8"),
    ) as { factory_salt: Hex };

    const [anvilPort, servicePort, productPort, sitePort] = [
      await freePort(),
      await freePort(),
      await freePort(),
      await freePort(),
    ];
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
    const deployed = async (hash: Hash): Promise<Address> => {
      const { contractAddress } = await chain.waitForTransactionReceipt({ hash });
      if (contractAddress == null) {
        throw new Error("contract deployment failed");
      }
      return contractAddress;
    };
    const token = await deployed(
      await chain.deployContract({ account: payer, abi: [], bytecode: bytecode("TestPha.sol", "TestPha") }),
    );
    // Anvil carries the deterministic deployment proxy; its calldata is the salt, then the init
    // code. The factory's constructor creates the implementation (its first CREATE).
    await chain.waitForTransactionReceipt({
      hash: await chain.sendTransaction({
        account: payer,
        to: DETERMINISTIC_PROXY,
        data: `${factorySalt}${factoryInitCode.slice(2)}`,
      }),
    });
    const implementation = await chain.readContract({
      address: FACTORY,
      abi: [
        { type: "function", name: "implementation", inputs: [], outputs: [{ type: "address" }], stateMutability: "view" },
      ],
      functionName: "implementation",
    });
    if (implementation.toLowerCase() !== IMPLEMENTATION.toLowerCase()) {
      throw new Error(`the factory's implementation is ${implementation}, not the pinned ${IMPLEMENTATION}`);
    }

    const webhookSeed = randomBytes(32);
    // The stand-in service does not check the key; the product runs with a restricted key's form.
    writeFileSync(join(work, "product.key"), `ppay_rk_test_${"A".repeat(43)}000000`, { mode: 0o600 });
    const product = `http://127.0.0.1:${productPort}`;
    const service = `http://127.0.0.1:${servicePort}`;
    const origin = `http://127.0.0.1:${sitePort}`;
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
          ...["--account", ACCOUNT, "--treasury", TREASURY],
        ],
        { stdio: ["ignore", openSync(join(logs, "fake_service.log"), "w"), "inherit"] },
      ),
    );
    const config = {
      service_url: service,
      account: ACCOUNT,
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
      web_origin: origin,
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

    // The page as `build:cloudflare` builds it, with the local API's origin instead of staging's.
    const dist = join(work, "dist");
    process.env["VITE_DEMO_API_ORIGIN"] = product;
    await build({ root: web, logLevel: "warn", build: { outDir: dist } });
    const stagingApi = "https://pay-demo-api.phala.com";
    const stagingService = "https://pay-api-staging.phala.com";
    const csp = /^\s+Content-Security-Policy: (.+)$/m.exec(readFileSync(join(web, "public/_headers"), "utf8"))?.[1];
    if (csp === undefined || !csp.includes(` ${stagingApi} ${stagingService};`)) {
      throw new Error(`public/_headers has no CSP connecting to ${stagingApi} and ${stagingService}`);
    }
    site = await preview({
      root: web,
      logLevel: "warn",
      build: { outDir: dist },
      preview: {
        host: "127.0.0.1",
        port: sitePort,
        strictPort: true,
        headers: { "content-security-policy": csp.replace(stagingApi, product).replace(stagingService, service) },
      },
    });

    Object.assign(process.env, {
      SITE_URL: `${origin}/`,
      API_URL: product,
      SERVICE_URL: service,
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

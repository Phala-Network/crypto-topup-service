# Integrator sandbox

The sandbox lets a product integrate before mainnet (`docs/architecture.md` section 12, work
package E4): Sepolia, a test token, product credentials, and scripted late, under, over, and
rejected payment scenarios. The same scenarios run now against a disposable local stack and
later, unchanged, against the Sepolia sandbox.

Every step that deploys contracts, changes a CVM, or writes to the sandbox database is marked
**HUMAN-ONLY**; agents and CI never run them.

## Contents

| File | Purpose |
|---|---|
| `routes/sandbox-sepolia.template.yaml` | Capped route for one integrator product (chain id 11155111). |
| `render-route.sh` | Renders the template from environment variables; refuses leftover placeholders. |
| `deploy-test-contracts.sh` | Deploys the test token (A1 `MockERC20`, public `mint`), a second token for the unsupported-asset scenario, and `MockSanctionsOracle`. |
| `issue-product.sh` | Registers a product's slug, public key, key id, settlement URL, and webhook URL, with an audit row. |
| `docker-compose.sepolia.yml`, `render-sepolia-compose.sh` | Overlay for `deploy/docker-compose.yml` and the renderer that inlines the sandbox route into the attested compose. |
| `docker-compose.local.yml`, `run-local.sh` | Local stack (`deploy/local` plus Anvil) and the end-to-end driver. |
| `scenarios/docker_restart.py` | The local `restart_command`: restarts the service container through the Docker API. |
| `scenarios/` | Scripted scenarios; `run.py` runs them against any configured stack. |

## Run everything locally

Requirements: Docker Compose, Foundry v1.8.3 (`forge`, `cast`), `jq`, and `uv`.

```sh
deploy/sandbox/run-local.sh                 # example, then every scenario
deploy/sandbox/run-local.sh happy_path      # example, then selected scenarios
```

The script builds the images, starts PostgreSQL, the dstack simulator, and an Anvil chain with
Sepolia's chain id (one-second blocks, `finalized` two blocks behind), deploys the forwarder
factory and the sandbox test contracts, creates a product key, renders and validates the route
with a 45-second rate-lock window, issues the product, starts the service, runs
`sdk/examples/phala_cloud_integration.py`, and then `scenarios/run.py`. The example and the
scenarios run in a pinned uv/Python 3.12 container on the compose network, where the service
reaches the product endpoints as `http://product:8089`; this works even where a host firewall drops
traffic from containers to the host. `restart_mid_flow` runs last in its own container, the
only one given the Docker socket, which it uses to restart the local service
(`scenarios/docker_restart.py`). Prices come from
the live Coin Metrics, Binance, and Kraken endpoints, as in production. All containers, volumes,
and temporary files are removed on exit.

## Scenarios

Each scenario registers fresh workspaces, pays with the test token, and asserts the deposit state
from the product API, the verified webhooks, and the product ledger of the reference endpoint.

| Scenario | Payment | Expected |
|---|---|---|
| `happy_path` | Exact locked amount, then a persistent-address payment | Both `credited`; lock valued at the lock price with exactly the quoted credit and `consumed`; persistent at spot; `deposit.confirmed` and `deposit.credited` delivered; one ledger credit each. |
| `late_payment` | Exact locked amount after `rate_lock.expired` | `credited` at spot; lock stays `expired`. |
| `underpayment` | 97% of the locked amount (tolerance is 1%) | `credited` at spot below the quote; lock not consumed; cancel is refused (`409 pending_payment` while the lock is open). |
| `overpayment` | +0.5%, then +5% on a second lock | Within tolerance: lock price, exact quoted credit, `consumed`. Beyond: spot for the full amount, lock not consumed. |
| `unsupported_asset` | A token without a route to a persistent address | `rejected`, `deposit.rejected` reason `unsupported_asset`; the product is never asked to settle. |
| `product_refusal` | Payment for a suspended workspace | Product records `rejected` without credit; deposit `rejected`, `deposit.rejected` reason `product_refused` with the product's reason. |
| `restart_mid_flow` | The product commits the credit but its answer is lost, then the service restarts | After restart the service `GET`s the key before any resend and adopts the answer; exactly one ledger credit. Needs `restart_command`, so it is skipped on Sepolia unless an operator runs it. |

These cover the same behaviours as the E1 conformance cases from the service side: business
refusal is a typed `200 rejected`, unknown results are resolved by `GET` before resending, and a
key is credited once.

## Obtaining sandbox credentials (integrators)

Credential issuance is a human step on both sides.

1. Create the product signing key on a machine you control and keep the seed file secret:

   ```sh
   cd sdk/python
   uv run --locked topup-sdk keygen --keyid acme/v1 --seed-out ~/acme-sandbox.seed
   ```

   It prints `{"keyid": ..., "public_key": ...}`.
2. Send the operator, through the agreed support channel: the product slug you want (lowercase
   letters, digits, and dashes), the printed key id and public key, and public HTTPS URLs for
   your settlement endpoint and webhook receiver. Never send the seed.
3. The operator returns the sandbox service URL, your route name, the chain id, the forwarder
   factory and implementation addresses, the test token and unsupported-token addresses, and the
   attestation instructions for pinning the settlement key (`keyid = settlement/v1`).

Test tokens are free: `MockERC20.mint(address,uint256)` is public. You also need Sepolia ETH for
gas from a public faucet.

## Issuing credentials and deploying the Sepolia sandbox (operators)

1. **HUMAN-ONLY:** deploy the forwarder factory on Sepolia with the A2 procedure in
   `deploy/CONTRACTS.md`, then the sandbox-only contracts, signed by a Foundry keystore account:

   ```sh
   cast wallet import sandbox-deployer --interactive   # once, with a funded throwaway key
   ETH_PASSWORD=... deploy/sandbox/deploy-test-contracts.sh \
     --rpc-url "$SEPOLIA_RPC_URL" --account sandbox-deployer
   ```

2. Render and validate the integrator's route (one route per product; the product slug and key
   id must match the issued credentials):

   ```sh
   FORWARDER_FACTORY=0x... IMPLEMENTATION=0x... TREASURY=0x... TEST_TOKEN=0x... \
     SANCTIONS_ORACLE=0x... PRODUCT_SLUG=acme PRODUCT_KID=acme/v1 \
     SETTLEMENT_URL=https://acme.example/topup/settlements \
     deploy/sandbox/render-route.sh > sandbox-acme.yaml
   docker run --rm -v "$PWD/sandbox-acme.yaml:/route.yaml:ro" "$TOPUP_IMAGE" \
     topup route validate /route.yaml
   ```

3. Render the sandbox compose with literal image digests and the inlined route:

   ```sh
   TOPUP_IMAGE=...@sha256:... POSTGRES_WALG_IMAGE=...@sha256:... \
     deploy/sandbox/render-sepolia-compose.sh sandbox-acme.yaml > sandbox-compose.json
   docker compose -f sandbox-compose.json config -q
   ```

4. **HUMAN-ONLY:** deploy or update the sandbox CVM with `sandbox-compose.json` exactly as the
   staging procedure in `deploy/README.md` describes, with a separate encrypted environment whose
   RPC providers point at Sepolia.

   **Known risk, verify before opening the sandbox to integrators:** the service rebuilds the
   signed `@target-uri` from the `Host` header and uses `http` unless the request carries
   `X-Forwarded-Proto` (`crates/topup/src/api/auth.rs`). Integrators sign the `https://` URL
   they call, so if the dstack gateway terminates TLS without forwarding
   `X-Forwarded-Proto: https`, every product request fails with `401`. Run
   `sdk/examples/phala_cloud_integration.py` against the deployed sandbox URL first. If it fails
   this way, keep the sandbox closed until the service can be configured with its public origin
   (tracked in #77).
5. **HUMAN-ONLY:** issue the product through the sandbox's administrative database access
   (there is deliberately no product-creation API):

   Keep the connection details and password out of the command line: define a
   `topup-sandbox-admin` entry in `~/.pg_service.conf` and the password in `~/.pgpass` (mode 0600).

   ```sh
   PSQL="psql service=topup-sandbox-admin" deploy/sandbox/issue-product.sh \
     --slug acme --keyid acme/v1 --public-key '<base64>' \
     --settlement-url https://acme.example/topup/settlements \
     --webhook-url https://acme.example/topup/webhooks --operator "$USER"
   ```

   Changing a product's key or URLs is a separate, audited change; the script refuses to
   overwrite an existing slug.

## Running the scenarios against Sepolia (integrators)

Write a configuration file; the fields are those of `SandboxConfig` in
`sdk/examples/phala_cloud_integration.py`:

```json
{
  "service_url": "https://sandbox.topup.example",
  "product_slug": "acme",
  "product_keyid": "acme/v1",
  "product_seed_file": "/home/me/acme-sandbox.seed",
  "route": "sandbox-acme-tpha-usd",
  "chain_id": 11155111,
  "rpc_url": "https://your-sepolia-rpc.example",
  "factory": "0x...",
  "implementation": "0x...",
  "token": "0x...",
  "token_symbol": "PHA",
  "unsupported_token": "0x...",
  "listen_host": "127.0.0.1",
  "listen_port": 8089,
  "public_url": "https://acme.example/topup",
  "payer": "0xYourTestAccount",
  "payer_account": "sandbox-payer"
}
```

- `listen_host` and `listen_port` are where the reference endpoint listens; `public_url` is the
  HTTPS URL registered with the operator, forwarded to it by your tunnel or reverse proxy. The
  endpoint verifies signatures against `public_url`, never the incoming `Host` header.
- `payer_account` is a Foundry keystore account (`cast wallet import sandbox-payer --interactive`)
  holding a throwaway test key with Sepolia ETH; export `ETH_PASSWORD` for `cast`.
- Set `settlement_public_key` (hex) after verifying the attestation quote; otherwise the example
  checks only the attestation's nonce binding and warns.

Then run:

```sh
uv run --locked --project sdk/python python sdk/examples/phala_cloud_integration.py --config sandbox.json
uv run --locked --project sdk/python python deploy/sandbox/scenarios/run.py --config sandbox.json
```

Sepolia finality takes about 15 minutes per deposit, so a full run takes a few hours; pass
scenario names to run a subset. The sandbox route's rate-lock window is 120 seconds, so the late
payment scenario waits at least that long. `restart_mid_flow` is reported as `SKIP` without a
`restart_command`.

# Integrator sandbox

The sandbox lets a product integrate before mainnet (`docs/architecture.md` section 12, work
package E4): Sepolia, a test token, product credentials, and scripted late, under, over, and
rejected payment scenarios. The same scenarios run now against a disposable local stack and
later, unchanged, against the Sepolia sandbox.

Every step that deploys contracts, changes a CVM, or issues a product with the sandbox's admin key
is marked **HUMAN-ONLY**; agents and CI never run them.

## Contents

| File | Purpose |
|---|---|
| `routes/sandbox-sepolia.template.yaml` | Capped route for one integrator product (chain id 11155111). |
| `render-route.sh` | Renders the template from environment variables; refuses leftover placeholders. |
| `deploy-test-contracts.sh` | Deploys the test token (A1 `MockERC20`, public `mint`), a second token for the unsupported-asset scenario, and `MockSanctionsOracle`. |
| `docker-compose.sepolia.yml`, `render-sepolia-compose.sh` | Overlay for `deploy/docker-compose.yml` and the renderer that inlines the sandbox route into the attested compose. |
| `docker-compose.local.yml`, `run-local.sh` | Local stack (the attested compose with the `deploy/local` overlay, plus Anvil) and the end-to-end driver. |
| `scenarios/docker_restart.py` | The local `restart_command`: restarts the service container through the Docker API. |
| `scenarios/` | Scripted scenarios; `run.py` runs them against any configured stack. |

## Run everything locally

Requirements: Docker Compose, Foundry v1.8.3 (`forge`, `cast`), `jq`, `uv`, `curl`, and OpenSSL 3.

```sh
deploy/sandbox/run-local.sh                 # examples, then every scenario
deploy/sandbox/run-local.sh happy_path      # examples, then selected scenarios
```

The script builds the images, starts PostgreSQL, the dstack simulator, and an Anvil chain with
Sepolia's chain id (one-second blocks, `finalized` eight blocks behind), deploys the forwarder
factory and the sandbox test contracts, creates a product key, renders and validates the route
with a 45-second rate-lock window, issues the product, starts the service, runs the SDK example
`sdk/examples/phala_cloud_integration.py`, the reference product (`deploy/product`) with one
deposit driven through it, and then `scenarios/run.py`. They run in a pinned uv/Python 3.12 container on the compose network, where the service
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
| `happy_path` | Exact locked amount, then a persistent-address payment | Before finality the lock shows `payment.status == "seen"` (in time, amount within tolerance) and the persistent payment is listed by `pending-deposits`; then both `credited`; lock valued at the lock price with exactly the quoted credit and `consumed`; persistent at spot; `deposit.confirmed` and `deposit.credited` delivered; one ledger credit each. |
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

   It prints `{"keyid": ..., "public_key": ...}`. Use this key only for the sandbox and create
   a separate key for every other deployment: each deployment records used signatures in its
   own database, so a request signed with a shared key could be replayed within the five-minute
   freshness window against another deployment that shares the same public origin.
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
   ETH_PASSWORD=/path/to/0600-password-file deploy/sandbox/deploy-test-contracts.sh \
     --rpc-url "$SEPOLIA_RPC_URL" --account sandbox-deployer
   ```

2. Render and validate the integrator's route (one route per product). The route is the only
   source of the product's key id and settlement URL; its slug must match the issued product:

   ```sh
   FORWARDER_FACTORY=0x... IMPLEMENTATION=0x... TREASURY=0x... TEST_TOKEN=0x... \
     SANCTIONS_ORACLE=0x... PRODUCT_SLUG=acme PRODUCT_KID=acme/v1 \
     SETTLEMENT_URL=https://acme.example/topup/settlements \
     deploy/sandbox/render-route.sh > sandbox-acme.yaml
   docker run --rm -v "$PWD/sandbox-acme.yaml:/route.yaml:ro" "$TOPUP_IMAGE" \
     topup route validate /route.yaml
   ```

3. Render the sandbox compose with literal image digests, the sandbox's attested settings (the
   names of the `staging` Environment variables in `deploy/README.md`, "Attested settings",
   exported with the sandbox's values: its keyless Sepolia RPC URLs, its own backup prefix and
   admin key, and `TOPUP_PUBLIC_ORIGIN`, the sandbox's public gateway URL, for example
   `https://sandbox.topup.example`, no path), and the inlined route:

   ```sh
   TOPUP_IMAGE=...@sha256:... POSTGRES_WALG_IMAGE=...@sha256:... \
     deploy/sandbox/render-sepolia-compose.sh sandbox-acme.yaml > sandbox-compose.json
   docker compose -f sandbox-compose.json config -q
   ```

4. **HUMAN-ONLY:** deploy or update the sandbox CVM with `sandbox-compose.json` exactly as the
   staging procedure in `deploy/README.md` describes, with a separate encrypted environment that
   holds only the sandbox's own secrets (the `staging.env.example` names). Integrators sign the
   URL they call and the service verifies `@target-uri` against the rendered
   `TOPUP_PUBLIC_ORIGIN`, so a wrong value makes every signed request fail with `401`. Run `sdk/examples/phala_cloud_integration.py` against the
   deployed sandbox URL before opening it to integrators.
5. **HUMAN-ONLY, sandbox admin key holder:** issue the product with `POST /v1/admin/products`
   against the sandbox's `TOPUP_PUBLIC_ORIGIN`, exactly as
   [Product credentials](../README.md#product-credentials) describes: the integrator's slug (the
   route's `destination.product`), public key, and HTTPS webhook URL. The request is audited; a
   repeat with the same values returns the same product, and different values for an issued slug
   are refused with `409`. The key id and settlement URL come only from the route.

## Running the scenarios against Sepolia (integrators)

Write a configuration file; the fields are those of `ProductConfig` in
`deploy/product/reference_product/config.py`:

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
  "payer_account": "sandbox-payer"
}
```

- `listen_host` and `listen_port` are where the reference endpoint listens; `public_url` is the
  HTTPS URL registered with the operator, forwarded to it by your tunnel or reverse proxy. The
  endpoint verifies signatures against `public_url`, never the incoming `Host` header.
- `payer_account` is a Foundry keystore account (`cast wallet import sandbox-payer --interactive`)
  holding a throwaway test key with Sepolia ETH; instead of it, `ETH_KEYSTORE` may name the
  keystore file. Export `ETH_PASSWORD` as the path of a mode-0600 file holding its keystore
  password (read as a password file, as Foundry does). `payer` (an unlocked address) is only for
  Anvil.
- Without a mode the reference product runs the product (`serve`) and one deposit (`deposit`) in
  one process; the two modes also run separately, as for staging (deploy/README.md, "Staging
  reference product").
- Set `settlement_public_key` (hex) after verifying the attestation quote; otherwise the product
  checks only the attestation's nonce binding and warns.

Then run:

```sh
uv run --locked --project sdk/python python sdk/examples/phala_cloud_integration.py --config sandbox.json
PYTHONPATH=deploy/product uv run --locked --project sdk/python python -m reference_product --config sandbox.json
uv run --locked --project sdk/python python deploy/sandbox/scenarios/run.py --config sandbox.json
```

Sepolia finality takes about 15 minutes per deposit, so a full run takes a few hours; pass
scenario names to run a subset. The sandbox route's rate-lock window is 120 seconds, so the late
payment scenario waits at least that long. `restart_mid_flow` is reported as `SKIP` without a
`restart_command`.

Staging settles with its own product CVM, so the scenarios do not run there; the deposit
driver's options play their payments through that product instead (deploy/README.md,
"Abnormal paths").

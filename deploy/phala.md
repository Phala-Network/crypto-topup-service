# Phala's instance

Phala runs an instance only for Phala Cloud and offers no hosted service to others. Its
`staging` Environment (`https://pay-api-staging.phala.com`, on Sepolia and Base Sepolia) also runs
a reference product whose API serves the live demo on Phala's website,
[pay.phala.com](https://pay.phala.com/). This page records that setup and Phala's own policies.
Another operator needs none of it, and can run the reference product the same way for its own
rehearsals. The generic procedures are in the [deployment reference](README.md).

## Onboarding policy

Phala onboards accounts as in [Operator onboarding](README.md#operator-onboarding), with due
diligence under Phala's policy (design §17). Until Phala's legal review signs off, live mode
(`charges_enabled`) is for Phala's own accounts only; third-party merchants go live only after
it.

## Staging routes

Staging serves four test-mode routes, two on Sepolia and two on Base Sepolia, all on the
deterministic factory ([Contracts](README.md#contracts)); any test key quotes on all of them, and
`GET /v1/config` lists each chain's assets:

| Route | Token | Pricing | Test tokens |
|---|---|---|---|
| `phala-cloud-sepolia-pha-usd` ([file](config/routes/phala-cloud-sepolia-pha.yaml)) | test PHA `0x8F40e7E99678F44c88158f049E62817580ab113B` (`MockERC20`, 18 decimals) | spot: Coin Metrics `pha`, checked against Binance `PHAUSDT` | `mint(address,uint256)` is public |
| `phala-cloud-sepolia-usdc-usd` ([file](config/routes/phala-cloud-sepolia-usdc.yaml)) | Circle's testnet USDC `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238` ([Circle's list](https://developers.circle.com/stablecoins/usdc-contract-addresses), 6 decimals) | stablecoin: 1.00 while Coin Metrics' `usdc` rate is within 1% | [Circle's faucet](https://faucet.circle.com) (Ethereum Sepolia) |
| `phala-cloud-base-sepolia-pha-usd` ([file](config/routes/phala-cloud-base-sepolia-pha.yaml)) | test PHA `0x1a6F260377e42ead1418C7C1afDFD5DE371A9284` (the same `MockERC20`, 18 decimals) | as on Sepolia | `mint(address,uint256)` is public |
| `phala-cloud-base-sepolia-usdc-usd` ([file](config/routes/phala-cloud-base-sepolia-usdc.yaml)) | Circle's testnet USDC `0x036CbD53842c5426634e7929541eC2318f3dCF7e` ([Circle's list](https://developers.circle.com/stablecoins/usdc-contract-addresses), 6 decimals) | as on Sepolia | [Circle's faucet](https://faucet.circle.com) (Base Sepolia) |

| Chain | `confirmations` | RPC providers | Sanctions oracle (a `MockSanctionsOracle`) |
|---|---|---|---|
| Sepolia (11155111) | `2`, Ethereum L1's default | `provider-a`, `provider-b` | `0x28A73f8235d966244210D9c49E34EDdA4fF9e1f6` |
| Base Sepolia (84532) | `safe`, the OP-stack default: about 5 minutes, never the sequencer's unsafe head (architecture §8) | `base-sepolia-a` `https://base-sepolia.gateway.tenderly.co`, `base-sepolia-b` `https://base-sepolia-rpc.publicnode.com`, both keyless | `0x8A0C93d85a05aD30741C193068abF2e5E16e7b35` |

There is no USDT route: Tether publishes no testnet USDT, and a third-party token is not one.
USDC moves one or two transfers a block on both chains, so its routes set `backstop: addresses`,
which puts each whole chain, PHA included, on transfer requests by recipient (architecture §8); the
RPC cost is unchanged while staging has fewer than 1 000 addresses ([Measuring RPC
usage](README.md#measuring-rpc-usage)). The head loop polls every 12 s on both chains
(`--head-poll-interval-s`), six Base blocks, so Base Sepolia costs about what Sepolia does.
Routes are attested config: adding or changing one is a PR and a Deploy `upgrade` of `topup`,
never a reset.

### RPC providers

Staging's providers, in the `staging` Environment's `TOPUP_RPC_<ID>_URL` variables
([RPC providers](README.md#rpc-providers)); all four are keyless and free:

| Chain | Slot | Provider |
|---|---|---|
| Sepolia | A | Tenderly public gateway (`https://sepolia.gateway.tenderly.co`) |
| Sepolia | B | PublicNode (`https://ethereum-sepolia-rpc.publicnode.com`) |
| Base Sepolia | A | Tenderly (`https://base-sepolia.gateway.tenderly.co`) |
| Base Sepolia | B | PublicNode (`https://base-sepolia-rpc.publicnode.com`) |

Checked on 2026-09-29, Tenderly is the only keyless public endpoint that serves slot A's
`eth_getLogs` over 2 000 blocks without a contract address on both chains (Grove/Pocket serves it
on Sepolia but was not chosen, and returned regressing `finalized` heads on Base Sepolia), while
`sepolia.base.org` caps the range at 1 000 blocks, PublicNode requires an address, thirdweb caps
the response size, and Nodies and 1RPC cap the range at 50 blocks.

Mainnet needs paid providers from two different companies.

## Staging reset (HUMAN-ONLY)

Phala's staging was reset this way for the multi-tenant launch, and the procedure is kept for a
future reset (pull request #208 pinned the reference product's new account). The multi-tenant
schema (design §14) replaced the migration history and migrates no data
(`crates/topup/migrations/README.md`), and staging's route now uses the new factory, so the
staging service is replaced, not upgraded: a new CVM on an empty backup prefix, with every account
created again. Nothing on staging is live, so no funds or merchants are affected. Every step below
is HUMAN-ONLY except the workflow runs, which the staging owner dispatches; agents and CI run none
of them. In order:

1. **Build.** Run Release images on `main` and note its run id.
2. **Factory on Sepolia: done.** The factory `0x45466D37587E6E46DC35eB96b74ba3D3b1E5b747` and its
   implementation `0x49F2F1F1a25269Ea0C6FF2AB1C7B09dCBE9c5bA9` are deployed and verified
   ([Contracts](README.md#contracts)); confirm Verify contracts is green.
3. **Treasury Safe: done.** The staging finance Safe `0x936c1991f8dA9a919fa11b557a3514719f5A4504`
   (v1.4.1, 1-of-1) has the `CompatibilityFallbackHandler` v1.4.1
   `0xfd0732Dc9E303f09fCEf3a7388Ad10A83459Ec99` as its fallback handler (Sepolia transaction
   `0xc63baf595bd13f9f27c27ba2a370c602bb2008c8703ab9629095af8844f10812`), so it can prove itself as
   a treasury; [contracts/safe-expectations.json](contracts/safe-expectations.json) records it, and
   `deploy/contracts/verify-safe.sh` passes on both providers. The reference product's account has
   since moved to the staging Safe `0x26430107887d4a691B340BdB887096B83E7a5844`, the same address
   (SafeL2 v1.4.1, 1-of-1, the same fallback handler) on Sepolia and Base Sepolia. Never use
   `0x936c…4504` on Base Sepolia: a copy exists there whose owner key is destroyed.
4. **Stop the old service.** `npx --yes phala@1.1.22 cvms stop "$TOPUP_CVM_ID"` and the same for
   `$STAGING_PRODUCT_CVM_ID`. Keep both CVMs and the old backup prefix for the retention period: they
   restore only with an image built before the reset. Record their ids.
5. **Point staging at an empty database.** Set the `staging` variable `WALG_S3_PREFIX` to a new,
   empty prefix (PostgreSQL initializes a cluster only on a prefix that provably holds no backup,
   [RESTORE.md](RESTORE.md#bootstrap-from-backup)); clear `TOPUP_CVM_ID` and
   `STAGING_PRODUCT_CVM_ID`.
6. **Provision the service.** Deploy (`staging`, `topup`, `provision`, the release of step 1); set
   `TOPUP_CVM_ID` to the new id; [seal the secrets](README.md#sealing-the-secrets); update the
   [DNS records](README.md#custom-domain) the summary lists (the CNAME to the new gateway, the
   `_dstack-app-address` TXT to the new instance); then Deploy `upgrade` with the same release,
   which waits for `/healthz` and verifies the attestation and the certificate evidence. The new
   app id derives new webhook keys for every account.
7. **Verify** the attestation from your machine ([Attestation](README.md#attestation-ingress-and-egress)) and
   that `GET /v1/config` with any test key lists the Sepolia assets with `confirmations` 2 (the
   routes' versions are in the attested compose Deploy `upgrade` verified).
8. **Onboard the staging accounts** ([Operator onboarding](README.md#operator-onboarding), steps 1–3, with
   `charges_enabled: false`: Sepolia routes are test routes), first the reference product's, then
   each internal merchant's (Phala Cloud's staging backend), and send each contact its `acct_…` and
   key.
9. **Set up the reference product's account** as its merchant ([Staging reference
   product](#staging-reference-product), steps 2–4): roll the key, a restricted key for the product,
   the treasury (step 3's Safe, as a Safe message), and, after the product is provisioned, its
   webhook endpoint.
10. **Run one deposit** of each collection method ([Staging reference product](#staging-reference-product),
    step 5) and one [sweep](README.md#sweeping) from the treasury Safe; confirm `swept` and the daily report.
11. **Retire the old CVMs** once the new service has run clean for a day: `npx --yes phala@1.1.22
    cvms delete "$OLD_CVM_ID" --force` for each, by the recorded id (never by name or app id); delete the old backup prefix only at the end of its retention.

## Staging reference product

Staging's reference product is a merchant like any other, with its own account, and a second CVM
running [product/reference_product](product/reference_product): `serve` mode is the webhook
receiver that applies every `deposit.*` snapshot by the balance rule (a deposit nets to
`amount − amount_refunded − amount_reversed` while `credited` or `reversed`; its tests are in
`product/tests`), an account API, and the API of the website's live demo, with a SQLite ledger;
`deposit` mode, run
from an operator's machine, plays a customer and signs the product's account API with a separate
driver key (`driver/v1`, the product's own authentication, not Phala Pay's).

- **Its key.** The sealed env holds only `PRODUCT_API_KEY`, the account's **restricted** test key
  (`ppay_rk_test_…`) with exactly the permissions it uses, listed in
  [product/staging.env.example](product/staging.env.example): `account.read`, `quotes.write`,
  `deposit_addresses.write`, `deposits.read`, `refunds.write`, `sweeps.read`, `forwarders.read`.
  Its preflight ([product/preflight.sh](product/preflight.sh)) accepts only a test key, restricted
  or secret. The account's secret key stays with the staging owner, offline.
- **Its pins.** The attested product config names its `account` (`acct_…`), the forwarder factory
  and implementation (the same on every chain), and, for each of its `chains` (Sepolia and Base
  Sepolia), the account's treasury there, from which the SDK recomputes every quote and
  deposit address before the product shows it; the placeholder `acct_000…` fails the online
  preflight until a PR sets the real id. It pins its account's test-mode webhook keys from the
  authenticated attestation at `TOPUP_ORIGIN`, fetched with `PRODUCT_API_KEY`, at startup or on the
  first webhook when the key is sealed later (until then it answers `503`, and topup retries).
- **Attested settings.** `TOPUP_ORIGIN` (`https://$TOPUP_DOMAIN`), `PRODUCT_PUBLIC_URL`
  (`https://$PRODUCT_DOMAIN`, its [custom domain](README.md#custom-domain)), `PRODUCT_DOMAIN` and
  `PRODUCT_GATEWAY_DOMAIN` (dstack-ingress's), and `PRODUCT_DRIVER_PUBLIC_KEY`. The config itself
  commits each chain's `rpc_url`, a keyless public RPC (publicnode's; the product seals no RPC key,
  and its preflight refuses a keyed URL and checks online that each reports its chain and that the
  chain's treasury is a contract), its test tokens, `bonus_bps` (the demo merchant's own +10% on
  credits paid in PHA, a promotion, not a Phala Pay feature), and `web_origin`,
  `https://pay.phala.com`, the only origin the demo's API allows.
- **Its restore records.** Its ledger keeps what a service restore asks merchants for
  ([restore runbook](runbooks/restore.md) step 2): each verified delivery once per `webhook-id`,
  in the webhook inbox, as received (the raw body bytes and the `webhook-id`,
  `webhook-timestamp`, and `webhook-signature` headers) in the transaction that applies it; and
  each quote and deposit address it creates, as the service returned it, `client_secret`
  included. A client secret is a capability: the ledger file is its owner's alone (mode 0600),
  and nothing logs one. `python -m reference_product export-restore-records --config FILE`, next
  to the ledger (in the product container), prints them as the bodies of the operator's
  `POST /v1/admin/restore/deposit_addresses`, `/quotes`, and `/events` requests (the events in
  batches of 100), without `reason`; `--since` takes the restore point, and `--output` writes a
  new mode-0600 file instead. A ledger from before the inbox is migrated in place when the product
  starts; the events it stored earlier have no raw delivery, so they are not exported. The
  controlled [restore drill](RESTORE.md#local-and-ci-drills) runs this receiver and imports what
  it exports.
- **Its networks.** The page offers a configured chain only once the service serves assets there
  (`GET /v1/config`): Base Sepolia appears when its route is deployed, with no product change.
- **Its custom domain.** The same pinned dstack-ingress as topup's
  ([Custom domain](README.md#custom-domain)) terminates TLS for `$PRODUCT_DOMAIN` (Phala's:
  `pay-demo-api.phala.com`) in the product's compose and forwards to `product:8089`, so the demo's
  API, the product's webhook endpoint, and its account API are at `https://$PRODUCT_DOMAIN`.
  Phala's website, `pay.phala.com`, is not a CVM's: Cloudflare serves it ([Website](#website)).

The product serves the JSON API of the live demo on the public **Phala Pay website**
([pay.phala.com](https://pay.phala.com/), [product/web](product/web), served by Cloudflare:
[Website](#website)) at `PRODUCT_PUBLIC_URL/api/`
([reference_product/demo.py](product/reference_product/demo.py)); it serves no page. The page
calls it cross-origin: the API answers CORS preflights and sends
`Access-Control-Allow-Origin: https://pay.phala.com` with `Access-Control-Allow-Credentials: true`
and `Vary: Origin` on every `/api/` response, errors included, and nothing of CORS to any other
origin; `/webhooks`, `/healthz`, and `/accounts` have no CORS. The page is a short headline, the
live demo, and the key properties. The demo
sets the product beside its backend (stacked on narrow screens): first, what the customer sees (a cloud console's
billing page, framed as the merchant's app); then what the merchant's backend sees (the
payment's live event stream, then tabs for payments, refunds, sweeps, API requests and webhooks,
and the attestation). The billing page has both ways to collect a payment: a **quote** (a locked price and an exact amount, paid through
`@phala/pay`'s `<Checkout expectedAddress>`) and the visitor's single **deposit address** (every
token on every network, any amount credited at spot, its payments read by the browser with the
address's `client_secret`). An order id set as `metadata` arrives in the `deposit.credited` event.
A timeline built only from real data (chain block times, the service's objects read with the
product key, this product's verified webhooks and ledger rows) shows each payment received, credited
(at two confirmations), final, and reversed if it is; the ledger follows the balance rule. Refunds
follow the merchant flow: declare (a final deposit), pay from the treasury of the deposit's
address, `mark_paid`, verified at finality; on staging that treasury is the finance Safe, so a
visitor who pays from their own wallet sees the verification fail (`sender_mismatch`). Sweeps are
the merchant's: the page shows the unswept balance, the `flush` call and the Safe Transaction
Builder batch the SDK builds, and the finalized sweeps; the product holds no wallet key. Each
browser gets a random demo account in a cookie of the API's origin (`HttpOnly; Secure;
SameSite=Lax; Path=/`, host-only: the two origins are same-site under `phala.com`, so the page's
credentialed requests carry it); quote creation is rate-limited per account (3 a minute, 20 a day)
and overall (30 a minute), POSTs must be JSON, and the page carries a strict CSP. Test PHA is
minted by the visitor's own wallet on the selected network (`mint` is public on the staging tokens);
test USDC comes from Circle's faucet, and gas from each testnet's public faucets. With `sdk/js` built, `cd product/web && pnpm run e2e` runs the whole flow on
Anvil, with the real factory at its deterministic address, against a stand-in service
([product/web/e2e/fake_service.py](product/web/e2e/fake_service.py)): it builds the page against
the local product and serves it from its own origin under the CSP of `public/_headers`, so the
demo runs cross-origin, with CORS and the cookie, as in production.

Setup, in order, after the [staging reset](#staging-reset-human-only)'s steps 1–8 (each step
**HUMAN-ONLY** unless it is a workflow run):

1. On the owner's machine (mode-0600 files, never committed), create the driver key and set the
   `staging` variable `PRODUCT_DRIVER_PUBLIC_KEY` (the driver's printed `public_key`):

   ```sh
   cd sdk/python
   uv run --locked topup-sdk keygen --keyid driver/v1 --seed-out ~/staging/driver.seed
   ```

2. **As the product's merchant**, with the account's first secret test key from
   [onboarding](README.md#operator-onboarding): roll it, then create the product's restricted key and keep
   its `secret` for step 4:

   ```sh
   curl -fsS "$TOPUP_PUBLIC_ORIGIN/v1/api_keys" -H "Authorization: Bearer $SECRET_KEY" \
     -H 'content-type: application/json' -d '{"name": "reference product", "type": "restricted",
     "permissions": ["account.read", "quotes.write", "deposit_addresses.write", "deposits.read",
     "refunds.write", "sweeps.read", "forwarders.read"]}'
   ```

3. **Treasury Safe owners**: set the account's treasury on each of its chains to the staging Safe
   ([Treasury setup](README.md#treasury-setup), Safe message; in test mode it applies at once). Open a PR
   setting the product config's `account` in [product/docker-compose.yml](product/docker-compose.yml)
   to the new `acct_…` id, and merge it.
4. Deploy (`staging`, target `product`, `provision`), set `STAGING_PRODUCT_CVM_ID`, create the
   [DNS records](README.md#custom-domain) for `$PRODUCT_DOMAIN` the summary lists, seal `.env.product`
   holding `PRODUCT_API_KEY=<ppay_rk_test_…>` with the two commands it prints, and Deploy
   `upgrade` with the same release, which waits for `https://$PRODUCT_DOMAIN/healthz` and verifies
   the certificate evidence. Then, with the secret key, register the product's endpoint:
   `POST /v1/webhook_endpoints {"url": "<PRODUCT_PUBLIC_URL>/webhooks", "enabled_events": ["*"]}`
   and `POST /v1/webhook_endpoints/{id}/test`.
5. Run a deposit. The payer is a Foundry keystore with a throwaway key and some testnet ETH; the
   test PHA token is a `MockERC20` with a public `mint`, so the driver mints the quoted amount and
   pays it. `driver.json` holds the `ProductConfig` fields: `service_url` (topup's origin),
   `account`, `factory`, `implementation`, `chains` (as in the product config: each with its
   `chain_id`, `name`, `rpc_url`, `treasury`, and `test_tokens`), and `public_url` (the product
   URL). The driver pays on the first chain, with its first test token; `--chain-id 84532` pays on
   Base Sepolia instead, once its route is served.

   ```sh
   export ETH_KEYSTORE=~/.foundry/keystores/staging-payer ETH_PASSWORD=~/staging/payer.password
   PYTHONPATH=deploy/product uv run --locked --project sdk/python python -m reference_product deposit \
     --config driver.json --driver-seed-file ~/staging/driver.seed \
     --amount-minor <cents>
   ```

   The driver recomputes the quote's address before paying and exits 0 once the product has
   recorded exactly one credit and the verified `deposit.credited` webhook (about 30 seconds after
   paying, at the route's two confirmations). `--min-atomic` refuses a quote below that many atomic
   units and prints the `--amount-minor` needed; the quote must also fit the 500000-cent
   per-deposit and per-account caps (PHA below about $0.24). `--until swept` also waits until the
   merchant [sweeps](README.md#sweeping) the forwarder and the sweep is finalized. The deposit address is
   exercised from the demo page: pay any amount of test PHA to it.

`make cvm-rehearsal` runs this product CVM locally, with one deposit.

### Abnormal paths

The driver also plays the sandbox scenarios' abnormal payments against staging, each in a fresh
workspace, and checks the deposit state, the verified webhooks, and the product ledger
(architecture §7, §9, §15):

| Path | Options | Expected |
|---|---|---|
| underpayment | `--pay-bps 9700` | `credited` at spot for what arrived, then `swept`; the lock later expires |
| after the quote window | `--pay-after-expiry` | `quote.expired`, then `credited` at spot and `swept` |
| unsupported token | `--token T --until rejected` | once final, at the next reconciliation round, `rejected(unsupported_asset)`; the tokens stay in the forwarder; `TopupUnsupportedInflows` |
| refund | `--pay-bps N --until refunded --refund-to A` | a payment of N/10000 of the quote above `max_deposit_atomic` (200000 test PHA): `rejected(out_of_bounds)`, swept; once the deposit is final the driver requests a refund and waits while the treasury Safe's owners, as the merchant, pay it from the treasury of the deposit's address and attaches the transaction with `POST /v1/refunds/{id}/mark_paid`, until `succeeded` and one `deposit.refunded` |

Each row adds its options to the step-5 driver command: `T` is the Sepolia unsupported test token
`0x287E3577c66866a3F5Cb7a8Dac6761EB43608392`, `A` an address the staging owner controls, and the refund
row needs `--timeout 43200`. A mismatch between the expected and the observed outcome exits
non-zero with the reason.

## Website

`pay.phala.com` is the static build of [product/web](product/web), served by the Cloudflare Worker
`phala-pay-web` with static assets only (no Worker script) and deployed by **Cloudflare Workers
Builds**, connected to this repository, with Cloudflare's [`cf` CLI](https://github.com/cloudflare/cf)
pinned in [product/web/package.json](product/web/package.json). `main` deploys to production and
every other branch to its own [Worker Preview](https://developers.cloudflare.com/workers/previews/).
Each build runs once and its deploy only uploads it. Its dashboard build settings: root directory
`deploy/product/web` for both; for production the build command `npm run build:cloudflare` and the
deploy command `npm run deploy`; for the Previews Base the build command
`npm run build:cloudflare:preview` and the deploy command `npm run deploy:preview`. A branch's
Preview copies the Previews Base when it is created, so changing the Base leaves existing Previews
on their old settings until each is edited too.

- **Build.** `build:cloudflare` builds `sdk/js` (the page depends on it through `file:`) and then
  the page, each from its own lockfile with `npx -y pnpm@12.6.0`, on the Node of
  `product/web/.node-version` (24, as CI). The Cloudflare Vite plugin writes the page as cf's
  Build Output, in `product/web/.cloudflare/output`. The page's API origin is fixed at build time:
  `VITE_DEMO_API_ORIGIN` in `product/web/.env.production`, `https://pay-demo-api.phala.com`.
- **Production.** `deploy` runs `cf deploy --prebuilt`, which uploads that Build Output and
  deploys it. CI checks the config and the Build Output with `cf deploy --prebuilt --dry-run`.
- **Previews.** `build:cloudflare:preview` runs `build:cloudflare` as a Preview build
  (`CLOUDFLARE_PREVIEW_BUILD=true`, which `cf previews deploy --prebuilt` requires; the Build
  Output then records `isPreview` and leaves out the custom domain), and `deploy:preview` runs
  `cf previews deploy --prebuilt`, which uploads it and creates or updates the Preview named after
  the branch (`WORKERS_CI_BRANCH`, set by Workers Builds). Its Preview URL,
  `https://<branch slug>-phala-pay-web.phala-dev.workers.dev`, always serves the branch's latest
  build; each deployment also has its own URL. Its demo API calls are refused by CORS by design:
  the API allows only `https://pay.phala.com`.
- **[cloudflare.config.ts](product/web/cloudflare.config.ts).** The Worker's name and
  compatibility date; any path but the page and its assets is a real `404`
  (`notFoundHandling: "none"`); the custom domain `pay.phala.com`, for production only (cf rejects
  custom domains in a Preview, so the config leaves them out when `isPreview`); no `workers.dev`
  copy of the production site (`workersDev: false`); and Preview URLs on (`previewUrls: true`) for
  production as well as Previews: a branch's Preview gets `workers.dev` URLs only when the Worker
  itself has Preview URLs enabled (`cf previews deploy` returns none otherwise). The cost is a
  `workers.dev` URL per production version, which serves the same public page with `noindex`.
- **[public/_headers](product/web/public/_headers).** Cloudflare's static-assets headers, served in
  production and in every Preview: the page's CSP (`connect-src` names only the demo API and
  `pay-api-staging.phala.com`, whose public quote and deposit address views the SDK components
  read), `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `no-cache` for the
  page, a year's immutable caching for the content-hashed `/assets/*`, a day's caching for the
  fixed-name icons, manifest, link preview image, `robots.txt`, and `sitemap.xml`, and
  `X-Robots-Tag: noindex` on `workers.dev`. `vite preview` serves the Build Output in the Workers
  runtime with these headers, as the end-to-end tests do.
- **Moving from Wrangler.** The site was deployed with Wrangler until cf replaced it. The build
  settings above name scripts only this configuration has, so they were set just before merging
  the change that adds them, and its branch Preview was checked first; the merge then deploys
  `main` with cf.

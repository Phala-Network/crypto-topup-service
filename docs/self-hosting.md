# Self-hosting Phala Pay

Phala Pay is open-source, self-hosted software. An operator runs its own instance in its own
dstack confidential VM (CVM) on Phala Cloud, for its own merchants, each an account (`acct_…`)
that the operator creates. Phala runs an instance only for Phala Cloud and offers no hosted
service to third parties; its staging instance and the demo at [pay.phala.com](https://pay.phala.com/)
are Phala's own, on Sepolia and Base Sepolia. Nothing in an instance depends on Phala's: you build your own
images from your fork, deploy to your own Phala Cloud workspace, serve your own domain, and hold
your own admin key. The only shared piece is the forwarder factory, a permissionless contract at
one address on every chain.

This guide is the order of the steps, from nothing to a credited test deposit and on to
operations. The linked documents hold the detail and are the reference; where they and this guide
differ, they win. Steps marked **HUMAN-ONLY** change a registry, Phala Cloud, a CVM, a contract,
DNS, or a secret, and are run by a person from their own machine, never by CI or an agent.

| Role | Who | Reads |
|---|---|---|
| Operator | you: runs the instance, creates accounts, decides live access, handles incidents | this guide, [deploy/README.md](../deploy/README.md), [runbooks](../deploy/runbooks/README.md) |
| Merchant | each account: its keys, treasuries, webhook endpoints, sweeps, refunds | [integration.md](integration.md) |

An operator may also be a merchant of its own instance, as Phala is for Phala Cloud; it then does
the merchant's steps with the account's keys, never with the admin key.

## 1. Prerequisites

- **A Phala Cloud workspace** with an API key for each GitHub Environment you deploy
  (`production`, and optionally `staging`), capacity for a `tdx.medium` CVM each, and a node that
  offers the OS image `dstack-0.5.9` ([deploy/README.md, "OS image"](../deploy/README.md#os-image)).
- **An S3-compatible object store** for the encrypted WAL-G backups: a bucket or prefix per
  Environment that the other Environment's credentials cannot reach, and a read-write token for it.
  Cloudflare R2 needs no further settings; any other store also sets `AWS_REGION` and
  `AWS_S3_FORCE_PATH_STYLE`.
- **Two RPC providers per chain**, from different companies, over HTTPS: an endpoint serves one
  chain, so each chain of your routes has two of its own. Public gateways rate-limit; use paid
  plans for a mainnet. The chain must carry the canonical Multicall3
  ([deploy/contracts/multicall3.json](../deploy/contracts/multicall3.json)).
- **A domain in DNS you control** for the API, for example `pay-api.example.com`: a CNAME and a TXT
  record per instance, not proxied. It becomes your `TOPUP_DOMAIN` and your merchants' service
  URL, so choose one you will keep: changing it later changes every merchant's configuration.
- **A GitHub fork** of this repository, to run the Release images and Deploy workflows from its
  `main`.
- **On the operator's machine**: Node.js (for `npx phala@1.1.22`), Docker (the dstack verifier
  runs in it), `jq`, `curl`, OpenSSL, [uv](https://docs.astral.sh/uv/) (the Python SDK's key tools),
  the AWS CLI (to list backups), and Foundry v1.8.3 if a chain needs the factory deployed.
- **Optional: a Sentry project** for errors, alerts, and Crons monitors. Without a DSN the
  service reports nothing, and you have only `/healthz`, the admin API, and the chain.

## 2. Fork and configure GitHub

**HUMAN-ONLY, owner of the fork.**

1. **Fork** the repository and enable Actions on the fork (GitHub disables workflows on a new
   fork). Workflows run on `ubuntu-latest` unless the repository variable `CI_RUNNER` names a
   runner label.
2. **Your registry needs no setting.** [Release images](../.github/workflows/release-images.yml)
   publishes to the repository owner's GHCR namespace, `ghcr.io/<owner, lowercased>/…`, with the
   repository's `GITHUB_TOKEN`, and Deploy accepts only the images of a Release images run of the
   same repository. The packages must then be made public (section 4).
3. **Create the admin key** on the machine that will keep it, one per Environment. The seed
   never leaves that machine; the printed `public_key` is the Environment variable
   `TOPUP_ADMIN_PUBLIC_KEY`, and `admin/<Environment>-v1` is the key id Deploy derives:

   ```sh
   cd sdk/python
   uv run --locked topup-sdk keygen --keyid admin/production-v1 --seed-out ~/phala-pay/admin.seed
   ```

4. **Environments.** Settings > Environments: `production` and, for a pre-production instance
   with test routes only, `staging` (the only names Deploy offers). Deployment branches: `main`
   only. The only secret is `PHALA_CLOUD_API_KEY`, the workspace's API key.
5. **Environment variables**, per Environment:

   | Name | Value |
   |---|---|
   | `PHALA_WORKSPACE` | the display name of the API key's workspace |
   | `TOPUP_CVM_ID` | empty until the first provisioning |
   | `TOPUP_DOMAIN` | your API's domain, for example `pay-api.example.com` |
   | `AWS_ENDPOINT` | the object store's endpoint, for example `https://<account>.r2.cloudflarestorage.com` |
   | `WALG_S3_PREFIX` | `s3://BUCKET/PATH`, empty and used by no other app |
   | `TOPUP_ADMIN_PUBLIC_KEY` | step 3's `public_key` |
   | `TOPUP_RPC_PROVIDER_A_URL`, `TOPUP_RPC_PROVIDER_B_URL` | the committed Sepolia routes' two providers' URLs, with `{key}` in place of an API key; a route on another chain adds one `TOPUP_RPC_<ID>_URL` per provider it names (section 3) |
   | `TOPUP_RPC_BASE_SEPOLIA_A_URL`, `TOPUP_RPC_BASE_SEPOLIA_B_URL` | the committed Base Sepolia routes' two providers' URLs; the first must serve `eth_getLogs` over 2 000 blocks and with no contract address ([deploy/README.md, "RPC providers"](../deploy/README.md#rpc-providers)) |

   The meaning of each, the derived settings (`AWS_REGION`, `AWS_S3_FORCE_PATH_STYLE`,
   `TOPUP_ADMIN_KID`, `SENTRY_ENVIRONMENT`), and why all but the first two are attested are in
   [deploy/README.md, "One-time setup"](../deploy/README.md#one-time-setup-human-only-repository-owner)
   and ["Attested settings"](../deploy/README.md#attested-settings). The reference product's
   variables (`STAGING_PRODUCT_CVM_ID`, `PRODUCT_DOMAIN`, `PRODUCT_DRIVER_PUBLIC_KEY`) are for
   Phala's demo ([Phala's instance](../deploy/phala.md)) and not needed.
6. **The other workflows a fork inherits.** CI runs on pull requests and needs no settings.
   [Restore drill](../.github/workflows/restore-drill.yml) runs weekly on a local stack, with no
   secrets. [Verify contracts](../.github/workflows/verify-contracts.yml) runs daily in the
   `staging` Environment against Sepolia with its RPC URL variables as they are, so they must be
   keyless there, and checks the Safe of [safe-expectations.json](../deploy/contracts/safe-expectations.json), which
   is Phala's: record your own Safe there or disable the workflow. Release SDKs and API reference
   publish Phala's SDKs and reference; a fork needs neither.

## 3. Routes and contracts

A route is one chain and token that accounts quote on and are paid through, in one mode. Route
files are committed and attested: a new route is a pull request to your fork and a Deploy
`upgrade`, never a runtime setting.

- **The committed routes** are in test mode on Sepolia and Base Sepolia, and any instance can use
  them; keep them for a first instance in test mode:
  `phala-cloud-sepolia-pha-usd`
  ([deploy/config/routes/phala-cloud-sepolia-pha.yaml](../deploy/config/routes/phala-cloud-sepolia-pha.yaml)),
  a test PHA token (`MockERC20`, public `mint`), and `phala-cloud-sepolia-usdc-usd`
  ([deploy/config/routes/phala-cloud-sepolia-usdc.yaml](../deploy/config/routes/phala-cloud-sepolia-usdc.yaml)),
  Circle's testnet USDC ([faucet](https://faucet.circle.com)), priced as a stablecoin; and the
  same two tokens on Base Sepolia, `phala-cloud-base-sepolia-pha-usd` and
  `phala-cloud-base-sepolia-usdc-usd`, credited at the OP-stack `safe` head, with providers of
  their own ([deploy/phala.md, "Staging routes"](../deploy/phala.md#staging-routes)).
- **Your own routes.** The fields and their defaults are in
  [architecture §14](architecture.md#14-configuration-and-deployment), and
  [examples/phala-cloud-pha.yaml](../examples/phala-cloud-pha.yaml) is a mainnet example. A
  route `NAME.yaml` goes in `deploy/config/routes/`, its identical copy in the `configs` of
  [deploy/docker-compose.yml](../deploy/docker-compose.yml) as `topup_route_<NAME, - as _>`,
  mounted at `/etc/topup/routes/NAME.yaml` in the `topup` and `restore-check` services and passed
  in their `--route` arguments; [validate-compose.sh](../deploy/validate-compose.sh) checks the
  compose against the files, and [cvm-rehearsal.sh](../deploy/local/cvm-rehearsal.sh) names a local
  stand-in token for each route. `cargo run --locked -p topup -- route validate FILE` checks a file
  and `route show FILE` prints it resolved. Deploy refuses a live route on a test network, a test
  route on a mainnet,
  any live route in `staging`, and a chain that [check-route-modes.sh](../deploy/check-route-modes.sh)
  does not list; add a chain there, and to
  [networks.json](../deploy/contracts/networks.json) for the contract scripts, after review. A
  chain without a Chainalysis sanctions oracle needs `chain.sanctions_oracle`.
- **Its RPC providers.** A route names its chain's providers by id, `chain.rpc_providers:
  [alchemy-base-sepolia, drpc-base-sepolia]` (lowercase letters, digits, `-`); one that names none
  uses `provider-a` and `provider-b`, the Sepolia routes'. Each id is listed once in the
  `x-rpc-providers` block of the compose: its URL, the Environment variable `TOPUP_RPC_<ID>_URL`
  (the id upper-cased, `-` as `_`), and, for a provider that puts an API key in its URL, the
  sealed `TOPUP_RPC_<ID>_KEY`, which also goes in
  [staging.env.example](../deploy/staging.env.example) and the `allowed_envs` of
  [app-compose.example.json](../deploy/app-compose.example.json). A provider serves one chain, so
  another chain's route names providers of its own; preflight checks that each reports the chain
  of every route naming it ([deploy/README.md, "RPC providers"](../deploy/README.md#rpc-providers)).
- **The contracts.** The `ForwarderFactory` has no owner, no roles, and no admin, and is deployed
  deterministically through the Arachnid proxy at `0x45466D37587E6E46DC35eB96b74ba3D3b1E5b747`,
  with its implementation at `0x49F2F1F1a25269Ea0C6FF2AB1C7B09dCBE9c5bA9`, on every chain. Reuse it;
  `topup run` refuses to start unless the chain holds exactly that code. Check a chain with the
  read-only `deploy/contracts/verify-deployment.sh --rpc NETWORK/a=URL_A --rpc NETWORK/b=URL_B`
  (`NETWORK` from `networks.json`, the URLs with their keys).
  Only where it is missing, deploy it (**HUMAN-ONLY**, a funded throwaway EOA) as in
  [deploy/CONTRACTS.md](../deploy/CONTRACTS.md); if anyone deployed it first, the broadcast sends
  nothing. It is deployed on Sepolia and Base Sepolia; Phala deploys it on mainnet after the
  contracts' independent security review ([plan](plan.md)), which an operator going live before
  then should weigh.

## 4. Release and provision

1. **Build the images.** Run **Release images** on `main` (`gh workflow run release-images.yml
   --ref main`) and note its run id. It pushes `phala-pay`, `postgres-walg`, and
   `phala-pay-reference-product` by digest; `phala-pay` builds bit for bit twice, so anyone can
   rebuild the commit and compare. **HUMAN-ONLY, once:** make the three packages public (package
   settings; irreversible): CVMs pull without credentials, and preflight refuses a private image.
2. **Provision.** Run **Deploy** with `environment: production`, `target: topup`,
   `mode: provision`, and the release run id. It runs preflight, creates the CVM with Phala
   Cloud's KMS, verifies the attested compose, and prints the CVM id, the DNS records, and the
   sealing commands in its summary. Set `TOPUP_CVM_ID` to the CVM id; if a later step of the run
   fails, set it anyway and continue with `upgrade`, never provision twice
   ([deploy/README.md, "Deploy"](../deploy/README.md#deploy)).
3. **Seal the secrets** (**HUMAN-ONLY**). The CVM waits for them: PostgreSQL initializes only
   after it can list the empty backup prefix. Write `.env.production` (mode 0600) with exactly the
   names of [staging.env.example](../deploy/staging.env.example): `AWS_ACCESS_KEY_ID`,
   `AWS_SECRET_ACCESS_KEY`, `SENTRY_DSN` (may be empty), and each provider's `TOPUP_RPC_<ID>_KEY`
   (`TOPUP_RPC_PROVIDER_A_KEY`, `TOPUP_RPC_PROVIDER_B_KEY`; empty for a keyless URL). From a
   checkout of the deployed commit,
   with the rendered compose from the run's artifact and `PHALA_CLOUD_API_KEY` exported:

   ```sh
   deploy/preflight.sh --env .env.production --compose docker-compose.production.yml --offline
   npx --yes phala@1.1.22 envs update "$TOPUP_CVM_ID" -e .env.production
   ```

   [Sealing the secrets](../deploy/README.md#sealing-the-secrets) has the rules; never change a
   setting this way.
4. **Create the DNS records** (**HUMAN-ONLY**) the summary lists: a CNAME from `$TOPUP_DOMAIN` to
   the node's gateway and a TXT `_dstack-app-address.$TOPUP_DOMAIN` naming the instance, not
   proxied. dstack-ingress in the CVM then obtains a Let's Encrypt certificate itself, through
   port 443 (`tls-alpn-01`); the CVM holds no DNS credentials
   ([Custom domain](../deploy/README.md#custom-domain)).
5. **Upgrade once with the same release.** Run Deploy with `mode: upgrade`: it waits for
   `https://$TOPUP_DOMAIN/healthz` and verifies the attestation and the certificate evidence.
   Backups have started once a WAL segment younger than two minutes is listed:

   ```sh
   aws s3 ls "${WALG_S3_PREFIX%/}/wal_005/" --endpoint-url "$AWS_ENDPOINT" | tail -1
   ```

6. **Sentry and egress** (**HUMAN-ONLY**), if you use them: the scrubbing settings, an alert, and
   an Uptime monitor on `/healthz` ([One-time setup](../deploy/README.md#one-time-setup-human-only-repository-owner),
   step 5); and outbound traffic restricted to the RPC hosts, price sources, object store, Sentry,
   DNS, the Phala Cloud platform, and public addresses on 443 and 80 for webhooks
   ([Attestation, ingress, and egress](../deploy/README.md#attestation-ingress-and-egress)).

## 5. Verify the attestation

**HUMAN-ONLY, verifier**, before creating any account, from a checkout of the deployed commit with
the run's rendered compose and `PHALA_CLOUD_API_KEY` exported. Deploy already did this; doing it
yourself is what makes it evidence:

```sh
export CVM_ID=$TOPUP_CVM_ID
npx --yes phala@1.1.22 cvms get "$CVM_ID" --json > cvm.json
npx --yes phala@1.1.22 cvms attestation "$CVM_ID" --json > attestation.json
APP_ID=$(jq -er '.app_id' cvm.json) && GATEWAY_DOMAIN=$(jq -er '.gateway.base_domain' cvm.json)
curl -fsS "https://${APP_ID#0x}-8090.$GATEWAY_DOMAIN/prpc/Info" > info.json
deploy/verify-attestation.sh attestation.json info.json "$APP_ID" docker-compose.production.yml
deploy/verify-ingress-evidence.sh "$TOPUP_DOMAIN" "$APP_ID"
```

The official dstack verifier checks the TDX quote, TCB, event log, and OS image; the script then
requires the app id, the compose hash of exactly the rendered compose, the sealed env names, and
`dstack-ingress` on 443 as the only published port
([deploy/README.md](../deploy/README.md#attestation-ingress-and-egress)). Give your merchants the
app id and the compose hash (`jq -j '.compose_file' attestation.json | sha256sum`): they check
their webhook keys' attestation against them ([integration.md §5.3](integration.md#53-pin-your-accounts-webhook-keys)),
and each upgrade changes the compose hash.

## 6. Onboard your first account

Accounts are created only by the operator, through the admin API; there is no signup. Due
diligence is done offline; the service records only its reference, date, and reviewer.

1. **Admin helper.** Convert the seed to the PEM the signer uses, once, and load the
   [runbooks' `admin` helper](../deploy/runbooks/README.md#environment) with
   `BASE_URL=https://$TOPUP_DOMAIN` (exactly `TOPUP_PUBLIC_ORIGIN`, or signatures answer `401`) and
   `ADMIN_KEY_ID` equal to `TOPUP_ADMIN_KID` (`admin/production-v1`):

   ```sh
   (umask 077 && { printf '302e020100300506032b657004220420'; tr -d '\n' < admin.seed; } |
     xxd -r -p | openssl pkey -inform DER -out admin.pem)
   ```

2. **Create the account** (**HUMAN-ONLY**, admin key holder) with `charges_enabled: false`; the
   answer holds the `acct_…` id and its first secret test key, shown only there
   ([Operator onboarding](../deploy/README.md#operator-onboarding), step 2).
3. **Hand over the key** through an encrypted channel to the recorded contact, and delete the
   answer. The merchant rolls it at once.

Live mode (`charges_enabled: true`, which returns the first live key), the per-account exposure cap
`max_unfinalized_credit`, pauses, and recovery keys are later admin calls in the same section.

## 7. The merchant's side

Each merchant does these steps with its own secret key, against your `https://$TOPUP_DOMAIN`; the
[integration guide](integration.md) is its reference, and [deploy/README.md, "Merchant
setup"](../deploy/README.md#merchant-setup) the summary.

1. **Keys**: roll the first key, keep secret keys offline for administration, and run servers with
   a restricted key ([integration.md §5.4](integration.md#54-manage-and-roll-keys)).
2. **Webhook keys**: fetch `GET /v1/attestation?nonce=…` with a key of the mode, verify it against
   the app id and compose hash you gave it, and pin the account's public webhook keys
   ([§5.3](integration.md#53-pin-your-accounts-webhook-keys)).
3. **Treasury proof**, per chain and mode: an EOA signs an EIP-4361 challenge
   (`pay.treasuries.set_eoa(…)` in the Python SDK, or
   [deploy/sandbox/set-treasury.sh](../deploy/sandbox/set-treasury.sh)); a Safe, deployed on the
   chain with the `CompatibilityFallbackHandler`, signs it as a Safe message
   ([§1.6](integration.md#16-treasuries)). A test-mode treasury applies at once; a later live change
   waits 48 hours.
4. **Webhook endpoint**: `POST /v1/webhook_endpoints` with the receiver's HTTPS URL, then
   `POST /v1/webhook_endpoints/{id}/test` ([§5.11](integration.md#511-webhook-endpoints-and-events)).
5. **Pins**: the account id, the factory and implementation, and the treasury of each chain,
   configured in the merchant's server; the SDKs recompute every address from them.

## 8. A first credited test deposit

With the committed Sepolia route, the account's test key, a Sepolia treasury, and a public HTTPS
URL for the webhook receiver (a tunnel will do), the reference product runs a merchant backend and
one deposit end to end: it creates a quote, pays it from a Foundry keystore holding Sepolia ETH
(minting the test token), and waits for the verified `deposit.credited` webhook, about 30 seconds
after the payment. Write the configuration and run it as in
[deploy/sandbox/README.md, "Running the scenarios against a deployed service"](../deploy/sandbox/README.md#running-the-scenarios-against-a-deployed-service),
with `service_url` set to `https://$TOPUP_DOMAIN`:

```sh
uv run --locked --project sdk/python python deploy/sandbox/smoke.py --config sandbox.json
PYTHONPATH=deploy/product uv run --locked --project sdk/python python -m reference_product --config sandbox.json
```

The deposit is then `credited` in `GET /v1/deposits`, final about 15 minutes later, and swept when
the merchant flushes its forwarders ([Sweeping](../deploy/README.md#sweeping)). The same
directory's scenarios play late, partial, rejected, and refunded payments.

## 9. Going live

1. A reviewed route PR with the live route, the factory verified on its chain, and the provider
   URLs of that chain; then Deploy `upgrade` ([Deploy](../deploy/README.md#deploy)).
2. Your own sign-off of the limits: route bounds, each account's caps, and
   `max_unfinalized_credit` ([architecture §17](architecture.md#17-delivery) lists Phala's), and a
   passed restore drill (section 10).
3. `charges_enabled: true` for each account you enable; the merchant then proves a live
   treasury and follows the [go-live checklist](integration.md#44-go-live-checklist).

## 10. Backups and restore

The CVM backs itself up: PostgreSQL archives WAL every minute and WAL-G takes base backups into
`WALG_S3_PREFIX`, encrypted with a key derived in the CVM from the app id. The same app id
derives the same key and database passwords, so a restore needs no secret; a new app can never
read the old app's backups, so never delete the app, and give a new app a new prefix. The
`topup-backup` Crons monitor alerts when WAL-G has not uploaded for over two minutes
([Backup age](../deploy/runbooks/backup-age.md)).

- **Restore**: a new instance of the same app boots the read-only restore-check variant, which
  fetches the newest backup, replays WAL, and verifies the result; you then resume it as the
  service. The service starts frozen until you have reconciled with every merchant
  ([RESTORE.md](../deploy/RESTORE.md), [Reconciliation after a restore](../deploy/runbooks/restore.md),
  [the merchant notice](../deploy/README.md#after-a-restore-the-merchant-notice)).
- **Drills**: `make restore-drill` locally (and weekly in CI), and a drill against your own
  backups in a throwaway instance ([Staging restore drill](../deploy/RESTORE.md#staging-restore-drill)).

## 11. Upgrades and operations

- **Upgrades.** Merge upstream changes into your fork's `main` by pull request and review them:
  each changes the attested compose. Then Release images and Deploy `upgrade`. An upgrade sends
  only the compose, so the sealed secrets stay; rollback is an upgrade to an earlier release, and
  a schema is never rolled back ([Deploy](../deploy/README.md#deploy)). Tell merchants the new
  compose hash. The OS image is fixed; moving to dstack 0.6 changes every derived key
  ([OS image](../deploy/README.md#os-image)).
- **Operations.** A production CVM has no SSH, logs, or database access: you work through Sentry,
  the admin API (daily report, deposit view, pauses, metrics), and the chain. Every alert names its
  runbook ([runbooks](../deploy/runbooks/README.md#alert-and-symptom-index)); RPC usage and cost
  are in [Measuring RPC usage](../deploy/README.md#measuring-rpc-usage), and communication in
  [Incident communication](../deploy/runbooks/incident-communication.md).
- **Security.** Report vulnerabilities in the software as in [SECURITY.md](../SECURITY.md);
  incidents of your instance are yours to handle and disclose.
- **Local rehearsal.** `make up`, `make cvm-rehearsal` (the release artifact against Anvil and the
  dstack simulator, through sealing, onboarding, and one credited deposit), and
  `make sandbox-local` run the same compose without Phala Cloud
  ([Local verification](../deploy/README.md#local-verification)).

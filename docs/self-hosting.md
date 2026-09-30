# Self-hosting Phala Pay

Phala Pay is open-source, self-hosted software. An operator runs its own instance in its own
dstack confidential VM (CVM) on Phala Cloud, for its own merchants, each an account (`acct_…`)
that the operator creates. Phala runs an instance only for Phala Cloud and offers no hosted
service to third parties; its staging instance and the demo at [pay.phala.com](https://pay.phala.com/)
are Phala's own, on Sepolia and Base Sepolia. Nothing in an instance depends on Phala's: you deploy
a verified [release](https://github.com/Phala-Network/phala-pay/releases) of the software from a
repository of your own that holds only your settings, to your own Phala Cloud workspace, serve your
own domain, and hold your own admin key. There is no fork to maintain and no image to build. The
only shared piece is the forwarder factory, a permissionless contract at one address on every
chain.

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
  The environment directory sets its endpoint, region, and addressing (R2: `auto` and path-style).
- **Two RPC providers per chain**, from different companies, over HTTPS: an endpoint serves one
  chain, so each chain of your routes has two of its own. Public gateways rate-limit; use paid
  plans for a mainnet. The chain must carry the canonical Multicall3
  ([deploy/contracts/multicall3.json](../deploy/contracts/multicall3.json)).
- **A domain in DNS you control** for the API, for example `pay-api.example.com`: a CNAME and a TXT
  record per instance, not proxied. It becomes your `public_origin` and your merchants' service
  URL, so choose one you will keep: changing it later changes every merchant's configuration.
- **A GitHub repository of your own**, private or public, for your environment directory and a
  deploy workflow (section 2).
- **On the operator's machine**: the [GitHub CLI](https://cli.github.com/) 2.101.0, the version
  Deploy pins (to download and verify releases), Node.js and npm (the kit's locked Phala Cloud CLI,
  `kit/deploy/phala`), Docker (the dstack verifier and
  `topup config check` run in it), `jq`, `curl`, OpenSSL, [uv](https://docs.astral.sh/uv/) (the
  Python SDK's key tools), the AWS CLI (to list backups), and Foundry v1.8.3 (`cast`, for
  preflight's chain checks, and `forge` if a chain needs the factory deployed).
- **Optional: a Sentry project** for errors, alerts, and Crons monitors. Without a DSN the
  service reports nothing, and you have only `/healthz`, the admin API, and the chain.

## 2. Your environment repository

**HUMAN-ONLY, owner of the repository.** It holds your settings and nothing else; the software
comes from a release.

1. **Verify a release** ([Verify a release](#verify-a-release)), the latest `v<version>` of the
   [releases](https://github.com/Phala-Network/phala-pay/releases), and extract its deploy kit next
   to your repository, with its locked Phala Cloud CLI:

   ```sh
   mkdir kit && tar -xzf release/phala-pay-deploy-v0.3.0.tar.gz -C kit --strip-components=1
   npm ci --prefix kit/deploy/tools --ignore-scripts
   ```

   The kit is `LICENSE`, `deploy/`, and `docs/` of the release: the attested stack, `render.sh`,
   the policy, preflight, the verifiers, the example environment, and this guide.
2. **Create the admin key** on the machine that will keep it, one per Environment. The seed
   never leaves that machine; the printed `public_key` and the key id go into the environment
   directory's `topup.yaml` (step 4):

   ```sh
   uvx --from phala-pay topup-sdk keygen --keyid admin/production-v1 --seed-out ~/phala-pay/admin.seed
   ```

3. **Environments.** In your repository's Settings > Environments: `production` and, for a
   pre-production instance with test routes only, `staging` (the only names Deploy accepts).
   Deployment branches: `main` only. The only secret is `PHALA_CLOUD_API_KEY`, the workspace's
   API key. Workflows run on `ubuntu-latest` unless the repository variable `CI_RUNNER` names a
   runner label.
4. **The environment directory**, per Environment and target: copy the kit's
   `deploy/environments/example/topup` to `<Environment>/topup/` in your repository, fill it in,
   and commit it by pull request. Preflight refuses the example's placeholders.

   | File | Settings |
   |---|---|
   | `topup.yaml` ([reference](configuration.md#the-configuration-file)) | `environment` (the Sentry environment), `public_origin` (`https://` + your domain), `admin_key` (step 2), `rpc_providers` (each chain's two providers' URLs, with `{key}` in place of an API key; the first must serve `eth_getLogs` over 2 000 blocks and with no contract address, [deploy/README.md, "RPC providers"](../deploy/README.md#rpc-providers)), and `routes` (section 3) |
   | `compose.yaml` | `WALG_S3_PREFIX` (`s3://BUCKET/PATH`, empty and used by no other app), `AWS_ENDPOINT`, `AWS_REGION`, `AWS_S3_FORCE_PATH_STYLE`, dstack-ingress's `DOMAIN` (your domain), and one `TOPUP_RPC_<ID>_KEY` line per keyed provider |

   The kit renders it and the release's image checks it, so you can check it before committing:

   ```sh
   kit/deploy/render.sh --images images.json --gateway-domain gateway.example.net production/topup >/dev/null
   docker run --rm -i "$(jq -r '."phala-pay"' images.json)" topup config check /dev/stdin <production/topup/topup.yaml
   ```

5. **The deploy workflow**, `.github/workflows/deploy.yml` in your repository. It calls the
   release's [Deploy](../.github/workflows/deploy.yml) at the release, which verifies the release
   and runs the kit's scripts on your environment directory
   ([deploy/README.md, "Deploy"](../deploy/README.md#deploy)). Pin it by the release's commit SHA,
   as GitHub recommends for third-party workflows (`gh api
   repos/Phala-Network/phala-pay/commits/v0.3.0 --jq .sha`); Deploy refuses to run at any commit
   but `version`'s. It reads only the Environment secret `PHALA_CLOUD_API_KEY`, so pass no
   secrets:

   ```yaml
   name: Deploy
   on:
     workflow_dispatch:
       inputs:
         environment:
           type: choice
           options: [staging, production]
           required: true
         mode:
           type: choice
           options: [provision, upgrade]
           required: true
   permissions:
     contents: read
     attestations: read
   jobs:
     deploy:
       uses: Phala-Network/phala-pay/.github/workflows/deploy.yml@<the v0.3.0 commit SHA> # v0.3.0
       with:
         version: v0.3.0
         environment: ${{ inputs.environment }}
         mode: ${{ inputs.mode }}
         environment_dir: ${{ inputs.environment }}/topup
   ```

   The Environment's variables are only deployment state: `PHALA_WORKSPACE` (the display name of
   the API key's workspace) and `TOPUP_CVM_ID` (empty until the first provisioning). Why every
   other setting is committed and attested is in
   [deploy/README.md, "Attested settings"](../deploy/README.md#attested-settings). Another CI
   system, or none, can run the same steps: they are the kit's commands, in the order Deploy runs
   them.

Your repository needs no other workflow. The contract and restore checks run in Phala Pay's own
repository ([Verify contracts](../.github/workflows/verify-contracts.yml) daily, the local
[Restore drill](../.github/workflows/restore-drill.yml) weekly); run the kit's
`deploy/contracts/verify-deployment.sh` against your providers whenever you add a chain.

## 3. Routes and contracts

A route is one chain and token that accounts quote on and are paid through, in one mode. Routes
are committed and attested: a new route is a pull request to your environment repository and a
Deploy `upgrade`, never a runtime setting.

- **The routes of Phala's staging** are in test mode on Sepolia and Base Sepolia, and any instance
  can copy them from its
  [topup.yaml](../deploy/environments/phala-network/staging/topup/topup.yaml) for a first
  instance in test mode. They are:
  - `phala-cloud-sepolia-pha-usd`, a test PHA token (`MockERC20`, public `mint`);
  - `phala-cloud-sepolia-usdc-usd`, Circle's testnet USDC ([faucet](https://faucet.circle.com)),
    priced as a stablecoin;
  - `phala-cloud-base-sepolia-pha-usd` and `phala-cloud-base-sepolia-usdc-usd`, the same two
    tokens on Base Sepolia, credited at the OP-stack `safe` head, with providers of their own
    ([deploy/phala.md, "Staging routes"](../deploy/phala.md#staging-routes)).
- **Your own routes** are items of `topup.yaml`'s `routes`, written as route files are. The fields
  and their defaults are in [architecture §14](architecture.md#14-configuration-and-deployment),
  and [examples/phala-cloud-pha.yaml](../examples/phala-cloud-pha.yaml) is a mainnet example.
  `topup config check FILE` in the release's image checks the file (section 2), and
  `config show FILE` prints it resolved.
- **What Deploy refuses:** a live route on a test network, a test route on a mainnet, any live
  route in `staging`, and a chain that [check-route-modes.sh](../deploy/check-route-modes.sh) does
  not list. A chain is added there, and to [networks.json](../deploy/contracts/networks.json) for
  the contract scripts, by a pull request to Phala Pay and ships in its next release. A chain without a Chainalysis sanctions oracle needs
  `chain.sanctions_oracle`.
- **Its RPC providers.** A route names its chain's providers by id, `chain.rpc_providers:
  [alchemy-base-sepolia, drpc-base-sepolia]` (lowercase letters, digits, `-`); one that names none
  uses `provider-a` and `provider-b`, the Sepolia routes'. `rpc_providers` gives each id its URL,
  with `{key}` as a whole path segment or query value for a provider that puts an API key in its
  URL. The key itself is sealed as `TOPUP_RPC_<ID>_KEY` (the id upper-cased, `-` as `_`) and
  declared in the directory's `compose.yaml`. A provider serves one chain, so another chain's route
  names providers of its own. `topup config check` enforces these rules, and preflight checks that
  each provider reports the chain of every route naming it
  ([deploy/README.md, "RPC providers"](../deploy/README.md#rpc-providers)).
- **The contracts.** The `ForwarderFactory` has no owner, no roles, and no admin, and is deployed
  deterministically through the Arachnid proxy at `0x45466D37587E6E46DC35eB96b74ba3D3b1E5b747`,
  with its implementation at `0x49F2F1F1a25269Ea0C6FF2AB1C7B09dCBE9c5bA9`, on every chain. Reuse it;
  `topup run` refuses to start unless the chain holds exactly that code. Check a chain with the
  kit's read-only `deploy/contracts/verify-deployment.sh --rpc NETWORK/a=URL_A --rpc NETWORK/b=URL_B`
  (`NETWORK` from `networks.json`, the URLs with their keys), which compares it with the release's
  reference deployment.
  Only where it is missing, deploy it (**HUMAN-ONLY**, a funded throwaway EOA) as in
  [deploy/CONTRACTS.md](../deploy/CONTRACTS.md); if anyone deployed it first, the broadcast sends
  nothing. It is deployed on Sepolia and Base Sepolia; Phala deploys it on mainnet after the
  contracts' independent security review ([plan](plan.md)), which an operator going live before
  then should weigh.

## 4. Release and provision

1. **Pick the release.** Your workflow's `uses: …@<commit> # v<version>` and `version` name it (section 2, step 5);
   its images are Phala's, public on GHCR, and each run verifies them again. Read its notes and
   [verify it](#verify-a-release) yourself once.
2. **Provision.** Run your Deploy workflow with `environment: production` and
   `mode: provision`. It runs preflight, creates the CVM with Phala Cloud's KMS, verifies the
   attested compose, and prints the CVM id, the DNS records, and the sealing commands in its
   summary. Set `TOPUP_CVM_ID` to the CVM id; if a later step of the run fails, set it anyway and
   continue with `upgrade`, never provision twice
   ([deploy/README.md, "Deploy"](../deploy/README.md#deploy)).
3. **Seal the secrets** (**HUMAN-ONLY**). The CVM waits for them: PostgreSQL initializes only
   after it can list the empty backup prefix. Write `.env.production` (mode 0600) with exactly the
   rendered compose's sealed names, which the provision summary lists: `AWS_ACCESS_KEY_ID`,
   `AWS_SECRET_ACCESS_KEY`, `SENTRY_DSN` (may be empty), and each `TOPUP_RPC_<ID>_KEY` your
   directory declares. With the release's kit, your repository at the deployed commit, the
   rendered compose from the run's artifact, and `PHALA_CLOUD_API_KEY` exported:

   ```sh
   docker pull <the compose's phala-pay image>   # preflight --offline checks the configuration in it
   kit/deploy/preflight.sh --env .env.production --compose docker-compose.production.yml \
     --environment-dir production/topup --offline
   kit/deploy/phala envs update "$TOPUP_CVM_ID" -e .env.production
   ```

   [Sealing the secrets](../deploy/README.md#sealing-the-secrets) has the rules; never change a
   setting this way.
4. **Create the DNS records** (**HUMAN-ONLY**) the summary lists: a CNAME from your domain to
   the node's gateway and a TXT `_dstack-app-address.<domain>` naming the instance, not
   proxied. dstack-ingress in the CVM then obtains a Let's Encrypt certificate itself, through
   port 443 (`tls-alpn-01`); the CVM holds no DNS credentials
   ([Custom domain](../deploy/README.md#custom-domain)).
5. **Upgrade once with the same release.** Run Deploy with `mode: upgrade`: it waits for
   `<public_origin>/healthz` and verifies the attestation and the certificate evidence.
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

**HUMAN-ONLY, verifier**, before creating any account, with the deployed release's verified kit,
the run's rendered compose, and `PHALA_CLOUD_API_KEY` exported. Deploy already did this; doing it
yourself is what makes it evidence:

```sh
export CVM_ID=$TOPUP_CVM_ID
kit/deploy/phala cvms get "$CVM_ID" --json > cvm.json
kit/deploy/phala cvms attestation "$CVM_ID" --json > attestation.json
APP_ID=$(jq -er '.app_id' cvm.json) && GATEWAY_DOMAIN=$(jq -er '.gateway.base_domain' cvm.json)
curl -fsS "https://${APP_ID#0x}-8090.$GATEWAY_DOMAIN/prpc/Info" > info.json
kit/deploy/verify-attestation.sh attestation.json info.json "$APP_ID" docker-compose.production.yml service
kit/deploy/verify-ingress-evidence.sh "<your domain>" "$APP_ID"
```

The official dstack verifier checks the TDX quote, TCB, event log, and OS image; the script then
requires the app id, the compose hash of exactly the rendered compose, the sealed env names, and
the policy of [compose-policy.jq](../deploy/compose-policy.jq), under which `dstack-ingress` on 443
is the only published port ([deploy/README.md](../deploy/README.md#attestation-ingress-and-egress)). Give your merchants the
app id and the compose hash (`jq -j '.compose_file' attestation.json | sha256sum`): they check
their webhook keys' attestation against them ([integration.md §5.3](integration.md#53-pin-your-accounts-webhook-keys)),
and each upgrade changes the compose hash.

## 6. Onboard your first account

Accounts are created only by the operator, through the admin API; there is no signup. Due
diligence is done offline; the service records only its reference, date, and reviewer.

1. **Admin helper.** Convert the seed to the PEM the signer uses, once, and load the
   [runbooks' `admin` helper](../deploy/runbooks/README.md#environment), in the kit's directory,
   with `BASE_URL` set to exactly `topup.yaml`'s `public_origin` (or signatures answer `401`) and
   `ADMIN_KEY_ID` to its `admin_key.id` (`admin/production-v1`):

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

Each merchant does these steps with its own secret key, against your `public_origin`; the
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
URL for the webhook receiver (a tunnel will do), the reference product, from a clone of Phala Pay at
the release's tag, runs a merchant backend and one deposit end to end: it creates a quote, pays it from a Foundry keystore holding Sepolia ETH
(minting the test token), and waits for the verified `deposit.credited` webhook, about 30 seconds
after the payment. Write the configuration and run it as in
[deploy/sandbox/README.md, "Running the scenarios against a deployed service"](../deploy/sandbox/README.md#running-the-scenarios-against-a-deployed-service),
with `service_url` set to your `public_origin`:

```sh
uv run --locked --project sdk/python python deploy/sandbox/smoke.py --config sandbox.json
PYTHONPATH=deploy/product uv run --locked --project sdk/python python -m reference_product --config sandbox.json
```

The deposit is then `credited` in `GET /v1/deposits`, final about 15 minutes later, and swept when
the merchant flushes its forwarders ([Sweeping](../deploy/README.md#sweeping)). The same
directory's scenarios play late, partial, rejected, and refunded payments.

## 9. Going live

1. A reviewed pull request to your environment repository adding the live route and its chain's
   providers to `topup.yaml`, with the factory verified on its chain; then Deploy `upgrade`
   ([Deploy](../deploy/README.md#deploy)).
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

- **Upgrades.** A new release is a pull request to your repository that changes the release in your
  workflow, in both places (the `uses:` commit and `version`). Review its notes and what it changes
  in the attested compose (`git diff v0.3.0 v0.4.0 -- deploy/` in a clone of Phala Pay, or render
  your directory with both kits and diff), [verify it](#verify-a-release), merge, and run Deploy
  `upgrade`. An upgrade sends only the compose, so the sealed secrets stay; rollback is an upgrade
  to an earlier release, and a schema is never rolled back ([Deploy](../deploy/README.md#deploy)).
  Tell merchants the new compose hash. The OS image is fixed; moving to dstack 0.6 changes every
  derived key ([OS image](../deploy/README.md#os-image)).
- **Operations.** A production CVM has no SSH, logs, or database access: you work through Sentry,
  the admin API (daily report, deposit view, pauses, metrics), and the chain. Every alert names its
  runbook ([runbooks](../deploy/runbooks/README.md#alert-and-symptom-index)); RPC usage and cost
  are in [Measuring RPC usage](../deploy/README.md#measuring-rpc-usage), and communication in
  [Incident communication](../deploy/runbooks/incident-communication.md).
- **Security.** Report vulnerabilities in the software as in [SECURITY.md](../SECURITY.md);
  incidents of your instance are yours to handle and disclose.
- **Local rehearsal**, from a clone of Phala Pay. `make up`, `make cvm-rehearsal` (a staging-shaped artifact against Anvil and
  the dstack simulator, through sealing, onboarding, and one credited deposit),
  `make upgrade-rehearsal` (an upgrade in place from a release, with real data, and its rollback),
  and `make sandbox-local` run the same compose without Phala Cloud
  ([Local verification](../deploy/README.md#local-verification)).

## Verify a release

[deploy/verify-release.sh](../deploy/verify-release.sh) is the verification Deploy runs on every
deployment; run the same program before you adopt a release. It downloads the release's assets
into a directory and stops at the first failure:

1. the tag's commit must be in Phala Pay's `main` history;
2. the assets must match `SHA256SUMS`;
3. every asset (`images.json`, the kit, `phala-cloud-template.yml`) and every image `images.json`
   names must have a GitHub build provenance attestation signed by
   [release.yml](../.github/workflows/release.yml) at the tag, on a GitHub-hosted runner, for that
   commit (`gh attestation verify --source-digest`).

```sh
gh api -H 'Accept: application/vnd.github.raw' \
  'repos/Phala-Network/phala-pay/contents/deploy/verify-release.sh?ref=v0.3.0' >verify-release.sh
bash verify-release.sh v0.3.0 release     # prints the release's commit
```

**Rebuild instead of trusting the build.** From a clone at the tag (`git clone --branch v0.3.0
--recurse-submodules https://github.com/Phala-Network/phala-pay.git`), with Buildx v0.37.1:

- `phala-pay` and `phala-pay-reference-product` are reproducible: `make verify-image` builds
  `phala-pay` twice on the release's pinned BuildKit, with the tag's commit time, and prints its
  manifest digest, which equals `images.json`'s
  (`DOCKERFILE=deploy/Dockerfile.reference-product deploy/verify-image.sh` for the other).
- `postgres-walg` is not reproducible (apt and dpkg timestamps): it has provenance only. Review
  its [Dockerfile](../deploy/Dockerfile.postgres-walg) at the tag.
- The kit's tar is `git archive` of the tag's `LICENSE`, `deploy/`, and `docs/`:
  `gzip -dc phala-pay-deploy-v0.3.0.tar.gz` equals
  `git archive --prefix=phala-pay-deploy-v0.3.0/ v0.3.0 -- LICENSE deploy docs`.

## The Phala Cloud template

Phala Cloud's [Phala Pay template](https://cloud.phala.com/templates/phala-pay) is a one-click
**testnet quick start**: the release's `phala-cloud-template.yml`, rendered by
`deploy/render.sh --template` from
[deploy/environments/phala-cloud-template](../deploy/environments/phala-cloud-template/topup)
([deploy/README.md, "The Phala Cloud template variant"](../deploy/README.md#the-phala-cloud-template-variant)).
It serves Phala's staging routes at the app's own gateway domain, with no custom domain or
dstack-ingress. Its deploy form cannot put values in the attested compose, so five values come
from the CVM's env and are outside the attestation: the admin public key
(`TOPUP_ADMIN_PUBLIC_KEY`), the public origin's host (`DSTACK_APP_DOMAIN`, from Phala Cloud's
reviewed pre-launch script), and the backup location (`WALG_S3_PREFIX`, `AWS_ENDPOINT`,
`AWS_REGION`). Whoever controls the workspace can change them without changing the compose hash.
topup and postgres-walg parse each strictly at startup and refuse to start otherwise, and the
policy allows a runtime value in no other place. A template instance also has no restore-check
path: its restore guarantees would rest on those unattested values
([deploy/README.md](../deploy/README.md#the-phala-cloud-template-variant)). For an instance with
merchants, deploy as this guide describes, where every one of them is attested.

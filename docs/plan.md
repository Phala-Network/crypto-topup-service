# Plan to production

What remains before Phala Pay takes live payments, first for Phala Cloud's account on Ethereum
Mainnet, and who owns each item. The decisions are the [design](design/multi-tenant.md) (§16 is
its PR plan); the specification is [architecture.md](architecture.md); merchants read
[integration.md](integration.md); operators read [deploy/README.md](../deploy/README.md) and the
[runbooks](../deploy/runbooks/README.md). Completed work is in git history and the changelogs.

## Where things stand

- **Service**: an API-only, multi-tenant service in Stripe's shape. The operator creates accounts
  through the admin API; merchants manage API keys (secret and restricted), treasuries (proven,
  time-locked, crediting pause), webhook endpoints, and webhook keys through the API, per mode.
  Quotes and persistent deposit addresses, fast credit with reversal at finality, merchant-signed
  sweeps and merchant-paid refunds, per-account webhook keys pinned from attestation, restore mode.
  The service holds no funds and sends no transactions.
- **Launch set** (design §16), merged: PR 1 fast credit and reversal (#187), PR 2 contracts (#186),
  PR 3 schema reset and tenancy (#188), PR 4 chain-sourced sweeps (#190), PR 5 operator onboarding
  and API keys (#191), PR 6 modes and per-account webhook keys (#197), PR 7 treasuries (#198),
  PR 8 webhook endpoints and delivery (#199), PR 9 merchant refunds (#192), PR 10 API vocabulary,
  SDKs, and sweep builder (#200), and PR 12 restricted keys (#203). Also merged: metadata (#193),
  deposit addresses (#194, one per customer across chains and assets #196), per-block scanning
  (#195), API conformance with Stripe (#201), contract gas bounds (#202), launch hardening (#203:
  mandatory address pinning in live mode, restricted keys, webhook key trust continuity, crediting
  pause per treasury), ledger correctness (#204: claw-back amounts, refund lifecycle, finality
  paging, unfinalized credit cap), restore mode (#205), and PR 11 deployment, documentation, and
  the demo (#206).
- **Contracts**: the permissionless factory is deterministic: `0x45466D37587E6E46DC35eB96b74ba3D3b1E5b747`,
  implementation `0x49F2F1F1a25269Ea0C6FF2AB1C7B09dCBE9c5bA9`, on every chain
  ([deploy/CONTRACTS.md](../deploy/CONTRACTS.md)). Deployed and verified on Sepolia; not yet on
  mainnet. The staging finance Safe has its `CompatibilityFallbackHandler` set and passes
  `verify-safe.sh`.
- **Staging** (`https://pay-api-staging.phala.com`, Sepolia) ran the single-tenant service; it
  cannot run the multi-tenant schema until it is reset (below). The paths verified there on
  2026-09-27 (exact payment, underpayment, late payment, unsupported token, refunds, the demo
  checkout) are pre-reset history and are verified again after the reset.
- **SDKs**: `@phala/pay` 0.1.2 on npm (provenance, trusted publisher) predates the multi-tenant
  API; `phala-pay` installs from GitHub. Both have unreleased breaking changes (their changelogs).

## Remaining work

### Deployment (HUMAN-ONLY)

- [x] Deploy the factory on Sepolia at the deterministic address above
      ([deploy/CONTRACTS.md](../deploy/CONTRACTS.md)). Owner: deployer.
- [x] Set the staging finance Safe's fallback handler to the `CompatibilityFallbackHandler`
      (Sepolia transaction `0xc63baf59…0812`). Owner: Safe owners.
- [ ] Reset staging ([deploy/README.md](../deploy/README.md), "Staging reset"), deploy the PR 11
      image, re-create the staging accounts, and run the staging paths again. Owner: staging owner.

### Phala Cloud

- [ ] The operator onboards Phala Cloud's account: `POST /v1/admin/accounts` with its due diligence
      record (Phala's own) and contact, and hands the first keys to its engineers, who roll them;
      `charges_enabled` once mainnet is ready. Owner: operator.
- [ ] Phala Cloud integrates like any merchant (design §16, "Phala Cloud"), in the monorepo draft
      PR: restricted key, pins (account, factory and implementation, its treasuries), webhook
      endpoint and pinned webhook keys, `client_reference_id` (team id), every `deposit.*` event
      by the balance rule, `<Checkout expectedAddress>`; its finance sets Phala's Safe as treasury
      per chain through the API (Safe message), sweeps with the SDK's `safe_batch`, and pays
      refunds from the Safe with `mark_paid`. Owner: engineering; review: Phala Cloud team.

### Production inputs

| Input | Owner | Status |
|---|---|---|
| Mainnet PHA contract (proposed `0x6c5bA91642F10282b576d91922Ae6448C9d52f4E`) | Finance | to confirm |
| Phala Cloud's treasury Safe per chain (owners, threshold), set by Phala Cloud through the API; not a route input | Finance | open |
| Two paid mainnet RPC providers (public gateways rate-limit) | Ops | open |
| Production R2 bucket and keys for WAL-G | Ops | open |
| Production Phala Cloud workspace and API key for the CVM (`production` Environment) | Ops | open |
| Production admin key (the operator's RFC 9421 key) | Operator | open |
| Sentry quota for production | Ops | open |
| DNS for `pay-api.phala.com` (CNAME and `_dstack-app-address` TXT) | Ops | open |
| Route defaults in architecture §14 (minimum deposit 0, minimum credit $1, 4 quote decimals, deposit bounds, open exposure caps) and Phala Cloud's `max_unfinalized_credit` (default $1 000) | Finance | to confirm |

### Before mainnet

- [ ] Independent security review of the contracts and the service (the repository is public).
- [ ] Deploy the factory on mainnet at the same deterministic address (HUMAN-ONLY).
- [ ] Production deploy (`provision`), then a small mainnet deposit, sweep, and refund end to end
      on Phala Cloud's account.
- [ ] Restore drill against production backups, including the freeze and reconciliation.

### Before third-party merchants go live

- [ ] Phala's legal review (design §17): money transmission and crypto-asset licensing, sanctions
      obligations, the due-diligence policy, merchant agreement, privacy and retention. Until it
      signs off, the operator enables live mode only for Phala's own accounts. Owner: Legal.
- [ ] Design PR 13, account closure.

### Releases

- [ ] `phala-pay` on PyPI: add the pending trusted publisher (environment `pypi`), then release
      it (CONTRIBUTING.md, "Releasing an SDK").
- [ ] `@phala/pay` with the multi-tenant API on npm.

### Optional

- [ ] A custom domain for the staging demo through dstack-ingress, like the API.

# Plan to production

What remains before Phala Pay credits Phala Cloud workspaces on Ethereum Mainnet, and who owns
each item. The design is [architecture.md](architecture.md); integrators read
[integration.md](integration.md); operators read [deploy/README.md](../deploy/README.md) and the
[runbooks](../deploy/runbooks/README.md). Completed work is in git history and the changelogs.

## Where things stand

- **Service**: Stripe-style API (quotes with `client_secret`, deposits, refunds, `/v1/config`,
  Stripe error object and events), webhook fulfillment, CREATE2 forwarders swept to a Safe,
  dual-RPC finality, reconciliation, WAL-G backups with restore drills, Sentry. Staging runs at
  `https://pay-api-staging.phala.com` (Sepolia) behind dstack-ingress, with attested compose and
  certificate evidence verified on every deploy.
- **Paths verified on staging** (2026-09-27): exact payment, underpayment, late payment,
  unsupported token, over the deposit limit with refund, product hold with refund, and the demo
  checkout.
- **SDKs**: `@phala/pay` 0.1.2 on npm (provenance, trusted publisher); `phala-pay` installs from
  GitHub until its first PyPI release.
- **Demo**: the staging reference product serves the Phala Pay demo at `/demo/`.

## Remaining work

### Integration

- [ ] Phala Cloud: create quotes, render `<Checkout>`, fulfill `deposit.credited` through the
      existing Order and credit path. A draft PR is open in the Phala Cloud monorepo, pending the
      `phala-pay` PyPI release and a staging run. Owner: engineering; review: Phala Cloud team.
- [ ] Register Phala Cloud as the production product (`PUT /v1/admin/products/phala-cloud`:
      webhook URL, product public key). Owner: operator.

### Production inputs

| Input | Owner | Status |
|---|---|---|
| Mainnet PHA contract (proposed `0x6c5bA91642F10282b576d91922Ae6448C9d52f4E`) | Finance | to confirm |
| Treasury Safe on mainnet: owners, threshold | Finance | open |
| Two paid mainnet RPC providers (public gateways rate-limit sends) | Ops | open |
| Production R2 bucket and keys for WAL-G | Ops | open |
| Production Phala Cloud workspace and API key (`production` Environment) | Ops | open |
| Sentry quota for production | Ops | open |
| DNS for `pay-api.phala.com` (CNAME and `_dstack-app-address` TXT) | Ops | open |
| Defaults in architecture §14: minimum deposit and flush 0, minimum credit $1, 4 quote decimals, deposit and exposure caps | Finance | to confirm |
| Refund policy and pilot allowlist | Finance, Product | open |
| Compliance: region, Travel Rule, KYT timing | Compliance | open |

### Before mainnet

- [ ] Independent security review of the contracts and the service (the repository is public
      and holds funds).
- [ ] Production deploy (`provision`), then a small mainnet deposit, sweep, and refund end to end.
- [ ] Restore drill against production backups.

### Releases

- [ ] `phala-pay` on PyPI: add the pending trusted publisher (environment `pypi`), then release
      0.2.0 (CONTRIBUTING.md, "Releasing an SDK").

### Optional

- [ ] A custom domain for the staging demo through dstack-ingress, like the API.
- [ ] The staging driver (`reference_product deposit`) recognizes a product hold, so that path
      needs no manual refund request.

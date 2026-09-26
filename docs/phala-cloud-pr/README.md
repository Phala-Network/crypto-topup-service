# Prepared pull request for the Phala Cloud repository

Opened as [phala-cloud-monorepo#2196](https://github.com/Phala-Network/phala-cloud-monorepo/pull/2196)
(`docs/integrations/crypto-topup.md`); the root orchestrator keeps it in sync with the file below,
which follows the webhook fulfillment contract (docs/integration.md §5).

- **Title:** `docs: crypto top-up integration checklist`
- **File:** `docs/integrations/crypto-topup.md`

Note: `Phala-Network/crypto-topup-service` is private; reviewers need read access to follow the
links.

## Body

```markdown
Adds a checklist for integrating Phala Cloud with the crypto top-up service (PHA on Ethereum →
USD credit). The authoritative guide stays in the service repository,
[docs/integration.md](https://github.com/Phala-Network/crypto-topup-service/blob/main/docs/integration.md);
this file only tracks the work on our side, so nothing is duplicated.

No code changes in this PR. Each unchecked item becomes its own issue or PR.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
```

## File: `docs/integrations/crypto-topup.md`

```markdown
# Crypto top-up integration

Phala Cloud credits USD for PHA deposits handled by the crypto top-up service. The service owns
addresses, chain evidence, pricing, screening, and sweeps, and tells Phala Cloud what to credit
with a signed `deposit.credited` webhook; Phala Cloud owns the balance and credits each deposit
once, like Stripe Checkout fulfillment.

The authoritative integration guide, including the fulfillment contract, the webhook format, and
the go-live checklist, is
[crypto-topup-service/docs/integration.md](https://github.com/Phala-Network/crypto-topup-service/blob/main/docs/integration.md).
Section numbers below refer to it.

## Service origins

| Environment | Origin | Chain |
|---|---|---|
| Production | `https://crypto-topup-api.phala.com` | Ethereum Mainnet (1) |
| Staging | `https://crypto-topup-api-staging.phala.com` | Sepolia (11155111) |

Both domains are being set up. The origin is part of every request signature: configure it
exactly.

## Changes on our side

### Fulfillment (guide §5)

- [ ] `POST {webhook_url}` reachable over public HTTPS from the service.
- [ ] Verify Standard Webhooks `v1a` with the pinned `(settlement/v1, public key)` over the raw
      body; answer `400` on failure.
- [ ] On `deposit.credited`, in one transaction: find-or-create the `Order` (`provider =
      crypto_topup`, `order_flow_code = 'crypto-top-up'`, `provider_order_id =
      "deposit:<deposit_id>"`, unique for that flow), the credit transaction for `external_id`
      and `amount_minor` (`funding_source = crypto:<asset>:<chain>`), and
      `complete_order_payment`; answer `2xx` after the commit. A repeat is a no-op.
- [ ] Refusals (closed or suspended workspace, our caps) are recorded as held and answered
      `2xx`, never `5xx`; support requests the refund with a user-supplied address.
- [ ] A repeated `deposit.credited` with a different `amount_minor` keeps the first credit and
      alerts (only possible after a service restore).
- [ ] Every other event type is stored once by `webhook-id` for notifications and history
      (`deposit.pending`, `deposit.rejected`, `deposit.refunded`, `rate_lock.expired`); ignore
      unknown types and fields; do not rely on event order.
- [ ] Optional hardening, decided with finance: per-deposit and per-period caps as holds, a
      `GET /deposits/{id}` check, own-node log verification.

### Account and quote flow (guide §4, §7; architecture §12 "Customer experience obligations")

- [ ] Workspaces (`external_id` = team id) are created by their first quote or address; no
      separate registration is needed.
- [ ] Quote page: check the returned address equals the one computed from `(phala-cloud, team id,
      lock_ref)`, then show the exact amount,
      EIP-681 QR, countdown, spread, and the late / under / over-payment rules; resume by
      `lock_ref`; cancel and re-quote.
- [ ] Waiting screen from the lock's `payment` (`seen`, confirmations, `estimated_final_at`) and
      `pending-deposits`; never shown as credited.
- [ ] Persistent address as an advanced option; recompute it (and every rotated version) before
      display.
- [ ] Deposit history from `deposits`; "needs attention" copy by rejection reason without showing
      the reason code.
- [ ] Refund request flow with a user-supplied destination address, also for held credits.
- [ ] Support lookup by transaction hash, address, or lock reference (`GET /deposits?…`).

### Key management

- [ ] One product key per environment, created with `topup-sdk keygen --keyid phala-cloud/v1`;
      seeds in the secret store; only the public key and key id go to the service operator.
- [ ] Pin the service's `settlement/v1` public key per environment from verified attestation
      (official dstack verifier plus the SDK's report-data binding check, guide §3.3).

### Configuration values

| Name (proposed) | Value |
|---|---|
| `CRYPTO_TOPUP_ORIGIN` | the service origin above |
| `CRYPTO_TOPUP_PRODUCT_SLUG` | `phala-cloud` |
| `CRYPTO_TOPUP_PRODUCT_KEYID` | `phala-cloud/v1` |
| `CRYPTO_TOPUP_PRODUCT_SEED` | secret: 32-byte hex seed |
| `CRYPTO_TOPUP_SETTLEMENT_KEYID` | `settlement/v1` |
| `CRYPTO_TOPUP_SETTLEMENT_PUBKEY` | pinned from verified attestation |
| `CRYPTO_TOPUP_WEBHOOK_URL` | our public webhook URL |
| `CRYPTO_TOPUP_CHAIN_ID`, `…_TOKEN`, `…_FACTORY`, `…_IMPLEMENTATION` | from the service's route, to recompute addresses |
| `CRYPTO_TOPUP_PER_DEPOSIT_CAP_MINOR`, `…_PER_PERIOD_CAP_MINOR`, `…_PERIOD_SECONDS` | optional holds, agreed with finance |

### Monitoring

- [ ] Alert on webhook verification failures, a repeated `deposit.credited` with a different
      amount, and held credits waiting for a refund.
- [ ] Reconcile credited orders with the service's `deposits` for our workspaces.

### Verification before go-live (guide §8)

- [ ] `topup-sdk send-test-event` passes against our production code path, and the ledger holds
      one credit.
- [ ] One quote-first deposit credited end to end on staging, plus the abnormal payments.
- [ ] Webhook URL and the product public key sent to the service operator: on staging the admin
      replaces the reference product's key and webhook URL with ours
      (`PUT /v1/admin/products/phala-cloud`).
```

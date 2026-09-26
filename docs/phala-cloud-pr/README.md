# Prepared pull request for the Phala Cloud repository

Not opened. The root orchestrator opens it after the owner approves. It adds one file to the
Phala Cloud repository; the path below is a proposal, to be adjusted to that repository's docs
layout.

- **Title:** `docs: crypto top-up integration checklist`
- **File:** `docs/crypto-topup-integration.md`

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

## File: `docs/crypto-topup-integration.md`

```markdown
# Crypto top-up integration

Phala Cloud credits USD for PHA deposits handled by the crypto top-up service. The service owns
addresses, chain evidence, pricing, and sweeps; Phala Cloud owns the balance and decides every
credit through a settlement endpoint it implements.

The authoritative integration guide, including the settlement contract, the webhook format, and
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

### Settlement endpoint (guide §5)

- [ ] `POST {settlement_url}` and `GET {settlement_url}/{key}`, reachable over public HTTPS from
      the service.
- [ ] Verify the RFC 9421 signature against the pinned `(settlement/v1, public key)`, with the
      target URI built from our configured public URL, never from `Host`.
- [ ] Recompute `deposit_id` from the evidence; require
      `idempotency_key == "deposit:" + deposit_id`.
- [ ] Verify the cited Transfer log on our own Ethereum RPC at finality: emitter is the approved
      token, `to` is an address we computed for the workspace, amount matches. RPC failure or
      not-yet-final → `503`, nothing stored.
- [ ] Per-deposit and per-period caps, checked in the same transaction as the credit.
- [ ] In one transaction: find-or-create the `Order` (`provider = crypto_topup`,
      `order_flow_code = 'crypto-top-up'`, `provider_order_id` = key, unique for that flow), the
      credit transaction (`funding_source = crypto:<asset>:<chain>`), and
      `complete_order_payment`; answer `accepted` with `destination_tx_id` =
      credit transaction id.
- [ ] Idempotency records never expire; replay returns the stored answer; same key with a
      different payload → `422`; unknown key on `GET` → `404`; `GET` returns the original
      payload.
- [ ] Durable business refusals (closed or unknown workspace, cap) → `200 rejected` with a
      reason.
- [ ] Test-environment-only conformance hooks (ledger observation endpoint, five test accounts),
      never enabled in production.

### Webhook receiver (guide §6)

- [ ] `POST {webhook_url}`: verify Standard Webhooks `v1a` with the same pinned key over the raw
      body; store by `webhook-id` once; answer `2xx` only after the store commits.
- [ ] Never change balances from events; refresh deposit or quote state and notify the user
      (`deposit.credited`, `deposit.rejected`, `deposit.refunded`, `rate_lock.expired`;
      optionally `deposit.pending`).
- [ ] Ignore unknown event types and fields; do not rely on event order.

### Account and quote flow (guide §4, §7; architecture §12 "Customer experience obligations")

- [ ] Register each workspace (`external_id` = team id) before its first quote or address.
- [ ] Quote page: record the lock address computed from `(phala-cloud, team id, lock_ref)` before
      calling `rate-locks`, check the returned address equals it, then show the exact amount,
      EIP-681 QR, countdown, spread, and the late / under / over-payment rules; resume by
      `lock_ref`; cancel and re-quote.
- [ ] Waiting screen from the lock's `payment` (`seen`, confirmations, `estimated_final_at`) and
      `pending-deposits`; never shown as credited.
- [ ] Persistent address as an advanced option; recompute it (and every rotated version) before
      display and record it.
- [ ] Deposit history from `deposits`; "needs attention" copy by rejection reason without showing
      the reason code.
- [ ] Refund request flow with a user-supplied destination address.
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
| `CRYPTO_TOPUP_PUBLIC_URL` | our public base URL for settlement and webhooks |
| `CRYPTO_TOPUP_RPC_URL` | our own Ethereum RPC for log verification |
| `CRYPTO_TOPUP_CHAIN_ID`, `…_TOKEN`, `…_FACTORY`, `…_IMPLEMENTATION` | from the service's route |
| `CRYPTO_TOPUP_PER_DEPOSIT_CAP_MINOR`, `…_PER_PERIOD_CAP_MINOR`, `…_PERIOD_SECONDS` | agreed with finance |

### Monitoring

- [ ] Alert on settlement `401` (key or URL drift), sustained `503` answers (RPC lag), `422`
      answers, and webhook verification failures.
- [ ] Reconcile credited orders with the service's `deposits` for our workspaces.

### Verification before go-live (guide §8)

- [ ] The service's conformance suite (`topup-conformance`) passes against our endpoint, with a
      restart, and reports no warnings.
- [ ] One quote-first deposit credited end to end on staging, plus the abnormal payments.
- [ ] Settlement and webhook URLs and the product public key sent to the service operator for
      registration.
```

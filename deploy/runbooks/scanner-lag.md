# Scanner lag

**Trigger:** the `topup-scanner-<chain_id>` monitor missing its check-ins (no successful finalized
scan for five minutes). If the chain is frozen (`TopupReconciliationMismatch`,
`check:address_derivation`), the scanner is paused on purpose: [Chain frozen](chain-frozen.md).

**Impact:** new finalized transfers are not detected, so customers wait; rate locks on the chain
cannot expire. Existing deposits continue. One chain and every route on it.

## First steps

```sh
cast block finalized --json --rpc-url "$RPC_PROVIDER_A_URL" | jq '(.data // .) | {number,hash}'
cast block finalized --json --rpc-url "$RPC_PROVIDER_B_URL" | jq '(.data // .) | {number,hash}'
```

When the lag is material, pause issuance (existing addresses stay valid and watched):
`admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["quotes"]}'`.

## Decide

- A provider down, throttling, or behind: the scanner reads provider A; follow
  [provider disagreement](provider-disagreement.md) and replace it through a route upgrade. A
  provider A whose `finalized` is below one it answered before, or below the committed cursor (a
  load-balanced gateway answering from a node that has not caught up; repeat the first command a
  few times), is retried, not trusted: the monitor reports errors until it catches up. If it
  keeps lagging, replace it.
- A chain scanner that stops on a failure it cannot retry stops the whole service (Sentry:
  `chain scanner task stopped; stopping every chain scanner`), which the container restart policy
  restarts; the scanner resumes from its committed cursor, and a transfer it had not recorded is
  recorded then. If the service keeps restarting, read the error and escalate. If both providers
  are healthy and the monitor is still silent, **HUMAN-ONLY:** restart the CVM
  (`npx --yes phala@1.1.22 cvms restart "$TOPUP_CVM_ID"`).

## Done when

`topup-scanner-<chain_id>` checks in again, deposits made during the lag appear (the merchant's
`GET /v1/deposits?tx_hash=…`, then the admin deposit view), and issuance is resumed.

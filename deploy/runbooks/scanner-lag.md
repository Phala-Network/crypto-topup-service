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
`admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["quotes","addresses"]}'`.

## Decide

- A provider down, throttling, or behind: the scanner reads provider A; follow
  [provider disagreement](provider-disagreement.md) and replace it through a route upgrade.
- Both providers healthy: the scanner loop has stopped. **HUMAN-ONLY:** restart the CVM
  (`npx --yes phala@1.1.22 cvms restart "$TOPUP_CVM_ID"`); the scanner resumes from its committed
  cursor.

## Done when

`topup-scanner-<chain_id>` checks in again, deposits made during the lag appear (support lookup by
`tx_hash`), and issuance is resumed.
